//! JSON-RPC 2.0 stdio server for the Ink TUI frontend.
//!
//! Reads JSON-RPC requests from stdin, dispatches them, and emits
//! `ui.*` notifications on stdout. Stdout is the protocol channel
//! so all logging MUST go through `tracing`, never `println!`.

use std::io::{self, BufRead, Write};

use anyhow::{Context, Result};
use prism_ingest::llm::{ChatMessage, LlmClient};
use prism_ingest::LlmConfig;
use prism_python_bridge::tool_server::{ToolServer, ToolServerHandle};
use serde_json::Value;

use crate::agent_loop;
use crate::prompts::SYSTEM_PROMPT;
use crate::types::{AgentConfig, AgentEvent};

// ── Emit helpers ──────────────────────────────────────────────────

fn emit_raw(value: &Value) {
    let line = serde_json::to_string(value).expect("JSON serialization failed");
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn emit_notification(method: &str, params: Value) {
    emit_raw(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }));
}

fn emit_response(id: Value, result: Value) {
    emit_raw(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }));
}

fn emit_error(code: i64, message: &str, id: Value) {
    emit_raw(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    }));
}

/// Map an [`AgentEvent`] to the appropriate `ui.*` JSON-RPC notification.
fn emit_agent_event(event: AgentEvent) {
    match event {
        AgentEvent::TextDelta { text } => {
            emit_notification("ui.text.delta", serde_json::json!({ "text": text }));
        }
        AgentEvent::ToolCallStart {
            tool_name,
            call_id,
        } => {
            emit_notification(
                "ui.tool.start",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                }),
            );
        }
        AgentEvent::ToolCallResult {
            call_id,
            tool_name,
            content,
            summary,
            elapsed_ms,
            is_error,
        } => {
            emit_notification(
                "ui.tool.result",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                    "content": content,
                    "summary": summary,
                    "elapsed_ms": elapsed_ms,
                    "is_error": is_error,
                }),
            );
        }
        AgentEvent::ToolApprovalRequest {
            tool_name,
            call_id,
            tool_args,
        } => {
            emit_notification(
                "ui.approval.required",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                    "tool_args": tool_args,
                }),
            );
        }
        AgentEvent::TurnComplete {
            text,
            has_more,
            usage,
            total_usage,
            estimated_cost,
        } => {
            emit_notification(
                "ui.turn.complete",
                serde_json::json!({
                    "text": text,
                    "has_more": has_more,
                    "usage": usage,
                    "total_usage": total_usage,
                    "estimated_cost": estimated_cost,
                }),
            );
        }
    }
}

// ── Command handlers ──────────────────────────────────────────────

fn handle_command(command: &str) -> bool {
    match command.trim() {
        "/tools" => {
            emit_notification(
                "ui.text.delta",
                serde_json::json!({ "text": "Use /tools in the REPL to list available tools." }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        "/clear" => {
            emit_notification("ui.clear", serde_json::json!({}));
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        "/help" => {
            emit_notification(
                "ui.text.delta",
                serde_json::json!({
                    "text": "Available commands:\n  /tools  - list tools\n  /clear  - clear history\n  /help   - show this help"
                }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        _ => false, // unknown command — treat as agent message
    }
}

// ── Main server loop ──────────────────────────────────────────────

/// Run the JSON-RPC stdio server. Blocks until stdin is closed.
pub async fn run_server(
    llm_config: LlmConfig,
    tool_server_config: ToolServer,
) -> Result<()> {
    let llm = LlmClient::new(llm_config);

    tracing::info!("spawning python tool server");
    let mut tool_server: ToolServerHandle = tool_server_config
        .spawn()
        .await
        .context("failed to spawn tool server")?;

    // Fetch tool definitions from Python
    let tools_json = tool_server
        .list_tools()
        .await
        .context("failed to list tools")?;
    let tools = agent_loop::tools_to_definitions(&tools_json);
    tracing::info!(tool_count = tools.len(), "loaded tool definitions");

    let config = AgentConfig {
        system_prompt: SYSTEM_PROMPT.to_string(),
        ..Default::default()
    };

    let mut history: Vec<ChatMessage> = Vec::new();

    // Read JSON-RPC lines from stdin
    let stdin = io::stdin();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // stdin closed
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                emit_error(-32700, &format!("Parse error: {e}"), Value::Null);
                continue;
            }
        };

        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "init" => {
                emit_response(id, serde_json::json!({ "status": "ok" }));
                emit_notification(
                    "ui.welcome",
                    serde_json::json!({
                        "version": env!("CARGO_PKG_VERSION"),
                        "tool_count": tools.len(),
                    }),
                );
            }

            "input.message" => {
                let text = params
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                if text.is_empty() {
                    emit_error(-32602, "Missing params.text", id);
                    continue;
                }

                emit_response(id, serde_json::json!({ "status": "ok" }));

                if let Err(e) = agent_loop::run_turn(
                    &llm,
                    &mut tool_server,
                    &mut history,
                    &tools,
                    &config,
                    text,
                    emit_agent_event,
                )
                .await
                {
                    tracing::error!(error = %e, "agent turn failed");
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({ "text": format!("Error: {e}") }),
                    );
                    emit_notification("ui.turn.complete", serde_json::json!({}));
                }
            }

            "input.command" => {
                let command = params
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");

                emit_response(id.clone(), serde_json::json!({ "status": "ok" }));

                // If not a known command, treat as agent message
                if !handle_command(command) {
                    let text = command.trim_start_matches('/');
                    if let Err(e) = agent_loop::run_turn(
                        &llm,
                        &mut tool_server,
                        &mut history,
                        &tools,
                        &config,
                        text,
                        emit_agent_event,
                    )
                    .await
                    {
                        tracing::error!(error = %e, "agent turn failed");
                        emit_notification(
                            "ui.text.delta",
                            serde_json::json!({ "text": format!("Error: {e}") }),
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                    }
                }
            }

            "input.prompt_response" => {
                // TODO: approval flow — forward response to pending approval queue
                emit_response(id, serde_json::json!({ "status": "ok" }));
            }

            _ => {
                emit_error(-32601, &format!("Method not found: {method}"), id);
            }
        }
    }

    tracing::info!("stdin closed, shutting down");
    let _ = tool_server.shutdown().await;
    Ok(())
}
