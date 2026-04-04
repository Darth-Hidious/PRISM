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
use crate::hooks::build_default_hooks;
use crate::permissions::ToolPermissionContext;
use crate::prompts::SYSTEM_PROMPT;
use crate::scratchpad::Scratchpad;
use crate::session::SessionStore;
use crate::transcript::TranscriptStore;
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

/// Map an [`AgentEvent`] to the `ui.*` JSON-RPC notifications that the Ink
/// frontend expects.  Event names and schemas MUST match
/// `frontend/src/bridge/types.ts` → `UI_EVENT_MAP`.
fn emit_agent_event(event: AgentEvent) {
    match event {
        AgentEvent::TextDelta { text } => {
            emit_notification("ui.text.delta", serde_json::json!({ "text": text }));
        }
        AgentEvent::ToolCallStart { tool_name, call_id } => {
            // Flush any buffered text before a tool starts
            emit_notification("ui.text.flush", serde_json::json!({ "text": "" }));
            emit_notification(
                "ui.tool.start",
                serde_json::json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                    "verb": format!("Running {tool_name}"),
                }),
            );
        }
        AgentEvent::ToolCallResult {
            call_id,
            tool_name,
            content,
            summary: _,
            elapsed_ms,
            is_error,
        } => {
            // Frontend expects "ui.card" with UiCard schema
            emit_notification(
                "ui.card",
                serde_json::json!({
                    "card_type": if is_error { "error" } else { "results" },
                    "tool_name": tool_name,
                    "elapsed_ms": elapsed_ms,
                    "content": content,
                    "data": { "call_id": call_id },
                }),
            );
        }
        AgentEvent::ToolApprovalRequest {
            tool_name,
            call_id: _,
            tool_args,
        } => {
            // Frontend expects "ui.prompt" with UiPrompt schema
            emit_notification(
                "ui.prompt",
                serde_json::json!({
                    "prompt_type": "approval",
                    "message": format!("Allow {}?", tool_name),
                    "choices": ["y", "n", "a"],
                    "tool_name": tool_name,
                    "tool_args": tool_args,
                }),
            );
        }
        AgentEvent::TurnComplete {
            text: _,
            has_more: _,
            usage,
            total_usage: _,
            estimated_cost,
        } => {
            // Emit ui.cost before ui.turn.complete (frontend expects both)
            let (input_tokens, output_tokens) = usage
                .as_ref()
                .map(|u| (u.input_tokens, u.output_tokens))
                .unwrap_or((0, 0));
            emit_notification(
                "ui.cost",
                serde_json::json!({
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "turn_cost": estimated_cost.unwrap_or(0.0),
                    "session_cost": estimated_cost.unwrap_or(0.0),
                }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
        }
    }
}

// ── Command handlers ──────────────────────────────────────────────

/// Handle built-in slash commands.  Returns `true` if the command was handled.
fn handle_command(
    command: &str,
    session_store: &mut SessionStore,
    history: &mut Vec<ChatMessage>,
    llm_config: &mut LlmConfig,
) -> bool {
    let trimmed = command.trim();

    match trimmed {
        "/tools" => {
            emit_notification(
                "ui.text.delta",
                serde_json::json!({ "text": "Use /tools in the REPL to list available tools." }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        "/clear" => {
            history.clear();
            emit_notification(
                "ui.text.delta",
                serde_json::json!({ "text": "Conversation cleared." }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        "/help" => {
            let help_text = "\
Commands:
  /tools                       — List available tools
  /clear                       — Clear conversation history
  /sessions                    — List saved sessions
  /session resume [id|latest]  — Resume a session
  /session fork [name]         — Fork current session
  /model [id]                  — Show or switch LLM model
  /help                        — Show this help";
            emit_notification("ui.text.delta", serde_json::json!({ "text": help_text }));
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        "/sessions" => {
            let sessions = session_store.list_sessions(20);
            if sessions.is_empty() {
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": "No saved sessions." }),
                );
            } else {
                let mut lines = vec!["Sessions:".to_string()];
                for s in &sessions {
                    let latest_marker = if s.is_latest { " (latest)" } else { "" };
                    lines.push(format!(
                        "  {} — {} turns, model: {}, {:.1}KB{}",
                        s.session_id, s.turn_count, s.model, s.size_kb, latest_marker
                    ));
                }
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": lines.join("\n") }),
                );
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        "/model" => {
            emit_notification(
                "ui.text.delta",
                serde_json::json!({ "text": format!("Current model: {}", llm_config.model) }),
            );
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        _ if trimmed.starts_with("/model ") => {
            let new_model = trimmed.strip_prefix("/model ").unwrap().trim();
            if new_model.is_empty() {
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": format!("Current model: {}", llm_config.model) }),
                );
            } else {
                let old = &llm_config.model;
                emit_notification(
                    "ui.text.delta",
                    serde_json::json!({ "text": format!("Model switched: {} → {}", old, new_model) }),
                );
                llm_config.model = new_model.to_string();
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        _ if trimmed.starts_with("/session resume") => {
            let rest = trimmed.strip_prefix("/session resume").unwrap().trim();
            let reference = if rest.is_empty() { "latest" } else { rest };
            match session_store.resume_session(reference) {
                Some((sid, messages)) => {
                    // Repopulate history from session messages
                    history.clear();
                    for msg in &messages {
                        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                        if !role.is_empty() && !content.is_empty() {
                            history.push(ChatMessage {
                                role: role.to_string(),
                                content: Some(content.to_string()),
                                tool_calls: None,
                                tool_call_id: None,
                            });
                        }
                    }
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({
                            "text": format!("Resumed session {} ({} messages)", sid, messages.len())
                        }),
                    );
                }
                None => {
                    emit_notification(
                        "ui.text.delta",
                        serde_json::json!({ "text": format!("Session not found: {}", reference) }),
                    );
                }
            }
            emit_notification("ui.turn.complete", serde_json::json!({}));
            true
        }
        _ if trimmed.starts_with("/session fork") => {
            let name = trimmed.strip_prefix("/session fork").unwrap().trim();
            let new_id = session_store.fork_session(name);
            emit_notification(
                "ui.text.delta",
                serde_json::json!({
                    "text": format!("Forked to new session: {}", new_id)
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
pub async fn run_server(mut llm_config: LlmConfig, tool_server_config: ToolServer) -> Result<()> {
    let llm = LlmClient::new(llm_config.clone());

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
    let mut transcript = TranscriptStore::new(None);
    let hooks = build_default_hooks();
    let permissions = ToolPermissionContext::default();
    let mut scratchpad = Scratchpad::new();

    // Session persistence
    let mut session_store = SessionStore::new(None);
    let session_id = session_store.new_session(&llm_config.model);

    // Approval channel — protocol sends responses, agent loop receives
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::channel::<agent_loop::ApprovalResponse>(1);

    // OPA policy engine — loads built-in + user/project policies
    let mut policy_engine = match prism_policy::PolicyEngine::with_discovery(None) {
        Ok(pe) => {
            tracing::info!(policies = pe.policy_count(), "OPA policy engine loaded");
            Some(pe)
        }
        Err(e) => {
            tracing::warn!(error = %e, "OPA policy engine failed to load — running without policies");
            None
        }
    };
    tracing::info!(session_id = %session_id, "started new session");

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

        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "init" => {
                // Check if init requests session resume
                let resume_ref = params.get("resume").and_then(|v| v.as_str()).unwrap_or("");

                let mut welcome = serde_json::json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "tool_count": tools.len(),
                    "session_id": session_store.current_id().unwrap_or(""),
                });

                if !resume_ref.is_empty() {
                    if let Some((sid, messages)) = session_store.resume_session(resume_ref) {
                        // Repopulate history from resumed session
                        history.clear();
                        for msg in &messages {
                            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                            let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
                            if !role.is_empty() && !content.is_empty() {
                                history.push(ChatMessage {
                                    role: role.to_string(),
                                    content: Some(content.to_string()),
                                    tool_calls: None,
                                    tool_call_id: None,
                                });
                            }
                        }
                        welcome["resumed"] = serde_json::json!(true);
                        welcome["session_id"] = serde_json::json!(sid);
                        welcome["resumed_messages"] = serde_json::json!(messages.len());
                        tracing::info!(
                            session_id = %sid,
                            messages = messages.len(),
                            "resumed session"
                        );
                    }
                }

                emit_response(id, serde_json::json!({ "status": "ok" }));
                emit_notification("ui.welcome", welcome);
                emit_notification(
                    "ui.status",
                    serde_json::json!({
                        "auto_approve": config.auto_approve,
                        "message_count": 0,
                        "has_plan": false,
                    }),
                );
            }

            "input.message" => {
                let text = params.get("text").and_then(|t| t.as_str()).unwrap_or("");

                if text.is_empty() {
                    emit_error(-32602, "Missing params.text", id);
                    continue;
                }

                emit_response(id, serde_json::json!({ "status": "ok" }));

                // Persist user message
                session_store.append_message("user", text, "", "", None);

                match agent_loop::run_turn(
                    &llm,
                    &mut tool_server,
                    &mut history,
                    &tools,
                    &config,
                    text,
                    &mut transcript,
                    &hooks,
                    &permissions,
                    &mut scratchpad,
                    &mut |event| {
                        // Persist assistant text and tool results as they flow through
                        match &event {
                            AgentEvent::TurnComplete { text: Some(t), .. } if !t.is_empty() => {
                                session_store.append_message("assistant", t, "", "", None);
                            }
                            AgentEvent::ToolCallResult {
                                call_id,
                                tool_name,
                                content,
                                ..
                            } => {
                                session_store
                                    .append_message("tool", content, tool_name, call_id, None);
                            }
                            _ => {}
                        }
                        emit_agent_event(event);
                    },
                    Some(&mut approval_rx),
                    policy_engine.as_mut(),
                )
                .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::error!(error = %e, "agent turn failed");
                        emit_notification(
                            "ui.text.delta",
                            serde_json::json!({ "text": format!("Error: {e}") }),
                        );
                        emit_notification("ui.turn.complete", serde_json::json!({}));
                    }
                }
            }

            "input.command" => {
                let command = params.get("command").and_then(|c| c.as_str()).unwrap_or("");

                emit_response(id.clone(), serde_json::json!({ "status": "ok" }));

                // If not a known command, treat as agent message
                if !handle_command(command, &mut session_store, &mut history, &mut llm_config) {
                    let text = command.trim_start_matches('/');
                    session_store.append_message("user", text, "", "", None);
                    if let Err(e) = agent_loop::run_turn(
                        &llm,
                        &mut tool_server,
                        &mut history,
                        &tools,
                        &config,
                        text,
                        &mut transcript,
                        &hooks,
                        &permissions,
                        &mut scratchpad,
                        &mut emit_agent_event,
                        Some(&mut approval_rx),
                        policy_engine.as_mut(),
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
                let response_str = params
                    .get("response")
                    .and_then(|r| r.as_str())
                    .unwrap_or("n");
                let approval = match response_str {
                    "y" | "yes" | "allow" => agent_loop::ApprovalResponse::Allow,
                    "a" | "all" | "always" => agent_loop::ApprovalResponse::AllowAll,
                    _ => agent_loop::ApprovalResponse::Deny,
                };
                let _ = approval_tx.try_send(approval);
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
