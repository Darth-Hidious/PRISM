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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeScenario {
    /// Minimal chat: welcome → status → user message → fake response.
    BasicChat,
    /// Streaming: many small text deltas to exercise streaming path.
    StreamingAnswer,
    /// Thinking + answer: thinking deltas then text deltas.
    ThinkingStream,
    /// Tool call success: tool start → tool card result.
    ToolSuccess,
    /// Tool call error: tool start → tool card error.
    ToolError,
    /// Approval required: prompt with rich fields, y/n/a responses.
    ApprovalRequired,
    /// Cost + metrics: token counts and zero cost.
    CostMetrics,
    /// Backend warning + structured error.
    BackendWarningError,
    /// ANSI injection: payloads with unsafe control sequences.
    AnsiInjection,
}

impl FakeScenario {
    /// Parse a scenario name from a CLI flag value.
    pub fn from_name(name: &str) -> Result<Self> {
        match name {
            "basic_chat" => Ok(Self::BasicChat),
            "streaming_answer" => Ok(Self::StreamingAnswer),
            "thinking_stream" => Ok(Self::ThinkingStream),
            "tool_success" => Ok(Self::ToolSuccess),
            "tool_error" => Ok(Self::ToolError),
            "approval_required" => Ok(Self::ApprovalRequired),
            "cost_metrics" => Ok(Self::CostMetrics),
            "backend_warning_error" => Ok(Self::BackendWarningError),
            "ansi_injection" => Ok(Self::AnsiInjection),
            other => bail!(
                "unknown fake backend scenario: '{other}'. \
                 Available scenarios: {}",
                Self::all_names().join(", ")
            ),
        }
    }

    /// The snake_case name used in CLI flags.
    pub fn as_name(&self) -> &'static str {
        match self {
            Self::BasicChat => "basic_chat",
            Self::StreamingAnswer => "streaming_answer",
            Self::ThinkingStream => "thinking_stream",
            Self::ToolSuccess => "tool_success",
            Self::ToolError => "tool_error",
            Self::ApprovalRequired => "approval_required",
            Self::CostMetrics => "cost_metrics",
            Self::BackendWarningError => "backend_warning_error",
            Self::AnsiInjection => "ansi_injection",
        }
    }

    /// All scenario names, for error messages and CLI help.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "basic_chat",
            "streaming_answer",
            "thinking_stream",
            "tool_success",
            "tool_error",
            "approval_required",
            "cost_metrics",
            "backend_warning_error",
            "ansi_injection",
        ]
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
    /// `~/.prism/logs/backend.log` as a Stdio sink (truncated per session so
    /// it can't grow unbounded).
    fn stderr_log_sink() -> Option<Stdio> {
        let home = std::env::var_os("HOME")?;
        let dir = std::path::PathBuf::from(home).join(".prism").join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let file = std::fs::File::create(dir.join("backend.log")).ok()?;
        Some(Stdio::from(file))
    }

    pub fn spawn(prism_binary: &str, project_root: &str, python_bin: &str) -> Result<Self> {
        // Backend stderr (tracing, panics) goes to ~/.prism/logs/backend.log
        // — it used to be Stdio::null(), which made every backend failure
        // invisible. Truncated per session so the file can't grow unbounded;
        // falls back to null only if the log dir can't be created.
        let stderr_sink = Self::stderr_log_sink().unwrap_or(Stdio::null());
        // Default RUST_LOG=info for the backend so backend.log actually has
        // content — an unset filter logs nothing. Explicit RUST_LOG wins.
        let rust_log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
        let mut child = Command::new(prism_binary)
            .env("RUST_LOG", rust_log)
            .arg("--python")
            .arg(python_bin)
            .arg("backend")
            .arg("--project-root")
            .arg(project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_sink)
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

    pub fn send_approval(&mut self, response: &str, tool_name: &str) -> Result<()> {
        // The backend routes prompt/approval responses through
        // `input.prompt_response`; `tool_name` lets an "allow all" (a)
        // persist as a session-level permission override.
        self.send_request(
            "input.prompt_response",
            serde_json::json!({"response": response, "tool_name": tool_name}),
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
    /// All scenarios emit the same startup — the difference is in the
    /// response to user messages and approvals.
    fn enqueue_startup(&self) {
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
        self.notify(
            "ui.tools.catalog",
            serde_json::json!({"tools": [
                {"name":"sample_material","description":"sample an alloy composition","approval":false},
                {"name":"materials_search","description":"search the materials catalog","approval":false},
                {"name":"compute_submit","description":"dispatch a GPU compute job","approval":true},
                {"name":"execute_bash","description":"run a guarded bash command","approval":true},
            ]}),
        );
    }

    /// Enqueue the response sequence for a user message.
    fn enqueue_response(&self, _user_text: &str) {
        match self.scenario {
            FakeScenario::BasicChat => {
                let response = "Fake backend response: PRISM TUI is running \
                    in deterministic test mode.";
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
            FakeScenario::StreamingAnswer => {
                // Many small deltas to exercise streaming.
                let words: Vec<&str> = "Streaming answer: each word is a \
                    separate delta to test the TUI streaming pipeline \
                    handles token-by-token rendering correctly without \
                    dropping or duplicating text."
                    .split_whitespace()
                    .collect();
                for word in &words {
                    self.notify(
                        "ui.text.delta",
                        serde_json::json!({"text": format!("{word} ")}),
                    );
                }
                self.notify("ui.text.flush", serde_json::json!({}));
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            FakeScenario::ThinkingStream => {
                // Thinking tokens first, then answer.
                let thinking = "Let me reason about this. The user is \
                    asking a question. I should respond with a clear answer.";
                for word in thinking.split_whitespace() {
                    self.notify(
                        "ui.thinking.delta",
                        serde_json::json!({"text": format!("{word} ")}),
                    );
                }
                let answer = "Based on my reasoning, here is the answer: \
                    the fake backend is working correctly.";
                for word in answer.split_whitespace() {
                    self.notify(
                        "ui.text.delta",
                        serde_json::json!({"text": format!("{word} ")}),
                    );
                }
                self.notify("ui.text.flush", serde_json::json!({}));
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            FakeScenario::ToolSuccess => {
                self.notify(
                    "ui.tool.start",
                    serde_json::json!({
                        "tool_name": "sample_material",
                        "verb": "Running",
                        "call_id": "call-1",
                        "preview": "{\"n\": 10}",
                        "approval_required": false,
                    }),
                );
                self.notify(
                    "ui.card",
                    serde_json::json!({
                        "tool_name": "sample_material",
                        "call_id": "call-1",
                        "content": "W0.3 Mo0.2 Ta0.3 Nb0.2",
                        "card_type": "results",
                        "elapsed_ms": 292,
                        "provenance_id": "prov_001",
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            FakeScenario::ToolError => {
                self.notify(
                    "ui.tool.start",
                    serde_json::json!({
                        "tool_name": "compute_submit",
                        "verb": "Running",
                        "call_id": "call-2",
                        "approval_required": true,
                    }),
                );
                self.notify(
                    "ui.card",
                    serde_json::json!({
                        "tool_name": "compute_submit",
                        "call_id": "call-2",
                        "content": "Error: budget exceeded ($50.00 limit)",
                        "card_type": "error",
                        "elapsed_ms": 1200,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            FakeScenario::ApprovalRequired => {
                self.notify(
                    "ui.prompt",
                    serde_json::json!({
                        "tool_name": "compute_submit",
                        "call_id": "call-3",
                        "message": "Allow compute_submit?",
                        "tool_args": {"image": "vasp:6.5", "budget_max_usd": 10.0},
                        "tool_description": "Dispatch a GPU compute job",
                        "requires_approval": true,
                        "permission_mode": "full_access",
                        "choices": ["y", "n", "a"],
                        "prompt_type": "approval",
                    }),
                );
            }
            FakeScenario::CostMetrics => {
                self.notify(
                    "ui.cost",
                    serde_json::json!({
                        "turn_cost": 0.001,
                        "session_cost": 0.05,
                        "input_tokens": 1200,
                        "output_tokens": 800,
                        "cache_tokens": 400,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            FakeScenario::BackendWarningError => {
                self.notify(
                    "ui.backend.warning",
                    serde_json::json!({
                        "code": "rate_limit",
                        "message": "Approaching API rate limit (80% of quota)",
                    }),
                );
                self.notify(
                    "ui.backend.error",
                    serde_json::json!({
                        "code": 429,
                        "message": "Rate limit exceeded, please retry in 60s",
                        "recoverable": true,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            FakeScenario::AnsiInjection => {
                // Inject ANSI/control sequences into text to verify the
                // sanitizer strips them before they reach the render path.
                self.notify(
                    "ui.text.delta",
                    serde_json::json!({
                        "text": "\x1b[31mred text\x1b[0m \x1b]0;owned\x07safe \x07BEL\x08BS\x0dCR\x7fDEL",
                    }),
                );
                self.notify(
                    "ui.tool.start",
                    serde_json::json!({
                        "tool_name": "\x1b[36mtainted_tool\x1b[0m",
                        "verb": "Running",
                        "call_id": "call-ansi",
                    }),
                );
                self.notify(
                    "ui.card",
                    serde_json::json!({
                        "tool_name": "tainted_tool",
                        "content": "\x1b[32mresult\x1b[0m with \x1b[2Jclear",
                        "card_type": "results",
                        "elapsed_ms": 50,
                    }),
                );
                self.notify("ui.text.flush", serde_json::json!({}));
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
        }
    }

    /// Enqueue a simple response for a slash command.
    fn enqueue_command_response(&self, command: &str) {
        // Model picker: `/models list` → deterministic `ui.model.list`.
        if command.starts_with("/models") {
            let items = vec![
                serde_json::json!({"id":"gemma-4-12B-it-qat-UD-Q4_K_XL.gguf","label":"Gemma 4 12B (local)","provider":"local","free":true}),
                serde_json::json!({"id":"anthropic/claude-sonnet-4","label":"Claude Sonnet 4","provider":"anthropic","free":false}),
                serde_json::json!({"id":"openai/gpt-5.5","label":"GPT-5.5","provider":"openai","free":false}),
                serde_json::json!({"id":"openrouter/arcee-ai/trinity-large-preview:free","label":"Arcee Trinity (free)","provider":"openrouter","free":true}),
            ];
            self.notify(
                "ui.model.list",
                serde_json::json!({"models": items, "current": "gemma-4-12B-it-qat-UD-Q4_K_XL.gguf"}),
            );
            self.notify("ui.turn.complete", serde_json::json!({}));
            return;
        }
        // GPU picker: `/gpus` → deterministic `ui.gpu.list`.
        if command == "/gpus" || command.starts_with("/gpus ") {
            let gpus = vec![
                serde_json::json!({"gpu_type":"RTX-4090","vram_gb":24,"region":"US","provider":"runpod","price_per_hour_usd":0.44,"available":true}),
                serde_json::json!({"gpu_type":"L40S","vram_gb":48,"region":"EU","provider":"datacrunch","price_per_hour_usd":0.89,"available":true}),
                serde_json::json!({"gpu_type":"A100-80GB","vram_gb":80,"region":"US","provider":"runpod","price_per_hour_usd":1.64,"available":true}),
                serde_json::json!({"gpu_type":"H100-SXM5","vram_gb":80,"region":"EU","provider":"datacrunch","price_per_hour_usd":2.19,"available":true}),
                serde_json::json!({"gpu_type":"B200","vram_gb":192,"region":"US","provider":"nebius","price_per_hour_usd":4.80,"available":false}),
            ];
            self.notify("ui.gpu.list", serde_json::json!({"gpus": gpus}));
            self.notify("ui.turn.complete", serde_json::json!({}));
            return;
        }
        // Session picker: `/sessions` → deterministic `ui.session.list`.
        if command == "/sessions" {
            let sessions = vec![
                serde_json::json!({"session_id":"sess-3","created_at":1751400000.0,"turn_count":12,"model":"gemma-4-12B-it-qat-UD-Q4_K_XL.gguf","size_kb":42.5,"is_latest":true}),
                serde_json::json!({"session_id":"sess-2","created_at":1751200000.0,"turn_count":4,"model":"anthropic/claude-sonnet-4","size_kb":9.1,"is_latest":false}),
                serde_json::json!({"session_id":"sess-1","created_at":1751000000.0,"turn_count":1,"model":"openai/gpt-5.5","size_kb":2.0,"is_latest":false}),
            ];
            self.notify("ui.session.list", serde_json::json!({"sessions": sessions}));
            self.notify("ui.turn.complete", serde_json::json!({}));
            return;
        }
        // GitHub panel: `/gh <tab>` → deterministic `ui.gh.data` payload.
        if command.starts_with("/gh ") || command == "/gh" {
            let tab = command
                .split_whitespace()
                .nth(1)
                .unwrap_or("issues")
                .to_string();
            let items = match tab.as_str() {
                "prs" => vec![
                    serde_json::json!({"number": 14, "title": "Add Ratatui theme picker", "state": "OPEN", "author": {"login": "alice"}, "headRefName": "feat/themes", "url": "https://github.com/Darth-Hidious/PRISM/pull/14"}),
                    serde_json::json!({"number": 9, "title": "Wire models to backend", "state": "OPEN", "author": {"login": "bob"}, "headRefName": "feat/models", "url": "https://github.com/Darth-Hidious/PRISM/pull/9"}),
                ],
                "status" => vec![
                    serde_json::json!({"name": "build", "status": "completed", "conclusion": "success", "headBranch": "main", "url": "https://github.com/Darth-Hidious/PRISM/actions/runs/1"}),
                    serde_json::json!({"name": "test", "status": "completed", "conclusion": "failure", "headBranch": "main", "url": "https://github.com/Darth-Hidious/PRISM/actions/runs/2"}),
                ],
                "bug" => vec![
                    serde_json::json!({"url": "https://github.com/Darth-Hidious/PRISM/issues/101", "title": "bug: sample"}),
                ],
                _ => vec![
                    serde_json::json!({"number": 42, "title": "TUI crashes on startup", "state": "OPEN", "author": {"login": "alice"}, "labels": [{"name": "bug"}], "url": "https://github.com/Darth-Hidious/PRISM/issues/42"}),
                    serde_json::json!({"number": 7, "title": "Add dark mode", "state": "CLOSED", "author": {"login": "bob"}, "labels": [{"name": "ui"},{"name": "help wanted"}], "url": "https://github.com/Darth-Hidious/PRISM/issues/7"}),
                ],
            };
            self.notify(
                "ui.gh.data",
                serde_json::json!({"tab": tab, "repo": "Darth-Hidious/PRISM", "items": items, "error": ()}),
            );
            self.notify("ui.turn.complete", serde_json::json!({}));
            return;
        }
        self.notify(
            "ui.status",
            serde_json::json!({
                "model": "fake-backend",
                "session_mode": "chat",
                "message_count": 1,
            }),
        );
        if command.starts_with("/tools") {
            self.notify(
                "ui.view",
                serde_json::json!({
                    "title": "Tools",
                    "tabs": [{
                        "title": "Available",
                        "body": "sample_material, evaluate_material, materials_search (fake backend)"
                    }],
                }),
            );
        }
        self.notify("ui.turn.complete", serde_json::json!({}));
    }

    /// Enqueue the approval response based on the user's decision.
    fn enqueue_approval_response(&self, response: &str) {
        match response {
            "y" => {
                self.notify(
                    "ui.card",
                    serde_json::json!({
                        "tool_name": "compute_submit",
                        "call_id": "call-3",
                        "content": "Job submitted successfully (job_id: fake-123)",
                        "card_type": "results",
                        "elapsed_ms": 500,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            "n" => {
                self.notify(
                    "ui.status",
                    serde_json::json!({
                        "model": "fake-backend",
                        "session_mode": "chat",
                        "message_count": 1,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            "a" => {
                self.notify(
                    "ui.permissions",
                    serde_json::json!({
                        "mode": "agent",
                        "auto_approved": true,
                    }),
                );
                self.notify(
                    "ui.card",
                    serde_json::json!({
                        "tool_name": "compute_submit",
                        "call_id": "call-3",
                        "content": "Job submitted (auto-approved for session)",
                        "card_type": "results",
                        "elapsed_ms": 500,
                    }),
                );
                self.notify("ui.turn.complete", serde_json::json!({}));
            }
            _ => {}
        }
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

    pub fn send_approval(&mut self, response: &str, _tool_name: &str) -> Result<()> {
        // Enqueue the deterministic approval response based on the
        // user's decision (y/n/a).  This lets the TUI test the full
        // approval lifecycle: prompt → user key → backend response.
        self.enqueue_approval_response(response);
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

    pub fn send_approval(&mut self, response: &str, tool_name: &str) -> Result<()> {
        match self {
            Self::Real(b) => b.send_approval(response, tool_name),
            Self::Fake(b) => b.send_approval(response, tool_name),
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
