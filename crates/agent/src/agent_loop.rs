use std::time::Instant;

use anyhow::{Context, Result};
use prism_ingest::llm::{ChatMessage, FunctionDef, LlmClient, ToolDefinition};
use prism_python_bridge::tool_server::ToolServerHandle;

use crate::types::{AgentConfig, AgentEvent};

/// Run a single conversational turn: push user message, loop LLM + tool calls
/// until the model produces a final text response or we hit max iterations.
pub async fn run_turn<F>(
    llm: &LlmClient,
    tool_server: &mut ToolServerHandle,
    history: &mut Vec<ChatMessage>,
    tools: &[ToolDefinition],
    config: &AgentConfig,
    user_message: &str,
    mut emit: F,
) -> Result<()>
where
    F: FnMut(AgentEvent),
{
    // Push user message
    history.push(ChatMessage {
        role: "user".to_string(),
        content: Some(user_message.to_string()),
        tool_calls: None,
        tool_call_id: None,
    });

    let mut total_input = 0u64;
    let mut total_output = 0u64;

    for _iteration in 0..config.max_iterations {
        // Build full message array: system prompt + conversation history
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(config.system_prompt.clone()),
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.extend(history.iter().cloned());

        let response = llm
            .chat_with_tools(&messages, tools)
            .await
            .context("LLM call failed")?;

        // Accumulate token usage
        if let Some(usage) = &response.usage {
            total_input += usage.prompt_tokens;
            total_output += usage.completion_tokens;
        }

        // Emit any text content
        if let Some(text) = &response.message.content {
            if !text.is_empty() {
                emit(AgentEvent::TextDelta { text: text.clone() });
            }
        }

        // Push assistant message to history
        history.push(response.message.clone());

        // If no tool calls, the turn is complete
        let tool_calls = match &response.message.tool_calls {
            Some(calls) if !calls.is_empty() => calls.clone(),
            _ => {
                emit(AgentEvent::TurnComplete {
                    text: None,
                    has_more: false,
                    usage: None,
                    total_usage: None,
                    estimated_cost: None,
                });
                return Ok(());
            }
        };

        // Dispatch each tool call
        for tc in &tool_calls {
            let tool_name = &tc.function.name;
            let call_id = &tc.id;

            emit(AgentEvent::ToolCallStart {
                tool_name: tool_name.clone(),
                call_id: call_id.clone(),
            });

            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or_default();

            let start = Instant::now();
            let result = tool_server.call_tool(tool_name, args).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;

            let (content, is_error) = match result {
                Ok(resp) => {
                    if let Some(err) = resp.get("error").and_then(|e| e.as_str()) {
                        (err.to_string(), true)
                    } else if let Some(r) = resp.get("result") {
                        (serde_json::to_string(r).unwrap_or_default(), false)
                    } else {
                        (serde_json::to_string(&resp).unwrap_or_default(), false)
                    }
                }
                Err(e) => (format!("Tool error: {e}"), true),
            };

            emit(AgentEvent::ToolCallResult {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                content: content.clone(),
                summary: None,
                elapsed_ms,
                is_error,
            });

            // Push tool result into history for next LLM call
            history.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(content),
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
            });
        }
    }

    // Hit max iterations without a final response
    emit(AgentEvent::TextDelta {
        text: "\n\n[Agent reached maximum iterations]".to_string(),
    });
    emit(AgentEvent::TurnComplete {
        text: None,
        has_more: false,
        usage: None,
        total_usage: None,
        estimated_cost: None,
    });
    Ok(())
}

/// Convert the Python tool server's JSON tool listing into OpenAI-format
/// tool definitions for the LLM.
pub fn tools_to_definitions(tools_json: &serde_json::Value) -> Vec<ToolDefinition> {
    let empty = vec![];
    let tools = tools_json
        .get("tools")
        .and_then(|t| t.as_array())
        .unwrap_or(&empty);
    tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?;
            let desc = t.get("description")?.as_str()?;
            let params = t.get("input_schema").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));
            Some(ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDef {
                    name: name.to_string(),
                    description: desc.to_string(),
                    parameters: params,
                },
            })
        })
        .collect()
}
