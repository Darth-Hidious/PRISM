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
//!   subagent's MEASURED spend is ALSO recorded against the parent's budget
//!   at the dispatch site — unconditionally, from the accumulator the nested
//!   turn fills as it runs, so an errored turn's real tokens are charged too
//!   and delegation is never a budget escape hatch.
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

use anyhow::{Context, Result};
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
use crate::types::{AgentConfig, AgentEvent};

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
            (same tools). It cannot ask questions — put all context in `task`. \
            Returns `status` (completed|incomplete), a summary and provenance \
            refs; `incomplete` means it stopped without a final answer, so do \
            not report that work as done. For several independent tasks use \
            orchestrate_agents."
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
                    "description": "Defaults to your model."
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
        declared_free: false,
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

fn parse_args(args: &Value, parent_model: &str) -> Result<SubagentArgs> {
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if task.is_empty() {
        anyhow::bail!("spawn_subagent requires a non-empty `task`");
    }
    // INHERIT the parent's model unless the caller names one. The old default
    // was the constant, which asks whatever endpoint the parent is routed to for
    // a model it may not serve: measured 2026-08-20 with the parent on glm-5.3,
    // every subagent died with `1214 modelCode：不存在` and the whole
    // decomposition path was dead on any non-Anthropic route. A subagent is the
    // same agent doing a smaller piece of the same job; it should not silently
    // change providers.
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let inherited = parent_model.trim();
            if inherited.is_empty() {
                DEFAULT_SUBAGENT_MODEL.to_string()
            } else {
                inherited.to_string()
            }
        });
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

/// Context attached to EVERY failed delegated turn, in `spawn_subagent` and in
/// every `orchestrate_agents` item.
///
/// A nested turn that died because its endpoint does not serve its model used
/// to surface as a bare `LLM call failed: LLM returned HTTP 400 …` — the two
/// facts an operator needs (WHICH model was asked of WHICH endpoint) were the
/// two facts the error did not carry, so the same misroute was diagnosed twice.
/// This names both, plus the two ways out, on the delegated turn's own error.
///
/// Deliberately unconditional rather than pattern-matched on the provider's
/// error body: provider error shapes differ, and a model/endpoint pair is
/// worth naming on ANY delegated failure.
pub(crate) fn delegation_failure_context(label: &str, model: &str, base_url: &str) -> String {
    format!(
        "delegated turn ({label}) failed while running model `{model}` against `{base_url}` \
         — if that endpoint does not serve `{model}`, name a model it does serve in the \
         `model` argument, or switch the session's model with /model"
    )
}

// ── Execution ─────────────────────────────────────────────────────────

/// Run one nested agent turn for a delegated task. Called from the agent
/// loop's dispatch (NOT from `execute_meta_tool` — this needs the live turn
/// machinery: LLM client, tool server, approval channel, policy engine).
///
/// Returns a boxed `dyn Future` (not an `async fn`): `run_turn` awaits this
/// and this awaits `run_turn`, so the erased, explicitly-`Send` type is what
/// breaks the recursive future-size/auto-trait cycle.
///
/// `run_metrics` is the caller's accumulator for the nested turn's spend. It
/// is an OUT-PARAMETER, not a return value, because the caller must be able
/// to charge what the subagent burned even when this future resolves to
/// `Err` — the spend is real either way. See the dispatch site in
/// `agent_loop`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_spawn_subagent<'a>(
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
    run_metrics: &'a mut crate::agent_loop::AgentRunMetrics,
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
            run_metrics,
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
    run_metrics: &mut crate::agent_loop::AgentRunMetrics,
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
    // The inherited model comes from the LIVE `LlmClient`, never from
    // `AgentConfig.model`.
    //
    // There are TWO model fields and only one of them is ever populated from
    // the resolved chat route. `LlmConfig.model` is what goes on the wire
    // (`crates/llm/src/lib.rs`, `"model": self.config.model`) and what a
    // mid-session `/model` switch mutates (`protocol.rs`). `AgentConfig.model`
    // is never assigned from the resolved route anywhere in production — the
    // one construction site is `AgentConfig { system_prompt, ..Default::default() }`
    // (`protocol.rs`), so it holds the `impl Default` literal for the whole
    // session while every real request goes out on the other field.
    //
    // The 2026-08-20 inheritance fix below was right about WHAT to inherit and
    // read the wrong field, so on a z.ai session the parent ran `glm-5.3` and
    // every unnamed subagent asked that endpoint for the Default literal and
    // died with `1214 modelCode does not exist`. Reading the client instead of
    // the config cannot drift from the wire, and survives `/model`.
    let sub = parse_args(args, &llm.config().model)?;

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
    // Per-write ledger, never a held store handle: this state lives across
    // every tool call of the nested turn, and a held handle blocks any PRISM
    // subprocess the subagent spawns from opening the store. See
    // `agent_loop::RunLedger`.
    let run_ledger = crate::agent_loop::RunLedger::start(&child_run, "subagent-run").await;
    let run_heartbeat =
        crate::agent_loop::AgentRunHeartbeat::start(run_ledger.clone(), child_run.id.clone());

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
    //
    // Spend is deliberately NOT harvested here. `run_metrics` (the caller's
    // accumulator, threaded into the nested turn below) accrues per LLM call
    // as the turn runs, so it is right whether the turn finishes or dies. The
    // deleted alternative read `TurnComplete { total_usage, estimated_cost }`
    // out of this callback — an event that never fires when a turn errors,
    // and that carries `estimated_cost: None` on the budget-exhausted arm
    // (`agent_loop.rs`, "Budget exhausted."). Both the reported figure and
    // the parent's charge now come from the one accumulator the child's own
    // ledger row is closed with, so they cannot drift apart again.
    let mut streamed_text = String::new();
    let mut final_text: Option<String> = None;
    let mut steps: Vec<String> = Vec::new();
    let nested_result;

    {
        // Nested event routing: the subagent's text is CAPTURED (it becomes
        // the tool result, not parent output), while tool activity and —
        // critically — approval requests are FORWARDED to the parent's sink,
        // so the parent's approver (TUI user or headless approve-list) gates
        // the subagent's tools exactly as it gates the parent's.
        let mut nested_emit = |event: AgentEvent| {
            match &event {
                // Background-activity notices are the parent's to show; a
                // subagent's are folded into its lane by the parent.
                AgentEvent::Activity { .. } => {}
                AgentEvent::TextDelta { text } => {
                    streamed_text.push_str(text);
                    return;
                }
                AgentEvent::ThinkingDelta { .. } | AgentEvent::TextFlush => return,
                AgentEvent::TurnComplete { text, .. } => {
                    final_text = text.clone();
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
                // Already tagged by a deeper agent: forwarded untouched.
                // Re-tagging here would claim a grandchild's work for its
                // parent.
                AgentEvent::AgentActivity { .. } => {}
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
                &mut *run_metrics,
            ));
        // `_chain_guard` restores the parent's chain on drop (Ok/Err/unwind).
        nested_result = nested.await;
    }
    // Release the lane as soon as the nested turn is over — a healthy child
    // returns to the pool for reuse; a desynchronized one is discarded and
    // replaced on the next acquire (see ToolServerLease).
    drop(own_lane);

    run_heartbeat.stop().await;

    // Name the model and the endpoint on the way out — see
    // `delegation_failure_context`. Applied BEFORE the ledger row is closed so
    // the durable `last_error` carries the same named message the model sees;
    // `with_context` is lazy, so a healthy turn formats nothing.
    let nested_result = nested_result.with_context(|| {
        delegation_failure_context(
            &crate::agent_loop::agent_run_label(&sub.task),
            &sub.model,
            &sub_llm.config().base_url,
        )
    });

    let (status, last_error) = match &nested_result {
        Ok(()) => (prism_provenance::AgentRunStatus::Completed, None),
        Err(error) => (
            prism_provenance::AgentRunStatus::Failed,
            Some(format!("{error:#}")),
        ),
    };
    if let Some(ledger) = run_ledger.as_ref() {
        ledger
            .finish(&child_run.id, status, run_metrics, last_error.as_deref())
            .await;
    }
    nested_result?;

    // References, not blobs: best-effort provenance pointers to what the
    // subagent did (its writes are async, so a still-in-flight record may be
    // missed — recall(query=…) covers anything not listed).
    let artifacts = harvest_artifacts(parent_session_id, before_ids.as_ref()).await;

    // A child that finished and a child that stopped mid-sentence must not look
    // the same to the parent.
    //
    // `final_text` is the child's answer. When it is absent this falls back to
    // `streamed_text` — everything the child said WHILE WORKING — and hands it
    // over as "summary". Google's ADK harness measured exactly this failure
    // (arXiv 2608.17528, Pattern 4): "A child that timed out, hit its step
    // limit, paused for approval, or completed normally all returned the exact
    // same structure... Partial commentary reads exactly like a finished
    // report." Their root agent reported "all 20 tests passing" from a delegate
    // that had timed out and written nothing.
    //
    // Their fix, adopted here: name the terminal state AND say it in the
    // summary. "A status field protects the calling code but not the model,
    // which reads the summary, not the field beside it." Codex's multi-agent
    // protocol draws the same line, typing every message NEW_TASK | MESSAGE |
    // FINAL_ANSWER so a parent branches on a kind rather than on prose.
    let completed = final_text.as_deref().is_some_and(|t| !t.trim().is_empty());
    let summary_src = final_text
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(streamed_text);
    let summary = clip(summary_src.trim(), SUMMARY_CHARS);
    let summary = if completed {
        summary
    } else {
        format!(
            "INCOMPLETE: the subagent stopped without producing a final answer. \
             What follows is its working commentary, NOT a finished result — do \
             not report this work as done.\n\n{summary}"
        )
    };
    let start = steps.len().saturating_sub(STEPS_SHOWN);

    Ok(json!({
        "model": sub.model,
        "status": if completed { "completed" } else { "incomplete" },
        "summary": summary,
        "steps": &steps[start..],
        "artifacts": artifacts,
        "usage": {
            "input_tokens": run_metrics.tokens_in,
            "output_tokens": run_metrics.tokens_out,
        },
        "estimated_cost": run_metrics.cost_usd,
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
    /// A child that stopped mid-sentence must not read like one that finished.
    ///
    /// Google's ADK harness measured this (arXiv 2608.17528, Pattern 4): a
    /// delegate that timed out returned the same shape as one that completed —
    /// "every line the child had said while working, joined together" — and the
    /// root agent reported "all 20 tests passing" from a run that wrote nothing.
    /// PRISM had the same shape: `final_text ... .unwrap_or(streamed_text)`.
    ///
    /// The status field alone is not the fix. ADK: "A status field protects the
    /// calling code but not the model, which reads the summary, not the field
    /// beside it." So the SUMMARY has to say it too.
    #[test]
    fn an_unfinished_subagent_says_so_in_the_summary_not_only_in_a_field() {
        let def = super::definition();
        assert!(
            def.description.contains("completed|incomplete"),
            "the parent must be told the field exists: {}",
            def.description
        );
        assert!(
            def.description.contains("do not report that work as done"),
            "and what it means: {}",
            def.description
        );
    }

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
        let sub = parse_args(&json!({ "task": "survey refractory HEAs" }), "glm-5.3").unwrap();
        assert_eq!(sub.task, "survey refractory HEAs");
        // CONTRACT CHANGE: inherits the parent's model. A subagent is the same
        // agent doing a smaller piece of the same job; it must not silently
        // switch providers to one the parent's endpoint does not serve.
        assert_eq!(sub.model, "glm-5.3");
        assert_eq!(sub.budget_tokens, DEFAULT_SUBAGENT_BUDGET_TOKENS);

        // Only when the parent has no model at all does the constant apply.
        let orphan = parse_args(&json!({ "task": "x" }), "  ").unwrap();
        assert_eq!(orphan.model, DEFAULT_SUBAGENT_MODEL);
    }

    #[test]
    fn parse_args_honors_overrides() {
        let sub = parse_args(
            &json!({
                "task": "t",
                "model": "claude-sonnet-5",
                "max_tokens": 42_000,
            }),
            "glm-5.3",
        )
        .unwrap();
        assert_eq!(
            sub.model, "claude-sonnet-5",
            "an explicitly named model still wins over the parent's"
        );
        assert_eq!(sub.budget_tokens, 42_000);
    }

    #[test]
    fn parse_args_rejects_empty_task() {
        assert!(parse_args(&json!({}), "glm-5.3").is_err());
        assert!(parse_args(&json!({ "task": "   " }), "glm-5.3").is_err());
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
}
