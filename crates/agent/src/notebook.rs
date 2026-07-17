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
/// Hard cap on ONE response line from the sidecar. The sidecar already
/// truncates its own captured output, so this is defense-in-depth against a
/// pathological unbounded line (e.g. a cell writing raw bytes to fd 1) that
/// would otherwise let `read_line` buffer until the backend runs out of
/// memory and takes the whole TUI down with it.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

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
    /// A cell is executing right now. The kernel is checked out of `kernel`
    /// (moved to None) across the await, so a second overlapping caller would
    /// otherwise see None and spawn a rival kernel; this flag makes it error
    /// "kernel busy" instead. Unreachable via today's serialized transports —
    /// cheap latent-race defense.
    busy: bool,
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
                busy: false,
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
            busy: false,
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
        // Own process group: the sidecar becomes a group leader (pgid == its
        // pid), so a hard-timeout kill can reap the WHOLE group — any
        // subprocess a cell spawned dies with it instead of surviving.
        #[cfg(unix)]
        cmd.process_group(0);

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

        // Read the hello line (size- and time-bounded) — proves the
        // interpreter actually ran.
        let hello: Value =
            match timeout(HELLO_TIMEOUT, read_capped_line(&mut stdout, MAX_LINE_BYTES)).await {
                Ok(Ok(LineRead::Line(line))) => serde_json::from_str(line.trim())
                    .context("notebook kernel sent a malformed hello line")?,
                Ok(Ok(LineRead::Eof)) => {
                    bail!(
                        "notebook kernel exited before starting — is `{}` a working \
                     Python 3 interpreter?",
                        python_bin.display()
                    );
                }
                Ok(Ok(LineRead::TooLarge)) => {
                    bail!("notebook kernel emitted an oversized hello line")
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

    /// Send one `execute` request and read back the matching `result` line.
    ///
    /// The response is correlated by `id`: a stray line on the sidecar's stdout
    /// (a C-extension print, `os.write(1, …)`, an inheriting subprocess) is
    /// SKIPPED rather than mis-attributed as this cell's output. Malformed
    /// lines are skipped too. A bounded skip budget guards against an endless
    /// desync (→ the caller kills+restarts).
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

        for _ in 0..64 {
            match read_capped_line(&mut self.stdout, MAX_LINE_BYTES).await? {
                LineRead::Line(text) => {
                    let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
                        continue; // stray non-JSON line — skip and keep looking.
                    };
                    if value.get("id").and_then(Value::as_u64) == Some(id) {
                        return Ok(value);
                    }
                    // A line for a different id (or an event) — skip.
                }
                LineRead::Eof => bail!("notebook kernel closed its output stream"),
                LineRead::TooLarge => bail!(
                    "notebook cell emitted more than {} MiB on a single output line",
                    MAX_LINE_BYTES / (1024 * 1024)
                ),
            }
        }
        bail!("notebook kernel desynchronized — no response for cell {id}")
    }

    async fn shutdown(&mut self) {
        // Capture the group id BEFORE any wait() reaps the child (`id()`
        // returns None afterwards). The sidecar is its own group leader.
        let pgid = self.child.id();
        let _ = self.stdin.write_all(b"{\"op\":\"shutdown\"}\n").await;
        let _ = self.stdin.flush().await;
        // Give it a moment to tear the IPython kernel down cleanly.
        if timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            // Unclean: the leader ignored shutdown and is STILL ALIVE, so its
            // pgid is guaranteed valid and the group non-empty. Kill the whole
            // group BEFORE reaping (race-free, mirrors `reap_kernel`), which
            // also takes any cell-spawned children down with it.
            if let Some(pgid) = pgid {
                kill_process_group(pgid);
            }
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
            return;
        }
        // Clean exit: the leader is gone. Cell-spawned background processes
        // (e.g. `subprocess.Popen(['sleep', …])`) share the sidecar's group
        // and must not survive the shutdown. The pgid stays valid while ANY
        // such child lives, so this killpg reaps them; with none left it is a
        // harmless ESRCH. (A pgid only becomes reusable once every member has
        // exited, and the OS will not recycle it to an unrelated new leader
        // within this un-awaited window, so the theoretical recycle race is
        // sub-ms and benign.)
        if let Some(pgid) = pgid {
            kill_process_group(pgid);
        }
    }
}

/// The outcome of one bounded line read.
enum LineRead {
    Line(String),
    /// Clean end of stream.
    Eof,
    /// A single line exceeded the byte cap; the stream was drained to the next
    /// newline so the reader stays in sync, but the content is discarded.
    TooLarge,
}

/// Read one `\n`-terminated line from `reader`, never buffering more than
/// `cap` bytes. On overflow it keeps consuming (discarding) until the newline
/// so the protocol stays framed, then reports [`LineRead::TooLarge`]. This is
/// what stops a giant cell output from OOM-ing the backend.
async fn read_capped_line(reader: &mut BufReader<ChildStdout>, cap: usize) -> Result<LineRead> {
    let mut out: Vec<u8> = Vec::new();
    let mut overflowed = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if overflowed {
                LineRead::TooLarge
            } else if out.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line(String::from_utf8_lossy(&out).into_owned())
            });
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if !overflowed {
                out.extend_from_slice(&available[..pos]);
            }
            reader.consume(pos + 1);
            return Ok(if overflowed {
                LineRead::TooLarge
            } else {
                LineRead::Line(String::from_utf8_lossy(&out).into_owned())
            });
        }
        let len = available.len();
        if !overflowed {
            out.extend_from_slice(available);
            if out.len() > cap {
                overflowed = true;
                out = Vec::new(); // release the buffer; keep draining to resync.
            }
        }
        reader.consume(len);
    }
}

/// SIGKILL an entire process group by its leader pid (we spawn the sidecar as
/// a group leader, so `pgid == pid`). No-op on non-unix.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: killpg is a simple libc call; an invalid pgid just returns ESRCH.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}
#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

/// How many saved `cell-*.png` files to keep in the state dir. Oldest beyond
/// this are pruned so a long session can't grow the dir without bound.
const MAX_SAVED_IMAGES: usize = 64;

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
    if !paths.is_empty() {
        prune_cell_images(&dir, MAX_SAVED_IMAGES);
    }
    paths
}

/// Best-effort: delete the oldest saved `cell-*.png` files so at most `keep`
/// remain. Only touches the notebook's own image files — nothing else in the
/// state dir (the sidecar script lives there too).
fn prune_cell_images(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("cell-") && name.ends_with(".png")
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|(modified, _)| *modified);
    for (_, path) in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(path);
    }
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
    // cell occupies the loop, same as the other slash handlers. `busy` rejects
    // an overlapping caller instead of letting it spawn a rival kernel.
    let (mut kernel, python_bin, workdir, exec_count) = {
        let mut guard = lock();
        let state = ensure_state(&mut guard)?;
        if state.busy {
            bail!("notebook kernel is busy running another cell — try again once it finishes");
        }
        state.busy = true;
        state.exec_count += 1;
        (
            state.kernel.take(),
            state.python_bin.clone(),
            state.workdir.clone(),
            state.exec_count,
        )
    };
    // Clear `busy` on every exit path (including `?`/panic), so one failure
    // can't wedge the notebook shut.
    let _busy = BusyGuard;

    if kernel.is_none() {
        kernel = Some(Kernel::spawn(&python_bin, &workdir).await?);
    }
    let mut kernel = kernel.expect("kernel spawned above");

    // Hard guard: sidecar timeout + a margin wide enough for the sidecar's
    // own soft-timeout recovery — interrupt + up to 5s drain-to-idle grace +
    // 1s shell-reply reap (see notebook_kernel.py) ≈ timeout + 6s. A tighter
    // margin would let this hard kill fire FIRST, losing the sidecar's honest
    // `kernel_dead` report and any partial output it captured. 15s keeps a
    // real ceiling while letting the soft path finish.
    let hard = Duration::from_secs(timeout_secs + 15);

    // On the success path we get a clean `result`; on a hard timeout or an I/O
    // fault the kernel is suspect, so we kill+restart it and record honestly.
    let failure_message: Option<String> = match timeout(hard, kernel.request(code, timeout_secs))
        .await
    {
        Ok(Ok(value)) => {
            // The sidecar sets kernel_dead when a soft-timeout interrupt left
            // the kernel wedged (not back to idle) — reusing it would be a lie
            // about "variables intact", so treat it as a failed kernel below.
            let kernel_dead = value
                .get("kernel_dead")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let error = format_error(value.get("error").unwrap_or(&Value::Null));
            let image_paths = save_images(exec_count, value.get("images").unwrap_or(&Value::Null));
            let cell = Cell {
                // Always the Rust-monotonic count — the kernel's own count
                // resets to 1 after a restart, which would collide with the
                // earliest pane cells (see apply_cell replace-by-count).
                execution_count: exec_count,
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
            if kernel_dead {
                // Fall through to the failure path to reap the wedged kernel,
                // but keep the cell we already built (it carries the timeout
                // error the sidecar reported).
                reap_kernel(&mut kernel).await;
                let mut guard = lock();
                if let Some(state) = guard.as_mut() {
                    state.kernel = None;
                    state.cells.push(cell.clone());
                }
                return Ok(cell);
            }
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

    // Failure path: reap the suspect kernel (whole process group) and force a
    // fresh spawn next time.
    reap_kernel(&mut kernel).await;
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

/// Kill a suspect kernel and its whole process group, then reap the child.
async fn reap_kernel(kernel: &mut Kernel) {
    if let Some(pid) = kernel.child.id() {
        kill_process_group(pid);
    }
    kernel.child.start_kill().ok();
    let _ = kernel.child.wait().await;
}

/// Clears the shared `busy` flag on drop — panic- and early-return-safe.
struct BusyGuard;
impl Drop for BusyGuard {
    fn drop(&mut self) {
        if let Some(state) = lock().as_mut() {
            state.busy = false;
        }
    }
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
///
/// Refuses while a cell is in flight: the executing call holds the kernel
/// checked out of the shared state, so a reset here would clear the log only
/// for the cell's success path to put the old kernel AND its cell straight
/// back — silently undoing the reset. Bailing is honest; the caller retries
/// once the cell finishes.
pub async fn reset() -> Result<()> {
    let kernel = {
        let mut guard = lock();
        match guard.as_mut() {
            Some(state) => {
                if state.busy {
                    bail!(
                        "notebook kernel is busy running a cell — wait for it to \
                         finish, then reset"
                    );
                }
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
    // The cell log is gone, so its saved plot files are unreachable — remove
    // them (best-effort) instead of letting the state dir grow forever.
    prune_cell_images(&notebook_dir(), 0);
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

    /// Does `python` have `module` importable?
    fn python_has_module(py: &Path, module: &str) -> bool {
        std::process::Command::new(py)
            .args(["-c", &format!("import {module}")])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn forces_builtin_backend_via_env() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        // SAFETY: single-threaded critical section (tests are serialized) and
        // the var is removed before any assertion can unwind.
        unsafe { std::env::set_var("PRISM_NOTEBOOK_FORCE_BUILTIN", "1") };
        configure(py, std::env::temp_dir());

        let cell = execute("21 * 2", Some(30), "user").await;
        let backend = status().backend;
        unsafe { std::env::remove_var("PRISM_NOTEBOOK_FORCE_BUILTIN") };

        let cell = cell.expect("execute should succeed on the stdlib fallback");
        assert_eq!(cell.result.as_deref(), Some("42"));
        assert_eq!(
            backend.as_deref(),
            Some("builtin"),
            "the env var must force the stdlib backend even where Jupyter is present"
        );

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn huge_exception_message_does_not_kill_the_shared_session() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        configure(py, std::env::temp_dir());

        let c1 = execute("keep = 'alive'", Some(30), "user").await.unwrap();
        assert!(c1.success, "error: {:?}", c1.error);

        // ~40 MB exception message — far beyond the 16 MiB line cap if the
        // sidecar emitted the error uncapped. Must come back as a normal
        // failed cell, NOT a TooLarge bail that reaps the kernel.
        let cell = execute("raise ValueError('x' * 40_000_000)", Some(120), "user")
            .await
            .expect("a huge exception is a failed cell, not a dead kernel");
        assert!(!cell.success);
        let err = cell.error.expect("error surfaced");
        assert!(err.contains("ValueError"), "exception name kept: {err}");
        assert!(err.contains("truncated"), "truncation reported: {err}");

        // THE point: the kernel — and the human's variables — survived.
        let c3 = execute("keep", Some(30), "user").await.unwrap();
        assert_eq!(
            c3.result.as_deref(),
            Some("'alive'"),
            "the shared session must survive a large recoverable exception"
        );

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn huge_nonascii_exception_does_not_kill_the_shared_session() {
        // HIGH-1 regression: char caps bound CHARACTERS, but json.dumps
        // escapes each emoji to a 12-byte surrogate pair. A 1M-char evalue
        // (+ its traceback frame) serializes to ~20+ MiB — far past the
        // 16 MiB wire cap — so WITHOUT the `_emit` byte budget the Rust
        // reader would TooLarge-bail and reap the kernel, wiping the session.
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        configure(py, std::env::temp_dir());

        let c1 = execute("keep = 'alive'", Some(30), "user").await.unwrap();
        assert!(c1.success, "error: {:?}", c1.error);

        // 2M emoji -> ~22.9 MiB serialized line if emitted uncapped.
        let cell = execute(
            "raise ValueError('\u{1F600}' * 2_000_000)",
            Some(120),
            "user",
        )
        .await
        .expect("a huge non-ASCII exception is a failed cell, not a dead kernel");
        assert!(!cell.success);
        let err = cell.error.expect("error surfaced");
        assert!(err.contains("ValueError"), "exception name kept: {err}");
        assert!(err.contains("truncated"), "truncation reported: {err}");

        // THE point: the kernel — and the human's variables — survived.
        let c3 = execute("keep", Some(30), "user").await.unwrap();
        assert_eq!(
            c3.result.as_deref(),
            Some("'alive'"),
            "the shared session must survive a large non-ASCII exception"
        );

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn huge_nonascii_output_across_streams_does_not_kill_the_session() {
        // HIGH-1 regression: even split across stdout + stderr + result, a
        // 1M-char-per-stream non-ASCII flood serializes to ~17 MiB (each `中`
        // escapes to `中`, 6 bytes). Per-field char caps can't see the
        // aggregate wire size; the `_emit` byte budget must, returning a
        // normal (truncated) cell rather than reaping the kernel.
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        configure(py, std::env::temp_dir());

        let c1 = execute("survivor = 7", Some(30), "user").await.unwrap();
        assert!(c1.success, "error: {:?}", c1.error);

        let code = "import sys\n\
                    s = '\u{4E2D}' * 1_000_000\n\
                    sys.stdout.write(s)\n\
                    sys.stderr.write(s)\n\
                    s";
        let cell = execute(code, Some(120), "user")
            .await
            .expect("a huge multi-stream output is a completed cell, not a dead kernel");
        assert!(cell.success, "error: {:?}", cell.error);
        assert!(
            cell.stdout.contains("truncated")
                || cell.stderr.contains("truncated")
                || cell.result.as_deref().unwrap_or("").contains("truncated"),
            "byte truncation must be reported somewhere"
        );

        // The kernel was NOT reaped — the earlier variable still resolves.
        let c3 = execute("survivor", Some(30), "user").await.unwrap();
        assert_eq!(
            c3.result.as_deref(),
            Some("7"),
            "the shared session must survive a large multi-stream output"
        );

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn print_flood_is_capped_at_append_time() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        // Builtin backend: exercises the capped stdout writer deterministically.
        unsafe { std::env::set_var("PRISM_NOTEBOOK_FORCE_BUILTIN", "1") };
        configure(py, std::env::temp_dir());

        let cell = execute(
            "for _ in range(8):\n    print('y' * 1_000_000)",
            Some(60),
            "user",
        )
        .await;
        unsafe { std::env::remove_var("PRISM_NOTEBOOK_FORCE_BUILTIN") };

        let cell = cell.expect("flood cell should execute");
        assert!(cell.success, "error: {:?}", cell.error);
        assert!(
            cell.stdout.len() <= 1_100_000,
            "stdout must stay near the 1 MB cap, got {} bytes",
            cell.stdout.len()
        );
        assert!(
            cell.stdout.contains("truncated"),
            "the cap must be reported, not silent"
        );

        reset().await.unwrap();
        reset_global();
    }

    #[tokio::test]
    async fn reset_refuses_while_a_cell_is_in_flight() {
        let _serial = test_serial().lock().await;
        reset_global();
        configure(PathBuf::from("python3"), std::env::temp_dir());
        {
            let mut guard = lock();
            let state = ensure_state(&mut guard).unwrap();
            state.busy = true;
            state.exec_count = 1;
            state.cells.push(Cell {
                execution_count: 1,
                origin: "user".to_string(),
                code: "x = 1".to_string(),
                stdout: String::new(),
                stderr: String::new(),
                result: None,
                image_paths: Vec::new(),
                error: None,
                success: true,
            });
        }

        let err = reset().await.expect_err("reset must bail while busy");
        assert!(format!("{err:#}").contains("busy"), "got: {err:#}");
        assert_eq!(cells().len(), 1, "a refused reset must not clear the log");

        if let Some(state) = lock().as_mut() {
            state.busy = false;
        }
        reset().await.expect("reset succeeds once idle");
        assert_eq!(cells().len(), 0);
        reset_global();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn clean_shutdown_kills_cell_spawned_children() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        reset_global();
        // Builtin backend: the cell's child lives in the sidecar's process
        // group, which the shutdown group-kill must reap.
        unsafe { std::env::set_var("PRISM_NOTEBOOK_FORCE_BUILTIN", "1") };
        configure(py, std::env::temp_dir());

        let cell = execute(
            "import subprocess\np = subprocess.Popen(['sleep', '300'])\np.pid",
            Some(30),
            "user",
        )
        .await;
        unsafe { std::env::remove_var("PRISM_NOTEBOOK_FORCE_BUILTIN") };
        let cell = cell.expect("spawn cell should execute");
        assert!(cell.success, "error: {:?}", cell.error);
        let pid: i32 = cell
            .result
            .as_deref()
            .expect("pid echoed")
            .parse()
            .expect("pid parses");
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the sleep child should be alive before shutdown"
        );

        // Clean shutdown: the sidecar exits 0 — the leaked child must die too.
        reset().await.unwrap();
        let mut alive = true;
        for _ in 0..30 {
            alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !alive,
            "a cell-spawned background process must not survive a clean \
             shutdown/reset (pid {pid} still alive)"
        );
        reset_global();
    }

    #[tokio::test]
    async fn captures_matplotlib_image_on_builtin_backend() {
        let _serial = test_serial().lock().await;
        let Some(py) = test_python() else {
            return;
        };
        if !python_has_module(&py, "matplotlib") {
            eprintln!("skipping: matplotlib not installed");
            return;
        }
        reset_global();
        // Force the builtin backend so the figure harvester (not the Jupyter
        // inline path) is exercised deterministically.
        unsafe { std::env::set_var("PRISM_NOTEBOOK_FORCE_BUILTIN", "1") };
        configure(py, std::env::temp_dir());

        let cell = execute(
            "import matplotlib.pyplot as plt\nplt.plot([1, 2, 3], [1, 4, 9])\n1",
            Some(60),
            "agent",
        )
        .await;
        unsafe { std::env::remove_var("PRISM_NOTEBOOK_FORCE_BUILTIN") };

        let cell = cell.expect("execute should succeed");
        assert!(cell.success, "error: {:?}", cell.error);
        assert_eq!(cell.image_paths.len(), 1, "one figure should be captured");
        let bytes = std::fs::read(&cell.image_paths[0]).expect("saved PNG exists");
        assert!(!bytes.is_empty(), "the saved plot must have bytes");
        // PNG magic number.
        assert_eq!(&bytes[..4], b"\x89PNG", "the saved file must be a PNG");

        reset().await.unwrap();
        reset_global();
    }
}
