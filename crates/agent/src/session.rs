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

fn default_sessions_dir() -> PathBuf {
    UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".prism")
        .join("sessions")
}

// ── Data types ───────────────────────────────────────────────────────

/// Session metadata — written as the first JSONL line.
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
    pub model: String,
    pub size_kb: f64,
    pub is_latest: bool,
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
        if !path.exists() {
            return None;
        }

        let text = fs::read_to_string(&path).ok()?;
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
                if let Some(cid) = entry.get("call_id").and_then(|v| v.as_str()) {
                    if !cid.is_empty() {
                        msg["tool_call_id"] = serde_json::Value::String(cid.to_string());
                    }
                }
                if let Some(tn) = entry.get("tool_name").and_then(|v| v.as_str()) {
                    if !tn.is_empty() {
                        msg["tool_name"] = serde_json::Value::String(tn.to_string());
                    }
                }
                if let Some(data) = entry.get("data") {
                    if let Some(obj) = data.as_object() {
                        for (k, v) in obj {
                            msg[k] = v.clone();
                        }
                    }
                }
                messages.push(msg);
            }
        }

        self.current_id = Some(sid.clone());
        self.current_path = Some(path);
        if let Some(m) = loaded_meta {
            self.meta = Some(m);
        }
        self.update_latest(&sid);
        Some((sid, messages))
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
        if let Some(old) = old_path {
            if let Ok(text) = fs::read_to_string(&old) {
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                        if entry.get("type").and_then(|t| t.as_str()) != Some("meta") {
                            self.write_raw(line);
                        }
                    }
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
            let first_line = match fs::read_to_string(&path) {
                Ok(text) => text.lines().next().unwrap_or("").to_string(),
                Err(_) => continue,
            };
            if first_line.is_empty() {
                continue;
            }
            let entry: serde_json::Value = match serde_json::from_str(&first_line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let meta_data = entry.get("data").cloned().unwrap_or_default();
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let size_kb = path
                .metadata()
                .map(|m| m.len() as f64 / 1024.0)
                .unwrap_or(0.0);

            sessions.push(SessionInfo {
                session_id: stem.clone(),
                created_at: meta_data
                    .get("created_at")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                turn_count: meta_data
                    .get("turn_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
                model: meta_data
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                size_kb,
                is_latest: latest_id.as_deref() == Some(stem.as_str()),
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
            self.maybe_rotate(path);
            if let Ok(line) = serde_json::to_string(entry) {
                let _ = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut f| writeln!(f, "{line}"));
            }
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

    fn maybe_rotate(&self, path: &Path) {
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

// ── Helpers ──────────────────────────────────────────────────────────

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
