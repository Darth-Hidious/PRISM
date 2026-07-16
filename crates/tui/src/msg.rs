//! Message types — every input becomes a Msg for the TEA update function.
//!
//! `AgentMsg` is the typed event protocol for the TUI.  Every JSON-RPC
//! notification from the agent backend is parsed into one of these
//! variants by [`parse_notification`].  The render path never sees raw
//! JSON — only typed events — so malformed payloads collapse into
//! [`AgentMsg::Unknown`] instead of leaking untrusted strings into the
//! view layer.
//!
//! ## Patch 1 scope
//!
//! This patch enriches existing variants with fields the backend
//! already sends but the parser was dropping.  It also adds variants
//! for wire methods that already exist (`ui.permissions`,
//! `ui.session.list`) and a structured `BackendError` for JSON-RPC
//! error objects.  It does **not** add variants for target events that
//! the backend doesn't emit yet (WorkflowStep*, MetricsUpdated,
//! BackendDisconnected, etc.) — those belong in the fake-backend or
//! backend-emission patches.
//!
//! All new fields on existing variants are `Option<T>` so missing
//! payload fields degrade gracefully.  Pattern matches in `app.rs`
//! use `..` to stay resilient to future field additions.

use serde_json::Value;

/// Agent backend events (from JSON-RPC notifications).
///
/// Variants are grouped by lifecycle phase.  Every variant is `Clone`
/// so the event can be logged to a replay buffer before being applied
/// to `App` state.
#[derive(Debug, Clone)]
pub enum AgentMsg {
    // ── Session lifecycle ────────────────────────────────────────────
    /// `ui.welcome` — sent once on backend startup.
    Welcome { version: String, tool_count: u64 },
    /// `ui.permissions` — permission mode update.  The backend sends
    /// this after `init` and whenever the permission mode changes.
    Permissions {
        mode: Option<String>,
        auto_approved: Option<bool>,
        /// The full params object, retained for forward compatibility
        /// (the backend may add fields like `blocked`, `read_only`,
        /// `full_access`, `allow_overrides`, etc.).
        raw: Value,
    },
    /// `ui.session.list` — list of previous sessions (response to a
    /// `/sessions` slash command).  Each session is an opaque JSON
    /// object; the TUI just displays them.
    SessionList { sessions: Vec<Value>, raw: Value },

    /// `ui.gh.data` — GitHub data for the in-TUI GitHub panel (response to
    /// `/gh issues|prs|status|bug`). `items` are raw `gh --json` objects;
    /// the TUI normalizes per `tab`. `error` is set if `gh` failed.
    GhData {
        tab: String,
        repo: String,
        items: Vec<Value>,
        error: Option<String>,
    },

    /// `ui.model.list` — hosted model catalog (response to `/models list`),
    /// rendered as a fuzzy model picker. Each item is `{id,label,provider,free}`.
    /// `notice` carries list provenance (e.g. stale-cache warning) so the
    /// picker never presents cached data as a live list.
    ModelList {
        models: Vec<Value>,
        current: String,
        notice: Option<String>,
    },

    /// `ui.gpu.list` — live GPU procurement catalog (response to `/gpus`),
    /// rendered as the GPU picker. Each item is
    /// `{gpu_type,vram_gb,region,provider,price_per_hour_usd,available}`.
    /// `error` is set if the catalog fetch failed.
    GpuList {
        gpus: Vec<Value>,
        error: Option<String>,
    },

    /// `ui.nodes.list` — the user's platform nodes (response to `/nodes`),
    /// rendered as the Nodes view. Each item is
    /// `{name,status,visibility,profile,last_seen_at,price_per_hour_usd}`.
    /// `error` is set if the node fetch failed.
    NodeList {
        nodes: Vec<Value>,
        error: Option<String>,
    },

    /// `ui.tools.catalog` — the live tool catalog (pushed by `/tools`), shown in
    /// the Workspace sidebar Tools tab. Each item is
    /// `{name,description,approval,source,source_detail}`.
    ToolsCatalog { tools: Vec<Value> },

    // ── Status / mode ────────────────────────────────────────────────
    /// `ui.status` — model / mode / message count update.
    Status {
        model: String,
        mode: String,
        message_count: usize,
    },

    // ── Streaming ────────────────────────────────────────────────────
    /// `ui.text.delta` — a chunk of visible answer text.
    TextDelta(String),
    /// `ui.thinking.delta` — a chunk of reasoning/thinking text (dimmed,
    /// collapsible, never mixed into the answer transcript).
    ThinkingDelta(String),
    /// `ui.text.flush` — flush accumulated streaming text.
    TextFlush,
    /// `ui.turn.complete` — the agent finished a full turn.
    TurnComplete,

    // ── Tool lifecycle ───────────────────────────────────────────────
    /// `ui.tool.start` — a tool call began.
    ///
    /// The backend sends `preview` (truncated args) and
    /// `approval_required` in addition to the base fields.  Both are
    /// captured here as `Option<T>`.
    ToolStart {
        tool_name: String,
        verb: String,
        call_id: Option<String>,
        preview: Option<String>,
        approval_required: Option<bool>,
    },
    /// `ui.card` — a tool call finished and produced a result card.
    ///
    /// The backend sends `data` (structured output) and may send
    /// `call_id` / `provenance_id` in future versions.  All are
    /// captured as `Option<T>`.
    ToolCard {
        tool_name: String,
        content: String,
        card_type: String,
        elapsed_ms: Option<u64>,
        call_id: Option<String>,
        provenance_id: Option<String>,
        data: Option<Value>,
    },

    // ── Approval lifecycle ───────────────────────────────────────────
    /// `ui.prompt` — the backend is requesting approval for a tool call.
    ///
    /// The backend sends a rich payload (`call_id`, `tool_args`,
    /// `tool_description`, `requires_approval`, `permission_mode`,
    /// `choices`, `prompt_type`).  All are captured here; the TUI
    /// currently only uses `tool_name` and `message` for the popup,
    /// but the rest is available for the approval-state-machine patch.
    ApprovalPrompt {
        tool_name: String,
        message: String,
        call_id: Option<String>,
        tool_args: Option<Value>,
        tool_description: Option<String>,
        requires_approval: Option<bool>,
        permission_mode: Option<String>,
        choices: Vec<String>,
        prompt_type: Option<String>,
    },

    // ── Cost / metrics ───────────────────────────────────────────────
    /// `ui.cost` — token counts + estimated cost for the turn/session.
    ///
    /// The backend sends `input_tokens` and `output_tokens` in
    /// addition to the cost figures.  Both are captured as `Option<T>`.
    Cost {
        turn_cost: f64,
        session_cost: f64,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cache_tokens: Option<u64>,
    },

    // ── Views ────────────────────────────────────────────────────────
    /// `ui.view` — a tabbed view panel (e.g. search results).
    View {
        title: String,
        tabs: Vec<(String, String)>,
    },

    // ── Backend health ───────────────────────────────────────────────
    /// `ui.backend.warning` — a non-fatal backend warning.
    ///
    /// Not currently emitted by the backend but included for safe
    /// parsing if it appears.  `code` is a string label (e.g.
    /// `"rate_limit"`).
    BackendWarning {
        code: Option<String>,
        message: String,
    },
    /// A JSON-RPC error response (object with an `error` field) or an
    /// `ui.backend.error` notification.  Supersedes the old
    /// `Error(String)` for structured errors while keeping `Error` as
    /// an alias for backward compatibility.
    BackendError {
        code: Option<i64>,
        message: String,
        recoverable: Option<bool>,
    },

    // ── Legacy / fallback ────────────────────────────────────────────
    /// A generic error string.  Kept for backward compatibility with
    /// the old `"" => Error(err.to_string())` arm.  New code should
    /// prefer `BackendError`.
    Error(String),
    /// Any unrecognized notification.  Kept so malformed-event tests
    /// can assert the TUI doesn't crash on unknown payloads.
    Unknown(Value),
}

/// Parse a JSON-RPC notification into an [`AgentMsg`].
///
/// Unknown methods collapse into [`AgentMsg::Unknown`] — the TUI never
/// crashes on a malformed payload.  Missing fields in known methods
/// fall back to sensible defaults (empty string, 0, `None`) so a
/// partial payload degrades gracefully instead of panicking.  Extra
/// unknown fields are silently ignored.
pub fn parse_notification(msg: &Value) -> AgentMsg {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        // ── Session lifecycle ────────────────────────────────────────
        "ui.welcome" => AgentMsg::Welcome {
            version: params
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            tool_count: params
                .get("tool_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        },
        "ui.permissions" => AgentMsg::Permissions {
            mode: params
                .get("mode")
                .and_then(|m| m.as_str())
                .map(str::to_string),
            auto_approved: params.get("auto_approved").and_then(|a| a.as_bool()),
            raw: params,
        },
        "ui.session.list" => AgentMsg::SessionList {
            sessions: params
                .get("sessions")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default(),
            raw: params,
        },

        // ── GitHub panel ─────────────────────────────────────────────
        "ui.gh.data" => AgentMsg::GhData {
            tab: params
                .get("tab")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            repo: params
                .get("repo")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            items: params
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            error: params
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },

        // ── Model picker ─────────────────────────────────────────────
        "ui.model.list" => AgentMsg::ModelList {
            models: params
                .get("models")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            current: params
                .get("current")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            notice: params
                .get("notice")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },

        // ── GPU picker ───────────────────────────────────────────────
        "ui.gpu.list" => AgentMsg::GpuList {
            gpus: params
                .get("gpus")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            error: params
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },

        // ── Nodes view ───────────────────────────────────────────────
        "ui.nodes.list" => AgentMsg::NodeList {
            nodes: params
                .get("nodes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            error: params
                .get("error")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },

        // ── Tool catalog (sidebar) ───────────────────────────────────
        "ui.tools.catalog" => AgentMsg::ToolsCatalog {
            tools: params
                .get("tools")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
        },

        // ── Status ───────────────────────────────────────────────────
        "ui.status" => AgentMsg::Status {
            model: params
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            mode: params
                .get("session_mode")
                .and_then(|m| m.as_str())
                .unwrap_or("chat")
                .to_string(),
            message_count: params
                .get("message_count")
                .and_then(|m| m.as_u64())
                .unwrap_or(0) as usize,
        },

        // ── Streaming ────────────────────────────────────────────────
        "ui.text.delta" => AgentMsg::TextDelta(
            params
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "ui.thinking.delta" => AgentMsg::ThinkingDelta(
            params
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        "ui.text.flush" => AgentMsg::TextFlush,
        "ui.turn.complete" => AgentMsg::TurnComplete,

        // ── Tool lifecycle ───────────────────────────────────────────
        "ui.tool.start" => AgentMsg::ToolStart {
            tool_name: params
                .get("tool_name")
                .and_then(|n| n.as_str())
                .unwrap_or("tool")
                .to_string(),
            verb: params
                .get("verb")
                .and_then(|v| v.as_str())
                .unwrap_or("Running")
                .to_string(),
            call_id: params
                .get("call_id")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            preview: params
                .get("preview")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            approval_required: params.get("approval_required").and_then(|a| a.as_bool()),
        },
        "ui.card" => AgentMsg::ToolCard {
            tool_name: params
                .get("tool_name")
                .and_then(|n| n.as_str())
                .unwrap_or("tool")
                .to_string(),
            content: params
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string(),
            card_type: params
                .get("card_type")
                .and_then(|c| c.as_str())
                .unwrap_or("results")
                .to_string(),
            elapsed_ms: params.get("elapsed_ms").and_then(|e| e.as_u64()),
            call_id: params
                .get("call_id")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            provenance_id: params
                .get("provenance_id")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            data: params.get("data").cloned(),
        },

        // ── Approval ─────────────────────────────────────────────────
        "ui.prompt" => AgentMsg::ApprovalPrompt {
            tool_name: params
                .get("tool_name")
                .and_then(|n| n.as_str())
                .unwrap_or("tool")
                .to_string(),
            message: params
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Approve?")
                .to_string(),
            call_id: params
                .get("call_id")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            tool_args: params.get("tool_args").cloned(),
            tool_description: params
                .get("tool_description")
                .and_then(|d| d.as_str())
                .map(str::to_string),
            requires_approval: params.get("requires_approval").and_then(|r| r.as_bool()),
            permission_mode: params
                .get("permission_mode")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            choices: params
                .get("choices")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            prompt_type: params
                .get("prompt_type")
                .and_then(|p| p.as_str())
                .map(str::to_string),
        },

        // ── Cost ─────────────────────────────────────────────────────
        "ui.cost" => AgentMsg::Cost {
            turn_cost: params
                .get("turn_cost")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0),
            session_cost: params
                .get("session_cost")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0),
            input_tokens: params.get("input_tokens").and_then(|t| t.as_u64()),
            output_tokens: params.get("output_tokens").and_then(|t| t.as_u64()),
            cache_tokens: params.get("cache_tokens").and_then(|t| t.as_u64()),
        },

        // ── Views ─────────────────────────────────────────────────────
        "ui.view" => {
            let title = params
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("View")
                .to_string();
            let tabs = params
                .get("tabs")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|tab| {
                            let t = tab.get("title").and_then(|t| t.as_str()).unwrap_or("");
                            let b = tab.get("body").and_then(|b| b.as_str()).unwrap_or("");
                            if !b.is_empty() {
                                Some((t.to_string(), b.to_string()))
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            AgentMsg::View { title, tabs }
        }

        // ── Backend health ───────────────────────────────────────────
        "ui.backend.warning" => AgentMsg::BackendWarning {
            code: params
                .get("code")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            message: params
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        },
        "ui.backend.error" => AgentMsg::BackendError {
            code: params.get("code").and_then(|c| c.as_i64()),
            message: params
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            recoverable: params.get("recoverable").and_then(|r| r.as_bool()),
        },

        // ── Fallbacks ────────────────────────────────────────────────
        "" => {
            // A JSON-RPC error response (no method, has error field).
            if let Some(err) = msg.get("error") {
                // Try to extract structured fields from the error object.
                let code = err.get("code").and_then(|c| c.as_i64());
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or_else(|| err.as_str().unwrap_or(""))
                    .to_string();
                if code.is_some() || err.get("message").is_some() {
                    AgentMsg::BackendError {
                        code,
                        message,
                        recoverable: None,
                    }
                } else {
                    // Fallback: treat the whole error value as a string.
                    AgentMsg::Error(err.to_string())
                }
            } else {
                AgentMsg::Unknown(msg.clone())
            }
        }
        _ => AgentMsg::Unknown(msg.clone()),
    }
}
