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
        env::set_var("MARC27_PLATFORM_URL", "https://api.marc27.com/");
        let endpoints = PlatformEndpoints::from_env();
        assert_eq!(endpoints.api_base, "https://api.marc27.com/api/v1");
        assert_eq!(
            endpoints.node_ws,
            "wss://api.marc27.com/api/v1/nodes/connect"
        );
        env::remove_var("MARC27_PLATFORM_URL");
    }
}
