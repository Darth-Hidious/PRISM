//! Method constants for JSON-RPC IPC protocol.
//!
//! These are the method names used between the Rust core and the Ink TUI.

/// Core → TUI: Initial handshake.
pub const HELLO: &str = "ipc.hello";

/// Core → TUI: Service status update.
pub const SERVICE_STATUS: &str = "node.service_status";

/// Core → TUI: Node is fully ready.
pub const NODE_READY: &str = "node.ready";

/// Core → TUI: Streaming text delta from LLM.
pub const TEXT_DELTA: &str = "ui.text.delta";

/// Core → TUI: Flush accumulated text.
pub const TEXT_FLUSH: &str = "ui.text.flush";

/// Core → TUI: Tool execution started.
pub const TOOL_START: &str = "ui.tool.start";

/// Core → TUI: Tool result card.
pub const CARD: &str = "ui.card";

/// Core → TUI: Token/cost update.
pub const COST: &str = "ui.cost";

/// Core → TUI: Approval prompt.
pub const PROMPT: &str = "ui.prompt";

/// Core → TUI: Turn completed.
pub const TURN_COMPLETE: &str = "ui.turn.complete";

/// TUI → Core: User sent a message.
pub const INPUT_MESSAGE: &str = "input.message";

/// TUI → Core: User sent a slash command.
pub const INPUT_COMMAND: &str = "input.command";

/// TUI → Core: User responded to a prompt.
pub const INPUT_PROMPT_RESPONSE: &str = "input.prompt_response";
