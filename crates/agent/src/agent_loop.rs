//! Full TAOR (Think-Act-Observe-Repeat) agent loop.
//!
//! Integrates: transcript, hooks, permissions, scratchpad, cost tracking,
//! doom-loop detection, large-result handling, and auto-compaction.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use prism_embed::EmbedBackend;
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
/// How many consecutive empty results from the same tool before we stop
const EMPTY_RESULT_MAX: usize = 2;

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
    // A 2000-char cliff made every oversized result look identical to the
    // agent (same first entries regardless of query) — keep enough of the
    // payload to be distinguishing, and say exactly how much was dropped.
    let mut end = content.len().min(8_000);
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = &content[..end];
    let total = content.len();
    format!(
        "{truncated}\n\n[Showing first {end} of {total} chars — the FULL result is in \
         durable memory; call recall(query=\"<keywords>\") to pull the rest back. \
         Refine the query or lower max_results for a result that fits whole.]"
    )
}

// ── Trajectory injection ──────────────────────────────────────────
//
// Long research turns die when the model forgets (or ignores) what it
// already did — memory tools are opt-in for the LLM, so recall is
// probabilistic. This makes the recent past DETERMINISTIC: the harness
// itself shows the last N executed steps (as one-line pointers, not
// payloads) in a system block every iteration. Owner design 2026-07-05.

const TRAJECTORY_SHOWN_STEPS: usize = 5;

fn trajectory_block(steps: &[String]) -> Option<String> {
    if steps.is_empty() {
        return None;
    }
    let start = steps.len().saturating_sub(TRAJECTORY_SHOWN_STEPS);
    let mut out = String::from(
        "TRAJECTORY — steps already executed this turn (deterministic record). \
         Do not repeat a step that succeeded; build on its result. Truncated \
         results can be expanded with recall(query=\"<keywords>\").\n",
    );
    for (idx, step) in steps.iter().enumerate().skip(start) {
        out.push_str(&format!("  #{} {}\n", idx + 1, step));
    }
    Some(out)
}

// ── Session memory injection (trajectory v2) ──────────────────────
//
// v1 makes the CURRENT turn deterministic; v2 extends the window across
// turns and resumed sessions: at turn start the harness loads this
// session's durable provenance records and injects the most recent ones
// as POINTERS — real record ids the model can expand with recall(id=…) —
// plus the running position ("resuming at step K+1"). Deterministic, not
// recall-dependent: the model doesn't have to remember to ask.

const SESSION_MEMORY_SHOWN: usize = 5;
const SESSION_MEMORY_HINT_CHARS: usize = 100;

/// Compact single-line hint of a JSON value, truncated on a char boundary.
fn compact_json_hint(value: &Value, max_chars: usize) -> String {
    let s = serde_json::to_string(value).unwrap_or_default();
    if s.chars().count() > max_chars {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    } else {
        s
    }
}

fn session_memory_block(records: &[prism_provenance::ProvenanceRecord]) -> Option<String> {
    if records.is_empty() {
        return None;
    }
    let start = records.len().saturating_sub(SESSION_MEMORY_SHOWN);
    let mut out = format!(
        "SESSION MEMORY — {} steps recorded in this session before this turn \
         (durable provenance record, deterministic). You are resuming at step \
         {}. Pointers below are expandable with recall(id=\"<id>\"); do not \
         redo work they already cover.\n",
        records.len(),
        records.len() + 1,
    );
    for (idx, rec) in records.iter().enumerate().skip(start) {
        let tool = rec.tool_name.as_deref().unwrap_or("(no tool)");
        let hint = compact_json_hint(&rec.input_json, SESSION_MEMORY_HINT_CHARS);
        out.push_str(&format!("  step {} [{}] {tool} {hint}\n", idx + 1, rec.id));
    }
    Some(out)
}

/// Load this session's durable records for the SESSION MEMORY block. Any
/// failure (no store, no session id, query error) degrades to `None` — the
/// block is an enhancement and must never fail or stall a turn.
async fn load_session_memory() -> Option<String> {
    let session_id = crate::hooks::provenance_session_id();
    if session_id == "unknown" {
        // No real session context — the "unknown" bucket aggregates
        // unrelated writes, so injecting it would show foreign steps.
        return None;
    }
    let db_path = dirs::home_dir()
        .map(|h| h.join(".prism/provenance.db"))
        .unwrap_or_else(|| std::path::PathBuf::from("provenance.db"));
    match prism_provenance::ProvenanceStore::open(&db_path).await {
        Ok(store) => match store.query_by_session(&session_id).await {
            Ok(records) => session_memory_block(&records),
            Err(e) => {
                tracing::debug!("session memory query failed: {e:#}");
                None
            }
        },
        Err(e) => {
            tracing::debug!("session memory store open failed: {e:#}");
            None
        }
    }
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

/// Returns true if a tool result looks like 0/empty results.
fn is_empty_result(content: &str) -> bool {
    if let Ok(val) = serde_json::from_str::<Value>(content) {
        // {"count": 0} or {"results": []}
        if let Some(count) = val.get("count").and_then(|v| v.as_u64())
            && count == 0
        {
            return true;
        }
        if let Some(results) = val.get("results").and_then(|v| v.as_array())
            && results.is_empty()
        {
            return true;
        }
    }
    false
}

// ── Summarize tool result ─────────────────────────────────────────

fn tool_preview(tool_name: &str, args: &Value) -> Option<String> {
    if let Some(preview) = command_tools::command_tool_preview(tool_name, args) {
        return Some(preview);
    }

    match tool_name {
        // Unified `file` tool: action ∈ read|write|edit, plus `path`.
        "file" => {
            let action = args.get("action").and_then(|value| value.as_str());
            args.get("path")
                .and_then(|value| value.as_str())
                .map(|path| match action {
                    Some(action) if !action.is_empty() => format!("{action} {path}"),
                    _ => path.to_string(),
                })
        }
        // Unified `web` tool: action ∈ read|search, plus `url` / `query`.
        "web" => {
            let action = args
                .get("action")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if action == "read" {
                args.get("url")
                    .and_then(|value| value.as_str())
                    .map(|url| format!("read {url}"))
            } else {
                args.get("query")
                    .and_then(|value| value.as_str())
                    .map(|query| format!("search \"{query}\""))
            }
        }
        "web_search" => args
            .get("query")
            .and_then(|value| value.as_str())
            .map(|query| format!("search \"{query}\"")),
        "web_read" | "web_fetch" => args
            .get("url")
            .and_then(|value| value.as_str())
            .map(|url| format!("read {url}")),
        "recall" => args
            .get("query")
            .or_else(|| args.get("id"))
            .and_then(|value| value.as_str())
            .map(|what| format!("recall {what}")),
        "find_tools" => args
            .get("query")
            .and_then(|value| value.as_str())
            .map(|query| format!("find tools for \"{query}\"")),
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
        if let Some(task) = val.get("task")
            && let Some(task_id) = task.get("task_id").and_then(|value| value.as_str())
        {
            let status = task
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            return format!("{tool_name}: {task_id} ({status})");
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
        if let Some(root) = val.get("root").and_then(|v| v.as_str())
            && let Some(stdout) = val.get("stdout").and_then(|v| v.as_str())
            && let Ok(parsed_stdout) = serde_json::from_str::<Value>(stdout.trim())
        {
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
                    if let Some(healthy) = parsed_stdout
                        .get("healthy")
                        .and_then(|value| value.as_bool())
                    {
                        return format!("{tool_name}: healthy={healthy}");
                    }
                }
                "discourse" => {
                    if let Some(items) = parsed_stdout
                        .get("specs")
                        .and_then(|value| value.as_array())
                    {
                        return format!("{tool_name}: {} specs", items.len());
                    }
                    if let Some(events) = parsed_stdout
                        .get("events")
                        .and_then(|value| value.as_array())
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
                    if let Some(turns) = parsed_stdout
                        .get("turns")
                        .and_then(|value| value.as_array())
                    {
                        return format!("{tool_name}: {} turns", turns.len());
                    }
                }
                _ => {}
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
            if let Some(exit_code) = exit_code
                && exit_code != 0
            {
                return format!("{tool_name}: exit {exit_code} — {invocation}");
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

/// Bias tool routing toward the CURRENT step, not just the turn's opening
/// message: the original intent plus a clip of the last couple of messages
/// (the model's latest reasoning / tool results). Lets the working set follow
/// the task as it evolves instead of freezing on iteration 0.
fn routing_query(user_message: &str, history: &[ChatMessage]) -> String {
    let mut q = String::from(user_message);
    for msg in history.iter().rev().take(2) {
        if let Some(content) = &msg.content {
            let clip: String = content.chars().take(200).collect();
            q.push(' ');
            q.push_str(&clip);
        }
    }
    q
}

/// Assemble the tool list for one LLM request: the top-K tools relevant to
/// `route`, the always-on meta-tools (recall + find_tools), and every tool the
/// model has pinned via find_tools this turn — with FULL definitions so a
/// discovered tool is actually callable. (find_tools previously returned
/// name-only, so "call one by name" silently failed.)
/// Given base capability names (priority order), produce the request tool list:
/// base defs + always-on meta-tools (recall + find_tools) + pinned tools,
/// deduped. Shared by the keyword and neural selection paths so the meta/pinned
/// contract is identical either way.
fn finalize_tools(
    catalog: &ToolCatalog,
    selected: &[String],
    pinned: &std::collections::HashSet<String>,
) -> Vec<ToolDefinition> {
    let mut defs = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in selected {
        // Meta-tool names (e.g. `recall`) are executed by the native meta-tool
        // layer, which intercepts BEFORE the Python dispatch. If the Python
        // catalog also defines one, its schema/description would be OFFERED
        // while the meta-tool actually RUNS — a description/execution mismatch.
        // Let the meta-tool loop below own these names so what's offered matches
        // what runs (dedups the shadowed catalog copy, e.g. the artifact-store
        // `recall` vs the provenance-store meta-tool `recall`).
        if crate::meta_tools::is_meta_tool(name) {
            continue;
        }
        if seen.insert(name.clone())
            && let Some(tool) = catalog.find(name)
        {
            defs.push(tool.to_definition());
        }
    }
    for def in crate::meta_tools::definitions()
        .iter()
        .map(|t| t.to_definition())
    {
        if seen.insert(def.function.name.clone()) {
            defs.push(def);
        }
    }
    for name in pinned {
        if !seen.contains(name)
            && let Some(tool) = catalog.find(name)
        {
            seen.insert(name.clone());
            defs.push(tool.to_definition());
        }
    }
    defs
}

/// Restrict a selected tool list to the curated [`CORE_TOOL_SET`], always
/// keeping the meta-tools (find_tools + recall) and pinned (discovered) tools so
/// a weak model can still reach the full catalog through find_tools. Used only
/// when the model's PromptProfile asked for a core-only surface.
fn tier_to_core(
    tools: Vec<ToolDefinition>,
    pinned: &std::collections::HashSet<String>,
) -> Vec<ToolDefinition> {
    tools
        .into_iter()
        .filter(|def| {
            let name = def.function.name.as_str();
            crate::prompt_profile::CORE_TOOL_SET.contains(&name)
                || crate::meta_tools::is_meta_tool(name)
                || pinned.contains(name)
        })
        .collect()
}

/// Keyword selection (fallback path): top-K by keyword match on `route`, then
/// meta-tools + pinned.
fn assemble_request_tools(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    top_k: usize,
) -> Vec<ToolDefinition> {
    let selected: Vec<String> = catalog
        .definitions_for_query(route, top_k)
        .into_iter()
        .map(|d| d.function.name)
        .collect();
    finalize_tools(catalog, &selected, pinned)
}

/// Neural selection (`PRISM_NEURAL_TOOLS`): embedding retrieval over the
/// capability index for the top-K relevant to `route`, then meta-tools +
/// pinned. Falls back to keyword selection when retrieval yields nothing (no
/// embeddings ready / backend error) — so it can never do worse than today.
async fn assemble_request_tools_neural(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    top_k: usize,
    backend: &dyn EmbedBackend,
) -> Vec<ToolDefinition> {
    let entries: Vec<(String, String)> = catalog
        .iter()
        .map(|t| (t.name.clone(), format!("{}: {}", t.name, t.description)))
        .collect();
    let index = crate::capability::global_index(entries, backend).await;
    let selected = index.retrieve(route, top_k, backend).await;
    if selected.is_empty() {
        tracing::debug!(
            "tool selection: neural retrieval empty (embeddings not ready) — keyword fallback"
        );
        return assemble_request_tools(catalog, route, pinned, top_k);
    }
    tracing::debug!(
        retrieved = selected.len(),
        "tool selection: neural embedding retrieval used"
    );
    finalize_tools(catalog, &selected, pinned)
}

/// Whether neural (embedding) tool selection is enabled. **ON by default**; set
/// `PRISM_NEURAL_TOOLS=0` / `false` / `off` to force the legacy keyword path.
/// Safe either way: on a cold turn (backend still warming) or when no embed
/// backend is available at all, neural selection falls back to keyword
/// automatically, so default-on can never do worse than the keyword path.
fn neural_tools_enabled() -> bool {
    match std::env::var("PRISM_NEURAL_TOOLS") {
        Ok(v) => {
            let v = v.trim();
            !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
        }
        Err(_) => true,
    }
}

/// `(name, "name: description")` entries for the neural index / L1 menu.
fn catalog_entries(catalog: &ToolCatalog) -> Vec<(String, String)> {
    catalog
        .iter()
        .map(|t| (t.name.clone(), format!("{}: {}", t.name, t.description)))
        .collect()
}

/// Warm the neural stack in the BACKGROUND (never on the turn path): load the
/// embed model if needed, then build+embed the capability index. Both are
/// process-global caches, so this is a no-op once warm; the whole-catalog embed
/// (seconds on CPU) therefore never stalls a turn — the turn serves keyword
/// until the index is ready, then flips to neural.
fn spawn_neural_warm(entries: Vec<(String, String)>) {
    tokio::spawn(async move {
        if let Some(backend) = crate::embeddings::backend().await {
            let _ = crate::capability::global_index(entries, backend.as_ref()).await;
        }
    });
}

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
    task: Option<&crate::task::ResearchTaskContext>,
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
    // One line per executed tool step — feeds the deterministic TRAJECTORY
    // block injected into every iteration's context.
    let mut traj_steps: Vec<String> = Vec::new();
    // Trajectory v2: durable cross-turn pointers, loaded ONCE per turn (a
    // local Turso open — the same cost the provenance hook already pays per
    // tool call). Missing store/session degrades to no block, never an error.
    let session_memory: Option<String> = load_session_memory().await;
    // Task-driven research context (TOOL_SURFACE_SPEC §5.1): when a task is
    // present, inject its deterministic TASK CONTEXT block every iteration so
    // the model carries the goal/plan-position/artifacts/notes across the
    // inner-loop cap and across turns. Chat turns (task=None) never build a
    // block → chat output is byte-for-byte unchanged (chat-path-unchanged test).
    let task_block = task.and_then(crate::task::task_context_block);
    let mut recent_sigs: VecDeque<String> = VecDeque::with_capacity(DOOM_LOOP_WINDOW + 1);
    // Track consecutive empty results per tool name
    let mut empty_result_streak: HashMap<String, usize> = HashMap::new();
    // Tools the model discovered via find_tools this turn — pinned so their
    // FULL definitions stay in the request every later iteration. Without this,
    // find_tools returned names the model could never actually call.
    let mut pinned_tools: std::collections::HashSet<String> = std::collections::HashSet::new();

    // ── 2. TAOR iteration loop ────────────────────────────────────
    for iteration in 0..config.max_iterations {
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

        // ── 2b. Tool selection ────────────────────────────────────
        // Filter to top-K relevant tools to avoid "tool stuffing" (all tool
        // definitions = tens of thousands of tokens every turn). Route from the
        // CURRENT step (not just the opening message) and fold in tools pinned
        // via find_tools, so discovery makes tools actually callable and the
        // working set follows the task. Done before message assembly so the L1
        // capability menu can reflect what's already callable.
        let route = routing_query(user_message, history);
        // Neural selection needs BOTH the embed model and the embedded capability
        // index warm. Building that index embeds the whole catalog (seconds on
        // CPU), so we NEVER build it on the turn path: if it isn't ready we kick a
        // one-time background warm (model → index) and serve the fast keyword path
        // this turn. Neural then engages a turn or two later with no stall.
        let relevant_tools = if neural_tools_enabled() {
            let entries = catalog_entries(tool_catalog);
            match crate::capability::global_index_if_ready(&entries)
                .and(crate::embeddings::backend_if_ready())
            {
                Some(backend) => {
                    tracing::debug!("tool selection: neural path (model + index ready)");
                    assemble_request_tools_neural(
                        tool_catalog,
                        &route,
                        &pinned_tools,
                        crate::tool_catalog::MAX_TOOLS_PER_REQUEST,
                        backend.as_ref(),
                    )
                    .await
                }
                None => {
                    tracing::debug!(
                        "tool selection: keyword path (neural model/index warming in background)"
                    );
                    spawn_neural_warm(entries);
                    assemble_request_tools(
                        tool_catalog,
                        &route,
                        &pinned_tools,
                        crate::tool_catalog::MAX_TOOLS_PER_REQUEST,
                    )
                }
            }
        } else {
            assemble_request_tools(
                tool_catalog,
                &route,
                &pinned_tools,
                crate::tool_catalog::MAX_TOOLS_PER_REQUEST,
            )
        };
        // Core-set tiering (weak/unknown models via their PromptProfile): keep
        // only the curated core tools, but ALWAYS keep the meta-tools
        // (find_tools + recall) and anything the model pinned via discovery — so
        // the model can still reach the full catalog through find_tools. Capable
        // models keep the full relevance-ranked top-K.
        let relevant_tools = if config.core_tools_only {
            tier_to_core(relevant_tools, &pinned_tools)
        } else {
            relevant_tools
        };
        tracing::debug!(
            total_tools = tool_catalog.len(),
            selected_tools = relevant_tools.len(),
            core_only = config.core_tools_only,
            "tool selection for LLM call"
        );

        // ── 2c. Build messages ────────────────────────────────────
        // L1 progressive disclosure (neural mode only): a compact metadata menu
        // of capabilities beyond the callable top-K, so the model is AWARE of
        // the wider catalog without paying the full-schema token cost. Off by
        // default → messages are byte-identical to before.
        let capability_menu = if neural_tools_enabled() {
            let included: std::collections::HashSet<String> = relevant_tools
                .iter()
                .map(|d| d.function.name.clone())
                .collect();
            let entries = catalog_entries(tool_catalog);
            crate::capability::capability_menu(&entries, &included, 150, 80)
        } else {
            None
        };

        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: Some(config.system_prompt.clone()),
            tool_calls: None,
            tool_call_id: None,
        }];
        // Task-driven context goes first (highest priority for a research task):
        // goal + plan position + artifact handles + working notes, every turn.
        if let Some(block) = &task_block {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(block.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        if let Some(menu) = &capability_menu {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(menu.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        // Progressive disclosure for the agent's OWN authored skills (P3 slice b):
        // surfaced every turn (independent of the neural flag) with the correct
        // `run_skill` call instruction, so a skill written earlier stays visible
        // without the model having to call list_skills. None when none exist.
        if let Some(skills_menu) = crate::skills::skills_menu(50) {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(skills_menu),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        // Trajectory v2: durable pointers from previous turns/sessions —
        // injected before the current turn's trajectory so the model reads
        // past→present in order.
        if let Some(mem) = &session_memory {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(mem.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        // Deterministic trajectory: the harness (not the model's memory) keeps
        // the last executed steps in front of the model every iteration.
        if let Some(traj) = trajectory_block(&traj_steps) {
            messages.push(ChatMessage {
                role: "system".to_string(),
                content: Some(traj),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        messages.extend(history.iter().cloned());

        // Stream tokens incrementally — collect deltas from the
        // streaming callback and emit them after the call completes.
        // Reasoning tokens (is_reasoning=true) are emitted as a separate
        // event so the TUI can render them dimmed/collapsed.
        let mut streamed_deltas: Vec<(String, bool)> = Vec::new();
        let response = llm
            .chat_with_tools_streaming(
                &messages,
                &relevant_tools,
                |delta: &str, is_reasoning: bool| {
                    if !delta.is_empty() {
                        streamed_deltas.push((delta.to_string(), is_reasoning));
                    }
                },
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "LLM call failed: {e:#}");
                // Surface error details in the UI, not just "LLM call failed"
                emit(AgentEvent::TextDelta {
                    text: format!("Error: {e:#}\n"),
                });
                e
            })
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

        // ── 2d-bis. Record the LLM turn in the provenance ledger ──
        // Tool calls were the only rows pre-fix; without the LLM turns
        // the ledger is half a story (and `recall` can't retrieve what
        // the model actually SAID). Non-blocking, same pattern as the
        // provenance hook.
        {
            let session_id = crate::hooks::provenance_session_id();
            let model = crate::hooks::PROVENANCE_CTX
                .read()
                .ok()
                .map(|c| c.llm_model.clone())
                .filter(|m| !m.is_empty());
            let mut record = prism_provenance::new_record(
                &session_id,
                prism_provenance::ActionType::LlmCall,
                prism_provenance::Actor::Agent,
                None,
                model.as_deref(),
                serde_json::json!({
                    "user_message": user_message,
                    "iteration": iteration,
                }),
            );
            record.output_json = Some(serde_json::json!({
                "content": response.message.content,
                "tool_calls": response.message.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|c| c.function.name.clone())
                        .collect::<Vec<_>>()
                }),
            }));
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let db_path = dirs::home_dir()
                        .map(|h| h.join(".prism/provenance.db"))
                        .unwrap_or_else(|| std::path::PathBuf::from("provenance.db"));
                    if let Ok(store) = prism_provenance::ProvenanceStore::open(&db_path).await {
                        match store.record(&record).await {
                            // Semantic memory: embed the turn for `recall`.
                            Ok(()) => crate::embeddings::embed_record(&store, &record).await,
                            Err(e) => tracing::warn!("llm-turn provenance write failed: {e}"),
                        }
                    }
                });
            }
        }

        // ── 2e. Emit text to TUI ─────────────────────────────────
        // Use the clean content from the response (tool call blocks already
        // stripped) instead of raw streaming deltas which can leak partial
        // tool call JSON when SSE chunks split across the ``` boundary.
        // ── 2e. Emit streamed content ───────────────────────────
        // Emit each delta individually so the TUI renders token-by-token.
        // Reasoning tokens (is_reasoning=true) are emitted as
        // ThinkingDelta so the TUI renders them dimmed and collapsed.
        for (delta, is_reasoning) in &streamed_deltas {
            if *is_reasoning {
                emit(AgentEvent::ThinkingDelta {
                    text: delta.clone(),
                });
            } else {
                emit(AgentEvent::TextDelta {
                    text: delta.clone(),
                });
            }
        }

        if let Some(content) = &response.message.content
            && !content.is_empty()
            && streamed_deltas.is_empty()
        {
            emit(AgentEvent::TextDelta {
                text: content.clone(),
            });
        }
        emit(AgentEvent::TextFlush);

        // ── 2f. Push assistant message ────────────────────────────
        history.push(response.message.clone());

        // ── 2g. Check for tool calls ──────────────────────────────
        let tool_calls = match &response.message.tool_calls {
            Some(calls) if !calls.is_empty() => calls.clone(),
            _ => {
                // No tool calls → turn complete

                // Auto-compact if needed
                if transcript.should_compact()
                    && let Some(summary) = transcript.compact(6)
                {
                    compact_history(history, &summary, 6);
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
                // Fail CLOSED via the shared gate helper: an evaluate() error
                // denies the tool rather than letting it run unchecked.
                match prism_policy::gate_outcome(pe.evaluate(&policy_input)) {
                    prism_policy::GateOutcome::Deny { reason } => {
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
                    prism_policy::GateOutcome::Allow { obligations } => {
                        // Log obligations (e.g. "audit_log")
                        for obligation in &obligations {
                            tracing::info!(
                                tool = %tool_name,
                                obligation = %obligation,
                                "OPA policy obligation"
                            );
                        }
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
                    requires_approval: tool_meta
                        .map(|tool| tool.requires_approval)
                        .unwrap_or(false),
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
                            // Approve this call AND auto-approve every later one
                            // for the rest of the session. Persist into the
                            // shared session overrides so the gate at the top of
                            // this loop skips future prompts (survives across
                            // turns; explicit denials are left untouched). Before
                            // this, "Allow All" silently behaved like "Allow Once".
                            if let Some(overrides) = live_permission_overrides.as_ref() {
                                overrides.write().await.allow_all();
                            }
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
            let result: Result<Value> = if crate::meta_tools::is_meta_tool(tool_name) {
                // Native meta-tools (recall / find_tools) operate on the agent's
                // own state — durable memory + the tool catalog — so intercept
                // them before command-tool / Python dispatch. Open the same
                // Turso store the provenance hook writes to.
                let db_path = dirs::home_dir()
                    .map(|h| h.join(".prism/provenance.db"))
                    .unwrap_or_else(|| std::path::PathBuf::from("provenance.db"));
                let store = prism_provenance::ProvenanceStore::open(&db_path).await.ok();
                // Real session id so `recall` scopes to THIS session
                // instead of the pre-fix literal "session" bucket.
                let session_id = crate::hooks::provenance_session_id();
                crate::meta_tools::execute_meta_tool(
                    tool_name,
                    &args,
                    store.as_ref(),
                    &session_id,
                    tool_catalog,
                )
                .await
                .map(|value| serde_json::json!({ "result": value }))
            } else if command_tools::is_command_tool(tool_name) {
                command_tools::execute_command_tool(
                    command_tool_runtime,
                    tool_name,
                    &args,
                    policy.as_deref_mut(),
                )
                .await
                .map(|value| serde_json::json!({ "result": value }))
            } else {
                tool_server
                    .call_tool(tool_name, args.clone())
                    .await
                    .map_err(Into::into)
            };

            // Auto-pin tools surfaced by find_tools so their full definitions
            // become callable next iteration (the "now available" hint used to
            // be false — names came back but were never wired into the request).
            if tool_name == "find_tools"
                && let Ok(v) = &result
                && let Some(matches) = v
                    .get("result")
                    .and_then(|r| r.get("matches"))
                    .and_then(|m| m.as_array())
            {
                for m in matches {
                    if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
                        pinned_tools.insert(name.to_string());
                    }
                }
            }
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

            // ── h7b. Empty-result streak detection ───────────────
            if is_empty_result(&content_after_hooks) {
                let streak = empty_result_streak
                    .entry(tool_name.to_string())
                    .or_insert(0);
                *streak += 1;
                if *streak >= EMPTY_RESULT_MAX {
                    let abort_msg = format!(
                        "{tool_name} returned empty results {streak} times in a row. \
                         This tool isn't finding what you need — try a different tool, \
                         rephrase the query, or answer from your own knowledge.",
                    );
                    emit(AgentEvent::ToolCallResult {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        content: abort_msg.clone(),
                        summary: Some(format!("{tool_name}: empty results, stopping")),
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
            } else {
                // Reset streak on successful result
                empty_result_streak.remove(tool_name.as_str());
            }

            // ── h8. Large-result handling ─────────────────────────
            let content = process_large_result(&content_after_hooks, &mut result_store);

            // ── h9. Log to scratchpad ─────────────────────────────
            let summary = summarize_tool_result(tool_name, preview.as_deref(), &content, is_error);
            traj_steps.push(summary.clone());
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

    fn tool_json(name: &str, desc: &str) -> serde_json::Value {
        serde_json::json!({ "name": name, "description": desc, "input_schema": { "type": "object" } })
    }

    #[test]
    fn assemble_pins_discovered_tool_even_when_keywords_miss() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                tool_json("mace_compute_elastic", "predict the elastic tensor of a structure"),
                tool_json("knowledge", "search the knowledge graph"),
                tool_json("web", "search the open web"),
            ]
        }));
        let mut pinned = std::collections::HashSet::new();
        pinned.insert("mace_compute_elastic".to_string());
        // top_k=2 < 3 tools forces keyword filtering; the query matches none.
        let defs = assemble_request_tools(&catalog, "hello there friend", &pinned, 2);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            names.contains(&"mace_compute_elastic"),
            "pinned tool must be callable even with no keyword match: {names:?}"
        );
        assert!(
            names.contains(&"find_tools"),
            "meta-tool must always be present: {names:?}"
        );
    }

    #[test]
    fn assemble_does_not_duplicate_pinned_and_selected() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [ tool_json("web", "search the open web") ]
        }));
        let mut pinned = std::collections::HashSet::new();
        pinned.insert("web".to_string());
        let defs = assemble_request_tools(&catalog, "web search", &pinned, 15);
        assert_eq!(
            defs.iter().filter(|d| d.function.name == "web").count(),
            1,
            "a pinned+selected tool must not be duplicated"
        );
    }

    #[test]
    fn finalize_prefers_meta_tool_over_colliding_catalog_name() {
        // The Python catalog also ships a `recall` (the artifact-store tool), but
        // the native meta-tool `recall` (provenance store) is what actually
        // executes via the intercept. The OFFERED definition must therefore be
        // the meta-tool's, not the shadowed catalog copy.
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [ tool_json("recall", "PYTHON ARTIFACT STORE recall — shadowed at runtime") ]
        }));
        let pinned = std::collections::HashSet::new();
        let defs = finalize_tools(&catalog, &["recall".to_string()], &pinned);
        let recall = defs
            .iter()
            .find(|d| d.function.name == "recall")
            .expect("recall must be offered");
        assert!(
            !recall
                .function
                .description
                .contains("PYTHON ARTIFACT STORE"),
            "the shadowed catalog recall must not be the offered definition"
        );
        assert!(
            recall.function.description.contains("durable memory"),
            "offered recall must match the executed meta-tool: {}",
            recall.function.description
        );
        assert_eq!(
            defs.iter().filter(|d| d.function.name == "recall").count(),
            1,
            "recall must appear exactly once"
        );
    }

    #[test]
    fn routing_query_folds_in_recent_step() {
        let history = vec![
            ChatMessage {
                role: "user".into(),
                content: Some("find alloys".into()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: Some("now compute the elastic tensor".into()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        let q = routing_query("find alloys", &history);
        assert!(q.contains("find alloys"));
        assert!(
            q.contains("elastic"),
            "recent step context must bias routing: {q}"
        );
    }

    struct KwEmbed;

    #[async_trait::async_trait]
    impl EmbedBackend for KwEmbed {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    let elastic = f32::from(t.contains("elastic") || t.contains("stiffness"));
                    let web = f32::from(t.contains("web") || t.contains("online"));
                    vec![elastic, web]
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            2
        }
        fn id(&self) -> &str {
            "test:kw-embed"
        }
    }

    #[tokio::test]
    async fn neural_assemble_retrieves_semantically_relevant_tool() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                tool_json("mace_compute_elastic", "predict the elastic stiffness tensor"),
                tool_json("web", "search the web online"),
                tool_json("analyze_phases", "calphad phase equilibrium check"),
            ]
        }));
        let pinned = std::collections::HashSet::new();
        let backend = KwEmbed;
        // "stiffness" shares no substring with the elastic tool's name; only the
        // neural path can surface it. Args: (catalog, route, pinned, top_k, backend).
        let defs =
            assemble_request_tools_neural(&catalog, "compute the stiffness", &pinned, 1, &backend)
                .await;
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            names.contains(&"mace_compute_elastic"),
            "neural retrieval should surface the elastic tool: {names:?}"
        );
        assert!(
            names.contains(&"find_tools"),
            "meta-tools must still be present: {names:?}"
        );
    }

    /// Live-verify the WIRED tool-selection path (P1 neural selection + P2 L1
    /// menu) against the REAL local ONNX embedder, in the regime that actually
    /// bites: a catalog larger than `MAX_TOOLS_PER_REQUEST`. With <=15 tools both
    /// paths return everything, so the fix is invisible; the "200 tools, agent
    /// calls 5" bug only exists when filtering kicks in.
    ///
    /// Construction: one needle whose *name/description* is semantically about
    /// mechanical stiffness ("elastic_stiffness_probe"), and a paraphrase route
    /// ("rigidity ... resistance to bending under load") that shares NO literal
    /// token with the needle's name or description — so the keyword scorer gives
    /// it 0 and drops it even with a 15-slot budget, while neural ranks it top-3
    /// out of 20. Names are suffixed `_wv` so this test's name-set is unique and
    /// `global_index`'s process-global cache can't hand back another test's
    /// (stub-embedded) index.
    ///
    /// Ignored by default (needs the ~128 MB model). Run with:
    ///   `cargo test -p prism-agent --lib -- --ignored real_backend_wired`
    #[tokio::test]
    #[ignore = "requires the local ONNX embed model; run with --ignored"]
    async fn real_backend_wired_selection_drops_from_keyword_survives_neural() {
        let needle = "elastic_stiffness_probe_wv";
        let mut tools = vec![tool_json(
            needle,
            "predict how a crystalline solid resists being squeezed or sheared",
        )];
        // 19 fillers in far-off domains; unique `_wv` names.
        for (n, d) in [
            ("web_fetch_wv", "search the open web and fetch a page"),
            ("send_email_wv", "compose and send an email message"),
            (
                "calendar_create_wv",
                "create a calendar event with attendees",
            ),
            (
                "currency_convert_wv",
                "convert an amount between world currencies",
            ),
            (
                "weather_lookup_wv",
                "get the current weather forecast for a city",
            ),
            ("flight_booking_wv", "find and book airline flights"),
            ("pdf_merge_wv", "merge several pdf documents into one file"),
            (
                "image_resize_wv",
                "resize and crop an image to given dimensions",
            ),
            ("sql_query_wv", "run a read-only sql query on a database"),
            ("git_blame_wv", "show git blame history for a source file"),
            ("dns_lookup_wv", "resolve dns records for a hostname"),
            (
                "timezone_convert_wv",
                "convert a timestamp between time zones",
            ),
            ("qr_generate_wv", "generate a qr code for a url"),
            (
                "markdown_lint_wv",
                "lint a markdown document for style issues",
            ),
            ("uuid_generate_wv", "generate a random unique identifier"),
            ("base64_encode_wv", "encode or decode base64 text"),
            (
                "translate_text_wv",
                "translate text between human languages",
            ),
            ("spellcheck_wv", "check spelling and grammar in a paragraph"),
            ("color_palette_wv", "suggest a color palette for a design"),
        ] {
            tools.push(tool_json(n, d));
        }
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(
            &serde_json::json!({ "tools": tools }),
        );
        assert!(
            catalog.len() > crate::tool_catalog::MAX_TOOLS_PER_REQUEST,
            "catalog must exceed the per-request cap for filtering to engage"
        );

        let backend = prism_embed::NativeOnnx::new().expect("load local embed model");
        let pinned = std::collections::HashSet::new();
        let route = routing_query(
            "quantify the material's rigidity and its resistance to bending under load",
            &[],
        );

        // Keyword path, generous 15-slot budget: the needle shares no token with
        // the route, scores 0, and is dropped.
        let kw = assemble_request_tools(&catalog, &route, &pinned, 15);
        let kw_names: Vec<&str> = kw.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            !kw_names.contains(&needle),
            "keyword filtering must DROP the paraphrase-only needle even with 15 slots: {kw_names:?}"
        );

        // Neural path, tight top-3: the needle is a top-3 semantic pick out of 20.
        let neural = assemble_request_tools_neural(&catalog, &route, &pinned, 3, &backend).await;
        let neural_names: Vec<&str> = neural.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            neural_names.contains(&needle),
            "neural retrieval must SURFACE the needle in its top-3: {neural_names:?}"
        );

        // P2 menu: given the neural-callable set, the L1 menu advertises the
        // excluded capabilities (so the model is AWARE), and does NOT re-list the
        // already-callable needle.
        let included: std::collections::HashSet<String> =
            neural.iter().map(|d| d.function.name.clone()).collect();
        let entries: Vec<(String, String)> = catalog
            .iter()
            .map(|t| (t.name.clone(), format!("{}: {}", t.name, t.description)))
            .collect();
        let menu = crate::capability::capability_menu(&entries, &included, 150, 80)
            .expect("menu must advertise the excluded capabilities");
        assert!(
            menu.contains("currency_convert_wv"),
            "L1 menu must advertise an excluded capability: {menu}"
        );
        assert!(
            !menu.contains(needle),
            "the callable needle must not be re-advertised in the menu"
        );
    }

    #[test]
    fn tool_preview_covers_web_and_meta_tools() {
        let p = |name: &str, args: serde_json::Value| tool_preview(name, &args);
        assert_eq!(
            p(
                "web",
                serde_json::json!({"action": "search", "query": "NiTi"})
            ),
            Some("search \"NiTi\"".to_string())
        );
        assert_eq!(
            p(
                "web",
                serde_json::json!({"action": "read", "url": "https://x.org"})
            ),
            Some("read https://x.org".to_string())
        );
        assert_eq!(
            p("web_search", serde_json::json!({"query": "q"})),
            Some("search \"q\"".to_string())
        );
        assert_eq!(
            p("web_read", serde_json::json!({"url": "https://y.io"})),
            Some("read https://y.io".to_string())
        );
        assert_eq!(
            p("recall", serde_json::json!({"query": "lattice"})),
            Some("recall lattice".to_string())
        );
        assert_eq!(
            p("find_tools", serde_json::json!({"query": "deploy a model"})),
            Some("find tools for \"deploy a model\"".to_string())
        );
    }

    #[test]
    fn tool_preview_covers_unified_file_tool() {
        assert_eq!(
            tool_preview(
                "file",
                &serde_json::json!({"action": "edit", "path": "src/a.rs"})
            ),
            Some("edit src/a.rs".to_string())
        );
        assert_eq!(
            tool_preview("file", &serde_json::json!({"path": "src/a.rs"})),
            Some("src/a.rs".to_string())
        );
        assert_eq!(tool_preview("file", &serde_json::json!({})), None);
    }

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
        assert!(result.contains("[Showing first 8000 of 40000 chars"));
        assert!(result.contains("recall"));
        assert_eq!(store.len(), 1);
        // Stored value is the full content
        let stored = store.values().next().unwrap();
        assert_eq!(stored.len(), 40_000);
    }

    #[test]
    fn test_trajectory_block_empty() {
        assert!(trajectory_block(&[]).is_none());
    }

    #[test]
    fn test_trajectory_block_shows_last_five_with_global_numbering() {
        let steps: Vec<String> = (1..=8).map(|i| format!("tool_{i}: ok")).collect();
        let block = trajectory_block(&steps).unwrap();
        // Only the last TRAJECTORY_SHOWN_STEPS appear…
        assert!(!block.contains("#3 tool_3"));
        assert!(block.contains("#4 tool_4"));
        assert!(block.contains("#8 tool_8"));
        // …numbered by their global step index, and framed as a directive.
        assert!(block.contains("Do not repeat a step that succeeded"));
        assert!(block.contains("recall"));
    }

    fn mem_record(session: &str, tool: &str, input: Value) -> prism_provenance::ProvenanceRecord {
        prism_provenance::new_record(
            session,
            prism_provenance::ActionType::ToolCall,
            prism_provenance::Actor::Agent,
            Some(tool),
            None,
            input,
        )
    }

    #[test]
    fn session_memory_block_empty_is_none() {
        assert!(session_memory_block(&[]).is_none());
    }

    #[test]
    fn session_memory_block_shows_pointers_and_resume_position() {
        let records: Vec<_> = (1..=8)
            .map(|i| {
                mem_record(
                    "sess",
                    &format!("tool_{i}"),
                    serde_json::json!({"q": format!("input {i}")}),
                )
            })
            .collect();
        let block = session_memory_block(&records).unwrap();
        // Position framing: 8 prior steps → resuming at step 9.
        assert!(block.contains("8 steps recorded"));
        assert!(block.contains("resuming at step 9"));
        // Only the last SESSION_MEMORY_SHOWN appear, with global numbering…
        assert!(!block.contains("tool_3"));
        assert!(block.contains("step 4 ["));
        assert!(block.contains("tool_4"));
        assert!(block.contains("tool_8"));
        // …each pointer carries the REAL record id so recall(id=…) can expand it.
        assert!(block.contains(&records[7].id));
        // …and the input hint is visible.
        assert!(block.contains("input 8"));
        assert!(block.contains("recall(id="));
    }

    #[test]
    fn compact_json_hint_truncates_on_char_boundary() {
        let long = serde_json::json!({"text": "é".repeat(200)});
        let hint = compact_json_hint(&long, 50);
        assert_eq!(hint.chars().count(), 51, "50 chars + ellipsis");
        assert!(hint.ends_with('…'));
        let short = serde_json::json!({"a": 1});
        assert_eq!(compact_json_hint(&short, 50), "{\"a\":1}");
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
        assert!(
            history[0]
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("summary text")
        );
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

    #[test]
    fn tier_to_core_keeps_core_meta_and_pinned_only() {
        let json = serde_json::json!({
            "tools": [
                { "name": "read_file", "description": "d", "input_schema": {"type":"object"} },
                { "name": "deploy_create", "description": "d", "input_schema": {"type":"object"} },
                { "name": "find_tools", "description": "d", "input_schema": {"type":"object"} },
                { "name": "mesh_publish", "description": "d", "input_schema": {"type":"object"} }
            ]
        });
        let defs = tools_to_definitions(&json);
        let mut pinned = std::collections::HashSet::new();
        pinned.insert("mesh_publish".to_string()); // discovered via find_tools → stays callable
        let names: Vec<String> = tier_to_core(defs, &pinned)
            .iter()
            .map(|d| d.function.name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "read_file"), "core tool kept");
        assert!(names.iter().any(|n| n == "find_tools"), "meta tool kept");
        assert!(
            names.iter().any(|n| n == "mesh_publish"),
            "pinned tool kept"
        );
        assert!(
            !names.iter().any(|n| n == "deploy_create"),
            "non-core non-pinned tool dropped"
        );
    }
}
