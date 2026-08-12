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
///
/// `Clone` because a [`crate::pool::ToolServerPool`] keeps the config and
/// spawns a fresh child from it for every lane it opens.
#[derive(Clone)]
pub struct ToolServer {
    pub python_bin: PathBuf,
    pub project_root: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// Hard ceiling on how long the agent waits for ANY tool-server response.
///
/// S5: the Python tools have their own internal deadlines (e.g.
/// materials_search's timeout_seconds), but without this ceiling a
/// wedged/looping tool could pin the agent forever. 60s is generous — it only
/// fires when something is genuinely broken (the tool ignored its own
/// deadline), in which case surfacing the timeout is correct.
pub const DEFAULT_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Handle to a running tool server child process.
pub struct ToolServerHandle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// `Some(reason)` once a call failed mid-exchange (partial write, timeout,
    /// worker exit, junk on stdout). The protocol is one request line → one
    /// response line with NO request ids, so after such a failure the next
    /// line on the pipe may be the FAILED call's late response — reading it
    /// would hand one caller's response to another caller. That is why a
    /// desynchronized handle refuses every further call instead of silently
    /// resuming: in a provenance system a misattributed tool result is
    /// corruption, not a glitch. Pool lanes discard + respawn the child;
    /// bare-handle owners must spawn a replacement.
    desynchronized: Option<&'static str>,
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
            .stderr(Stdio::piped())
            // Reap the child when the handle is dropped without an explicit
            // `shutdown()`. Long-lived singletons always shut down explicitly;
            // pool lanes rely on this so a discarded (desynchronized/dead)
            // child never lingers as an orphaned Python process.
            .kill_on_drop(true);
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
            desynchronized: None,
        })
    }
}

impl ToolServerHandle {
    /// Send a JSON request and read one JSON-line response, waiting at most
    /// [`DEFAULT_CALL_TIMEOUT`] (see its doc for why the ceiling exists).
    pub async fn call(&mut self, request: &Value) -> Result<Value, PythonBridgeError> {
        self.call_with_timeout(request, DEFAULT_CALL_TIMEOUT).await
    }

    /// [`Self::call`] with an explicit response deadline (pool policies and
    /// tests declare their own).
    ///
    /// Any failure BETWEEN writing the request and parsing the response marks
    /// the handle desynchronized (see [`ToolServerHandle::desynchronized`]):
    /// the wire protocol has no request ids, so once a request/response pair
    /// is broken the next line on the pipe cannot be attributed to any caller
    /// and every further call is refused with the specific reason.
    pub async fn call_with_timeout(
        &mut self,
        request: &Value,
        timeout_dur: std::time::Duration,
    ) -> Result<Value, PythonBridgeError> {
        if let Some(reason) = self.desynchronized {
            return Err(PythonBridgeError::Desynchronized { reason });
        }
        // Serialization failure writes nothing — the pipe stays aligned.
        let mut line = serde_json::to_string(request)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        if let Err(e) = self.stdin.write_all(line.as_bytes()).await {
            self.desynchronized = Some("failed mid-write (the request may be partially written)");
            return Err(e.into());
        }
        if let Err(e) = self.stdin.flush().await {
            self.desynchronized = Some("failed mid-write (the request may be partially written)");
            return Err(e.into());
        }

        let mut response_line = String::new();
        let bytes_read = match tokio::time::timeout(
            timeout_dur,
            self.stdout.read_line(&mut response_line),
        )
        .await
        {
            // The response is still owed on the pipe — a later call would
            // read THIS call's late response as its own.
            Err(_elapsed) => {
                self.desynchronized = Some("timed out with its response still owed on the pipe");
                return Err(PythonBridgeError::Timeout(timeout_dur));
            }
            Ok(Err(e)) => {
                self.desynchronized = Some("failed mid-read");
                return Err(e.into());
            }
            Ok(Ok(n)) => n,
        };
        if bytes_read == 0 {
            self.desynchronized = Some("lost its worker (stdout closed mid-call)");
            return Err(PythonBridgeError::WorkerExited);
        }

        match serde_json::from_str(&response_line) {
            Ok(value) => Ok(value),
            Err(e) => {
                // A non-protocol line (e.g. a library print that escaped to
                // stdout) means framing can no longer be trusted.
                self.desynchronized = Some("got a non-protocol line on stdout");
                Err(PythonBridgeError::Parse(e))
            }
        }
    }

    /// Whether this handle refused/will refuse calls because a previous call
    /// broke request/response alignment. A desynchronized child must be
    /// replaced, never reused (a pool does this automatically on lane return).
    #[must_use]
    pub fn is_desynchronized(&self) -> bool {
        self.desynchronized.is_some()
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

    /// The wire protocol has no request ids: after a timeout the child still
    /// owes the timed-out call's response, so the next read on the same pipe
    /// would hand caller B caller A's payload. The handle must refuse with
    /// the specific desynchronization error rather than misdeliver.
    #[tokio::test]
    async fn desynchronized_handle_refuses_instead_of_misdelivering() {
        let Some(python_bin) = python_executable() else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let project = tempfile::tempdir().expect("temp project");
        let app = project.path().join("app");
        std::fs::create_dir_all(&app).expect("create app package");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        // Sleeps 1.5s per request, then echoes the request's token.
        std::fs::write(
            app.join("tool_server.py"),
            r#"import json, sys, time
for line in sys.stdin:
    request = json.loads(line)
    time.sleep(1.5)
    sys.stdout.write(json.dumps({"result": {"token": request.get("args", {}).get("token")}}) + "\n")
    sys.stdout.flush()
"#,
        )
        .expect("write worker");

        let server = ToolServer {
            python_bin,
            project_root: project.path().to_path_buf(),
            env: BTreeMap::new(),
        };
        let mut worker = server.spawn().await.expect("spawn worker");

        let slow = serde_json::json!({
            "method": "call_tool", "tool": "echo", "args": { "token": "caller-A" },
        });
        let err = worker
            .call_with_timeout(&slow, std::time::Duration::from_millis(100))
            .await
            .expect_err("the 1.5s response cannot beat a 100ms deadline");
        assert!(matches!(err, PythonBridgeError::Timeout(_)), "got: {err}");
        assert!(worker.is_desynchronized());

        // Caller B on the same handle: without the refusal this would read
        // caller A's late {"token": "caller-A"} line as B's response.
        let fast = serde_json::json!({
            "method": "call_tool", "tool": "echo", "args": { "token": "caller-B" },
        });
        let err = worker
            .call(&fast)
            .await
            .expect_err("a desynchronized pipe must refuse further calls");
        assert!(
            matches!(err, PythonBridgeError::Desynchronized { .. }),
            "got: {err}"
        );
        assert!(
            err.to_string().contains("timed out"),
            "the refusal must name the original fault: {err}"
        );
        worker.shutdown().await.expect("shutdown worker");
    }
}
