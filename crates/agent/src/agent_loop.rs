//! Full TAOR (Think-Act-Observe-Repeat) agent loop.
//!
//! Integrates: transcript, hooks, permissions, scratchpad, cost tracking,
//! doom-loop detection, large-result handling, and auto-compaction.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use prism_ingest::llm::{ChatMessage, LlmClient, ToolDefinition};
use prism_python_bridge::tool_server::ToolServerHandle;
use serde_json::Value;

use crate::command_tools::{self, CommandToolRuntime};
use crate::hooks::HookRegistry;
use crate::models::{estimate_cost, get_model_config};
use crate::permissions::{SharedPermissionOverrides, ToolPermissionContext};
use crate::scratchpad::Scratchpad;
use crate::tool_catalog::ToolCatalog;
use crate::transcript::{TranscriptEntry, TranscriptStore};
use crate::types::{AgentConfig, AgentEvent, UsageInfo};

/// Approval response from the TUI/frontend.
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalResponse {
    /// User approved this single tool call.
    Allow,
    /// User denied this tool call.
    Deny,
    /// User approved all remaining tool calls (auto-approve).
    AllowAll,
}

/// Channel-based gate for tool approval.
/// The protocol layer sends responses through this when the TUI replies.
pub type ApprovalSender = tokio::sync::mpsc::Sender<ApprovalResponse>;
pub type ApprovalReceiver = tokio::sync::mpsc::Receiver<ApprovalResponse>;
pub type SharedApprovalReceiver = Arc<tokio::sync::Mutex<ApprovalReceiver>>;

// ── Constants ─────────────────────────────────────────────────────

const MAX_TOOL_RESULT_CHARS: usize = 30_000;
const DOOM_LOOP_WINDOW: usize = 3;

// ── Large-result handling ─────────────────────────────────────────

fn uuid_hex8() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:08x}", (ts ^ (ts >> 32)) & 0xFFFF_FFFF)
}

fn process_large_result(content: &str, result_store: &mut HashMap<String, String>) -> String {
    if content.len() <= MAX_TOOL_RESULT_CHARS {
        return content.to_string();
    }
    let result_id = uuid_hex8();
    result_store.insert(result_id.clone(), content.to_string());
    let end = content.len().min(2000);
    let truncated = &content[..end];
    format!("{truncated}\n\n[Result truncated. Use peek_result('{result_id}') to see more]")
}

// ── Doom-loop detection ───────────────────────────────────────────

fn doom_loop_signature(tool_name: &str, args: &Value) -> String {
    let args_str = serde_json::to_string(args).unwrap_or_default();
    format!("{tool_name}:{args_str}")
}

fn check_doom_loop(recent: &VecDeque<String>, sig: &str) -> bool {
    if recent.len() < DOOM_LOOP_WINDOW {
        return false;
    }
    recent.iter().rev().take(DOOM_LOOP_WINDOW).all(|s| s == sig)
}

// ── Summarize tool result ─────────────────────────────────────────

fn tool_preview(tool_name: &str, args: &Value) -> Option<String> {
    if let Some(preview) = command_tools::command_tool_preview(tool_name, args) {
        return Some(preview);
    }

    match tool_name {
        "read_file" => args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("read {}", path)),
        "edit_file" => args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("edit {}", path)),
        "write_file" => args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("write {}", path)),
        "execute_bash" => args
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|command| format!("$ {}", command.lines().next().unwrap_or(command))),
        "read_bash_task" | "stop_bash_task" => args
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(|task_id| format!("{tool_name}: {task_id}")),
        "execute_python" => args
            .get("description")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|description| format!("python: {description}"))
            .or_else(|| {
                args.get("code")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|code| format!("python: {}", code.lines().next().unwrap_or(code)))
            }),
        _ => None,
    }
}

fn summarize_tool_result(
    tool_name: &str,
    preview: Option<&str>,
    content: &str,
    is_error: bool,
) -> String {
    if is_error {
        let preview = if content.len() > 60 {
            &content[..60]
        } else {
            content
        };
        return format!("{tool_name}: error — {preview}");
    }
    // Try to parse as JSON for richer summaries
    if let Ok(val) = serde_json::from_str::<Value>(content) {
        if let Some(path) = val.get("path").and_then(|v| v.as_str()) {
            let size_bytes = val.get("size_bytes").and_then(|v| v.as_u64());
            return match tool_name {
                "read_file" => size_bytes
                    .map(|size| format!("read_file: {path} ({size} bytes)"))
                    .unwrap_or_else(|| format!("read_file: {path}")),
                "edit_file" => {
                    let replacements = val.get("replacements").and_then(|v| v.as_u64());
                    match (replacements, size_bytes) {
                        (Some(replacements), Some(size)) => {
                            format!("edit_file: {path} ({replacements} replacements, {size} bytes)")
                        }
                        (Some(replacements), None) => {
                            format!("edit_file: {path} ({replacements} replacements)")
                        }
                        _ => format!("edit_file: {path}"),
                    }
                }
                "write_file" => size_bytes
                    .map(|size| format!("write_file: {path} ({size} bytes)"))
                    .unwrap_or_else(|| format!("write_file: {path}")),
                _ => format!("{tool_name}: {path}"),
            };
        }
        if let Some(count) = val.get("count").and_then(|v| v.as_u64()) {
            return format!("{tool_name}: {count} results");
        }
        if let Some(task) = val.get("task") {
            if let Some(task_id) = task.get("task_id").and_then(|value| value.as_str()) {
                let status = task
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                return format!("{tool_name}: {task_id} ({status})");
            }
        }
        if let Some(arr) = val.get("results").and_then(|v| v.as_array()) {
            return format!("{tool_name}: {} results", arr.len());
        }
        if let Some(arr) = val.get("tasks").and_then(|v| v.as_array()) {
            return format!("{tool_name}: {} tasks", arr.len());
        }
        if let Some(f) = val.get("filename").and_then(|v| v.as_str()) {
            return format!("{tool_name}: saved to {f}");
        }
        if let Some(root) = val.get("root").and_then(|v| v.as_str()) {
            if let Some(stdout) = val.get("stdout").and_then(|v| v.as_str()) {
                if let Ok(parsed_stdout) = serde_json::from_str::<Value>(stdout.trim()) {
                    match root {
                        "models" => {
                            if let Some(items) = parsed_stdout.as_array() {
                                return format!("{tool_name}: {} models", items.len());
                            }
                            if let Some(model_id) = parsed_stdout
                                .get("model_id")
                                .and_then(|value| value.as_str())
                            {
                                return format!("{tool_name}: {model_id}");
                            }
                        }
                        "deploy" => {
                            if let Some(items) = parsed_stdout.as_array() {
                                return format!("{tool_name}: {} deployments", items.len());
                            }
                            if let Some(status) =
                                parsed_stdout.get("status").and_then(|value| value.as_str())
                            {
                                let deployment_id = parsed_stdout
                                    .get("deployment_id")
                                    .or_else(|| parsed_stdout.get("id"))
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("deployment");
                                return format!("{tool_name}: {deployment_id} ({status})");
                            }
                            if let Some(healthy) =
                                parsed_stdout.get("healthy").and_then(|value| value.as_bool())
                            {
                                return format!("{tool_name}: healthy={healthy}");
                            }
                        }
                        "discourse" => {
                            if let Some(items) =
                                parsed_stdout.get("specs").and_then(|value| value.as_array())
                            {
                                return format!("{tool_name}: {} specs", items.len());
                            }
                            if let Some(events) =
                                parsed_stdout.get("events").and_then(|value| value.as_array())
                            {
                                let instance_id = parsed_stdout
                                    .get("instance_id")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("instance");
                                return format!("{tool_name}: {instance_id} ({} events)", events.len());
                            }
                            if let Some(status) =
                                parsed_stdout.get("status").and_then(|value| value.as_str())
                            {
                                let instance_id = parsed_stdout
                                    .get("instance_id")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("instance");
                                return format!("{tool_name}: {instance_id} ({status})");
                            }
                            if let Some(turns) =
                                parsed_stdout.get("turns").and_then(|value| value.as_array())
                            {
                                return format!("{tool_name}: {} turns", turns.len());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(invocation) = val.get("invocation").and_then(|v| v.as_str()) {
            let timed_out = val
                .get("timed_out")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let exit_code = val.get("exit_code").and_then(|v| v.as_i64());
            if timed_out {
                return format!("{tool_name}: timed out — {invocation}");
            }
            if let Some(exit_code) = exit_code {
                if exit_code != 0 {
                    return format!("{tool_name}: exit {exit_code} — {invocation}");
                }
            }
            return format!("{tool_name}: {invocation}");
        }
    }
    if let Some(preview) = preview {
        return preview.to_string();
    }
    format!("{tool_name}: completed")
}

pub(crate) fn compact_history(history: &mut Vec<ChatMessage>, summary: &str, keep_last: usize) {
    if history.len() <= keep_last {
        return;
    }

    let split_at = history.len().saturating_sub(keep_last);
    let recent = history.split_off(split_at);
    history.clear();
    history.push(ChatMessage {
        role: "system".to_string(),
        content: Some(format!("[Conversation context compacted]\n{summary}")),
        tool_calls: None,
        tool_call_id: None,
    });
    history.extend(recent);
}

// ── Main turn loop ────────────────────────────────────────────────

/// Run a single conversational turn through the full TAOR pipeline.
///
/// Flow:
/// 1. Push user message to history + transcript
/// 2. Loop up to `max_iterations`:
///    a. Budget check (warn / exhaust)
///    b. Build messages = system_prompt + history
///    c. Call LLM with tools
///    d. Track usage
///    e. Emit text deltas
///    f. If no tool calls → compact if needed, emit TurnComplete, return
///    g. For each tool call → hooks, permissions, approval, execute, doom-loop,
///    large-result handling, scratchpad, transcript, emit result
/// 3. If max_iterations reached → emit warning + TurnComplete
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    llm: &LlmClient,
    tool_server: &mut ToolServerHandle,
    command_tool_runtime: &CommandToolRuntime,
    history: &mut Vec<ChatMessage>,
    tool_catalog: &ToolCatalog,
    config: &AgentConfig,
    user_message: &str,
    transcript: &mut TranscriptStore,
    hooks: &HookRegistry,
    permissions: &ToolPermissionContext,
    live_permission_overrides: Option<SharedPermissionOverrides>,
    scratchpad: &mut Scratchpad,
    emit: &mut (dyn FnMut(AgentEvent) + Send),
    approval_rx: Option<SharedApprovalReceiver>,
    mut policy: Option<&mut prism_policy::PolicyEngine>,
) -> Result<()> {
    // ── 1. Push user message ──────────────────────────────────────
    history.push(ChatMessage {
        role: "user".to_string(),
        content: Some(user_message.to_string()),
        tool_calls: None,
        tool_call_id: None,
    });
    transcript.append(TranscriptEntry::new("user", user_message));

    let mut total_usage = UsageInfo::default();
    let mut result_store: HashMap<String, String> = HashMap::new();
    let mut recent_sigs: VecDeque<String> = VecDeque::with_capacity(DOOM_LOOP_WINDOW + 1);

    // ── 2. TAOR iteration loop ────────────────────────────────────
    for _iteration in 0..config.max_iterations {
        // ── 2a. Budget check ──────────────────────────────────────
        if let Some(warning) = transcript.budget_warning() {
            emit(AgentEvent::TextDelta {
                text: format!("\n[{warning}]\n"),
            });
        }
        if transcript.budget_exhausted() {
            emit(AgentEvent::TextDelta {
                text: "Budget exhausted.".to_string(),
            });
            emit(AgentEvent::TurnComplete {
                text: Some("Budget exhausted.".to_string()),
                has_more: false,
                usage: None,
                total_usage: Some(total_usage),
                estimated_cost: None,
            });
            return Ok(());
        }

        // ── 2b. Build messages ────────────────────────────────────
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(config.system_prompt.clone()),
            tool_calls: None,
            tool_call_id: None,
        }];
        messages.extend(history.iter().cloned());

        // ── 2c. Call LLM (with streaming) ────────────────────────
        let mut streaming_deltas: Vec<String> = Vec::new();
        let response = llm
            .chat_with_tools_streaming(&messages, tool_catalog.definitions(), |delta| {
                streaming_deltas.push(delta.to_string());
            })
            .await
            .context("LLM call failed")?;

        // ── 2d. Track usage ───────────────────────────────────────
        if let Some(usage) = &response.usage {
            total_usage += UsageInfo {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            };
            transcript.record_cost("llm_turn", usage.prompt_tokens, usage.completion_tokens);
        }

        // ── 2e. Emit streamed text deltas ─────────────────────────
        for delta in &streaming_deltas {
            emit(AgentEvent::TextDelta {
                text: delta.clone(),
            });
        }

        // ── 2f. Push assistant message ────────────────────────────
        history.push(response.message.clone());

        // ── 2g. Check for tool calls ──────────────────────────────
        let tool_calls = match &response.message.tool_calls {
            Some(calls) if !calls.is_empty() => calls.clone(),
            _ => {
                // No tool calls → turn complete

                // Auto-compact if needed
                if transcript.should_compact() {
                    if let Some(summary) = transcript.compact(6) {
                        compact_history(history, &summary, 6);
                    }
                }

                // Record assistant message in transcript
                if let Some(text) = &response.message.content {
                    transcript.append(TranscriptEntry::new("assistant", text.as_str()));
                }

                // Calculate cost
                let model_cfg = get_model_config(&config.model);
                let estimated_cost = estimate_cost(&total_usage, &model_cfg);

                emit(AgentEvent::TurnComplete {
                    text: response.message.content.clone(),
                    has_more: false,
                    usage: response.usage.as_ref().map(|u| UsageInfo {
                        input_tokens: u.prompt_tokens,
                        output_tokens: u.completion_tokens,
                        cache_creation_tokens: 0,
                        cache_read_tokens: 0,
                    }),
                    total_usage: Some(total_usage),
                    estimated_cost: Some(estimated_cost),
                });
                return Ok(());
            }
        };

        // ── 2h. Process each tool call ────────────────────────────
        for tc in &tool_calls {
            let tool_name = &tc.function.name;
            let call_id = &tc.id;

            let args: Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
            let preview = tool_preview(tool_name, &args);

            // ── h1. Emit ToolCallStart ────────────────────────────
            emit(AgentEvent::ToolCallStart {
                tool_name: tool_name.clone(),
                call_id: call_id.clone(),
                preview: preview.clone(),
            });

            // ── h2. Fire pre-hooks ────────────────────────────────
            let pre_result = hooks.fire_before(tool_name, &args);
            if pre_result.abort {
                let error_msg = format!("Blocked by hook: {}", pre_result.reason);
                emit(AgentEvent::ToolCallResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    content: error_msg.clone(),
                    summary: Some(format!("{tool_name}: blocked by hook")),
                    preview: preview.clone(),
                    elapsed_ms: 0,
                    is_error: true,
                });
                history.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(error_msg),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                });
                continue;
            }

            // ── h3. Check permissions ─────────────────────────────
            let permission_decision = if let Some(overrides) = live_permission_overrides.as_ref() {
                // Session-level allow/block edits can arrive while the turn is
                // still running, so each tool checks the latest shared view.
                let overrides = overrides.read().await;
                permissions.decision_for(tool_name, Some(&overrides))
            } else {
                permissions.decision_for(tool_name, None)
            };

            if permission_decision.blocked {
                let error_msg = format!("Tool '{tool_name}' is blocked by permission policy.");
                emit(AgentEvent::ToolCallResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    content: error_msg.clone(),
                    summary: Some(format!("{tool_name}: blocked by permissions")),
                    preview: preview.clone(),
                    elapsed_ms: 0,
                    is_error: true,
                });
                history.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(error_msg),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                });
                continue;
            }

            // ── h4. OPA policy check ──────────────────────────────
            if let Some(ref mut pe) = policy {
                let policy_input = prism_policy::PolicyInput {
                    action: "tool.call".to_string(),
                    principal: "agent".to_string(),
                    role: "agent".to_string(),
                    resource: tool_name.clone(),
                    context: args.clone(),
                };
                match pe.evaluate(&policy_input) {
                    Ok(decision) if !decision.allowed => {
                        let reason = if decision.violations.is_empty() {
                            decision.reason
                        } else {
                            decision.violations.join("; ")
                        };
                        let denied_msg =
                            format!("Tool '{tool_name}' denied by OPA policy: {reason}");
                        emit(AgentEvent::ToolCallResult {
                            call_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            content: denied_msg.clone(),
                            summary: Some(format!("{tool_name}: denied by policy")),
                            preview: preview.clone(),
                            elapsed_ms: 0,
                            is_error: true,
                        });
                        history.push(ChatMessage {
                            role: "tool".to_string(),
                            content: Some(denied_msg),
                            tool_calls: None,
                            tool_call_id: Some(call_id.clone()),
                        });
                        continue;
                    }
                    Ok(decision) => {
                        // Log obligations (e.g. "audit_log")
                        for obligation in &decision.obligations {
                            tracing::info!(
                                tool = %tool_name,
                                obligation = %obligation,
                                "OPA policy obligation"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            tool = %tool_name,
                            error = %e,
                            "OPA policy evaluation failed — allowing (fail-open)"
                        );
                    }
                }
            }

            // ── h5. Approval gate ─────────────────────────────────
            if !config.auto_approve && !permission_decision.auto_approved {
                let tool_meta = tool_catalog.find(tool_name);
                // Feed the TUI the loaded tool metadata so approval prompts can
                // explain *why* something like execute_bash is gated.
                emit(AgentEvent::ToolApprovalRequest {
                    tool_name: tool_name.clone(),
                    tool_args: args.clone(),
                    call_id: call_id.clone(),
                    tool_description: tool_meta.map(|tool| tool.description.clone()),
                    requires_approval: tool_meta.map(|tool| tool.requires_approval).unwrap_or(false),
                    permission_mode: tool_meta
                        .map(|tool| tool.permission_mode.as_str().to_string())
                        .unwrap_or_else(|| "workspace-write".to_string()),
                });

                // Wait for approval from TUI (if approval channel is wired)
                if let Some(rx) = approval_rx.as_ref() {
                    // Turn execution now runs outside the stdin loop, so the
                    // approval receiver must be shared across the spawned turn.
                    let mut rx = rx.lock().await;
                    match rx.recv().await {
                        Some(ApprovalResponse::Allow) => {
                            // Proceed with this tool call
                        }
                        Some(ApprovalResponse::AllowAll) => {
                            // Switch to auto-approve for rest of session
                            // (handled by not entering this branch again)
                        }
                        Some(ApprovalResponse::Deny) | None => {
                            let denied_msg = format!("Tool '{tool_name}' denied by user.");
                            emit(AgentEvent::ToolCallResult {
                                call_id: call_id.clone(),
                                tool_name: tool_name.clone(),
                                content: denied_msg.clone(),
                                summary: Some(format!("{tool_name}: denied")),
                                preview: preview.clone(),
                                elapsed_ms: 0,
                                is_error: true,
                            });
                            history.push(ChatMessage {
                                role: "tool".to_string(),
                                content: Some(denied_msg),
                                tool_calls: None,
                                tool_call_id: Some(call_id.clone()),
                            });
                            continue;
                        }
                    }
                }
                // If no approval channel, auto-approve (backward compat)
            }

            // ── h5. Execute tool ──────────────────────────────────
            let start = Instant::now();
            let result: Result<Value> = if command_tools::is_command_tool(tool_name) {
                command_tools::execute_command_tool(
                    command_tool_runtime,
                    tool_name,
                    &args,
                    policy.as_deref_mut(),
                )
                    .await
                    .map(|value| serde_json::json!({ "result": value }))
            } else {
                tool_server.call_tool(tool_name, args.clone()).await.map_err(Into::into)
            };
            let elapsed_ms = start.elapsed().as_millis() as u64;

            let (raw_content, is_error): (String, bool) = match result {
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

            // ── h6. Fire post-hooks ───────────────────────────────
            let result_value: Value = serde_json::from_str(&raw_content)
                .unwrap_or_else(|_| Value::String(raw_content.to_string()));
            let post_result = hooks.fire_after(tool_name, &args, &result_value, elapsed_ms as f64);
            let content_after_hooks = if post_result != result_value {
                serde_json::to_string(&post_result).unwrap_or(raw_content.to_string())
            } else {
                raw_content.to_string()
            };

            // ── h7. Doom-loop detection ───────────────────────────
            let sig = doom_loop_signature(tool_name, &args);
            recent_sigs.push_back(sig.clone());
            if recent_sigs.len() > DOOM_LOOP_WINDOW {
                recent_sigs.pop_front();
            }
            if check_doom_loop(&recent_sigs, &sig) {
                let abort_msg = format!(
                    "DOOM LOOP DETECTED: {tool_name} called {} times with identical arguments. \
                     Try a different approach or ask the user for help.",
                    DOOM_LOOP_WINDOW
                );
                emit(AgentEvent::ToolCallResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    content: abort_msg.clone(),
                    summary: Some(format!("{tool_name}: doom loop aborted")),
                    preview: preview.clone(),
                    elapsed_ms,
                    is_error: true,
                });
                history.push(ChatMessage {
                    role: "tool".to_string(),
                    content: Some(abort_msg),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                });
                continue;
            }

            // ── h8. Large-result handling ─────────────────────────
            let content = process_large_result(&content_after_hooks, &mut result_store);

            // ── h9. Log to scratchpad ─────────────────────────────
            let summary = summarize_tool_result(tool_name, preview.as_deref(), &content, is_error);
            scratchpad.log(
                "tool_call",
                Some(tool_name.as_str()),
                &summary,
                Some(serde_json::json!({
                    "args": args,
                    "elapsed_ms": elapsed_ms,
                    "is_error": is_error,
                })),
            );

            // ── h10. Record cost ──────────────────────────────────
            transcript.record_cost(format!("tool:{tool_name}"), 0, 0);

            // ── h11. Emit ToolCallResult ──────────────────────────
            emit(AgentEvent::ToolCallResult {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                content: content.clone(),
                summary: Some(summary),
                preview,
                elapsed_ms,
                is_error,
            });

            // ── h12. Push tool result to history ──────────────────
            history.push(ChatMessage {
                role: "tool".to_string(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
            });

            // ── h13. Append to transcript ─────────────────────────
            transcript
                .append(TranscriptEntry::new("tool", &content).with_tool_name(tool_name.as_str()));
        }

        // ── 2i. Loop back ─────────────────────────────────────────
    }

    // ── 3. Max iterations reached ─────────────────────────────────
    emit(AgentEvent::TextDelta {
        text: "\n\n[Agent reached maximum iterations]".to_string(),
    });

    let model_cfg = get_model_config(&config.model);
    let estimated_cost = estimate_cost(&total_usage, &model_cfg);

    emit(AgentEvent::TurnComplete {
        text: None,
        has_more: false,
        usage: None,
        total_usage: Some(total_usage),
        estimated_cost: Some(estimated_cost),
    });
    Ok(())
}

// ── tools_to_definitions ──────────────────────────────────────────

/// Backward-compatible helper for call sites that still only need plain tool
/// definitions. The richer runtime path should prefer `ToolCatalog`.
pub fn tools_to_definitions(tools_json: &serde_json::Value) -> Vec<ToolDefinition> {
    ToolCatalog::from_tool_server_json(tools_json)
        .definitions()
        .to_vec()
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_large_result_small() {
        let mut store = HashMap::new();
        let content = "small result";
        let result = process_large_result(content, &mut store);
        assert_eq!(result, "small result");
        assert!(store.is_empty());
    }

    #[test]
    fn test_process_large_result_large() {
        let mut store = HashMap::new();
        let content = "x".repeat(40_000);
        let result = process_large_result(&content, &mut store);
        assert!(result.contains("[Result truncated"));
        assert!(result.contains("peek_result"));
        assert_eq!(store.len(), 1);
        // Stored value is the full content
        let stored = store.values().next().unwrap();
        assert_eq!(stored.len(), 40_000);
    }

    #[test]
    fn test_doom_loop_detection() {
        let mut recent: VecDeque<String> = VecDeque::new();
        let sig = "tool:{}".to_string();

        // Not enough entries
        recent.push_back(sig.clone());
        assert!(!check_doom_loop(&recent, &sig));

        recent.push_back(sig.clone());
        assert!(!check_doom_loop(&recent, &sig));

        // Now 3 identical
        recent.push_back(sig.clone());
        assert!(check_doom_loop(&recent, &sig));
    }

    #[test]
    fn test_doom_loop_different_sigs() {
        let mut recent: VecDeque<String> = VecDeque::new();
        recent.push_back("tool_a:{}".to_string());
        recent.push_back("tool_b:{}".to_string());
        recent.push_back("tool_a:{}".to_string());
        assert!(!check_doom_loop(&recent, "tool_a:{}"));
    }

    #[test]
    fn test_summarize_tool_result_error() {
        let summary = summarize_tool_result("search", None, "something went wrong", true);
        assert!(summary.contains("error"));
        assert!(summary.contains("search"));
    }

    #[test]
    fn test_summarize_tool_result_with_count() {
        let content = r#"{"count": 42}"#;
        let summary = summarize_tool_result("search", None, content, false);
        assert_eq!(summary, "search: 42 results");
    }

    #[test]
    fn test_summarize_tool_result_with_results_array() {
        let content = r#"{"results": [1, 2, 3]}"#;
        let summary = summarize_tool_result("query", None, content, false);
        assert_eq!(summary, "query: 3 results");
    }

    #[test]
    fn test_summarize_tool_result_with_filename() {
        let content = r#"{"filename": "output.csv"}"#;
        let summary = summarize_tool_result("export", None, content, false);
        assert_eq!(summary, "export: saved to output.csv");
    }

    #[test]
    fn test_summarize_tool_result_generic() {
        let content = r#"{"status": "ok"}"#;
        let summary = summarize_tool_result("run", None, content, false);
        assert_eq!(summary, "run: completed");
    }

    #[test]
    fn test_summarize_tool_result_prefers_execution_preview_when_available() {
        let summary = summarize_tool_result(
            "execute_bash",
            Some("$ cargo test -p prism-agent"),
            r#"{"success": true, "exit_code": 0}"#,
            false,
        );
        assert_eq!(summary, "$ cargo test -p prism-agent");
    }

    #[test]
    fn test_uuid_hex8_format() {
        let id = uuid_hex8();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_doom_loop_signature() {
        let sig = doom_loop_signature("search", &serde_json::json!({"q": "test"}));
        assert!(sig.starts_with("search:"));
        assert!(sig.contains("test"));
    }

    #[test]
    fn test_compact_history_replaces_older_messages_with_summary() {
        let mut history = vec![
            ChatMessage {
                role: "user".to_string(),
                content: Some("one".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("two".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("three".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: Some("four".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        compact_history(&mut history, "summary text", 2);

        assert_eq!(history.len(), 3);
        assert_eq!(history[0].role, "system");
        assert!(history[0]
            .content
            .as_deref()
            .unwrap_or_default()
            .contains("summary text"));
        assert_eq!(history[1].content.as_deref(), Some("three"));
        assert_eq!(history[2].content.as_deref(), Some("four"));
    }

    #[test]
    fn test_tools_to_definitions() {
        let json = serde_json::json!({
            "tools": [
                {
                    "name": "search",
                    "description": "Search for materials",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" }
                        }
                    }
                }
            ]
        });
        let defs = tools_to_definitions(&json);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "search");
    }
}
