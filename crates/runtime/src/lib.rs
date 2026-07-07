// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Shared runtime primitives for PRISM Rust binaries.
//!
//! Provides [`PrismPaths`] (XDG-based directory discovery), [`PrismCliState`]
//! (credential persistence), and [`PlatformEndpoints`] (URL derivation for
//! the MARC27 platform API, WebSocket, and dashboard).

use std::env;
use std::fs;
use std::path::PathBuf;

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

impl PrismPaths {
    pub fn discover() -> Result<Self, RuntimeError> {
        let dirs = ProjectDirs::from("com", "marc27", "prism")
            .ok_or(RuntimeError::ProjectDirsUnavailable)?;

        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            state_dir: dirs
                .state_dir()
                .unwrap_or_else(|| dirs.data_local_dir())
                .to_path_buf(),
        })
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

    #[test]
    fn derives_api_and_ws_endpoints_from_platform_url() {
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
}
