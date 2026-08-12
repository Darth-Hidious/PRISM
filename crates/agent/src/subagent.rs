//! `spawn_subagent` — delegate a self-contained task to a nested agent turn.
//!
//! The subagent is not a stub: it runs one full [`crate::agent_loop::run_turn`]
//! (the same TAOR loop the parent is running) against the same command-tool
//! runtime, tool catalog, hooks, and permission context — with a fresh
//! history/transcript/scratchpad and (by default) the Fable frontier model.
//! When the caller provides a lane pool ([`ToolServerPool`]), the subagent
//! checks out its OWN tool-server lane instead of borrowing the parent's
//! handle, so its Python tool calls no longer serialize behind the parent's;
//! without a pool it borrows the parent's handle exactly as before.
//! The parent receives a short text summary plus provenance REFERENCES
//! to what the subagent produced ([`ArtifactHandle`]s — pointers, not blobs;
//! `recall(id=…)` expands them).
//!
//! ## Conservative safety defaults (sequential MVP — no parallel fan-out)
//!
//! - **Recursion cap** ([`MAX_SUBAGENT_DEPTH`] = 2): the depth counter is
//!   threaded through `AgentConfig::subagent_depth`; an agent already at
//!   depth 2 may not spawn deeper — the call returns a model-visible error
//!   instead of recursing.
//! - **Token budget** ([`DEFAULT_SUBAGENT_BUDGET_TOKENS`], overridable via the
//!   `max_tokens` argument): the nested turn gets its own `TranscriptStore`
//!   whose cumulative input-token budget stops a runaway subagent. The
//!   subagent's reported spend is ALSO recorded against the parent's budget
//!   at the dispatch site, so delegation is never a budget escape hatch.
//! - **Inherited gating — no privilege escalation**: the nested turn runs
//!   under the parent's `ToolPermissionContext`, live permission overrides,
//!   OPA policy engine, and approval channel. Approval requests raised inside
//!   the subagent are forwarded to the parent's event sink, so the SAME
//!   human/headless approver answers them; `auto_approve` is inherited
//!   verbatim, never widened. Platform access is inherited too:
//!   `current_platform_access` is a task-local scoped by the transport around
//!   the whole parent turn (`command_tools::with_platform_access`), and the
//!   nested `run_turn` is awaited within the same task and scope — so the
//!   subagent sees exactly the caller's access, and every gate (command
//!   tools, Python/MCP dispatch, meta-tools) re-runs inside the nested turn.
//! - **The spawn itself is access-gated** (effect-classified `ExecutesCode`):
//!   delegation drives a nested turn over the same code-running tool surface,
//!   so it requires node-owner access like the tools it can drive — a
//!   LocalOnly caller gets an honest refusal at the spawn instead of a nested
//!   turn that spends frontier-model tokens before failing at the inner gates.

use anyhow::Result;
use serde_json::{Value, json};

use prism_ingest::llm::LlmClient;
use prism_python_bridge::tool_server::ToolServerHandle;
use prism_python_bridge::{ToolServerLease, ToolServerPool};

use crate::agent_loop::SharedApprovalReceiver;
use crate::command_tools::CommandToolRuntime;
use crate::hooks::HookRegistry;
use crate::models::get_model_config;
use crate::permissions::{PermissionMode, SharedPermissionOverrides, ToolPermissionContext};
use crate::scratchpad::Scratchpad;
use crate::task::ArtifactHandle;
use crate::tool_catalog::{LoadedTool, ToolCatalog};
use crate::transcript::{TranscriptStore, TurnBudget};
use crate::types::{AgentConfig, AgentEvent, UsageInfo};

/// The meta-tool name (the [`crate::meta_tools::MetaTool::SpawnSubagent`]
/// variant of the closed meta-tool registry).
pub const SPAWN_SUBAGENT_TOOL: &str = "spawn_subagent";

/// Default model for delegated tasks — the frontier tier. Registered in
/// `models::MODEL_REGISTRY` so budget/context accounting is real.
pub const DEFAULT_SUBAGENT_MODEL: &str = "claude-fable-5";

/// SAFETY: recursion cap. Depth 0 = top-level agent; its subagent runs at
/// depth 1 and may spawn one more level (depth 2); a depth-2 agent may not
/// spawn further. Prevents unbounded (and unbounded-cost) recursion.
pub const MAX_SUBAGENT_DEPTH: usize = 2;

/// SAFETY: default cumulative input-token budget for one subagent turn.
/// Deliberately below the parent's default (200K) — a delegated task should
/// be self-contained; callers raise it explicitly via `max_tokens`.
pub const DEFAULT_SUBAGENT_BUDGET_TOKENS: u64 = 100_000;

/// Cap (chars) on the summary echoed back to the parent model.
const SUMMARY_CHARS: usize = 4_000;
/// Most-recent tool-step summaries echoed back to the parent.
const STEPS_SHOWN: usize = 12;
/// Most-recent provenance references echoed back to the parent.
const MAX_ARTIFACT_HANDLES: usize = 8;
/// Per-artifact input-hint length (chars).
const ARTIFACT_HINT_CHARS: usize = 100;

/// Catalog entry for `spawn_subagent`, merged into the always-on meta-tool
/// definitions (`meta_tools::definitions`).
#[must_use]
pub fn definition() -> LoadedTool {
    LoadedTool {
        name: SPAWN_SUBAGENT_TOOL.to_string(),
        description: "Delegate a SELF-CONTAINED task to a nested subagent turn \
            (a full agent loop with the same tools; default model claude-fable-5). \
            The subagent cannot ask questions — include all context in `task`. \
            Returns a short summary plus provenance references (expand with \
            recall(id=…)). For several independent tasks use orchestrate_agents."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Complete, self-contained task instruction."
                },
                "model": {
                    "type": "string",
                    "description": "Default 'claude-fable-5'."
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Input-token budget (default 100000)."
                }
            },
            "required": ["task"]
        }),
        // Delegation spends real tokens and can drive workspace-write tools
        // (each still individually gated inside the nested turn) — gate the
        // spawn itself like the other code-running meta-tools.
        requires_approval: true,
        permission_mode: PermissionMode::WorkspaceWrite,
        source: Some("builtin".to_string()),
        source_detail: Some("orchestration".to_string()),
    }
}

// ── Argument parsing ──────────────────────────────────────────────────

struct SubagentArgs {
    task: String,
    model: String,
    budget_tokens: u64,
}

fn parse_args(args: &Value) -> Result<SubagentArgs> {
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if task.is_empty() {
        anyhow::bail!("spawn_subagent requires a non-empty `task`");
    }
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(DEFAULT_SUBAGENT_MODEL)
        .to_string();
    let budget_tokens = args
        .get("max_tokens")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_SUBAGENT_BUDGET_TOKENS);
    Ok(SubagentArgs {
        task: task.to_string(),
        model,
        budget_tokens,
    })
}

/// Model-visible depth-cap error, or `None` when spawning is allowed.
/// Soft error (not `Err`) so the model reads it and does the work itself,
/// matching the `find_tools` error convention.
fn depth_cap_error(config: &AgentConfig) -> Option<Value> {
    (config.subagent_depth >= MAX_SUBAGENT_DEPTH).then(|| {
        json!({
            "error": format!(
                "subagent recursion cap reached (depth {} of {MAX_SUBAGENT_DEPTH}) — \
                 do this task yourself instead of delegating further",
                config.subagent_depth,
            ),
        })
    })
}

/// `(input_tokens, output_tokens)` reported in a spawn_subagent result — used
/// by the dispatch site to charge the subagent's spend to the PARENT budget.
#[must_use]
pub fn usage_from_result(value: &Value) -> (u64, u64) {
    let usage = value.get("usage");
    let read = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    (read("input_tokens"), read("output_tokens"))
}

// ── Execution ─────────────────────────────────────────────────────────

/// Run one nested agent turn for a delegated task. Called from the agent
/// loop's dispatch (NOT from `execute_meta_tool` — this needs the live turn
/// machinery: LLM client, tool server, approval channel, policy engine).
///
/// Returns a boxed `dyn Future` (not an `async fn`): `run_turn` awaits this
/// and this awaits `run_turn`, so the erased, explicitly-`Send` type is what
/// breaks the recursive future-size/auto-trait cycle.
#[allow(clippy::too_many_arguments)]
pub fn execute_spawn_subagent<'a>(
    llm: &'a LlmClient,
    tool_server: &'a mut ToolServerHandle,
    command_tool_runtime: &'a CommandToolRuntime,
    tool_catalog: &'a ToolCatalog,
    parent_config: &'a AgentConfig,
    parent_run_id: &'a str,
    parent_session_id: &'a str,
    args: &'a Value,
    hooks: &'a HookRegistry,
    permissions: &'a ToolPermissionContext,
    live_permission_overrides: Option<SharedPermissionOverrides>,
    emit: &'a mut (dyn FnMut(AgentEvent) + Send),
    approval_rx: Option<SharedApprovalReceiver>,
    policy: Option<&'a mut prism_policy::PolicyEngine>,
    subagent_lanes: Option<&'a ToolServerPool>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        execute_spawn_subagent_inner(
            llm,
            tool_server,
            command_tool_runtime,
            tool_catalog,
            parent_config,
            parent_run_id,
            parent_session_id,
            args,
            hooks,
            permissions,
            live_permission_overrides,
            emit,
            approval_rx,
            policy,
            subagent_lanes,
        )
        .await
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_spawn_subagent_inner(
    llm: &LlmClient,
    tool_server: &mut ToolServerHandle,
    command_tool_runtime: &CommandToolRuntime,
    tool_catalog: &ToolCatalog,
    parent_config: &AgentConfig,
    parent_run_id: &str,
    parent_session_id: &str,
    args: &Value,
    hooks: &HookRegistry,
    permissions: &ToolPermissionContext,
    live_permission_overrides: Option<SharedPermissionOverrides>,
    emit: &mut (dyn FnMut(AgentEvent) + Send),
    approval_rx: Option<SharedApprovalReceiver>,
    policy: Option<&mut prism_policy::PolicyEngine>,
    subagent_lanes: Option<&ToolServerPool>,
) -> Result<Value> {
    // SAFETY: access gate FIRST. spawn_subagent is effect-classified
    // ExecutesCode (it drives a nested turn over the same code-running tool
    // surface), so it resolves node-owner access through the same gate every
    // other execution surface uses — before a nested turn, a model call, or
    // any token is spent. The nested turn inherits THIS access via the
    // task-local scope and re-runs every gate inside; delegation can never
    // widen it (see module docs).
    crate::command_tools::gate_meta_tool_execution(
        crate::meta_tools::MetaTool::SpawnSubagent,
        crate::command_tools::current_platform_access(),
    )?;

    // SAFETY: recursion cap — enforced before anything is spent.
    if let Some(err) = depth_cap_error(parent_config) {
        return Ok(err);
    }
    let sub = parse_args(args)?;

    // Own tool-server lane. With a pool, the subagent's Python tool calls run
    // on a child of its OWN instead of serializing behind (and mutably
    // borrowing) the parent's handle — the substrate for concurrent
    // delegation. Acquired BEFORE the durable child row for the same reason
    // the gates run first: a refused lane leaves no ghost agent. Refusal is a
    // model-visible soft error (the honest pool error names the bound), never
    // a silent fallback onto the parent's handle — quietly re-serializing
    // would hide the resource pressure the pool bound exists to surface.
    //
    // LocalOnly isolation note: this pool vends normal-environment children,
    // which is safe here because the ExecutesCode gate above already refused
    // every non-node-owner caller — and the LocalOnly chat path additionally
    // passes no pool at all (see `ChatService::chat_inner`).
    let mut own_lane: Option<ToolServerLease> = match subagent_lanes {
        Some(pool) => match pool.acquire().await {
            Ok(mut lane) => {
                // Point the lane's Python artifact recorder at the parent's
                // session (the subagent's work belongs to the same session).
                // App-level refusal is nonfatal — the same best-effort
                // contract as `sync_tool_server_session` in protocol.rs — but
                // a TRANSPORT failure means the lane itself is broken, and a
                // broken lane must be reported, not used.
                match lane.set_session_id(parent_session_id).await {
                    Ok(response) => {
                        if let Some(error) = response.get("error") {
                            tracing::warn!(
                                session_id = parent_session_id,
                                %error,
                                "subagent lane session sync refused; continuing"
                            );
                        }
                        Some(lane)
                    }
                    Err(error) => {
                        return Ok(json!({
                            "error": format!(
                                "subagent tool-server lane failed during session sync: {error}. \
                                 Do the task yourself in this turn instead of delegating."
                            ),
                        }));
                    }
                }
            }
            Err(error) => {
                return Ok(json!({
                    "error": format!(
                        "subagent tool-server lane unavailable: {error}. \
                         Do the task yourself in this turn instead of delegating."
                    ),
                }));
            }
        },
        None => None,
    };

    // The child row is created only after every spawn gate above succeeds, so
    // refused delegation does not leave a ghost agent. Its explicit parent id
    // is the durable spawn edge; provenance_records.parent_id remains the
    // unrelated tool repair/retry chain.
    let child_run = prism_provenance::new_agent_run(
        parent_session_id,
        "subagent",
        &crate::agent_loop::agent_run_label(&sub.task),
        Some(parent_run_id),
    );
    let db_path = crate::hooks::provenance_db_path();
    let run_store = match prism_provenance::ProvenanceStore::open(&db_path).await {
        Ok(store) => match store.start_agent_run(&child_run).await {
            Ok(()) => Some(std::sync::Arc::new(store)),
            Err(error) => {
                tracing::warn!(
                    run_id = %child_run.id,
                    parent_run_id,
                    error = %error,
                    "subagent-run ledger start failed; continuing spawn"
                );
                None
            }
        },
        Err(error) => {
            tracing::warn!(
                run_id = %child_run.id,
                parent_run_id,
                error = %error,
                "subagent-run ledger open failed; continuing spawn"
            );
            None
        }
    };
    let run_heartbeat =
        crate::agent_loop::AgentRunHeartbeat::start(run_store.clone(), child_run.id.clone());

    // Real budget/context accounting for the subagent model (WU1: the default
    // fable model is registered, so this is never the $0 UNKNOWN fallback).
    let model_cfg = get_model_config(&sub.model);

    // Sibling LLM client: same endpoint/credentials as the parent, model
    // swapped, catalog-derived window/output caps so client-side output
    // clamping and compaction budgeting are correct for the nested model.
    let mut llm_config = llm.config().clone();
    llm_config.model = sub.model.clone();
    llm_config.context_window = Some(model_cfg.context_window as u64);
    llm_config.max_output_tokens = Some(model_cfg.max_output_tokens as u64);
    let sub_llm = LlmClient::new(llm_config);

    // Nested config: inherit the parent's prompt/iteration/approval settings
    // verbatim (no escalation), bump the depth, swap the model.
    let mut config = parent_config.clone();
    config.model = sub.model.clone();
    config.subagent_depth = parent_config.subagent_depth + 1;

    // SAFETY: explicit per-subagent token budget (cumulative input tokens).
    let mut budget = TurnBudget::for_model(
        Some(model_cfg.context_window as u64),
        Some(model_cfg.max_output_tokens as u64),
    );
    budget.max_input_tokens = sub.budget_tokens;
    let mut transcript = TranscriptStore::new(Some(budget));

    let mut history = Vec::new();
    let mut scratchpad = Scratchpad::new();

    // Snapshot the session's provenance ids so the subagent's new records can
    // be handed back as references afterwards.
    let before_ids = session_record_ids(parent_session_id).await;

    // Harvest state filled by the nested emit callback.
    let mut streamed_text = String::new();
    let mut final_text: Option<String> = None;
    let mut steps: Vec<String> = Vec::new();
    let mut usage: Option<UsageInfo> = None;
    let mut estimated_cost: Option<f64> = None;
    let mut run_metrics = crate::agent_loop::AgentRunMetrics::default();
    let nested_result;

    {
        // Nested event routing: the subagent's text is CAPTURED (it becomes
        // the tool result, not parent output), while tool activity and —
        // critically — approval requests are FORWARDED to the parent's sink,
        // so the parent's approver (TUI user or headless approve-list) gates
        // the subagent's tools exactly as it gates the parent's.
        let mut nested_emit = |event: AgentEvent| {
            match &event {
                AgentEvent::TextDelta { text } => {
                    streamed_text.push_str(text);
                    return;
                }
                AgentEvent::ThinkingDelta { .. } | AgentEvent::TextFlush => return,
                AgentEvent::TurnComplete {
                    text,
                    total_usage,
                    estimated_cost: cost,
                    ..
                } => {
                    final_text = text.clone();
                    usage = total_usage.clone();
                    estimated_cost = *cost;
                    return;
                }
                AgentEvent::ToolCallResult { summary, .. } => {
                    if let Some(summary) = summary {
                        steps.push(summary.clone());
                    }
                }
                AgentEvent::ContextPriming { .. }
                | AgentEvent::ToolCallStart { .. }
                | AgentEvent::ToolApprovalRequest { .. } => {}
            }
            emit(event);
        };

        // Boxed as a dyn future: run_turn → spawn_subagent → run_turn is
        // recursive, so the indirection (and the erased type) breaks the
        // otherwise-infinite future size / auto-trait cycle.
        //
        // G2: snapshot the parent's repair-chain memory around the nested turn.
        // run_turn's entry-reset would otherwise WIPE the parent's in-flight
        // chain, and the subagent's leftover record would chain into the
        // parent's next call. The RAII guard snapshots now and restores on drop
        // — covering Ok, Err, AND a panic unwinding through the nested run_turn
        // (H3: the old restore-before-`?` was skipped on unwind). The parent's
        // chain survives intact and the subagent's is isolated + discarded.
        let _chain_guard = crate::hooks::CodeRunChainGuard::new();
        // The nested turn executes tools on the subagent's own lane when one
        // was acquired; only the legacy no-pool path still borrows the
        // parent's handle.
        let nested_tool_server: &mut ToolServerHandle = match own_lane.as_mut() {
            Some(lane) => lane,
            None => tool_server,
        };
        let nested: std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> =
            Box::pin(crate::agent_loop::run_turn_inner(
                &sub_llm,
                nested_tool_server,
                command_tool_runtime,
                &mut history,
                tool_catalog,
                &config,
                &sub.task,
                None, // subagent turns are chat-shaped; no research task context
                &mut transcript,
                hooks,
                permissions,
                live_permission_overrides,
                &mut scratchpad,
                &mut nested_emit,
                approval_rx,
                policy,
                // Forwarded so a depth-2 subagent also takes its own lane
                // (bounded by the pool, refused honestly on exhaustion).
                subagent_lanes,
                &child_run.id,
                &child_run.session_id,
                &mut run_metrics,
            ));
        // `_chain_guard` restores the parent's chain on drop (Ok/Err/unwind).
        nested_result = nested.await;
    }
    // Release the lane as soon as the nested turn is over — a healthy child
    // returns to the pool for reuse; a desynchronized one is discarded and
    // replaced on the next acquire (see ToolServerLease).
    drop(own_lane);

    run_heartbeat.stop().await;

    let (status, last_error) = match &nested_result {
        Ok(()) => (prism_provenance::AgentRunStatus::Completed, None),
        Err(error) => (
            prism_provenance::AgentRunStatus::Failed,
            Some(format!("{error:#}")),
        ),
    };
    if let Some(store) = run_store.as_deref()
        && let Err(error) = store
            .finish_agent_run(
                &child_run.id,
                status,
                run_metrics.tokens_in,
                run_metrics.tokens_out,
                run_metrics.cost_usd,
                last_error.as_deref(),
            )
            .await
    {
        tracing::warn!(
            run_id = %child_run.id,
            parent_run_id,
            error = %error,
            "subagent-run ledger finish failed; preserving nested result"
        );
    }
    nested_result?;

    // References, not blobs: best-effort provenance pointers to what the
    // subagent did (its writes are async, so a still-in-flight record may be
    // missed — recall(query=…) covers anything not listed).
    let artifacts = harvest_artifacts(parent_session_id, before_ids.as_ref()).await;

    let summary_src = final_text
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(streamed_text);
    let start = steps.len().saturating_sub(STEPS_SHOWN);

    Ok(json!({
        "model": sub.model,
        "summary": clip(summary_src.trim(), SUMMARY_CHARS),
        "steps": &steps[start..],
        "artifacts": artifacts,
        "usage": usage.map(|u| json!({
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
        })),
        "estimated_cost": estimated_cost,
        "hint": "artifacts are provenance references — expand one with recall(id=…); \
                 recall(query=…) finds anything not listed",
    }))
}

// ── Provenance references ─────────────────────────────────────────────

async fn open_session_store(session_id: &str) -> Option<prism_provenance::ProvenanceStore> {
    if session_id == "unknown" {
        // No real session context — the "unknown" bucket aggregates unrelated
        // writes, so a diff against it would hand back foreign records.
        return None;
    }
    let db_path = crate::hooks::provenance_db_path();
    prism_provenance::ProvenanceStore::open(&db_path).await.ok()
}

/// Ids of every provenance record currently in the session. `None` when the
/// store/session is unavailable (harvest then degrades to no references).
async fn session_record_ids(session_id: &str) -> Option<std::collections::HashSet<String>> {
    let store = open_session_store(session_id).await?;
    let records = store.query_by_session(session_id).await.ok()?;
    Some(records.into_iter().map(|r| r.id).collect())
}

/// Tool-call records added to the session during the nested turn, as
/// [`ArtifactHandle`] references. Best-effort by design (never fails a turn).
async fn harvest_artifacts(
    session_id: &str,
    before: Option<&std::collections::HashSet<String>>,
) -> Vec<ArtifactHandle> {
    let Some(before) = before else {
        return Vec::new();
    };
    let Some(store) = open_session_store(session_id).await else {
        return Vec::new();
    };
    let Ok(records) = store.query_by_session(session_id).await else {
        return Vec::new();
    };
    let mut handles: Vec<ArtifactHandle> = records
        .into_iter()
        .filter(|r| {
            r.action_type == prism_provenance::ActionType::ToolCall && !before.contains(&r.id)
        })
        .map(|r| {
            let tool = r.tool_name.as_deref().unwrap_or("(tool)");
            let hint = clip(&r.input_json.to_string(), ARTIFACT_HINT_CHARS);
            ArtifactHandle {
                summary: format!("{tool} {hint}"),
                bytes: r
                    .output_json
                    .as_ref()
                    .map(|o| o.to_string().len())
                    .unwrap_or(0),
                id: r.id,
            }
        })
        .collect();
    let excess = handles.len().saturating_sub(MAX_ARTIFACT_HANDLES);
    handles.drain(0..excess);
    handles
}

/// Clip a string to `max` chars (whole chars, not bytes).
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_is_conservative() {
        let def = definition();
        assert_eq!(def.name, SPAWN_SUBAGENT_TOOL);
        assert!(def.requires_approval, "delegation spends tokens → gated");
        assert_eq!(def.permission_mode, PermissionMode::WorkspaceWrite);
        assert_eq!(def.input_schema["required"], json!(["task"]));
    }

    #[test]
    fn parse_args_applies_defaults() {
        let sub = parse_args(&json!({ "task": "survey refractory HEAs" })).unwrap();
        assert_eq!(sub.task, "survey refractory HEAs");
        assert_eq!(sub.model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(sub.budget_tokens, DEFAULT_SUBAGENT_BUDGET_TOKENS);
    }

    #[test]
    fn parse_args_honors_overrides() {
        let sub = parse_args(&json!({
            "task": "t",
            "model": "claude-sonnet-5",
            "max_tokens": 42_000,
        }))
        .unwrap();
        assert_eq!(sub.model, "claude-sonnet-5");
        assert_eq!(sub.budget_tokens, 42_000);
    }

    #[test]
    fn parse_args_rejects_empty_task() {
        assert!(parse_args(&json!({})).is_err());
        assert!(parse_args(&json!({ "task": "   " })).is_err());
    }

    #[test]
    fn depth_cap_blocks_at_max_depth() {
        let mut config = AgentConfig::default();
        assert_eq!(config.subagent_depth, 0);
        assert!(depth_cap_error(&config).is_none(), "top level may spawn");
        config.subagent_depth = 1;
        assert!(
            depth_cap_error(&config).is_none(),
            "depth-1 subagent may spawn one more level"
        );
        config.subagent_depth = MAX_SUBAGENT_DEPTH;
        let err = depth_cap_error(&config).expect("depth-2 agent must not spawn");
        assert!(
            err["error"].as_str().unwrap().contains("recursion cap"),
            "{err}"
        );
    }

    #[test]
    fn usage_from_result_reads_reported_spend() {
        let value = json!({
            "usage": { "input_tokens": 1200, "output_tokens": 340 },
        });
        assert_eq!(usage_from_result(&value), (1200, 340));
        assert_eq!(usage_from_result(&json!({ "error": "x" })), (0, 0));
    }
}
