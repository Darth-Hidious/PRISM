use serde::{Deserialize, Serialize};
use std::ops::{Add, AddAssign};

// ---------------------------------------------------------------------------
// UsageInfo — token counts with arithmetic operators
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl UsageInfo {
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_tokens
            + self.cache_read_tokens
    }
}

impl Add for UsageInfo {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            cache_creation_tokens: self.cache_creation_tokens + rhs.cache_creation_tokens,
            cache_read_tokens: self.cache_read_tokens + rhs.cache_read_tokens,
        }
    }
}

impl AddAssign for UsageInfo {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_creation_tokens += rhs.cache_creation_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
    }
}

// ---------------------------------------------------------------------------
// ToolCallEvent — a single tool call from the LLM
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallEvent {
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub call_id: String,
}

// ---------------------------------------------------------------------------
// AgentResponse — complete LLM response for one turn
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCallEvent>,
    pub usage: Option<UsageInfo>,
}

impl AgentResponse {
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

// ---------------------------------------------------------------------------
// AgentEvent — streaming events for the UI layer (tagged enum)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    TextDelta {
        text: String,
    },
    ToolCallStart {
        tool_name: String,
        call_id: String,
    },
    ToolCallResult {
        call_id: String,
        tool_name: String,
        content: String,
        summary: Option<String>,
        elapsed_ms: u64,
        is_error: bool,
    },
    ToolApprovalRequest {
        tool_name: String,
        tool_args: serde_json::Value,
        call_id: String,
    },
    TurnComplete {
        text: Option<String>,
        has_more: bool,
        usage: Option<UsageInfo>,
        total_usage: Option<UsageInfo>,
        estimated_cost: Option<f64>,
    },
}

// ---------------------------------------------------------------------------
// AgentConfig — session configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub max_iterations: usize,
    pub auto_approve: bool,
    pub model: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_iterations: 20,
            auto_approve: false,
            model: "claude-sonnet-4-6".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_total_tokens() {
        let u = UsageInfo {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 10,
            cache_read_tokens: 5,
        };
        assert_eq!(u.total_tokens(), 165);
    }

    #[test]
    fn usage_add() {
        let a = UsageInfo { input_tokens: 10, output_tokens: 5, ..Default::default() };
        let b = UsageInfo { input_tokens: 20, output_tokens: 3, ..Default::default() };
        let c = a + b;
        assert_eq!(c.input_tokens, 30);
        assert_eq!(c.output_tokens, 8);
    }

    #[test]
    fn usage_add_assign() {
        let mut a = UsageInfo { input_tokens: 10, output_tokens: 5, ..Default::default() };
        a += UsageInfo { input_tokens: 20, output_tokens: 3, ..Default::default() };
        assert_eq!(a.input_tokens, 30);
        assert_eq!(a.output_tokens, 8);
    }

    #[test]
    fn agent_response_has_tool_calls() {
        let empty = AgentResponse::default();
        assert!(!empty.has_tool_calls());

        let with_call = AgentResponse {
            tool_calls: vec![ToolCallEvent {
                tool_name: "read".into(),
                tool_args: serde_json::json!({}),
                call_id: "c1".into(),
            }],
            ..Default::default()
        };
        assert!(with_call.has_tool_calls());
    }

    #[test]
    fn agent_event_serializes_tagged() {
        let ev = AgentEvent::TextDelta { text: "hello".into() };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "TextDelta");
        assert_eq!(json["text"], "hello");
    }
}
