//! Backend handle — abstracts the agent backend connection.
//!
//! Two variants:
//! - [`BackendHandle::Real`] — spawns `prism backend` as a subprocess
//!   and communicates via JSON-RPC over stdio pipes.  This is the
//!   production path.
//! - [`BackendHandle::Fake`] — replays deterministic JSON-RPC
//!   notifications from an in-memory event queue.  No subprocess, no
//!   network, no LLM.  Used for testing, PTY verification, and
//!   snapshot tests.
//!
//! Both variants expose the same public API (`send_message`,
//! `send_command`, `send_approval`, `recv`, `init`, `kill`) so
//! [`crate::app::App`] doesn't need to know which backend it's driving.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use tokio::sync::mpsc;

// ── Fake scenario ───────────────────────────────────────────────────

/// Deterministic fake-backend scenario.  Each variant corresponds to a
/// fixed sequence of JSON-RPC notifications that the fake backend
/// replays when the user interacts with the TUI.
///
/// Only `BasicChat` is implemented in Patch 3A.  More scenarios
/// (approval, tool_error, streaming, stress) will be added in later
/// patches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeScenario {
    /// Minimal chat: welcome → status → user message → fake response.
    BasicChat,
}

impl FakeScenario {
    /// Parse a scenario name from a CLI flag value.
    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "basic_chat" => Ok(Self::BasicChat),
            other => bail!(
                "unknown fake backend scenario: '{other}'. \
                 Available scenarios: basic_chat"
            ),
        }
    }

    /// The snake_case name used in CLI flags.
    pub fn as_name(&self) -> &'static str {
        match self {
            Self::BasicChat => "basic_chat",
        }
    }
}

// ── Backend enum ────────────────────────────────────────────────────

/// The backend connection — either a real subprocess or a fake
/// deterministic event player.
///
/// `App` holds this and calls `send_message` / `recv` / etc. without
/// caring which variant is active.
pub enum BackendHandle {
    Real(RealBackend),
    Fake(FakeBackend),
}

// ── Real backend ────────────────────────────────────────────────────

/// Real subprocess backend — spawns `prism backend` and talks
/// JSON-RPC over stdio pipes.
pub struct RealBackend {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
}

impl RealBackend {
    pub fn spawn(prism_binary: &str, project_root: &str, python_bin: &str) -> Result<Self> {
        let mut child = Command::new(prism_binary)
            .arg("--python")
            .arg(python_bin)
            .arg("backend")
            .arg("--project-root")
            .arg(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn prism backend")?;

        let stdin = child.stdin.take().context("no stdin on backend")?;
        let stdout = child.stdout.take().context("no stdout on backend")?;

        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) if !l.trim().is_empty() => {
                        if let Ok(v) = serde_json::from_str::<Value>(&l)
                            && tx.send(v).is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                    Ok(_) => continue,
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
        })
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id();
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id,
            "params": params,
        });
        let line = serde_json::to_string(&req)? + "\n";
        self.stdin
            .write_all(line.as_bytes())
            .context("failed to write to backend stdin")?;
        self.stdin.flush()?;
        Ok(id)
    }

    pub async fn init(&mut self) -> Result<()> {
        self.send_request(
            "init",
            serde_json::json!({"auto_approve": false, "resume": ""}),
        )?;
        // Wait for the init response (first message with a "result" field)
        if let Some(resp) = self.rx.recv().await
            && (resp.get("result").is_some() || resp.get("method").is_some())
        {
            // Could be the response or a welcome notification — both are fine
            // If it's a notification, process it as a welcome
            if resp.get("method").and_then(|m| m.as_str()) == Some("ui.welcome") {
                // Re-send to the channel for the app to process
                // Actually we should just let the app handle it — but since we
                // consumed it, we need to handle it. Let's just return Ok.
                return Ok(());
            }
            return Ok(());
        }
        anyhow::bail!("init failed — no response from backend")
    }

    pub fn send_message(&mut self, text: &str) -> Result<u64> {
        self.send_request("input.message", serde_json::json!({"text": text}))
    }

    pub fn send_command(&mut self, command: &str) -> Result<u64> {
        self.send_request(
            "input.command",
            serde_json::json!({"command": command, "silent": false}),
        )
    }

    pub fn send_approval(&mut self, response: &str) -> Result<()> {
        self.send_request(
            "approval.respond",
            serde_json::json!({"response": response}),
        )?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Option<Value> {
        self.rx.recv().await
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RealBackend {
    fn drop(&mut self) {
        self.kill();
    }
}

// ── Fake backend ────────────────────────────────────────────────────

/// Fake backend — replays deterministic JSON-RPC notifications from
/// an in-memory queue.  No subprocess, no network, no LLM.
///
/// On construction, it enqueues a startup sequence (welcome + status).
/// When `send_message` is called, it enqueues a fake response
/// sequence (text deltas + flush + turn complete).  `recv` drains the
/// queue just like the real backend drains its stdout pipe.
pub struct FakeBackend {
    /// The JSON-RPC notification queue.  Each entry is a complete
    /// JSON-RPC notification object (with `method` and `params`).
    tx: mpsc::UnboundedSender<Value>,
    rx: mpsc::UnboundedReceiver<Value>,
    next_id: u64,
    scenario: FakeScenario,
}

impl FakeBackend {
    /// Create a fake backend with the given scenario.  Immediately
    /// enqueues the startup notifications so `recv` will produce
    /// welcome + status on the first calls.
    pub fn new(scenario: FakeScenario) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Self {
            tx,
            rx,
            next_id: 1,
            scenario,
        };
        backend.enqueue_startup();
        backend
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Enqueue a JSON-RPC notification onto the fake event queue.
    fn notify(&self, method: &str, params: Value) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let _ = self.tx.send(msg);
    }

    /// Enqueue the startup sequence: welcome + status.
    fn enqueue_startup(&self) {
        match self.scenario {
            FakeScenario::BasicChat => {
                self.notify(
                    "ui.welcome",
                    serde_json::json!({
                        "version": "2.7.1-fake",
                        "tool_count": 99,
                    }),
                );
                self.notify(
                    "ui.status",
                    serde_json::json!({
                        "model": "fake-backend",
                        "session_mode": "chat",
                        "message_count": 0,
                    }),
                );
            }
        }
    }

    /// Enqueue the response sequence for a user message.
    fn enqueue_response(&self, _user_text: &str) {
        match self.scenario {
            FakeScenario::BasicChat => {
                // Send a deterministic fake response in a few deltas
                // so the TUI's streaming path is exercised.
                let response = "Fake backend response: PRISM TUI is running \
                    in deterministic test mode.";
                // Split into a few chunks to simulate streaming.
                let words: Vec<&str> = response.split_whitespace().collect();
                for chunk in words.chunks(3) {
                    let text = chunk.join(" ") + " ";
                    self.notify("ui.text.delta", serde_json::json!({"text": text}));
                }
                self.notify("ui.text.flush", serde_json::json!({}));
                self.notify(
                    "ui.cost",
                    serde_json::json!({
                        "turn_cost": 0.0,
                        "session_cost": 0.0,
                        "input_tokens": 10,
                        "output_tokens": 20,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
        }
    }

    /// Enqueue a simple response for a slash command.
    fn enqueue_command_response(&self, command: &str) {
        self.notify(
            "ui.status",
            serde_json::json!({
                "model": "fake-backend",
                "session_mode": "chat",
                "message_count": 1,
            }),
        );
        // For /tools, emit a minimal tool list.
        if command.starts_with("/tools") {
            self.notify(
                "ui.view",
                serde_json::json!({
                    "title": "Tools",
                    "tabs": [{
                        "title": "Available",
                        "body": "alloy_sample, gfn_evaluate, materials_search (fake backend)"
                    }],
                }),
            );
        }
        self.notify("ui.turn.complete", serde_json::json!({}));
    }

    pub async fn init(&mut self) -> Result<()> {
        // Fake backend is already "initialized" — the startup events
        // are in the queue.  Return Ok immediately.
        Ok(())
    }

    pub fn send_message(&mut self, text: &str) -> Result<u64> {
        let id = self.next_id();
        self.enqueue_response(text);
        Ok(id)
    }

    pub fn send_command(&mut self, command: &str) -> Result<u64> {
        let id = self.next_id();
        self.enqueue_command_response(command);
        Ok(id)
    }

    pub fn send_approval(&mut self, _response: &str) -> Result<()> {
        // No-op for fake backend — the approval state is handled by
        // the TUI locally.  A future approval scenario can enqueue
        // a tool result here.
        Ok(())
    }

    pub async fn recv(&mut self) -> Option<Value> {
        self.rx.recv().await
    }

    pub fn kill(&mut self) {
        // Nothing to kill — no subprocess.
    }
}

// ── BackendHandle enum dispatch ─────────────────────────────────────

impl BackendHandle {
    /// Spawn the real backend subprocess.
    pub fn spawn(prism_binary: &str, project_root: &str, python_bin: &str) -> Result<Self> {
        Ok(Self::Real(RealBackend::spawn(
            prism_binary,
            project_root,
            python_bin,
        )?))
    }

    /// Create a fake backend with the given scenario.  Does NOT spawn
    /// a subprocess.
    pub fn fake(scenario: FakeScenario) -> Self {
        Self::Fake(FakeBackend::new(scenario))
    }

    /// Test-only constructor: assemble a real backend from pre-spawned
    /// parts.  Kept for backward compatibility with existing tests.
    #[doc(hidden)]
    pub fn from_parts(
        child: Child,
        stdin: std::process::ChildStdin,
        rx: mpsc::UnboundedReceiver<Value>,
        next_id: u64,
    ) -> Self {
        Self::Real(RealBackend {
            child,
            stdin,
            rx,
            next_id,
        })
    }

    pub async fn init(&mut self) -> Result<()> {
        match self {
            Self::Real(b) => b.init().await,
            Self::Fake(b) => b.init().await,
        }
    }

    pub fn send_message(&mut self, text: &str) -> Result<u64> {
        match self {
            Self::Real(b) => b.send_message(text),
            Self::Fake(b) => b.send_message(text),
        }
    }

    pub fn send_command(&mut self, command: &str) -> Result<u64> {
        match self {
            Self::Real(b) => b.send_command(command),
            Self::Fake(b) => b.send_command(command),
        }
    }

    pub fn send_approval(&mut self, response: &str) -> Result<()> {
        match self {
            Self::Real(b) => b.send_approval(response),
            Self::Fake(b) => b.send_approval(response),
        }
    }

    pub async fn recv(&mut self) -> Option<Value> {
        match self {
            Self::Real(b) => b.recv().await,
            Self::Fake(b) => b.recv().await,
        }
    }

    pub fn kill(&mut self) {
        match self {
            Self::Real(b) => b.kill(),
            Self::Fake(b) => b.kill(),
        }
    }
}

impl Drop for BackendHandle {
    fn drop(&mut self) {
        self.kill();
    }
}
