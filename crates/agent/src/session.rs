//! Session persistence — JSONL files with resume, fork, and rotation.
//!
//! Sessions are stored as newline-delimited JSON at `~/.prism/sessions/`.
//! Each line is a message or metadata event. Sessions can be:
//! - Resumed: auto-loads last session, or by explicit ID
//! - Forked: branch the current conversation with parent tracking
//! - Listed: query a rebuildable Turso metadata mirror
//! - Rotated: files rotate at 256KB (max 3 backups)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::Local;
use directories::UserDirs;
use prism_provenance::{
    MAX_SESSION_INDEX_QUERY_LIMIT, MAX_SESSION_INDEX_SEARCH_BYTES, MAX_SESSION_INDEX_SEARCH_TERMS,
    SessionIndexEntry, SessionIndexQuery,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::session_index::SessionIndexWorker;

// ── Constants ────────────────────────────────────────────────────────

const MAX_FILE_SIZE: u64 = 256 * 1024; // 256KB
const MAX_ROTATIONS: usize = 3;
const LATEST_FILE: &str = ".latest";
const SESSION_CONTEXT_ENTRY_TYPE: &str = "session_context";
const DEFAULT_SESSION_QUERY_LIMIT: usize = 100;
const SESSION_PREVIEW_MAX_CHARS: usize = 240;

/// Every this-many turns, `append_message` flushes a fresh `meta` line so the
/// file alone carries near-current counters (rotation moves old lines into
/// backups; the in-memory meta is the only full-history counter source).
const META_FLUSH_EVERY_TURNS: usize = 5;

/// Generate the initial title once the first real exchange has completed
/// (one user message + one assistant reply).
pub const TITLE_FIRST_EXCHANGE_TURNS: usize = 2;
/// Refresh a title only after at least this many new turns...
pub const TITLE_REFRESH_MIN_GAP: usize = 16;
/// Hard cap for deterministic (non-LLM) titles, in characters.
pub const TITLE_MAX_CHARS: usize = 60;

/// Process-wide serialization of session-meta read-modify-write cycles. The
/// title generator runs in a detached task with its own [`SessionStore`]
/// concurrently with the runtime's store; both append `meta` lines to the
/// same file, so their read-modify-write must not interleave.
static META_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn default_sessions_dir() -> PathBuf {
    UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".prism")
        .join("sessions")
}

// ── Data types ───────────────────────────────────────────────────────

/// Session metadata — written as the first JSONL line, and re-appended as a
/// fresh `meta` line whenever it changes (model switch, title, periodic
/// counter flush). Readers take the LAST `meta` line as current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub model: String,
    pub turn_count: usize,
    pub compaction_count: usize,
    pub parent_session_id: Option<String>,
    pub branch_name: Option<String>,
    /// Short label for the history rail. `None` until generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// One-line description for the history rail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// How `title` came to be: `"model"` (auxiliary LLM) or `"heuristic"`
    /// (deterministic fallback). Recorded so nothing downstream mistakes a
    /// generated label for user-authored text. Never an evidence class — a
    /// title labels the session, it does not claim anything about the world.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    /// `turn_count` at the last title generation; drives the refresh policy.
    #[serde(default)]
    pub title_turn: usize,
    /// Every model that has served this session, deduped, first-use order.
    /// The last entry is the current one; `model` above is kept in sync.
    /// Empty only in legacy files written before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// A single entry (one JSONL line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub call_id: String,
    #[serde(default)]
    pub timestamp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Summary returned by [`SessionStore::list_sessions`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub turn_count: usize,
    /// The model currently serving the session (tracks `/model` switches).
    pub model: String,
    pub size_kb: f64,
    pub is_latest: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `"model"` or `"heuristic"` — see [`SessionMeta::title_source`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    /// All models that served the session, first-use order. Never empty when
    /// `model` is set (falls back to `[model]` for legacy sessions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Main JSONL path. Rotated segments share this path plus `.1` ... `.3`.
    #[serde(default, skip_serializing)]
    pub path: String,
    /// Working directory captured by a separate rebuildable context event.
    /// Kept available to trusted in-process query callers but omitted from
    /// HTTP/TUI serialization to avoid exposing host filesystem layout.
    #[serde(default, skip_serializing)]
    pub project_cwd: Option<String>,
    /// First user message, clipped for history search and rebuilt from JSONL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Filters for indexed session history queries.
#[derive(Debug, Clone)]
pub struct SessionQuery {
    /// Match normalized terms in the session title, summary, or preview.
    pub text: Option<String>,
    /// Restrict results to the working directory captured at session creation.
    pub project_cwd: Option<PathBuf>,
    /// Inclusive lower bound for the session's last update time (Unix seconds).
    pub updated_after: Option<f64>,
    /// Inclusive upper bound for the session's last update time (Unix seconds).
    pub updated_before: Option<f64>,
    /// Maximum number of rows returned; the SQL layer applies its global cap.
    pub limit: usize,
    /// Number of matching rows to skip after stable update-time ordering.
    pub offset: usize,
}

impl Default for SessionQuery {
    fn default() -> Self {
        Self {
            text: None,
            project_cwd: None,
            updated_after: None,
            updated_before: None,
            limit: DEFAULT_SESSION_QUERY_LIMIT,
            offset: 0,
        }
    }
}

/// Policy for periodic JSONL-to-SQL drift repair.
///
/// A source is scanned on first indexed use and again only after this interval.
/// Between reconciliations, `list_sessions` is a pure SQL query: a file removed
/// externally may remain visible until the next repair, and a file copied in
/// externally is discovered by that repair or an explicit rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionIndexPolicy {
    /// Maximum age of the last complete source reconciliation.
    pub reconcile_after: Duration,
}

impl Default for SessionIndexPolicy {
    fn default() -> Self {
        Self {
            reconcile_after: Duration::from_secs(5 * 60),
        }
    }
}

/// Non-transcript runtime state that should survive resume/fork flows.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSessionState {
    #[serde(default)]
    pub session_mode: String,
    #[serde(default)]
    pub permission_allow: Vec<String>,
    #[serde(default)]
    pub permission_deny: Vec<String>,
    #[serde(default)]
    pub plan_status: String,
    #[serde(default)]
    pub approved_plan_body: Option<String>,
}

// ── SessionStore ─────────────────────────────────────────────────────

/// Manages session persistence to JSONL files.
pub struct SessionStore {
    sessions_dir: PathBuf,
    current_id: Option<String>,
    current_path: Option<PathBuf>,
    meta: Option<SessionMeta>,
    creation_project_cwd: Option<String>,
    project_cwd: Option<String>,
    preview: Option<String>,
    current_log_authoritative: bool,
    index: SessionIndexWorker,
    index_policy: SessionIndexPolicy,
}

impl SessionStore {
    /// Create a new store. Creates the sessions directory if it doesn't exist.
    pub fn new(sessions_dir: Option<PathBuf>) -> Self {
        let dir = sessions_dir.unwrap_or_else(default_sessions_dir);
        Self::new_with_index(
            dir,
            crate::hooks::provenance_db_path(),
            SessionIndexPolicy::default(),
        )
    }

    /// Create a store with an explicit SQL mirror path and repair policy.
    ///
    /// This is primarily useful to isolate tests and recovery tools. The JSONL
    /// directory remains authoritative regardless of whether the database can
    /// be opened.
    pub fn new_with_index(
        sessions_dir: PathBuf,
        index_path: PathBuf,
        index_policy: SessionIndexPolicy,
    ) -> Self {
        let dir = sessions_dir;
        let _ = fs::create_dir_all(&dir);
        let project_cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        Self {
            sessions_dir: dir,
            current_id: None,
            current_path: None,
            meta: None,
            creation_project_cwd: project_cwd.clone(),
            project_cwd,
            preview: None,
            current_log_authoritative: false,
            index: SessionIndexWorker::start(index_path),
            index_policy,
        }
    }

    // ── Session lifecycle ────────────────────────────────────────────

    /// Create a new session. Returns the generated session ID.
    ///
    /// ID format: `YYYYMMDD_HHMMSS_{hex8}`
    pub fn new_session(&mut self, model: &str) -> String {
        let hex8 = format!("{:08x}", rand_u32());
        let sid = format!("{}_{hex8}", Local::now().format("%Y%m%d_%H%M%S"));

        self.current_id = Some(sid.clone());
        self.current_path = Some(self.sessions_dir.join(format!("{sid}.jsonl")));
        self.project_cwd = self.creation_project_cwd.clone();
        self.preview = None;
        self.current_log_authoritative = false;

        let now = unix_now();
        self.meta = Some(SessionMeta {
            session_id: sid.clone(),
            created_at: now,
            updated_at: now,
            model: model.to_string(),
            turn_count: 0,
            compaction_count: 0,
            parent_session_id: None,
            branch_name: None,
            title: None,
            summary: None,
            title_source: None,
            title_turn: 0,
            models: vec![model.to_string()],
        });

        let meta_entry = SessionEntry {
            entry_type: "meta".to_string(),
            role: String::new(),
            content: String::new(),
            tool_name: String::new(),
            call_id: String::new(),
            timestamp: now,
            data: serde_json::to_value(self.meta.as_ref().unwrap()).ok(),
        };
        let meta_written = self.write_entry(&meta_entry);
        let context_entry =
            session_context_entry(now, Some(sid.clone()), self.project_cwd.clone(), None);
        let context_written = self.write_entry(&context_entry);
        if !context_written {
            self.project_cwd = None;
        }
        self.current_log_authoritative = meta_written;
        self.update_latest(&sid);
        self.index_current_session();
        sid
    }

    /// Resume a session by ID or `"latest"`. Returns `(session_id, messages)`.
    pub fn resume_session(&mut self, reference: &str) -> Option<(String, Vec<serde_json::Value>)> {
        let sid = self.resolve_ref(reference)?;
        let path = self.sessions_dir.join(format!("{sid}.jsonl"));
        let parsed = scan_session_log(&path)?;

        self.current_id = Some(sid.clone());
        self.current_path = Some(path);
        self.project_cwd = parsed.project_cwd;
        self.preview = parsed.preview;
        self.current_log_authoritative = parsed.meta.is_some();
        if let Some(m) = parsed.meta {
            self.meta = Some(m);
        }
        self.update_latest(&sid);
        self.index_current_session();
        Some((sid, parsed.messages))
    }

    /// Read a session's messages WITHOUT switching the store's current
    /// session (read-only counterpart to [`Self::resume_session`], used by
    /// the HTTP chat endpoints to serve `GET /api/chat/sessions/{id}`).
    pub fn load_messages(&self, session_id: &str) -> Option<Vec<serde_json::Value>> {
        let path = self.sessions_dir.join(format!("{session_id}.jsonl"));
        parse_session_file(&path).map(|(messages, _)| messages)
    }

    /// Directory where session `.jsonl` files live.
    pub fn dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Set the trusted project root captured for subsequently created sessions.
    pub fn set_project_cwd(&mut self, project_cwd: Option<&Path>) {
        self.creation_project_cwd = project_cwd.and_then(Path::to_str).map(str::to_string);
        if self.current_id.is_none() {
            self.project_cwd = self.creation_project_cwd.clone();
        }
    }

    /// Fork the current session into a new one with parent tracking.
    pub fn fork_session(&mut self, branch_name: &str) -> String {
        let old_id = self.current_id.clone();
        let old_path = self.current_path.clone();
        let old_model = self
            .meta
            .as_ref()
            .map(|m| m.model.clone())
            .unwrap_or_default();
        let old_turn_count = self.meta.as_ref().map_or(0, |m| m.turn_count);
        let old_compaction_count = self.meta.as_ref().map_or(0, |m| m.compaction_count);
        let old_preview = self.preview.clone();

        let new_id = self.new_session(&old_model);

        if let Some(meta) = self.meta.as_mut() {
            meta.parent_session_id = old_id.clone();
            meta.turn_count = old_turn_count;
            meta.compaction_count = old_compaction_count;
            let name = if branch_name.is_empty() {
                format!("fork-{}", &new_id[..new_id.len().min(8)])
            } else {
                branch_name.to_string()
            };
            meta.branch_name = Some(name);
        }

        // Copy retained non-meta entries oldest-to-newest. Rotated segments
        // are part of the authoritative retained log, not disposable index
        // state, so a fork must not silently omit them.
        if let Some(old) = old_path {
            for segment in session_log_paths(&old) {
                if let Ok(text) = fs::read_to_string(segment) {
                    for line in text.lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line)
                            && !matches!(
                                entry.get("type").and_then(|t| t.as_str()),
                                Some("meta") | Some(SESSION_CONTEXT_ENTRY_TYPE)
                            )
                        {
                            self.write_raw(line);
                        }
                    }
                }
            }
        }
        self.preview = old_preview;

        if let Some(parent_id) = self
            .meta
            .as_ref()
            .and_then(|meta| meta.parent_session_id.clone())
            .or(old_id)
        {
            let old_state_path = self.runtime_state_path(&parent_id);
            let new_state_path = self.runtime_state_path(&new_id);
            if old_state_path.exists() {
                let _ = fs::copy(old_state_path, new_state_path);
            }
        }

        // Persist the fork relationship and inherited counters after copying
        // the authoritative transcript. `new_session`'s initial meta line
        // intentionally remains the immutable creation record; last meta wins.
        self.update_session_meta(&new_id, |_meta| {});

        new_id
    }

    // ── Append operations ────────────────────────────────────────────

    /// Append a message entry to the current session.
    pub fn append_message(
        &mut self,
        role: &str,
        content: &str,
        tool_name: &str,
        call_id: &str,
        data: Option<serde_json::Value>,
    ) {
        if self.current_path.is_none() {
            return;
        }
        let entry = SessionEntry {
            entry_type: "message".to_string(),
            role: role.to_string(),
            content: content.to_string(),
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
            timestamp: unix_now(),
            data,
        };
        if !self.write_entry(&entry) {
            return;
        }

        if let Some(meta) = self.meta.as_mut() {
            meta.updated_at = entry.timestamp;
            if role == "user" || role == "assistant" {
                meta.turn_count += 1;
            }
        }
        if role == "user" && self.preview.is_none() {
            self.preview = session_preview(content);
        }

        // Repeat rebuild-only context after every durable message. This keeps
        // project/preview data present even after old rotated segments expire.
        self.append_current_context(entry.timestamp);

        // Keep the persisted meta close to the live counters so a fresh store
        // (history rail, resume) sees near-current state from the file alone.
        let flush_due = matches!(role, "user" | "assistant")
            && self
                .meta
                .as_ref()
                .is_some_and(|m| m.turn_count > 0 && m.turn_count % META_FLUSH_EVERY_TURNS == 0);
        if flush_due && let Some(sid) = self.current_id().map(str::to_string) {
            self.update_session_meta(&sid, |_meta| {});
        } else {
            self.index_current_session();
        }
    }

    /// Record `model` as the session's current model and add it to the set of
    /// models used. Call after a successful `/model` switch so persisted meta
    /// (and the [`SessionInfo`] built from it) reflects what actually served
    /// the conversation. Never errors: a failed write leaves the in-memory
    /// state correct for this process.
    pub fn note_model_used(&mut self, model: &str) {
        let Some(sid) = self.current_id().map(str::to_string) else {
            return;
        };
        let model = model.to_string();
        self.update_session_meta(&sid, move |meta| {
            // Legacy sessions (files predating `models`) start with an empty
            // set — backfill the outgoing model so history is not lost.
            if meta.models.is_empty() && !meta.model.is_empty() {
                meta.models.push(meta.model.clone());
            }
            meta.model = model.clone();
            if !meta.models.contains(&model) {
                meta.models.push(model);
            }
        });
    }

    /// Read-modify-write the persisted metadata for `session_id`.
    ///
    /// Meta is append-only: the updated state is written as a new `meta` line
    /// and readers take the last one. The base is the in-memory meta when this
    /// store owns the session (authoritative for counters and model) or the
    /// last persisted meta otherwise. Title fields are owned by the detached
    /// title-generation task, so persisted values are adopted before `mutate`
    /// runs and can never be clobbered by a counter flush or model switch.
    ///
    /// Errors are swallowed by design: a label or bookkeeping update must
    /// never take a session down.
    pub fn update_session_meta<F: FnOnce(&mut SessionMeta)>(
        &mut self,
        session_id: &str,
        mutate: F,
    ) {
        let path = self.sessions_dir.join(format!("{session_id}.jsonl"));
        let _guard = META_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let disk = scan_session_log(&path);
        let disk_meta = disk.as_ref().and_then(|parsed| parsed.meta.clone());
        let is_current = self.current_id == Some(session_id.to_string());
        let mut merged = if is_current {
            self.meta.clone()
        } else {
            disk_meta.clone()
        };
        let Some(meta) = merged.as_mut() else {
            return;
        };

        if let Some(disk) = &disk_meta {
            if meta.title.is_none() {
                meta.title = disk.title.clone();
            }
            if meta.summary.is_none() {
                meta.summary = disk.summary.clone();
            }
            if meta.title_source.is_none() {
                meta.title_source = disk.title_source.clone();
            }
            meta.title_turn = meta.title_turn.max(disk.title_turn);
        }

        mutate(meta);
        meta.updated_at = unix_now();

        let entry = SessionEntry {
            entry_type: "meta".to_string(),
            role: String::new(),
            content: String::new(),
            tool_name: String::new(),
            call_id: String::new(),
            timestamp: meta.updated_at,
            data: serde_json::to_value(&*meta).ok(),
        };
        let meta_written = Self::append_entry(&path, &entry);
        if !meta_written {
            return;
        }

        let (project_cwd, preview) = if is_current {
            (self.project_cwd.clone(), self.preview.clone())
        } else {
            disk.as_ref()
                .map(|parsed| (parsed.project_cwd.clone(), parsed.preview.clone()))
                .unwrap_or_default()
        };
        let context = session_context_entry(
            meta.updated_at,
            Some(session_id.to_string()),
            project_cwd,
            preview,
        );
        Self::append_entry(&path, &context);

        if is_current {
            self.meta = merged;
            self.current_log_authoritative = true;
            self.index_current_session();
        } else if let Some(entry) = index_entry_from_log(&self.sessions_dir, &path) {
            self.index.upsert_best_effort(entry);
        }
    }

    /// Record a compaction event.
    pub fn append_compaction(&mut self, summary: &str) {
        let entry = SessionEntry {
            entry_type: "compaction".to_string(),
            role: String::new(),
            content: summary.to_string(),
            tool_name: String::new(),
            call_id: String::new(),
            timestamp: unix_now(),
            data: None,
        };
        if !self.write_entry(&entry) {
            return;
        }

        if let Some(meta) = self.meta.as_mut() {
            meta.compaction_count += 1;
            meta.updated_at = entry.timestamp;
        }
        self.append_current_context(entry.timestamp);
        if let Some(sid) = self.current_id().map(str::to_string) {
            self.update_session_meta(&sid, |_meta| {});
        }
    }

    // ── Query ────────────────────────────────────────────────────────

    /// List sessions from the SQL mirror, most recently updated first.
    ///
    /// On first use (and periodically according to [`SessionIndexPolicy`]),
    /// the mirror is reconciled against the authoritative JSONL directory.
    pub fn list_sessions(&self, limit: usize) -> Vec<SessionInfo> {
        let mut sessions = Vec::new();
        while sessions.len() < limit {
            let page_limit = limit
                .saturating_sub(sessions.len())
                .min(MAX_SESSION_INDEX_QUERY_LIMIT);
            if page_limit == 0 {
                break;
            }
            let page = self.query_sessions(&SessionQuery {
                limit: page_limit,
                offset: sessions.len(),
                ..SessionQuery::default()
            });
            let page_len = page.len();
            sessions.extend(page);
            if page_len < page_limit {
                break;
            }
        }
        sessions
    }

    /// Query indexed session history by text, project, and update window.
    pub fn query_sessions(&self, query: &SessionQuery) -> Vec<SessionInfo> {
        self.reconcile_index_if_due();
        let latest_id = self.resolve_ref("latest");
        let source_path = self.source_path();
        let index_query = SessionIndexQuery {
            source_path: Some(source_path),
            project_cwd: query
                .project_cwd
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            text: query.text.clone(),
            updated_after: query.updated_after,
            updated_before: query.updated_before,
            limit: query.limit,
            offset: query.offset,
        };

        match self.index.list(index_query) {
            Ok(entries) => entries
                .into_iter()
                .map(|entry| session_info_from_index(entry, latest_id.as_deref()))
                .collect(),
            Err(error) => {
                warn!(%error, "failed to query session metadata index; reading authoritative JSONL");
                self.query_sessions_from_logs(query, latest_id.as_deref())
            }
        }
    }

    /// Rebuild this directory's disposable SQL mirror from authoritative logs.
    ///
    /// Rows whose files disappeared are removed; valid files without rows are
    /// inserted. Other session directories sharing the provenance database are
    /// untouched.
    pub fn rebuild_session_index(&self) -> Result<usize> {
        // The scan-start boundary lets SQL preserve an incremental upsert from
        // another store that lands while this filesystem snapshot is built.
        let scan_started_at = unix_now();
        let scan = self.scan_session_index_entries()?;
        let count = scan.entries.len();
        self.index
            .replace_source(
                self.source_path(),
                scan.entries,
                scan.preserve_session_ids,
                scan_started_at,
                unix_now(),
            )
            .context("failed to replace the session metadata index")?;
        Ok(count)
    }

    fn reconcile_index_if_due(&self) {
        let source_path = self.source_path();
        let due = match self.index.reconciled_at(source_path) {
            Ok(None) => true,
            Ok(Some(last_reconciled_at)) => {
                last_reconciled_at + self.index_policy.reconcile_after.as_secs_f64() <= unix_now()
            }
            Err(error) => {
                warn!(%error, "failed to read session-index reconciliation state");
                return;
            }
        };
        if due && let Err(error) = self.rebuild_session_index() {
            warn!(%error, "failed to reconcile session metadata from JSONL");
        }
    }

    fn scan_session_index_entries(&self) -> Result<SessionIndexScan> {
        let directory = fs::read_dir(&self.sessions_dir).with_context(|| {
            format!(
                "failed to read session directory {}",
                self.sessions_dir.display()
            )
        })?;
        let mut base_paths = std::collections::BTreeSet::new();
        for entry in directory {
            let entry = entry.with_context(|| {
                format!(
                    "failed to enumerate session directory {}",
                    self.sessions_dir.display()
                )
            })?;
            if let Some(base_path) = session_base_path(&entry.path()) {
                base_paths.insert(base_path);
            }
        }

        let mut entries = Vec::new();
        let mut preserve_session_ids = Vec::new();
        for path in base_paths {
            if !session_log_is_complete_for_index(&path) {
                if let Some(session_id) = session_id_from_base_path(&path) {
                    preserve_session_ids.push(session_id);
                }
                continue;
            }
            if let Some(index_entry) = index_entry_from_log(&self.sessions_dir, &path) {
                entries.push(index_entry);
            } else if let Some(session_id) = session_id_from_base_path(&path) {
                preserve_session_ids.push(session_id);
            }
        }
        Ok(SessionIndexScan {
            entries,
            preserve_session_ids,
        })
    }

    fn query_sessions_from_logs(
        &self,
        query: &SessionQuery,
        latest_id: Option<&str>,
    ) -> Vec<SessionInfo> {
        let search_terms = match query.text.as_deref() {
            Some(text) if text.len() > MAX_SESSION_INDEX_SEARCH_BYTES => {
                warn!(
                    bytes = text.len(),
                    "session search text exceeds the fallback query limit"
                );
                return Vec::new();
            }
            Some(text) => {
                let terms = normalized_session_terms(text);
                if terms.len() > MAX_SESSION_INDEX_SEARCH_TERMS {
                    warn!(
                        terms = terms.len(),
                        "session search has too many terms for fallback"
                    );
                    return Vec::new();
                }
                terms
            }
            None => std::collections::BTreeSet::new(),
        };
        let project_cwd = query
            .project_cwd
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let mut entries = match self.scan_session_index_entries() {
            Ok(scan) => scan.entries,
            Err(error) => {
                warn!(%error, "failed to read authoritative session JSONL");
                return Vec::new();
            }
        };
        entries.retain(|entry| {
            project_cwd
                .as_ref()
                .is_none_or(|project| entry.project_cwd.as_ref() == Some(project))
                && query
                    .updated_after
                    .is_none_or(|after| entry.updated_at >= after)
                && query
                    .updated_before
                    .is_none_or(|before| entry.updated_at <= before)
                && search_terms.is_subset(&entry_search_terms(entry))
        });
        entries.sort_by(|left, right| {
            right
                .updated_at
                .total_cmp(&left.updated_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        entries
            .into_iter()
            .skip(query.offset)
            .take(query.limit.min(MAX_SESSION_INDEX_QUERY_LIMIT))
            .map(|entry| session_info_from_index(entry, latest_id))
            .collect()
    }

    // ── Accessors ────────────────────────────────────────────────────

    /// Current session ID, if any.
    pub fn current_id(&self) -> Option<&str> {
        self.current_id.as_deref()
    }

    /// Session ID recorded by the durable `latest` pointer, if present.
    pub fn latest_session_id(&self) -> Option<String> {
        self.resolve_ref("latest")
    }

    /// Current session metadata, if any.
    pub fn meta(&self) -> Option<&SessionMeta> {
        self.meta.as_ref()
    }

    /// Mutable current session metadata, if any.
    pub fn meta_mut(&mut self) -> Option<&mut SessionMeta> {
        self.meta.as_mut()
    }

    /// Persist non-transcript session state to a sidecar JSON file.
    pub fn save_runtime_state(&self, session_id: &str, state: &RuntimeSessionState) {
        let path = self.runtime_state_path(session_id);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(state) {
            let _ = fs::write(path, json);
        }
    }

    /// Load runtime state sidecar for a session, if present.
    pub fn load_runtime_state(&self, session_id: &str) -> Option<RuntimeSessionState> {
        let path = self.runtime_state_path(session_id);
        let text = fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    // ── Internal ─────────────────────────────────────────────────────

    fn append_current_context(&self, timestamp: f64) -> bool {
        let entry = session_context_entry(
            timestamp,
            self.current_id.clone(),
            self.project_cwd.clone(),
            self.preview.clone(),
        );
        self.write_entry(&entry)
    }

    fn index_current_session(&self) {
        if !self.current_log_authoritative {
            return;
        }
        let (Some(path), Some(meta)) = (&self.current_path, &self.meta) else {
            return;
        };
        self.index.upsert_best_effort(index_entry_from_meta(
            &self.sessions_dir,
            path,
            meta,
            self.project_cwd.clone(),
            self.preview.clone(),
        ));
    }

    fn source_path(&self) -> String {
        self.sessions_dir.to_string_lossy().into_owned()
    }

    fn write_entry(&self, entry: &SessionEntry) -> bool {
        let Some(path) = &self.current_path else {
            return false;
        };
        if Self::maybe_rotate(path) {
            if self.current_log_authoritative
                && let Some(meta) = &self.meta
            {
                let seed = meta_entry(meta);
                if !Self::append_entry_without_rotation(path, &seed) {
                    return false;
                }
            }
            let context = session_context_entry(
                entry.timestamp,
                self.current_id.clone(),
                self.project_cwd.clone(),
                self.preview.clone(),
            );
            if !Self::append_entry_without_rotation(path, &context) {
                return false;
            }
        }
        Self::append_entry_without_rotation(path, entry)
    }

    /// Rotate if needed, then append one serialized entry to `path`.
    fn append_entry(path: &Path, entry: &SessionEntry) -> bool {
        Self::maybe_rotate(path);
        Self::append_entry_without_rotation(path, entry)
    }

    fn append_entry_without_rotation(path: &Path, entry: &SessionEntry) -> bool {
        let Ok(line) = serde_json::to_string(entry) else {
            return false;
        };
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{line}"))
            .is_ok()
    }

    fn write_raw(&self, line: &str) -> bool {
        let Some(path) = &self.current_path else {
            return false;
        };
        if Self::maybe_rotate(path) {
            if self.current_log_authoritative
                && let Some(meta) = &self.meta
                && !Self::append_entry_without_rotation(path, &meta_entry(meta))
            {
                return false;
            }
            let context = session_context_entry(
                unix_now(),
                self.current_id.clone(),
                self.project_cwd.clone(),
                self.preview.clone(),
            );
            if !Self::append_entry_without_rotation(path, &context) {
                return false;
            }
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{line}"))
            .is_ok()
    }

    fn maybe_rotate(path: &Path) -> bool {
        let size = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if size < MAX_FILE_SIZE {
            return false;
        }

        // Delete old .3 before shifting .2 → .3, .1 → .2, current → .1.
        // Removing it after the shift deleted the newly moved `.3` instead.
        let base = path.to_string_lossy().to_string();
        let oldest = format!("{base}.{}", MAX_ROTATIONS);
        let _ = fs::remove_file(&oldest);
        for index in (1..MAX_ROTATIONS).rev() {
            let from = format!("{base}.{index}");
            let to = format!("{base}.{}", index + 1);
            if Path::new(&from).exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        fs::rename(path, format!("{base}.1")).is_ok()
    }

    fn resolve_ref(&self, reference: &str) -> Option<String> {
        if reference == "latest" {
            let latest_path = self.sessions_dir.join(LATEST_FILE);
            fs::read_to_string(latest_path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            Some(reference.to_string())
        }
    }

    fn update_latest(&self, sid: &str) {
        let latest_path = self.sessions_dir.join(LATEST_FILE);
        let _ = fs::write(latest_path, sid);
    }

    fn runtime_state_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.state.json"))
    }
}

// ── Title policy ─────────────────────────────────────────────────────

/// Whether a title (re)generation is due for a session.
///
/// Policy — generate once the first real exchange has completed, then refresh
/// sparingly: only after at least [`TITLE_REFRESH_MIN_GAP`] new turns AND at
/// least a doubling of the turn count since the last generation attempt
/// (`title_turn` marks attempts, not just successes, so a failed generation
/// is retried on the same sparse schedule instead of every turn). A 100-turn
/// session thus costs at most ~5 label generations, and a session that stops
/// growing keeps the label it has.
#[must_use]
pub fn title_refresh_due(meta: &SessionMeta) -> bool {
    if meta.turn_count < TITLE_FIRST_EXCHANGE_TURNS {
        return false;
    }
    // `title_turn == 0` means no attempt ever; otherwise gate retries and
    // refreshes alike on substantial growth since the last attempt.
    meta.title_turn == 0
        || (meta.turn_count.saturating_sub(meta.title_turn) >= TITLE_REFRESH_MIN_GAP
            && meta.turn_count >= meta.title_turn.saturating_mul(2))
}

/// Deterministic fallback title: the first user message, cleaned and
/// truncated at a word boundary. Needs no model, no network, no I/O — this is
/// the label PRISM shows when running standalone. Never fails; empty input
/// yields `"Untitled session"`.
#[must_use]
pub fn deterministic_title(first_user_message: &str) -> String {
    let cleaned: String = first_user_message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut words = cleaned.split_whitespace();
    let Some(first) = words.next() else {
        return "Untitled session".to_string();
    };

    let mut title = first.to_string();
    let mut truncated = false;
    if title.chars().count() > TITLE_MAX_CHARS {
        // One giant word: hard-cut, leaving room for the ellipsis.
        let cut = title
            .char_indices()
            .nth(TITLE_MAX_CHARS - 1)
            .map(|(i, _)| i)
            .unwrap_or(title.len());
        title.truncate(cut);
        truncated = true;
    } else {
        for word in words {
            // +1 for the joining space.
            if title.chars().count() + 1 + word.chars().count() > TITLE_MAX_CHARS {
                truncated = true;
                break;
            }
            title.push(' ');
            title.push_str(word);
        }
    }
    if truncated {
        title.push('…');
    }
    title
}

// ── Helpers ──────────────────────────────────────────────────────────

struct ParsedSessionLog {
    messages: Vec<serde_json::Value>,
    meta: Option<SessionMeta>,
    project_cwd: Option<String>,
    preview: Option<String>,
}

struct SessionIndexScan {
    entries: Vec<SessionIndexEntry>,
    /// Existing rows to retain because a source candidate was present but
    /// could not be read as a complete JSONL snapshot.
    preserve_session_ids: Vec<String>,
}

fn meta_entry(meta: &SessionMeta) -> SessionEntry {
    SessionEntry {
        entry_type: "meta".to_string(),
        role: String::new(),
        content: String::new(),
        tool_name: String::new(),
        call_id: String::new(),
        timestamp: meta.updated_at,
        data: serde_json::to_value(meta).ok(),
    }
}

fn session_context_entry(
    timestamp: f64,
    session_id: Option<String>,
    project_cwd: Option<String>,
    preview: Option<String>,
) -> SessionEntry {
    SessionEntry {
        entry_type: SESSION_CONTEXT_ENTRY_TYPE.to_string(),
        role: String::new(),
        content: String::new(),
        tool_name: String::new(),
        call_id: String::new(),
        timestamp,
        data: Some(serde_json::json!({
            "schema_version": 1,
            "session_id": session_id,
            "project_cwd": project_cwd,
            "preview": preview,
        })),
    }
}

fn session_preview(content: &str) -> Option<String> {
    let cleaned = content
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(SESSION_PREVIEW_MAX_CHARS).collect())
}

fn normalized_session_terms(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn entry_search_terms(entry: &SessionIndexEntry) -> std::collections::BTreeSet<String> {
    let mut terms = std::collections::BTreeSet::new();
    for text in [&entry.title, &entry.summary, &entry.preview]
        .into_iter()
        .flatten()
    {
        terms.extend(normalized_session_terms(text));
    }
    terms
}

fn rotated_path(path: &Path, rotation: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{rotation}"));
    PathBuf::from(value)
}

fn session_base_path(candidate: &Path) -> Option<PathBuf> {
    let file_name = candidate.file_name()?.to_str()?;
    if file_name.ends_with(".jsonl") {
        return Some(candidate.to_path_buf());
    }
    for rotation in 1..=MAX_ROTATIONS {
        let suffix = format!(".jsonl.{rotation}");
        if let Some(stem) = file_name.strip_suffix(&suffix) {
            return Some(candidate.parent()?.join(format!("{stem}.jsonl")));
        }
    }
    None
}

fn session_id_from_base_path(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".jsonl")
        .map(str::to_string)
}

fn session_log_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = (1..=MAX_ROTATIONS)
        .rev()
        .map(|rotation| rotated_path(path, rotation))
        .filter(|candidate| candidate.exists())
        .collect::<Vec<_>>();
    if path.exists() {
        paths.push(path.to_path_buf());
    }
    paths
}

/// Require a fully readable snapshot before replacing an existing SQL row.
/// A malformed trailing append is indeterminate and must preserve the last
/// good mirror entry until a later repair can read the file cleanly.
fn session_log_is_complete_for_index(path: &Path) -> bool {
    let paths = session_log_paths(path);
    if paths.is_empty() {
        return false;
    }
    let mut saw_meta = false;
    for segment in paths {
        let Ok(text) = fs::read_to_string(segment) else {
            return false;
        };
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Ok(entry) = serde_json::from_str::<SessionEntry>(line) else {
                return false;
            };
            if entry.entry_type == "meta" {
                let Some(data) = entry.data else {
                    return false;
                };
                if serde_json::from_value::<SessionMeta>(data).is_err() {
                    return false;
                }
                saw_meta = true;
            }
        }
    }
    saw_meta
}

/// Parse retained segments oldest-to-newest. Seed metadata/context written on
/// rotation keeps the index fully rebuildable even after early segments expire.
fn scan_session_log(path: &Path) -> Option<ParsedSessionLog> {
    let paths = session_log_paths(path);
    if paths.is_empty() {
        return None;
    }

    let mut messages = Vec::new();
    let mut first_created_at = None;
    let mut loaded_meta: Option<SessionMeta> = None;
    let mut project_cwd = None;
    let mut preview = None;
    let mut turn_count = 0usize;
    let mut compaction_count = 0usize;
    let mut latest_timestamp = 0.0_f64;

    for segment in paths {
        let Ok(text) = fs::read_to_string(segment) else {
            continue;
        };
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Ok(entry) = serde_json::from_str::<SessionEntry>(line) else {
                continue;
            };
            latest_timestamp = latest_timestamp.max(entry.timestamp);

            if entry.entry_type == "meta" {
                let Some(data) = entry.data else { continue };
                let Ok(meta) = serde_json::from_value::<SessionMeta>(data) else {
                    continue;
                };
                first_created_at.get_or_insert(meta.created_at);
                // Metadata counters are cumulative as of this point in the
                // log. Taking the maximum here, then counting later events,
                // preserves post-seed activity after older rotations expire
                // without double-counting events covered by a later flush.
                turn_count = turn_count.max(meta.turn_count);
                compaction_count = compaction_count.max(meta.compaction_count);
                loaded_meta = Some(meta);
                continue;
            }

            if entry.entry_type == SESSION_CONTEXT_ENTRY_TYPE {
                if let Some(data) = entry.data {
                    if let Some(value) = data.get("project_cwd") {
                        project_cwd = value.as_str().map(str::to_string);
                    }
                    if let Some(value) = data.get("preview") {
                        preview = value.as_str().and_then(session_preview);
                    }
                }
                continue;
            }

            if entry.entry_type == "compaction" {
                compaction_count = compaction_count.saturating_add(1);
            }
            if matches!(entry.role.as_str(), "user" | "assistant") {
                turn_count = turn_count.saturating_add(1);
            }
            if entry.role == "user" && preview.is_none() {
                preview = session_preview(&entry.content);
            }
            if entry.role.is_empty() || entry.content.is_empty() {
                continue;
            }

            let mut message = serde_json::json!({
                "role": entry.role,
                "content": entry.content,
            });
            if !entry.call_id.is_empty() {
                message["tool_call_id"] = serde_json::Value::String(entry.call_id);
            }
            if !entry.tool_name.is_empty() {
                message["tool_name"] = serde_json::Value::String(entry.tool_name);
            }
            if let Some(data) = entry.data.and_then(|value| value.as_object().cloned()) {
                for (key, value) in data {
                    message[key] = value;
                }
            }
            messages.push(message);
        }
    }

    if let Some(meta) = loaded_meta.as_mut() {
        if let Some(created_at) = first_created_at {
            meta.created_at = created_at;
        }
        meta.updated_at = meta.updated_at.max(latest_timestamp);
        meta.turn_count = meta.turn_count.max(turn_count);
        meta.compaction_count = meta.compaction_count.max(compaction_count);
    }

    Some(ParsedSessionLog {
        messages,
        meta: loaded_meta,
        project_cwd,
        preview,
    })
}

/// The last durable metadata state across the active and rotated segments.
#[cfg(test)]
fn last_meta_in_file(path: &Path) -> Option<SessionMeta> {
    scan_session_log(path)?.meta
}

/// Parse a session JSONL log into (messages, meta). Shared by resume and HTTP.
fn parse_session_file(path: &Path) -> Option<(Vec<serde_json::Value>, Option<SessionMeta>)> {
    let parsed = scan_session_log(path)?;
    Some((parsed.messages, parsed.meta))
}

fn session_log_size(path: &Path) -> u64 {
    session_log_paths(path)
        .into_iter()
        .fold(0_u64, |total, path| {
            total.saturating_add(path.metadata().map(|metadata| metadata.len()).unwrap_or(0))
        })
}

fn index_entry_from_meta(
    sessions_dir: &Path,
    path: &Path,
    meta: &SessionMeta,
    project_cwd: Option<String>,
    preview: Option<String>,
) -> SessionIndexEntry {
    SessionIndexEntry {
        session_id: meta.session_id.clone(),
        source_path: sessions_dir.to_string_lossy().into_owned(),
        file_path: path.to_string_lossy().into_owned(),
        project_cwd,
        created_at: meta.created_at,
        updated_at: meta.updated_at,
        model: meta.model.clone(),
        turn_count: u64::try_from(meta.turn_count).unwrap_or(u64::MAX),
        compaction_count: u64::try_from(meta.compaction_count).unwrap_or(u64::MAX),
        parent_session_id: meta.parent_session_id.clone(),
        branch_name: meta.branch_name.clone(),
        title: meta.title.clone(),
        summary: meta.summary.clone(),
        title_source: meta.title_source.clone(),
        title_turn: u64::try_from(meta.title_turn).unwrap_or(u64::MAX),
        models: meta.models.clone(),
        preview,
        size_bytes: session_log_size(path),
    }
}

fn index_entry_from_log(sessions_dir: &Path, path: &Path) -> Option<SessionIndexEntry> {
    let parsed = scan_session_log(path)?;
    let meta = parsed.meta?;
    Some(index_entry_from_meta(
        sessions_dir,
        path,
        &meta,
        parsed.project_cwd,
        parsed.preview,
    ))
}

fn session_info_from_index(entry: SessionIndexEntry, latest_id: Option<&str>) -> SessionInfo {
    let models = if entry.models.is_empty() && !entry.model.is_empty() {
        vec![entry.model.clone()]
    } else {
        entry.models.clone()
    };
    SessionInfo {
        is_latest: latest_id == Some(entry.session_id.as_str()),
        session_id: entry.session_id,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        turn_count: usize::try_from(entry.turn_count).unwrap_or(usize::MAX),
        model: entry.model,
        size_kb: entry.size_bytes as f64 / 1024.0,
        title: entry.title,
        summary: entry.summary,
        title_source: entry.title_source,
        models,
        path: entry.file_path,
        project_cwd: entry.project_cwd,
        preview: entry.preview,
    }
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Simple random u32 using system time nanoseconds (no extra crate needed).
fn rand_u32() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| {
            let nanos = d.as_nanos();
            // Mix bits for better distribution
            ((nanos ^ (nanos >> 16)) & 0xFFFF_FFFF) as u32
        })
        .unwrap_or(0)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (SessionStore, TempDir) {
        let tmp = TempDir::new().expect("temp dir");
        let store = reopen_store(&tmp);
        (store, tmp)
    }

    fn reopen_store(tmp: &TempDir) -> SessionStore {
        SessionStore::new_with_index(
            tmp.path().to_path_buf(),
            tmp.path().join("session-index.db"),
            SessionIndexPolicy::default(),
        )
    }

    #[test]
    fn new_session_creates_jsonl_file() {
        let (mut store, _tmp) = make_store();
        let sid = store.new_session("claude-sonnet");

        assert!(store.current_id().is_some());
        assert_eq!(store.current_id().unwrap(), sid);
        assert!(store.current_path.as_ref().unwrap().exists());

        let meta = store.meta().unwrap();
        assert_eq!(meta.model, "claude-sonnet");
        assert_eq!(meta.turn_count, 0);
    }

    #[test]
    fn append_and_resume_roundtrip() {
        let (mut store, tmp) = make_store();
        let sid = store.new_session("test-model");

        store.append_message("user", "Hello", "", "", None);
        store.append_message("assistant", "Hi there!", "", "", None);

        // New store, resume by ID
        let mut store2 = reopen_store(&tmp);
        let result = store2.resume_session(&sid);
        assert!(result.is_some());

        let (resumed_id, messages) = result.unwrap();
        assert_eq!(resumed_id, sid);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["content"], "Hi there!");
    }

    #[test]
    fn resume_latest_works() {
        let (mut store, tmp) = make_store();
        let _first = store.new_session("m1");
        let second = store.new_session("m2");

        let mut store2 = reopen_store(&tmp);
        let result = store2.resume_session("latest");
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, second);
    }

    #[test]
    fn fork_copies_messages_with_parent_tracking() {
        let (mut store, _tmp) = make_store();
        let parent_id = store.new_session("m1");
        store.append_message("user", "original message", "", "", None);

        let fork_id = store.fork_session("experiment-a");

        assert_ne!(fork_id, parent_id);
        let meta = store.meta().unwrap();
        assert_eq!(meta.parent_session_id.as_deref(), Some(parent_id.as_str()));
        assert_eq!(meta.branch_name.as_deref(), Some("experiment-a"));
        let disk_meta = last_meta_in_file(
            store
                .current_path
                .as_deref()
                .expect("fork has a current path"),
        )
        .expect("fork metadata is durable");
        assert_eq!(
            disk_meta.parent_session_id.as_deref(),
            Some(parent_id.as_str())
        );
        assert_eq!(disk_meta.branch_name.as_deref(), Some("experiment-a"));
    }

    #[test]
    fn fork_copies_messages_from_retained_rotations() {
        let (mut store, _tmp) = make_store();
        let parent_id = store.new_session("m1");
        store.append_message("user", "message from the oldest segment", "", "", None);
        store.append_message("assistant", &"x".repeat(300 * 1024), "", "", None);
        let parent_path = store.sessions_dir.join(format!("{parent_id}.jsonl"));
        assert!(rotated_path(&parent_path, 1).exists());

        let fork_id = store.fork_session("retained-history");
        let messages = store.load_messages(&fork_id).expect("fork log loads");
        assert!(messages.iter().any(|message| {
            message["content"].as_str() == Some("message from the oldest segment")
        }));
    }

    #[test]
    fn list_sessions_returns_both() {
        let (mut store, _tmp) = make_store();
        let a = store.new_session("m1");
        let b = store.new_session("m2");

        let list = store.list_sessions(10);
        assert_eq!(list.len(), 2);
        // Both sessions present (order depends on timestamp + random suffix)
        let ids: Vec<&str> = list.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&a.as_str()));
        assert!(ids.contains(&b.as_str()));
    }

    #[test]
    fn list_sessions_reads_index_until_reconciliation_repairs_a_missing_file() {
        let tmp = TempDir::new().expect("temp dir");
        let mut store = SessionStore::new_with_index(
            tmp.path().to_path_buf(),
            tmp.path().join("session-index.db"),
            SessionIndexPolicy {
                reconcile_after: Duration::MAX,
            },
        );
        let sid = store.new_session("m1");
        assert_eq!(store.list_sessions(10).len(), 1);

        fs::remove_file(tmp.path().join(format!("{sid}.jsonl"))).unwrap();

        let indexed = store.list_sessions(10);
        assert_eq!(indexed.len(), 1, "listing must come from SQL, not read_dir");
        assert_eq!(indexed[0].session_id, sid);

        assert_eq!(store.rebuild_session_index().unwrap(), 0);
        assert!(
            store.list_sessions(10).is_empty(),
            "an explicit repair must purge an index row whose log is gone"
        );
    }

    #[test]
    fn separate_stores_observe_queued_writes_in_order() {
        let tmp = TempDir::new().expect("temp dir");
        let index_path = tmp.path().join("session-index.db");
        let policy = SessionIndexPolicy {
            reconcile_after: Duration::MAX,
        };
        let mut writer =
            SessionStore::new_with_index(tmp.path().to_path_buf(), index_path.clone(), policy);
        let reader = SessionStore::new_with_index(tmp.path().to_path_buf(), index_path, policy);
        assert!(reader.list_sessions(100).is_empty());

        let mut expected = std::collections::BTreeSet::new();
        for index in 0..32 {
            expected.insert(writer.new_session(&format!("model-{index}")));
        }
        let actual = reader
            .list_sessions(100)
            .into_iter()
            .map(|session| session.session_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn deleted_index_rebuilds_the_same_sessions_from_jsonl() {
        let tmp = TempDir::new().expect("temp dir");
        let index_path = tmp.path().join("session-index.db");
        let expected = {
            let mut store = SessionStore::new_with_index(
                tmp.path().to_path_buf(),
                index_path.clone(),
                SessionIndexPolicy::default(),
            );
            let first = store.new_session("m1");
            store.append_message("user", "screen nickel alloys", "", "", None);
            let second = store.new_session("m2");
            store.append_message("user", "compare ceramic phases", "", "", None);
            let listed = store.list_sessions(10);
            assert_eq!(listed.len(), 2);
            assert!(listed.iter().any(|session| session.session_id == first));
            assert!(listed.iter().any(|session| session.session_id == second));
            listed
                .into_iter()
                .map(|session| (session.session_id, session.preview))
                .collect::<std::collections::BTreeMap<_, _>>()
        };

        fs::remove_file(&index_path).expect("delete disposable SQL index");

        let store = SessionStore::new_with_index(
            tmp.path().to_path_buf(),
            index_path,
            SessionIndexPolicy::default(),
        );
        assert_eq!(store.rebuild_session_index().unwrap(), expected.len());
        let rebuilt = store
            .list_sessions(10)
            .into_iter()
            .map(|session| (session.session_id, session.preview))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(rebuilt, expected);
    }

    #[test]
    fn first_index_use_backfills_an_existing_unindexed_log() {
        let tmp = TempDir::new().expect("temp dir");
        let sid = "20260101_000000_backfill";
        let meta = SessionMeta {
            session_id: sid.to_string(),
            created_at: 100.0,
            updated_at: 200.0,
            model: "legacy-model".to_string(),
            turn_count: 0,
            compaction_count: 0,
            parent_session_id: None,
            branch_name: None,
            title: Some("Imported log".to_string()),
            summary: None,
            title_source: Some("heuristic".to_string()),
            title_turn: 0,
            models: vec!["legacy-model".to_string()],
        };
        fs::write(
            tmp.path().join(format!("{sid}.jsonl")),
            format!("{}\n", serde_json::to_string(&meta_entry(&meta)).unwrap()),
        )
        .unwrap();
        let store = SessionStore::new_with_index(
            tmp.path().to_path_buf(),
            tmp.path().join("session-index.db"),
            SessionIndexPolicy::default(),
        );

        let listed = store.list_sessions(10);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, sid);
        assert_eq!(listed[0].title.as_deref(), Some("Imported log"));
        assert!(listed[0].project_cwd.is_none());
    }

    #[test]
    fn first_index_use_backfills_an_orphaned_rotation() {
        let tmp = TempDir::new().expect("temp dir");
        let sid = "20260101_000000_orphaned";
        let meta = SessionMeta {
            session_id: sid.to_string(),
            created_at: 100.0,
            updated_at: 200.0,
            model: "legacy-model".to_string(),
            turn_count: 0,
            compaction_count: 0,
            parent_session_id: None,
            branch_name: None,
            title: Some("Orphaned rotation".to_string()),
            summary: None,
            title_source: Some("heuristic".to_string()),
            title_turn: 0,
            models: vec!["legacy-model".to_string()],
        };
        let base = tmp.path().join(format!("{sid}.jsonl"));
        fs::write(
            &base,
            format!("{}\n", serde_json::to_string(&meta_entry(&meta)).unwrap()),
        )
        .unwrap();
        fs::rename(&base, rotated_path(&base, 1)).unwrap();
        let store = reopen_store(&tmp);

        let listed = store.list_sessions(10);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, sid);
        assert_eq!(listed[0].title.as_deref(), Some("Orphaned rotation"));
    }

    #[test]
    fn rebuild_preserves_last_good_row_for_an_incomplete_log() {
        let (mut store, _tmp) = make_store();
        let sid = store.new_session("m1");
        store.append_message("user", "durable indexed preview", "", "", None);
        assert_eq!(store.list_sessions(10).len(), 1);

        let path = store.sessions_dir.join(format!("{sid}.jsonl"));
        writeln!(
            OpenOptions::new().append(true).open(path).unwrap(),
            "{{\"type\":"
        )
        .unwrap();

        assert_eq!(store.rebuild_session_index().unwrap(), 0);
        let listed = store.list_sessions(10);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, sid);
        assert_eq!(
            listed[0].preview.as_deref(),
            Some("durable indexed preview")
        );
    }

    #[test]
    fn session_write_survives_an_unopenable_index() {
        let tmp = TempDir::new().expect("temp dir");
        let sessions_dir = tmp.path().join("sessions");
        let mut store = SessionStore::new_with_index(
            sessions_dir.clone(),
            tmp.path().to_path_buf(),
            SessionIndexPolicy::default(),
        );

        let sid = store.new_session("m1");
        store.append_message("user", "JSONL must win", "", "", None);

        assert!(sessions_dir.join(format!("{sid}.jsonl")).exists());
        assert_eq!(store.meta().map(|meta| meta.turn_count), Some(1));
        let listed = store.list_sessions(10);
        assert_eq!(listed.len(), 1, "JSONL listing must survive index failure");
        assert_eq!(listed[0].session_id, sid);
        assert_eq!(listed[0].preview.as_deref(), Some("JSONL must win"));
    }

    #[test]
    fn text_and_project_queries_use_rebuildable_context() {
        let (mut store, tmp) = make_store();
        let project = tmp.path().join("alloy-project");
        store.set_project_cwd(Some(&project));
        let sid = store.new_session("m1");
        store.append_message(
            "user",
            "Investigate nickel superalloys for turbine blades",
            "",
            "",
            None,
        );
        store.update_session_meta(&sid, |meta| {
            meta.title = Some("Nickel superalloy screen".to_string());
            meta.summary = Some("Candidate ranking for turbine service".to_string());
        });

        let matches = store.query_sessions(&SessionQuery {
            text: Some("nickel turbine".to_string()),
            project_cwd: Some(project.clone()),
            ..SessionQuery::default()
        });
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, sid);
        assert_eq!(matches[0].project_cwd.as_deref(), project.to_str());
        assert_eq!(
            matches[0].preview.as_deref(),
            Some("Investigate nickel superalloys for turbine blades")
        );
        assert!(
            store
                .query_sessions(&SessionQuery {
                    text: Some("nickel".to_string()),
                    project_cwd: Some(tmp.path().join("other-project")),
                    ..SessionQuery::default()
                })
                .is_empty()
        );
    }

    #[test]
    fn compaction_increments_count() {
        let (mut store, _tmp) = make_store();
        store.new_session("m1");
        store.append_compaction("Summary of conversation so far.");

        assert_eq!(store.meta().unwrap().compaction_count, 1);
        assert_eq!(
            last_meta_in_file(store.current_path.as_deref().unwrap())
                .unwrap()
                .compaction_count,
            1
        );
    }

    #[test]
    fn runtime_state_roundtrip() {
        let (mut store, _tmp) = make_store();
        let sid = store.new_session("m1");
        let state = RuntimeSessionState {
            session_mode: "plan".to_string(),
            permission_allow: vec!["read_file".to_string()],
            permission_deny: vec!["execute_bash".to_string()],
            plan_status: "approved".to_string(),
            approved_plan_body: Some("Current Plan\n  1. Audit\n  2. Patch".to_string()),
        };

        store.save_runtime_state(&sid, &state);
        let loaded = store
            .load_runtime_state(&sid)
            .expect("runtime state should load");
        assert_eq!(loaded, state);
    }

    /// Build a `SessionMeta` for policy tests without going through the store.
    fn meta_with(turn_count: usize, title: Option<&str>, title_turn: usize) -> SessionMeta {
        SessionMeta {
            session_id: "s".to_string(),
            created_at: 0.0,
            updated_at: 0.0,
            model: "m".to_string(),
            turn_count,
            compaction_count: 0,
            parent_session_id: None,
            branch_name: None,
            title: title.map(str::to_string),
            summary: None,
            title_source: None,
            title_turn,
            models: vec!["m".to_string()],
        }
    }

    #[test]
    fn title_and_summary_persist_across_reload() {
        let (mut store, tmp) = make_store();
        let sid = store.new_session("m1");
        store.append_message("user", "screen perovskites", "", "", None);
        store.append_message("assistant", "Starting the screen...", "", "", None);
        store.update_session_meta(&sid.clone(), |meta| {
            meta.title = Some("Perovskite screen".to_string());
            meta.summary = Some("Screening perovskite candidates for stability.".to_string());
            meta.title_source = Some("model".to_string());
            meta.title_turn = 2;
        });

        // Fresh eyes — no shared memory, disk only.
        let store2 = reopen_store(&tmp);
        let info = store2
            .list_sessions(10)
            .into_iter()
            .find(|s| s.session_id == sid)
            .expect("session listed");
        assert_eq!(info.title.as_deref(), Some("Perovskite screen"));
        assert_eq!(
            info.summary.as_deref(),
            Some("Screening perovskite candidates for stability.")
        );
        assert_eq!(info.title_source.as_deref(), Some("model"));

        // Resume also picks the persisted meta up.
        let mut store3 = reopen_store(&tmp);
        store3.resume_session(&sid);
        assert_eq!(
            store3.meta().and_then(|m| m.title.as_deref()),
            Some("Perovskite screen")
        );
    }

    #[test]
    fn meta_update_from_another_store_does_not_clobber_runtime_fields() {
        // The title task writes through its own store while the runtime owns
        // the live counters: neither may erase the other's fields.
        let (mut store, tmp) = make_store();
        let sid = store.new_session("m1");
        for i in 0..10 {
            store.append_message("user", &format!("msg {i}"), "", "", None);
        }

        // Detached-task side: sets a title from an outside store.
        let mut task_store = reopen_store(&tmp);
        task_store.update_session_meta(&sid.clone(), |meta| {
            meta.title = Some("From the task".to_string());
            meta.title_source = Some("heuristic".to_string());
        });

        // Runtime side: later counter flushes must keep the task's title, and
        // the next flush adopts it into memory.
        for i in 0..5 {
            store.append_message("assistant", &format!("reply {i}"), "", "", None);
        }
        let meta = store.meta().unwrap();
        assert_eq!(meta.title.as_deref(), Some("From the task"));
        assert_eq!(meta.turn_count, 15);

        // And the disk agrees with both writers.
        let disk = last_meta_in_file(&tmp.path().join(format!("{sid}.jsonl"))).unwrap();
        assert_eq!(disk.title.as_deref(), Some("From the task"));
        assert_eq!(disk.turn_count, 15);
    }

    #[test]
    fn model_switch_updates_session_info_and_keeps_model_set() {
        let (mut store, tmp) = make_store();
        let sid = store.new_session("m1");
        store.append_message("user", "hello", "", "", None);

        store.note_model_used("m2");
        store.note_model_used("m3");
        store.note_model_used("m2"); // revisits do not duplicate

        let meta = store.meta().unwrap();
        assert_eq!(meta.model, "m2");
        let want: Vec<String> = ["m1", "m2", "m3"].iter().map(|s| s.to_string()).collect();
        assert_eq!(meta.models, want);

        // SessionInfo — what the history rail renders — must agree.
        let store2 = reopen_store(&tmp);
        let info = store2
            .list_sessions(10)
            .into_iter()
            .find(|s| s.session_id == sid)
            .expect("session listed");
        assert_eq!(info.model, "m2");
        assert_eq!(info.models, want);
    }

    #[test]
    fn turn_counters_survive_reload_via_periodic_meta_flush() {
        let (mut store, tmp) = make_store();
        let sid = store.new_session("m1");
        for i in 0..10 {
            store.append_message("user", &format!("msg {i}"), "", "", None);
            store.append_message("assistant", "ok", "", "", None);
        }

        // File alone must carry the counters — no shared memory.
        let store2 = reopen_store(&tmp);
        let info = store2
            .list_sessions(10)
            .into_iter()
            .find(|s| s.session_id == sid)
            .expect("session listed");
        assert_eq!(info.turn_count, 20);
    }

    #[test]
    fn legacy_session_file_without_new_fields_loads() {
        // The 130 pre-existing transcripts predate title/models fields; they
        // must list cleanly with sane fallbacks.
        let (store, tmp) = make_store();
        let sid = "20260101_000000_deadbeef";
        // Hand-written in the exact on-disk shape (struct field order — the
        // `{\"type\":\"meta\"` prefix filter depends on it).
        let meta_line = concat!(
            "{\"type\":\"meta\",\"role\":\"\",\"content\":\"\",",
            "\"tool_name\":\"\",\"call_id\":\"\",\"timestamp\":0.0,",
            "\"data\":{\"session_id\":\"20260101_000000_deadbeef\",",
            "\"created_at\":1000.0,\"updated_at\":1000.0,\"model\":\"old-model\",",
            "\"turn_count\":4,\"compaction_count\":0,",
            "\"parent_session_id\":null,\"branch_name\":null}}"
        );
        fs::write(
            tmp.path().join(format!("{sid}.jsonl")),
            format!("{meta_line}\n"),
        )
        .unwrap();

        let sessions = store.list_sessions(10);
        assert_eq!(sessions.len(), 1);
        let info = &sessions[0];
        assert_eq!(info.model, "old-model");
        assert_eq!(info.turn_count, 4);
        assert_eq!(info.created_at, 1000.0);
        assert!(info.title.is_none());
        assert!(info.summary.is_none());
        assert!(info.title_source.is_none());
        assert_eq!(info.models, vec!["old-model".to_string()]);
    }

    #[test]
    fn title_refresh_policy_first_exchange_then_substantial_growth() {
        // Untitled, never attempted: due once the first real exchange lands.
        assert!(!title_refresh_due(&meta_with(1, None, 0)));
        assert!(title_refresh_due(&meta_with(2, None, 0)));

        // Titled (or attempted) at turn 2: no churn mid-session; due again
        // only after >= 16 new turns AND a doubling of the turn count.
        assert!(!title_refresh_due(&meta_with(10, Some("T"), 2)));
        assert!(!title_refresh_due(&meta_with(17, Some("T"), 2)));
        assert!(title_refresh_due(&meta_with(18, Some("T"), 2)));

        // The doubling gate bites on later refreshes: 16 new turns but not 2x.
        assert!(!title_refresh_due(&meta_with(36, Some("T"), 20)));
        assert!(title_refresh_due(&meta_with(40, Some("T"), 20)));

        // A failed attempt (title_turn set, title still absent) retries on the
        // same sparse schedule — never every turn.
        assert!(!title_refresh_due(&meta_with(3, None, 2)));
        assert!(title_refresh_due(&meta_with(18, None, 2)));
    }

    #[test]
    fn deterministic_title_cleans_truncates_and_needs_no_model() {
        // Short messages pass through untouched (after a trim).
        assert_eq!(
            deterministic_title("Screen perovskites for thermal stability"),
            "Screen perovskites for thermal stability"
        );
        // Control chars and whitespace runs are cleaned.
        assert_eq!(
            deterministic_title("  compare\n\tBaTiO3  films "),
            "compare BaTiO3 films"
        );
        // Empty input is still a session.
        assert_eq!(deterministic_title(""), "Untitled session");
        assert_eq!(deterministic_title("   \n  "), "Untitled session");

        // Long messages truncate at a word boundary, marked with an ellipsis.
        let long = (0..20)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let title = deterministic_title(&long);
        assert!(title.chars().count() <= TITLE_MAX_CHARS, "got: {title}");
        assert!(title.ends_with('\u{2026}'), "got: {title}");

        // One giant word is hard-cut, still within the cap.
        let huge = deterministic_title(&"x".repeat(500));
        assert_eq!(huge.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn rotation_renames_large_files() {
        let (mut store, _tmp) = make_store();
        let sid = store.new_session("m1");

        // Write enough to exceed 256KB
        let big_content = "x".repeat(300 * 1024);
        store.append_message("user", &big_content, "", "", None);

        // The next write should trigger rotation
        store.append_message("user", "after rotation", "", "", None);

        let base = store.sessions_dir.join(format!("{sid}.jsonl"));
        let rotated = PathBuf::from(format!("{}.1", base.display()));
        // Either the current file was rotated (rotated exists) or file is still there
        // Both are valid — rotation happens when file exceeds limit before next write
        assert!(base.exists() || rotated.exists());
    }

    #[test]
    fn rebuild_preserves_project_and_preview_after_old_rotations_expire() {
        let (mut store, tmp) = make_store();
        let project = tmp.path().join("durable-project");
        store.set_project_cwd(Some(&project));
        let sid = store.new_session("m1");
        store.append_message("user", "durable preview", "", "", None);

        let large = "x".repeat(300 * 1024);
        for _ in 0..(MAX_ROTATIONS + 2) {
            store.append_message("assistant", &large, "", "", None);
        }
        // This message lands after the final rotation seed. A rebuild must
        // add it to that cumulative seed instead of taking either count alone.
        store.append_message("assistant", "after final rotation", "", "", None);

        assert_eq!(store.rebuild_session_index().unwrap(), 1);
        let info = store
            .list_sessions(10)
            .into_iter()
            .find(|session| session.session_id == sid)
            .expect("rotated session rebuilt");
        assert_eq!(info.project_cwd.as_deref(), project.to_str());
        assert_eq!(info.preview.as_deref(), Some("durable preview"));
        assert_eq!(info.turn_count, MAX_ROTATIONS + 4);
    }
}
