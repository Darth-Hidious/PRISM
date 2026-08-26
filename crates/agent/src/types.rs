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
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
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

/// Truthful context-selection outcome for one LLM request, retaining the
/// agent-loop iteration that produced it.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextPrimingRecord {
    pub iteration: usize,
    pub status: crate::influence::ContextPrimingStatus,
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
    /// Reasoning/thinking tokens — separate from the response text.
    /// Rendered dimmed and collapsible by the TUI.
    ThinkingDelta {
        text: String,
    },
    /// Signal the frontend to flush accumulated streaming text into chat history.
    TextFlush,
    /// Truthful per-request report of whether influence-ranked context was
    /// actually injected, or which fallback handled the request instead.
    ContextPriming {
        iteration: usize,
        status: crate::influence::ContextPrimingStatus,
    },
    ToolCallStart {
        tool_name: String,
        call_id: String,
        preview: Option<String>,
    },
    ToolCallResult {
        call_id: String,
        tool_name: String,
        content: String,
        /// The arguments the call was made with.
        ///
        /// Carried on the RESULT, not just on the approval request, because
        /// the durable session record is written from this event. Without it
        /// `~/.prism/sessions/*.jsonl` held every tool's answer and none of
        /// their questions, so no call was reproducible and a failure taught
        /// nothing.
        tool_args: serde_json::Value,
        summary: Option<String>,
        preview: Option<String>,
        elapsed_ms: u64,
        is_error: bool,
    },
    ToolApprovalRequest {
        tool_name: String,
        tool_args: serde_json::Value,
        call_id: String,
        tool_description: Option<String>,
        requires_approval: bool,
        permission_mode: String,
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

/// Default reasoning-step backstop: high enough that it never interrupts real
/// research, low enough to stop a genuine runaway from billing forever.
/// Set `max_iterations: 0` for no cap at all.
pub fn default_max_iterations() -> usize {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub system_prompt: String,
    /// Reasoning steps one turn may take. `0` means no cap.
    ///
    /// This is a runaway backstop, NOT a research budget. It used to default
    /// to 20, which is roughly a dozen web reads — measured on 2026-08-25, a
    /// polymer literature question spent 19 tool calls establishing a correct
    /// answer, hit the cap, and the turn ended with `text: None`. The work was
    /// done and then thrown away. A limit low enough to interrupt ordinary
    /// research is a muzzle; the doom-loop detector, not this number, is what
    /// catches an agent going in circles.
    pub max_iterations: usize,
    pub auto_approve: bool,
    pub model: String,
    /// When set (weak/unknown models via their PromptProfile), the per-request
    /// tool set is restricted to the curated core set + find_tools + tools the
    /// model has pinned via discovery. Default false = today's full top-K.
    #[serde(default)]
    pub core_tools_only: bool,
    /// Nesting depth of THIS agent in a `spawn_subagent` chain: 0 = the
    /// top-level agent, 1 = a subagent it spawned, and so on. Threaded through
    /// the nested config so the recursion cap can be enforced (see
    /// `subagent::MAX_SUBAGENT_DEPTH`).
    #[serde(default)]
    pub subagent_depth: usize,
    /// Set on every agent spawned BY `orchestrate_agents`: such an agent may
    /// not orchestrate again.
    ///
    /// Fan-out is WIDTH, not depth. Without this, a nested orchestration got
    /// its own fresh call budget and inherited `auto_approve`, so one ordinary
    /// "Allow All" could authorise a second batch the approver never saw and
    /// the first budget never counted — width x width, on the order of a
    /// thousand turns from one consent. The depth cap did not stop it, because
    /// the escape is not depth.
    ///
    /// A caller who wants more parallel work asks for a WIDER batch: visible
    /// in the one prompt, charged to the one budget.
    #[serde(default)]
    pub orchestration_forbidden: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            max_iterations: default_max_iterations(),
            auto_approve: false,
            model: "claude-sonnet-4-6".to_string(),
            core_tools_only: false,
            subagent_depth: 0,
            orchestration_forbidden: false,
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
        let a = UsageInfo {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        let b = UsageInfo {
            input_tokens: 20,
            output_tokens: 3,
            ..Default::default()
        };
        let c = a + b;
        assert_eq!(c.input_tokens, 30);
        assert_eq!(c.output_tokens, 8);
    }

    #[test]
    fn usage_add_assign() {
        let mut a = UsageInfo {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        a += UsageInfo {
            input_tokens: 20,
            output_tokens: 3,
            ..Default::default()
        };
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
        let ev = AgentEvent::TextDelta {
            text: "hello".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "TextDelta");
        assert_eq!(json["text"], "hello");
    }
}
