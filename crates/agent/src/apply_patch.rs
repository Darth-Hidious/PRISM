//! Transactional, drift-tolerant application of model-authored source edits,
//! and exclusive creation of new files.

use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::ffi::{CStr, CString, OsStr};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

const BEGIN_PATCH: &str = "*** Begin Patch";
const UPDATE_FILE: &str = "*** Update File: ";
const ADD_FILE: &str = "*** Add File: ";
const END_PATCH: &str = "*** End Patch";
const HUNK_MARKER: &str = "@@";

const MIN_FUZZY_SIMILARITY: f64 = 0.5;
const MAX_FUZZY_SIMILARITY: f64 = 1.0;
const DEFAULT_FUZZY_WINDOW_LINES: usize = 4_096;
const MAX_FUZZY_WINDOW_LINES: usize = 100_000;
const MAX_FUZZY_PATTERN_LINES: usize = 256;
const MAX_FUZZY_BLOCK_BYTES: usize = 64 * 1024;
const MAX_FUZZY_TOTAL_DP_CELLS: usize = 4_000_000;
const AMBIGUITY_WITNESS_LIMIT: usize = 2;
#[cfg(unix)]
const TEMP_CREATE_ATTEMPTS: usize = 100;

#[cfg(unix)]
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    patch: String,
    #[serde(default)]
    match_policy: MatchPolicy,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct MatchPolicy {
    allow_whitespace: bool,
    allow_fuzzy: bool,
    fuzzy_similarity_threshold: f64,
    fuzzy_window_lines: usize,
}

impl Default for MatchPolicy {
    fn default() -> Self {
        Self {
            allow_whitespace: true,
            allow_fuzzy: true,
            fuzzy_similarity_threshold: 0.9,
            fuzzy_window_lines: DEFAULT_FUZZY_WINDOW_LINES,
        }
    }
}

impl MatchPolicy {
    fn validate(&self) -> Result<()> {
        if !self.fuzzy_similarity_threshold.is_finite()
            || !(MIN_FUZZY_SIMILARITY..=MAX_FUZZY_SIMILARITY)
                .contains(&self.fuzzy_similarity_threshold)
        {
            bail!(
                "match_policy.fuzzy_similarity_threshold must be finite and between {MIN_FUZZY_SIMILARITY} and {MAX_FUZZY_SIMILARITY}"
            );
        }
        if !(1..=MAX_FUZZY_WINDOW_LINES).contains(&self.fuzzy_window_lines) {
            bail!("match_policy.fuzzy_window_lines must be between 1 and {MAX_FUZZY_WINDOW_LINES}");
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ParsedPatch {
    path: PathBuf,
    body: PatchBody,
}

/// What the envelope asks for: hunks against an existing file, or the whole
/// text of a file that does not exist yet.
#[derive(Clone, Debug)]
enum PatchBody {
    Update(Vec<Hunk>),
    Add(String),
}

#[derive(Clone, Debug)]
struct Hunk {
    lines: Vec<PatchLine>,
}

#[derive(Clone, Debug)]
enum PatchLine {
    Context(String),
    Add(String),
    Remove(String),
}

impl PatchLine {
    fn source_text(&self) -> Option<&str> {
        match self {
            Self::Context(text) | Self::Remove(text) => Some(text),
            Self::Add(_) => None,
        }
    }

    fn destination_text(&self) -> Option<&str> {
        match self {
            Self::Context(text) | Self::Add(text) => Some(text),
            Self::Remove(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
            Self::Cr => b"\r",
        }
    }
}

#[derive(Clone, Debug)]
struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

#[derive(Debug)]
struct SourceFile {
    has_bom: bool,
    lines: Vec<SourceLine>,
    preferred_ending: LineEnding,
    trailing_ending: Option<LineEnding>,
}

impl SourceFile {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let (has_bom, text_bytes) = if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
            (true, rest)
        } else {
            (false, bytes)
        };
        let text = std::str::from_utf8(text_bytes)
            .context("apply_patch only supports existing UTF-8 text files")?;

        let mut lines = Vec::new();
        let mut preferred_ending = None;
        let mut line_start = 0;
        let mut cursor = 0;
        while cursor < text.len() {
            let (ending, ending_len) = match text.as_bytes()[cursor] {
                b'\r' if text.as_bytes().get(cursor + 1) == Some(&b'\n') => (LineEnding::CrLf, 2),
                b'\r' => (LineEnding::Cr, 1),
                b'\n' => (LineEnding::Lf, 1),
                _ => {
                    cursor += 1;
                    continue;
                }
            };
            preferred_ending.get_or_insert(ending);
            lines.push(SourceLine {
                text: text[line_start..cursor].to_owned(),
                ending: Some(ending),
            });
            cursor += ending_len;
            line_start = cursor;
        }
        if line_start < text.len() {
            lines.push(SourceLine {
                text: text[line_start..].to_owned(),
                ending: None,
            });
        }

        let trailing_ending = lines.last().and_then(|line| line.ending);
        Ok(Self {
            has_bom,
            lines,
            preferred_ending: preferred_ending.unwrap_or(LineEnding::Lf),
            trailing_ending,
        })
    }

    fn render(mut self) -> Result<Vec<u8>> {
        if self.lines.is_empty() && self.trailing_ending.is_some() {
            bail!("patch would make it impossible to preserve the file's trailing newline state");
        }

        let last_index = self.lines.len().checked_sub(1);
        for (index, line) in self.lines.iter_mut().enumerate() {
            if Some(index) == last_index {
                if let Some(ending) = self.trailing_ending {
                    line.ending.get_or_insert(ending);
                } else {
                    line.ending = None;
                }
            } else {
                line.ending.get_or_insert(self.preferred_ending);
            }
        }

        let text_bytes = self.lines.iter().fold(0usize, |size, line| {
            size.saturating_add(line.text.len())
                .saturating_add(line.ending.map_or(0, |ending| ending.bytes().len()))
        });
        let mut output = Vec::with_capacity(text_bytes + usize::from(self.has_bom) * 3);
        if self.has_bom {
            output.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        for line in self.lines {
            output.extend_from_slice(line.text.as_bytes());
            if let Some(ending) = line.ending {
                output.extend_from_slice(ending.bytes());
            }
        }
        Ok(output)
    }
}

#[derive(Clone, Copy, Debug)]
enum MatchTier {
    Exact,
    Whitespace,
    Fuzzy,
}

impl MatchTier {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Whitespace => "whitespace",
            Self::Fuzzy => "fuzzy",
        }
    }
}

#[derive(Debug)]
struct PlannedHunk {
    start: usize,
    source_len: usize,
    replacement: Vec<SourceLine>,
    tier: MatchTier,
}

/// Execute one patch below `project_root`: context-block hunks against an
/// existing file, or `*** Add File:` to create one.
///
/// The envelope intentionally has one target. Context-block hunks are small
/// enough for a model to emit consistently while still carrying the unchanged
/// text needed for safe drift-tolerant location. `Add File` is accepted because
/// this tool carries the Codex grammar's name and the live log shows models
/// emitting it; delete/move and line-number syntax stay absent because they
/// enlarge the parser and make destructive intent less explicit.
pub(crate) fn execute(project_root: &Path, args: &Value) -> Result<Value> {
    let args: ApplyPatchArgs =
        serde_json::from_value(args.clone()).context("invalid apply_patch arguments")?;
    args.match_policy.validate()?;
    let patch = parse_patch(&args.patch)?;
    validate_target_path(&patch.path)?;

    #[cfg(not(unix))]
    {
        let _ = (project_root, patch, args);
        bail!(
            "apply_patch is unavailable on this platform because confined descriptor-relative replacement is not implemented"
        );
    }

    #[cfg(unix)]
    match patch.body {
        PatchBody::Update(hunks) => {
            execute_confined(project_root, &patch.path, &hunks, args.match_policy)
        }
        PatchBody::Add(content) => create_confined(project_root, &patch.path, &content),
    }
}

#[cfg(unix)]
fn execute_confined(
    project_root: &Path,
    path: &Path,
    hunks: &[Hunk],
    match_policy: MatchPolicy,
) -> Result<Value> {
    let (target, original) = ConfinedTarget::open(project_root, path)?;
    let source = SourceFile::parse(&original)?;
    let plans = plan_hunks(&source, hunks, &match_policy)?;
    let match_tiers: Vec<&str> = plans.iter().map(|plan| plan.tier.as_str()).collect();
    let updated = apply_plans(source, &plans)?.render()?;

    if updated == original {
        bail!("patch produces no byte-level change");
    }
    target.atomic_replace(&original, &updated)?;

    Ok(json!({
        "success": true,
        "path": path.to_string_lossy(),
        "hunks_applied": hunks.len(),
        "bytes_written": updated.len(),
        "size_bytes": updated.len(),
        "match_tiers": match_tiers,
    }))
}

/// Create `relative` below `project_root` with `content`, through the same
/// symlink-refusing directory walk as an update. Exclusive create: an existing
/// file is never overwritten by an `Add File`.
#[cfg(unix)]
fn create_confined(project_root: &Path, relative: &Path, content: &str) -> Result<Value> {
    let (parent, name) = ConfinedTarget::resolve_parent(project_root, relative)?;
    let raw_fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    let fd = match owned_fd(raw_fd) {
        Ok(fd) => fd,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => bail!(
            "'{}' already exists; '{ADD_FILE}' only creates a file — use '{UPDATE_FILE}{}' with '@@' hunks to change it",
            relative.display(),
            relative.display()
        ),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to create '{}'", relative.display()));
        }
    };
    let mut file = File::from(fd);
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write '{}'", relative.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync '{}'", relative.display()))?;
    Ok(json!({
        "success": true,
        "path": relative.to_string_lossy(),
        "created": true,
        "bytes_written": content.len(),
        "size_bytes": content.len(),
    }))
}

fn parse_patch(input: &str) -> Result<ParsedPatch> {
    let lines: Vec<&str> = input.lines().collect();
    if lines.iter().any(|line| line.contains('\r')) {
        bail!("invalid patch: lone carriage returns are not allowed");
    }
    if lines.first() != Some(&BEGIN_PATCH) {
        bail!("invalid patch: first line must be '{BEGIN_PATCH}'");
    }
    if lines.last() != Some(&END_PATCH) {
        bail!("invalid patch: last line must be '{END_PATCH}'");
    }
    if lines.len() < 3 {
        bail!(
            "invalid patch: the patch is empty — between '{BEGIN_PATCH}' and '{END_PATCH}' put \
             '{UPDATE_FILE}<relative path>' followed by '@@' hunks, or '{ADD_FILE}<relative path>' \
             followed by '+' lines"
        );
    }

    let target_line = lines[1];
    if let Some(path_text) = target_line.strip_prefix(ADD_FILE) {
        let path = target_path(path_text, ADD_FILE)?;
        let content = parse_added_file(&path, &lines[2..lines.len() - 1])?;
        return Ok(ParsedPatch {
            path,
            body: PatchBody::Add(content),
        });
    }
    let path_text = target_line.strip_prefix(UPDATE_FILE).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid patch: second line must be '{UPDATE_FILE}<relative path>' (or \
             '{ADD_FILE}<relative path>' to create a file)"
        )
    })?;
    let path = target_path(path_text, UPDATE_FILE)?;
    if lines.len() < 5 {
        bail!(
            "invalid patch: '{UPDATE_FILE}{}' has no hunk — a hunk is an '@@' line followed by \
             lines prefixed with a space (unchanged), '-' (removed) or '+' (added)",
            path.display()
        );
    }

    let mut hunks = Vec::new();
    let mut cursor = 2;
    while cursor < lines.len() - 1 {
        if lines[cursor].starts_with(UPDATE_FILE) {
            bail!("invalid patch: multiple target sections are not supported");
        }
        if lines[cursor] != HUNK_MARKER {
            bail!(
                "invalid patch at line {}: expected '{HUNK_MARKER}'",
                cursor + 1
            );
        }
        cursor += 1;

        let mut hunk_lines = Vec::new();
        while cursor < lines.len() - 1 && lines[cursor] != HUNK_MARKER {
            let line = lines[cursor];
            if line.starts_with(UPDATE_FILE) {
                bail!("invalid patch: multiple target sections are not supported");
            }
            let (prefix, text) = line.split_at_checked(1).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid patch at line {}: hunk lines require a space, '+', or '-' prefix",
                    cursor + 1
                )
            })?;
            let patch_line = match prefix {
                " " => PatchLine::Context(text.to_owned()),
                "+" => PatchLine::Add(text.to_owned()),
                "-" => PatchLine::Remove(text.to_owned()),
                _ => bail!(
                    "invalid patch at line {}: hunk lines require a space, '+', or '-' prefix",
                    cursor + 1
                ),
            };
            hunk_lines.push(patch_line);
            cursor += 1;
        }
        validate_hunk(&hunk_lines, hunks.len() + 1)?;
        hunks.push(Hunk { lines: hunk_lines });
    }

    if hunks.is_empty() {
        bail!("invalid patch: at least one hunk is required");
    }
    Ok(ParsedPatch {
        path,
        body: PatchBody::Update(hunks),
    })
}

fn target_path(path_text: &str, verb: &str) -> Result<PathBuf> {
    if path_text.is_empty() || path_text.trim() != path_text {
        bail!(
            "invalid patch: the path after '{}' must be non-empty with no surrounding whitespace",
            verb.trim()
        );
    }
    Ok(PathBuf::from(path_text))
}

/// The body of an `Add File`: every line is `+<text>`; the file gets exactly
/// those lines, each newline-terminated.
fn parse_added_file(path: &Path, body: &[&str]) -> Result<String> {
    if body.is_empty() {
        bail!(
            "invalid patch: '{ADD_FILE}{}' has no '+' lines — each line of the new file is written as '+<text>'",
            path.display()
        );
    }
    let mut content = String::new();
    for (offset, line) in body.iter().enumerate() {
        let Some(text) = line.strip_prefix('+') else {
            bail!(
                "invalid patch at line {}: every line of an added file is written as '+<text>'; \
                 '{ADD_FILE}' creates a file and cannot modify one — use '{UPDATE_FILE}' with \
                 '@@' hunks for that",
                offset + 3
            );
        };
        content.push_str(text);
        content.push('\n');
    }
    Ok(content)
}

fn validate_hunk(lines: &[PatchLine], hunk_number: usize) -> Result<()> {
    if lines.is_empty() {
        bail!("invalid patch: hunk {hunk_number} is empty");
    }
    if !lines
        .iter()
        .any(|line| matches!(line, PatchLine::Add(_) | PatchLine::Remove(_)))
    {
        bail!(
            "invalid patch: hunk {hunk_number} has no '+' or '-' line — every line starts with a \
             space, which marks unchanged context. Prefix each added line with '+' and each \
             removed line with '-'. To append to a file, quote its last existing line as context \
             and put the '+' lines after it"
        );
    }

    let source: Vec<&str> = lines.iter().filter_map(PatchLine::source_text).collect();
    let destination: Vec<&str> = lines
        .iter()
        .filter_map(PatchLine::destination_text)
        .collect();
    if source.is_empty() {
        bail!("invalid patch: hunk {hunk_number} has no source anchor");
    }
    if source == destination {
        bail!(
            "invalid patch: hunk {hunk_number}'s '+' lines are identical to its '-' lines, so it \
             changes nothing — put the new text on the '+' lines"
        );
    }
    Ok(())
}

fn validate_target_path(relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        bail!("patch target must be a non-empty relative path");
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("patch target may contain only normal relative path components");
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
struct ConfinedTarget {
    parent: OwnedFd,
    name: CString,
    relative: PathBuf,
    identity: FileIdentity,
    permission_mode: libc::mode_t,
}

#[cfg(unix)]
impl ConfinedTarget {
    fn open(project_root: &Path, relative: &Path) -> Result<(Self, Vec<u8>)> {
        let (parent, name) = Self::resolve_parent(project_root, relative)?;
        let (mut file, metadata) = open_regular_file_at(parent.as_raw_fd(), &name, relative)?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let permission_mode = metadata.mode() as libc::mode_t & 0o7777;
        let mut original = Vec::new();
        file.read_to_end(&mut original)
            .with_context(|| format!("failed to read patch target '{}'", relative.display()))?;

        Ok((
            Self {
                parent,
                name,
                relative: relative.to_owned(),
                identity,
                permission_mode,
            },
            original,
        ))
    }

    /// Walk to the target's directory without following a symlink anywhere on
    /// the way, and return it with the target's own name.
    fn resolve_parent(project_root: &Path, relative: &Path) -> Result<(OwnedFd, CString)> {
        let canonical_root = std::fs::canonicalize(project_root).with_context(|| {
            format!(
                "failed to canonicalize project root '{}'",
                project_root.display()
            )
        })?;
        let mut parent = open_directory_path(&canonical_root).with_context(|| {
            format!(
                "failed to open project root '{}' without following symlinks",
                canonical_root.display()
            )
        })?;

        let components: Vec<&OsStr> = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value),
                _ => None,
            })
            .collect();
        let (file_name, directories) = components
            .split_last()
            .ok_or_else(|| anyhow::anyhow!("patch target has no file name"))?;
        for directory in directories {
            parent = open_directory_at(parent.as_raw_fd(), directory).with_context(|| {
                format!(
                    "patch target directory component '{}' is a symlink, missing, or not a directory",
                    directory.to_string_lossy()
                )
            })?;
        }

        let name = os_str_to_cstring(file_name, "patch target file name")?;
        Ok((parent, name))
    }
}

#[cfg(unix)]
fn os_str_to_cstring(value: &OsStr, description: &str) -> Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| anyhow::anyhow!("{description} contains a NUL byte"))
}

#[cfg(unix)]
fn owned_fd(raw_fd: libc::c_int) -> std::io::Result<OwnedFd> {
    if raw_fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: a non-negative descriptor returned by open/openat is newly
        // owned by this call and is transferred exactly once into `OwnedFd`.
        Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
    }
}

#[cfg(unix)]
fn open_directory_path(path: &Path) -> Result<OwnedFd> {
    let path = os_str_to_cstring(path.as_os_str(), "project root path")?;
    // SAFETY: `path` is NUL-terminated and remains alive for the call. No
    // variadic mode argument is required because O_CREAT is absent.
    let raw_fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_fd(raw_fd).map_err(Into::into)
}

#[cfg(unix)]
fn open_directory_at(parent_fd: RawFd, name: &OsStr) -> Result<OwnedFd> {
    let name = os_str_to_cstring(name, "patch target directory component")?;
    // SAFETY: `name` is NUL-terminated, `parent_fd` is a live directory
    // descriptor owned by the caller, and O_CREAT is absent.
    let raw_fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_fd(raw_fd).map_err(Into::into)
}

#[cfg(unix)]
fn open_regular_file_at(
    parent_fd: RawFd,
    name: &CStr,
    relative: &Path,
) -> Result<(File, std::fs::Metadata)> {
    // SAFETY: `name` is NUL-terminated, `parent_fd` is a live directory
    // descriptor, and O_CREAT is absent.
    let raw_fd = unsafe {
        libc::openat(
            parent_fd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    let fd = match owned_fd(raw_fd) {
        Ok(fd) => fd,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            bail!("patch target '{}' may not be a symlink", relative.display())
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("patch target '{}' must already exist", relative.display())
            });
        }
    };
    let file = File::from(fd);
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect patch target '{}'", relative.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "patch target '{}' must be an existing regular file",
            relative.display()
        );
    }
    Ok((file, metadata))
}

fn plan_hunks(
    source: &SourceFile,
    hunks: &[Hunk],
    policy: &MatchPolicy,
) -> Result<Vec<PlannedHunk>> {
    let mut plans = Vec::with_capacity(hunks.len());
    let mut source_cursor = 0;
    let mut fuzzy_remaining_cells = MAX_FUZZY_TOTAL_DP_CELLS;

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let pattern: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(PatchLine::source_text)
            .collect();
        let (start, tier) = locate_hunk(
            &source.lines,
            &pattern,
            source_cursor,
            policy,
            hunk_index + 1,
            &mut fuzzy_remaining_cells,
        )?;
        let source_len = pattern.len();
        let replacement = build_replacement(source, hunk, start);
        plans.push(PlannedHunk {
            start,
            source_len,
            replacement,
            tier,
        });
        // Patch hunks are source-ordered: a successful earlier hunk narrows
        // later searches to the untouched suffix, while overlap remains an
        // error. This intentionally permits forward searching past drift.
        source_cursor = start + source_len;
    }
    Ok(plans)
}

fn locate_hunk(
    lines: &[SourceLine],
    pattern: &[&str],
    search_start: usize,
    policy: &MatchPolicy,
    hunk_number: usize,
    fuzzy_remaining_cells: &mut usize,
) -> Result<(usize, MatchTier)> {
    let exact = collect_candidates(lines, pattern, search_start, |actual, expected| {
        actual == expected
    });
    if let Some(index) = require_unique(exact, MatchTier::Exact, hunk_number)? {
        return Ok((index, MatchTier::Exact));
    }

    if policy.allow_whitespace {
        let whitespace =
            collect_candidates(lines, pattern, search_start, whitespace_insensitive_eq);
        if let Some(index) = require_unique(whitespace, MatchTier::Whitespace, hunk_number)? {
            return Ok((index, MatchTier::Whitespace));
        }
    }

    if policy.allow_fuzzy && pattern.len() <= MAX_FUZZY_PATTERN_LINES {
        let fuzzy = collect_fuzzy_candidates(
            lines,
            pattern,
            search_start,
            policy.fuzzy_similarity_threshold,
            policy.allow_whitespace,
            policy.fuzzy_window_lines,
            fuzzy_remaining_cells,
        )?;
        if let Some(index) = require_unique(fuzzy, MatchTier::Fuzzy, hunk_number)? {
            return Ok((index, MatchTier::Fuzzy));
        }
    }

    let fuzzy_note = if policy.allow_fuzzy && pattern.len() > MAX_FUZZY_PATTERN_LINES {
        format!(
            "; fuzzy matching refused because the {}-line source block exceeds the hard {MAX_FUZZY_PATTERN_LINES}-line pattern cap",
            pattern.len(),
        )
    } else {
        String::new()
    };
    bail!(
        "hunk {hunk_number} source context was not found after line {}{fuzzy_note}",
        search_start + 1
    )
}

fn collect_candidates<F>(
    lines: &[SourceLine],
    pattern: &[&str],
    search_start: usize,
    mut equals: F,
) -> Vec<usize>
where
    F: FnMut(&str, &str) -> bool,
{
    if pattern.len() > lines.len().saturating_sub(search_start) {
        return Vec::new();
    }
    let mut candidates = Vec::with_capacity(AMBIGUITY_WITNESS_LIMIT);
    for start in search_start..=lines.len() - pattern.len() {
        if pattern
            .iter()
            .enumerate()
            .all(|(offset, expected)| equals(&lines[start + offset].text, expected))
        {
            candidates.push(start);
            if candidates.len() == AMBIGUITY_WITNESS_LIMIT {
                break;
            }
        }
    }
    candidates
}

fn require_unique(
    candidates: Vec<usize>,
    tier: MatchTier,
    hunk_number: usize,
) -> Result<Option<usize>> {
    match candidates.as_slice() {
        [] => Ok(None),
        [index] => Ok(Some(*index)),
        _ => {
            let locations = candidates
                .iter()
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "ambiguous hunk {hunk_number}: {} matching found candidates at lines {locations}",
                tier.as_str()
            )
        }
    }
}

fn whitespace_insensitive_eq(actual: &str, expected: &str) -> bool {
    actual.split_whitespace().eq(expected.split_whitespace())
}

fn collect_fuzzy_candidates(
    lines: &[SourceLine],
    pattern: &[&str],
    search_start: usize,
    threshold: f64,
    normalize_whitespace: bool,
    candidate_window: usize,
    remaining_cells: &mut usize,
) -> Result<Vec<usize>> {
    if pattern.len() > lines.len().saturating_sub(search_start) {
        return Ok(Vec::new());
    }
    let expected_bytes = block_byte_size(pattern.iter().copied())
        .ok_or_else(|| anyhow::anyhow!("fuzzy source block size overflow"))?;
    if expected_bytes > MAX_FUZZY_BLOCK_BYTES {
        bail!(
            "fuzzy source block is {expected_bytes} bytes, exceeding the hard {MAX_FUZZY_BLOCK_BYTES}-byte safety cap"
        );
    }
    let expected = comparable_block(pattern.iter().copied(), normalize_whitespace);
    let available_starts = lines.len() - pattern.len() - search_start + 1;
    let end_exclusive = search_start + available_starts.min(candidate_window);
    let mut candidates = Vec::with_capacity(AMBIGUITY_WITNESS_LIMIT);
    for start in search_start..end_exclusive {
        let candidate_lines = &lines[start..start + pattern.len()];
        let candidate_bytes =
            block_byte_size(candidate_lines.iter().map(|line| line.text.as_str()));
        if candidate_bytes.is_none_or(|size| size > MAX_FUZZY_BLOCK_BYTES) {
            continue;
        }
        let actual = comparable_block(
            candidate_lines.iter().map(|line| line.text.as_str()),
            normalize_whitespace,
        );
        if similarity_at_least(&actual, &expected, threshold, remaining_cells)? {
            candidates.push(start);
            if candidates.len() == AMBIGUITY_WITNESS_LIMIT {
                break;
            }
        }
    }
    Ok(candidates)
}

fn block_byte_size<'a>(lines: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    let mut total = 0usize;
    let mut saw_line = false;
    for line in lines {
        if saw_line {
            total = total.checked_add(1)?;
        }
        total = total.checked_add(line.len())?;
        saw_line = true;
    }
    Some(total)
}

fn comparable_block<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    normalize_whitespace: bool,
) -> String {
    lines
        .into_iter()
        .map(|line| {
            if normalize_whitespace {
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn similarity_at_least(
    left: &str,
    right: &str,
    threshold: f64,
    remaining_cells: &mut usize,
) -> Result<bool> {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let longest = left.len().max(right.len());
    if longest == 0 {
        return Ok(true);
    }
    // `ceil` makes this band conservative around floating-point boundaries;
    // the exact score check below still enforces the caller's threshold.
    let max_distance = ((1.0 - threshold) * longest as f64).ceil() as usize;
    let Some(distance) = bounded_levenshtein(&left, &right, max_distance, remaining_cells)? else {
        return Ok(false);
    };
    Ok(1.0 - distance as f64 / longest as f64 + f64::EPSILON >= threshold)
}

fn bounded_levenshtein(
    left: &[char],
    right: &[char],
    limit: usize,
    remaining_cells: &mut usize,
) -> Result<Option<usize>> {
    let common_prefix = left
        .iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count();
    let left = &left[common_prefix..];
    let right = &right[common_prefix..];
    let common_suffix = left
        .iter()
        .rev()
        .zip(right.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let left = &left[..left.len() - common_suffix];
    let right = &right[..right.len() - common_suffix];

    if left.len().abs_diff(right.len()) > limit {
        return Ok(None);
    }
    let initial_end = right.len().min(limit);
    let mut estimated_cells = initial_end + 1;
    for row in 1..=left.len() {
        let first_column = row.saturating_sub(limit);
        let last_column = row.saturating_add(limit).min(right.len());
        estimated_cells = estimated_cells
            .checked_add(last_column - first_column + 1)
            .ok_or_else(|| anyhow::anyhow!("fuzzy comparison work estimate overflow"))?;
    }
    if estimated_cells > *remaining_cells {
        bail!(
            "fuzzy comparison exhausted the hard {MAX_FUZZY_TOTAL_DP_CELLS}-cell total work budget before every candidate could be checked"
        );
    }
    *remaining_cells -= estimated_cells;

    let unreachable = limit.saturating_add(1);
    let mut previous: Vec<usize> = (0..=initial_end).collect();
    let mut previous_start = 0;

    for (row_index, left_char) in left.iter().enumerate() {
        let row = row_index + 1;
        let first_column = row.saturating_sub(limit);
        let last_column = row.saturating_add(limit).min(right.len());
        let mut current: Vec<usize> = Vec::with_capacity(last_column - first_column + 1);
        for column in first_column..=last_column {
            let value = if column == 0 {
                row
            } else {
                let substitution = band_value(&previous, previous_start, column - 1, unreachable)
                    .saturating_add(usize::from(*left_char != right[column - 1]));
                let deletion =
                    band_value(&previous, previous_start, column, unreachable).saturating_add(1);
                let insertion = if column > first_column {
                    current[column - first_column - 1].saturating_add(1)
                } else {
                    unreachable
                };
                substitution.min(deletion).min(insertion).min(unreachable)
            };
            current.push(value);
        }
        previous = current;
        previous_start = first_column;
    }

    let distance = band_value(&previous, previous_start, right.len(), unreachable);
    Ok((distance <= limit).then_some(distance))
}

fn band_value(values: &[usize], start: usize, column: usize, unreachable: usize) -> usize {
    column
        .checked_sub(start)
        .and_then(|index| values.get(index))
        .copied()
        .unwrap_or(unreachable)
}

fn build_replacement(source: &SourceFile, hunk: &Hunk, start: usize) -> Vec<SourceLine> {
    let mut source_offset = 0;
    let mut replacement = Vec::new();
    let mut patch_offset = 0;
    while patch_offset < hunk.lines.len() {
        match &hunk.lines[patch_offset] {
            PatchLine::Context(_) => {
                replacement.push(source.lines[start + source_offset].clone());
                source_offset += 1;
                patch_offset += 1;
            }
            PatchLine::Add(_) | PatchLine::Remove(_) => {
                let segment_start = patch_offset;
                while patch_offset < hunk.lines.len()
                    && !matches!(hunk.lines[patch_offset], PatchLine::Context(_))
                {
                    patch_offset += 1;
                }
                let segment = &hunk.lines[segment_start..patch_offset];
                let removed_endings: Vec<Option<LineEnding>> = segment
                    .iter()
                    .filter(|line| matches!(line, PatchLine::Remove(_)))
                    .enumerate()
                    .map(|(offset, _)| source.lines[start + source_offset + offset].ending)
                    .collect();
                let nearby_ending = removed_endings
                    .iter()
                    .copied()
                    .flatten()
                    .next()
                    .or_else(|| {
                        source
                            .lines
                            .get(start + source_offset)
                            .and_then(|line| line.ending)
                    })
                    .or_else(|| {
                        (start + source_offset)
                            .checked_sub(1)
                            .and_then(|index| source.lines.get(index))
                            .and_then(|line| line.ending)
                    })
                    .unwrap_or(source.preferred_ending);
                let mut add_offset = 0;
                for line in segment {
                    match line {
                        PatchLine::Remove(_) => source_offset += 1,
                        PatchLine::Add(text) => {
                            let ending = removed_endings
                                .get(add_offset)
                                .copied()
                                .flatten()
                                .or_else(|| removed_endings.last().copied().flatten())
                                .unwrap_or(nearby_ending);
                            replacement.push(SourceLine {
                                text: text.clone(),
                                ending: Some(ending),
                            });
                            add_offset += 1;
                        }
                        PatchLine::Context(_) => unreachable!("segment excludes context lines"),
                    }
                }
            }
        }
    }
    let final_destination_is_added = hunk
        .lines
        .iter()
        .rev()
        .find(|line| !matches!(line, PatchLine::Remove(_)))
        .is_some_and(|line| matches!(line, PatchLine::Add(_)));
    if start + source_offset == source.lines.len()
        && final_destination_is_added
        && let Some(last_line) = replacement.last_mut()
    {
        // A replacement that consumes EOF inherits the original final
        // terminator, even when several differently-terminated source lines
        // collapse into one. Suffix deletion is different: its surviving
        // context line keeps its own existing terminator in `render`.
        last_line.ending = source.trailing_ending;
    }
    replacement
}

fn apply_plans(mut source: SourceFile, plans: &[PlannedHunk]) -> Result<SourceFile> {
    let mut output = Vec::new();
    let mut source_cursor = 0;
    for plan in plans {
        if plan.start < source_cursor {
            bail!("patch hunks overlap or are not in source order");
        }
        output.extend_from_slice(&source.lines[source_cursor..plan.start]);
        output.extend(plan.replacement.iter().cloned());
        source_cursor = plan.start + plan.source_len;
    }
    output.extend_from_slice(&source.lines[source_cursor..]);
    source.lines = output;
    Ok(source)
}

#[cfg(unix)]
struct TempAt<'a> {
    parent: &'a OwnedFd,
    name: CString,
    keep: bool,
}

#[cfg(unix)]
impl Drop for TempAt<'_> {
    fn drop(&mut self) {
        if !self.keep {
            // SAFETY: `parent` remains live for this guard's lifetime and
            // `name` is NUL-terminated. unlinkat does not follow this name.
            let _ = unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) };
        }
    }
}

#[cfg(unix)]
fn create_temp_file_at(parent: &OwnedFd) -> Result<(TempAt<'_>, File)> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(
            ".prism-apply-patch-{}-{id}.tmp",
            std::process::id()
        ))
        .expect("generated temporary file name has no NUL bytes");
        // SAFETY: `parent` is a live directory descriptor, `name` is
        // NUL-terminated, and the mode argument is present because O_CREAT is
        // set. O_EXCL prevents opening an attacker-planted file.
        let raw_fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        let fd = match owned_fd(raw_fd) {
            Ok(fd) => fd,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("failed to create atomic patch file"),
        };
        let temporary = TempAt {
            parent,
            name,
            keep: false,
        };
        return Ok((temporary, File::from(fd)));
    }
    bail!("failed to create a unique atomic patch file after {TEMP_CREATE_ATTEMPTS} attempts")
}

#[cfg(unix)]
impl ConfinedTarget {
    fn atomic_replace(&self, expected: &[u8], updated: &[u8]) -> Result<()> {
        let (mut temporary, mut file) = create_temp_file_at(&self.parent)?;
        file.write_all(updated)
            .context("failed to write atomic patch file")?;
        // Set the final mode after writing because Unix may clear setuid or
        // setgid bits when file contents change.
        // SAFETY: `file` is live here and the mode contains only the original
        // target's permission and special bits.
        if unsafe { libc::fchmod(file.as_raw_fd(), self.permission_mode) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to preserve patch target permissions");
        }
        file.sync_all()
            .context("failed to sync atomic patch file")?;
        drop(file);

        let (mut current_file, metadata) =
            open_regular_file_at(self.parent.as_raw_fd(), &self.name, &self.relative)?;
        let current_identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let current_mode = metadata.mode() as libc::mode_t & 0o7777;
        if current_identity != self.identity || current_mode != self.permission_mode {
            bail!(
                "patch target changed identity or permissions while the patch was being prepared; refusing to overwrite it"
            );
        }
        let mut current = Vec::new();
        current_file.read_to_end(&mut current).with_context(|| {
            format!(
                "failed to re-read patch target '{}'",
                self.relative.display()
            )
        })?;
        if current != expected {
            bail!(
                "patch target changed while the patch was being prepared; refusing to overwrite it"
            );
        }
        drop(current_file);

        // SAFETY: both names are NUL-terminated and both directory arguments
        // are the same retained, live parent descriptor. renameat atomically
        // replaces the target without resolving any path component again.
        let rename_result = unsafe {
            libc::renameat(
                self.parent.as_raw_fd(),
                temporary.name.as_ptr(),
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
            )
        };
        if rename_result != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to atomically replace patch target '{}'",
                    self.relative.display()
                )
            });
        }
        temporary.keep = true;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn envelope(path: &str, hunks: &str) -> String {
        format!("{BEGIN_PATCH}\n{UPDATE_FILE}{path}\n{hunks}\n{END_PATCH}")
    }

    fn default_args(patch: String) -> Value {
        json!({ "patch": patch })
    }

    // Audit 2026-09-07 + the live log: 18 of the last 30 apply_patch calls
    // failed, and the tool's own words were the reason they kept failing.
    // Five were the empty envelope; four were hunks whose every line began
    // with a space (the model meant to append and had nothing to anchor on);
    // five had '+' lines identical to their '-' lines; one was the Codex
    // `*** Add File:` this tool is named after. Each error now says what was
    // wrong and what to send instead, and a file can be created.

    #[test]
    fn an_empty_patch_is_named_as_empty() {
        let root = tempdir().unwrap();
        let error = execute(
            root.path(),
            &default_args(format!("{BEGIN_PATCH}\n{END_PATCH}")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("empty"), "{error}");
        assert!(error.contains(UPDATE_FILE.trim()), "{error}");
        assert!(error.contains(ADD_FILE.trim()), "{error}");
    }

    #[test]
    fn a_context_only_hunk_is_told_how_to_add_lines() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("notes.md"), "one\n").unwrap();
        let patch = envelope("notes.md", "@@\n placeholder");
        let error = execute(root.path(), &default_args(patch))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no '+' or '-' line"), "{error}");
        assert!(error.contains("append"), "{error}");
        assert!(error.contains("last existing line"), "{error}");
    }

    #[test]
    fn identical_remove_and_add_lines_are_named_as_identical() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("notes.md"), "same\n").unwrap();
        let patch = envelope("notes.md", "@@\n-same\n+same");
        let error = execute(root.path(), &default_args(patch))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("'+' lines are identical to its '-' lines"),
            "{error}"
        );
    }

    #[test]
    fn add_file_creates_the_file_from_its_plus_lines() {
        let root = tempdir().unwrap();
        let patch = format!("{BEGIN_PATCH}\n{ADD_FILE}note.md\n+# Title\n+\n+body\n{END_PATCH}");
        let result = execute(root.path(), &default_args(patch)).expect("a new file is created");
        assert_eq!(result["created"], true);
        assert_eq!(
            fs::read_to_string(root.path().join("note.md")).unwrap(),
            "# Title\n\nbody\n"
        );
    }

    #[test]
    fn add_file_refuses_to_overwrite_and_names_update_file() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("note.md"), "already here\n").unwrap();
        let patch = format!("{BEGIN_PATCH}\n{ADD_FILE}note.md\n+replacement\n{END_PATCH}");
        let error = execute(root.path(), &default_args(patch))
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
        assert!(error.contains(UPDATE_FILE.trim()), "{error}");
        assert_eq!(
            fs::read_to_string(root.path().join("note.md")).unwrap(),
            "already here\n"
        );
    }

    #[test]
    fn ambiguous_context_is_refused_instead_of_using_first_match() {
        let root = tempdir().unwrap();
        let target = root.path().join("ambiguous.txt");
        let before = b"anchor\nold\nmiddle\nanchor\nold\nmiddle-2\nanchor\nold\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("ambiguous.txt", "@@\n anchor\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("ambiguous hunk 1: exact"));
        assert!(message.contains("at lines 1, 4"));
        assert!(!message.contains(", 7"));
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn whitespace_ambiguity_is_refused_instead_of_using_first_match() {
        let root = tempdir().unwrap();
        let target = root.path().join("whitespace-ambiguous.txt");
        let before = b"  anchor\n  old\nmiddle\n\tanchor\n\told\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("whitespace-ambiguous.txt", "@@\n anchor\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(error.to_string().contains("ambiguous hunk 1: whitespace"));
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn third_hunk_failure_leaves_file_byte_identical() {
        let root = tempdir().unwrap();
        let target = root.path().join("transactional.txt");
        let before = b"one\nold-a\ntwo\nold-b\nthree\nold-c\n";
        fs::write(&target, before).unwrap();
        let patch = envelope(
            "transactional.txt",
            "@@\n one\n-old-a\n+new-a\n@@\n two\n-old-b\n+new-b\n@@\n missing\n-old-c\n+new-c",
        );

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("hunk 3 source context was not found")
        );
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn drift_above_and_changed_indentation_apply_with_actual_context_preserved() {
        let root = tempdir().unwrap();
        let target = root.path().join("drift.rs");
        fs::write(
            &target,
            "inserted above\n    fn main() {\n        old();\n    }\n",
        )
        .unwrap();
        let patch = envelope(
            "drift.rs",
            "@@\n fn main() {\n-    old();\n+        new();\n }",
        );

        let result = execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(result["match_tiers"], json!(["whitespace"]));
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            "inserted above\n    fn main() {\n        new();\n    }\n"
        );
    }

    #[test]
    fn crlf_and_utf8_bom_round_trip_without_line_ending_conversion() {
        let root = tempdir().unwrap();
        let target = root.path().join("windows.txt");
        fs::write(&target, b"\xef\xbb\xbfalpha\r\nold\r\nomega\r\n").unwrap();
        let patch = envelope("windows.txt", "@@\n alpha\n-old\n+new\n omega");

        execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(
            fs::read(target).unwrap(),
            b"\xef\xbb\xbfalpha\r\nnew\r\nomega\r\n"
        );
    }

    #[test]
    fn no_trailing_newline_round_trips_as_unterminated() {
        let root = tempdir().unwrap();
        let target = root.path().join("unterminated.txt");
        fs::write(&target, b"alpha\nold").unwrap();
        let patch = envelope("unterminated.txt", "@@\n alpha\n-old\n+new");

        execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(fs::read(target).unwrap(), b"alpha\nnew");
    }

    #[test]
    fn replacement_preserves_the_exact_mixed_file_final_terminator() {
        let root = tempdir().unwrap();
        let target = root.path().join("mixed.txt");
        fs::write(&target, b"alpha\nold\r\n").unwrap();
        let patch = envelope("mixed.txt", "@@\n-old\n+new");

        execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(fs::read(target).unwrap(), b"alpha\nnew\r\n");
    }

    #[test]
    fn replacement_inherits_the_removed_lines_mixed_terminator() {
        let root = tempdir().unwrap();
        let target = root.path().join("mixed-internal.txt");
        fs::write(&target, b"first\nold\r\nlast\n").unwrap();
        let patch = envelope("mixed-internal.txt", "@@\n-old\n+new");

        execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(fs::read(target).unwrap(), b"first\nnew\r\nlast\n");
    }

    #[test]
    fn many_to_one_suffix_replacement_keeps_the_original_final_terminator() {
        let root = tempdir().unwrap();
        let target = root.path().join("mixed-many-to-one.txt");
        fs::write(&target, b"head\nold-a\r\nold-b\n").unwrap();
        let patch = envelope("mixed-many-to-one.txt", "@@\n-old-a\n-old-b\n+new");

        execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(fs::read(target).unwrap(), b"head\nnew\n");
    }

    #[test]
    fn suffix_deletion_keeps_the_surviving_lines_mixed_terminator() {
        let root = tempdir().unwrap();
        let target = root.path().join("mixed-suffix.txt");
        fs::write(&target, b"keep\r\nremove\n").unwrap();
        let patch = envelope("mixed-suffix.txt", "@@\n-remove");

        execute(root.path(), &default_args(patch)).unwrap();

        assert_eq!(fs::read(target).unwrap(), b"keep\r\n");
    }

    #[test]
    fn parent_directory_target_is_refused() {
        let root = tempdir().unwrap();
        let patch = envelope("../outside", "@@\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("normal relative path components")
        );
    }

    #[test]
    fn exact_only_policy_refuses_indentation_drift() {
        let root = tempdir().unwrap();
        let target = root.path().join("exact.txt");
        let before = b"    anchor\n    old\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("exact.txt", "@@\n anchor\n-old\n+new");
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": false
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("source context was not found"));
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn fuzzy_threshold_below_floor_is_refused() {
        let root = tempdir().unwrap();
        let target = root.path().join("floor.txt");
        fs::write(&target, b"old\n").unwrap();
        let patch = envelope("floor.txt", "@@\n-old\n+new");
        let args = json!({
            "patch": patch,
            "match_policy": { "fuzzy_similarity_threshold": 0.49 }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must be finite and between 0.5 and 1")
        );
    }

    #[test]
    fn fuzzy_match_applies_above_the_declared_floor() {
        let root = tempdir().unwrap();
        let target = root.path().join("fuzzy.txt");
        fs::write(&target, b"the color is blue\nfooter\n").unwrap();
        let patch = envelope("fuzzy.txt", "@@\n-the colour is blue\n+updated");
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.9,
                "fuzzy_window_lines": 1
            }
        });

        let result = execute(root.path(), &args).unwrap();

        assert_eq!(result["match_tiers"], json!(["fuzzy"]));
        assert_eq!(fs::read(target).unwrap(), b"updated\nfooter\n");
    }

    #[test]
    fn fuzzy_candidate_below_declared_threshold_is_refused() {
        let root = tempdir().unwrap();
        let target = root.path().join("below-threshold.txt");
        let before = b"the color is blue\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("below-threshold.txt", "@@\n-the colour is blue\n+updated");
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.99,
                "fuzzy_window_lines": 1
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("source context was not found"));
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn fuzzy_candidate_window_bounds_start_positions() {
        let root = tempdir().unwrap();
        let target = root.path().join("fuzzy-window.txt");
        let before = b"zero\none\ntwo\nthe color is blue\nend\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("fuzzy-window.txt", "@@\n-the colour is blue\n+updated");
        let small_window = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.9,
                "fuzzy_window_lines": 3
            }
        });

        let error = execute(root.path(), &small_window).unwrap_err();
        assert!(error.to_string().contains("source context was not found"));
        assert_eq!(fs::read(&target).unwrap(), before);

        let mut sufficient_window = small_window;
        sufficient_window["match_policy"]["fuzzy_window_lines"] = json!(4);
        let result = execute(root.path(), &sufficient_window).unwrap();
        assert_eq!(result["match_tiers"], json!(["fuzzy"]));
        assert_eq!(fs::read(target).unwrap(), b"zero\none\ntwo\nupdated\nend\n");
    }

    #[test]
    fn fuzzy_matching_does_not_hide_whitespace_when_relaxation_is_disabled() {
        let root = tempdir().unwrap();
        let target = root.path().join("fuzzy-whitespace.txt");
        let before = b"                                        anchor\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("fuzzy-whitespace.txt", "@@\n-anchor\n+updated");
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.9,
                "fuzzy_window_lines": 1
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("source context was not found"));
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[test]
    fn fuzzy_source_block_over_hard_byte_cap_is_refused_without_writing() {
        let root = tempdir().unwrap();
        let target = root.path().join("giant.txt");
        let actual = "b".repeat(MAX_FUZZY_BLOCK_BYTES + 1);
        let expected = "a".repeat(MAX_FUZZY_BLOCK_BYTES + 1);
        let before = format!("{actual}\n");
        fs::write(&target, before.as_bytes()).unwrap();
        let patch = envelope("giant.txt", &format!("@@\n-{expected}\n+updated"));
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.5,
                "fuzzy_window_lines": 1
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("hard 65536-byte safety cap"));
        assert_eq!(fs::read(target).unwrap(), before.as_bytes());
    }

    #[test]
    fn fuzzy_total_work_budget_refuses_before_using_partial_candidate_results() {
        let root = tempdir().unwrap();
        let target = root.path().join("fuzzy-budget.txt");
        let candidate = "b".repeat(1_000);
        let expected = "a".repeat(1_000);
        let before = format!(
            "{}\n",
            std::iter::repeat_n(candidate.as_str(), 6)
                .collect::<Vec<_>>()
                .join("\n")
        );
        fs::write(&target, before.as_bytes()).unwrap();
        let patch = envelope("fuzzy-budget.txt", &format!("@@\n-{expected}\n+updated"));
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.5,
                "fuzzy_window_lines": 6
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("total work budget"));
        assert_eq!(fs::read(target).unwrap(), before.as_bytes());
    }

    #[test]
    fn high_threshold_long_fuzzy_comparison_uses_a_narrow_band() {
        let root = tempdir().unwrap();
        let target = root.path().join("narrow-band.txt");
        let actual = "b".repeat(20_000);
        let expected = "a".repeat(20_000);
        let before = format!("{actual}\n");
        fs::write(&target, before.as_bytes()).unwrap();
        let patch = envelope("narrow-band.txt", &format!("@@\n-{expected}\n+updated"));
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.9999,
                "fuzzy_window_lines": 1
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("source context was not found"));
        assert_eq!(fs::read(target).unwrap(), before.as_bytes());
    }

    #[test]
    fn narrow_band_charges_only_the_cells_it_allocates() {
        let left = vec!['a'; 20_000];
        let right = vec!['b'; 20_000];
        let mut remaining_cells = 100_000;

        let distance = bounded_levenshtein(&left, &right, 2, &mut remaining_cells).unwrap();

        assert_eq!(distance, None);
        assert_eq!(remaining_cells, 1);
    }

    #[test]
    fn fuzzy_work_budget_is_shared_across_all_hunks() {
        let root = tempdir().unwrap();
        let target = root.path().join("shared-budget.txt");
        let length = 2_002;
        let actual_one = "a".repeat(length);
        let actual_two = "c".repeat(length);
        let expected_one: String = (0..length)
            .map(|index| {
                if (index % 2 == 0 && index < length - 2) || index == length - 1 {
                    'b'
                } else {
                    'a'
                }
            })
            .collect();
        let expected_two: String = (0..length)
            .map(|index| {
                if (index % 2 == 0 && index < length - 2) || index == length - 1 {
                    'd'
                } else {
                    'c'
                }
            })
            .collect();
        let before = format!("{actual_one}\n{actual_two}\n");
        fs::write(&target, before.as_bytes()).unwrap();
        let patch = envelope(
            "shared-budget.txt",
            &format!("@@\n-{expected_one}\n+updated-one\n@@\n-{expected_two}\n+updated-two"),
        );
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.5,
                "fuzzy_window_lines": 1
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        assert!(error.to_string().contains("total work budget"));
        assert_eq!(fs::read(target).unwrap(), before.as_bytes());
    }

    #[test]
    fn multiple_fuzzy_candidates_above_threshold_are_refused() {
        let root = tempdir().unwrap();
        let target = root.path().join("fuzzy-ambiguous.txt");
        let before = b"alpha one\nseparator\nalpha two\nseparator\nalpha six\n";
        fs::write(&target, before).unwrap();
        let patch = envelope("fuzzy-ambiguous.txt", "@@\n-alpha xxx\n+updated");
        let args = json!({
            "patch": patch,
            "match_policy": {
                "allow_whitespace": false,
                "allow_fuzzy": true,
                "fuzzy_similarity_threshold": 0.6,
                "fuzzy_window_lines": 5
            }
        });

        let error = execute(root.path(), &args).unwrap_err();

        let message = error.to_string();
        assert!(message.contains("ambiguous hunk 1: fuzzy"));
        assert!(message.contains("at lines 1, 3"));
        assert!(!message.contains(", 5"));
        assert_eq!(fs::read(target).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_refused() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let real_target = root.path().join("real.txt");
        fs::write(&real_target, b"old\n").unwrap();
        symlink("real.txt", root.path().join("link.txt")).unwrap();
        let patch = envelope("link.txt", "@@\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(error.to_string().contains("may not be a symlink"));
        assert_eq!(fs::read(real_target).unwrap(), b"old\n");
    }

    #[test]
    fn fifo_target_is_refused_without_blocking() {
        let root = tempdir().unwrap();
        let fifo = root.path().join("pipe");
        let fifo_path = os_str_to_cstring(fifo.as_os_str(), "FIFO test path").unwrap();
        // SAFETY: `fifo_path` is a live NUL-terminated path in a temporary
        // directory owned by this test.
        let result = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "mkfifo failed: {}",
            std::io::Error::last_os_error()
        );
        let patch = envelope("pipe", "@@\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(error.to_string().contains("existing regular file"));
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlink_to_inside_is_refused() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let real_directory = root.path().join("real");
        fs::create_dir(&real_directory).unwrap();
        let real_target = real_directory.join("target.txt");
        fs::write(&real_target, b"old\n").unwrap();
        symlink("real", root.path().join("linked")).unwrap();
        let patch = envelope("linked/target.txt", "@@\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(error.to_string().contains("directory component 'linked'"));
        assert_eq!(fs::read(real_target).unwrap(), b"old\n");
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlink_to_outside_is_refused() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_target = outside.path().join("target.txt");
        fs::write(&outside_target, b"old\n").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();
        let patch = envelope("linked/target.txt", "@@\n-old\n+new");

        let error = execute(root.path(), &default_args(patch)).unwrap_err();

        assert!(error.to_string().contains("directory component 'linked'"));
        assert_eq!(fs::read(outside_target).unwrap(), b"old\n");
    }

    #[test]
    fn parser_refuses_multiple_targets_and_context_only_hunks() {
        let multiple = format!(
            "{BEGIN_PATCH}\n{UPDATE_FILE}one\n@@\n-a\n+b\n{UPDATE_FILE}two\n@@\n-c\n+d\n{END_PATCH}"
        );
        let no_change = envelope("one", "@@\n context");

        assert!(
            parse_patch(&multiple)
                .unwrap_err()
                .to_string()
                .contains("multiple target sections")
        );
        assert!(
            parse_patch(&no_change)
                .unwrap_err()
                .to_string()
                .contains("no '+' or '-' line")
        );
    }
}
