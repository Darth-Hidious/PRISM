//! Routing decisions emitted by the function-call router.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A parsed tool invocation extracted from FunctionGemma's output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

/// What the router decided for this turn.
#[derive(Debug, Clone)]
pub enum RoutingDecision {
    /// FunctionGemma emitted a concrete tool call. Caller should execute it
    /// and use the result instead of round-tripping to the chat LLM.
    Invoke(ToolCall),

    /// FunctionGemma did not emit a tool call. Caller should pass the
    /// request through to the chat LLM normally (with the top-K filtered
    /// tools attached, since the LLM might still want to call one or want
    /// to chat about them).
    Passthrough,
}
