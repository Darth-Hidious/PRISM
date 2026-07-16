// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! In-app Python notebook kernel — supervised sidecar + shared cell log.
//!
//! PRISM used to expose `notebook` only as a shell-out that launched detached
//! Jupyter Lab servers (`crates/cli/src/notebook.rs`) — a human had to leave
//! the TUI to a browser and the agent could not run code at all. This module
//! replaces that with a real, in-process kernel that BOTH front-ends share:
//!
//! * the human's TUI notebook pane (`/notebook run …`), and
//! * the agent's `notebook_exec` tool.
//!
//! Because `prism backend` (the stdio server the TUI spawns) and the agent
//! loop run in the SAME process, one global kernel + one shared cell log
//! serve both with zero synchronization work: a cell the agent runs shows up
//! in the human's pane and vice-versa, and variables persist across both.
//!
//! ## Design (mirrors the existing sidecar + node-supervisor patterns)
//!
//! A small Python sidecar (`notebook_kernel.py`, embedded via `include_str!`
//! and written to the state dir at spawn) speaks one-JSON-object-per-line over
//! stdio — the exact shape of `python-bridge`'s tool server. Rust owns its
//! lifecycle (spawn / health / restart / shutdown), mirroring
//! [`crate::node_supervisor`]. The sidecar prefers a real IPython kernel
//! (`jupyter_client` + `ipykernel`) and transparently falls back to a
//! stdlib-only `exec` kernel, so notebooks work with zero setup.
//!
//! ## Remote seam (NOT built in v1 — documented on purpose)
//!
//! Everything above [`Kernel::request`] is transport-agnostic. A future
//! kernel hosted on procured PRISM compute would implement the same
//! request/response contract over the node channel instead of local stdio;
//! nothing in the tool layer or the TUI would change. That remote backend is
//! intentionally out of scope for v1 (local-first, naive-then-optimize).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

/// The sidecar source, embedded so it ships inside the PRISM binary and needs
/// no separate install step. Written to the state dir verbatim at spawn.
const KERNEL_SIDECAR: &str = include_str!("notebook_kernel.py");

/// Default per-cell wall-clock limit, and the hard ceiling a caller may set.
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
/// How long to wait for the sidecar's `hello` line before declaring it dead.
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// One executed cell — the shared record both the agent tool and the TUI
/// pane read. Serialized straight onto the `ui.notebook.*` wire and into the
/// `notebook_exec` tool result.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    pub execution_count: u64,
    /// Who ran it: `"user"` (TUI pane) or `"agent"` (tool call).
    pub origin: String,
    pub code: String,
    pub stdout: String,
    pub stderr: String,
    /// The last expression's `repr`, notebook-style, when the cell ended in a
    /// bare expression; `None` otherwise.
    pub result: Option<String>,
    /// Saved PNG paths for any plots/rich images the cell produced.
    pub image_paths: Vec<String>,
    /// `ename` / `evalue` / `traceback` joined for display; `None` on success.
    pub error: Option<String>,
    pub success: bool,
}

/// A live sidecar handle.
struct Kernel {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    backend: String,
    python: String,
    detail: String,
    next_id: u64,
}

/// Process-wide notebook state: at most one kernel plus the shared cell log.
struct NotebookState {
    kernel: Option<Kernel>,
    python_bin: PathBuf,
    workdir: PathBuf,
    cells: Vec<Cell>,
    exec_count: u64,
}

static STATE: Mutex<Option<NotebookState>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<NotebookState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read-only kernel status for the pane header / `notebook_status` tool.
#[derive(Debug, Clone, Serialize)]
pub struct KernelStatus {
    pub running: bool,
    pub backend: Option<String>,
    pub python: Option<String>,
    pub detail: Option<String>,
    pub cell_count: usize,
}

/// Directory the sidecar script and cell images live in.
fn notebook_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".prism/state/notebook")
}

/// Configure the interpreter + working directory a freshly spawned kernel
/// uses. Idempotent; changing them takes effect on the next (re)spawn.
pub fn configure(python_bin: PathBuf, workdir: PathBuf) {
    let mut guard = lock();
    match guard.as_mut() {
        Some(state) => {
            state.python_bin = python_bin;
            state.workdir = workdir;
        }
        None => {
            *guard = Some(NotebookState {
                kernel: None,
                python_bin,
                workdir,
                cells: Vec::new(),
                exec_count: 0,
            });
        }
    }
}

fn ensure_state(guard: &mut Option<NotebookState>) -> Result<&mut NotebookState> {
    if guard.is_none() {
        // No explicit configure() yet — fall back to a sane default so the
        // kernel still works (PRISM venv python, current dir).
        let python_bin = default_python_bin();
        let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        *guard = Some(NotebookState {
            kernel: None,
            python_bin,
            workdir,
            cells: Vec::new(),
            exec_count: 0,
        });
    }
    Ok(guard.as_mut().expect("state initialized above"))
}

/// The PRISM-managed venv interpreter, matching where the tool server runs.
fn default_python_bin() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".prism/venv/bin/python3"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// Write the embedded sidecar to disk (only if missing/changed) and return
/// its path. Kept next to the cell images under the state dir.
fn materialize_sidecar(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join("notebook_kernel.py");
    let needs_write = std::fs::read_to_string(&path)
        .map(|existing| existing != KERNEL_SIDECAR)
        .unwrap_or(true);
    if needs_write {
        let mut file = std::fs::File::create(&path)
            .with_context(|| format!("failed to write {}", path.display()))?;
        file.write_all(KERNEL_SIDECAR.as_bytes())?;
    }
    Ok(path)
}

impl Kernel {
    /// Spawn the sidecar and read its `hello` handshake.
    async fn spawn(python_bin: &Path, workdir: &Path) -> Result<Self> {
        let dir = notebook_dir();
        let script = materialize_sidecar(&dir)?;

        let mut cmd = Command::new(python_bin);
        cmd.arg(&script)
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "Python interpreter not found at {}.\n\
                     PRISM needs Python 3 to run notebooks. Install Python 3.11+ \
                     (macOS: `brew install python`, Debian/Ubuntu: `sudo apt-get \
                     install -y python3`), then reopen the notebook. For rich \
                     outputs also run `{} -m pip install jupyter_client ipykernel`.",
                    python_bin.display(),
                    python_bin.display()
                )
            } else {
                anyhow::anyhow!("failed to spawn notebook kernel: {e}")
            }
        })?;

        let stdin = child.stdin.take().context("kernel stdin was piped")?;
        let stdout = child.stdout.take().context("kernel stdout was piped")?;
        let mut stdout = BufReader::new(stdout);

        // Read the hello line (bounded) — proves the interpreter actually ran.
        let mut hello_line = String::new();
        let read = timeout(HELLO_TIMEOUT, stdout.read_line(&mut hello_line)).await;
        let hello: Value = match read {
            Ok(Ok(n)) if n > 0 => serde_json::from_str(hello_line.trim())
                .context("notebook kernel sent a malformed hello line")?,
            Ok(Ok(_)) => {
                bail!(
                    "notebook kernel exited before starting — is `{}` a working \
                     Python 3 interpreter?",
                    python_bin.display()
                );
            }
            Ok(Err(e)) => bail!("failed to read from notebook kernel: {e}"),
            Err(_) => bail!(
                "notebook kernel did not start within {}s",
                HELLO_TIMEOUT.as_secs()
            ),
        };

        Ok(Self {
            child,
            stdin,
            stdout,
            backend: hello
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            python: hello
                .get("python")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            detail: hello
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            next_id: 1,
        })
    }

    /// Send one `execute` request and read its single `result` line.
    async fn request(&mut self, code: &str, timeout_secs: u64) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_string(&serde_json::json!({
            "op": "execute",
            "id": id,
            "code": code,
            "timeout": timeout_secs,
        }))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut response = String::new();
        let n = self.stdout.read_line(&mut response).await?;
        if n == 0 {
            bail!("notebook kernel closed its output stream");
        }
        serde_json::from_str(response.trim()).context("kernel sent malformed result JSON")
    }

    async fn shutdown(&mut self) {
        let _ = self.stdin.write_all(b"{\"op\":\"shutdown\"}\n").await;
        let _ = self.stdin.flush().await;
        // Give it a moment to tear the IPython kernel down cleanly, then reap.
        let _ = timeout(Duration::from_secs(5), self.child.wait()).await;
        let _ = self.child.start_kill();
    }
}

/// Persist any base64 images a cell produced to PNG files under the state dir,
/// returning their paths. Keeps huge blobs out of the agent's context: the
/// tool result carries paths, not bytes, unless base64 is explicitly asked
/// for by the caller.
fn save_images(exec_count: u64, images: &Value) -> Vec<String> {
    let Some(array) = images.as_array() else {
        return Vec::new();
    };
    let dir = notebook_dir();
    let mut paths = Vec::new();
    for (index, image) in array.iter().enumerate() {
        let Some(b64) = image.get("b64").and_then(Value::as_str) else {
            continue;
        };
        use base64::Engine as _;
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            continue;
        };
        let path = dir.join(format!("cell-{exec_count}-{index}.png"));
        if std::fs::write(&path, &bytes).is_ok() {
            paths.push(path.to_string_lossy().to_string());
        }
    }
    paths
}

/// Join a kernel `error` object into a single displayable string.
fn format_error(error: &Value) -> Option<String> {
    if error.is_null() {
        return None;
    }
    let ename = error
        .get("ename")
        .and_then(Value::as_str)
        .unwrap_or("Error");
    let evalue = error.get("evalue").and_then(Value::as_str).unwrap_or("");
    let traceback = error
        .get("traceback")
        .and_then(Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|t| !t.trim().is_empty());
    Some(traceback.unwrap_or_else(|| format!("{ename}: {evalue}")))
}

/// Execute `code`, (re)starting the kernel if needed, and append the result
/// to the shared cell log. `origin` is `"user"` or `"agent"`.
///
/// Timeout handling: the sidecar gets `timeout_secs` as a soft limit (the
/// jupyter backend interrupts and returns a clean error); Rust adds a hard
/// wall-clock guard that kills+restarts the sidecar if it blocks entirely,
/// reporting honestly that in-kernel variables were lost.
pub async fn execute(code: &str, timeout_secs: Option<u64>, origin: &str) -> Result<Cell> {
    let timeout_secs = timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);

    // Take the kernel out of the shared state for the duration of the call so
    // the lock isn't held across await points. One cell at a time — a running
    // cell occupies the loop, same as the other slash handlers.
    let (mut kernel, python_bin, workdir, exec_count) = {
        let mut guard = lock();
        let state = ensure_state(&mut guard)?;
        state.exec_count += 1;
        (
            state.kernel.take(),
            state.python_bin.clone(),
            state.workdir.clone(),
            state.exec_count,
        )
    };

    if kernel.is_none() {
        kernel = Some(Kernel::spawn(&python_bin, &workdir).await?);
    }
    let mut kernel = kernel.expect("kernel spawned above");

    // Hard guard: sidecar timeout + a small margin for the round-trip.
    let hard = Duration::from_secs(timeout_secs + 5);

    // On the success path we get a clean `result`; on a hard timeout or an I/O
    // fault the kernel is suspect, so we kill+restart it and record honestly.
    let failure_message: Option<String> = match timeout(hard, kernel.request(code, timeout_secs))
        .await
    {
        Ok(Ok(value)) => {
            let error = format_error(value.get("error").unwrap_or(&Value::Null));
            let image_paths = save_images(exec_count, value.get("images").unwrap_or(&Value::Null));
            let cell = Cell {
                execution_count: value
                    .get("execution_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(exec_count),
                origin: origin.to_string(),
                code: code.to_string(),
                stdout: value
                    .get("stdout")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                stderr: value
                    .get("stderr")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                result: value
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                image_paths,
                success: error.is_none(),
                error,
            };
            // Healthy kernel — put it back for the next cell.
            let mut guard = lock();
            if let Some(state) = guard.as_mut() {
                state.kernel = Some(kernel);
                state.cells.push(cell.clone());
            }
            return Ok(cell);
        }
        Ok(Err(e)) => Some(format!("notebook kernel error: {e}")),
        Err(_) => Some(format!(
            "cell exceeded {timeout_secs}s — the kernel was restarted, so variables \
             from this session were lost. Re-run setup cells, or raise the timeout."
        )),
    };

    // Failure path: reap the suspect kernel and force a fresh spawn next time.
    kernel.child.start_kill().ok();
    let _ = kernel.child.wait().await;
    let cell = Cell {
        execution_count: exec_count,
        origin: origin.to_string(),
        code: code.to_string(),
        stdout: String::new(),
        stderr: String::new(),
        result: None,
        image_paths: Vec::new(),
        error: failure_message,
        success: false,
    };
    let mut guard = lock();
    if let Some(state) = guard.as_mut() {
        state.kernel = None;
        state.cells.push(cell.clone());
    }
    Ok(cell)
}

/// Current kernel status (does not spawn a kernel).
pub fn status() -> KernelStatus {
    let guard = lock();
    match guard.as_ref() {
        Some(state) => match &state.kernel {
            Some(k) => KernelStatus {
                running: true,
                backend: Some(k.backend.clone()),
                python: Some(k.python.clone()),
                detail: Some(k.detail.clone()),
                cell_count: state.cells.len(),
            },
            None => KernelStatus {
                running: false,
                backend: None,
                python: None,
                detail: None,
                cell_count: state.cells.len(),
            },
        },
        None => KernelStatus {
            running: false,
            backend: None,
            python: None,
            detail: None,
            cell_count: 0,
        },
    }
}

/// Snapshot of the shared cell log (for the pane's initial render).
pub fn cells() -> Vec<Cell> {
    lock().as_ref().map(|s| s.cells.clone()).unwrap_or_default()
}

/// Shut the kernel down and clear all cell state — a fresh notebook.
pub async fn reset() -> Result<()> {
    let kernel = {
        let mut guard = lock();
        match guard.as_mut() {
            Some(state) => {
                state.cells.clear();
                state.exec_count = 0;
                state.kernel.take()
            }
            None => None,
        }
    };
    if let Some(mut kernel) = kernel {
        kernel.shutdown().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// These tests drive the process-global kernel singleton, so they must not
    /// run concurrently. This async mutex serializes them regardless of the
    /// harness thread count (an await-safe alternative to `--test-threads=1`).
    fn test_serial() -> &'static tokio::sync::Mutex<()> {
        static GUARD: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// A Python 3 interpreter for tests, or `None` to skip (CI without python).
    fn test_python() -> Option<PathBuf> {
        for cand in ["python3", "python"] {
            if std::process::Command::new(cand)
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return Some(PathBuf::from(cand));
            }
        }
        None
    }

    fn reset_global() {
        *lock() = None;
    }

    #[tokio::test]
    async fn execute_returns_stdout_and_last_expr_result() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            eprintln!("skipping: no python3 on PATH");
            return;
        };
        reset_global();
        configure(py, std::env::temp_dir());

        let cell = execute("print('hello'); 40 + 2", Some(30), "user")
            .await
            .expect("execute should succeed");
        assert!(cell.success, "error: {:?}", cell.error);
        assert!(cell.stdout.contains("hello"));
        assert_eq!(cell.result.as_deref(), Some("42"));

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn state_persists_across_cells() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        configure(py, std::env::temp_dir());

        let c1 = execute("x = 10", Some(30), "user").await.unwrap();
        assert!(c1.success);
        let c2 = execute("x * 5", Some(30), "agent").await.unwrap();
        assert_eq!(c2.result.as_deref(), Some("50"));
        assert_eq!(c2.origin, "agent");

        // Shared log accumulates both cells with monotonic counts.
        assert_eq!(cells().len(), 2);
        assert!(status().cell_count >= 2);

        reset().await.unwrap();
        assert_eq!(cells().len(), 0, "reset clears the shared log");
        reset_global();
    }

    #[tokio::test]
    async fn exception_is_reported_not_panicked() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        configure(py, std::env::temp_dir());

        let cell = execute("1 / 0", Some(30), "user").await.unwrap();
        assert!(!cell.success);
        let err = cell.error.expect("error present");
        assert!(err.contains("ZeroDivisionError"), "got: {err}");

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn missing_python_gives_actionable_error() {
        let _serial = test_serial().lock().await;
        reset_global();
        configure(
            PathBuf::from("/nonexistent/prism-no-such-python"),
            std::env::temp_dir(),
        );
        let result = execute("1 + 1", Some(10), "user").await;
        let err = result.expect_err("spawn must fail when python is missing");
        let msg = format!("{err:#}");
        assert!(msg.contains("Python"), "message should name Python: {msg}");
        assert!(
            msg.contains("Install") || msg.contains("not found"),
            "message should be actionable: {msg}"
        );
        reset_global();
    }
}
