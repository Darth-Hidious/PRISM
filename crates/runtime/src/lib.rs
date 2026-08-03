// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared runtime primitives for PRISM Rust binaries.
//!
//! Provides [`PrismPaths`] (XDG-based directory discovery), [`PrismCliState`]
//! (credential persistence), and [`PlatformEndpoints`] (URL derivation for
//! the MARC27 platform API, WebSocket, and dashboard).

pub mod llm_resolve;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("could not resolve PRISM project directories")]
    ProjectDirsUnavailable,
    #[error("failed to read state file {path}: {source}")]
    ReadState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write state file {path}: {source}")]
    WriteState {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse state file {path}: {source}")]
    ParseState {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrismPaths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

/// The on-disk qualifier triple. The PRODUCT's name, not a company's: this
/// string is visible to every user as the path (`~/Library/Application
/// Support/dev.prism.prism` on macOS) and `prism status` prints it. A
/// company name here made PRISM look like a client for someone else's
/// platform.
const BUNDLE_ID: [&str; 3] = ["dev", "prism", "prism"];

/// The qualifier every shipped release up to and including v2.7.1 used.
/// Those installs are real — they hold `cli-state.json` (the login),
/// `node_key` / `node_signing_key` (the node's identity), and the audit,
/// session, rbac and subscription databases — so [`PrismPaths::discover`]
/// moves them across once rather than orphaning them. Frozen: changing it
/// strands everyone who has not upgraded yet.
const LEGACY_BUNDLE_ID: [&str; 3] = ["com", "marc27", "prism"];

impl PrismPaths {
    pub fn discover() -> Result<Self, RuntimeError> {
        let dirs = Self::for_bundle_id(BUNDLE_ID).ok_or(RuntimeError::ProjectDirsUnavailable)?;
        // Releases v1.0.0…v2.7.1 wrote under `com.marc27.prism`. Renaming the
        // bundle id without moving the data would silently log those installs
        // out, mint a second node identity on the next `node up`, and hide
        // their audit/rbac/subscription history. Best-effort by design — see
        // `migrate_dir`.
        dirs.migrate_legacy_install();
        Ok(dirs)
    }

    /// The four directories `directories` derives for one qualifier triple.
    /// `None` only when no home directory can be resolved at all.
    fn for_bundle_id(id: [&str; 3]) -> Option<Self> {
        let dirs = ProjectDirs::from(id[0], id[1], id[2])?;
        Some(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            state_dir: dirs
                .state_dir()
                .unwrap_or_else(|| dirs.data_local_dir())
                .to_path_buf(),
        })
    }

    /// One-time move of a pre-rename install into these directories.
    ///
    /// On platforms where the qualifier is not part of the path (Linux/XDG
    /// derives both from the application name alone) the old and new paths
    /// are identical and every pair is a no-op.
    fn migrate_legacy_install(&self) {
        let Some(legacy) = Self::for_bundle_id(LEGACY_BUNDLE_ID) else {
            return;
        };
        for (from, to) in [
            (&legacy.config_dir, &self.config_dir),
            (&legacy.cache_dir, &self.cache_dir),
            (&legacy.data_dir, &self.data_dir),
            (&legacy.state_dir, &self.state_dir),
        ] {
            migrate_dir(from, to);
        }
    }
}

/// Move one pre-rename directory to its new home, exactly once.
///
/// Skips unless the old directory exists and the new one does not, which is
/// what makes it both idempotent and non-destructive: a user who already
/// launched a renamed build and signed in again keeps that state, and a
/// second run after a successful move finds nothing left to do.
///
/// `rename` rather than copy: old and new are siblings under the same
/// parent, so it cannot cross a filesystem, it is atomic, and the 0600 modes
/// on `cli-state.json`, `node_key` and `node_signing_key` survive untouched
/// because the inodes never move.
///
/// Failure is reported and swallowed. A migration that cannot complete must
/// leave PRISM starting on an empty new directory — recoverable, and the old
/// data is still there — instead of refusing to start at all.
fn migrate_dir(from: &Path, to: &Path) {
    if from == to || to.exists() || !from.is_dir() {
        return;
    }
    if let Some(parent) = to.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        eprintln!("[prism] could not prepare {}: {e}", parent.display());
        return;
    }
    match fs::rename(from, to) {
        Ok(()) => eprintln!(
            "[prism] moved your PRISM data from {} to {}",
            from.display(),
            to.display()
        ),
        Err(e) => eprintln!(
            "[prism] could not move {} to {} ({e}); starting fresh at the new \
             location — your old data is untouched.",
            from.display(),
            to.display()
        ),
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct StoredCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub platform_url: String,
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredentials")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("platform_url", &self.platform_url)
            .field("user_id", &self.user_id)
            .field("display_name", &self.display_name)
            .field("org_id", &self.org_id)
            .field("org_name", &self.org_name)
            .field("project_id", &self.project_id)
            .field("project_name", &self.project_name)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PrismCliState {
    pub credentials: Option<StoredCredentials>,
    #[serde(default)]
    pub preferred_python: Option<String>,
}

/// A durable node token — a stable, non-rotating node-scoped API key minted
/// once and reused by `node up` so the daemon survives session refresh-token
/// rotation. Stored at [`PrismPaths::node_token_path`], separately from the
/// rotating [`StoredCredentials`].
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct StoredNodeToken {
    /// Full API key (`m27_…`). Sent as the WS auth token; never rotates.
    pub key: String,
    /// API-key row id (for revocation via `DELETE /api-keys/{id}`).
    pub id: String,
    /// Short prefix for display (`m27_abcd…`).
    pub prefix: String,
}

impl std::fmt::Debug for StoredNodeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredNodeToken")
            .field("key", &"[REDACTED]")
            .field("id", &self.id)
            .field("prefix", &self.prefix)
            .finish()
    }
}

impl PrismPaths {
    pub fn cli_state_path(&self) -> PathBuf {
        self.config_dir.join("cli-state.json")
    }

    pub fn load_cli_state(&self) -> Result<PrismCliState, RuntimeError> {
        let path = self.cli_state_path();
        if !path.exists() {
            return Ok(PrismCliState::default());
        }
        let text = fs::read_to_string(&path).map_err(|source| RuntimeError::ReadState {
            path: path.clone(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| RuntimeError::ParseState { path, source })
    }

    pub fn save_cli_state(&self, state: &PrismCliState) -> Result<(), RuntimeError> {
        fs::create_dir_all(&self.config_dir).map_err(|source| RuntimeError::WriteState {
            path: self.config_dir.clone(),
            source,
        })?;
        let path = self.cli_state_path();
        let text =
            serde_json::to_string_pretty(state).expect("serializing cli state should not fail");
        // Write with restricted permissions (0600) — file contains tokens.
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map_err(|source| RuntimeError::WriteState {
                    path: path.clone(),
                    source,
                })?;
            file.write_all(format!("{text}\n").as_bytes())
                .map_err(|source| RuntimeError::WriteState { path, source })
        }
        #[cfg(not(unix))]
        {
            fs::write(&path, format!("{text}\n"))
                .map_err(|source| RuntimeError::WriteState { path, source })
        }
    }

    /// Path to the SDK credential mirror (`~/.prism/credentials.json`) that the
    /// Python platform tools read. HOME-based (NOT the XDG `config_dir`) to
    /// match where `_platform_creds.py` looks.
    pub fn sdk_credentials_path() -> Option<PathBuf> {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(".prism").join("credentials.json"))
    }

    /// Persist credentials to BOTH stores: the authoritative `cli-state.json`
    /// AND the `~/.prism/credentials.json` SDK mirror.
    ///
    /// EVERY refresh MUST go through this. Writing only cli-state (the old
    /// silent-refresh behavior) left the SDK mirror holding a refresh token
    /// that single-use rotation had since REVOKED — replaying it tripped the
    /// server's token-family invalidation and forced a device-flow re-login
    /// (the "re-login every ~24h" drift). The cli-state write is authoritative
    /// and returns its error; the mirror is best-effort so a mirror hiccup can
    /// never fail a refresh.
    pub fn persist_credentials(&self, creds: &StoredCredentials) -> Result<(), RuntimeError> {
        let mut state = self.load_cli_state().unwrap_or_default();
        state.credentials = Some(creds.clone());
        self.save_cli_state(&state)?;
        Self::save_sdk_credentials(creds);
        Ok(())
    }

    /// Write the `~/.prism/credentials.json` SDK mirror (0600 on unix).
    /// Best-effort: errors are swallowed so a mirror write never fails auth.
    /// The JSON shape MUST stay in sync with the Python `_platform_creds.py`.
    pub fn save_sdk_credentials(creds: &StoredCredentials) {
        let Some(path) = Self::sdk_credentials_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mirror = serde_json::json!({
            "access_token": creds.access_token,
            "refresh_token": creds.refresh_token,
            "platform_url": creds.platform_url,
            "user_id": creds.user_id,
            "org_id": creds.org_id,
            "project_id": creds.project_id,
            // Persisted so a cli-state rebuilt from this mirror keeps the
            // expiry and the proactive (within-5-min) refresh fires. Without
            // it the mirror yields expires_at=None → every launch falls into
            // the reactive retry path. Extra field: ignored by readers that
            // don't know it (the Python _platform_creds.py reader), and
            // emitted as RFC 3339 so it deserializes back into DateTime<Utc>.
            "expires_at": creds.expires_at,
        });
        let Ok(json) = serde_json::to_string_pretty(&mirror) else {
            return;
        };
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut file) = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
            {
                let _ = file.write_all(json.as_bytes());
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&path, json);
        }
    }

    /// Path to the durable node-token file (`{state_dir}/node-token`).
    pub fn node_token_path(&self) -> PathBuf {
        self.state_dir.join("node-token")
    }

    /// Load the durable node token, if one is stored. Returns `None` when the
    /// file is absent or unreadable (never panics — the daemon falls back to
    /// the rotating session token).
    pub fn load_node_token(&self) -> Option<StoredNodeToken> {
        let path = self.node_token_path();
        let text = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Persist a durable node token with 0600 permissions (contains a key).
    pub fn save_node_token(&self, token: &StoredNodeToken) -> Result<(), RuntimeError> {
        let path = self.node_token_path();
        fs::create_dir_all(&self.state_dir).map_err(|source| RuntimeError::WriteState {
            path: self.state_dir.clone(),
            source,
        })?;
        let text =
            serde_json::to_string_pretty(token).expect("serializing node token should not fail");
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map_err(|source| RuntimeError::WriteState {
                    path: path.clone(),
                    source,
                })?;
            file.write_all(format!("{text}\n").as_bytes())
                .map_err(|source| RuntimeError::WriteState { path, source })
        }
        #[cfg(not(unix))]
        {
            fs::write(&path, format!("{text}\n"))
                .map_err(|source| RuntimeError::WriteState { path, source })
        }
    }

    /// Remove the durable node-token file (best-effort; returns whether it existed).
    pub fn clear_node_token(&self) -> bool {
        fs::remove_file(self.node_token_path()).is_ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformEndpoints {
    pub api_base: String,
    pub node_ws: String,
}

impl PlatformEndpoints {
    pub fn from_env() -> Self {
        let default_root = "https://api.marc27.com".to_string();
        let root = env::var("MARC27_PLATFORM_URL")
            .unwrap_or(default_root)
            .trim_end_matches('/')
            .to_string();

        let ws_root = if let Some(rest) = root.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = root.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            root.clone()
        };

        Self {
            api_base: format!("{root}/api/v1"),
            node_ws: format!("{ws_root}/api/v1/nodes/connect"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `env::set_var` mutates the process-global environment, which is not
    // thread-safe against concurrent env access on any variable. Serialize
    // every env-touching test through this guard.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn derives_api_and_ws_endpoints_from_platform_url() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            env::set_var("MARC27_PLATFORM_URL", "https://api.marc27.com/");
        }
        let endpoints = PlatformEndpoints::from_env();
        assert_eq!(endpoints.api_base, "https://api.marc27.com/api/v1");
        assert_eq!(
            endpoints.node_ws,
            "wss://api.marc27.com/api/v1/nodes/connect"
        );
        unsafe {
            env::remove_var("MARC27_PLATFORM_URL");
        }
    }

    // The regression guard for the "re-login every ~24h" dance: a refresh must
    // land in BOTH the authoritative `cli-state.json` AND the HOME-based
    // `~/.prism/credentials.json` SDK mirror, or the neglected store keeps a
    // server-revoked refresh token and the next start is forced back through
    // device flow.
    #[test]
    fn persist_credentials_writes_both_stores() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());

        let base = env::temp_dir().join(format!("prism-cred-test-{}", std::process::id()));
        let home = base.join("home");
        let cfg = base.join("config");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cfg).unwrap();

        let prev_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", &home);
        }

        let paths = PrismPaths {
            config_dir: cfg.clone(),
            cache_dir: base.join("cache"),
            data_dir: base.join("data"),
            state_dir: base.join("state"),
        };
        let creds = StoredCredentials {
            access_token: "at-new".into(),
            refresh_token: "rt-rotated".into(),
            platform_url: "https://api.marc27.com".into(),
            user_id: Some("u1".into()),
            org_id: Some("o1".into()),
            project_id: Some("p1".into()),
            // expires_at MUST survive into the mirror: without it a cli-state
            // rebuilt from the mirror gets expires_at=None → the proactive
            // (within-5-min) refresh never fires → every launch falls into the
            // reactive retry path. (F0 review fix #3.)
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(24)),
            ..Default::default()
        };

        paths.persist_credentials(&creds).unwrap();

        // Store 1: cli-state.json round-trips the rotated tokens + expiry.
        let stored = paths
            .load_cli_state()
            .unwrap()
            .credentials
            .expect("cli-state must hold credentials");
        assert_eq!(stored.access_token, "at-new");
        assert_eq!(stored.refresh_token, "rt-rotated");
        assert!(
            stored.expires_at.is_some(),
            "cli-state must persist expires_at"
        );

        // Store 2: SDK mirror exists with the 6-field Python shape PLUS
        // expires_at (review fix #3).
        let mirror_path = home.join(".prism").join("credentials.json");
        assert!(
            mirror_path.exists(),
            "SDK mirror must be written on refresh"
        );
        let mirror: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mirror_path).unwrap()).unwrap();
        assert_eq!(mirror["access_token"], "at-new");
        assert_eq!(mirror["refresh_token"], "rt-rotated");
        assert_eq!(mirror["platform_url"], "https://api.marc27.com");
        assert_eq!(mirror["user_id"], "u1");
        assert_eq!(mirror["org_id"], "o1");
        assert_eq!(mirror["project_id"], "p1");
        assert!(
            !mirror["expires_at"].is_null(),
            "SDK mirror MUST carry expires_at (F0 review fix #3) — a mirror \
             without it yields expires_at=None on rebuild and the proactive \
             refresh never fires"
        );
        // And it must deserialize back into a DateTime<Utc> (the shape
        // StoredCredentials expects on reload).
        assert!(
            serde_json::from_value::<chrono::DateTime<chrono::Utc>>(mirror["expires_at"].clone())
                .is_ok(),
            "mirror expires_at must be a valid RFC 3339 timestamp"
        );

        // The mirror holds bearer + refresh tokens — must be owner-only (0600).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&mirror_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "SDK mirror must be 0600");
        }

        unsafe {
            match prev_home {
                Some(h) => env::set_var("HOME", h),
                None => env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn stored_node_token_roundtrips() {
        let token = StoredNodeToken {
            key: "m27_secret_key_value".into(),
            id: "00000000-0000-0000-0000-000000000001".into(),
            prefix: "m27_abcd".into(),
        };
        let json = serde_json::to_string(&token).unwrap();
        let parsed: StoredNodeToken = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, token);
    }

    #[test]
    fn stored_node_token_debug_redacts_the_key() {
        let token = StoredNodeToken {
            key: "m27_DO_NOT_LEAK_THIS".into(),
            id: "id-1".into(),
            prefix: "m27_xy".into(),
        };
        let dbg = format!("{token:?}");
        assert!(
            !dbg.contains("DO_NOT_LEAK_THIS"),
            "debug leaked the key: {dbg}"
        );
        assert!(dbg.contains("id-1") && dbg.contains("m27_xy"));
        assert!(dbg.contains("[REDACTED]"));
    }

    /// The durable-token path is a single source of truth shared by the daemon
    /// (load) and the CLI (mint/revoke) — they must agree.
    #[test]
    fn node_token_path_is_under_state_dir() {
        let paths = PrismPaths {
            config_dir: std::path::PathBuf::from("/tmp/prism-cfg"),
            cache_dir: std::path::PathBuf::from("/tmp/prism-cache"),
            data_dir: std::path::PathBuf::from("/tmp/prism-data"),
            state_dir: std::path::PathBuf::from("/tmp/prism-state"),
        };
        assert_eq!(
            paths.node_token_path(),
            std::path::PathBuf::from("/tmp/prism-state/node-token")
        );
    }

    /// save → load → clear round-trip in a temp dir: proves the daemon's
    /// preference for a stored node token actually reads back what mint wrote.
    #[test]
    fn save_load_clear_node_token_roundtrip() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "prism-node-token-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = PrismPaths {
            config_dir: dir.join("cfg"),
            cache_dir: dir.join("cache"),
            data_dir: dir.join("data"),
            state_dir: dir.clone(),
        };

        // Absent → None (daemon falls back to the rotating session token).
        assert!(paths.load_node_token().is_none());

        // Save + load returns exactly what was stored.
        let token = StoredNodeToken {
            key: "m27_roundtrip_key".into(),
            id: "id-42".into(),
            prefix: "m27_rt".into(),
        };
        paths.save_node_token(&token).unwrap();
        assert_eq!(paths.load_node_token().as_ref(), Some(&token));

        // The file must be 0600 on unix (it contains a key).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.node_token_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "node-token file is not 0600");
        }

        // Clear removes it → back to None.
        assert!(paths.clear_node_token());
        assert!(paths.load_node_token().is_none());
        // Clear is idempotent once gone (returns false, no panic).
        assert!(!paths.clear_node_token());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Pre-rename install migration (`com.marc27.prism` → `dev.prism.prism`) ──

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Seed a directory shaped like a real pre-rename install: the login
    /// state and the node's identity keys at 0600, a world-readable database,
    /// and a nested directory.
    fn seed_legacy_install(dir: &Path, marker: &str) {
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("cli-state.json"), marker).unwrap();
        fs::write(dir.join("node_key"), b"raw-32-bytes").unwrap();
        fs::write(dir.join("node_signing_key"), b"raw-32-bytes").unwrap();
        fs::write(dir.join("audit.db"), b"sqlite").unwrap();
        fs::write(dir.join("nested").join("sessions.db"), b"sqlite").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for secret in ["cli-state.json", "node_key", "node_signing_key"] {
                fs::set_permissions(dir.join(secret), fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }

    /// The whole point of the migration: everything an install owns arrives
    /// at the new location, and the owner-only modes on the credential and
    /// node-identity files survive the move.
    #[test]
    fn migrate_dir_moves_a_legacy_install_preserving_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");
        seed_legacy_install(&legacy, "the-original-login");

        migrate_dir(&legacy, &new);

        assert_eq!(
            fs::read_to_string(new.join("cli-state.json")).unwrap(),
            "the-original-login",
            "the login must survive the bundle-id rename"
        );
        for carried in ["node_key", "node_signing_key", "audit.db"] {
            assert!(new.join(carried).exists(), "{carried} was left behind");
        }
        assert!(
            new.join("nested").join("sessions.db").exists(),
            "nested databases must come across too"
        );
        #[cfg(unix)]
        for secret in ["cli-state.json", "node_key", "node_signing_key"] {
            assert_eq!(
                mode_of(&new.join(secret)),
                0o600,
                "{secret} must stay owner-only after migration"
            );
        }
        assert!(!legacy.exists(), "the old directory should be gone");
    }

    /// Run it twice — the second launch of an upgraded build must be a
    /// no-op, not a second move that undoes the first.
    #[test]
    fn migrate_dir_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");
        seed_legacy_install(&legacy, "the-original-login");

        migrate_dir(&legacy, &new);
        fs::write(new.join("cli-state.json"), "refreshed-after-upgrade").unwrap();
        migrate_dir(&legacy, &new);

        assert_eq!(
            fs::read_to_string(new.join("cli-state.json")).unwrap(),
            "refreshed-after-upgrade",
            "a second run must not resurrect the pre-migration state"
        );
    }

    /// Someone who already ran the un-migrated build and signed in again has
    /// live state at the new location. Migration must never overwrite it.
    #[test]
    fn migrate_dir_never_clobbers_the_new_location() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");
        seed_legacy_install(&legacy, "stale-login");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("cli-state.json"), "the-login-in-use").unwrap();

        migrate_dir(&legacy, &new);

        assert_eq!(
            fs::read_to_string(new.join("cli-state.json")).unwrap(),
            "the-login-in-use",
            "an existing new-location file must win"
        );
        assert!(
            legacy.exists(),
            "with nothing to do, the old directory is left for the user to inspect"
        );
    }

    /// A fresh install has no legacy directory. Migration must not conjure
    /// the new one either — `discover()` runs on every invocation, including
    /// ones that go on to touch no state at all.
    #[test]
    fn migrate_dir_does_nothing_without_a_legacy_install() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("com.marc27.prism");
        let new = tmp.path().join("dev.prism.prism");

        migrate_dir(&legacy, &new);

        assert!(!new.exists(), "no legacy install ⇒ nothing to create");
    }

    /// The wiring test: `discover()` itself must perform the migration.
    /// Releases v1.0.0…v2.7.1 wrote under `com.marc27.prism`; without this
    /// call an upgrade silently logs the user out.
    ///
    /// On platforms where the qualifier is not part of the path (Linux/XDG)
    /// the two locations coincide and this passes trivially — which is
    /// correct: there is nothing to migrate there.
    #[test]
    fn discover_adopts_a_pre_rename_install() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", tmp.path());
        }

        let legacy = PrismPaths::for_bundle_id(LEGACY_BUNDLE_ID).expect("legacy dirs under $HOME");
        fs::create_dir_all(&legacy.config_dir).unwrap();
        let state = PrismCliState {
            credentials: Some(StoredCredentials {
                access_token: "at-from-the-old-bundle-id".into(),
                ..Default::default()
            }),
            preferred_python: None,
        };
        legacy.save_cli_state(&state).unwrap();

        let paths = PrismPaths::discover().unwrap();
        let carried = paths
            .load_cli_state()
            .unwrap()
            .credentials
            .expect("an upgrade must not log the user out");

        unsafe {
            match prev_home {
                Some(h) => env::set_var("HOME", h),
                None => env::remove_var("HOME"),
            }
        }

        assert_eq!(carried.access_token, "at-from-the-old-bundle-id");
        #[cfg(unix)]
        assert_eq!(
            mode_of(&paths.cli_state_path()),
            0o600,
            "the migrated credential file must still be owner-only"
        );
    }
}
