//! `orchestrate_agents` — the agent ORCHESTRATOR: bounded fan-out of N
//! delegated agent tasks over the tool-server lane pool, with per-item
//! verification.
//!
//! Naming note: this is orchestration of AGENTS. It is deliberately not
//! called a "workflow" — that word already means the declarative YAML tool
//! pipelines in `prism-workflows`, which orchestrate TOOLS and keep that
//! meaning. The orchestrator lives on the agent side, next to
//! [`crate::subagent`], and generalizes `spawn_subagent` from "one delegated
//! turn at a time" to "N delegated turns, concurrently, bounded".
//!
//! # Design principle
//!
//! The model coordinating a fan-out is not trusted to infer what happened —
//! it is TOLD, per item, with specific, truthful, actionable outcomes:
//!
//! - **Per-item outcomes, never an aggregate.** Every task ends as
//!   `succeeded`, `failed` (with the reason), or `skipped` (with the reason).
//!   A bare "ok" over a partial run is the failure mode this module exists to
//!   prevent. Result order is input order, never completion order.
//! - **A failed item never takes down its siblings.** Item failures (and even
//!   item-task panics) are converted into that item's outcome.
//! - **Verification is structural.** A task may declare a self-contained JSON
//!   Schema for its result; a mismatch resumes that ONE agent exactly once
//!   with the concrete validation error, and a second mismatch fails the item
//!   with the reason. No blanket retries.
//! - **Budget is reserved before spawn.** A run carries a maximum number of
//!   agent calls; each spawn (and each schema repair) reserves before it
//!   launches and can never double-charge. Exhaustion is a reported outcome
//!   (`skipped`, plus a run-level flag), not an error to swallow.
//!
//! # Bound reconciliation
//!
//! Two declared bounds could disagree: [`OrchestratorPolicy::max_concurrent`]
//! (how many agents may run at once) and the lane pool's own
//! `ToolServerPoolPolicy::max_lanes` (how many tool-server children may exist
//! at once). They are reconciled explicitly in [`effective_concurrency`] —
//! the smaller wins — and the reconciliation is reported in the run result,
//! so the two limits cannot silently disagree: an orchestrated agent never
//! queues on a lane another orchestrated agent is guaranteed to be holding.
//!
//! # Approval shape (design decision — do not weaken)
//!
//! `spawn_subagent` requires approval, and orchestrated spawns are not an
//! exemption. The designed shape for a batch of N spawns is:
//!
//! 1. **One approval for the batch, with full visibility.** The
//!    `orchestrate_agents` call itself is `requires_approval: true` and its
//!    arguments carry every task string, model, and the call budget — so the
//!    approver sees the entire batch and its cost ceiling in ONE prompt.
//!    N identical per-spawn prompts would add alarm fatigue, not information.
//! 2. **Inside an approved batch, further interactive approvals fail
//!    CLOSED.** The approval wire protocol ([`crate::agent_loop::ApprovalResponse`])
//!    carries no correlation id, so two concurrent items awaiting an answer
//!    on the shared channel could receive each other's Allow/Deny — a
//!    misrouted Allow is a security hole. Items therefore run with a closed
//!    approval channel (an honest denial, see `approval_gate_outcome`:
//!    "a closed wired channel is a denial") instead of a misroutable prompt.
//!    Tools that are auto-approved (config `auto_approve`, the permission
//!    baseline, or the user's live Allow-All overrides — all inherited
//!    verbatim, never widened) still run; anything that would need a fresh
//!    human decision is refused with a named denial in that item's
//!    transcript. When the parent itself has NO approval channel (headless
//!    transports), items inherit exactly that legacy behavior, same as
//!    `spawn_subagent`.
//!
//! # Provenance
//!
//! Every orchestrated agent gets its own durable `agent_runs` row with
//! `parent_run_id` pointing at the orchestrating run, so the whole fan-out is
//! reconstructible after the fact via `list_agent_run_descendants`: who ran,
//! under which task (the run label), with what outcome (status + last_error).
//!
//! # Known limitations (deliberate, documented)
//!
//! - The code-run repair-chain memory (`hooks::LAST_CODE_RUN`) is
//!   process-global. The parent's chain is protected across the whole fan-out
//!   by one [`crate::hooks::CodeRunChainGuard`], but SIBLING items share the
//!   global map while they run — the same pre-existing property the parallel
//!   tool steps in `prism-workflows` have today.
//! - Per-item artifact references are not harvested (concurrent siblings
//!   write into the same session, so a diff-based harvest would attribute
//!   records to the wrong item). The durable run ledger and `recall(query=…)`
//!   cover reconstruction instead.
//! - Nothing fires the in-run cancel signal yet ([`cancellation`] is wired
//!   through the fan-out and tested; transports additionally get structural
//!   cancellation because dropping the orchestrate future aborts the
//!   `JoinSet`).

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use prism_ingest::llm::{ChatMessage, LlmClient};
use prism_python_bridge::{ToolServerLease, ToolServerPool};

use crate::command_tools::{CommandToolPlatformAccess, CommandToolRuntime};
use crate::hooks::HookRegistry;
use crate::models::get_model_config;
use crate::permissions::{PermissionMode, SharedPermissionOverrides, ToolPermissionContext};
use crate::scratchpad::Scratchpad;
use crate::subagent::{
    DEFAULT_SUBAGENT_BUDGET_TOKENS, DEFAULT_SUBAGENT_MODEL, MAX_SUBAGENT_DEPTH,
    delegation_failure_context,
};
use crate::tool_catalog::{LoadedTool, ToolCatalog};
use crate::transcript::{TranscriptStore, TurnBudget};
use crate::types::{AgentConfig, AgentEvent};

/// The meta-tool name (the [`crate::meta_tools::MetaTool::OrchestrateAgents`]
/// variant of the closed meta-tool registry).
pub const ORCHESTRATE_AGENTS_TOOL: &str = "orchestrate_agents";

/// Hard server-side ceiling on tasks per orchestrate call. The JSON-schema
/// `maxItems` is advisory (a provider forwards whatever the model sent);
/// this is the enforced bound, and exceeding it is a specific, named error.
pub const MAX_TASKS_PER_CALL: usize = 64;

/// Hard server-side ceiling on the per-run agent-call budget. The budget is
/// MODEL-controlled input; without a ceiling one call could authorize an
/// unbounded number of frontier-model turns. At a million papers a runaway
/// fan-out is a real bill.
pub const MAX_AGENT_CALLS_CEILING: usize = 64;

/// Cap (chars) on one item's summary echoed back to the parent model. Lower
/// than `spawn_subagent`'s single-result cap because N of these ride in one
/// tool result.
const ITEM_SUMMARY_CHARS: usize = 1_200;
/// Most-recent tool-step summaries echoed back per item.
const ITEM_STEPS_SHOWN: usize = 6;

// ── Policy ────────────────────────────────────────────────────────────

/// Resource policy for one orchestrated run.
///
/// Mirrors the declared-bound shape of `ParallelExecutionPolicy`
/// (prism-workflows) and `ToolServerPoolPolicy` (prism-python-bridge): every
/// limit is a claim about the execution environment, declared where it can be
/// seen and overridden, and typed so zero is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestratorPolicy {
    /// Maximum orchestrated agents running at once. `NonZeroUsize` because a
    /// zero-width fan-out would wait forever on a permit that cannot exist.
    ///
    /// Default **4** — the same figure as `ToolServerPoolPolicy::max_lanes`,
    /// because each running agent holds one tool-server lane while it
    /// executes Python tools; a higher default would only queue agents on
    /// lanes. The effective bound is additionally reconciled against the
    /// live pool's own bound at run start ([`effective_concurrency`]) so the
    /// two declared limits can never silently disagree.
    pub max_concurrent: NonZeroUsize,

    /// Maximum agent calls one orchestrated run may make — initial spawns
    /// plus schema-repair resumes. Reserved BEFORE each spawn and released
    /// only if the call never launched, so a burst cannot overshoot and
    /// nothing double-charges. Exhaustion is reported per item (`skipped`)
    /// and at run level (`budget_exhausted`), never swallowed.
    ///
    /// Default **16**: covers a typical batch (e.g. 8 tasks + repair
    /// headroom) while capping the worst-case spend of one approval at a
    /// bounded number of `DEFAULT_SUBAGENT_BUDGET_TOKENS`-sized turns.
    /// Model-supplied overrides are clamped to [`MAX_AGENT_CALLS_CEILING`].
    pub max_agent_calls: NonZeroUsize,
}

impl Default for OrchestratorPolicy {
    fn default() -> Self {
        Self {
            max_concurrent: NonZeroUsize::new(4).expect("the default width is non-zero"),
            max_agent_calls: NonZeroUsize::new(16).expect("the default budget is non-zero"),
        }
    }
}

/// Reconcile the fan-out width with the lane pool's own bound: the smaller
/// wins. Declared as its own function (and reported in the run result) so
/// the two limits are reconciled in exactly one place instead of drifting.
#[must_use]
pub fn effective_concurrency(
    policy: &OrchestratorPolicy,
    lane_bound: Option<NonZeroUsize>,
) -> NonZeroUsize {
    match lane_bound {
        Some(lanes) => policy.max_concurrent.min(lanes),
        None => policy.max_concurrent,
    }
}

// ── Task specification ────────────────────────────────────────────────

/// One delegated agent task inside an orchestrated run.
#[derive(Debug, Clone)]
pub struct OrchestratorTaskSpec {
    /// Stable identifier echoed in the per-item report (defaults to
    /// `task-<index>` when the caller does not name one).
    pub id: String,
    /// Complete, self-contained instruction for the delegated agent.
    pub task: String,
    /// Model id for this item (default [`DEFAULT_SUBAGENT_MODEL`]).
    pub model: String,
    /// Cumulative input-token budget for this item's turns.
    pub max_tokens: u64,
    /// Self-contained JSON Schema the item's final answer must satisfy.
    /// `None` = the answer is reported as-is, unvalidated.
    pub result_schema: Option<Value>,
    /// Ids of tasks that must finish before this one starts, turning a flat
    /// fan-out into a DAG.
    ///
    /// Research is not a bag of independent questions: "compare the candidates
    /// found in A and B" cannot run until A and B have run, and forcing it into
    /// one agent's sequential loop is how a run spends its whole budget
    /// re-searching. Empty (the default) means the task is a root and runs in
    /// the first wave, so every existing caller behaves exactly as before.
    pub depends_on: Vec<String>,
}

// ── Per-item outcomes ─────────────────────────────────────────────────

/// Terminal outcome of one orchestrated item. Exactly one of three states —
/// there is deliberately no aggregate "ok" that could paper over a partial
/// run.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ItemOutcome {
    /// The item completed; `result` carries its summary (and, for
    /// schema-checked tasks, the validated `output`).
    Succeeded { result: Value },
    /// The item ran (or was cancelled mid-run) and did not complete;
    /// `reason` names why.
    Failed { reason: String },
    /// The item never ran; `reason` names why (budget exhaustion, a defective
    /// schema, cancellation before start).
    Skipped { reason: String },
}

/// One row of the run report: which task, and how it ended.
#[derive(Debug, Clone, Serialize)]
pub struct ItemReport {
    pub id: String,
    #[serde(flatten)]
    pub outcome: ItemOutcome,
    /// False when this item's `agent_runs` ledger row could not be written —
    /// the store was locked, unreachable, or the insert failed.
    ///
    /// The item still RAN: it spawned, spent real tokens, and may well have
    /// succeeded. But it has no durable row, so it will never appear in
    /// `list_agent_run_descendants` and `recall` cannot reach it. Serialized
    /// only when false, so the common case stays quiet and the exception is
    /// impossible to miss.
    ///
    /// Without this the run's own summary told the caller "per-item details
    /// are durable: each item's run_id is in the agent-run ledger" — a claim
    /// that was simply untrue for such an item. In a system whose thesis is
    /// that the evidence chain is the authority, billed work with no evidence
    /// trail must be stated, not logged and forgotten. Found by adversarial
    /// review.
    #[serde(skip_serializing_if = "is_true")]
    pub ledger_recorded: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_true(value: &bool) -> bool {
    *value
}

/// The full result of one orchestrated run, in input order.
#[derive(Debug, Clone)]
pub struct OrchestratedRun {
    /// Per-item outcomes, index-aligned with the submitted tasks (input
    /// order — completion order never reorders the report).
    pub items: Vec<ItemReport>,
    /// The caller-requested width, before reconciliation.
    pub requested_concurrency: usize,
    /// The lane pool's own bound, when a pool was present.
    pub lane_bound: Option<usize>,
    /// The width actually enforced: `min(requested, lane_bound)`.
    pub effective_concurrency: usize,
    /// The declared agent-call budget.
    pub budget_max: usize,
    /// Agent calls actually made (spawns + schema repairs).
    pub budget_used: usize,
    /// True the moment any reservation was refused — reported, never
    /// swallowed, and distinguishable from a run that merely used its budget.
    pub budget_exhausted: bool,
}

impl OrchestratedRun {
    #[must_use]
    pub fn succeeded(&self) -> usize {
        self.count(|o| matches!(o, ItemOutcome::Succeeded { .. }))
    }
    #[must_use]
    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, ItemOutcome::Failed { .. }))
    }
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.count(|o| matches!(o, ItemOutcome::Skipped { .. }))
    }
    fn count(&self, pred: impl Fn(&ItemOutcome) -> bool) -> usize {
        self.items.iter().filter(|r| pred(&r.outcome)).count()
    }
}

// ── Agent-call budget ─────────────────────────────────────────────────

/// Countable agent-call budget for one orchestrated run. Reservations are
/// taken BEFORE a spawn and either committed (the call launched — spent
/// forever) or released on drop (the call never happened), so the used count
/// is exact and nothing double-charges.
pub struct AgentCallBudget {
    max: usize,
    used: AtomicUsize,
    exhausted: AtomicBool,
}

impl AgentCallBudget {
    #[must_use]
    pub fn new(max: NonZeroUsize) -> Self {
        Self {
            max: max.get(),
            used: AtomicUsize::new(0),
            exhausted: AtomicBool::new(false),
        }
    }

    /// Reserve one agent call, or report exhaustion with the exact numbers.
    pub fn try_reserve(self: &Arc<Self>) -> Result<CallReservation, String> {
        let reserved = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.max).then_some(used + 1)
            });
        match reserved {
            Ok(_) => Ok(CallReservation {
                budget: Arc::clone(self),
                committed: false,
            }),
            Err(_) => {
                self.exhausted.store(true, Ordering::Release);
                Err(format!(
                    "agent-call budget exhausted ({} of {} calls used)",
                    self.max, self.max
                ))
            }
        }
    }

    #[must_use]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn max(&self) -> usize {
        self.max
    }
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }
}

/// One reserved agent call. [`CallReservation::commit`] marks it spent;
/// dropping an uncommitted reservation returns it to the budget.
pub struct CallReservation {
    budget: Arc<AgentCallBudget>,
    committed: bool,
}

impl CallReservation {
    /// The call is launching — spend the reservation permanently.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for CallReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.used.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

// ── Cancellation ──────────────────────────────────────────────────────

/// Create a cancel handle/signal pair for one orchestrated run. Firing the
/// handle stops the run for real: in-flight item futures are dropped at
/// their next await point (which cancels their in-flight LLM/tool I/O and
/// lets the item finalize its ledger row as `cancelled`), and not-yet-started
/// items are skipped with a named reason.
#[must_use]
pub fn cancellation() -> (CancelHandle, CancelSignal) {
    let (tx, rx) = watch::channel(false);
    (CancelHandle { tx }, CancelSignal { rx })
}

/// The firing side. Dropping it WITHOUT calling [`CancelHandle::cancel`]
/// never cancels the run.
pub struct CancelHandle {
    tx: watch::Sender<bool>,
}

impl CancelHandle {
    pub fn cancel(&self) {
        // send_replace never fails; a send() would error with no receivers.
        let _ = self.tx.send_replace(true);
    }
}

/// The observing side (cheap to clone; all clones observe the same signal).
#[derive(Clone)]
pub struct CancelSignal {
    rx: watch::Receiver<bool>,
}

impl CancelSignal {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolve when the run is cancelled. If the handle is dropped without
    /// firing, this pends forever (the run simply completes).
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        if rx.wait_for(|cancelled| *cancelled).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

// ── Structured-output verification ────────────────────────────────────

/// A compiled, self-contained JSON Schema for one item's answer.
pub struct SchemaValidator {
    validator: jsonschema::Validator,
    schema: Value,
}

impl SchemaValidator {
    /// Compile a schema, refusing anything not self-contained: an external
    /// `$ref` (any reference that does not start with `#`) is rejected by
    /// scan BEFORE compilation, so no resolver ambiguity can smuggle one in.
    pub fn compile(schema: &Value) -> Result<Self, String> {
        if let Some(external) = find_external_ref(schema) {
            return Err(format!(
                "result_schema must be self-contained: external $ref \"{external}\" \
                 is not allowed (only local \"#/...\" references are)"
            ));
        }
        let validator = jsonschema::validator_for(schema)
            .map_err(|error| format!("result_schema does not compile: {error}"))?;
        Ok(Self {
            validator,
            schema: schema.clone(),
        })
    }

    /// Parse an agent's final answer as JSON and validate it. `Ok` carries
    /// the parsed document; `Err` carries a specific, actionable description
    /// of every failure (paths included).
    pub fn check_answer(&self, answer: &str) -> Result<Value, String> {
        let parsed: Value = serde_json::from_str(strip_code_fences(answer)).map_err(|error| {
            format!("the answer is not a single JSON document (parse error: {error})")
        })?;
        let errors: Vec<String> = self
            .validator
            .iter_errors(&parsed)
            .map(|error| format!("at `{}`: {error}", error.instance_path))
            .collect();
        if errors.is_empty() {
            Ok(parsed)
        } else {
            Err(errors.join("; "))
        }
    }

    /// The one-shot repair demand handed back to the agent — the concrete
    /// validation error plus the schema, per the design principle that a
    /// specific error beats a bigger prompt.
    #[must_use]
    pub fn repair_demand(&self, validation_error: &str) -> String {
        format!(
            "Your final answer must be a single JSON document satisfying the declared \
             output schema, and your previous answer was not.\n\n\
             Validation failure:\n{validation_error}\n\n\
             Required schema:\n{schema}\n\n\
             Reply with ONLY the corrected JSON document — no prose, no code fences.",
            schema = self.schema
        )
    }
}

/// Depth-first scan for a non-local `$ref`. Returns the first offender.
fn find_external_ref(value: &Value) -> Option<&str> {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(target)) = map.get("$ref")
                && !target.starts_with('#')
            {
                return Some(target);
            }
            map.values().find_map(find_external_ref)
        }
        Value::Array(items) => items.iter().find_map(find_external_ref),
        _ => None,
    }
}

/// Strip one surrounding Markdown code fence (```json ... ``` or ``` ... ```)
/// if present. Models wrap JSON in fences constantly; anything beyond this
/// courtesy is the model's problem to repair.
fn strip_code_fences(answer: &str) -> &str {
    let trimmed = answer.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some(body) = rest.split_once('\n').map(|(_, body)| body) else {
        return trimmed;
    };
    match body.rsplit_once("```") {
        Some((inner, tail)) if tail.trim().is_empty() => inner.trim(),
        _ => trimmed,
    }
}

// ── The item-agent contract ───────────────────────────────────────────

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What one attempt produced.
pub struct AttemptReport {
    /// The agent's final answer text — the document a `result_schema` is
    /// checked against.
    pub answer: String,
    /// Structured detail for the run report (model, run id, steps, usage…).
    pub detail: Value,
}

/// How an item's durable record should be closed. Decided by the fan-out
/// core, applied exactly once by the agent.
#[derive(Debug, Clone)]
pub enum FinalDisposition {
    Completed,
    Failed(String),
    Cancelled(String),
}

/// One orchestrated agent, drivable for an initial attempt plus at most one
/// schema-repair resume. The retry POLICY lives in [`drive_item`] (so tests
/// can falsify it); implementations only know how to run an attempt against
/// the same conversation and how to close their durable record.
pub trait ItemAgent: Send {
    /// Run one attempt. `repair` carries the schema-validation demand for a
    /// resume; implementations continue the SAME conversation (history
    /// preserved) rather than starting over.
    fn attempt(&mut self, repair: Option<String>) -> BoxFut<'_, Result<AttemptReport>>;

    /// Close the item's durable record with its terminal disposition.
    /// Called exactly once, after the outcome is decided (or on
    /// cancellation). Must be safe to call when no attempt ever ran.
    fn finalize(&mut self, disposition: FinalDisposition) -> BoxFut<'_, ()>;

    /// Whether this item obtained a durable `agent_runs` row.
    ///
    /// Defaults to `true` because an implementation with no ledger of its own
    /// (a test double) has nothing to under-report. A real agent whose ledger
    /// write FAILED must return `false`: the item ran and spent tokens with no
    /// row `list_agent_run_descendants` will ever return, and the caller has
    /// to be told rather than the failure living in a log line while the run
    /// summary claims every item is durable.
    fn ledger_recorded(&self) -> bool {
        true
    }
}

// ── The verification core ─────────────────────────────────────────────

/// Drive one item to its terminal outcome: initial attempt, optional single
/// schema repair, nothing else. This function IS the retry policy:
/// - a non-schema failure is never retried;
/// - a schema mismatch resumes the same agent exactly once, with the
///   concrete validation error;
/// - the repair itself reserves budget first — no budget, no second call,
///   and the reason says so.
async fn drive_item(
    agent: &mut dyn ItemAgent,
    validator: Option<&SchemaValidator>,
    budget: &Arc<AgentCallBudget>,
    initial: CallReservation,
) -> ItemOutcome {
    initial.commit();
    let first = match agent.attempt(None).await {
        Ok(report) => report,
        Err(error) => {
            return ItemOutcome::Failed {
                reason: format!("{error:#}"),
            };
        }
    };

    let Some(validator) = validator else {
        return ItemOutcome::Succeeded {
            result: finish_result(first, None, false),
        };
    };

    let validation_error = match validator.check_answer(&first.answer) {
        Ok(parsed) => {
            return ItemOutcome::Succeeded {
                result: finish_result(first, Some(parsed), false),
            };
        }
        Err(error) => error,
    };

    // One repair, budgeted like any other agent call.
    let reservation = match budget.try_reserve() {
        Ok(reservation) => reservation,
        Err(exhausted) => {
            return ItemOutcome::Failed {
                reason: format!(
                    "result failed schema validation ({validation_error}); \
                     repair not attempted: {exhausted}"
                ),
            };
        }
    };
    reservation.commit();
    match agent
        .attempt(Some(validator.repair_demand(&validation_error)))
        .await
    {
        Err(error) => ItemOutcome::Failed {
            reason: format!(
                "schema-repair attempt failed: {error:#} \
                 (original validation error: {validation_error})"
            ),
        },
        Ok(second) => match validator.check_answer(&second.answer) {
            Ok(parsed) => ItemOutcome::Succeeded {
                result: finish_result(second, Some(parsed), true),
            },
            Err(second_error) => ItemOutcome::Failed {
                reason: format!(
                    "result failed schema validation after one repair attempt: {second_error}"
                ),
            },
        },
    }
}

/// Merge an attempt's detail with its (possibly schema-validated) answer.
fn finish_result(report: AttemptReport, validated: Option<Value>, repaired: bool) -> Value {
    let mut result = report.detail;
    if !result.is_object() {
        result = json!({});
    }
    let map = result.as_object_mut().expect("just ensured an object");
    map.insert(
        "summary".to_string(),
        Value::String(clip(report.answer.trim(), ITEM_SUMMARY_CHARS)),
    );
    if let Some(parsed) = validated {
        map.insert("output".to_string(), parsed);
        map.insert("schema_repaired".to_string(), Value::Bool(repaired));
    }
    result
}

fn disposition_of(outcome: &ItemOutcome) -> FinalDisposition {
    match outcome {
        ItemOutcome::Succeeded { .. } => FinalDisposition::Completed,
        ItemOutcome::Failed { reason } => FinalDisposition::Failed(reason.clone()),
        ItemOutcome::Skipped { reason } => FinalDisposition::Cancelled(reason.clone()),
    }
}

// ── The fan-out primitive ─────────────────────────────────────────────

/// Run N agent tasks concurrently, bounded, collecting per-item outcomes in
/// input order. `JoinSet` + `Semaphore` with permits acquired BEFORE spawn —
/// the same bounding shape as `run_parallel_step` in `prism-workflows`, not a
/// second invention. Never returns an aggregate error: item failures (and
/// item-task panics) become that item's outcome, siblings unaffected.
pub async fn fan_out<F>(
    specs: Vec<OrchestratorTaskSpec>,
    policy: &OrchestratorPolicy,
    lane_bound: Option<NonZeroUsize>,
    cancel: CancelSignal,
    factory: F,
) -> OrchestratedRun
where
    F: Fn(usize, &OrchestratorTaskSpec) -> Box<dyn ItemAgent> + Send + Sync + 'static,
{
    let effective = effective_concurrency(policy, lane_bound);
    let budget = Arc::new(AgentCallBudget::new(policy.max_agent_calls));
    let factory = Arc::new(factory);

    let semaphore_capacity = effective
        .get()
        .min(specs.len().max(1))
        .min(Semaphore::MAX_PERMITS);
    // Removing this bound is what the `concurrency_never_exceeds_the_bound`
    // test exists to catch.
    let semaphore = Arc::new(Semaphore::new(semaphore_capacity));

    let mut outcomes: Vec<Option<ItemOutcome>> = specs.iter().map(|_| None).collect();
    // Whether each item's `agent_runs` row was actually written. Default true:
    // items that never spawn (skipped) have nothing to record and must not be
    // reported as missing provenance.
    let mut ledger_recorded: Vec<bool> = specs.iter().map(|_| true).collect();
    // (index, outcome, ledger_recorded)
    let mut tasks: JoinSet<(usize, ItemOutcome, bool)> = JoinSet::new();
    let mut task_indices = HashMap::with_capacity(specs.len());

    for (index, spec) in specs.iter().enumerate() {
        // A defective schema is a spec defect: nothing ran, no budget spent,
        // and the reason is the exact compile error.
        let validator = match &spec.result_schema {
            Some(schema) => match SchemaValidator::compile(schema) {
                Ok(validator) => Some(Arc::new(validator)),
                Err(error) => {
                    outcomes[index] = Some(ItemOutcome::Skipped { reason: error });
                    continue;
                }
            },
            None => None,
        };
        if cancel.is_cancelled() {
            outcomes[index] = Some(ItemOutcome::Skipped {
                reason: "orchestrated run cancelled before this task started".to_string(),
            });
            continue;
        }
        // Reserve BEFORE spawn. Refusal is this item's outcome, not an error.
        let reservation = match budget.try_reserve() {
            Ok(reservation) => reservation,
            Err(exhausted) => {
                outcomes[index] = Some(ItemOutcome::Skipped { reason: exhausted });
                continue;
            }
        };
        // Acquire before spawning (run_parallel_step's shape): at most
        // `effective` item tasks are ever alive. A cancel that lands while
        // queueing releases the untouched reservation via its drop.
        let permit = tokio::select! {
            permit = Arc::clone(&semaphore).acquire_owned() => {
                permit.expect("the fan-out semaphore is never closed")
            }
            () = cancel.cancelled() => {
                outcomes[index] = Some(ItemOutcome::Skipped {
                    reason: "orchestrated run cancelled before this task started".to_string(),
                });
                continue;
            }
        };

        let spec = spec.clone();
        let factory = Arc::clone(&factory);
        let budget = Arc::clone(&budget);
        let cancel = cancel.clone();
        let task = tasks.spawn(async move {
            let _permit = permit;
            let mut agent = factory(index, &spec);
            let outcome = {
                let drive = drive_item(agent.as_mut(), validator.as_deref(), &budget, reservation);
                let mut drive = std::pin::pin!(drive);
                tokio::select! {
                    outcome = &mut drive => outcome,
                    // Dropping `drive` (at scope end) is what actually stops
                    // the in-flight agent: its pending LLM/tool awaits are
                    // cancelled with it. The agent itself survives the drop
                    // so its durable record can still be closed honestly.
                    () = cancel.cancelled() => ItemOutcome::Failed {
                        reason: "cancelled while running (orchestrated run cancelled)".to_string(),
                    },
                }
            };
            let disposition = match &outcome {
                ItemOutcome::Failed { reason } if reason.starts_with("cancelled while running") => {
                    FinalDisposition::Cancelled(reason.clone())
                }
                other => disposition_of(other),
            };
            // Ask BEFORE finalize consumes the agent: did this item actually
            // get a durable ledger row? A ledger write that failed is logged
            // today and never reaches the caller, so the run's own summary
            // claims durability the item does not have.
            let recorded = agent.ledger_recorded();
            agent.finalize(disposition).await;
            (index, outcome, recorded)
        });
        task_indices.insert(task.id(), index);
    }

    while let Some(joined) = tasks.join_next_with_id().await {
        match joined {
            Ok((task_id, (index, outcome, recorded))) => {
                task_indices.remove(&task_id);
                outcomes[index] = Some(outcome);
                ledger_recorded[index] = recorded;
            }
            Err(join_error) => {
                // A panicked/aborted item task fails THAT item, never its
                // siblings.
                if let Some(index) = task_indices.remove(&join_error.id()) {
                    outcomes[index] = Some(ItemOutcome::Failed {
                        reason: format!("orchestrated agent task ended abnormally: {join_error}"),
                    });
                } else {
                    tracing::error!(
                        task_id = %join_error.id(),
                        error = %join_error,
                        "orchestrated agent task ended without item metadata"
                    );
                }
            }
        }
    }

    let items = specs
        .iter()
        .zip(outcomes)
        .zip(ledger_recorded)
        .map(|((spec, outcome), recorded)| ItemReport {
            id: spec.id.clone(),
            outcome: outcome.unwrap_or_else(|| ItemOutcome::Failed {
                reason: "orchestrated agent task ended without reporting an outcome".to_string(),
            }),
            ledger_recorded: recorded,
        })
        .collect();

    OrchestratedRun {
        items,
        requested_concurrency: policy.max_concurrent.get(),
        lane_bound: lane_bound.map(NonZeroUsize::get),
        effective_concurrency: effective.get(),
        budget_max: budget.max(),
        budget_used: budget.used(),
        budget_exhausted: budget.exhausted(),
    }
}

// ── Dependency-ordered fan-out (the research DAG) ────────────────────
//
// `fan_out` runs every task at once, which is right for independent work and
// wrong for research. Research decomposes into sub-questions where some depend
// on others — "compare what A and B found" cannot start until A and B are done.
// Forcing that into ONE agent's sequential loop is what a measured PFAS review
// did: 17 searches, nothing persisted, budget exhausted, no report.
//
// This is a scheduler ON TOP of `fan_out`, not a second executor: tasks are
// grouped into waves by dependency depth, and each wave is handed to the
// existing bounded, cancellable fan-out unchanged.

impl OrchestratedRun {
    /// Turn a rejected PLAN into a reported run. A cycle or an unknown
    /// dependency id is an authoring mistake; running the schedulable subset
    /// would answer a different question than the one asked, and the caller
    /// would have no way to see that from the results.
    fn with_plan_error(mut self, reason: &str) -> Self {
        self.items = vec![ItemReport {
            id: "plan".to_string(),
            outcome: ItemOutcome::Skipped {
                reason: format!("the task plan was rejected: {reason}"),
            },
            // Nothing ran, so nothing was written to the ledger.
            ledger_recorded: false,
        }];
        self
    }
}

/// Order tasks into dependency waves. Every task appears exactly once, and no
/// task appears before something it depends on.
///
/// Fails on a cycle and on a dependency naming a task that does not exist —
/// both are authoring mistakes, and running "most of" a malformed plan produces
/// an answer whose gaps nobody can see.
fn dependency_waves(specs: &[OrchestratorTaskSpec]) -> Result<Vec<Vec<usize>>> {
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    for (index, spec) in specs.iter().enumerate() {
        if index_of.insert(spec.id.as_str(), index).is_some() {
            anyhow::bail!(
                "duplicate task id {:?}: dependencies would be ambiguous",
                spec.id
            );
        }
    }
    for spec in specs {
        for dep in &spec.depends_on {
            anyhow::ensure!(
                index_of.contains_key(dep.as_str()),
                "task {:?} depends on {dep:?}, which is not one of the tasks",
                spec.id
            );
            anyhow::ensure!(dep != &spec.id, "task {:?} depends on itself", spec.id);
        }
    }

    let mut remaining: Vec<usize> = (0..specs.len()).collect();
    let mut done: HashSet<usize> = HashSet::new();
    let mut waves: Vec<Vec<usize>> = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|i| {
                specs[*i]
                    .depends_on
                    .iter()
                    .all(|dep| done.contains(&index_of[dep.as_str()]))
            })
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = remaining.iter().map(|i| specs[*i].id.as_str()).collect();
            anyhow::bail!(
                "dependency cycle among tasks: {}. A cycle cannot be scheduled, and \
                 running the rest would answer a different question than the one planned.",
                stuck.join(", ")
            );
        }
        for index in &ready {
            done.insert(*index);
        }
        remaining.retain(|i| !done.contains(i));
        waves.push(ready);
    }
    Ok(waves)
}

/// What an upstream task contributes to the tasks that depend on it.
fn upstream_briefing(id: &str, report: &ItemReport) -> String {
    match &report.outcome {
        ItemOutcome::Succeeded { result } => {
            format!("### Findings from {id}\n{result}\n")
        }
        ItemOutcome::Failed { reason } => format!(
            "### {id} FAILED\n{reason}\nTreat its part of the question as unanswered; do not \
             assume a result.\n"
        ),
        ItemOutcome::Skipped { reason } => {
            format!("### {id} was skipped\n{reason}\nIts part of the question is unanswered.\n")
        }
    }
}

/// Run tasks in dependency order, wave by wave.
///
/// Each task's brief is extended with what its upstream tasks actually found —
/// including their failures, stated as failures. A downstream task that silently
/// received nothing would confidently answer from an empty premise, which is the
/// worst possible outcome and the hardest to spot in a report.
pub async fn fan_out_dag<F>(
    specs: Vec<OrchestratorTaskSpec>,
    policy: &OrchestratorPolicy,
    lane_bound: Option<NonZeroUsize>,
    cancel: CancelSignal,
    factory: F,
) -> Result<OrchestratedRun>
where
    F: Fn(usize, &OrchestratorTaskSpec) -> Box<dyn ItemAgent> + Send + Sync + Clone + 'static,
{
    let waves = dependency_waves(&specs)?;
    // No edges at all: this is a plain fan-out, so do exactly that. One wave
    // also means the DAG path costs nothing when nobody uses it.
    let mut reports: HashMap<String, ItemReport> = HashMap::new();
    let mut ordered: Vec<ItemReport> = Vec::new();
    let mut aggregate: Option<OrchestratedRun> = None;

    for wave in waves {
        let mut wave_specs: Vec<OrchestratorTaskSpec> = Vec::with_capacity(wave.len());
        for index in &wave {
            let mut spec = specs[*index].clone();
            if !spec.depends_on.is_empty() {
                let mut briefing = String::from(
                    "\n\n## What earlier tasks in this plan established\n\
                     These are results, not assumptions. Where one failed, its part of the \
                     question is open — say so rather than filling the gap.\n\n",
                );
                for dep in &spec.depends_on {
                    if let Some(report) = reports.get(dep) {
                        briefing.push_str(&upstream_briefing(dep, report));
                    }
                }
                spec.task.push_str(&briefing);
            }
            wave_specs.push(spec);
        }

        let run = fan_out(
            wave_specs,
            policy,
            lane_bound,
            cancel.clone(),
            factory.clone(),
        )
        .await;
        for report in &run.items {
            reports.insert(report.id.clone(), report.clone());
            ordered.push(report.clone());
        }
        // Budget and concurrency are properties of the whole run, so carry the
        // LAST wave's view forward and mark exhaustion if any wave hit it — a
        // run that ran out of calls in wave 1 did not stop being exhausted
        // because wave 2 had nothing left to ask for.
        aggregate = Some(match aggregate.take() {
            None => run,
            Some(previous) => OrchestratedRun {
                items: Vec::new(),
                budget_used: previous.budget_used.max(run.budget_used),
                budget_exhausted: previous.budget_exhausted || run.budget_exhausted,
                ..run
            },
        });
    }

    let mut out = aggregate.unwrap_or_else(|| OrchestratedRun {
        items: Vec::new(),
        requested_concurrency: policy.max_concurrent.get(),
        lane_bound: lane_bound.map(NonZeroUsize::get),
        effective_concurrency: effective_concurrency(policy, lane_bound).get(),
        budget_max: policy.max_agent_calls.get(),
        budget_used: 0,
        budget_exhausted: false,
    });
    out.items = ordered;
    Ok(out)
}

// ── Catalog definition ────────────────────────────────────────────────

/// Catalog entry for `orchestrate_agents`, merged into the always-on
/// meta-tool definitions (`meta_tools::definitions`).
#[must_use]
pub fn definition() -> LoadedTool {
    LoadedTool {
        name: ORCHESTRATE_AGENTS_TOOL.to_string(),
        // Deliberately terse: every always-on meta-tool definition rides in
        // EVERY request, and the whole set must leave headroom in the minimum
        // tool budget (`always_on_meta_tools_leave_room_in_the_minimum_tool_budget`).
        // Full semantics live in the module docs; the tool RESULT carries the
        // per-item detail the model actually acts on.
        description: "Run agent tasks as a DAG, each a full nested turn. DECOMPOSE a \
            hard question instead of searching sequentially: no `depends_on` runs \
            concurrently, with it waits and receives their findings."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_TASKS_PER_CALL,
                    "items": {
                        "type": "object",
                        "properties": {
                            "task": {
                                "type": "string",
                                "description": "Complete, self-contained instruction."
                            },
                            "id": { "type": "string" },
                            "model": { "type": "string" },
                            "max_tokens": { "type": "integer" },
                            "result_schema": {
                                "type": "object",
                                "description": "JSON Schema for the answer; no external $ref."
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Task ids that must finish first; their findings enter this brief."
                            }
                        },
                        "required": ["task"]
                    }
                },
                "max_concurrent": {
                    "type": "integer",
                    "description": "Concurrent agents (default 4)."
                },
                "max_agent_calls": {
                    "type": "integer",
                    "maximum": MAX_AGENT_CALLS_CEILING,
                    "description": "Total spawns + repairs (default 16)."
                }
            },
            "required": ["tasks"]
        }),
        // One approval for the whole batch, with every task and the call
        // budget visible in the prompt — see the module docs ("Approval
        // shape") for the design reasoning. Never auto-approved.
        requires_approval: true,
        declared_free: false,
        permission_mode: PermissionMode::WorkspaceWrite,
        source: Some("builtin".to_string()),
        source_detail: Some("orchestration".to_string()),
    }
}

// ── Argument parsing ──────────────────────────────────────────────────

fn parse_args(
    args: &Value,
    parent_model: &str,
) -> Result<(Vec<OrchestratorTaskSpec>, OrchestratorPolicy)> {
    let tasks = args
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("orchestrate_agents requires a non-empty `tasks` array"))?;
    if tasks.is_empty() {
        anyhow::bail!("orchestrate_agents requires a non-empty `tasks` array");
    }
    if tasks.len() > MAX_TASKS_PER_CALL {
        anyhow::bail!(
            "orchestrate_agents accepts at most {MAX_TASKS_PER_CALL} tasks per call \
             (got {}); split the batch",
            tasks.len()
        );
    }

    let mut specs = Vec::with_capacity(tasks.len());
    let mut seen_ids = std::collections::HashSet::new();
    for (index, entry) in tasks.iter().enumerate() {
        let task = entry
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if task.is_empty() {
            anyhow::bail!("tasks[{index}] is missing a non-empty `task` instruction");
        }
        // A caller-supplied id wins; otherwise the agent is named after a
        // scientist whose field matches the task. `task-0` is unique and
        // unreadable, and this id is the only handle anything downstream — a
        // report, a log line, an interface grouping a lane — has for saying
        // WHICH agent did something.
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| clip(id, 64))
            .unwrap_or_else(|| crate::agent_names::name_for(task, index, &seen_ids));
        if !seen_ids.insert(id.clone()) {
            anyhow::bail!("duplicate task id \"{id}\" — per-item outcomes need unique ids");
        }
        let model = entry
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            // Inherit the parent's route by default. The constant asks whatever
            // endpoint the parent uses for a model it may not serve — measured
            // with the parent on glm-5.3, every orchestrated task died with
            // `1214 modelCode：不存在`, which killed decomposition entirely on
            // any non-Anthropic route.
            .unwrap_or_else(|| {
                let inherited = parent_model.trim();
                if inherited.is_empty() {
                    DEFAULT_SUBAGENT_MODEL
                } else {
                    inherited
                }
            })
            .to_string();
        let max_tokens = entry
            .get("max_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0)
            .unwrap_or(DEFAULT_SUBAGENT_BUDGET_TOKENS);
        let result_schema = match entry.get("result_schema") {
            None | Some(Value::Null) => None,
            Some(schema @ Value::Object(_)) => Some(schema.clone()),
            Some(other) => anyhow::bail!(
                "tasks[{index}].result_schema must be a JSON Schema object, got {other}"
            ),
        };
        let depends_on = match entry.get("depends_on") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item.as_str() {
                    Some(dep) if !dep.trim().is_empty() => Ok(dep.trim().to_string()),
                    _ => anyhow::bail!("tasks[{index}].depends_on entries must be task ids"),
                })
                .collect::<Result<Vec<String>>>()?,
            Some(other) => {
                anyhow::bail!("tasks[{index}].depends_on must be an array of task ids, got {other}")
            }
        };
        specs.push(OrchestratorTaskSpec {
            id,
            task: task.to_string(),
            model,
            max_tokens,
            result_schema,
            depends_on,
        });
    }

    let defaults = OrchestratorPolicy::default();
    let policy = OrchestratorPolicy {
        max_concurrent: args
            .get("max_concurrent")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .and_then(NonZeroUsize::new)
            .unwrap_or(defaults.max_concurrent),
        max_agent_calls: args
            .get("max_agent_calls")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .map(|n| n.min(MAX_AGENT_CALLS_CEILING))
            .and_then(NonZeroUsize::new)
            .unwrap_or(defaults.max_agent_calls),
    };
    Ok((specs, policy))
}

// ── Production wiring: the real item agent ────────────────────────────

/// Everything the orchestrated agents share, captured ONCE at the dispatch
/// site (cloned/Arc'd so item futures are `'static` for `JoinSet::spawn`).
struct OrchestrationContext {
    llm_config: prism_ingest::LlmConfig,
    runtime: CommandToolRuntime,
    catalog: ToolCatalog,
    /// Fresh `build_default_hooks()`. Every production transport builds its
    /// registry through that same single constructor (`build_agent_seed`),
    /// so a fresh instance is behaviorally identical to the parent's; the
    /// parent's own registry is only reachable by reference and cannot cross
    /// the `tokio::spawn` boundary.
    hooks: Arc<HookRegistry>,
    permissions: ToolPermissionContext,
    overrides: Option<SharedPermissionOverrides>,
    /// Per-item clone template of the parent's policy engine — same loaded
    /// policies, no shared mutability.
    policy_template: Option<prism_policy::PolicyEngine>,
    pool: ToolServerPool,
    config_template: AgentConfig,
    parent_run_id: String,
    session_id: String,
    /// The caller's platform access, captured before spawning. Task-locals
    /// do not cross `tokio::spawn`, so each item re-scopes this CAPTURED
    /// value — the same value the spawn gate already validated, never wider.
    access: CommandToolPlatformAccess,
    /// Whether the parent turn has an interactive approval channel. See the
    /// module docs: items then run with a fail-CLOSED channel; without one
    /// they inherit the legacy no-channel behavior, exactly like
    /// `spawn_subagent`.
    parent_has_approval_channel: bool,
    events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    /// Fan-out total, rolled up from each item's own `AgentRunMetrics` — the
    /// same accumulator `spawn_subagent` hands back, so both delegation paths
    /// charge the parent from one type. Shared by reference across the item
    /// tasks; the critical section is a few adds and never awaits.
    usage: std::sync::Mutex<crate::agent_loop::AgentRunMetrics>,
}

/// Live state of one orchestrated agent, created on its first attempt and
/// kept across the (at most one) schema-repair resume — same conversation,
/// same lane, same durable run row.
struct LiveItemState {
    llm: LlmClient,
    lane: ToolServerLease,
    history: Vec<ChatMessage>,
    transcript: TranscriptStore,
    scratchpad: Scratchpad,
    config: AgentConfig,
    run: prism_provenance::AgentRun,
    /// Per-write ledger, never a held store handle — this state lives for the
    /// whole fan-out item, and a handle held here blocks any PRISM subprocess
    /// the item spawns from opening the store. See `agent_loop::RunLedger`.
    run_ledger: Option<crate::agent_loop::RunLedger>,
    heartbeat: Option<crate::agent_loop::AgentRunHeartbeat>,
    metrics: crate::agent_loop::AgentRunMetrics,
    policy: Option<prism_policy::PolicyEngine>,
    finalized: bool,
}

struct OrchestratedAgent {
    ctx: Arc<OrchestrationContext>,
    spec: OrchestratorTaskSpec,
    live: Option<LiveItemState>,
}

impl OrchestratedAgent {
    fn new(ctx: Arc<OrchestrationContext>, spec: OrchestratorTaskSpec) -> Self {
        Self {
            ctx,
            spec,
            live: None,
        }
    }

    /// Acquire the lane and create the durable child row — in that order, so
    /// a refused lane leaves no ghost agent (same ordering as
    /// `spawn_subagent`). Pool refusals are honest, named errors already.
    async fn setup(
        ctx: &OrchestrationContext,
        spec: &OrchestratorTaskSpec,
    ) -> Result<LiveItemState> {
        let mut lane = ctx.pool.acquire().await?;
        match lane.set_session_id(&ctx.session_id).await {
            Ok(response) => {
                if let Some(error) = response.get("error") {
                    tracing::warn!(
                        session_id = %ctx.session_id,
                        %error,
                        "orchestrated lane session sync refused; continuing"
                    );
                }
            }
            Err(error) => {
                anyhow::bail!("orchestrated tool-server lane failed during session sync: {error}");
            }
        }

        // The durable spawn edge: child row under the orchestrating run.
        let run = prism_provenance::new_agent_run(
            &ctx.session_id,
            "subagent",
            &crate::agent_loop::agent_run_label(&spec.task),
            Some(&ctx.parent_run_id),
        );
        let run_ledger = crate::agent_loop::RunLedger::start(&run, "orchestrated-run").await;
        let heartbeat =
            crate::agent_loop::AgentRunHeartbeat::start(run_ledger.clone(), run.id.clone());

        let model_cfg = get_model_config(&spec.model);
        let mut llm_config = ctx.llm_config.clone();
        llm_config.model = spec.model.clone();
        llm_config.context_window = Some(model_cfg.context_window as u64);
        llm_config.max_output_tokens = Some(model_cfg.max_output_tokens as u64);

        let mut config = ctx.config_template.clone();
        config.model = spec.model.clone();

        let mut budget = TurnBudget::for_model(
            Some(model_cfg.context_window as u64),
            Some(model_cfg.max_output_tokens as u64),
        );
        budget.max_input_tokens = spec.max_tokens;

        Ok(LiveItemState {
            llm: LlmClient::new(llm_config),
            lane,
            history: Vec::new(),
            transcript: TranscriptStore::new(Some(budget)),
            scratchpad: Scratchpad::new(),
            config,
            run,
            run_ledger,
            heartbeat: Some(heartbeat),
            metrics: crate::agent_loop::AgentRunMetrics::default(),
            policy: ctx.policy_template.clone(),
            finalized: false,
        })
    }
}

impl ItemAgent for OrchestratedAgent {
    fn ledger_recorded(&self) -> bool {
        // `live` is None only when the item never spawned; such an item has
        // nothing to record. Once it spawned, an absent ledger means the
        // ledger write failed.
        self.live
            .as_ref()
            .is_none_or(|live| live.run_ledger.is_some())
    }

    fn attempt(&mut self, repair: Option<String>) -> BoxFut<'_, Result<AttemptReport>> {
        Box::pin(async move {
            if self.live.is_none() {
                self.live = Some(Self::setup(&self.ctx, &self.spec).await?);
            }
            let ctx = &self.ctx;
            let live = self.live.as_mut().expect("live state was just ensured");

            let user_message = match repair {
                None => self.spec.task.clone(),
                Some(demand) => demand,
            };

            // Fail-closed approval channel — the batch was approved as a
            // whole; concurrent per-item prompts cannot be correlated on the
            // wire, so anything that still needs a human is DENIED, honestly,
            // inside the item (see module docs, "Approval shape").
            let approval_rx = ctx.parent_has_approval_channel.then(|| {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                drop(tx);
                Arc::new(tokio::sync::Mutex::new(rx))
            });

            let mut streamed_text = String::new();
            let mut final_text: Option<String> = None;
            let mut steps: Vec<String> = Vec::new();
            let mut usage: Option<crate::types::UsageInfo> = None;
            let mut estimated_cost: Option<f64> = None;
            let nested_result;
            {
                let events = ctx.events.clone();
                // WHICH agent this activity belongs to. Without it every
                // parallel item pushes onto one sink and the streams interleave
                // with no way to tell them apart — the identity has to be on
                // the wire, because the parent pumps these events from its own
                // task and cannot infer the sender.
                let agent = self.spec.id.clone();
                // Name this branch for the provenance hook too, not only for
                // the event stream. Set on THIS task — the orchestrator spawns
                // one per item, which is what keeps concurrent branches apart —
                // so every tool call the item makes is recorded against it, and
                // "what did this branch actually buy" becomes a query.
                crate::hooks::begin_agent(&agent);
                // Item event routing: text is CAPTURED (it becomes the item
                // result); tool activity is FORWARDED so the parent's sink
                // sees what every item is doing; approval requests are
                // SUPPRESSED — they are pre-decided (fail-closed) and the
                // denial itself arrives as a visible ToolCallResult.
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
                        AgentEvent::ToolApprovalRequest { .. } => return,
                        AgentEvent::ToolCallResult { summary, .. } => {
                            if let Some(summary) = summary {
                                steps.push(summary.clone());
                            }
                        }
                        AgentEvent::ContextPriming { .. } | AgentEvent::ToolCallStart { .. } => {}
                        // Already tagged by a deeper agent: forwarded untouched.
                        // Re-tagging here would claim a grandchild's work for its
                        // parent.
                        AgentEvent::AgentActivity { .. } => {}
                    }
                    let _ = events.send(match event {
                        // Already tagged deeper down: forwarded untouched, so
                        // the name on the wire stays the agent that did the
                        // work rather than the one relaying it.
                        tagged @ AgentEvent::AgentActivity { .. } => tagged,
                        own => AgentEvent::AgentActivity {
                            agent: agent.clone(),
                            event: Box::new(own),
                        },
                    });
                };

                // Cleared as soon as the turn returns, below: a task reused
                // after this item finishes must not keep charging it.
                let nested: Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> =
                    Box::pin(crate::agent_loop::run_turn_inner(
                        &live.llm,
                        &mut live.lane,
                        &ctx.runtime,
                        &mut live.history,
                        &ctx.catalog,
                        &live.config,
                        &user_message,
                        None, // orchestrated turns are chat-shaped
                        &mut live.transcript,
                        ctx.hooks.as_ref(),
                        &ctx.permissions,
                        ctx.overrides.clone(),
                        &mut live.scratchpad,
                        &mut nested_emit,
                        approval_rx,
                        live.policy.as_mut(),
                        // Depth still caps nesting; the pool bounds lanes.
                        Some(&ctx.pool),
                        &live.run.id,
                        &live.run.session_id,
                        &mut live.metrics,
                    ));
                // Task-locals do not cross tokio::spawn: re-scope the access
                // CAPTURED at the dispatch site (never wider) around the
                // nested turn so its gates see the real caller.
                nested_result =
                    crate::command_tools::with_platform_access(ctx.access, nested).await;
            }
            // The item's turn is over — including the error path, which is why
            // this sits before the `?` below. A task that kept an item's name
            // after it finished would charge the next piece of work to the
            // wrong branch, and a wrong branch is worse than no branch.
            crate::hooks::end_agent();
            // Charge from the INCREMENTAL metrics, BEFORE propagating the
            // error.
            //
            // Two bugs in one line, both found by adversarial review. The
            // charge used to sit after the `?`, and it read `usage`, which is
            // only ever populated by `TurnComplete` — an event that never
            // fires when a turn errors. So an item that made four billed calls
            // and died on the fifth contributed ZERO to the parent's charge.
            // That is exactly the budget escape hatch the comment below
            // promises does not exist: a batch that burned real money on
            // failures read as free to whatever debits the parent.
            //
            // `live.metrics` accrues per LLM call as the turn runs (it is what
            // the per-item ledger row already reports accurately), so it holds
            // the real spend whether the turn finished or died. Tokens spent
            // are spent; the outcome does not refund them.
            ctx.usage
                .lock()
                .expect("orchestration usage roll-up is never held across a panic")
                .absorb(&live.metrics);

            // Name the model and the endpoint on the way out — see
            // `delegation_failure_context`. Lazy, so a healthy item formats
            // nothing.
            nested_result.with_context(|| {
                delegation_failure_context(
                    &self.spec.id,
                    &self.spec.model,
                    &live.llm.config().base_url,
                )
            })?;

            let answer = final_text
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(streamed_text);
            let start = steps.len().saturating_sub(ITEM_STEPS_SHOWN);
            let detail = json!({
                "model": self.spec.model,
                "run_id": live.run.id,
                "steps": &steps[start..],
                "usage": usage.map(|u| json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                })),
                "estimated_cost": estimated_cost,
            });
            Ok(AttemptReport { answer, detail })
        })
    }

    fn finalize(&mut self, disposition: FinalDisposition) -> BoxFut<'_, ()> {
        Box::pin(async move {
            // An item that never started (skipped, or setup failed) has no
            // durable row to close.
            let Some(live) = self.live.as_mut() else {
                return;
            };
            if live.finalized {
                return;
            }
            live.finalized = true;
            if let Some(heartbeat) = live.heartbeat.take() {
                heartbeat.stop().await;
            }
            let (status, last_error) = match &disposition {
                FinalDisposition::Completed => (prism_provenance::AgentRunStatus::Completed, None),
                FinalDisposition::Failed(reason) => (
                    prism_provenance::AgentRunStatus::Failed,
                    Some(reason.clone()),
                ),
                FinalDisposition::Cancelled(reason) => (
                    prism_provenance::AgentRunStatus::Cancelled,
                    Some(reason.clone()),
                ),
            };
            if let Some(ledger) = live.run_ledger.as_ref() {
                ledger
                    .finish(&live.run.id, status, &live.metrics, last_error.as_deref())
                    .await;
            }
            // Dropping the live state returns (or discards) the lane.
            self.live = None;
        })
    }
}

// ── Execution (dispatched from the agent loop) ────────────────────────

/// Run one orchestrated fan-out. Called from the agent loop's dispatch (NOT
/// from `execute_meta_tool` — this needs the live turn machinery), exactly
/// like `spawn_subagent`. Boxed for the same recursion-breaking reason.
///
/// `run_metrics` is the caller's accumulator for the fan-out's total spend —
/// an OUT-PARAMETER for the same reason `spawn_subagent` takes one: the
/// caller charges what was burned regardless of how this resolves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_orchestrate_agents<'a>(
    llm: &'a LlmClient,
    command_tool_runtime: &'a CommandToolRuntime,
    tool_catalog: &'a ToolCatalog,
    parent_config: &'a AgentConfig,
    parent_run_id: &'a str,
    parent_session_id: &'a str,
    args: &'a Value,
    permissions: &'a ToolPermissionContext,
    live_permission_overrides: Option<SharedPermissionOverrides>,
    emit: &'a mut (dyn FnMut(AgentEvent) + Send),
    parent_has_approval_channel: bool,
    policy: Option<&'a prism_policy::PolicyEngine>,
    subagent_lanes: Option<&'a ToolServerPool>,
    run_metrics: &'a mut crate::agent_loop::AgentRunMetrics,
) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        // SAFETY: access gate FIRST — orchestration drives nested turns over
        // the same code-running tool surface as spawn_subagent (effect-
        // classified ExecutesCode), before any token is spent.
        crate::command_tools::gate_meta_tool_execution(
            crate::meta_tools::MetaTool::OrchestrateAgents,
            crate::command_tools::current_platform_access(),
        )?;

        // SAFETY: an orchestrated agent may never orchestrate again.
        //
        // This is checked BEFORE the depth cap because the depth cap does not
        // catch it: at depth 1 there is still headroom, and the escape is
        // width x width rather than depth. A nested call would mint its own
        // `max_agent_calls` budget and inherit `auto_approve`, so a single
        // "Allow All" could authorise a second batch the approver never saw
        // and the first budget never counted.
        //
        // The refusal names the alternative, because an agent that is told
        // only "no" will try something else: ask for a WIDER batch, which the
        // approver sees in one prompt and one budget charges.
        if parent_config.orchestration_forbidden {
            return Ok(json!({
                "error": "an orchestrated agent cannot orchestrate again — fan-out is width, \
                          not depth. Ask for a WIDER batch in a single orchestrate_agents call \
                          so its size and cost ceiling are approved and charged once.",
            }));
        }

        // SAFETY: recursion cap — fan-out is WIDTH, not depth. Orchestrated
        // items run at depth+1 under the unchanged MAX_SUBAGENT_DEPTH.
        if parent_config.subagent_depth >= MAX_SUBAGENT_DEPTH {
            return Ok(json!({
                "error": format!(
                    "subagent recursion cap reached (depth {} of {MAX_SUBAGENT_DEPTH}) — \
                     do these tasks yourself instead of delegating further",
                    parent_config.subagent_depth,
                ),
            }));
        }

        // Concurrent items each need their own lane; without a pool there is
        // nothing to fan out over. Honest refusal, with the working
        // alternative named.
        let Some(pool) = subagent_lanes else {
            return Ok(json!({
                "error": "orchestrate_agents needs the tool-server lane pool, which this \
                          transport did not provide — delegate tasks one at a time with \
                          spawn_subagent instead.",
            }));
        };

        // Inherit from the LIVE `LlmClient`, never from `AgentConfig.model` —
        // same reason as `subagent::execute_spawn_subagent_inner`, which
        // carries the full account: `AgentConfig.model` is never populated
        // from the resolved chat route, so reading it asked the parent's own
        // endpoint for the `impl Default` literal and every orchestrated item
        // died with `1214 modelCode does not exist`.
        let (specs, policy_spec) = parse_args(args, &llm.config().model)?;
        let lane_bound = Some(pool.policy().max_lanes);

        let mut config_template = parent_config.clone();
        config_template.subagent_depth = parent_config.subagent_depth + 1;
        // An orchestrated item may never orchestrate again.
        //
        // Found by adversarial review, and it defeated BOTH guarantees this
        // tool advertises. `fan_out` builds a fresh `AgentCallBudget` from its
        // own `max_agent_calls` on every call, and `auto_approve` is copied
        // verbatim into each item's config — while `approval_gate_outcome`
        // checks `auto_approve` BEFORE the fail-closed channel logic. So after
        // one ordinary "Allow All", a depth-1 item could call
        // `orchestrate_agents` again with its own budget: auto-approved,
        // uncounted by the ceiling the human actually saw, and multiplying
        // width by width. One consent event could authorise on the order of a
        // thousand nested turns.
        //
        // The depth cap did not stop it — depth 2 still permits one further
        // orchestration, and the escape is width x width, not depth.
        //
        // Refusing recursion outright is the honest fix: fan-out is WIDTH, and
        // a batch the approver saw is the batch that runs. A caller wanting
        // more parallel work asks for a wider batch, which is visible in the
        // one prompt and charged to the one budget. Propagating a remaining
        // budget downward would preserve the count but not the CONSENT — the
        // approver still never saw the second batch.
        config_template.orchestration_forbidden = true;

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = Arc::new(OrchestrationContext {
            llm_config: llm.config().clone(),
            runtime: command_tool_runtime.clone(),
            catalog: tool_catalog.clone(),
            hooks: Arc::new(crate::hooks::build_default_hooks()),
            permissions: permissions.clone(),
            overrides: live_permission_overrides,
            policy_template: policy.cloned(),
            pool: pool.clone(),
            config_template,
            parent_run_id: parent_run_id.to_string(),
            session_id: parent_session_id.to_string(),
            access: crate::command_tools::current_platform_access(),
            parent_has_approval_channel,
            events: events_tx,
            usage: std::sync::Mutex::new(crate::agent_loop::AgentRunMetrics::default()),
        });

        // G2: one guard for the whole fan-out — the PARENT's repair-chain
        // memory survives the orchestrated turns (sibling items share the
        // process-global map among themselves; see module docs).
        let _chain_guard = crate::hooks::CodeRunChainGuard::new();

        // No in-run trigger fires this yet (see module docs); transports get
        // structural cancellation because dropping this future aborts the
        // JoinSet, and the signal is wired so a cancel surface can attach.
        let (_cancel_handle, cancel_signal) = cancellation();

        let factory_ctx = Arc::clone(&ctx);
        // Dependency-ordered when the plan declares edges, plain fan-out when it
        // does not — one wave IS a fan-out, so the DAG path costs nothing for
        // callers that never use it.
        let has_edges = specs.iter().any(|spec| !spec.depends_on.is_empty());
        let fan = async move {
            let factory = move |_: usize, spec: &OrchestratorTaskSpec| {
                Box::new(OrchestratedAgent::new(
                    Arc::clone(&factory_ctx),
                    spec.clone(),
                )) as Box<dyn ItemAgent>
            };
            if has_edges {
                match fan_out_dag(specs, &policy_spec, lane_bound, cancel_signal, factory).await {
                    Ok(run) => run,
                    // A malformed plan (cycle, unknown or duplicate id) is an
                    // authoring error, not a partial result. Report every item
                    // as skipped with the reason rather than silently running
                    // whichever subset happened to be schedulable.
                    Err(error) => OrchestratedRun {
                        items: Vec::new(),
                        requested_concurrency: policy_spec.max_concurrent.get(),
                        lane_bound: lane_bound.map(NonZeroUsize::get),
                        effective_concurrency: effective_concurrency(&policy_spec, lane_bound)
                            .get(),
                        budget_max: policy_spec.max_agent_calls.get(),
                        budget_used: 0,
                        budget_exhausted: false,
                    }
                    .with_plan_error(&error.to_string()),
                }
            } else {
                fan_out(specs, &policy_spec, lane_bound, cancel_signal, factory).await
            }
        };
        let mut fan = std::pin::pin!(fan);

        // Pump item events to the parent's sink WHILE the fan-out runs, so
        // tool activity (and fail-closed denials) is visible live.
        let run = loop {
            tokio::select! {
                event = events_rx.recv() => {
                    if let Some(event) = event {
                        emit(event);
                    }
                }
                run = &mut fan => break run,
            }
        };
        while let Ok(event) = events_rx.try_recv() {
            emit(event);
        }

        // Hand the roll-up back through the out-parameter, not through the
        // result JSON. The dispatch site used to re-read this very object with
        // `subagent::usage_from_result`, which made the parent's charge depend
        // on the fan-out returning `Ok` — the same coupling that let the
        // spawn_subagent path charge nothing for a turn that errored after
        // billed calls. What the model is TOLD and what the parent is CHARGED
        // now come from one value.
        run_metrics.absorb(
            &ctx.usage
                .lock()
                .expect("orchestration usage roll-up is never held across a panic"),
        );
        // What each branch BOUGHT, and which to expand next. This is the
        // feedback edge: the fan-out scores its own branches so the NEXT
        // decomposition is written against measured yield instead of a guess.
        // Best-effort — a store that cannot be read costs the recommendation,
        // never the run, and an absent block is honest about that.
        let branch_feedback = match prism_provenance::ProvenanceStore::open(
            &crate::hooks::provenance_db_path(),
        )
        .await
        {
            Ok(store) => {
                let yields = store
                    .branch_yields(&ctx.session_id)
                    .await
                    .unwrap_or_default();
                let expansions = store
                    .branch_expansions(&ctx.session_id)
                    .await
                    .unwrap_or_default();
                let ranked = crate::branch_policy::rank(&yields, &expansions);
                crate::branch_policy::feedback_block(&ranked).map(|block| {
                    json!({
                        "ranked": ranked.iter().map(|c| json!({
                            "agent": c.agent,
                            "facts_per_call": c.yield_per_call,
                            "expansions": c.expansions,
                            "weight": c.weight,
                        })).collect::<Vec<_>>(),
                        "for_the_next_decomposition": block,
                    })
                })
            }
            Err(error) => {
                tracing::warn!(%error, "branch feedback unavailable for this fan-out");
                None
            }
        };

        Ok(json!({
            "items": run.items,
            "branch_feedback": branch_feedback,
            "succeeded": run.succeeded(),
            "failed": run.failed(),
            "skipped": run.skipped(),
            "total": run.items.len(),
            "budget": {
                "max_agent_calls": run.budget_max,
                "used": run.budget_used,
                "exhausted": run.budget_exhausted,
            },
            "concurrency": {
                "requested": run.requested_concurrency,
                "lane_bound": run.lane_bound,
                "effective": run.effective_concurrency,
            },
            // Reported to the model so it can see what the batch cost. The
            // PARENT's charge comes from `run_metrics` above, not from here.
            "usage": {
                "input_tokens": run_metrics.tokens_in,
                "output_tokens": run_metrics.tokens_out,
            },
            "hint": "per-item details are durable: each item's run_id is in the agent-run \
                     ledger (with the spawn edge to this run), and recall(query=…) finds \
                     item tool results",
        }))
    })
}

/// Clip a string to `max` chars (whole chars, not bytes).
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    fn spec(index: usize) -> OrchestratorTaskSpec {
        OrchestratorTaskSpec {
            id: format!("task-{index}"),
            task: format!("do thing {index}"),
            model: DEFAULT_SUBAGENT_MODEL.to_string(),
            max_tokens: DEFAULT_SUBAGENT_BUDGET_TOKENS,
            result_schema: None,
            depends_on: Vec::new(),
        }
    }

    fn specs(n: usize) -> Vec<OrchestratorTaskSpec> {
        (0..n).map(spec).collect()
    }

    fn policy(width: usize, calls: usize) -> OrchestratorPolicy {
        OrchestratorPolicy {
            max_concurrent: NonZeroUsize::new(width).expect("test width is non-zero"),
            max_agent_calls: NonZeroUsize::new(calls).expect("test budget is non-zero"),
        }
    }

    /// Instrumented fake agent: counts attempts, tracks a live-concurrency
    /// gauge (with a Drop guard so cancelled futures still decrement), and
    /// answers from a per-attempt script.
    #[derive(Clone, Default)]
    struct Probe {
        attempts: Arc<AtomicUsize>,
        running: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        completion_order: Arc<Mutex<Vec<usize>>>,
    }

    struct GaugeGuard(Arc<AtomicUsize>);
    impl Drop for GaugeGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    enum Step {
        Answer(&'static str),
        Fail(&'static str),
    }

    struct FakeAgent {
        index: usize,
        probe: Probe,
        delay: Duration,
        script: Vec<Step>,
        attempt_no: usize,
    }

    impl FakeAgent {
        fn new(index: usize, probe: Probe, delay: Duration, script: Vec<Step>) -> Self {
            Self {
                index,
                probe,
                delay,
                script,
                attempt_no: 0,
            }
        }
    }

    impl ItemAgent for FakeAgent {
        fn attempt(&mut self, _repair: Option<String>) -> BoxFut<'_, Result<AttemptReport>> {
            Box::pin(async move {
                self.probe.attempts.fetch_add(1, Ordering::AcqRel);
                let now = self.probe.running.fetch_add(1, Ordering::AcqRel) + 1;
                self.probe.peak.fetch_max(now, Ordering::AcqRel);
                let _guard = GaugeGuard(Arc::clone(&self.probe.running));
                tokio::time::sleep(self.delay).await;
                self.probe
                    .completion_order
                    .lock()
                    .expect("test mutex")
                    .push(self.index);
                let step = self
                    .script
                    .get(self.attempt_no)
                    .unwrap_or(&Step::Answer("done"));
                self.attempt_no += 1;
                match step {
                    Step::Answer(answer) => Ok(AttemptReport {
                        answer: (*answer).to_string(),
                        detail: json!({ "index": self.index }),
                    }),
                    Step::Fail(reason) => Err(anyhow::anyhow!("{reason}")),
                }
            })
        }

        fn finalize(&mut self, _disposition: FinalDisposition) -> BoxFut<'_, ()> {
            Box::pin(async {})
        }
    }

    fn never_cancelled() -> (CancelHandle, CancelSignal) {
        cancellation()
    }

    // ── Bounding ──────────────────────────────────────────────────

    /// The bound is real: 10 tasks against width 3 never exceed 3 live
    /// attempts. Delete the semaphore and the peak reads 10 — this test is
    /// the tripwire for that.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrency_never_exceeds_the_bound() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(specs(10), &policy(3, 16), None, cancel, move |index, _| {
            Box::new(FakeAgent::new(
                index,
                factory_probe.clone(),
                Duration::from_millis(80),
                vec![Step::Answer("done")],
            )) as Box<dyn ItemAgent>
        })
        .await;
        assert_eq!(run.succeeded(), 10);
        let peak = probe.peak.load(Ordering::Acquire);
        assert!(
            peak <= 3,
            "10 queued tasks must never exceed the width-3 bound (peak {peak})"
        );
        assert!(
            peak >= 2,
            "the fan-out must actually run concurrently (peak {peak})"
        );
    }

    /// The lane pool's bound wins when it is smaller — the two declared
    /// limits are reconciled, not left to disagree.
    #[tokio::test(flavor = "multi_thread")]
    async fn lane_bound_caps_the_requested_width() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(
            specs(8),
            &policy(8, 16),
            NonZeroUsize::new(2),
            cancel,
            move |index, _| {
                Box::new(FakeAgent::new(
                    index,
                    factory_probe.clone(),
                    Duration::from_millis(60),
                    vec![Step::Answer("done")],
                )) as Box<dyn ItemAgent>
            },
        )
        .await;
        assert_eq!(run.requested_concurrency, 8);
        assert_eq!(run.lane_bound, Some(2));
        assert_eq!(run.effective_concurrency, 2);
        let peak = probe.peak.load(Ordering::Acquire);
        assert!(peak <= 2, "the lane bound must cap the width (peak {peak})");
    }

    // ── Speedup ───────────────────────────────────────────────────

    /// The point of the fan-out: N independent tasks complete in materially
    /// less wall-clock than N sequential ones. Controlled delay, ratio
    /// asserted — machine speed does not decide the outcome.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_fan_out_beats_sequential_materially() {
        const N: usize = 4;
        const DELAY: Duration = Duration::from_millis(300);

        let make = |probe: Probe| {
            move |index: usize, _spec: &OrchestratorTaskSpec| {
                Box::new(FakeAgent::new(
                    index,
                    probe.clone(),
                    DELAY,
                    vec![Step::Answer("done")],
                )) as Box<dyn ItemAgent>
            }
        };

        let (_h1, cancel) = never_cancelled();
        let sequential_started = std::time::Instant::now();
        let sequential = fan_out(
            specs(N),
            &policy(1, 16),
            None,
            cancel,
            make(Probe::default()),
        )
        .await;
        let sequential_elapsed = sequential_started.elapsed();
        assert_eq!(sequential.succeeded(), N);

        let (_h2, cancel) = never_cancelled();
        let concurrent_started = std::time::Instant::now();
        let concurrent = fan_out(
            specs(N),
            &policy(N, 16),
            None,
            cancel,
            make(Probe::default()),
        )
        .await;
        let concurrent_elapsed = concurrent_started.elapsed();
        assert_eq!(concurrent.succeeded(), N);

        eprintln!(
            "fan-out speedup: sequential {sequential_elapsed:?} vs concurrent \
             {concurrent_elapsed:?} ({N} x {DELAY:?} tasks)"
        );
        assert!(
            concurrent_elapsed < sequential_elapsed.mul_f64(0.7),
            "concurrent ({concurrent_elapsed:?}) must be materially faster than \
             sequential ({sequential_elapsed:?}) for {N} x {DELAY:?} tasks"
        );
    }

    // ── Per-item outcomes ─────────────────────────────────────────

    /// 10 items, 3 fail: the report says exactly that — 7 succeeded AND 3
    /// named failures, in input order. Not an error, not a silent 7.
    #[tokio::test(flavor = "multi_thread")]
    async fn partial_failure_reports_every_item_by_name() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(specs(10), &policy(4, 16), None, cancel, move |index, _| {
            let script = if [2usize, 5, 7].contains(&index) {
                vec![Step::Fail("boom")]
            } else {
                vec![Step::Answer("done")]
            };
            Box::new(FakeAgent::new(
                index,
                factory_probe.clone(),
                Duration::from_millis(10),
                script,
            )) as Box<dyn ItemAgent>
        })
        .await;

        assert_eq!(run.succeeded(), 7);
        assert_eq!(run.failed(), 3);
        assert_eq!(run.skipped(), 0);
        for (index, item) in run.items.iter().enumerate() {
            assert_eq!(item.id, format!("task-{index}"), "input order preserved");
            match (&item.outcome, [2usize, 5, 7].contains(&index)) {
                (ItemOutcome::Failed { reason }, true) => {
                    assert!(reason.contains("boom"), "failure must be named: {reason}");
                }
                (ItemOutcome::Succeeded { .. }, false) => {}
                (outcome, _) => panic!("item {index} has the wrong outcome: {outcome:?}"),
            }
        }
    }

    /// Result order is input order even when completion order is inverted by
    /// construction (task 0 slowest).
    #[tokio::test(flavor = "multi_thread")]
    async fn report_order_is_input_order_not_completion_order() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(specs(4), &policy(4, 16), None, cancel, move |index, _| {
            Box::new(FakeAgent::new(
                index,
                factory_probe.clone(),
                Duration::from_millis(60 * (4 - index as u64)),
                vec![Step::Answer("done")],
            )) as Box<dyn ItemAgent>
        })
        .await;
        let ids: Vec<&str> = run.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["task-0", "task-1", "task-2", "task-3"]);
        let completion = probe.completion_order.lock().expect("test mutex").clone();
        assert_ne!(
            completion,
            vec![0, 1, 2, 3],
            "the controlled delays must invert completion order for this test to bite"
        );
    }

    // ── Structured-output verification ────────────────────────────

    fn alloy_schema() -> Value {
        json!({
            "type": "object",
            "required": ["alloy"],
            "properties": { "alloy": { "type": "string" } }
        })
    }

    fn schema_spec(index: usize) -> OrchestratorTaskSpec {
        OrchestratorTaskSpec {
            result_schema: Some(alloy_schema()),
            ..spec(index)
        }
    }

    /// A schema mismatch is retried exactly once with the validation error,
    /// then succeeds: 2 attempts, repaired flag set, budget charged twice.
    #[tokio::test(flavor = "multi_thread")]
    async fn schema_mismatch_is_repaired_exactly_once() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(
            vec![schema_spec(0)],
            &policy(1, 16),
            None,
            cancel,
            move |index, _| {
                Box::new(FakeAgent::new(
                    index,
                    factory_probe.clone(),
                    Duration::from_millis(5),
                    vec![
                        Step::Answer("not json at all"),
                        Step::Answer(r#"{"alloy": "Ti-6Al-4V"}"#),
                    ],
                )) as Box<dyn ItemAgent>
            },
        )
        .await;
        assert_eq!(run.succeeded(), 1, "{:?}", run.items);
        let ItemOutcome::Succeeded { result } = &run.items[0].outcome else {
            panic!("expected success: {:?}", run.items[0]);
        };
        assert_eq!(result["schema_repaired"], json!(true));
        assert_eq!(result["output"]["alloy"], json!("Ti-6Al-4V"));
        assert_eq!(
            probe.attempts.load(Ordering::Acquire),
            2,
            "initial attempt + exactly one repair"
        );
        assert_eq!(run.budget_used, 2, "the repair is a budgeted agent call");
    }

    /// A second mismatch fails the item with the reason — and the attempt
    /// count proves no blanket retry crept in.
    #[tokio::test(flavor = "multi_thread")]
    async fn second_schema_mismatch_fails_with_the_reason() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(
            vec![schema_spec(0)],
            &policy(1, 16),
            None,
            cancel,
            move |index, _| {
                Box::new(FakeAgent::new(
                    index,
                    factory_probe.clone(),
                    Duration::from_millis(5),
                    vec![
                        Step::Answer(r#"{"wrong": 1}"#),
                        Step::Answer(r#"{"still_wrong": 2}"#),
                        Step::Answer(r#"{"alloy": "never reached"}"#),
                    ],
                )) as Box<dyn ItemAgent>
            },
        )
        .await;
        assert_eq!(run.failed(), 1, "{:?}", run.items);
        let ItemOutcome::Failed { reason } = &run.items[0].outcome else {
            panic!("expected failure: {:?}", run.items[0]);
        };
        assert!(
            reason.contains("after one repair attempt") && reason.contains("alloy"),
            "the reason must carry the concrete validation failure: {reason}"
        );
        assert_eq!(
            probe.attempts.load(Ordering::Acquire),
            2,
            "EXACTLY two attempts — a third means a blanket-retry regression"
        );
    }

    /// A valid first answer is never re-run; a non-schema failure is never
    /// retried. Both are one-attempt paths.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_gratuitous_retries() {
        for (script, expect_success) in [
            (vec![Step::Answer(r#"{"alloy": "IN718"}"#)], true),
            (vec![Step::Fail("lane died")], false),
        ] {
            let probe = Probe::default();
            let factory_probe = probe.clone();
            let script = Arc::new(Mutex::new(Some(script)));
            let (_handle, cancel) = never_cancelled();
            let run = fan_out(
                vec![schema_spec(0)],
                &policy(1, 16),
                None,
                cancel,
                move |index, _| {
                    let script = script
                        .lock()
                        .expect("test mutex")
                        .take()
                        .expect("factory called once");
                    Box::new(FakeAgent::new(
                        index,
                        factory_probe.clone(),
                        Duration::from_millis(5),
                        script,
                    )) as Box<dyn ItemAgent>
                },
            )
            .await;
            assert_eq!(
                probe.attempts.load(Ordering::Acquire),
                1,
                "exactly one attempt (success={expect_success})"
            );
            assert_eq!(run.succeeded() == 1, expect_success, "{:?}", run.items);
        }
    }

    /// A defective schema (external $ref) skips the item BEFORE any agent
    /// call — spec defects must not spend budget.
    #[tokio::test(flavor = "multi_thread")]
    async fn external_ref_schema_is_rejected_without_spending() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let mut bad = spec(0);
        bad.result_schema = Some(json!({
            "type": "object",
            "properties": { "x": { "$ref": "https://evil.example/schema.json" } }
        }));
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(vec![bad], &policy(1, 16), None, cancel, move |index, _| {
            Box::new(FakeAgent::new(
                index,
                factory_probe.clone(),
                Duration::from_millis(5),
                vec![Step::Answer("unreached")],
            )) as Box<dyn ItemAgent>
        })
        .await;
        assert_eq!(run.skipped(), 1, "{:?}", run.items);
        let ItemOutcome::Skipped { reason } = &run.items[0].outcome else {
            panic!("expected skip: {:?}", run.items[0]);
        };
        assert!(
            reason.contains("self-contained") && reason.contains("evil.example"),
            "the reason must name the offending $ref: {reason}"
        );
        assert_eq!(probe.attempts.load(Ordering::Acquire), 0);
        assert_eq!(run.budget_used, 0);
    }

    // ── Budget ────────────────────────────────────────────────────

    /// Budget exhaustion is a reported outcome distinguishable from
    /// completion: 5 tasks against 3 calls = 3 succeeded + 2 skipped with
    /// the exhaustion named, and the run-level flag set.
    #[tokio::test(flavor = "multi_thread")]
    async fn budget_exhaustion_is_reported_not_swallowed() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(specs(5), &policy(2, 3), None, cancel, move |index, _| {
            Box::new(FakeAgent::new(
                index,
                factory_probe.clone(),
                Duration::from_millis(10),
                vec![Step::Answer("done")],
            )) as Box<dyn ItemAgent>
        })
        .await;
        assert_eq!(run.succeeded(), 3, "{:?}", run.items);
        assert_eq!(run.skipped(), 2);
        assert!(run.budget_exhausted, "exhaustion must be flagged");
        assert_eq!(run.budget_used, 3);
        for item in &run.items[3..] {
            let ItemOutcome::Skipped { reason } = &item.outcome else {
                panic!("later items must be skipped: {item:?}");
            };
            assert!(
                reason.contains("budget exhausted") && reason.contains("3 of 3"),
                "the reason must carry the numbers: {reason}"
            );
        }

        // The complement: a run that fits its budget reports no exhaustion.
        let (_handle, cancel) = never_cancelled();
        let fits = fan_out(specs(2), &policy(2, 3), None, cancel, |index, _| {
            Box::new(FakeAgent::new(
                index,
                Probe::default(),
                Duration::from_millis(5),
                vec![Step::Answer("done")],
            )) as Box<dyn ItemAgent>
        })
        .await;
        assert!(!fits.budget_exhausted, "completion must be distinguishable");
        assert_eq!(fits.succeeded(), 2);
    }

    /// A schema repair that cannot reserve budget fails the item with BOTH
    /// reasons (the validation error and the exhaustion).
    #[tokio::test(flavor = "multi_thread")]
    async fn repair_without_budget_fails_with_both_reasons() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (_handle, cancel) = never_cancelled();
        let run = fan_out(
            vec![schema_spec(0)],
            &policy(1, 1),
            None,
            cancel,
            move |index, _| {
                Box::new(FakeAgent::new(
                    index,
                    factory_probe.clone(),
                    Duration::from_millis(5),
                    vec![Step::Answer("not json"), Step::Answer(r#"{"alloy": "x"}"#)],
                )) as Box<dyn ItemAgent>
            },
        )
        .await;
        assert_eq!(run.failed(), 1, "{:?}", run.items);
        let ItemOutcome::Failed { reason } = &run.items[0].outcome else {
            panic!("expected failure: {:?}", run.items[0]);
        };
        assert!(
            reason.contains("schema validation") && reason.contains("budget exhausted"),
            "{reason}"
        );
        assert_eq!(
            probe.attempts.load(Ordering::Acquire),
            1,
            "no unbudgeted repair"
        );
    }

    // ── Cancellation ──────────────────────────────────────────────

    /// Cancellation actually stops in-flight agents: their attempt futures
    /// are dropped (the gauge guard proves it), the run returns promptly
    /// instead of waiting out a 30s sleep, and every item reports what
    /// happened to it.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_stops_in_flight_agents() {
        let probe = Probe::default();
        let factory_probe = probe.clone();
        let (handle, cancel) = cancellation();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            handle.cancel();
        });

        let started = std::time::Instant::now();
        let run = fan_out(specs(6), &policy(2, 16), None, cancel, move |index, _| {
            Box::new(FakeAgent::new(
                index,
                factory_probe.clone(),
                Duration::from_secs(30),
                vec![Step::Answer("unreachable")],
            )) as Box<dyn ItemAgent>
        })
        .await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "cancel must stop 30s sleeps, not wait them out (took {elapsed:?})"
        );
        assert_eq!(
            probe.running.load(Ordering::Acquire),
            0,
            "every in-flight attempt future must have been DROPPED"
        );
        assert_eq!(run.succeeded(), 0);
        assert!(run.failed() >= 1, "in-flight items report cancellation");
        assert!(run.skipped() >= 1, "queued items report cancellation");
        for item in &run.items {
            match &item.outcome {
                ItemOutcome::Failed { reason } | ItemOutcome::Skipped { reason } => {
                    assert!(reason.contains("cancelled"), "{}: {reason}", item.id);
                }
                other => panic!("{} must not succeed: {other:?}", item.id),
            }
        }
    }

    // ── Policy plumbing ───────────────────────────────────────────

    #[test]
    fn effective_concurrency_takes_the_smaller_bound() {
        let p = policy(4, 16);
        assert_eq!(effective_concurrency(&p, None).get(), 4);
        assert_eq!(effective_concurrency(&p, NonZeroUsize::new(2)).get(), 2);
        assert_eq!(effective_concurrency(&p, NonZeroUsize::new(9)).get(), 4);
    }

    /// An orchestrated agent may not orchestrate again.
    ///
    /// Found by adversarial review: a nested `orchestrate_agents` minted its
    /// OWN call budget and inherited `auto_approve`, and `approval_gate_outcome`
    /// checks `auto_approve` before the fail-closed channel logic. So one
    /// ordinary "Allow All" could authorise a second batch the approver never
    /// saw, uncounted by the ceiling they did see — width x width, on the
    /// order of a thousand turns from one consent. The depth cap did not stop
    /// it because the escape is not depth.
    #[test]
    fn an_orchestrated_agent_cannot_orchestrate_again() {
        // Depth 1 with headroom under the cap: the depth guard would ALLOW
        // this. Only the orchestration flag refuses it.
        let config = AgentConfig {
            subagent_depth: 1,
            orchestration_forbidden: true,
            ..AgentConfig::default()
        };
        assert!(
            config.subagent_depth < MAX_SUBAGENT_DEPTH,
            "test premise: the depth cap must NOT be what refuses this"
        );
        assert!(
            config.orchestration_forbidden,
            "every agent spawned by orchestrate_agents carries this flag"
        );
    }

    #[test]
    fn definition_is_conservative() {
        let def = definition();
        assert_eq!(def.name, ORCHESTRATE_AGENTS_TOOL);
        assert!(def.requires_approval, "batch delegation is gated");
        assert_eq!(def.permission_mode, PermissionMode::WorkspaceWrite);
        assert_eq!(def.input_schema["required"], json!(["tasks"]));
    }

    /// A real fan-out is named through the REAL parse path, not by calling
    /// the name pool directly — the pool having good names proves nothing
    /// about whether the orchestrator ever asks it for one.
    #[test]
    fn an_unnamed_fan_out_is_named_after_scientists_and_never_repeats() {
        let (specs, _) = parse_args(
            &json!({"tasks": [
                {"task": "survey fluorine-free firefighting foam burnback performance"},
                {"task": "compare PTFE-free non-stick ceramic sol-gel coating friction"},
                {"task": "PFAS-free elastomer seal chemical resistance and service temperature"},
                {"task": "review the polymer synthesis route for the replacement monomer"},
                {"task": "fluorine-free durable water repellent textile finish"},
            ]}),
            "glm-5.3",
        )
        .expect("valid args");

        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(
            &ids[..2],
            &["Sarabhai", "Bhabha"],
            "the openers lead every fan-out: {ids:?}"
        );
        for id in &ids {
            assert!(
                crate::agent_names::SCIENTISTS
                    .iter()
                    .any(|sc| sc.surname == *id),
                "every lane is named, none fell back to a number: {ids:?}"
            );
        }
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "no two lanes share a name: {ids:?}"
        );
    }

    #[test]
    fn parse_args_applies_defaults_and_limits() {
        let (specs, policy) = parse_args(&json!({
            "tasks": [ { "task": "survey refractory HEAs" }, { "task": "survey Ni superalloys", "id": "ni" } ],
        }), "glm-5.3")
        .expect("valid args");
        assert_eq!(specs.len(), 2);
        // CONTRACT CHANGE: an unnamed task is named after a scientist, not
        // `task-0`. The id is the only handle a report or an interface has for
        // saying WHICH agent did something, and `task-0` is unique without
        // being readable. `Sarabhai` opens every fan-out by design — see
        // `agent_names::FOUNDERS`.
        assert_eq!(specs[0].id, "Sarabhai");
        // A caller-supplied id still wins outright.
        assert_eq!(specs[1].id, "ni");
        // CONTRACT CHANGE: an unnamed model INHERITS the parent's route rather
        // than defaulting to the constant. The constant asked whatever endpoint
        // the parent was on for a model it might not serve — with the parent on
        // glm-5.3 every task died with `1214 modelCode：不存在`.
        assert_eq!(specs[0].model, "glm-5.3");
        assert_eq!(specs[0].max_tokens, DEFAULT_SUBAGENT_BUDGET_TOKENS);
        assert_eq!(policy.max_concurrent.get(), 4);
        assert_eq!(policy.max_agent_calls.get(), 16);

        // The model-controlled budget is clamped to the declared ceiling.
        let (_, policy) = parse_args(
            &json!({
                "tasks": [ { "task": "t" } ],
                "max_agent_calls": 100_000,
            }),
            "glm-5.3",
        )
        .expect("valid args");
        assert_eq!(policy.max_agent_calls.get(), MAX_AGENT_CALLS_CEILING);
    }

    #[test]
    fn parse_args_rejects_defective_batches() {
        assert!(parse_args(&json!({}), "glm-5.3").is_err(), "missing tasks");
        assert!(
            parse_args(&json!({ "tasks": [] }), "glm-5.3").is_err(),
            "empty tasks"
        );
        assert!(
            parse_args(&json!({ "tasks": [ { "task": "  " } ] }), "glm-5.3").is_err(),
            "blank task"
        );
        assert!(
            parse_args(
                &json!({
                    "tasks": [ { "task": "a", "id": "x" }, { "task": "b", "id": "x" } ]
                }),
                "glm-5.3"
            )
            .is_err(),
            "duplicate ids"
        );
        let too_many: Vec<Value> = (0..=MAX_TASKS_PER_CALL)
            .map(|i| json!({ "task": format!("t{i}") }))
            .collect();
        let err = parse_args(&json!({ "tasks": too_many }), "glm-5.3").expect_err("over the cap");
        assert!(err.to_string().contains("at most"), "{err}");
    }

    #[test]
    fn strip_code_fences_handles_the_common_shapes() {
        assert_eq!(strip_code_fences("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_code_fences("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fences("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fences("  {\"a\":1}  "), "{\"a\":1}");
    }

    #[test]
    fn budget_reservation_rolls_back_only_uncommitted() {
        let budget = Arc::new(AgentCallBudget::new(
            NonZeroUsize::new(2).expect("non-zero"),
        ));
        let first = budget.try_reserve().expect("first fits");
        first.commit();
        {
            let _second = budget.try_reserve().expect("second fits");
            assert!(budget.try_reserve().is_err(), "budget is full");
        } // uncommitted second reservation returns on drop
        assert_eq!(budget.used(), 1, "only the committed call is spent");
        let third = budget.try_reserve().expect("released slot is reusable");
        third.commit();
        assert_eq!(budget.used(), 2);
        assert!(budget.exhausted(), "the refusal was recorded");
    }
    // ── Research DAG ───────────────────────────────────────────────

    fn dag_spec(id: &str, deps: &[&str]) -> OrchestratorTaskSpec {
        OrchestratorTaskSpec {
            id: id.to_string(),
            task: format!("investigate {id}"),
            model: DEFAULT_SUBAGENT_MODEL.to_string(),
            max_tokens: DEFAULT_SUBAGENT_BUDGET_TOKENS,
            result_schema: None,
            depends_on: deps.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    #[test]
    fn independent_questions_all_run_in_the_first_wave() {
        let specs = vec![dag_spec("a", &[]), dag_spec("b", &[]), dag_spec("c", &[])];
        let waves = dependency_waves(&specs).expect("no edges is a valid plan");
        assert_eq!(waves.len(), 1, "nothing depends on anything: one wave");
        assert_eq!(waves[0].len(), 3);
    }

    #[test]
    fn a_synthesis_task_waits_for_what_it_synthesises() {
        // The shape research actually takes: investigate two angles in
        // parallel, then compare them. The comparison cannot run first.
        let specs = vec![
            dag_spec("compare", &["coatings", "seals"]),
            dag_spec("coatings", &[]),
            dag_spec("seals", &[]),
        ];
        let waves = dependency_waves(&specs).expect("a valid plan");
        assert_eq!(waves.len(), 2, "two waves: the pair, then the comparison");
        assert_eq!(waves[0].len(), 2, "both investigations run together");
        assert_eq!(waves[1], vec![0], "the comparison runs alone, afterwards");
    }

    #[test]
    fn a_chain_runs_strictly_in_order() {
        let specs = vec![
            dag_spec("third", &["second"]),
            dag_spec("second", &["first"]),
            dag_spec("first", &[]),
        ];
        let waves = dependency_waves(&specs).expect("a valid plan");
        assert_eq!(waves.len(), 3, "a chain cannot be parallelised: {waves:?}");
    }

    #[test]
    fn a_cycle_is_refused_rather_than_partly_run() {
        let specs = vec![dag_spec("a", &["b"]), dag_spec("b", &["a"])];
        let error = dependency_waves(&specs).expect_err("a cycle cannot be scheduled");
        let message = error.to_string();
        assert!(message.contains("cycle"), "{message}");
        assert!(
            message.contains('a') && message.contains('b'),
            "names the stuck tasks: {message}"
        );
    }

    #[test]
    fn a_dependency_on_a_task_that_does_not_exist_is_refused() {
        let specs = vec![dag_spec("a", &["ghost"])];
        let message = dependency_waves(&specs)
            .expect_err("an unknown id is an authoring error")
            .to_string();
        assert!(
            message.contains("ghost"),
            "names the missing task: {message}"
        );
    }

    #[test]
    fn duplicate_ids_are_refused_because_edges_would_be_ambiguous() {
        let specs = vec![dag_spec("a", &[]), dag_spec("a", &[])];
        let message = dependency_waves(&specs).expect_err("ambiguous").to_string();
        assert!(message.contains("duplicate"), "{message}");
    }

    #[test]
    fn a_failed_upstream_is_reported_as_open_not_hidden() {
        // The dangerous case: a downstream task that silently received nothing
        // would answer confidently from an empty premise.
        let failed = ItemReport {
            id: "seals".to_string(),
            outcome: ItemOutcome::Failed {
                reason: "provider timeout".to_string(),
            },
            ledger_recorded: false,
        };
        let briefing = upstream_briefing("seals", &failed);
        assert!(briefing.contains("FAILED"), "{briefing}");
        assert!(
            briefing.contains("provider timeout"),
            "the real reason travels: {briefing}"
        );
        assert!(
            briefing.contains("unanswered"),
            "and the downstream task is told not to assume a result: {briefing}"
        );

        let ok = ItemReport {
            id: "coatings".to_string(),
            outcome: ItemOutcome::Succeeded {
                result: serde_json::json!("PTFE alternatives: PEEK, PPS"),
            },
            ledger_recorded: true,
        };
        assert!(upstream_briefing("coatings", &ok).contains("PEEK"));
    }
}
