//! Thin wrapper around `python3 -m app.tool_server` — spawns the Python tool
//! server as a child process and communicates via JSON-line stdio protocol.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::PythonBridgeError;

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
        let mut cmd = Command::new(&self.python_bin);
        cmd.arg("-m")
            .arg("app.tool_server")
            .current_dir(&self.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

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

        self.stdin
            .write_all(line.as_bytes())
            .await?;
        self.stdin.flush().await?;

        let mut response_line = String::new();
        let bytes_read = self.stdout.read_line(&mut response_line).await?;
        if bytes_read == 0 {
            return Err(PythonBridgeError::Spawn(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "tool server process closed stdout",
            )));
        }

        serde_json::from_str(&response_line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e).into())
    }

    /// List all available tools from the Python registry.
    pub async fn list_tools(&mut self) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({"method": "list_tools"});
        self.call(&req).await
    }

    /// Call a named tool with the given arguments.
    pub async fn call_tool(
        &mut self,
        name: &str,
        args: Value,
    ) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({
            "method": "call_tool",
            "tool": name,
            "args": args,
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
