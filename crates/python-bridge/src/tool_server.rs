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

/// How often an unbounded call says it is still running. Not a ceiling: the
/// line is the operator's signal that a tool is slow, and their cue to set
/// one if they want it.
pub const CALL_HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(60);

/// The operator's response ceiling: `PRISM_TOOL_CALL_TIMEOUT_SECS`, else
/// NONE.
///
/// There used to be a 60 s default, documented as "generous — it only fires
/// when something is genuinely broken". That was false twice over: a plain
/// `structure` build for tungsten exceeded it on an ordinary run, and on
/// 2026-09-02 it fired in a live session and took the next call down with it
/// ("tool server pipe desynchronized") — the only real error of that day.
/// Legitimate scientific work is allowed to be slow; PRISM imposes no
/// deadline on it. The operator may. A wedged tool is not silent either: an
/// unbounded call logs a heartbeat every [`CALL_HEARTBEAT`].
///
/// What is never allowed is an unattributable response, which is why a call
/// that exceeds an OPERATOR-set ceiling still desynchronizes the handle.
///
/// Read per call rather than cached so a long-running session can be retuned
/// without a restart. A value that is not a positive integer is ignored in
/// favour of the default: a malformed ceiling must not become "zero" (every
/// call would fail instantly).
#[must_use]
pub fn call_timeout() -> Option<std::time::Duration> {
    parse_call_timeout(
        std::env::var("PRISM_TOOL_CALL_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

/// Margin added to a tool's own promise before PRISM stops waiting: the
/// tool's subprocess is killed at its deadline and the result still has to be
/// serialised and written.
pub const PROMISE_MARGIN_SECS: u64 = 60;

/// Tools that run code in a subprocess with a deadline of their own:
/// (name, default seconds when the call gives none, hard maximum).
const PROMISING_TOOLS: &[(&str, u64, u64)] =
    &[("execute_python", 60, 300), ("execute_bash", 60, 300)];

/// How long to wait for this call. A tool that promises to finish within N
/// seconds (explicitly in its `timeout` argument, or by its documented
/// default) is waited for N plus [`PROMISE_MARGIN_SECS`]; a tool that
/// promises nothing keeps the operator's ceiling, which may be none.
///
/// Measured 2026-09-05: an execute_python call never answered — the server
/// sat on a condition variable with no child process — and with no ceiling
/// the agent waited on the pipe for the rest of the session. Scientific work
/// is still allowed to be slow: only a tool's own promise bounds the wait.
#[must_use]
pub fn ceiling_for_call(
    tool: &str,
    args: &Value,
    operator_ceiling: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    let Some((_, default_secs, max_secs)) = PROMISING_TOOLS.iter().find(|(n, _, _)| *n == tool)
    else {
        return operator_ceiling;
    };
    let promised = args
        .get("timeout")
        .and_then(|v| v.as_f64())
        .filter(|s| *s > 0.0)
        .map_or(*default_secs, |s| s.ceil() as u64)
        .min(*max_secs);
    let own = std::time::Duration::from_secs(promised + PROMISE_MARGIN_SECS);
    Some(operator_ceiling.map_or(own, |op| op.min(own)))
}

/// [`call_timeout`]'s decision, separated from the environment so it is
/// testable without mutating global state.
#[must_use]
pub fn parse_call_timeout(raw: Option<&str>) -> Option<std::time::Duration> {
    match raw.map(str::trim) {
        Some(value) if !value.is_empty() => match value.parse::<u64>() {
            Ok(secs) if secs > 0 => Some(std::time::Duration::from_secs(secs)),
            _ => None,
        },
        _ => None,
    }
}

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
    /// How this child was spawned, kept so a desynchronized handle can replace
    /// it with an IDENTICAL one — see [`ToolServerHandle::recover`].
    origin: ToolServer,
    /// Whether the child was spawned with a cleared environment. Recovery must
    /// preserve this: respawning a `spawn_with_clean_environment` handle
    /// through the inheriting path would silently widen the LocalOnly
    /// credential boundary that flag exists to draw.
    clean_environment: bool,
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
            origin: self.clone(),
            clean_environment: clear_environment,
        })
    }
}

impl ToolServerHandle {
    /// Send a JSON request and read one JSON-line response, waiting at most
    /// [`call_timeout`] (see [`call_timeout`] for why a ceiling exists
    /// and why the operator may raise it).
    pub async fn call(&mut self, request: &Value) -> Result<Value, PythonBridgeError> {
        self.call_with_timeout(request, call_timeout()).await
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
        timeout_dur: Option<std::time::Duration>,
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
        let started = std::time::Instant::now();
        let read = self.stdout.read_line(&mut response_line);
        tokio::pin!(read);
        let outcome = loop {
            let slice = timeout_dur.unwrap_or(CALL_HEARTBEAT);
            match tokio::time::timeout(slice, &mut read).await {
                Ok(result) => break result,
                Err(_elapsed) if timeout_dur.is_some() => {
                    self.desynchronized =
                        Some("timed out with its response still owed on the pipe");
                    return Err(PythonBridgeError::Timeout(slice));
                }
                Err(_elapsed) => tracing::info!(
                    elapsed_secs = started.elapsed().as_secs(),
                    "tool call still running — no ceiling is set (PRISM_TOOL_CALL_TIMEOUT_SECS)"
                ),
            }
        };
        let bytes_read = match outcome {
            Err(e) => {
                self.desynchronized = Some("failed mid-read");
                return Err(e.into());
            }
            Ok(n) => n,
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
    /// Replace a desynchronized child with an identical fresh one.
    ///
    /// A timed-out call leaves its response still owed on the pipe, so the
    /// handle refuses every further call rather than misattribute it. That is
    /// correct, but for a long-lived owner it also means one slow tool ends the
    /// session: every later tool, unrelated to the one that stalled, fails.
    /// Observed live — a `structure` build overran the ceiling and the next
    /// call, `prior_art_search`, died with it.
    ///
    /// Pool lanes already discard and replace such a child. This gives the same
    /// recovery to a bare handle. The old child is dropped, and `kill_on_drop`
    /// reaps it, so the stalled worker cannot linger and write its late
    /// response to a pipe someone is still reading.
    ///
    /// Returns `Ok(false)` when the handle was healthy and nothing was done, so
    /// a caller can log an actual replacement without guessing. Session state
    /// does NOT survive: the replacement is a new process, and an owner that
    /// bound a session id to the old child must bind it again.
    pub async fn recover(&mut self) -> Result<bool, PythonBridgeError> {
        if self.desynchronized.is_none() {
            return Ok(false);
        }
        let replacement = if self.clean_environment {
            self.origin.spawn_with_clean_environment().await?
        } else {
            self.origin.spawn().await?
        };
        // Assign only after the replacement exists: a failed spawn must leave
        // the handle refusing calls, never holding a half-replaced child.
        *self = replacement;
        tracing::info!("replaced desynchronized python tool server");
        Ok(true)
    }

    #[must_use]
    pub fn is_desynchronized(&self) -> bool {
        self.desynchronized.is_some()
    }

    /// List all available tools from the Python registry.
    pub async fn list_tools(&mut self) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({"method": "list_tools"});
        self.call(&req).await
    }

    /// Rebuild the tool catalog in the RUNNING worker.
    ///
    /// A tool the agent wrote is invisible until the catalog is rebuilt, and
    /// rebuilding it by restarting the kernel throws away every variable,
    /// every loaded dataset and the notebook the human is working in. That is
    /// an absurd price for the harness to learn that a file appeared.
    ///
    /// Returns the worker's report: the new count, and which tools appeared or
    /// vanished. A failed rebuild keeps the previous catalog serving.
    pub async fn reload_tools(&mut self) -> Result<Value, PythonBridgeError> {
        let req = serde_json::json!({"method": "reload_tools"});
        self.call(&req).await
    }

    /// Call a named tool with the given arguments. The wait is bounded by the
    /// tool's own promise when it makes one (see [`ceiling_for_call`]).
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, PythonBridgeError> {
        let ceiling = ceiling_for_call(name, &args, call_timeout());
        let req = serde_json::json!({
            "method": "call_tool",
            "tool": name,
            "args": args,
        });
        self.call_with_timeout(&req, ceiling).await
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
            .call_with_timeout(&slow, Some(std::time::Duration::from_millis(100)))
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

    /// The 60 s default ceiling killed legitimate calls (a tungsten structure
    /// build; a live session on 2026-09-02). With no operator ceiling a slow
    /// tool is simply waited for — here a 1.5 s reply against no deadline.
    #[tokio::test]
    async fn without_an_operator_ceiling_a_slow_tool_is_waited_for() {
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
            "method": "call_tool", "tool": "echo", "args": { "token": "patient" },
        });
        let reply = worker
            .call_with_timeout(&slow, None)
            .await
            .expect("no ceiling: the reply arrives when the tool is done");
        assert!(reply.to_string().contains("patient"), "{reply}");
        assert!(!worker.is_desynchronized(), "a slow reply is not a fault");
    }

    /// The ceiling documented itself as only firing "when something is
    /// genuinely broken". A real `structure` build for tungsten exceeded it,
    /// so an operator must be able to raise it. Pinning the default here as
    /// well means a silent change to either value fails this test.
    #[test]
    fn the_operator_can_raise_the_response_ceiling() {
        assert_eq!(
            parse_call_timeout(Some("900")),
            Some(std::time::Duration::from_secs(900)),
            "an explicit ceiling must be honoured"
        );
        assert_eq!(
            parse_call_timeout(Some("  900  ")),
            Some(std::time::Duration::from_secs(900)),
            "surrounding whitespace is not a malformed value"
        );
        assert_eq!(
            parse_call_timeout(None),
            None,
            "no ceiling unless the operator sets one"
        );
    }

    /// A malformed ceiling must not become "no ceiling" (the agent would hang
    /// forever on a wedged tool) or "zero" (every call would fail instantly).
    /// Both failure modes are worse than ignoring the value.
    #[test]
    fn a_malformed_ceiling_is_ignored_not_turned_into_zero() {
        for raw in [
            "0",
            "-5",
            "abc",
            "",
            "   ",
            "12.5",
            "9999999999999999999999",
        ] {
            assert_eq!(
                parse_call_timeout(Some(raw)),
                None,
                "{raw:?} must be ignored, leaving no ceiling"
            );
        }
    }

    /// Refusing after a desync is correct, but for a long-lived owner it also
    /// means one slow tool ends the session. Recovery must give back a WORKING
    /// handle whose answers are its own — the stalled child's late
    /// `caller-A` line must never surface as a later caller's result.
    #[tokio::test]
    async fn recovery_replaces_the_child_and_never_serves_the_stale_answer() {
        let Some(python_bin) = python_executable() else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let project = tempfile::tempdir().expect("temp project");
        let app = project.path().join("app");
        std::fs::create_dir_all(&app).expect("create app package");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        // First request is slow enough to blow a short deadline; later ones
        // answer immediately, so a healthy child is visibly responsive.
        std::fs::write(
            app.join("tool_server.py"),
            r#"import json, sys, time
first = True
for line in sys.stdin:
    request = json.loads(line)
    if first:
        first = False
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
        worker
            .call_with_timeout(&slow, Some(std::time::Duration::from_millis(100)))
            .await
            .expect_err("the 1.5s response cannot beat a 100ms deadline");
        assert!(
            worker.is_desynchronized(),
            "the timeout must poison the handle"
        );

        assert!(
            worker.recover().await.expect("respawn the worker"),
            "a desynchronized handle reports that it actually replaced the child"
        );
        assert!(
            !worker.is_desynchronized(),
            "the replacement must accept calls again"
        );

        let fresh = serde_json::json!({
            "method": "call_tool", "tool": "echo", "args": { "token": "caller-B" },
        });
        let response = worker
            .call_with_timeout(&fresh, Some(std::time::Duration::from_secs(10)))
            .await
            .expect("the replacement child answers");
        assert_eq!(
            response["result"]["token"], "caller-B",
            "the fresh child must answer for caller-B, never replay caller-A"
        );

        assert!(
            !worker.recover().await.expect("healthy recover is a no-op"),
            "a healthy handle must not be needlessly replaced"
        );
        worker.shutdown().await.expect("shutdown worker");
    }

    /// `spawn_with_clean_environment` draws the LocalOnly credential boundary.
    /// Recovery respawns the child, so it must respawn through the SAME path —
    /// otherwise a stalled tool silently converts a clean-environment worker
    /// into an inheriting one, and the parent's credentials appear in a child
    /// that was created precisely to exclude them.
    #[tokio::test]
    async fn recovery_preserves_the_clean_environment_boundary() {
        let Some(python_bin) = python_executable() else {
            eprintln!("SKIP: python3 not on PATH");
            return;
        };
        let project = tempfile::tempdir().expect("temp project");
        let app = project.path().join("app");
        std::fs::create_dir_all(&app).expect("create app package");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        // Reports whether a variable that exists only in the PARENT leaked in.
        std::fs::write(
            app.join("tool_server.py"),
            r#"import json, os, sys, time
first = True
for line in sys.stdin:
    json.loads(line)
    if first:
        first = False
        time.sleep(1.5)
    sys.stdout.write(json.dumps({"result": {"leaked": os.environ.get("PRISM_PARENT_ONLY_SECRET")}}) + "\n")
    sys.stdout.flush()
"#,
        )
        .expect("write worker");

        // SAFETY: single-threaded test setup, before any child is spawned.
        unsafe { std::env::set_var("PRISM_PARENT_ONLY_SECRET", "must-not-leak") };

        let mut env = BTreeMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        );
        let server = ToolServer {
            python_bin,
            project_root: project.path().to_path_buf(),
            env,
        };
        let mut worker = server
            .spawn_with_clean_environment()
            .await
            .expect("spawn clean worker");

        let request = serde_json::json!({ "method": "call_tool", "tool": "env", "args": {} });
        worker
            .call_with_timeout(&request, Some(std::time::Duration::from_millis(100)))
            .await
            .expect_err("the 1.5s response cannot beat a 100ms deadline");
        assert!(worker.is_desynchronized());
        assert!(worker.recover().await.expect("respawn the clean worker"));

        let response = worker
            .call_with_timeout(&request, Some(std::time::Duration::from_secs(10)))
            .await
            .expect("the replacement answers");
        assert_eq!(
            response["result"]["leaked"],
            serde_json::Value::Null,
            "the replacement must still be a cleared-environment child"
        );

        // SAFETY: as above.
        unsafe { std::env::remove_var("PRISM_PARENT_ONLY_SECRET") };
        worker.shutdown().await.expect("shutdown worker");
    }
}

#[cfg(test)]
mod ceiling_tests {
    use super::*;
    use std::time::Duration;

    /// Run 2 of the SX500 research (2026-09-05): an execute_python call the
    /// tool itself would have ended within 60 s never answered, and with no
    /// ceiling the agent waited on the pipe for the rest of the session. A
    /// tool that promises to finish in N seconds is waited for N + a margin;
    /// a tool that promises nothing keeps the operator's rule.
    #[test]
    fn the_tools_own_promise_bounds_the_wait() {
        let explicit =
            ceiling_for_call("execute_python", &serde_json::json!({"timeout": 120}), None);
        assert_eq!(
            explicit,
            Some(Duration::from_secs(120 + PROMISE_MARGIN_SECS))
        );
        let implicit = ceiling_for_call(
            "execute_python",
            &serde_json::json!({"code": "print(1)"}),
            None,
        );
        assert_eq!(
            implicit,
            Some(Duration::from_secs(60 + PROMISE_MARGIN_SECS)),
            "code.py's default is 60 s"
        );
        let bash = ceiling_for_call("execute_bash", &serde_json::json!({"timeout": 30}), None);
        assert_eq!(bash, Some(Duration::from_secs(30 + PROMISE_MARGIN_SECS)));
        assert_eq!(
            ceiling_for_call("qe_run", &serde_json::json!({}), None),
            None,
            "no promise, no ceiling"
        );
        assert_eq!(
            ceiling_for_call(
                "qe_run",
                &serde_json::json!({}),
                Some(Duration::from_secs(900))
            ),
            Some(Duration::from_secs(900)),
            "the operator's ceiling still applies to a tool without a promise"
        );
        assert_eq!(
            ceiling_for_call(
                "execute_python",
                &serde_json::json!({"timeout": 5000}),
                None
            ),
            Some(Duration::from_secs(300 + PROMISE_MARGIN_SECS)),
            "a promise above the tool's own maximum is read as that maximum"
        );
    }
}
