//! Method-name constants for the JSON-RPC IPC layer.
//!
//! Two vocabularies live here:
//!
//! 1. The **external surface** (`initialize`, `chat/send`, `tools/list`,
//!    `session/status`) that [`crate::surface`] serves to external frontends
//!    such as PRISM Desktop and IDE extensions.
//! 2. The **native backend protocol** that `prism backend` speaks over stdio.
//!    [`crate::bridge::BackendBridge`] uses these to drive the real agent —
//!    they are the same method names emitted/consumed by
//!    `prism_agent::protocol`.

// ── External surface (frontend ⇄ `prism ipc-serve`) ─────────────────

/// Frontend → server: handshake; returns version + capabilities.
pub const INITIALIZE: &str = "initialize";

/// Frontend → server: drive one agent turn with `params.text`.
pub const CHAT_SEND: &str = "chat/send";

/// Frontend → server: list the tools the agent can call.
pub const TOOLS_LIST: &str = "tools/list";

/// Frontend → server: current session status snapshot.
pub const SESSION_STATUS: &str = "session/status";

// ── Native backend protocol (`prism ipc-serve` ⇄ `prism backend`) ───

/// Server → backend: initialize the agent session.
pub const BACKEND_INIT: &str = "init";

/// Server → backend: user message that starts a turn.
pub const INPUT_MESSAGE: &str = "input.message";

/// Server → backend: slash command.
pub const INPUT_COMMAND: &str = "input.command";

/// Server → backend: response to an approval prompt.
pub const INPUT_PROMPT_RESPONSE: &str = "input.prompt_response";

/// Backend → server: welcome payload emitted after `init` (version, tool count).
pub const UI_WELCOME: &str = "ui.welcome";

/// Backend → server: full tool catalog emitted after `init`.
pub const UI_TOOLS_CATALOG: &str = "ui.tools.catalog";

/// Backend → server: status snapshot (model, session mode, message count).
pub const UI_STATUS: &str = "ui.status";

/// Backend → server: streaming text delta from the model.
pub const UI_TEXT_DELTA: &str = "ui.text.delta";

/// Backend → server: flush accumulated text.
pub const UI_TEXT_FLUSH: &str = "ui.text.flush";

/// Backend → server: turn finished — the sentinel that ends a turn drain.
pub const UI_TURN_COMPLETE: &str = "ui.turn.complete";
