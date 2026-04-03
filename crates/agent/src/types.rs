use serde::Serialize;

/// Configuration for the agent loop.
pub struct AgentConfig {
    pub system_prompt: String,
    pub max_iterations: usize,
    pub auto_approve: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_iterations: 20,
            auto_approve: false,
        }
    }
}

/// Events emitted by the agent loop to the UI layer.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    TextDelta { text: String },
    ToolStart { tool_name: String, call_id: String },
    ToolResult {
        tool_name: String,
        call_id: String,
        content: String,
        elapsed_ms: u64,
        is_error: bool,
    },
    ApprovalRequired {
        tool_name: String,
        call_id: String,
        tool_args: serde_json::Value,
    },
    Cost {
        input_tokens: u64,
        output_tokens: u64,
        turn_cost: f64,
        session_cost: f64,
    },
    TurnComplete,
}
