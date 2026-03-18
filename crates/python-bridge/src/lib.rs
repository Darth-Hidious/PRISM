//! Python worker launch and supervision primitives.
//!
//! This crate intentionally starts small: the first job is to make Python an
//! explicitly supervised worker process rather than the top-level CLI shell.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

#[derive(Debug, thiserror::Error)]
pub enum PythonBridgeError {
    #[error("failed to spawn python worker: {0}")]
    Spawn(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PythonWorkerConfig {
    pub python_bin: PathBuf,
    pub module: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl PythonWorkerConfig {
    pub fn backend(project_root: impl Into<PathBuf>) -> Self {
        Self {
            python_bin: PathBuf::from("python3"),
            module: "app.backend".to_string(),
            cwd: project_root.into(),
            env: BTreeMap::new(),
        }
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.python_bin);
        cmd.arg("-m")
            .arg(&self.module)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        cmd
    }

    pub async fn spawn(&self) -> Result<Child, PythonBridgeError> {
        let child = self.command().spawn()?;
        tracing::info!(
            module = %self.module,
            cwd = %self.cwd.display(),
            "spawned python worker"
        );
        Ok(child)
    }

    pub fn stdio_command(&self) -> Command {
        let mut cmd = Command::new(&self.python_bin);
        cmd.arg("-m")
            .arg(&self.module)
            .current_dir(&self.cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        cmd
    }
}
