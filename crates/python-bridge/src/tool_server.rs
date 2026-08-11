//! Thin wrapper around `python3 -m app.tool_server` — spawns the Python tool
//! server as a child process and communicates via JSON-line stdio protocol.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::PythonBridgeError;

/// The Python module that IS the tool server.
///
/// Exported so `prism status` can report it instead of restating it. It
/// used to restate it as `app.backend`, a module that does not exist —
/// a label nobody could act on, and nothing tied the two together.
pub const TOOL_SERVER_MODULE: &str = "app.tool_server";

/// Configuration for spawning a Python tool server.
pub struct ToolServer {
    pub python_bin: PathBuf,
    pub project_root: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// Handle to a running tool server child process.
pub struct ToolServerHandle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ToolServer {
    /// Spawn `python3 -m app.tool_server` and return a handle for communication.
    pub async fn spawn(&self) -> Result<ToolServerHandle, PythonBridgeError> {
        self.spawn_inner(false).await
    }

    /// Spawn with an empty inherited environment, adding only [`Self::env`].
    ///
    /// This is the LocalOnly credential boundary. Callers must construct
    /// `env` as an allowlist; unlike [`Self::spawn`], a credential added to the
    /// parent process next month cannot silently appear in this child.
    pub async fn spawn_with_clean_environment(
        &self,
    ) -> Result<ToolServerHandle, PythonBridgeError> {
        self.spawn_inner(true).await
    }

    async fn spawn_inner(
        &self,
        clear_environment: bool,
    ) -> Result<ToolServerHandle, PythonBridgeError> {
        let mut cmd = Command::new(&self.python_bin);
        cmd.arg("-m")
            .arg(TOOL_SERVER_MODULE)
            .current_dir(&self.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if clear_environment {
            cmd.env_clear();
        }
        cmd.envs(&self.env);

        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        tracing::info!(
            cwd = %self.project_root.display(),
            "spawned python tool server"
        );

        Ok(ToolServerHandle {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl ToolServerHandle {
    /// Send a JSON request and read one JSON-line response.
    pub async fn call(&mut self, request: &Value) -> Result<Value, PythonBridgeError> {
        let mut line = serde_json::to_string(request)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut response_line = String::new();
        // S5: a hard cap on how long the agent waits for ANY tool-server
        // response. The Python tools have their own internal deadlines (e.g.
        // materials_search's timeout_seconds), but without this ceiling a
        // wedged/looping tool could pin the agent forever. 60s is generous —
        // it only fires when something is genuinely broken (the tool ignored
        // its own deadline), in which case surfacing the timeout is correct.
        let timeout_dur = std::time::Duration::from_secs(60);
        let bytes_read =
            tokio::time::timeout(timeout_dur, self.stdout.read_line(&mut response_line))
                .await
                .map_err(|_| PythonBridgeError::Timeout(timeout_dur))??;
        if bytes_read == 0 {
            return Err(PythonBridgeError::WorkerExited);
        }

        serde_json::from_str(&response_line).map_err(PythonBridgeError::Parse)
    }

    /// List all available tools from the Python registry.
    pub async fn list_tools(&mut self) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({"method": "list_tools"});
        self.call(&req).await
    }

    /// Call a named tool with the given arguments.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({
            "method": "call_tool",
            "tool": name,
            "args": args,
        });
        self.call(&req).await
    }

    /// Set the artifact recorder's authoritative session identifier.
    pub async fn set_session_id(&mut self, session_id: &str) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({
            "method": "set_session_id",
            "session_id": session_id,
        });
        self.call(&req).await
    }

    /// Kill the child process.
    pub async fn shutdown(&mut self) -> Result<(), PythonBridgeError> {
        self.child.kill().await?;
        tracing::info!("tool server shut down");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_executable() -> Option<PathBuf> {
        let output = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string()))
    }

    #[tokio::test]
    async fn clean_environment_worker_inherits_nothing_and_keeps_explicit_offline_flag() {
        let Some(python_bin) = python_executable() else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let project = tempfile::tempdir().expect("temp project");
        let app = project.path().join("app");
        std::fs::create_dir_all(&app).expect("create app package");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        std::fs::write(
            app.join("tool_server.py"),
            r#"import json
import os
import sys
for line in sys.stdin:
    request = json.loads(line)
    if request.get("method") == "set_session_id":
        response = {
            "status": "ok",
            "session_id": request.get("session_id"),
        }
    else:
        response = {"result": {
            "offline": os.environ.get("PRISM_OFFLINE"),
            "home": os.environ.get("HOME"),
        }}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#,
        )
        .expect("write worker");

        let server = ToolServer {
            python_bin,
            project_root: project.path().to_path_buf(),
            env: BTreeMap::from([("PRISM_OFFLINE".to_string(), "1".to_string())]),
        };
        let mut worker = server
            .spawn_with_clean_environment()
            .await
            .expect("spawn clean worker");
        let response = worker
            .call_tool("environment_probe", serde_json::json!({}))
            .await
            .expect("call environment probe");

        assert_eq!(response["result"]["offline"], "1");
        assert!(response["result"]["home"].is_null(), "response: {response}");

        let response = worker
            .set_session_id("session-from-rust")
            .await
            .expect("set worker session id");
        assert_eq!(response["status"], "ok");
        assert_eq!(response["session_id"], "session-from-rust");
        worker.shutdown().await.expect("shutdown worker");
    }
}
