//! Message types — every input becomes a Msg for the TEA update function.

use serde_json::Value;

/// Agent backend events (from JSON-RPC notifications)
#[derive(Debug, Clone)]
pub enum AgentMsg {
    Welcome { version: String, tool_count: u64 },
    Status { model: String, mode: String, message_count: usize },
    TextDelta(String),
    ThinkingDelta(String),
    TextFlush,
    ToolStart { tool_name: String, verb: String, call_id: String },
    ToolCard { tool_name: String, content: String, card_type: String, elapsed_ms: u64 },
    ApprovalPrompt { tool_name: String, message: String },
    Cost { turn_cost: f64, session_cost: f64 },
    TurnComplete,
    View { title: String, tabs: Vec<(String, String)> },
    Error(String),
    Unknown(Value),
}

/// Parse a JSON-RPC notification into an AgentMsg
pub fn parse_notification(msg: &Value) -> AgentMsg {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "ui.welcome" => AgentMsg::Welcome {
            version: params.get("version").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
            tool_count: params.get("tool_count").and_then(|v| v.as_u64()).unwrap_or(0),
        },
        "ui.status" => AgentMsg::Status {
            model: params.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string(),
            mode: params.get("session_mode").and_then(|m| m.as_str()).unwrap_or("chat").to_string(),
            message_count: params.get("message_count").and_then(|m| m.as_u64()).unwrap_or(0) as usize,
        },
        "ui.text.delta" => AgentMsg::TextDelta(
            params.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        ),
        "ui.thinking.delta" => AgentMsg::ThinkingDelta(
            params.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        ),
        "ui.text.flush" => AgentMsg::TextFlush,
        "ui.tool.start" => AgentMsg::ToolStart {
            tool_name: params.get("tool_name").and_then(|n| n.as_str()).unwrap_or("tool").to_string(),
            verb: params.get("verb").and_then(|v| v.as_str()).unwrap_or("Running").to_string(),
            call_id: params.get("call_id").and_then(|c| c.as_str()).unwrap_or("").to_string(),
        },
        "ui.card" => AgentMsg::ToolCard {
            tool_name: params.get("tool_name").and_then(|n| n.as_str()).unwrap_or("tool").to_string(),
            content: params.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string(),
            card_type: params.get("card_type").and_then(|c| c.as_str()).unwrap_or("results").to_string(),
            elapsed_ms: params.get("elapsed_ms").and_then(|e| e.as_u64()).unwrap_or(0),
        },
        "ui.prompt" => AgentMsg::ApprovalPrompt {
            tool_name: params.get("tool_name").and_then(|n| n.as_str()).unwrap_or("tool").to_string(),
            message: params.get("message").and_then(|m| m.as_str()).unwrap_or("Approve?").to_string(),
        },
        "ui.cost" => AgentMsg::Cost {
            turn_cost: params.get("turn_cost").and_then(|c| c.as_f64()).unwrap_or(0.0),
            session_cost: params.get("session_cost").and_then(|c| c.as_f64()).unwrap_or(0.0),
        },
        "ui.turn.complete" => AgentMsg::TurnComplete,
        "ui.view" => {
            let title = params.get("title").and_then(|t| t.as_str()).unwrap_or("View").to_string();
            let tabs = params.get("tabs")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter().filter_map(|tab| {
                        let t = tab.get("title").and_then(|t| t.as_str()).unwrap_or("");
                        let b = tab.get("body").and_then(|b| b.as_str()).unwrap_or("");
                        if !b.is_empty() { Some((t.to_string(), b.to_string())) } else { None }
                    }).collect()
                })
                .unwrap_or_default();
            AgentMsg::View { title, tabs }
        }
        "" => {
            if let Some(err) = msg.get("error") {
                AgentMsg::Error(err.to_string())
            } else {
                AgentMsg::Unknown(msg.clone())
            }
        }
        _ => AgentMsg::Unknown(msg.clone()),
    }
}