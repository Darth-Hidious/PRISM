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
            ..Default::default()
        };

        paths.persist_credentials(&creds).unwrap();

        // Store 1: cli-state.json round-trips the rotated tokens.
        let stored = paths
            .load_cli_state()
            .unwrap()
            .credentials
            .expect("cli-state must hold credentials");
        assert_eq!(stored.access_token, "at-new");
        assert_eq!(stored.refresh_token, "rt-rotated");

        // Store 2: SDK mirror exists with the exact 6-field shape the Python
        // platform tools read.
        let mirror_path = home.join(".prism").join("credentials.json");
        assert!(mirror_path.exists(), "SDK mirror must be written on refresh");
        let mirror: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mirror_path).unwrap()).unwrap();
        assert_eq!(mirror["access_token"], "at-new");
        assert_eq!(mirror["refresh_token"], "rt-rotated");
        assert_eq!(mirror["platform_url"], "https://api.marc27.com");
        assert_eq!(mirror["user_id"], "u1");
        assert_eq!(mirror["org_id"], "o1");
        assert_eq!(mirror["project_id"], "p1");

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
}
