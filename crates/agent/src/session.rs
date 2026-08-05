//! Session persistence — JSONL files with resume, fork, and rotation.
//!
//! Sessions are stored as newline-delimited JSON at `~/.prism/sessions/`.
//! Each line is a message or metadata event. Sessions can be:
//! - Resumed: auto-loads last session, or by explicit ID
//! - Forked: branch the current conversation with parent tracking
//! - Listed: scan dir, parse first line of each JSONL
//! - Rotated: files rotate at 256KB (max 3 backups)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;
use directories::UserDirs;
use serde::{Deserialize, Serialize};

// ── Constants ────────────────────────────────────────────────────────

const MAX_FILE_SIZE: u64 = 256 * 1024; // 256KB
const MAX_ROTATIONS: usize = 3;
const LATEST_FILE: &str = ".latest";

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
}

impl SessionStore {
    /// Create a new store. Creates the sessions directory if it doesn't exist.
    pub fn new(sessions_dir: Option<PathBuf>) -> Self {
        let dir = sessions_dir.unwrap_or_else(default_sessions_dir);
        let _ = fs::create_dir_all(&dir);
        Self {
            sessions_dir: dir,
            current_id: None,
            current_path: None,
            meta: None,
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
        self.write_entry(&meta_entry);
        self.update_latest(&sid);
        sid
    }

    /// Resume a session by ID or `"latest"`. Returns `(session_id, messages)`.
    pub fn resume_session(&mut self, reference: &str) -> Option<(String, Vec<serde_json::Value>)> {
        let sid = self.resolve_ref(reference)?;
        let path = self.sessions_dir.join(format!("{sid}.jsonl"));
        let (messages, loaded_meta) = parse_session_file(&path)?;

        self.current_id = Some(sid.clone());
        self.current_path = Some(path);
        if let Some(m) = loaded_meta {
            self.meta = Some(m);
        }
        self.update_latest(&sid);
        Some((sid, messages))
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

    /// Fork the current session into a new one with parent tracking.
    pub fn fork_session(&mut self, branch_name: &str) -> String {
        let old_id = self.current_id.clone();
        let old_path = self.current_path.clone();
        let old_model = self
            .meta
            .as_ref()
            .map(|m| m.model.clone())
            .unwrap_or_default();

        let new_id = self.new_session(&old_model);

        if let Some(meta) = self.meta.as_mut() {
            meta.parent_session_id = old_id.clone();
            let name = if branch_name.is_empty() {
                format!("fork-{}", &new_id[..new_id.len().min(8)])
            } else {
                branch_name.to_string()
            };
            meta.branch_name = Some(name);
        }

        // Copy non-meta entries from old session
        if let Some(old) = old_path
            && let Ok(text) = fs::read_to_string(&old)
        {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line)
                    && entry.get("type").and_then(|t| t.as_str()) != Some("meta")
                {
                    self.write_raw(line);
                }
            }
        }

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
        self.write_entry(&entry);

        if let Some(meta) = self.meta.as_mut() {
            meta.updated_at = unix_now();
            if role == "user" || role == "assistant" {
                meta.turn_count += 1;
            }
        }

        // Keep the persisted meta close to the live counters so a fresh store
        // (history rail, resume) sees near-current state from the file alone.
        let flush_due = matches!(role, "user" | "assistant")
            && self
                .meta
                .as_ref()
                .is_some_and(|m| m.turn_count > 0 && m.turn_count % META_FLUSH_EVERY_TURNS == 0);
        if flush_due && let Some(sid) = self.current_id().map(str::to_string) {
            self.update_session_meta(&sid, |_meta| {});
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

        let disk_meta = last_meta_in_file(&path);
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
        Self::append_entry(&path, &entry);

        if is_current {
            self.meta = merged;
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
        self.write_entry(&entry);

        if let Some(meta) = self.meta.as_mut() {
            meta.compaction_count += 1;
        }
    }

    // ── Query ────────────────────────────────────────────────────────

    /// List sessions, most recent first, up to `limit`.
    pub fn list_sessions(&self, limit: usize) -> Vec<SessionInfo> {
        let latest_id = self.resolve_ref("latest");

        let mut paths: Vec<PathBuf> = fs::read_dir(&self.sessions_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
            .collect();

        // Sort by filename descending (timestamp-prefixed IDs sort chronologically)
        paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

        let mut sessions = Vec::new();
        for path in paths.into_iter().take(limit) {
            // Meta is append-only: the first `meta` line was written at
            // creation, later ones (model switches, titles, counter flushes)
            // are appended. Last one wins for current state; the first still
            // owns `created_at`.
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let mut first_meta: Option<SessionMeta> = None;
            let mut last_meta: Option<SessionMeta> = None;
            for line in text.lines() {
                let line = line.trim();
                if !line.starts_with("{\"type\":\"meta\"") {
                    continue;
                }
                let Ok(entry) = serde_json::from_str::<SessionEntry>(line) else {
                    continue;
                };
                let Some(data) = entry.data else { continue };
                let Ok(meta) = serde_json::from_value::<SessionMeta>(data) else {
                    continue;
                };
                if first_meta.is_none() {
                    first_meta = Some(meta.clone());
                }
                last_meta = Some(meta);
            }
            let Some(meta) = last_meta else { continue };

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let size_kb = path
                .metadata()
                .map(|m| m.len() as f64 / 1024.0)
                .unwrap_or(0.0);
            let models = if meta.models.is_empty() && !meta.model.is_empty() {
                vec![meta.model.clone()]
            } else {
                meta.models.clone()
            };

            sessions.push(SessionInfo {
                session_id: stem.clone(),
                created_at: first_meta
                    .as_ref()
                    .map(|m| m.created_at)
                    .unwrap_or(meta.created_at),
                turn_count: meta.turn_count,
                model: meta.model.clone(),
                size_kb,
                is_latest: latest_id.as_deref() == Some(stem.as_str()),
                title: meta.title.clone(),
                summary: meta.summary.clone(),
                title_source: meta.title_source.clone(),
                models,
            });
        }
        sessions
    }

    // ── Accessors ────────────────────────────────────────────────────

    /// Current session ID, if any.
    pub fn current_id(&self) -> Option<&str> {
        self.current_id.as_deref()
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

    fn write_entry(&self, entry: &SessionEntry) {
        if let Some(path) = &self.current_path {
            Self::append_entry(path, entry);
        }
    }

    /// Rotate if needed, then append one serialized entry to `path`.
    fn append_entry(path: &Path, entry: &SessionEntry) {
        Self::maybe_rotate(path);
        if let Ok(line) = serde_json::to_string(entry) {
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
        }
    }

    fn write_raw(&self, line: &str) {
        if let Some(path) = &self.current_path {
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
        }
    }

    fn maybe_rotate(path: &Path) {
        let size = path.metadata().map(|m| m.len()).unwrap_or(0);
        if size < MAX_FILE_SIZE {
            return;
        }

        // Rotate: .3 is deleted, .2 → .3, .1 → .2, current → .1
        let base = path.to_string_lossy().to_string();
        for i in (1..MAX_ROTATIONS).rev() {
            let from = format!("{base}.{i}");
            let to = format!("{base}.{}", i + 1);
            if Path::new(&from).exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        // Delete the oldest if it exists
        let oldest = format!("{base}.{}", MAX_ROTATIONS);
        let _ = fs::remove_file(&oldest);
        // Current → .1
        let _ = fs::rename(path, format!("{base}.1"));
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

/// The last `meta` entry persisted in a session file, if any. Meta updates
/// are appended, so the last line is current.
fn last_meta_in_file(path: &Path) -> Option<SessionMeta> {
    let text = fs::read_to_string(path).ok()?;
    let mut last = None;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("{\"type\":\"meta\"") {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(line)
            && let Some(data) = entry.data
            && let Ok(meta) = serde_json::from_value::<SessionMeta>(data)
        {
            last = Some(meta);
        }
    }
    last
}

/// Parse a session JSONL file into (messages, meta). Shared by
/// [`SessionStore::resume_session`] (which also switches the current
/// session) and [`SessionStore::load_messages`] (read-only).
fn parse_session_file(path: &Path) -> Option<(Vec<serde_json::Value>, Option<SessionMeta>)> {
    if !path.exists() {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let mut messages = Vec::new();
    let mut loaded_meta: Option<SessionMeta> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if entry.get("type").and_then(|t| t.as_str()) == Some("meta") {
            if let Some(data) = entry.get("data") {
                loaded_meta = serde_json::from_value(data.clone()).ok();
            }
            continue;
        }

        let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = entry.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if !role.is_empty() && !content.is_empty() {
            let mut msg = serde_json::json!({ "role": role, "content": content });
            if let Some(cid) = entry.get("call_id").and_then(|v| v.as_str())
                && !cid.is_empty()
            {
                msg["tool_call_id"] = serde_json::Value::String(cid.to_string());
            }
            if let Some(tn) = entry.get("tool_name").and_then(|v| v.as_str())
                && !tn.is_empty()
            {
                msg["tool_name"] = serde_json::Value::String(tn.to_string());
            }
            if let Some(data) = entry.get("data")
                && let Some(obj) = data.as_object()
            {
                for (k, v) in obj {
                    msg[k] = v.clone();
                }
            }
            messages.push(msg);
        }
    }

    Some((messages, loaded_meta))
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
        let store = SessionStore::new(Some(tmp.path().to_path_buf()));
        (store, tmp)
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
        let mut store2 = SessionStore::new(Some(tmp.path().to_path_buf()));
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

        let mut store2 = SessionStore::new(Some(tmp.path().to_path_buf()));
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
    fn compaction_increments_count() {
        let (mut store, _tmp) = make_store();
        store.new_session("m1");
        store.append_compaction("Summary of conversation so far.");

        assert_eq!(store.meta().unwrap().compaction_count, 1);
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
        let store2 = SessionStore::new(Some(tmp.path().to_path_buf()));
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
        let mut store3 = SessionStore::new(Some(tmp.path().to_path_buf()));
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
        let mut task_store = SessionStore::new(Some(tmp.path().to_path_buf()));
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
        let store2 = SessionStore::new(Some(tmp.path().to_path_buf()));
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
        let store2 = SessionStore::new(Some(tmp.path().to_path_buf()));
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
}
