//! Full TAOR (Think-Act-Observe-Repeat) agent loop.
//!
//! Integrates: transcript, hooks, permissions, scratchpad, cost tracking,
//! doom-loop detection, large-result handling, and auto-compaction.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use prism_embed::EmbedBackend;
use prism_ingest::llm::{ChatMessage, LlmClient, ToolDefinition};
use prism_python_bridge::tool_server::ToolServerHandle;
use serde_json::Value;

use crate::command_tools::{self, CommandToolRuntime};
use crate::hooks::HookRegistry;
use crate::models::{estimate_cost, get_model_config, request_context_window};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalGateOutcome {
    Proceed,
    Denied,
}

/// Apply the human-approval boundary for one tool call.
///
/// Keeping the decision and its denial side effects together gives callers a
/// single outcome to check immediately before dispatch. A missing approval
/// channel retains the legacy auto-approve behavior.
#[allow(clippy::too_many_arguments)]
async fn approval_gate_outcome(
    config: &AgentConfig,
    permission_decision: &crate::permissions::ToolPermissionDecision,
    tool_catalog: &ToolCatalog,
    tool_name: &str,
    args: &Value,
    call_id: &str,
    preview: &Option<String>,
    approval_rx: Option<&SharedApprovalReceiver>,
    live_permission_overrides: Option<&SharedPermissionOverrides>,
    history: &mut Vec<ChatMessage>,
    emit: &mut (dyn FnMut(AgentEvent) + Send),
) -> ApprovalGateOutcome {
    if config.auto_approve || permission_decision.auto_approved {
        return ApprovalGateOutcome::Proceed;
    }

    let tool_meta = tool_catalog.find(tool_name);
    // Feed the TUI the loaded tool metadata so approval prompts can explain
    // *why* something like execute_bash is gated.
    emit(AgentEvent::ToolApprovalRequest {
        tool_name: tool_name.to_string(),
        tool_args: args.clone(),
        call_id: call_id.to_string(),
        tool_description: tool_meta.map(|tool| tool.description.clone()),
        requires_approval: tool_meta
            .map(|tool| tool.requires_approval)
            .unwrap_or(false),
        permission_mode: tool_meta
            .map(|tool| tool.permission_mode.as_str().to_string())
            .unwrap_or_else(|| "workspace-write".to_string()),
    });

    // If no approval channel is wired, auto-approve for backward
    // compatibility. A closed wired channel is a denial.
    let Some(rx) = approval_rx else {
        return ApprovalGateOutcome::Proceed;
    };

    // Turn execution runs outside the stdin loop, so the approval receiver
    // must be shared across the spawned turn.
    let mut rx = rx.lock().await;
    match rx.recv().await {
        Some(ApprovalResponse::Allow) => ApprovalGateOutcome::Proceed,
        Some(ApprovalResponse::AllowAll) => {
            // Approve this call AND auto-approve every later one for the rest
            // of the session. Explicit denials remain intact.
            if let Some(overrides) = live_permission_overrides {
                overrides.write().await.allow_all();
            }
            ApprovalGateOutcome::Proceed
        }
        Some(ApprovalResponse::Deny) | None => {
            let denied_msg = format!("Tool '{tool_name}' denied by user.");
            emit(AgentEvent::ToolCallResult {
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
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
                tool_call_id: Some(call_id.to_string()),
            });
            ApprovalGateOutcome::Denied
        }
    }
}

// ── Constants ─────────────────────────────────────────────────────

const MAX_TOOL_RESULT_CHARS: usize = 30_000;
const DOOM_LOOP_WINDOW: usize = 3;
/// How many consecutive empty results from the same tool before we stop
const EMPTY_RESULT_MAX: usize = 2;
// VS2-P1b: bounded verify-by-execution. Three attempts means the initial
// execution plus two repairs; after that the agent must report honestly.
const CODE_REPAIR_MAX: usize = 3;
/// Canonical code-execution tool names whose failures count toward that cap.
const CODE_EXEC_TOOLS: &[&str] = &["execute_python", "execute_bash", "notebook_exec"];
/// How many times the execution-contract gate may reject a finalization in one
/// turn. This bounds the cost of false positives.
const MAX_CONTRACT_GATE_FIRINGS: usize = 2;
/// How many tools the capability-gap re-retrieval pins after the model admits
/// it lacked one.
const CAPABILITY_GAP_RETRIEVE: usize = 5;
/// Prefix of the temporary capability-gap note injected into the turn.
const CAPABILITY_GAP_NOTE: &str =
    "You said you lacked a capability. These matching tools are now available to call: ";
/// Operator-facing run labels are hints, not a second transcript.
const AGENT_RUN_LABEL_CHARS: usize = 512;

/// Cadence for refreshing the durable liveness timestamp of an active run.
///
/// The heartbeat is independent of model and tool progress, so a healthy turn
/// remains visibly alive while it waits on a slow provider or human approval.
/// Dropping the turn aborts its heartbeat and leaves the row `running`, which
/// lets the stale-running query detect a cancelled task or crashed process.
#[derive(Debug, Clone)]
pub(crate) struct AgentRunHeartbeatPolicy {
    /// Time between best-effort heartbeat writes.
    pub(crate) interval: Duration,
}

impl Default for AgentRunHeartbeatPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
        }
    }
}

/// Scoped heartbeat task for one persisted run.
pub(crate) struct AgentRunHeartbeat {
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl AgentRunHeartbeat {
    pub(crate) fn start(
        store: Option<Arc<prism_provenance::ProvenanceStore>>,
        run_id: String,
    ) -> Self {
        Self::start_with_policy(store, run_id, AgentRunHeartbeatPolicy::default())
    }

    fn start_with_policy(
        store: Option<Arc<prism_provenance::ProvenanceStore>>,
        run_id: String,
        policy: AgentRunHeartbeatPolicy,
    ) -> Self {
        let Some(store) = store else {
            return Self {
                stop_tx: None,
                task: None,
            };
        };
        if policy.interval.is_zero() {
            tracing::warn!(
                run_id,
                "agent-run heartbeat interval is zero; heartbeat disabled"
            );
            return Self {
                stop_tx: None,
                task: None,
            };
        }

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(policy.interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Tokio's first interval tick is immediate. The start write already
            // supplied the initial timestamp, so wait one full cadence.
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = ticker.tick() => {
                        if let Err(error) = store.heartbeat_agent_run(&run_id).await {
                            tracing::warn!(
                                run_id,
                                error = %error,
                                "agent-run ledger heartbeat failed; continuing turn"
                            );
                        }
                    }
                }
            }
        });

        Self {
            stop_tx: Some(stop_tx),
            task: Some(task),
        }
    }

    pub(crate) async fn stop(mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(task) = self.task.take()
            && let Err(error) = task.await
            && !error.is_cancelled()
        {
            tracing::warn!(
                error = %error,
                "agent-run heartbeat task failed while stopping"
            );
        }
    }
}

impl Drop for AgentRunHeartbeat {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Usage accumulated by the active run, retained outside the inner loop so an
/// error after a paid provider call can still close the row with honest spend.
#[derive(Debug, Default)]
pub(crate) struct AgentRunMetrics {
    pub(crate) tokens_in: u64,
    pub(crate) tokens_out: u64,
    pub(crate) cost_usd: f64,
}

impl AgentRunMetrics {
    pub(crate) fn record_usage(&mut self, usage: &UsageInfo, model: &str) {
        self.tokens_in = self.tokens_in.saturating_add(usage.input_tokens);
        self.tokens_out = self.tokens_out.saturating_add(usage.output_tokens);
        self.cost_usd += estimate_cost(usage, &get_model_config(model));
    }

    /// Roll a finished nested run's spend into this one. Cost is carried too:
    /// it is already priced against the model that actually ran, which a
    /// token-only roll-up would silently re-price against the parent's.
    pub(crate) fn absorb(&mut self, other: &Self) {
        self.tokens_in = self.tokens_in.saturating_add(other.tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(other.tokens_out);
        self.cost_usd += other.cost_usd;
    }
}

pub(crate) fn agent_run_label(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() <= AGENT_RUN_LABEL_CHARS {
        value.to_string()
    } else {
        value.chars().take(AGENT_RUN_LABEL_CHARS).collect()
    }
}

async fn start_root_agent_run(
    run: &prism_provenance::AgentRun,
) -> Option<Arc<prism_provenance::ProvenanceStore>> {
    let db_path = crate::hooks::provenance_db_path();
    match prism_provenance::ProvenanceStore::open(&db_path).await {
        Ok(store) => match store.start_agent_run(run).await {
            Ok(()) => Some(Arc::new(store)),
            Err(error) => {
                tracing::warn!(
                    run_id = %run.id,
                    error = %error,
                    "agent-run ledger start failed; continuing turn"
                );
                None
            }
        },
        Err(error) => {
            tracing::warn!(
                run_id = %run.id,
                error = %error,
                "agent-run ledger open failed; continuing turn"
            );
            None
        }
    }
}

async fn finish_root_agent_run(
    store: Option<&prism_provenance::ProvenanceStore>,
    run_id: &str,
    result: &Result<()>,
    metrics: &AgentRunMetrics,
) {
    let Some(store) = store else {
        return;
    };
    let (status, last_error) = match result {
        Ok(()) => (prism_provenance::AgentRunStatus::Completed, None),
        Err(error) => (
            prism_provenance::AgentRunStatus::Failed,
            Some(format!("{error:#}")),
        ),
    };
    if let Err(error) = store
        .finish_agent_run(
            run_id,
            status,
            metrics.tokens_in,
            metrics.tokens_out,
            metrics.cost_usd,
            last_error.as_deref(),
        )
        .await
    {
        tracing::warn!(
            run_id,
            error = %error,
            "agent-run ledger finish failed; preserving turn result"
        );
    }
}

// ── Large-result handling ─────────────────────────────────
//
// B2 RESOLUTION: the write-only in-memory `result_store` and the
// `uuid_hex8` id it minted (B3) are DELETED. The truncation message's
// promise — "the FULL result is in durable memory; call recall(...)" — is
// fulfilled by the PROVENANCE STORE, not by an in-memory map: the post-hook
// (h6) records every tool call's complete output BEFORE this truncation
// runs (h8), and the `recall` meta-tool serves it by record id or query.
// An in-memory HashMap dropped at turn end could never have served
// "durable memory" across sessions anyway; keeping it was a parallel
// half-path that lied about being the mechanism.

/// Replace a counted search result with a compact digest.
///
/// A literature search returns twenty-odd records with abstracts. Those stay in
/// the conversation for the rest of the turn and are re-sent on every
/// subsequent request, so the cost of round 1 is paid again in rounds 2..N.
/// Measured 2026-08-19: a PFAS review spent 240,967 cumulative input tokens
/// across 27 tool calls and died on its budget with no report — the second time
/// the same question died that way.
///
/// The full payload is NOT lost: the provenance post-hook records every tool
/// call's complete output BEFORE this runs, and `recall` serves it back by id
/// or query. What the model keeps is what it needs to decide the next move —
/// how many, how many new, and what they were called.
///
/// Returns `None` when this is not a countable search result, leaving the
/// normal large-result path to handle it.
fn search_digest(tool: &str, result: &Value, fresh: usize) -> Option<String> {
    if !SEARCH_TOOLS.contains(&tool) {
        return None;
    }
    let payload = cli_payload(result)?;
    let records = paper_records(&payload);
    if records.is_empty() {
        return None;
    }
    let mut out = format!(
        "{} result(s), {fresh} not seen before in this session.\n",
        records.len()
    );
    // Carry the handle INGESTION needs, not just the one citation needs.
    //
    // The digest used to emit title + dedup key and nothing else, while
    // `papers_ingest` requires `url` or `pmc`. So the harness could tell the
    // model to ingest and the model had no way to name a paper to ingest —
    // its only route to a URL was `recall`, which is the budget sink this same
    // change is trying to stop. A directive the model cannot follow is worse
    // than no directive: it burns the turn proving it cannot comply.
    //
    // `fulltext_url` is on every record the engine returns and costs ~60-100
    // chars here, against a ~30k recall to fetch the same string back.
    let mut ingestable = 0usize;
    for record in records.iter().take(SEARCH_DIGEST_TITLES) {
        let title = record
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let year = record
            .get("year")
            .and_then(Value::as_u64)
            .map(|y| format!(" ({y})"))
            .unwrap_or_default();
        let id = paper_key(record).unwrap_or_else(|| "unidentified".to_string());
        out.push_str(&format!("  - {title}{year} [{id}]\n"));
        match ingest_handle(record) {
            Some(handle) => {
                ingestable += 1;
                out.push_str(&format!("      papers_ingest {handle}\n"));
            }
            None => {
                out.push_str("      (no fetchable full text — do not spend an ingest on this)\n")
            }
        }
    }
    if records.len() > SEARCH_DIGEST_TITLES {
        out.push_str(&format!(
            "  … and {} more\n",
            records.len() - SEARCH_DIGEST_TITLES
        ));
    }
    for status in payload
        .get("source_status")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let failed = status.get("error").is_some_and(|e| !e.is_null())
            || status.get("ok").and_then(Value::as_bool) == Some(false);
        if failed && let Some(name) = status.get("source").and_then(Value::as_str) {
            out.push_str(&format!("  [source {name} returned an error]\n"));
        }
    }
    if ingestable > 0 {
        out.push_str(&format!(
            "{ingestable} of the above have full text and can be ingested directly with the \
             url shown — you do not need to recall anything to do it.\n"
        ));
    }
    out.push_str(
        "Abstracts and full records are in durable memory, not here: recall(query=\"<keywords>\") \
         pulls one back, but it spends this turn's remaining budget and persists nothing. \
         Ingesting is what turns a paper's FACTS into durable graph rows; a search result \
         itself does not survive this conversation.\n",
    );
    Some(out)
}

/// Hosts whose full text an unattended fetcher can actually retrieve.
///
/// Measured 2026-08-20 against the live web, not assumed:
///   arxiv.org                200, real PDF -> 86 assertions extracted
///   www.mdpi.com             403, hard block
///   iopscience.iop.org       302 -> validate.perfdrive.com (Radware bot manager)
///   chemrxiv.org             403 behind Cloudflare
///
/// The allowlist is deliberately small and positive. A denylist would have to
/// keep pace with every publisher's bot vendor; an allowlist fails safe, and
/// the honest fallback ("no fetchable full text") costs the model nothing.
const FETCHABLE_FULLTEXT_HOSTS: &[&str] = &[
    "arxiv.org",
    "europepmc.org",
    "ncbi.nlm.nih.gov",
    "biorxiv.org",
    "medrxiv.org",
    "openalex.org",
];

/// The best `papers_ingest` argument for this record, or `None` when nothing
/// about it is retrievable.
///
/// Added because the digest previously emitted `url=<fulltext_url>` for ANY
/// advertised location. Measured on a live run: all three ingest attempts went
/// to publisher PDFs (MDPI ×2, IOPscience) and every one came back
/// `no_fulltext_available`. The harness was confidently handing the model walls
/// to walk into, and each attempt costs a tool call and an approval.
///
/// PMC ids are preferred over any URL because `papers_ingest` fetches the JATS
/// open-access XML for them — structured text rather than a scraped PDF.
fn ingest_handle(record: &Value) -> Option<String> {
    if let Some(pmc) = record
        .pointer("/external_ids/pmc")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(format!("pmc={pmc}"));
    }
    let url = record
        .get("fulltext_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())?;
    let host = url
        .split("://")
        .nth(1)?
        .split('/')
        .next()?
        .trim_start_matches("www.");
    FETCHABLE_FULLTEXT_HOSTS
        .iter()
        .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
        .then(|| format!("url={url}"))
}

// ── Retroactive tool-output pruning ───────────────────────────────
//
// Adapted from Google's ADK long-horizon harness
// (`core/python/long-horizon-harness/horizon/context/tool_output_pruning.py`,
// Apache-2.0, © 2026 Google LLC). Modified: PRISM walks `Vec<ChatMessage>`
// rather than ADK events, resolves the tool name through `tool_call_id`, and
// points the marker at `recall` because every result here is already durable.
//
// The reason this is a SEPARATE mechanism from the per-call cap, in their
// words and confirmed here: "a cap bounds the worst single call; the pruner
// reclaims a long tail of mid-sized results that were each individually fine.
// Dropping either one leaves a real session unbounded." That is exactly the
// failure measured on 2026-08-20 — nine `recall`s, none of them oversized, 87%
// to 100% of a 200k window between them. Capping recall (which this session
// also did) bounds one call; only pruning reclaims the accumulated tail.
const PRUNE_MARKER: &str = "[output pruned to reclaim context — the FULL result is still in durable memory; \
     recall(query=\"<keywords>\") finds it if you need it again]";
/// Recent tool output kept untouched, so the model never loses the thread it
/// is currently pulling.
///
/// ADK protects "the last N USER turns" and that rule does not transfer. Their
/// long-horizon unit is a chat spanning days, so user turns are frequent. The
/// PRISM failure this exists for is ONE user turn making forty-plus tool calls
/// — `turns_seen` never passes 3, so a turn-keyed rule protects the entire
/// history and the pruner never fires in exactly the case it was added for.
/// Caught by the test below on the first run, which is why this is a token
/// countdown instead.
const PRUNE_PROTECT_TOKEN_BUDGET: usize = 40_000;
/// Below this a result is not worth the churn of pruning.
const PRUNE_MIN_PART_TOKENS: usize = 500;
/// Anti-thrash floor: rewrite history only when the reclaim is material.
const PRUNE_MIN_RECLAIM_TOKENS: usize = 20_000;
/// Never pruned. A subagent or orchestrated report cost minutes and money to
/// produce, and "recall it" is not the same bargain as for a search result.
const PRUNE_PROTECTED_TOOLS: &[&str] = &["subagent", "orchestrate", "skill", "clarify"];

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PruneOutcome {
    pub pruned: usize,
    pub reclaimed_tokens: usize,
}

fn prune_is_protected_tool(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    PRUNE_PROTECTED_TOOLS
        .iter()
        .any(|needle| lowered.contains(needle))
}

/// Zero the bodies of old, large tool results in place.
///
/// Walks newest to oldest so "recent" is a simple countdown. Mutates only when
/// the total reclaim clears [`PRUNE_MIN_RECLAIM_TOKENS`], so an ordinary short
/// turn is never rewritten for a trivial gain.
pub(crate) fn prune_stale_tool_results(history: &mut [ChatMessage]) -> PruneOutcome {
    // A tool message carries only `tool_call_id`; the NAME lives on the
    // assistant message that requested it, so protection needs this map.
    let mut name_by_call_id: HashMap<&str, String> = HashMap::new();
    for message in history.iter() {
        for call in message.tool_calls.iter().flatten() {
            name_by_call_id.insert(call.id.as_str(), call.function.name.clone());
        }
    }
    let name_by_call_id: HashMap<String, String> = name_by_call_id
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    let mut candidates: Vec<(usize, usize)> = Vec::new(); // (index, tokens)
    let mut protected_tokens = 0usize;

    for (index, message) in history.iter().enumerate().rev() {
        if message.role != "tool" {
            continue;
        }
        let Some(content) = message.content.as_deref() else {
            continue;
        };
        if content.starts_with(PRUNE_MARKER) {
            continue;
        }
        let tokens = content.len() / prism_llm::CHARS_PER_TOKEN;
        if protected_tokens < PRUNE_PROTECT_TOKEN_BUDGET {
            protected_tokens += tokens;
            continue;
        }
        if message
            .tool_call_id
            .as_deref()
            .and_then(|id| name_by_call_id.get(id))
            .is_some_and(|name| prune_is_protected_tool(name))
        {
            continue;
        }
        if tokens < PRUNE_MIN_PART_TOKENS {
            continue;
        }
        candidates.push((index, tokens));
    }

    let reclaimable: usize = candidates.iter().map(|(_, tokens)| tokens).sum();
    if reclaimable < PRUNE_MIN_RECLAIM_TOKENS {
        return PruneOutcome::default();
    }
    for (index, _) in &candidates {
        history[*index].content = Some(PRUNE_MARKER.to_string());
    }
    PruneOutcome {
        pruned: candidates.len(),
        reclaimed_tokens: reclaimable,
    }
}

fn process_large_result(content: &str) -> String {
    if content.len() <= MAX_TOOL_RESULT_CHARS {
        return content.to_string();
    }
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
        // Do NOT advertise a fixed fetch size here. This message used to promise
        // "up to 64k chars per fetch", and a turn that took the offer nine times
        // spent 86% of its window re-reading records it had already stored.
        // recall now sizes itself against the budget left, so any number printed
        // here would be a promise the tool cannot keep.
        // (see search_digest for why the ingest handle travels with the digest)
        "{truncated}\n\n[Showing first {end} of {total} chars — the FULL result is already in \
         durable memory and is NOT lost. Refining the query or lowering max_results is the \
         cheap way to get a result that fits whole; recall(id=\"<id>\") pulls a record back but \
         spends the turn's remaining budget to do it, so prefer ingesting a paper over \
         re-reading it.]"
    )
}

/// Persist the IDENTITY of every paper a search returned, into the knowledge
/// graph, with no LLM call.
///
/// Measured 2026-08-20 on the live store: `provenance_records` held 9,213 rows
/// while `emmo_entity`, `emmo_edge` and `prov_assertion` held ZERO. Every tool
/// call's raw output was durable and recallable, and not one paper had ever
/// become a node. The research was saved as a transcript and lost as knowledge.
///
/// `papers_ingest` is the expensive path — an LLM call per paper to extract
/// typed, cited claims — and it is still the right way to get FACTS. This is
/// the cheap half that was missing entirely: title, identity key and source, so
/// a later turn (or a later session) can ask the graph what it has already seen
/// instead of searching for it again.
///
/// Best-effort and quiet: a store that will not open must never fail a search
/// the model already got its answer from.
async fn persist_paper_identities(tool: &str, result: &Value) {
    if !SEARCH_TOOLS.contains(&tool) {
        return;
    }
    let Some(payload) = cli_payload(result) else {
        return;
    };
    let records = paper_records(&payload);
    if records.is_empty() {
        return;
    }
    let db_path = crate::hooks::provenance_db_path();
    let store = match prism_provenance::ProvenanceStore::open(&db_path).await {
        Ok(store) => store,
        Err(error) => {
            tracing::debug!("paper identities not persisted: {error:#}");
            return;
        }
    };
    let mut written = 0usize;
    for record in records {
        let Some(key) = paper_key(record) else {
            continue;
        };
        let title = record
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("(untitled)");
        let props = serde_json::json!({
            "identity": key,
            "title": title,
            "doi": record.get("doi"),
            "source": record.get("source"),
            "year": record.get("year"),
            "url": record.get("url"),
            "fulltext_url": record.get("fulltext_url"),
            "seen_via": tool,
        })
        .to_string();
        if let Err(error) = store
            .write_extracted_entity(&key, "Paper", Some(props), "local")
            .await
        {
            tracing::debug!("paper {key} not persisted: {error:#}");
            continue;
        }
        written += 1;
    }
    if written > 0 {
        tracing::debug!("persisted {written} paper identities from {tool}");
    }
}

// ── Saturation signal ─────────────────────────────────────────────
//
// The owner's requirement, verbatim: "the model should have some understanding
// ... is this enough or do I need to research more? and this can only be
// provided by the harness because we are not going to store all the papers in
// the context itself."
//
// Measured 2026-08-19: a literature-review turn on glm-5.2 called `papers` 14
// times, `prior_art_search` twice and `papers_ingest` ZERO times, re-finding an
// increasingly overlapping corpus, and died at round 4 on the token budget with
// no report and no artifact. Nothing told it the well was running dry, and
// nothing told it that everything it had found was about to evaporate.
//
// This counts IDENTITY, never content: a set of dedup keys and a few integers.
// The papers themselves stay out of context, which is the whole point.

/// Tools whose results are a literature search.
const SEARCH_TOOLS: &[&str] = &["papers", "papers_search", "prior_art_search"];
/// How many recent searches the new-yield ratio is computed over.
const SATURATION_WINDOW: usize = 3;
/// New-unique share below which searching is mostly re-buying what you have.
/// From Guest, Namey & Chen (2020) — a <=5% new-information rate over a short
/// run is their validated thematic-saturation criterion. Not tuned here.
const SATURATION_NEW_RATIO: f64 = 0.05;
/// Cap on the "already tried" list, so the block cannot grow without bound.
const SATURATION_QUERY_LIST_MAX: usize = 8;
/// Unique papers after which "0 ingested" becomes a directive, not a note —
/// even when searching is still yielding new work.
///
/// Measured 2026-08-20 across two full runs that both ended with a large corpus
/// and ZERO extracted facts, for opposite reasons:
///   run 1: saturated at 264 papers, then spent the rest of the budget on recall
///   run 2: 357 papers over 30 calls, NEVER saturated, budget exhausted searching
///
/// Run 2 is why saturation alone is not enough. On a broad question the
/// literature keeps yielding genuinely new papers, so a saturation-only trigger
/// never fires. Forty is well past the point where a question has the papers it
/// needs — the skill file's own rule is "ingest the papers that carry the
/// evidence, do not ingest a hundred because you found a hundred" — and it
/// leaves budget to actually ingest them.
const INGEST_NUDGE_PAPERS: usize = 40;
/// Titles kept in a compacted search result.
const SEARCH_DIGEST_TITLES: usize = 8;

#[derive(Debug, Clone)]
struct SearchCall {
    tool: String,
    query: String,
    returned: usize,
    fresh: usize,
    /// The result could not be read (truncated or unparseable), so its yield is
    /// UNKNOWN. Counting it as zero-new is how a big successful search gets
    /// mistaken for a dry well.
    unreadable: bool,
}

#[derive(Debug, Default)]
struct SaturationTracker {
    seen: std::collections::HashSet<String>,
    searches: Vec<SearchCall>,
    ingested_ok: usize,
    facts_written: usize,
    /// Sources that reported a failure, so "half the providers are down" is
    /// never silently rendered as "the literature is exhausted".
    degraded_sources: std::collections::BTreeSet<String>,
}

/// Exact-identifier dedup key for one paper record.
///
/// Mirrors `prism_retrieval::Paper::dedup_key` — DOI beats arXiv beats PMC,
/// else per-source. `prism-agent` does not depend on `prism-retrieval` (it
/// would pull a whole HTTP stack in for twelve lines), so the precedence is
/// re-stated here and pinned by test. There is deliberately NO url tier and no
/// fuzzy title match: merging records on guessed identity manufactures wrong
/// data, which is the same reason the original refuses to.
fn paper_key(paper: &Value) -> Option<String> {
    let text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(doi) = text(paper.get("doi")) {
        return Some(format!("doi:{doi}"));
    }
    let ids = paper.get("external_ids");
    if let Some(arxiv) = text(ids.and_then(|i| i.get("arxiv"))) {
        return Some(format!("arxiv:{arxiv}"));
    }
    if let Some(pmc) = text(ids.and_then(|i| i.get("pmc"))) {
        return Some(format!("pmc:{pmc}"));
    }
    match (text(paper.get("source")), text(paper.get("source_id"))) {
        (Some(source), Some(id)) => Some(format!("{source}:{id}")),
        _ => None,
    }
}

/// Unwrap a CLI tool result to the JSON its command actually printed.
///
/// Command tools return an ENVELOPE — `{root, invocation, success, stdout, …}`
/// (`command_tools::structured_success`) — with the command's JSON carried as a
/// STRING in `stdout`, truncated at 30k with an `[Output truncated]` marker. A
/// walker that looks for `papers[]` at the top level finds nothing on every
/// call, which reads as permanent saturation. Returns `None` when the payload
/// was truncated or is not JSON, so the caller can record "unknown" instead of
/// inventing a zero.
fn cli_payload(result: &Value) -> Option<Value> {
    if let Some(stdout) = result.get("stdout").and_then(Value::as_str) {
        if stdout.contains("[Output truncated]") {
            return None;
        }
        return serde_json::from_str(stdout.trim()).ok();
    }
    // A native (non-CLI) tool already returns structured JSON.
    Some(result.clone())
}

/// Every paper record in a search payload, whichever shape it arrived in.
fn paper_records(payload: &Value) -> Vec<&Value> {
    // `papers search` -> {papers:[…]}; `papers sweep` -> {outcome:{papers:[…]}};
    // `prior_art_search` -> {papers:[…], patents:[…]}. Sweep is the biggest
    // producer, so missing its nesting would silently exclude the most
    // productive tool from the count.
    for path in [
        &["papers"][..],
        &["outcome", "papers"][..],
        &["results"][..],
    ] {
        let mut node = payload;
        let mut ok = true;
        for key in path {
            match node.get(*key) {
                Some(next) => node = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && let Some(list) = node.as_array() {
            return list.iter().collect();
        }
    }
    Vec::new()
}

impl SaturationTracker {
    /// Fold one finished tool call into the counts. Called with the SAME value
    /// the provenance hook persists, so the block and the store can never
    /// disagree about what happened.
    fn observe(&mut self, tool: &str, args: &Value, result: &Value, is_error: bool) {
        if is_error {
            return;
        }
        if tool == "papers_ingest" {
            self.ingested_ok += 1;
            if let Some(payload) = cli_payload(result) {
                // `written` sits under the command's own `stored` object, not
                // at the envelope root.
                let written = payload
                    .pointer("/stored/written")
                    .or_else(|| payload.get("written"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                self.facts_written += written as usize;
            }
            return;
        }
        if !SEARCH_TOOLS.contains(&tool) {
            return;
        }

        let query = args
            .get("query")
            .or_else(|| args.get("prompt"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                // Command tools carry the query inside `args: ["--query", "…"]`.
                let list = args.get("args")?.as_array()?;
                let at = list
                    .iter()
                    .position(|a| a.as_str() == Some("--query") || a.as_str() == Some("--q"))?;
                list.get(at + 1)?.as_str().map(str::to_string)
            })
            .unwrap_or_else(|| "(query not recorded)".to_string());

        let Some(payload) = cli_payload(result) else {
            self.searches.push(SearchCall {
                tool: tool.to_string(),
                query,
                returned: 0,
                fresh: 0,
                unreadable: true,
            });
            return;
        };

        for status in payload
            .get("source_status")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let failed = status.get("error").is_some_and(|e| !e.is_null())
                || status.get("ok").and_then(Value::as_bool) == Some(false);
            if failed && let Some(name) = status.get("source").and_then(Value::as_str) {
                self.degraded_sources.insert(name.to_string());
            }
        }

        let keys: Vec<String> = paper_records(&payload)
            .into_iter()
            .filter_map(paper_key)
            .collect();
        let returned = keys.len();
        let fresh = keys.iter().filter(|k| !self.seen.contains(*k)).count();
        self.seen.extend(keys);
        self.searches.push(SearchCall {
            tool: tool.to_string(),
            query,
            returned,
            fresh,
            unreadable: false,
        });
    }

    /// Share of the recent window that was new. `None` when the window holds no
    /// readable search — an unknown yield must not read as a dry well.
    fn recent_new_ratio(&self) -> Option<f64> {
        let window: Vec<&SearchCall> = self
            .searches
            .iter()
            .rev()
            .filter(|c| !c.unreadable)
            .take(SATURATION_WINDOW)
            .collect();
        if window.len() < SATURATION_WINDOW {
            return None;
        }
        let returned: usize = window.iter().map(|c| c.returned).sum();
        if returned == 0 {
            return None;
        }
        let fresh: usize = window.iter().map(|c| c.fresh).sum();
        Some(fresh as f64 / returned as f64)
    }

    /// The block the model sees, or `None` when no search has run — a chat turn
    /// stays byte-for-byte what it was.
    /// Whether the harness should stop OFFERING search, because it has already
    /// told this run to ingest and been ignored.
    ///
    /// Exactly the condition that renders `ACTION REQUIRED` in [`Self::block`],
    /// so the prompt and the tool list can never disagree about what the run is
    /// being asked to do.
    fn should_withhold_search(&self) -> bool {
        if self.ingested_ok > 0 || self.seen.is_empty() {
            return false;
        }
        let saturated =
            matches!(self.recent_new_ratio(), Some(ratio) if ratio <= SATURATION_NEW_RATIO);
        saturated || self.seen.len() >= INGEST_NUDGE_PAPERS
    }

    fn block(&self) -> Option<String> {
        if self.searches.is_empty() {
            return None;
        }
        let mut out = String::from(
            "RESEARCH COVERAGE — counted by the harness from what your tools returned, not from papers held in this conversation.\n",
        );
        out.push_str(&format!(
            "Searches: {}   Unique papers seen: {}   Ingested: {} call(s), {} fact(s) written\n",
            self.searches.len(),
            self.seen.len(),
            self.ingested_ok,
            self.facts_written
        ));
        for (index, call) in self
            .searches
            .iter()
            .enumerate()
            .skip(self.searches.len().saturating_sub(SATURATION_WINDOW))
        {
            if call.unreadable {
                out.push_str(&format!(
                    "  #{} {} {:?} -> result too large to count (yield unknown)\n",
                    index + 1,
                    call.tool,
                    call.query
                ));
            } else {
                out.push_str(&format!(
                    "  #{} {} {:?} -> {} found, {} new\n",
                    index + 1,
                    call.tool,
                    call.query,
                    call.returned,
                    call.fresh
                ));
            }
        }

        let tried: Vec<String> = self
            .searches
            .iter()
            .rev()
            .take(SATURATION_QUERY_LIST_MAX)
            .map(|c| format!("{:?}", c.query))
            .collect();
        out.push_str(&format!(
            "Already tried, do not re-run these or close variants: {}\n",
            tried.join("; ")
        ));

        match self.recent_new_ratio() {
            Some(ratio) if ratio <= SATURATION_NEW_RATIO => out.push_str(&format!(
                "STATUS: SATURATED — the last {} searches were {:.0}% new. More searching will mostly re-find what you have. Searching further is allowed and will not be blocked; it is unlikely to pay.\n",
                SATURATION_WINDOW,
                ratio * 100.0
            )),
            Some(ratio) => out.push_str(&format!(
                "STATUS: STILL FINDING — the last {} searches were {:.0}% new.\n",
                SATURATION_WINDOW,
                ratio * 100.0
            )),
            None => out.push_str(
                "STATUS: too early to say whether coverage is saturating.\n"
                    .trim_end_matches("\n"),
            ),
        }
        if !self.degraded_sources.is_empty() {
            out.push_str(&format!(
                "CAUTION: {} reported errors this session, so low yield may mean a source is down rather than the literature being exhausted.\n",
                self.degraded_sources
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // Seen but never ingested. Two different situations, and the old text
        // was wrong about both.
        //
        // It said "nothing found this session has been persisted", which stopped
        // being true when `persist_paper_identities` landed: identity IS written
        // to the graph on every search. A block that overstates the loss is a
        // block the model learns to discount, and this is the line its own skill
        // file calls "the most important line in the block and the easiest to
        // ignore".
        //
        // And it never escalated. Measured twice now — 2026-08-19 on glm-5.2 and
        // 2026-08-20 on glm-5.3 — a research turn saturated, ingested nothing,
        // and spent the rest of its budget on `recall`: 18 `papers`, 5
        // `prior_art_search`, 9 `recall`, ZERO ingests, dead at 100% of a 200k
        // window with 570 papers saved and not one fact extracted. Once
        // searching is saturated, "more searching is unlikely to pay" is no
        // longer the useful sentence; naming the one action that converts what
        // you have into knowledge is.
        if self.ingested_ok == 0 && !self.seen.is_empty() {
            let saturated =
                matches!(self.recent_new_ratio(), Some(ratio) if ratio <= SATURATION_NEW_RATIO);
            // Saturation is not the only reason to stop searching.
            //
            // Measured 2026-08-20, run 2: 26 tool calls, 357 unique papers, ZERO
            // ingests, and never once saturated — on a broad question the
            // literature keeps yielding genuinely new papers, so a
            // saturation-only trigger never fires and the run ends with a large
            // corpus and no knowledge. Run 1 failed the same way for the
            // opposite reason (it saturated, then spent the budget on recall).
            //
            // Past this many distinct papers the marginal search is worth less
            // than the first ingest: the question is no longer "is there more?"
            // but "what do the ones I have actually say?".
            let past_enough_papers = self.seen.len() >= INGEST_NUDGE_PAPERS;
            if saturated || past_enough_papers {
                let why = if saturated {
                    "Searching is saturated, so further searches will not add knowledge"
                } else {
                    "You have found plenty; more searching is worth less now than the first ingest"
                };
                out.push_str(&format!(
                    "ACTION REQUIRED: {} papers seen, 0 ingested. {why} — and `recall` does NOT persist \
                     anything: it re-reads a stored record back into this conversation at the \
                     cost of the budget you have left. `papers_ingest` is the only action that \
                     turns these papers into durable, cited graph rows. Choose the ones whose \
                     numbers or mechanisms your question actually turns on, and ingest them now.\n",
                    self.seen.len()
                ));
            } else {
                out.push_str(&format!(
                    "NOTE: {} papers seen, 0 ingested. Their IDENTITY is already saved to the \
                     graph; their FACTS are not — only `papers_ingest` writes those, and a search \
                     result itself does not survive this conversation.\n",
                    self.seen.len()
                ));
            }
        }
        Some(out)
    }
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
    let db_path = crate::hooks::provenance_db_path();
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

/// Build the value handed to the post-hooks (esp. the provenance classifier)
/// from the model-facing `raw_content`.
///
/// VS1 fix-round #2: the hard dispatch-`Err` arm sets `raw_content` to a plain
/// `"Tool error: ..."` string that is not JSON. The old code re-parsed it and
/// fell back to a bare [`Value::String`], which
/// [`crate::tool_result::tool_result_is_error`] classifies as *not* an error
/// (its `.as_object()` is `None`) — so a failed tool call was recorded with
/// provenance `status:ok`. When the content is already valid JSON we pass it
/// through unchanged; when it is a non-JSON error string we wrap it as
/// `{success:false, error:...}` so the shared classifier records the failure.
/// A non-JSON, non-error content stays a bare string (unchanged behaviour).
pub(crate) fn hook_result_value(raw_content: &str, is_error: bool) -> Value {
    serde_json::from_str(raw_content).unwrap_or_else(|_| {
        if is_error {
            serde_json::json!({ "success": false, "error": raw_content })
        } else {
            Value::String(raw_content.to_string())
        }
    })
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

/// Workspace mutation is evidence of an edit only after it succeeds. Other
/// tools remain attempt-evidence (for example, a failing test command was
/// still run), preserving the execution contract's existing semantics.
fn tool_evidence_requires_success(tool_name: &str) -> bool {
    crate::meta_tools::MetaTool::from_name(tool_name)
        .is_some_and(|tool| tool.effect() == crate::meta_tools::MetaToolEffect::WritesWorkspace)
}

/// VS2-P1b: build the "stop self-repairing" directive for a code-exec tool that
/// has failed `streak` consecutive times. Returns `None` unless `streak >=
/// CODE_REPAIR_MAX` AND `tool` is a code-exec tool — so the call site can gate
/// on `if let Some(msg) = code_repair_directive(...)`. Pure (no loop state) so
/// it is unit-testable in isolation.
///
/// The message is a DIRECTIVE, not a narration of the trace — research shows a
/// model narrating its own trace degrades the fix. The real last error is
/// pushed to history separately (h12) BEFORE this directive so the model has
/// both the honest failure and the instruction to stop.
fn code_repair_directive(tool: &str, streak: usize) -> Option<String> {
    if streak < CODE_REPAIR_MAX {
        return None;
    }
    if !CODE_EXEC_TOOLS.contains(&tool) {
        return None;
    }
    Some(format!(
        "{tool} failed {streak} consecutive times. Self-repair beyond 2 attempts rarely \
         fixes the root cause — STOP editing and retrying. Report honestly: quote the \
         traceback (the final error line is the real cause), and either ask the user for \
         help or take a fundamentally different approach. Do not narrate the trace; act on \
         the final error line."
    ))
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
        "list_failures" => Some("list failed runs".to_string()),
        "find_tools" => args
            .get("query")
            .and_then(|value| value.as_str())
            .map(|query| format!("find tools for \"{query}\"")),
        "apply_patch" => Some("apply project patch".to_string()),
        "spawn_subagent" => args
            .get("task")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|task| format!("subagent: {}", task.lines().next().unwrap_or(task))),
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
        // Richer failure summaries for code-execution tools. Today the error
        // branch only emitted "{tool}: error — {preview}", but execute_python /
        // execute_bash / skills failures carry `success:false`, `exit_code`,
        // `timed_out`, and/or an `error` string — surface those instead of the
        // opaque content prefix so the model can see WHY it failed.
        if let Ok(val) = serde_json::from_str::<Value>(content) {
            let timed_out = val
                .get("timed_out")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let exit_code = val.get("exit_code").and_then(|v| v.as_i64());
            let err_str = val.get("error").and_then(|v| v.as_str());
            if timed_out {
                if let Some(code) = exit_code {
                    return format!("{tool_name}: error — timed out (exit {code})");
                }
                return format!("{tool_name}: error — timed out");
            }
            if let Some(code) = exit_code
                && code != 0
            {
                if let Some(err) = err_str {
                    let err_short = first_line(err, 80);
                    return format!("{tool_name}: error — exit {code}: {err_short}");
                }
                return format!("{tool_name}: error — exit {code}");
            }
            if let Some(err) = err_str {
                let err_short = first_line(err, 80);
                return format!("{tool_name}: error — {err_short}");
            }
        }
        // B1 FIX: byte-slicing at 60 panicked when the boundary landed
        // mid-UTF-8 (a non-JSON error message with multibyte text near byte
        // 60 aborted the whole turn). `first_line` is char-boundary-safe;
        // the fallback branch now uses the same helper.
        let preview = first_line(content, 80);
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

/// First line of `s`, clipped to `max` chars (whole chars, not bytes). Used by
/// `summarize_tool_result` to surface the actual error line (e.g. a Python
/// `ValueError: ...` or a bash `command not found`) instead of the raw prefix
/// of a multi-line payload. Returns "(no detail)" for empty input.
fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim_end();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let clipped: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{clipped}…")
}

pub(crate) fn compact_history(history: &mut Vec<ChatMessage>, summary: &str, keep_last: usize) {
    if history.len() <= keep_last {
        return;
    }

    // Never split an assistant message away from the tool results answering
    // it. A `tool` message refers to a `tool_call_id` announced by the
    // assistant message before it; keeping the result while dropping its
    // parent leaves a dangling reference that providers reject outright —
    // and this runs at exactly the moment context is already under pressure,
    // so the failure lands when recovery is hardest.
    //
    // Walking the boundary EARLIER only ever keeps more, so it cannot lose an
    // assistant whose results are being retained. `keep_last` is a floor, not
    // a ceiling.
    //
    // The paper-reading loop already respects this invariant deliberately (it
    // blanks tool bodies instead of removing the messages, see
    // ingest/src/paper_agent.rs) — this brings the main loop in line.
    let mut split_at = history.len().saturating_sub(keep_last);
    while split_at > 0 && history[split_at].role == "tool" {
        split_at -= 1;
    }
    let recent = history.split_off(split_at);
    history.clear();
    // NOT a system message. `iteration_messages` puts the whole preamble first
    // and appends history after it, so a system role inserted here lands in the
    // MIDDLE of the array — which providers reject outright: GLM answers
    // `1214 messages 参数非法` and mlx-lm answers `System message must be at the
    // beginning`. It never bit while compaction only ran after twenty turns;
    // compacting mid-turn under token pressure fires it on every long research
    // run, which is exactly when the failure is least recoverable.
    //
    // `user` is the role every provider accepts at any position. The marker
    // keeps it unmistakably harness-generated rather than something the human
    // said.
    history.push(ChatMessage {
        role: "user".to_string(),
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
    let mut query = String::from(user_message);
    for msg in history.iter().rev().take(2) {
        if let Some(content) = &msg.content {
            let clip: String = content.chars().take(200).collect();
            query.push(' ');
            query.push_str(&clip);
        }
    }
    query
}

#[allow(clippy::too_many_arguments)]
fn iteration_messages(
    system_prompt: &str,
    task_block: Option<&str>,
    capability_menu: Option<&str>,
    discovery_prompt: Option<&str>,
    session_memory: Option<&str>,
    saturation: Option<&str>,
    traj_steps: &[String],
    history: &[ChatMessage],
    selected_prompt: Option<&str>,
) -> Vec<ChatMessage> {
    // ONE leading system message, not six.
    //
    // These blocks were always contiguous and always ahead of `history`, so
    // concatenating them is semantically identical — and prefix caching is
    // unaffected, because providers cache on the token prefix and
    // `system_prompt` still comes first inside the joined text.
    //
    // What forced it: mlx-lm's OpenAI-compatible server answers a second
    // system-role message with `System message must be at the beginning.`
    // (HTTP 404), so every agent turn against a local MLX model failed before
    // it began. A single leading system message is what every provider
    // accepts, so this is the portable shape rather than an MLX special case.
    let preamble: Vec<String> = std::iter::once(system_prompt.to_string())
        .chain(task_block.map(str::to_string))
        .chain(capability_menu.map(str::to_string))
        .chain(discovery_prompt.map(str::to_string))
        .chain(session_memory.map(str::to_string))
        .chain(saturation.map(str::to_string))
        .chain(trajectory_block(traj_steps))
        .collect();
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: Some(preamble.join("\n\n")),
        tool_calls: None,
        tool_call_id: None,
    }];
    messages.extend(history.iter().cloned());
    if let Some(selection) = selected_prompt {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(selection.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    messages
}

/// Given capability names in relevance order, produce the request tool list:
/// selected defs + always-on meta-tools (recall + find_tools) + pinned tools,
/// deduped, with FULL definitions so a discovered tool is actually callable.
/// Shared by the keyword and neural selection paths so the meta/pinned contract
/// is identical either way.
///
/// `token_budget` bounds the WHOLE request, not just selection. Meta-tools are
/// the only unconditional entry — `recall` and `find_tools` are the escape hatch
/// and must survive every eviction path — so they are charged first. Pinned
/// (discovered) tools are charged next, in relevance order, and are admitted
/// only while the budget can carry them; selection then fills the remainder and
/// stops at the first tool it cannot afford.
///
/// The pinned cap is a BACKSTOP. Pins are admitted within budget where they are
/// created ([`pin_within_budget`]), which is also where the model is TOLD what
/// did not fit — a silently dropped tool the model asked for is exactly how the
/// old 15-tool cap misled it. This loop exists so the budget holds whatever put
/// a name in `pinned`.
fn finalize_tools(
    catalog: &ToolCatalog,
    selected: &[String],
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
) -> Vec<ToolDefinition> {
    let mut defs = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut spent: usize = meta_tools_tokens();
    let mut pinned_defs = Vec::new();
    for name in pinned_by_relevance(pinned, selected) {
        // Meta names are offered (and charged) by the meta loop below; pinning
        // one must never charge it twice.
        if crate::meta_tools::is_meta_tool(name) {
            continue;
        }
        let Some(tool) = catalog.find(name) else {
            continue;
        };
        let def = tool.to_definition();
        let cost = crate::tool_catalog::definition_tokens(&def);
        if spent + cost > token_budget {
            break;
        }
        spent += cost;
        seen.insert(name.clone());
        pinned_defs.push(def);
    }
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
        if pinned.contains(name) {
            continue; // already paid for; the pinned loop below adds it
        }
        // Ranked retrieval is only progressive disclosure if the ranking is
        // TRUNCATED. Stop at the count cap even when the budget would afford
        // more: the block is re-sent every request, so its cost is paid once
        // per turn, not once per session.
        if defs.len() >= crate::tool_catalog::MAX_REQUEST_TOOLS {
            break;
        }
        if seen.insert(name.clone())
            && let Some(tool) = catalog.find(name)
        {
            let def = tool.to_definition();
            let cost = crate::tool_catalog::definition_tokens(&def);
            if spent + cost > token_budget {
                seen.remove(name);
                break;
            }
            spent += cost;
            defs.push(def);
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
    defs.extend(pinned_defs);
    defs
}

/// Charged cost of the always-on meta-tools. They are mandatory, so every
/// budget computation starts by subtracting this.
fn meta_tools_tokens() -> usize {
    crate::meta_tools::definitions()
        .iter()
        .map(|t| crate::tool_catalog::definition_tokens(&t.to_definition()))
        .sum()
}

/// `pinned` in relevance order (`selected` is the ranking already used for
/// selection), unranked names last, ties broken by name. `pinned` is a HashSet:
/// without this the budget cap above would drop a DIFFERENT tool on every run.
fn pinned_by_relevance<'a>(
    pinned: &'a std::collections::HashSet<String>,
    selected: &[String],
) -> Vec<&'a String> {
    let rank: std::collections::HashMap<&str, usize> = selected
        .iter()
        .enumerate()
        .map(|(position, name)| (name.as_str(), position))
        .collect();
    let mut ordered: Vec<&String> = pinned.iter().collect();
    ordered.sort_by(|a, b| {
        let ra = rank.get(a.as_str()).copied().unwrap_or(usize::MAX);
        let rb = rank.get(b.as_str()).copied().unwrap_or(usize::MAX);
        ra.cmp(&rb).then_with(|| a.cmp(b))
    });
    ordered
}

/// Tokens of pinned-tool definitions a request may carry. Meta-tools are
/// mandatory and never evicted, so pins may only spend what is left after them.
fn pin_token_budget(token_budget: usize) -> usize {
    token_budget.saturating_sub(meta_tools_tokens())
}

/// Admit `candidates` into `pinned` while the pinned set still fits
/// [`pin_token_budget`]. Returns, in order, the names that did NOT fit.
///
/// A pin is not free: its FULL definition rides in every later request this
/// turn. `find_tools`'s `limit` is model-controlled, so one call could pin the
/// whole catalog: 33,410 charged tokens as measured by the adversarial review
/// against the live catalog, 37,960 as measured by
/// `one_unbounded_find_tools_call_cannot_blow_the_tool_budget` against its
/// 130-tool stand-in — versus an 8k model's ENTIRE 2,048-token budget, before a
/// single word of prompt or history. Candidates arrive in relevance order and
/// are admitted greedily until the first one that does not fit; the rest are
/// returned so the CALLER can tell the model what is not callable, instead of
/// dropping it silently.
fn pin_within_budget(
    catalog: &ToolCatalog,
    pinned: &mut std::collections::HashSet<String>,
    candidates: impl IntoIterator<Item = String>,
    token_budget: usize,
) -> Vec<String> {
    let budget = pin_token_budget(token_budget);
    let cost_of = |name: &str| {
        catalog
            .find(name)
            .map(|t| crate::tool_catalog::definition_tokens(&t.to_definition()))
    };
    let mut spent: usize = pinned.iter().filter_map(|n| cost_of(n)).sum();
    let mut rejected = Vec::new();
    let mut full = false;
    for name in candidates {
        // Already callable, or served unconditionally by the meta layer.
        if pinned.contains(&name) || crate::meta_tools::is_meta_tool(&name) {
            continue;
        }
        let Some(cost) = cost_of(&name) else {
            continue; // not in the catalog: nothing to pin, nothing to report
        };
        if full || spent + cost > budget {
            full = true;
            rejected.push(name);
            continue;
        }
        spent += cost;
        pinned.insert(name);
    }
    rejected
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

fn apply_tool_tier(
    tools: Vec<ToolDefinition>,
    pinned: &std::collections::HashSet<String>,
    core_tools_only: bool,
) -> Vec<ToolDefinition> {
    if core_tools_only {
        tier_to_core(tools, pinned)
    } else {
        tools
    }
}

fn capability_menu_for_request(
    catalog: &ToolCatalog,
    relevant_tools: &[ToolDefinition],
) -> Option<String> {
    if !neural_tools_enabled() {
        return None;
    }
    let included = relevant_tools
        .iter()
        .map(|definition| definition.function.name.clone())
        .collect::<std::collections::HashSet<_>>();
    let entries = catalog_entries(catalog);
    crate::capability::capability_menu(&entries, &included, 150, 80)
}

/// Keyword selection (fallback path): the catalog ranked by keyword match on
/// Withhold the SEARCH tools once the harness has told the run to ingest.
///
/// Measured 2026-08-20, third full run: 495 unique papers, 12 `papers` calls,
/// ZERO ingests — with the ACTION REQUIRED directive firing on every turn from
/// paper 40 onward. The block was in the prompt and the model kept searching.
///
/// Google's ADK long-horizon harness reached the same conclusion the same way
/// (arXiv 2608.17528 / their Pattern 4): "strip the tools off the request and
/// let the model write a plain-text handoff. Leave the tools attached and it
/// keeps calling them, trading a runaway loop for an error loop."
///
/// So this is not a stronger instruction — a directive the model can decline is
/// not a control. It removes the AFFORDANCE. `papers_ingest`, `recall`, the
/// graph tools and everything else stay; only the three literature-search tools
/// go, leaving "ingest what you have" and "answer" as the reachable moves.
/// Nothing is refused and nothing errors: the tool simply is not offered, which
/// is the difference between annotating and muzzling.
///
/// `recall` goes with them, learned the hard way. Removing only search produced
/// exactly the failure ADK warns about — "trading a runaway loop for an error
/// loop": measured 2026-08-20 run 4, the model answered the withdrawal of search
/// with SIX `recall` calls, three of them against hallucinated record ids that
/// could only error. Recall is re-reading, which is the behaviour this state
/// exists to stop, and it is not needed to ingest: the digest already carries
/// `papers_ingest url=…` / `pmc=…` handles, and 20 fetchable ones were in front
/// of it at the time. Narrow the affordances to converting and answering, or the
/// model finds the next way to keep gathering.
fn withhold_search_tools(defs: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    defs.into_iter()
        .filter(|d| {
            let name = d.function.name.as_str();
            !SEARCH_TOOLS.contains(&name) && name != "recall"
        })
        .collect()
}

/// `route`, filled until `token_budget` is spent, then meta-tools + pinned.
fn assemble_request_tools(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
) -> Vec<ToolDefinition> {
    let selected = catalog.names_by_relevance(route);
    finalize_tools(catalog, &selected, pinned, token_budget)
}

struct RequestToolSelection {
    definitions: Vec<ToolDefinition>,
    applied: crate::influence::ToolSelectionMethod,
}

fn assemble_request_tools_with_status(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
) -> RequestToolSelection {
    RequestToolSelection {
        definitions: assemble_request_tools(catalog, route, pinned, token_budget),
        applied: crate::influence::ToolSelectionMethod::Keyword,
    }
}

/// Neural selection (`PRISM_NEURAL_TOOLS`): embedding retrieval over the
/// capability index ranks the WHOLE catalog for `route`; the token budget then
/// decides how far down that ranking the request can afford to go. Falls back
/// to keyword selection when retrieval yields nothing (no embeddings ready /
/// backend error) — so it can never do worse than today.
#[cfg(test)]
async fn assemble_request_tools_neural(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
    backend: &dyn EmbedBackend,
) -> Vec<ToolDefinition> {
    assemble_request_tools_neural_with_status(catalog, route, pinned, token_budget, backend)
        .await
        .definitions
}

async fn assemble_request_tools_neural_with_status(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
    backend: &dyn EmbedBackend,
) -> RequestToolSelection {
    let entries: Vec<(String, String)> = catalog
        .iter()
        .map(|t| (t.name.clone(), format!("{}: {}", t.name, t.description)))
        .collect();
    let index = crate::capability::global_index(entries, backend).await;
    let selected = index.retrieve(route, catalog.len(), backend).await;
    if selected.is_empty() {
        tracing::debug!(
            "tool selection: neural retrieval empty (embeddings not ready) — keyword fallback"
        );
        return assemble_request_tools_with_status(catalog, route, pinned, token_budget);
    }
    tracing::debug!(
        retrieved = selected.len(),
        "tool selection: neural embedding retrieval used"
    );
    RequestToolSelection {
        definitions: finalize_tools(catalog, &selected, pinned, token_budget),
        applied: crate::influence::ToolSelectionMethod::Cosine,
    }
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

/// Whether experimental inference-context influence routing was explicitly
/// requested. It is deliberately OFF by default until the paired benchmark
/// beats cosine at the same final rendered prompt-token budget.
fn context_influence_enabled() -> bool {
    context_influence_value_enabled(std::env::var("PRISM_CONTEXT_INFLUENCE").ok().as_deref())
}

fn context_influence_value_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("on")
    })
}

#[derive(Debug)]
struct InfluenceSelectionMeta {
    index_id: String,
    ranked_candidates: Vec<String>,
    scorer_input_tokens: u64,
    scoring_ms: u64,
    model_sha256: String,
    template_sha256: String,
}

/// Build the immutable prompt baseline for influence interventions: native
/// meta-tools first, followed by the pinned full definitions that really fit
/// this request's definition budget. This exact order is reused for final
/// generation so a candidate's measured insertion site and deployed insertion
/// site do not drift.
#[cfg(any(feature = "local-inference", test))]
fn influence_fixed_tools(
    catalog: &ToolCatalog,
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
) -> Vec<ToolDefinition> {
    finalize_tools(catalog, &[], pinned, token_budget)
}

/// Snapshot the full candidate pool in catalog order. Meta-tools and pinned
/// tools are fixed context, not interventions, and therefore cannot also be
/// candidates.
#[cfg(any(feature = "local-inference", test))]
fn influence_candidates(
    catalog: &ToolCatalog,
    pinned: &std::collections::HashSet<String>,
) -> Vec<ToolDefinition> {
    catalog
        .iter()
        .filter(|tool| !crate::meta_tools::is_meta_tool(&tool.name) && !pinned.contains(&tool.name))
        .map(|tool| tool.to_definition())
        .collect()
}

/// Reorder the approximate-budget result to match the scorer intervention:
/// fixed baseline tools first, then influence-ranked candidates. Names are
/// unique within the callable surface; any unexpected remainder is sorted so
/// request construction stays deterministic rather than inheriting HashMap
/// iteration order.
#[cfg(any(feature = "local-inference", test))]
fn order_influence_request(
    packed: Vec<ToolDefinition>,
    fixed: &[ToolDefinition],
    ranked_candidates: &[String],
) -> Vec<ToolDefinition> {
    let mut by_name = packed
        .into_iter()
        .map(|definition| (definition.function.name.clone(), definition))
        .collect::<HashMap<_, _>>();
    let mut ordered = Vec::with_capacity(by_name.len());
    for definition in fixed {
        if let Some(definition) = by_name.remove(&definition.function.name) {
            ordered.push(definition);
        }
    }
    for name in ranked_candidates {
        if let Some(definition) = by_name.remove(name) {
            ordered.push(definition);
        }
    }
    let mut remainder = by_name.into_values().collect::<Vec<_>>();
    remainder.sort_by(|left, right| left.function.name.cmp(&right.function.name));
    ordered.extend(remainder);
    ordered
}

/// Score and pack one exact local-GGUF influence request. Errors collapse to
/// stable machine-readable reason codes at the caller while their diagnostics
/// remain in logs. This function never substitutes another model or backend.
#[cfg(not(feature = "local-inference"))]
async fn influence_tool_selection(
    _llm: &LlmClient,
    _catalog: &ToolCatalog,
    _messages: &[ChatMessage],
    _pinned: &std::collections::HashSet<String>,
    _token_budget: usize,
) -> std::result::Result<(RequestToolSelection, InfluenceSelectionMeta), &'static str> {
    // Refuse before touching the artifact. Besides preserving the precise
    // status, this prevents a non-local build from hashing a multi-gigabyte
    // GGUF that it cannot use.
    Err("local_inference_feature_disabled")
}

#[cfg(feature = "local-inference")]
async fn influence_tool_selection(
    llm: &LlmClient,
    catalog: &ToolCatalog,
    messages: &[ChatMessage],
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
) -> std::result::Result<(RequestToolSelection, InfluenceSelectionMeta), &'static str> {
    if !prism_ingest::llm::is_local_gguf_url(&llm.config().base_url) {
        return Err("target_model_unsupported");
    }

    let model_identity = match llm.local_model_identity().await {
        Ok(prism_ingest::llm::LocalModelIdentityOutcome::Verified { identity }) => identity,
        Ok(prism_ingest::llm::LocalModelIdentityOutcome::Unavailable { code, detail }) => {
            tracing::debug!(?code, detail, "local model identity unavailable");
            return Err(match code {
                prism_ingest::llm::LocalPromptInfluenceUnavailableCode::HostedBackend => {
                    "target_model_unsupported"
                }
                prism_ingest::llm::LocalPromptInfluenceUnavailableCode::LocalInferenceFeatureDisabled => {
                    "local_inference_feature_disabled"
                }
                prism_ingest::llm::LocalPromptInfluenceUnavailableCode::DescriptorBackedIdentityUnavailable => {
                    "descriptor_backed_identity_unavailable"
                }
            });
        }
        Err(error) => {
            tracing::warn!(error = %error, "influence routing could not bind target-model identity");
            return Err("target_model_unverified");
        }
    };
    if model_identity.sha256 != prism_ingest::llm::BUNDLED_GEMMA.sha256
        || model_identity.size_bytes != prism_ingest::llm::BUNDLED_GEMMA.size_bytes
    {
        tracing::warn!(
            actual_sha256 = model_identity.sha256,
            actual_size_bytes = model_identity.size_bytes,
            expected_sha256 = prism_ingest::llm::BUNDLED_GEMMA.sha256,
            expected_size_bytes = prism_ingest::llm::BUNDLED_GEMMA.size_bytes,
            "influence routing refused a local model other than the pinned Gemma artifact"
        );
        return Err("target_model_unverified");
    }
    let fixed = influence_fixed_tools(catalog, pinned, token_budget);
    let candidates = influence_candidates(catalog, pinned);
    if candidates.is_empty() {
        return Err("candidate_pool_empty");
    }

    let outcome = match llm
        .score_local_tool_influence(messages, &fixed, &candidates)
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::warn!(error = %error, "local prompt-influence scoring failed");
            return Err("influence_scoring_failed");
        }
    };
    let report = match outcome {
        prism_ingest::llm::LocalPromptInfluenceOutcome::Scored { report } => report,
        prism_ingest::llm::LocalPromptInfluenceOutcome::Unavailable { code, detail } => {
            tracing::debug!(?code, detail, "local prompt-influence scoring unavailable");
            return Err(match code {
                prism_ingest::llm::LocalPromptInfluenceUnavailableCode::HostedBackend => {
                    "target_model_unsupported"
                }
                prism_ingest::llm::LocalPromptInfluenceUnavailableCode::LocalInferenceFeatureDisabled => {
                    "local_inference_feature_disabled"
                }
                prism_ingest::llm::LocalPromptInfluenceUnavailableCode::DescriptorBackedIdentityUnavailable => {
                    "descriptor_backed_identity_unavailable"
                }
            });
        }
    };
    if report.model_sha256 != model_identity.sha256
        || report.model_size_bytes != model_identity.size_bytes
    {
        tracing::warn!(
            identity_sha256 = model_identity.sha256,
            report_sha256 = report.model_sha256,
            "local model identity changed between the target gate and influence scoring"
        );
        return Err("target_model_identity_changed");
    }
    if report.candidates.len() != candidates.len()
        || report.candidates.iter().any(|score| {
            candidates
                .get(score.candidate_index)
                .is_none_or(|definition| definition.function.name != score.tool_name)
        })
    {
        tracing::warn!(
            expected = candidates.len(),
            actual = report.candidates.len(),
            "local prompt-influence scorer returned a misaligned candidate pool"
        );
        return Err("influence_scores_misaligned");
    }

    let scores = report
        .candidates
        .iter()
        .filter_map(|score| {
            score
                .normalized_js_divergence_per_added_prompt_token
                .filter(|value| value.is_finite())
                .map(
                    |score_per_added_token| crate::influence::CandidateInfluence {
                        name: score.tool_name.clone(),
                        score_per_added_token,
                    },
                )
        })
        .collect::<Vec<_>>();
    if scores.is_empty() {
        return Err("influence_scores_unusable");
    }
    if let Some(reason) = crate::influence::classify_score_discrimination(&scores).refusal_reason()
    {
        return Err(reason);
    }

    let index =
        crate::influence::global_index(candidates, &model_identity.sha256, &report.template_sha256);
    let ranked_candidates = index.rank(&scores);
    let packed = finalize_tools(catalog, &ranked_candidates, pinned, token_budget);
    let definitions = order_influence_request(packed, &fixed, &ranked_candidates);
    let scorer_input_tokens = report
        .candidates
        .iter()
        .fold(report.baseline_prompt_tokens, |total, score| {
            total.saturating_add(score.candidate_prompt_tokens)
        });
    let scoring_ms = report.total_scoring_wall_time_micros.saturating_add(999) / 1_000;
    let metadata = InfluenceSelectionMeta {
        index_id: index.identity().to_string(),
        ranked_candidates,
        scorer_input_tokens,
        scoring_ms,
        model_sha256: index.model_sha256().to_string(),
        template_sha256: index.template_sha256().to_string(),
    };
    Ok((
        RequestToolSelection {
            definitions,
            applied: crate::influence::ToolSelectionMethod::Influence,
        },
        metadata,
    ))
}

async fn baseline_tool_selection(
    catalog: &ToolCatalog,
    route: &str,
    pinned: &std::collections::HashSet<String>,
    token_budget: usize,
) -> RequestToolSelection {
    if neural_tools_enabled() {
        let entries = catalog_entries(catalog);
        match crate::capability::global_index_if_ready(&entries)
            .and(crate::embeddings::backend_if_ready())
        {
            Some(backend) => {
                tracing::debug!("tool selection: neural path (model + index ready)");
                assemble_request_tools_neural_with_status(
                    catalog,
                    route,
                    pinned,
                    token_budget,
                    backend.as_ref(),
                )
                .await
            }
            None => {
                tracing::debug!(
                    "tool selection: keyword path (neural model/index warming in background)"
                );
                spawn_neural_warm(entries);
                assemble_request_tools_with_status(catalog, route, pinned, token_budget)
            }
        }
    } else {
        assemble_request_tools_with_status(catalog, route, pinned, token_budget)
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
    policy: Option<&mut prism_policy::PolicyEngine>,
    subagent_lanes: Option<&prism_python_bridge::ToolServerPool>,
) -> Result<()> {
    let session_id = crate::hooks::provenance_session_id();
    let run =
        prism_provenance::new_agent_run(&session_id, "agent", &agent_run_label(user_message), None);
    // Generate the id independently of persistence. A child can still retain
    // the intended topology if this best-effort parent write is unavailable.
    let run_store = start_root_agent_run(&run).await;
    let run_heartbeat = AgentRunHeartbeat::start(run_store.clone(), run.id.clone());
    let mut run_metrics = AgentRunMetrics::default();
    let surface_policy = crate::skills::SkillSurfacePolicy::default();
    let turn_skill_context = crate::skills::prepare_turn_skill_context(
        user_message,
        &command_tool_runtime.project_root,
        &surface_policy,
    );
    let result = match turn_skill_context {
        Ok(turn_skill_context) => {
            crate::skills::with_turn_skill_context(
                turn_skill_context,
                run_turn_inner(
                    llm,
                    tool_server,
                    command_tool_runtime,
                    history,
                    tool_catalog,
                    config,
                    user_message,
                    task,
                    transcript,
                    hooks,
                    permissions,
                    live_permission_overrides,
                    scratchpad,
                    emit,
                    approval_rx,
                    policy,
                    subagent_lanes,
                    &run.id,
                    &run.session_id,
                    &mut run_metrics,
                ),
            )
            .await
        }
        Err(error) => {
            // Refuse before reprompting or model inference. In particular, do
            // not let an ambiguous `$name` fall through to a model that might
            // guess the first skill or workflow it sees.
            let refusal = format!("Skill/workflow selection refused: {error}");
            history.push(ChatMessage {
                role: "user".to_string(),
                content: Some(user_message.to_string()),
                tool_calls: None,
                tool_call_id: None,
            });
            transcript.append(TranscriptEntry::new("user", user_message));
            history.push(ChatMessage {
                role: "assistant".to_string(),
                content: Some(refusal.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
            transcript.append(TranscriptEntry::new("assistant", &refusal));
            emit(AgentEvent::TextDelta {
                text: refusal.clone(),
            });
            emit(AgentEvent::TextFlush);
            emit(AgentEvent::TurnComplete {
                text: Some(refusal),
                has_more: false,
                usage: None,
                total_usage: Some(UsageInfo::default()),
                estimated_cost: Some(0.0),
            });
            Ok(())
        }
    };
    run_heartbeat.stop().await;
    finish_root_agent_run(run_store.as_deref(), &run.id, &result, &run_metrics).await;
    result
}

/// Execute a turn inside an already-created durable run. Subagents create
/// their child row at the spawn boundary, then call this function so the same
/// row—and not a duplicate—is updated by the nested loop.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_inner(
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
    // Lane pool for delegated (subagent) turns. `None` = the legacy
    // serialized path: a spawned subagent borrows THIS turn's tool server.
    subagent_lanes: Option<&prism_python_bridge::ToolServerPool>,
    current_run_id: &str,
    current_session_id: &str,
    run_metrics: &mut AgentRunMetrics,
) -> Result<()> {
    let turn_skill_context = crate::skills::current_turn_skill_context();
    let skill_surface_policy = crate::skills::SkillSurfacePolicy::default();
    // ── 1. Push user message ──────────────────────────────────────
    history.push(ChatMessage {
        role: "user".to_string(),
        content: Some(user_message.to_string()),
        tool_calls: None,
        tool_call_id: None,
    });
    transcript.append(TranscriptEntry::new("user", user_message));

    let mut total_usage = UsageInfo::default();
    // `AgentConfig` can lag a runtime `/model` switch. Billing must follow the
    // client that actually made each primary-model request.
    let billing_model = llm.config().model.clone();

    // ── 1b. Pre-flight reprompt ───────────────────────────────────
    // Deterministic triage FIRST (pure function, no I/O): a well-formed expert
    // query returns Proceed here having spent nothing — no classifier call, no
    // added latency, no added tokens. Only a message carrying positive evidence
    // of misrouting, or an opening directive that names nothing, reaches the one
    // cheap LLM call. See `reprompt`.
    //
    // A stale routing hint is stripped BEFORE anything else: the strip has to be
    // unconditional and at the START of the turn, because `run_turn` can also
    // leave via `?`, budget exhaustion, or the max-iterations arm, and a hint
    // that survived one of those would silently misroute every later turn.
    history.retain(|m| {
        !m.content
            .as_deref()
            .is_some_and(|c| c.starts_with(crate::reprompt::ROUTE_HINT_PREFIX))
    });
    // Only an attended turn may ask. A subagent or a research task step has no
    // human on the other end, so its question would land as a dead tool result
    // — those get the routing hint, which carries the same honesty.
    let can_ask = task.is_none() && config.subagent_depth == 0;
    // The domain words the pre-flight menus may use come from the project's
    // ACTIVE ontology — never a Rust domain list. Resolved once per turn.
    let domain_vocabulary =
        crate::reprompt::DomainVocabulary::for_project(&command_tool_runtime.project_root);
    // An exhausted budget must not pay for a classifier call either.
    let preflight = if transcript.budget_exhausted() || turn_skill_context.has_explicit_selections()
    {
        (crate::reprompt::Preflight::Proceed, None)
    } else {
        crate::reprompt::preflight(
            llm,
            config,
            user_message,
            history,
            can_ask,
            &domain_vocabulary,
        )
        .await
    };
    // The classifier is a real billed call. Fold it into the turn's usage and
    // the cost ledger like any other — an LLM call nobody accounts for is how a
    // bill becomes a surprise.
    if let Some(billed) = preflight.1 {
        let usage = UsageInfo {
            input_tokens: billed.usage.prompt_tokens,
            output_tokens: billed.usage.completion_tokens,
            ..Default::default()
        };
        transcript.record_cost(
            "reprompt_classifier",
            billed.usage.prompt_tokens,
            billed.usage.completion_tokens,
        );
        run_metrics.record_usage(&usage, &billed.model);
        total_usage += usage;
    }
    match preflight.0 {
        crate::reprompt::Preflight::Proceed => {}
        crate::reprompt::Preflight::Route { hint } => {
            // Intent resolved and servable — do not interrogate. Hand the model
            // the capability that serves it (and, for an intent PRISM cannot
            // serve, the plain statement that it cannot) so the turn is routed
            // rather than guessed.
            history.push(ChatMessage {
                role: "system".to_string(),
                content: Some(hint),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        crate::reprompt::Preflight::Ask { question, key } => {
            // ONE consolidated question, and the turn ends. No agent loop runs,
            // so this path is cheaper than the confidently-irrelevant answer it
            // replaces. The question itself is the never-ask-twice ledger: it
            // lands in `history` verbatim, which is what resume restores.
            tracing::info!(intent = %key, "pre-flight reprompt: asked instead of answering");
            emit(AgentEvent::TextDelta {
                text: question.clone(),
            });
            emit(AgentEvent::TextFlush);
            history.push(ChatMessage {
                role: "assistant".to_string(),
                content: Some(question.clone()),
                tool_calls: None,
                tool_call_id: None,
            });
            transcript.append(TranscriptEntry::new("assistant", question.as_str()));
            let estimated_cost = run_metrics.cost_usd;
            emit(AgentEvent::TurnComplete {
                text: Some(question),
                has_more: false,
                usage: None,
                total_usage: Some(total_usage),
                estimated_cost: Some(estimated_cost),
            });
            return Ok(());
        }
    }

    // One line per executed tool step — feeds the deterministic TRAJECTORY
    // block injected into every iteration's context.
    let mut traj_steps: Vec<String> = Vec::new();
    let mut saturation = SaturationTracker::default();
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
    // VS2-P1b: track consecutive FAILED code-exec calls per tool name. Resets
    // on any successful code-exec call. Mirrors empty_result_streak's pattern.
    let mut code_failure_streak: HashMap<String, usize> = HashMap::new();
    // VS2-P1 FIX-6: reset the provenance repair-chain memory at turn start so a
    // new turn's first code run is not tagged repair_attempt pointing at last
    // turn's failure, and so an in-process subagent does not splice into the
    // parent's chain. Matches the turn-scope of the streak maps above.
    crate::hooks::reset_code_run_chain();
    // Tools the model discovered via find_tools this turn — pinned so their
    // FULL definitions stay in the request every later iteration. Without this,
    // find_tools returned names the model could never actually call.
    let mut pinned_tools: std::collections::HashSet<String> =
        turn_skill_context.pinned_tools().cloned().collect();
    // Execution-contract gate state: names of tools that ACTUALLY EXECUTED
    // this turn (recorded at h5, after the permission / policy / approval
    // gates — a blocked call produced no evidence and must not count), and how
    // many times the finalization gate has fired. Capping the firings bounds
    // the cost of a false positive; it is not a completeness guarantee.
    let mut tools_used_this_turn: Vec<String> = Vec::new();
    let mut contract_gate_firings: usize = 0;
    // Tool-definition token budget for THIS model's real context window,
    // resolved once per turn (the catalog and the model do not change mid-turn).
    let tool_token_budget =
        crate::tool_catalog::tool_token_budget(request_context_window(llm.config()));
    // Capability-gap re-retrieval: fires at most once per turn (see 2g).
    let mut capability_gap_retried = false;

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
        // Rank by relevance, then fill until the TOKEN BUDGET is spent — not a
        // fixed count. On today's models the whole catalog fits, so the model
        // sees everything it has; only a genuinely small context truncates, and
        // then by relevance. Route from the CURRENT step (not just the opening
        // message) and fold in tools pinned via find_tools or by the
        // capability-gap retry, so discovery makes tools actually callable and
        // the working set follows the task. Done before message assembly so the
        // L1 capability menu can reflect what's already callable.
        // Recomputed each turn from the running counts, so the model watches its
        // own yield fall instead of being told once and forgetting.
        let saturation_block = saturation.block();
        let route = routing_query(user_message, history);
        let influence_requested = context_influence_enabled();
        // Influence scoring and successful generation deliberately omit the L1
        // capability menu: otherwise the scorer would measure one prompt while
        // deployment used another. Baseline/fallback requests preserve the
        // existing menu behavior.
        let influence_messages = iteration_messages(
            &config.system_prompt,
            task_block.as_deref(),
            None,
            turn_skill_context.discovery_prompt.as_deref(),
            session_memory.as_deref(),
            saturation_block.as_deref(),
            &traj_steps,
            history,
            turn_skill_context.selected_prompt.as_deref(),
        );
        let (selection, mut influence_meta, mut fallback_reason) = if influence_requested {
            match influence_tool_selection(
                llm,
                tool_catalog,
                &influence_messages,
                &pinned_tools,
                tool_token_budget,
            )
            .await
            {
                Ok((selection, metadata)) => (selection, Some(metadata), None),
                Err(reason) => (
                    baseline_tool_selection(tool_catalog, &route, &pinned_tools, tool_token_budget)
                        .await,
                    None,
                    Some(reason.to_string()),
                ),
            }
        } else {
            (
                baseline_tool_selection(tool_catalog, &route, &pinned_tools, tool_token_budget)
                    .await,
                None,
                None,
            )
        };
        let mut applied_method = selection.applied;
        let mut relevant_tools =
            apply_tool_tier(selection.definitions, &pinned_tools, config.core_tools_only);

        // A ready scorer/index is not enough to claim priming. At least one
        // influence-ranked candidate must survive both final packing and the
        // model's core-tier filter and actually reach generation.
        if let Some(metadata) = influence_meta.as_ref()
            && crate::influence::applied_candidates(&metadata.ranked_candidates, &relevant_tools)
                .is_empty()
        {
            fallback_reason = Some("influence_candidates_evicted".to_string());
            influence_meta = None;
            let fallback =
                baseline_tool_selection(tool_catalog, &route, &pinned_tools, tool_token_budget)
                    .await;
            applied_method = fallback.applied;
            relevant_tools =
                apply_tool_tier(fallback.definitions, &pinned_tools, config.core_tools_only);
        }

        let mut capability_menu = influence_meta
            .is_none()
            .then(|| capability_menu_for_request(tool_catalog, &relevant_tools))
            .flatten();
        let mut messages = if influence_meta.is_some() {
            influence_messages
        } else {
            iteration_messages(
                &config.system_prompt,
                task_block.as_deref(),
                capability_menu.as_deref(),
                turn_skill_context.discovery_prompt.as_deref(),
                session_memory.as_deref(),
                saturation_block.as_deref(),
                &traj_steps,
                history,
                turn_skill_context.selected_prompt.as_deref(),
            )
        };

        let mut priming_status = if influence_requested {
            crate::influence::ContextPrimingStatus::Fallback {
                requested: crate::influence::ToolSelectionMethod::Influence,
                applied: applied_method,
                reason: fallback_reason
                    .clone()
                    .unwrap_or_else(|| "influence_status_unresolved".to_string()),
            }
        } else {
            crate::influence::ContextPrimingStatus::NotRequested {
                applied: applied_method,
            }
        };

        if let Some(metadata) = influence_meta {
            let selected_candidates =
                crate::influence::applied_candidates(&metadata.ranked_candidates, &relevant_tools);
            match llm.render_local_prompt(&messages, &relevant_tools).await {
                Ok(rendered) if rendered.template_sha256 == metadata.template_sha256 => {
                    priming_status = crate::influence::ContextPrimingStatus::Primed {
                        index_id: metadata.index_id,
                        selected_candidates,
                        exact_context_tokens: rendered.token_count,
                        scorer_input_tokens: metadata.scorer_input_tokens,
                        scoring_ms: metadata.scoring_ms,
                        model_sha256: metadata.model_sha256,
                        template_sha256: metadata.template_sha256,
                    };
                }
                Ok(rendered) => {
                    tracing::warn!(
                        scored_template = metadata.template_sha256,
                        rendered_template = rendered.template_sha256,
                        "influence routing template changed before generation"
                    );
                    fallback_reason = Some("template_identity_changed".to_string());
                }
                Err(error) => {
                    tracing::warn!(error = %error, "exact influence prompt render failed");
                    fallback_reason = Some("exact_prompt_render_failed".to_string());
                }
            }

            // Rendering/template identity is the last truth gate. If it fails,
            // rebuild the complete baseline request and report the method that
            // really reached the LLM; never reuse influence-ranked tools under
            // a fallback label.
            if let Some(reason) = fallback_reason.as_ref() {
                let fallback =
                    baseline_tool_selection(tool_catalog, &route, &pinned_tools, tool_token_budget)
                        .await;
                applied_method = fallback.applied;
                relevant_tools =
                    apply_tool_tier(fallback.definitions, &pinned_tools, config.core_tools_only);
                capability_menu = capability_menu_for_request(tool_catalog, &relevant_tools);
                messages = iteration_messages(
                    &config.system_prompt,
                    task_block.as_deref(),
                    capability_menu.as_deref(),
                    turn_skill_context.discovery_prompt.as_deref(),
                    session_memory.as_deref(),
                    saturation_block.as_deref(),
                    &traj_steps,
                    history,
                    turn_skill_context.selected_prompt.as_deref(),
                );
                priming_status = crate::influence::ContextPrimingStatus::Fallback {
                    requested: crate::influence::ToolSelectionMethod::Influence,
                    applied: applied_method,
                    reason: reason.clone(),
                };
            }
        }

        // The directive becomes a control here, not a suggestion.
        //
        // Once the harness has told the run to ingest, the search tools stop
        // being offered. Measured 2026-08-20: 495 unique papers, 12 `papers`
        // calls, ZERO ingests, with ACTION REQUIRED in the prompt on every turn
        // from paper 40 onward. The model simply kept searching. Per Google's
        // ADK harness (arXiv 2608.17528): "Leave the tools attached and it keeps
        // calling them."
        //
        // Only the three literature-search tools go. `papers_ingest`, `recall`,
        // the graph tools and everything else remain, so the reachable moves
        // become "ingest what you have" and "answer". Nothing is refused and
        // nothing errors — the affordance is simply absent, which is the
        // difference between this and muzzling.
        if saturation.should_withhold_search() {
            let before = relevant_tools.len();
            relevant_tools = withhold_search_tools(relevant_tools);
            if relevant_tools.len() != before {
                tracing::debug!(
                    "withheld {} search tool(s): {} papers seen, 0 ingested",
                    before - relevant_tools.len(),
                    saturation.seen.len()
                );
            }
        }
        tracing::debug!(
            total_tools = tool_catalog.len(),
            selected_tools = relevant_tools.len(),
            token_budget = tool_token_budget,
            core_only = config.core_tools_only,
            primed = priming_status.is_primed(),
            "tool selection for LLM call"
        );

        // ── 2c. Build messages ────────────────────────────────────
        // Stream tokens incrementally — collect deltas from the
        // streaming callback and emit them after the call completes.
        // Reasoning tokens (is_reasoning=true) are emitted as a separate
        // event so the TUI can render them dimmed/collapsed.
        let mut streamed_deltas: Vec<(String, bool)> = Vec::new();
        let first_attempt = llm
            .chat_with_tools_streaming(
                &messages,
                &relevant_tools,
                |delta: &str, is_reasoning: bool| {
                    if !delta.is_empty() {
                        streamed_deltas.push((delta.to_string(), is_reasoning));
                    }
                },
            )
            .await;

        // OVERFLOW RECOVERY — the same contract `paper_agent` already honours,
        // which this loop did not. No context-window TABLE can be right: a
        // local server's `-c` is whatever it was started with, and a hosted
        // model's window changes under us (GLM ships 1M). The provider's own
        // "request (N tokens) exceeds the available context size (M)" is the
        // ONE authoritative statement of the limit, and it arrives exactly
        // when it matters — so treat it as an instruction to compact, not as
        // a fatal error.
        //
        // Measured 2026-08-19: a research turn died here on HTTP 400
        // (19,679 vs 16,384) after seventeen successful tool calls. Every one
        // of those results was already in hand; the run was lost to a
        // recoverable condition the harness knew how to answer.
        let response = match first_attempt {
            Ok(response) => response,
            Err(error) if prism_llm::error_is_context_window_exceeded(&error) => {
                // Exactly one recovery per turn — the retry below is inline,
                // so a second overflow propagates instead of looping on a
                // request compaction has already failed to shrink.
                tracing::warn!(
                    error = %error,
                    "context window exceeded — compacting the transcript and retrying this turn"
                );
                emit(AgentEvent::TextDelta {
                    text: "[context full — compacting and retrying]\n".to_string(),
                });
                if let Some(summary) = transcript.compact(6) {
                    compact_history(&mut messages, &summary, 6);
                }
                streamed_deltas.clear();
                llm.chat_with_tools_streaming(
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
                    tracing::error!(error = %e, "LLM call failed after compaction: {e:#}");
                    emit(AgentEvent::TextDelta {
                        text: format!("Error: {e:#}\n"),
                    });
                    e
                })
                .context("LLM call failed")?
            }
            Err(e) => {
                tracing::error!(error = %e, "LLM call failed: {e:#}");
                // Surface error details in the UI, not just "LLM call failed"
                emit(AgentEvent::TextDelta {
                    text: format!("Error: {e:#}\n"),
                });
                return Err(e.context("LLM call failed"));
            }
        };

        if let crate::influence::ContextPrimingStatus::Primed {
            exact_context_tokens,
            ..
        } = &mut priming_status
            && let Some(usage) = response.usage.as_ref()
        {
            if *exact_context_tokens != usage.prompt_tokens {
                tracing::warn!(
                    rendered_prompt_tokens = *exact_context_tokens,
                    generated_prompt_tokens = usage.prompt_tokens,
                    "local generation token count differed from the preflight render"
                );
            }
            // Generation usage is the final authority for what was really
            // prefetched; the earlier render remains the template/hash gate.
            *exact_context_tokens = usage.prompt_tokens;
        }

        // Only a successful generation proves the prompt made it through
        // grammar construction and prefill. Emitting `Primed` before this
        // point would let a failed, never-prefilled request claim priming.
        emit(AgentEvent::ContextPriming {
            iteration,
            status: priming_status,
        });

        // ── 2d. Track usage ───────────────────────────────────────
        if let Some(usage) = &response.usage {
            let billed_usage = UsageInfo {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            };
            transcript.record_cost("llm_turn", usage.prompt_tokens, usage.completion_tokens);
            run_metrics.record_usage(&billed_usage, &billing_model);
            total_usage += billed_usage;
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
                    let db_path = crate::hooks::provenance_db_path();
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
                // No tool calls → turn complete.

                // ── Execution-contract gate ───────────────────────
                // Deterministic, no-LLM: an answer claiming an action that no
                // tool of the matching class performed this turn has no
                // evidence behind it. Reject the finalization and hand the
                // model the fork (do it, or stop claiming it). See
                // `execution_contract`.
                //
                // `iteration + 1 < max_iterations` is load-bearing: `continue`
                // on the LAST iteration would fall through to the
                // max-iterations arm, which emits `TurnComplete { text: None }`
                // — the user would lose the answer entirely. Never trade a
                // fabricated answer for no answer; on the last iteration the
                // claim ships and the prompt is the only line of defence.
                if contract_gate_firings < MAX_CONTRACT_GATE_FIRINGS
                    && iteration + 1 < config.max_iterations
                    && let Some(claim) = crate::execution_contract::unsupported_execution_claim(
                        response.message.content.as_deref().unwrap_or(""),
                        &tools_used_this_turn,
                    )
                {
                    contract_gate_firings += 1;
                    tracing::info!(
                        claim = %claim,
                        firing = contract_gate_firings,
                        "execution-contract gate: rejected unsupported execution claim"
                    );
                    // The rejected text has ALREADY streamed to the user (2e
                    // runs before this check). Without this marker the retry
                    // would be appended straight onto the rejected text as one
                    // self-contradicting message.
                    emit(AgentEvent::TextDelta {
                        text: format!(
                            "\n\n[unverified claim \"{claim}…\" — no matching tool ran this turn; re-checking]\n\n"
                        ),
                    });
                    history.push(ChatMessage {
                        role: "system".to_string(),
                        content: Some(
                            crate::execution_contract::UNSUPPORTED_CLAIM_REMINDER.to_string(),
                        ),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    continue;
                }

                // ── Capability-gap re-retrieval ───────────────────
                // The model ended the turn saying it lacked a capability. Do
                // NOT tell it to call find_tools — an instruction it can ignore
                // for free. Retrieve on its own words HERE and pin what comes
                // back, so the next request carries those definitions whether
                // or not the model would have gone looking. Bounded: once per
                // turn, and only when retrieval actually found something.
                if !capability_gap_retried
                    && iteration + 1 < config.max_iterations
                    && let Some(gap) = crate::tool_catalog::capability_gap_query(
                        response.message.content.as_deref().unwrap_or(""),
                    )
                {
                    let found: Vec<String> = tool_catalog
                        .search(&gap, CAPABILITY_GAP_RETRIEVE)
                        .into_iter()
                        .map(|tool| tool.name.clone())
                        .collect();
                    // Bounded by the same tool budget as find_tools' pins, and
                    // only what actually fits is announced: claiming a tool is
                    // retrieved when its definition never reaches the request is
                    // the silent-truncation failure this budget exists to stop.
                    let rejected = pin_within_budget(
                        tool_catalog,
                        &mut pinned_tools,
                        found.clone(),
                        tool_token_budget,
                    );
                    let admitted: Vec<String> = found
                        .into_iter()
                        .filter(|name| !rejected.contains(name))
                        .collect();
                    // Nothing to offer ⇒ nothing to retry. The model's answer
                    // stands and the turn ends normally.
                    if !admitted.is_empty() {
                        capability_gap_retried = true;
                        let names = admitted.join(", ");
                        tracing::info!(gap = %gap, tools = %names, "capability-gap re-retrieval");
                        emit(AgentEvent::TextDelta {
                            text: format!("\n\n[retrieved for \"{gap}\": {names}]\n\n"),
                        });
                        history.push(ChatMessage {
                            role: "system".to_string(),
                            content: Some(format!("{CAPABILITY_GAP_NOTE}{names}")),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                        continue;
                    }
                }

                // Harness scaffolding for ONE finalization, not conversation.
                // `history` outlives the turn on the TUI path
                // (`ServerRuntime::history`), so leaving these in would resend a
                // stale scolding / a stale retrieval note on every later turn.
                if contract_gate_firings > 0 {
                    history.retain(|m| {
                        m.content.as_deref()
                            != Some(crate::execution_contract::UNSUPPORTED_CLAIM_REMINDER)
                    });
                }
                if capability_gap_retried {
                    history.retain(|m| {
                        !m.content
                            .as_deref()
                            .is_some_and(|c| c.starts_with(CAPABILITY_GAP_NOTE))
                    });
                }
                // The pre-flight routing hint is stripped at the START of every
                // turn instead (1b) — unconditionally, so no exit path can leak
                // it. Nothing to do here.

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
                let estimated_cost = run_metrics.cost_usd;

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
        for tool_call in &tool_calls {
            let tool_name = &tool_call.function.name;
            let call_id = &tool_call.id;

            let args: Value =
                serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();
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

            // Human Markdown is not an execution bypass. Even a generic
            // read/bash call that reaches into a skill package must respect
            // its declared implicit-invocation policy before OPA, approval,
            // or an executor sees the call. Explicit selection is recorded in
            // task-local turn context by the deterministic `$name` resolver,
            // never by a model-supplied flag.
            if let Err(error) = crate::skills::gate_implicit_human_skill_invocation(
                tool_name,
                &args,
                &command_tool_runtime.project_root,
                &skill_surface_policy,
            ) {
                let error_msg = format!("Skill invocation blocked: {error}");
                emit(AgentEvent::ToolCallResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    content: error_msg.clone(),
                    summary: Some(format!("{tool_name}: blocked by skill policy")),
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
            //
            // A MISSING ENGINE DENIES. `policy` is `None` only when
            // `PolicyEngine::with_discovery` returned `Err` — i.e. a `.rego`
            // file failed to load (service.rs: "OPA policy engine failed to
            // load — running without policies"). It is NOT the
            // no-policies-configured case, which returns `Ok` with a count of
            // zero.
            //
            // This used to be a bare `if let Some(..)`, so one malformed policy
            // file silently disabled ALL tool policy enforcement in the agent
            // loop while `mcp_server_native.rs` refused on the same condition.
            // A policy layer that turns itself off when its rules will not
            // parse is worse than none, because the operator believes it is on.
            let Some(pe) = policy.as_mut() else {
                let denied_msg = format!(
                    "Tool '{tool_name}' refused: the OPA policy engine failed to \
                     initialize and policy cannot be bypassed (fail-closed). \
                     Check ~/.prism/policies and .prism/policies for invalid \
                     .rego files."
                );
                emit(AgentEvent::ToolCallResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    content: denied_msg.clone(),
                    summary: Some(format!("{tool_name}: policy engine unavailable")),
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
            };
            {
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
            if approval_gate_outcome(
                config,
                &permission_decision,
                tool_catalog,
                tool_name,
                &args,
                call_id,
                &preview,
                approval_rx.as_ref(),
                live_permission_overrides.as_ref(),
                history,
                emit,
            )
            .await
                == ApprovalGateOutcome::Denied
            {
                continue;
            }

            // ── h5. Execute tool ──────────────────────────────────
            // Evidence for the execution-contract gate is recorded HERE, not
            // where the model requested the call: h2-h5 above all `continue`
            // on hook abort / permission block / policy deny / approval deny,
            // and a call that never ran is not evidence of anything. Workspace
            // mutations are deferred until the executor confirms success: an
            // ambiguous/refused patch is not evidence that a file was edited.
            let evidence_requires_success = tool_evidence_requires_success(tool_name);
            if !evidence_requires_success {
                tools_used_this_turn.push(tool_name.clone());
            }
            let start = Instant::now();
            let mut result: Result<Value> =
                if let Some(meta_tool) = crate::meta_tools::MetaTool::from_name(tool_name) {
                    // Native meta-tools, classified by EFFECT (`MetaTool::effect` —
                    // an exhaustive, wildcard-free match). Read-only state access
                    // (recall / find_tools / list_skills / list_failures) operates
                    // on the agent's own state — durable memory + the tool catalog
                    // — and is intercepted here before command-tool / Python
                    // dispatch. The MUTATING/EXECUTING members (apply_patch
                    // changes project source; write_skill / run_skill run
                    // un-sandboxed shell/Python as the node OS user;
                    // spawn_subagent drives a nested turn over the same tool
                    // surface) pass the same platform-access gate as every other
                    // execution surface — resolved inside their executors so no
                    // dispatch path can skip it. The old blanket `is_meta_tool`
                    // interception carried them ahead of every gate call site.
                    if meta_tool == crate::meta_tools::MetaTool::SpawnSubagent {
                        // spawn_subagent needs the LIVE turn machinery (LLM
                        // client, tool server, approval channel, policy engine)
                        // that execute_meta_tool cannot carry — dispatched here,
                        // access-gated inside execute_spawn_subagent. It runs one
                        // nested run_turn under the parent's permission/approval
                        // gating, inheriting (never widening) the caller's
                        // platform access.
                        //
                        // The subagent's spend counts against the PARENT's cumulative
                        // budget too — delegation must not be a budget escape hatch.
                        // Charge from the INCREMENTAL metrics, unconditionally.
                        //
                        // The identical defect was found and fixed in the
                        // orchestrator copy (see "Two bugs in one line" in
                        // orchestrator.rs) and left live here. This site used to
                        // gate the charge on `if let Ok(value)` and read the spend
                        // back out of the RESULT JSON, which the subagent only
                        // populated from `TurnComplete` — an event that never fires
                        // when a turn errors. A delegated turn that made four billed
                        // calls and died on the fifth therefore contributed ZERO to
                        // the parent's budget: the escape hatch this very comment
                        // promised did not exist. `AgentRunMetrics` accrues per LLM
                        // call, so it holds the real spend whether the nested turn
                        // finished or died — and it is the same accumulator the
                        // child's own ledger row is closed with, so the report and
                        // the charge cannot drift apart.
                        let mut sub_metrics = AgentRunMetrics::default();
                        let sub_result = crate::subagent::execute_spawn_subagent(
                            llm,
                            tool_server,
                            command_tool_runtime,
                            tool_catalog,
                            config,
                            current_run_id,
                            current_session_id,
                            &args,
                            hooks,
                            permissions,
                            live_permission_overrides.clone(),
                            emit,
                            approval_rx.clone(),
                            policy.as_deref_mut(),
                            subagent_lanes,
                            &mut sub_metrics,
                        )
                        .await;
                        transcript.record_cost(
                            "subagent",
                            sub_metrics.tokens_in,
                            sub_metrics.tokens_out,
                        );
                        sub_result.map(|value| serde_json::json!({ "result": value }))
                    } else if meta_tool == crate::meta_tools::MetaTool::OrchestrateAgents {
                        // orchestrate_agents is spawn_subagent's fan-out sibling:
                        // N nested turns, concurrently, over the lane pool. Same
                        // dispatch shape (needs the live turn machinery), same
                        // access gate inside the executor. It receives whether an
                        // approval CHANNEL exists — not the channel itself —
                        // because concurrent items cannot share one uncorrelated
                        // Allow/Deny stream; see orchestrator.rs "Approval shape".
                        //
                        // The whole fan-out's spend counts against the PARENT's
                        // budget — orchestration must not be a budget escape hatch
                        // either. Same accumulator, same unconditional charge as the
                        // spawn_subagent arm above: one idiom, so a future reader
                        // cannot fix one delegation path and miss the other (which
                        // is exactly how the subagent hole survived).
                        let mut orch_metrics = AgentRunMetrics::default();
                        let orch_result = crate::orchestrator::execute_orchestrate_agents(
                            llm,
                            command_tool_runtime,
                            tool_catalog,
                            config,
                            current_run_id,
                            current_session_id,
                            &args,
                            permissions,
                            live_permission_overrides.clone(),
                            emit,
                            approval_rx.is_some(),
                            policy.as_deref(),
                            subagent_lanes,
                            &mut orch_metrics,
                        )
                        .await;
                        transcript.record_cost(
                            "orchestrate_agents",
                            orch_metrics.tokens_in,
                            orch_metrics.tokens_out,
                        );
                        orch_result.map(|value| serde_json::json!({ "result": value }))
                    } else {
                        // Open the same Turso store the provenance hook writes to.
                        let db_path = crate::hooks::provenance_db_path();
                        let store = prism_provenance::ProvenanceStore::open(&db_path).await.ok();
                        // Real session id supplies `recall`'s default scope
                        // instead of the pre-fix literal "session" bucket.
                        let session_id = crate::hooks::provenance_session_id();
                        crate::meta_tools::execute_meta_tool_with_project_root(
                            tool_name,
                            &args,
                            store.as_ref(),
                            &session_id,
                            tool_catalog,
                            Some(&command_tool_runtime.project_root),
                            // The real turn budget. `recall` is the only
                            // meta-tool that can pull an arbitrarily large
                            // payload back into the conversation, so it sizes
                            // itself against what the turn actually has left.
                            Some(crate::meta_tools::TurnRemaining::new(
                                transcript.cost.total_input,
                                transcript.budget.max_input_tokens,
                            )),
                        )
                        .await
                        .map(|value| serde_json::json!({ "result": value }))
                    }
                } else if command_tools::is_command_tool(tool_name) {
                    command_tools::execute_command_tool(
                        command_tool_runtime,
                        tool_name,
                        &args,
                        policy.as_deref_mut(),
                    )
                    .await
                    .map(|value| serde_json::json!({ "result": value }))
                } else if tool_catalog
                    .find(tool_name)
                    .is_some_and(|tool| tool.source.as_deref() == Some("mcp"))
                {
                    // External MCP tool (namespaced `mcp__<server>__<tool>`,
                    // admitted via extend_untrusted): route to the connected MCP
                    // server session instead of the Python tool server. The
                    // approval gate above already ran — MCP tools are untrusted,
                    // always requires_approval, never in the auto-approve set.
                    crate::mcp::call_global_tool(tool_name, &args).await
                } else {
                    // Purpose-built Python tools remain available to LocalOnly
                    // callers, but arbitrary execute_python/execute_bash dispatch
                    // must prove node ownership before signaling the worker.
                    match command_tools::gate_external_tool_execution(
                        tool_name,
                        command_tools::current_platform_access(),
                    ) {
                        Ok(_) => tool_server
                            .call_tool(tool_name, args.clone())
                            .await
                            .map_err(Into::into),
                        Err(error) => Err(error),
                    }
                };

            if evidence_requires_success && result.is_ok() {
                tools_used_this_turn.push(tool_name.clone());
            }

            // Auto-pin tools surfaced by find_tools so their full definitions
            // become callable next iteration (the "now available" hint used to
            // be false — names came back but were never wired into the request).
            // Bounded by the turn's tool budget: a pinned definition rides in
            // EVERY later request, so an unbounded pin set turns the budget into
            // decoration. What does not fit is reported back in this very tool
            // result — the model must never be told a tool is "now available"
            // when it is not.
            if tool_name == "find_tools"
                && let Ok(v) = &mut result
            {
                let candidates: Vec<String> = v
                    .get("result")
                    .and_then(|r| r.get("matches"))
                    .and_then(|m| m.as_array())
                    .map(|matches| {
                        matches
                            .iter()
                            .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                            .map(ToOwned::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let rejected = pin_within_budget(
                    tool_catalog,
                    &mut pinned_tools,
                    candidates,
                    tool_token_budget,
                );
                if !rejected.is_empty()
                    && let Some(obj) = v.get_mut("result").and_then(Value::as_object_mut)
                {
                    tracing::info!(
                        dropped = rejected.len(),
                        budget = tool_token_budget,
                        "find_tools: pins exceeded the tool budget"
                    );
                    obj.insert("not_available".to_string(), serde_json::json!(rejected));
                    obj.insert(
                        "hint".to_string(),
                        serde_json::json!(format!(
                            "these tools are now available — call one by name to use it. \
                             {} did NOT fit this model's tool-definition budget and are NOT \
                             callable: {}. Narrow the query (or pass a smaller `limit`) and \
                             call find_tools again if you need one of them.",
                            rejected.len(),
                            rejected.join(", ")
                        )),
                    );
                }
            }
            let elapsed_ms = start.elapsed().as_millis() as u64;

            let (raw_content, is_error): (String, bool) = match result {
                Ok(resp) => {
                    // is_error is derived via the shared tool_result helper so
                    // this gate and the protocol.rs / provenance-hook gates can
                    // never drift. The OLD gate only matched a top-level
                    // "error" key, but tool_server.py wraps every Python-tool
                    // payload under "result" — so a failed run (success:false)
                    // arrived as is_error=false and was rendered as a green
                    // "results" card. See crates/agent/src/tool_result.rs.
                    let is_error = crate::tool_result::tool_result_is_error(&resp);
                    let raw_content = if let Some(r) = resp.get("result") {
                        serde_json::to_string(r).unwrap_or_default()
                    } else {
                        serde_json::to_string(&resp).unwrap_or_default()
                    };
                    (raw_content, is_error)
                }
                Err(e) => (format!("Tool error: {e}"), true),
            };

            // ── h6. Fire post-hooks ───────────────────────────────
            // VS1 fix-round #2: the hard dispatch-Err arm sets raw_content to a
            // plain "Tool error: ..." string (not JSON). Re-parsing it here fell
            // through to a bare Value::String, which the provenance classifier
            // reads as status:ok (its `.as_object()` is None) — provenance LIED
            // that a failed tool call succeeded. hook_result_value wraps a
            // non-JSON, is_error content as {success:false,error:...} so the
            // shared classifier records status:error. Model-facing raw_content
            // is unchanged (the provenance hook never mutates the value).
            let result_value: Value = hook_result_value(&raw_content, is_error);
            // Counted from the same bytes h6 persists, so the rendered coverage
            // and the durable record can never disagree about what happened.
            saturation.observe(tool_name, &args, &result_value, is_error);
            // Identity into the graph, from the same bytes, before the payload
            // is compacted out of the conversation.
            if !is_error {
                persist_paper_identities(tool_name, &result_value).await;
            }
            let last_search_fresh = saturation.searches.last().map_or(0, |call| call.fresh);
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
                     Try a materially different approach, or stop and report plainly what you \
                     could not do and why.",
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
                         This tool isn't finding what you need — try a different tool \
                         or rephrase the query. If nothing finds it, report that it \
                         was not found and say which attempts you made. Do NOT fill \
                         the gap from memory.",
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

            // ── h7c. Bounded verify-by-execution (VS2-P1b) ─────────
            // A code-exec tool that fails N>=CODE_REPAIR_MAX times in a row is
            // spiraling: self-repair beyond ~2 attempts rarely fixes root cause.
            // Push the REAL last error (so the model has the honest failure)
            // THEN a directive to stop editing and report honestly. Do NOT
            // swallow the real result, and do NOT ask the model to narrate the
            // trace. Resets on any successful code-exec call, mirroring h7b.
            //
            // FIX-5: normalize the tool name (notebook_run/run_python_notebook/
            // notebook -> notebook_exec) so alias-invoked cells count toward
            // the cap and share the streak with the canonical name.
            let canonical_tool = command_tools::canonical_code_exec_tool(tool_name.as_str());
            if CODE_EXEC_TOOLS.contains(&canonical_tool) {
                if is_error {
                    let streak = code_failure_streak
                        .entry(canonical_tool.to_string())
                        .or_insert(0);
                    *streak += 1;
                    if let Some(directive) = code_repair_directive(canonical_tool, *streak) {
                        // h8/h12 haven't run yet (we're before them), so push
                        // the real filtered error ourselves first — never swallow it.
                        let real_content = process_large_result(&content_after_hooks);
                        let real_summary = summarize_tool_result(
                            canonical_tool,
                            preview.as_deref(),
                            &real_content,
                            true,
                        );
                        emit(AgentEvent::ToolCallResult {
                            call_id: call_id.clone(),
                            tool_name: canonical_tool.to_string(),
                            content: real_content.clone(),
                            summary: Some(real_summary.clone()),
                            preview: preview.clone(),
                            elapsed_ms,
                            is_error: true,
                        });
                        traj_steps.push(real_summary);
                        // FIX-4: emit the directive as a SEPARATE TUI stream
                        // event (so the human pane sees result-then-directive),
                        // but push ONE merged tool message to history. Two
                        // role:"tool" messages with the same tool_call_id is a
                        // protocol violation for strict OpenAI-compat backends.
                        emit(AgentEvent::ToolCallResult {
                            call_id: call_id.clone(),
                            tool_name: canonical_tool.to_string(),
                            content: directive.clone(),
                            summary: Some(format!("{canonical_tool}: repair cap reached")),
                            preview: preview.clone(),
                            elapsed_ms,
                            is_error: true,
                        });
                        let merged_content = format!(
                            "{real_content}\n\n---\n{directive}\n\n[repair cap reached: {canonical_tool} \
                             failed {streak} consecutive times. See the real error above; do NOT retry the \
                             same approach.]"
                        );
                        history.push(ChatMessage {
                            role: "tool".to_string(),
                            content: Some(merged_content),
                            tool_calls: None,
                            tool_call_id: Some(call_id.clone()),
                        });
                        continue;
                    }
                } else {
                    // Success: reset this tool's failure streak.
                    code_failure_streak.remove(canonical_tool);
                }
            }

            // ── h8. Large-result handling ─────────────────────────
            // A counted search collapses to its digest FIRST. h6 has already
            // persisted the full payload, so nothing is lost — and the twenty
            // abstracts stop being re-sent on every later request, which is what
            // ended the last two runs of this exact question on their budget.
            let content = match search_digest(tool_name, &result_value, last_search_fresh) {
                Some(digest) => digest,
                None => process_large_result(&content_after_hooks),
            };

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

        // ── 2h. Compact BEFORE looping back ───────────────────────
        //
        // The other `should_compact` call lives in the "no tool calls -> turn
        // complete" arm, so a long research turn never compacted at all: it
        // grew until the cumulative-input budget killed it, then compacted on
        // the way out, which helps nobody. Measured twice on the same PFAS
        // question — 207,689 tokens over 4 rounds, then 240,967 over 27.
        //
        // Cumulative input is the guard that actually binds here, so compaction
        // triggers on token pressure as well as turn count. The last six
        // messages always survive, so the model keeps the thread it is
        // currently pulling; everything older becomes a summary, and the full
        // text of every tool call remains in the provenance store behind
        // `recall`.
        // Reclaim the free context FIRST. Compaction costs an LLM call and
        // rewrites history; zeroing stale tool bodies costs nothing and often
        // makes the call unnecessary. Pattern lifted from Google's ADK
        // long-horizon harness (`horizon/context/tool_output_pruning.py`,
        // Apache-2.0), adapted: PRISM stores every tool result durably, so a
        // pruned body is genuinely recoverable via `recall` rather than only
        // re-runnable.
        let pruned = prune_stale_tool_results(history);
        if pruned.pruned > 0 {
            tracing::debug!(
                "pruned {} stale tool result(s), ~{} tokens reclaimed",
                pruned.pruned,
                pruned.reclaimed_tokens
            );
        }
        if transcript.needs_compaction_under_pressure()
            && let Some(summary) = transcript.compact(6)
        {
            tracing::debug!("compacting mid-turn under token pressure");
            compact_history(history, &summary, 6);
        }

        // ── 2i. Loop back ─────────────────────────────────────────
    }

    // ── 3. Max iterations reached ─────────────────────────────────
    emit(AgentEvent::TextDelta {
        text: "\n\n[Agent reached maximum iterations]".to_string(),
    });

    let estimated_cost = run_metrics.cost_usd;

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

    #[tokio::test]
    async fn active_run_heartbeat_refreshes_until_stopped() {
        let store = Arc::new(
            prism_provenance::ProvenanceStore::open(std::path::Path::new(":memory:"))
                .await
                .unwrap(),
        );
        let run = prism_provenance::new_agent_run(
            "heartbeat-session",
            "agent",
            "waiting on a provider",
            None,
        );
        store.start_agent_run(&run).await.unwrap();
        let heartbeat = AgentRunHeartbeat::start_with_policy(
            Some(store.clone()),
            run.id.clone(),
            AgentRunHeartbeatPolicy {
                interval: Duration::from_millis(5),
            },
        );

        tokio::time::sleep(Duration::from_millis(30)).await;
        heartbeat.stop().await;
        let rows = store
            .list_agent_runs(&prism_provenance::AgentRunFilter {
                session_id: Some(run.session_id.clone()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].updated_at, run.updated_at);
    }

    fn tool_json(name: &str, desc: &str) -> serde_json::Value {
        serde_json::json!({ "name": name, "description": desc, "input_schema": { "type": "object" } })
    }

    /// Token cost of the always-on meta-tools — the mandatory floor every
    /// request pays before a single catalog tool is selected.
    fn meta_tool_tokens() -> usize {
        crate::meta_tools::definitions()
            .iter()
            .map(|t| crate::tool_catalog::definition_tokens(&t.to_definition()))
            .sum()
    }

    // ── VS1 fix-round #2: provenance must not read a hard-Err as status:ok ──

    #[test]
    fn hook_result_value_passes_through_valid_json() {
        // The Ok(resp) arm serializes a JSON payload; it must survive verbatim.
        let v = hook_result_value(r#"{"success":false,"stderr":"boom"}"#, true);
        assert_eq!(v, serde_json::json!({ "success": false, "stderr": "boom" }));
    }

    #[test]
    fn hook_result_value_wraps_non_json_error_so_provenance_records_error() {
        // THE REGRESSION GUARD: the dispatch-Err arm hands us "Tool error: ..."
        // (not JSON). A bare Value::String is classified status:ok by the shared
        // helper -> provenance would LIE. The wrap must make it read as error.
        let v = hook_result_value("Tool error: connection refused", true);
        assert!(
            crate::tool_result::tool_result_is_error(&v),
            "a hard tool Err must classify as an error for provenance, got {v}"
        );
        assert_eq!(
            v["error"],
            serde_json::json!("Tool error: connection refused")
        );
    }

    #[test]
    fn hook_result_value_leaves_non_json_non_error_as_bare_string() {
        // Defensive: a non-JSON, non-error content keeps the old shape and must
        // NOT be forced into an error (no over-flagging).
        let v = hook_result_value("plain text result", false);
        assert_eq!(v, Value::String("plain text result".to_string()));
        assert!(!crate::tool_result::tool_result_is_error(&v));
    }

    #[test]
    fn workspace_patch_is_edit_evidence_only_after_success() {
        assert!(tool_evidence_requires_success("apply_patch"));
        assert!(!tool_evidence_requires_success("execute_bash"));
        assert!(!tool_evidence_requires_success("recall"));
    }

    #[tokio::test]
    async fn denied_apply_patch_stops_before_dispatch_and_preserves_target_bytes() {
        use crate::command_tools::{CommandToolPlatformAccess, with_platform_access};

        let project = tempfile::tempdir().expect("temp project");
        let target = project.path().join("denied.txt");
        let original = b"before\n".to_vec();
        std::fs::write(&target, &original).expect("write target");
        let runtime = CommandToolRuntime {
            project_root: project.path().to_path_buf(),
            ..Default::default()
        };
        let args = serde_json::json!({
            "patch": "*** Begin Patch\n*** Update File: denied.txt\n@@\n-before\n+after\n*** End Patch"
        });

        // Use the authoritative built-in definition and the same permission
        // decision the turn loop computes immediately before this seam.
        let mut catalog = ToolCatalog::default();
        catalog.extend(crate::meta_tools::definitions());
        let permissions = ToolPermissionContext::default();
        let permission_decision = permissions.decision_for("apply_patch", None);
        assert!(!permission_decision.blocked);
        assert!(!permission_decision.auto_approved);

        let config = AgentConfig {
            auto_approve: false,
            ..Default::default()
        };
        let (approval_tx, approval_rx) = tokio::sync::mpsc::channel(1);
        approval_tx
            .send(ApprovalResponse::Deny)
            .await
            .expect("queue denial");
        let approval_rx = Arc::new(tokio::sync::Mutex::new(approval_rx));
        let mut history = Vec::new();
        let mut events = Vec::new();
        let outcome = {
            let mut emit = |event| events.push(event);
            approval_gate_outcome(
                &config,
                &permission_decision,
                &catalog,
                "apply_patch",
                &args,
                "denied-apply-patch",
                &Some("apply project patch".to_string()),
                Some(&approval_rx),
                None,
                &mut history,
                &mut emit,
            )
            .await
        };

        // This is the production ordering: only Proceed may reach the real
        // dispatcher, and it receives the runtime's authoritative project
        // root. If the seam ever permits Deny, this valid patch mutates the
        // target and the byte-identity assertion below catches it.
        let mut dispatched = false;
        if outcome == ApprovalGateOutcome::Proceed {
            dispatched = true;
            with_platform_access(
                CommandToolPlatformAccess::VerifiedNodeOwner,
                crate::meta_tools::execute_meta_tool_with_project_root(
                    "apply_patch",
                    &args,
                    None,
                    "approval-denial-test",
                    &catalog,
                    Some(&runtime.project_root),
                    None,
                ),
            )
            .await
            .expect("a mistakenly approved patch should reach the real dispatcher");
        }

        assert_eq!(outcome, ApprovalGateOutcome::Denied);
        assert!(!dispatched, "denied patch must stop before dispatch");
        assert_eq!(
            std::fs::read(&target).expect("read target after denial"),
            original,
            "denial must leave the target byte-identical"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolApprovalRequest {
                tool_name,
                tool_args,
                requires_approval: true,
                permission_mode,
                ..
            } if tool_name == "apply_patch"
                && tool_args == &args
                && permission_mode == "workspace-write"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallResult {
                tool_name,
                is_error: true,
                content,
                ..
            } if tool_name == "apply_patch" && content.contains("denied by user")
        )));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "tool");
        assert_eq!(
            history[0].tool_call_id.as_deref(),
            Some("denied-apply-patch")
        );
        assert!(
            history[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("denied by user"))
        );
    }

    // ── VS2-P1b: code_repair_directive ─────────────────────────────────

    #[test]
    fn p1b_code_repair_directive_none_below_cap() {
        assert_eq!(code_repair_directive("execute_python", 0), None);
        assert_eq!(code_repair_directive("execute_python", 1), None);
        assert_eq!(code_repair_directive("execute_python", 2), None);
    }

    #[test]
    fn p1b_code_repair_directive_some_at_cap_for_code_tools() {
        for tool in CODE_EXEC_TOOLS {
            let msg =
                code_repair_directive(tool, CODE_REPAIR_MAX).expect("cap reached for code tool");
            assert!(
                msg.contains(&format!(
                    "{tool} failed {CODE_REPAIR_MAX} consecutive times"
                )),
                "directive names the tool + streak: {msg}"
            );
            assert!(
                msg.to_lowercase().contains("stop"),
                "directive must tell the model to stop retrying: {msg}"
            );
            assert!(
                msg.to_lowercase().contains("do not narrate"),
                "directive must explicitly tell the model not to narrate its trace: {msg}"
            );
        }
        // Above the cap still fires.
        assert!(code_repair_directive("execute_bash", 5).is_some());
    }

    #[test]
    fn p1b_code_repair_directive_none_for_non_code_tools() {
        // A non-code tool failing repeatedly is NOT a verify-by-execution spiral
        // — don't gate it with the repair directive.
        assert_eq!(code_repair_directive("read_file", 3), None);
        assert_eq!(code_repair_directive("search", 10), None);
    }

    #[test]
    fn p1b_code_exec_tools_canonical() {
        // FIX-5: the constant holds the 3 canonical names...
        assert_eq!(
            CODE_EXEC_TOOLS,
            &["execute_python", "execute_bash", "notebook_exec"]
        );
        // ...and canonical_code_exec_tool resolves aliases/root/case variants
        // to those canonical names so alias-invoked cells count toward the cap.
        use crate::command_tools::canonical_code_exec_tool as canon;
        assert_eq!(canon("notebook_run"), "notebook_exec");
        assert_eq!(canon("run_python_notebook"), "notebook_exec");
        assert_eq!(canon("NOTEBOOK_EXEC"), "notebook_exec");
        assert_eq!(canon("notebook"), "notebook_exec"); // root
        assert_eq!(canon("Notebook_Run"), "notebook_exec"); // case-insensitive
        // Canonical names pass through.
        assert_eq!(canon("execute_python"), "execute_python");
        assert_eq!(canon("execute_bash"), "execute_bash");
        assert_eq!(canon("notebook_exec"), "notebook_exec");
        // Non-code tools are unchanged.
        assert_eq!(canon("search"), "search");
        assert_eq!(canon("read_file"), "read_file");
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
        // The query matches nothing, so selection would never offer this tool;
        // being pinned is the only reason it is callable. The budget is the real
        // one for the smallest model PRISM supports — a pin outranks selection,
        // but it is not exempt from the budget (see
        // `one_unbounded_find_tools_call_cannot_blow_the_tool_budget`).
        let budget = crate::tool_catalog::MIN_TOOL_TOKENS;
        let defs = assemble_request_tools(&catalog, "hello there friend", &pinned, budget);
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

    // ── Token budget: reach, truncation, cost ─────────────────────

    /// A catalog shaped like the real one: 54 tools whose definitions are the
    /// same order of magnitude as production (the live 54 Python tools average
    /// 1,325 JSON bytes each).
    fn catalog_of_54() -> crate::tool_catalog::ToolCatalog {
        let filler = "x".repeat(900);
        let tools: Vec<serde_json::Value> = (0..54)
            .map(|i| {
                serde_json::json!({
                    "name": format!("tool_{i:02}"),
                    "description": format!("capability number {i}: {filler}"),
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "target": { "type": "string", "description": "what to act on" },
                            "mode":   { "type": "string", "description": "how to act" }
                        },
                        "required": ["target"]
                    }
                })
            })
            .collect();
        crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": tools
        }))
    }

    /// TWO measured failures, and the lever that answers both.
    ///
    /// A fixed cap of 15 of 54 tools made 39 capabilities INVISIBLE, and
    /// "call find_tools if you need something else" is an instruction models
    /// reliably ignore. Removing the cap fixed that — and created the opposite
    /// failure: the tool block is re-sent on EVERY request, so on 2026-08-19 a
    /// literature review on glm-5.2 spent its whole 200,000-token budget
    /// re-sending 171 definitions (12,289 x 17 = 208,913 against 207,689
    /// observed) and stopped at round 4 with nothing left to answer with.
    ///
    /// Both are real, so the fix separates what each one costs. DEFINITIONS —
    /// full JSON schemas, the expensive part — are capped by count, because
    /// their cost is paid once per turn. VISIBILITY is not capped: everything
    /// withheld is listed in the L1 capability menu as name + one line and
    /// pulled back by `find_tools`. Nothing becomes invisible; the schemas just
    /// stop being re-sent 171 at a time.
    #[test]
    fn schemas_are_capped_by_count_while_every_tool_stays_visible() {
        let catalog = catalog_of_54();
        let pinned = std::collections::HashSet::new();
        let route = "help me with something";

        // A large-context model: the token budget alone would afford ALL 54,
        // which is exactly why the count cap has to be the binding constraint.
        let budget = crate::tool_catalog::tool_token_budget(131_072);
        let defs = assemble_request_tools(&catalog, route, &pinned, budget);
        let names: std::collections::HashSet<&str> =
            defs.iter().map(|d| d.function.name.as_str()).collect();

        let catalog_defs = defs
            .iter()
            .filter(|d| !crate::meta_tools::is_meta_tool(&d.function.name))
            .count();
        assert!(
            catalog_defs <= crate::tool_catalog::MAX_REQUEST_TOOLS,
            "{catalog_defs} catalog schemas shipped, cap is {}",
            crate::tool_catalog::MAX_REQUEST_TOOLS
        );
        assert!(
            catalog_defs < 54,
            "the cap must actually bind on a 54-tool catalog"
        );
        assert!(names.contains("find_tools"), "escape hatch stays offered");

        // VISIBILITY: every tool that did not get a schema is named in the menu.
        let entries: Vec<(String, String)> = catalog
            .iter()
            .map(|t| (t.name.clone(), format!("{}: {}", t.name, t.description)))
            .collect();
        let included: std::collections::HashSet<String> =
            names.iter().map(|n| (*n).to_string()).collect();
        let menu = crate::capability::capability_menu(&entries, &included, 150, 80)
            .expect("withheld tools must produce a menu");
        for i in 0..54 {
            let want = format!("tool_{i:02}");
            assert!(
                names.contains(want.as_str()) || menu.contains(&want),
                "{want} is neither callable nor listed — that is the invisibility bug"
            );
        }

        // And the point of the whole exercise: the per-request cost collapses.
        let spent: usize = defs
            .iter()
            .map(crate::tool_catalog::definition_tokens)
            .sum();
        let whole_catalog: usize = catalog
            .iter()
            .map(|t| crate::tool_catalog::definition_tokens(&t.to_definition()))
            .sum();
        assert!(
            spent < whole_catalog,
            "capped request ({spent}) must cost less than shipping everything ({whole_catalog})"
        );
        assert!(
            spent <= budget,
            "cost {spent} must respect the budget {budget}"
        );
        println!(
            "{catalog_defs} schemas + meta = {spent} tokens; whole catalog would be {whole_catalog}"
        );
    }

    /// The budget is a real bound, not decoration: a small-context model gets
    /// fewer tools, and the ones it keeps are the RELEVANT ones.
    #[test]
    fn small_context_model_truncates_by_relevance_within_budget() {
        let catalog = catalog_of_54();
        let pinned = std::collections::HashSet::new();
        // 16k context → 4,096 tokens. Meta-tools alone charge ~984, so only a
        // handful of catalog tools can follow.
        let budget = crate::tool_catalog::tool_token_budget(16_384);
        assert_eq!(budget, 4_096);

        let defs = assemble_request_tools(&catalog, "capability number 7", &pinned, budget);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        let spent: usize = defs
            .iter()
            .map(crate::tool_catalog::definition_tokens)
            .sum();

        assert!(
            spent <= budget,
            "budget blown: {spent} > {budget} ({names:?})"
        );
        assert!(
            defs.len() < 54,
            "a 16k model cannot afford the whole catalog: {} offered",
            defs.len()
        );
        assert!(
            names.contains(&"tool_07"),
            "truncation must keep the RELEVANT tool: {names:?}"
        );
        assert!(
            names.contains(&"find_tools"),
            "and the escape hatch, so the rest stays reachable: {names:?}"
        );
    }

    /// Priority order under pressure: meta-tools are unconditional, a pinned
    /// tool outranks every SELECTED tool, and selection is what gets squeezed.
    ///
    /// This used to assert the pinned tool survived a **1-token** budget, i.e.
    /// that pins were exempt from the budget entirely. That exemption was the
    /// defect — with a model-controlled `find_tools(limit)` it let one call
    /// spend 18x the budget. A pin now outranks selection but is still charged;
    /// what it must never do is push the request over the bound, and what must
    /// never be evicted is the escape hatch.
    #[test]
    fn budget_never_evicts_meta_and_pins_outrank_selection() {
        let catalog = catalog_of_54();
        let mut pinned = std::collections::HashSet::new();
        pinned.insert("tool_42".to_string());
        // The smallest budget PRISM ever hands a model: meta (1,536) plus room
        // for a tool or two. The route matches nothing, so nothing but the pin
        // has any claim on the remainder.
        let budget = crate::tool_catalog::MIN_TOOL_TOKENS;

        let defs = assemble_request_tools(&catalog, "unrelated chatter", &pinned, budget);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"find_tools"), "{names:?}");
        assert!(names.contains(&"recall"), "{names:?}");
        assert!(
            names.contains(&"tool_42"),
            "a pin outranks selection: {names:?}"
        );
        let spent: usize = defs
            .iter()
            .map(crate::tool_catalog::definition_tokens)
            .sum();
        assert!(spent <= budget, "budget blown: {spent} > {budget}");

        // Squeezed to nothing, only the escape hatch remains — and the request
        // is still inside the bound rather than 260 tokens over it.
        let defs = assemble_request_tools(&catalog, "unrelated chatter", &pinned, 1);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"find_tools"), "{names:?}");
        assert!(names.contains(&"recall"), "{names:?}");
        assert_eq!(
            names.len(),
            crate::meta_tools::definitions().len(),
            "nothing but the meta-tools fits in a 1-token budget: {names:?}"
        );
    }

    /// A catalog the size of the LIVE one — 130 tools (54 Python + 77 command +
    /// meta, as loaded today) — with definitions the same order of magnitude as
    /// production. `catalog_of_54` is the same shape; this one is big enough to
    /// reproduce the pinning defect.
    fn catalog_of_130() -> crate::tool_catalog::ToolCatalog {
        let filler = "x".repeat(900);
        let tools: Vec<serde_json::Value> = (0..130)
            .map(|i| {
                serde_json::json!({
                    "name": format!("tool_{i:03}"),
                    "description": format!("capability number {i}: {filler}"),
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "target": { "type": "string", "description": "what to act on" },
                            "mode":   { "type": "string", "description": "how to act" }
                        },
                        "required": ["target"]
                    }
                })
            })
            .collect();
        crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": tools
        }))
    }

    /// THE DEFECT (adversarial review, reproduced by execution). `find_tools`'
    /// `limit` is MODEL-controlled, had no schema `maximum` and was not clamped
    /// server-side; every match auto-pins; and the pinned loop added every pin's
    /// FULL definition with no budget check at all. One
    /// `find_tools(query, limit=130)` against a 130-tool catalog therefore put
    /// the whole catalog into every later request — measured below against an
    /// 8k-context model whose ENTIRE tool budget is `MIN_TOOL_TOKENS` (2,048).
    ///
    /// The budget must bound the REQUEST, not merely the selection step.
    #[test]
    fn one_unbounded_find_tools_call_cannot_blow_the_tool_budget() {
        let catalog = catalog_of_130();
        // An 8k-context model: 8192/4 = 2048, i.e. exactly the floor.
        let budget = crate::tool_catalog::tool_token_budget(8_192);
        assert_eq!(budget, crate::tool_catalog::MIN_TOOL_TOKENS);

        // Exactly what the model asked for: find_tools(query, limit=130).
        let found: Vec<String> = catalog
            .search("capability", 130)
            .into_iter()
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(
            found.len(),
            130,
            "the whole catalog matched, as it did live"
        );

        // MEASURED, not asserted in prose: what pinning all of them costs.
        let unbounded: usize = found
            .iter()
            .filter_map(|n| catalog.find(n))
            .map(|t| crate::tool_catalog::definition_tokens(&t.to_definition()))
            .sum();
        println!(
            "unbounded pin cost: {unbounded} charged tokens vs a {budget}-token budget ({}x over)",
            unbounded / budget
        );
        assert!(
            unbounded > 16 * budget,
            "the defect's premise: {unbounded} must dwarf {budget}"
        );

        // Every match auto-pins (agent_loop h5) and stays pinned for the turn.
        let pinned: std::collections::HashSet<String> = found.into_iter().collect();
        let defs = assemble_request_tools(&catalog, "capability", &pinned, budget);
        let spent: usize = defs
            .iter()
            .map(crate::tool_catalog::definition_tokens)
            .sum();
        println!(
            "assembled request: {} tools, {spent} charged tokens (budget {budget})",
            defs.len()
        );
        assert!(
            spent <= budget,
            "pinned tools blew the budget: {spent} > {budget} — the budget is not a budget"
        );
        // …and the escape hatch is still there, so the rest stays reachable.
        let names: std::collections::HashSet<&str> =
            defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains("find_tools"), "{names:?}");
        assert!(names.contains("recall"), "{names:?}");
    }

    /// Silent truncation is how the original 15-tool cap misled the model. A
    /// pin the model asked for and did not get must come back as a NAME, so the
    /// caller can say which tools are not callable and why.
    #[test]
    fn pins_beyond_the_budget_are_reported_not_silently_dropped() {
        let catalog = catalog_of_130();
        let budget = crate::tool_catalog::tool_token_budget(8_192);
        let found: Vec<String> = catalog
            .search("capability", 130)
            .into_iter()
            .map(|t| t.name.clone())
            .collect();

        let mut pinned = std::collections::HashSet::new();
        let rejected = pin_within_budget(&catalog, &mut pinned, found.clone(), budget);

        assert!(
            !pinned.is_empty(),
            "the budget must still afford SOME of what the model asked for"
        );
        assert!(!rejected.is_empty(), "130 tools cannot fit a 2,048 budget");
        assert_eq!(
            pinned.len() + rejected.len(),
            found.len(),
            "every requested tool is either callable or reported: \
             {} pinned + {} reported != {} asked for",
            pinned.len(),
            rejected.len(),
            found.len()
        );
        for name in &rejected {
            assert!(!pinned.contains(name), "{name} reported AND pinned");
        }
        println!(
            "budget {budget}: {} of {} pinned, {} reported back to the model",
            pinned.len(),
            found.len(),
            rejected.len()
        );

        // A second call cannot sneak past the cap: the already-pinned cost is
        // charged, so the bound holds across the whole turn.
        let more = pin_within_budget(&catalog, &mut pinned, found.clone(), budget);
        assert_eq!(
            more.len(),
            rejected.len(),
            "the cap held on the second call"
        );
        let defs = assemble_request_tools(&catalog, "capability", &pinned, budget);
        let spent: usize = defs
            .iter()
            .map(crate::tool_catalog::definition_tokens)
            .sum();
        assert!(
            spent <= budget,
            "budget blown after two pin rounds: {spent}"
        );
    }

    /// Meta-tools are offered unconditionally by the meta loop, so pinning must
    /// never spend budget on one nor report one as dropped.
    #[test]
    fn pinning_never_touches_meta_tools() {
        let catalog = catalog_of_130();
        let mut pinned = std::collections::HashSet::new();
        let rejected = pin_within_budget(
            &catalog,
            &mut pinned,
            ["recall".to_string(), "find_tools".to_string()],
            0,
        );
        assert!(
            pinned.is_empty(),
            "meta-tools must not be pinned: {pinned:?}"
        );
        assert!(
            rejected.is_empty(),
            "meta-tools are always offered — never reported as dropped: {rejected:?}"
        );
    }

    /// The meta-tools are the escape hatch: `recall` and `find_tools` must
    /// survive EVERY eviction path, whatever the budget and whatever is pinned.
    #[test]
    fn meta_tools_survive_every_eviction_path() {
        let catalog = catalog_of_130();
        let all: std::collections::HashSet<String> =
            catalog.iter().map(|t| t.name.clone()).collect();
        let empty = std::collections::HashSet::new();

        for (label, pinned) in [("nothing pinned", &empty), ("whole catalog pinned", &all)] {
            for budget in [0, 1, crate::tool_catalog::MIN_TOOL_TOKENS] {
                let defs = assemble_request_tools(&catalog, "capability", pinned, budget);
                let names: std::collections::HashSet<&str> =
                    defs.iter().map(|d| d.function.name.as_str()).collect();
                for meta in ["recall", "find_tools"] {
                    assert!(
                        names.contains(meta),
                        "{meta} evicted at budget {budget} with {label}: {names:?}"
                    );
                }
                // Core-set tiering is an eviction path too.
                let tiered = tier_to_core(defs, pinned);
                let tiered_names: std::collections::HashSet<&str> =
                    tiered.iter().map(|d| d.function.name.as_str()).collect();
                for meta in ["recall", "find_tools"] {
                    assert!(
                        tiered_names.contains(meta),
                        "{meta} evicted by core tiering at budget {budget} with {label}"
                    );
                }
            }
        }
    }

    /// The whole point of the capability-gap retry: the harness re-retrieves on
    /// the model's own words and PINS the result, so those definitions are in
    /// the next request whether or not the model would have gone looking.
    #[test]
    fn capability_gap_retry_makes_the_missing_tool_reachable() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                tool_json("web", "fetch a url or search the open web"),
                tool_json("simulate_xrd", "simulate an X-ray diffraction pattern from a structure"),
                tool_json("predict", "predict a material property from composition"),
            ]
        }));
        let route = "what does the pattern look like";
        let mut pinned = std::collections::HashSet::new();

        // Tight budget + a route that matches nothing: the needle is out.
        let before = assemble_request_tools(&catalog, route, &pinned, 1);
        let before_names: Vec<&str> = before.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            !before_names.contains(&"simulate_xrd"),
            "precondition: the tool is unreachable: {before_names:?}"
        );

        // The model ends the turn admitting the gap. The harness — not the
        // model — retrieves on that sentence and pins what it finds.
        let text = "I can't help with that. I don't have a tool for X-ray diffraction simulation.";
        let gap = crate::tool_catalog::capability_gap_query(text).expect("gap detected");
        let found: Vec<String> = catalog
            .search(&gap, CAPABILITY_GAP_RETRIEVE)
            .into_iter()
            .map(|t| t.name.clone())
            .collect();
        assert!(
            found.contains(&"simulate_xrd".to_string()),
            "retrieved: {found:?}"
        );
        // Pin exactly the way the loop does now — through the budget, so the
        // retry can never claim a tool whose definition never reaches the model.
        let budget = crate::tool_catalog::MIN_TOOL_TOKENS;
        let rejected = pin_within_budget(&catalog, &mut pinned, found, budget);
        assert!(
            !rejected.contains(&"simulate_xrd".to_string()),
            "the retrieved tool fits the budget: rejected {rejected:?}"
        );

        let after = assemble_request_tools(&catalog, route, &pinned, budget);
        let after_names: Vec<&str> = after.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            after_names.contains(&"simulate_xrd"),
            "after re-retrieval the tool is callable with a FULL definition: {after_names:?}"
        );
    }

    /// ...and it stays off ordinary turns: no gap sentence, no retrieval, so
    /// the branch never fires and costs nothing.
    #[test]
    fn capability_gap_retry_does_not_fire_on_a_normal_turn() {
        for text in [
            "Inconel 718 is a precipitation-hardened nickel superalloy.",
            "I can't tell from the abstract alone whether the sample was homogenised.",
            "I ran the search and the API returned three candidates.",
        ] {
            assert!(
                crate::tool_catalog::capability_gap_query(text).is_none(),
                "retry must not fire on: {text}"
            );
        }
    }

    #[test]
    fn assemble_does_not_duplicate_pinned_and_selected() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [ tool_json("web", "search the open web") ]
        }));
        let mut pinned = std::collections::HashSet::new();
        pinned.insert("web".to_string());
        let defs = assemble_request_tools(&catalog, "web search", &pinned, 4_096);
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
        let defs = finalize_tools(&catalog, &["recall".to_string()], &pinned, 4_096);
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
    fn influence_flag_is_explicit_and_off_by_default() {
        for disabled in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("off"),
            Some("yes"),
        ] {
            assert!(!context_influence_value_enabled(disabled));
        }
        for enabled in [
            Some("1"),
            Some(" true "),
            Some("TRUE"),
            Some("on"),
            Some("ON"),
        ] {
            assert!(context_influence_value_enabled(enabled));
        }
    }

    #[cfg(not(feature = "local-inference"))]
    #[tokio::test]
    async fn influence_refuses_feature_disabled_before_artifact_verification() {
        let llm = LlmClient::new(prism_ingest::llm::LlmConfig {
            base_url: prism_ingest::llm::LOCAL_GGUF_URL.to_string(),
            model: "/definitely/not/an/installed/model.gguf".to_string(),
            ..Default::default()
        });
        let catalog = ToolCatalog::from_tool_server_json(&serde_json::json!({"tools": []}));
        let result = influence_tool_selection(
            &llm,
            &catalog,
            &[],
            &std::collections::HashSet::new(),
            4_096,
        )
        .await;
        assert!(matches!(result, Err("local_inference_feature_disabled")));
    }

    #[test]
    fn influence_pool_uses_full_definitions_and_separates_fixed_tools() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                {
                    "name": "alpha",
                    "description": "first candidate",
                    "input_schema": {
                        "type": "object",
                        "properties": {"composition": {"type": "string"}},
                        "required": ["composition"],
                        "additionalProperties": false
                    }
                },
                tool_json("pinned", "fixed discovered tool"),
                tool_json("gamma", "second candidate"),
                tool_json("recall", "shadowed catalog meta-tool")
            ]
        }));
        let pinned = std::collections::HashSet::from(["pinned".to_string()]);
        let candidates = influence_candidates(&catalog, &pinned);
        let names = candidates
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["alpha", "gamma"]);
        assert_eq!(
            candidates[0].function.parameters["required"],
            serde_json::json!(["composition"]),
            "the intervention must retain the complete callable schema"
        );

        let fixed = influence_fixed_tools(&catalog, &pinned, 8_192);
        let fixed_names = fixed
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();
        assert!(fixed_names.contains(&"recall"));
        assert!(fixed_names.contains(&"pinned"));
        assert!(!fixed_names.contains(&"alpha"));
        assert!(!fixed_names.contains(&"gamma"));
    }

    #[test]
    fn influence_generation_order_matches_fixed_plus_ranked_intervention() {
        let catalog = crate::tool_catalog::ToolCatalog::from_tool_server_json(&serde_json::json!({
            "tools": [
                tool_json("alpha", "first candidate"),
                tool_json("pinned", "fixed discovered tool"),
                tool_json("gamma", "second candidate")
            ]
        }));
        let pinned = std::collections::HashSet::from(["pinned".to_string()]);
        let ranked = vec!["gamma".to_string(), "alpha".to_string()];
        let fixed = influence_fixed_tools(&catalog, &pinned, 8_192);
        let packed = finalize_tools(&catalog, &ranked, &pinned, 8_192);
        let ordered = order_influence_request(packed, &fixed, &ranked);
        let names = ordered
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();
        let expected_fixed = fixed
            .iter()
            .map(|definition| definition.function.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(&names[..expected_fixed.len()], expected_fixed);
        assert_eq!(&names[expected_fixed.len()..], ["gamma", "alpha"]);
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
        // neural path can surface it. Args: (catalog, route, pinned, token_budget,
        // backend) — a budget that comfortably affords this 3-tool catalog, so
        // what is being tested is retrieval, not truncation.
        let defs = assemble_request_tools_neural(
            &catalog,
            "compute the stiffness",
            &pinned,
            4_096,
            &backend,
        )
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
    /// bites: a token budget too small for the whole catalog. When everything
    /// fits, both paths return everything and ranking is invisible; ranking only
    /// matters once the budget forces a cut.
    ///
    /// Construction: one needle whose *name/description* is semantically about
    /// mechanical stiffness ("elastic_stiffness_probe"), and a paraphrase route
    /// ("rigidity ... resistance to bending under load") that shares NO literal
    /// token with the needle's name or description — so the keyword scorer gives
    /// it 0 and ranks it in the dropped tail, while neural ranks it at the top
    /// of 20. Names are suffixed `_wv` so this test's name-set is unique and
    /// `global_index`'s process-global cache can't hand back another test's
    /// (stub-embedded) index.
    ///
    /// Ignored by default (needs the ~128 MB model). Run with:
    ///   `cargo test -p prism-agent --lib -- --ignored real_backend_wired`
    // No native backend on Intel macOS (no ONNX Runtime for x86_64-apple-darwin).
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
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
        let backend = prism_embed::NativeOnnx::new().expect("load local embed model");
        let pinned = std::collections::HashSet::new();
        let route = routing_query(
            "quantify the material's rigidity and its resistance to bending under load",
            &[],
        );
        // A budget that affords the meta-tools plus only the first few ranked
        // entries — so which tools rank first is what decides the outcome.
        let widest = catalog
            .iter()
            .map(|t| crate::tool_catalog::definition_tokens(&t.to_definition()))
            .max()
            .unwrap_or(0);
        let budget = meta_tool_tokens() + 3 * widest;

        // Keyword path: the needle shares no token with the route, scores 0, and
        // lands in the dropped tail.
        let kw = assemble_request_tools(&catalog, &route, &pinned, budget);
        let kw_names: Vec<&str> = kw.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            !kw_names.contains(&needle),
            "keyword ranking must leave the paraphrase-only needle in the dropped tail: {kw_names:?}"
        );

        // Neural path, same budget: the needle is a top semantic pick out of 20.
        let neural =
            assemble_request_tools_neural(&catalog, &route, &pinned, budget, &backend).await;
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
        // CONTRACT CHANGE (B2): `process_large_result` no longer takes a
        // result-store map — the write-only in-memory store is deleted; the
        // truncation pointer is fulfilled by the durable provenance store
        // (the post-hook records the full output before truncation) and the
        // `recall` meta-tool. This test now pins only the pass-through.
        let result = process_large_result("small result");
        assert_eq!(result, "small result");
    }

    #[test]
    fn test_process_large_result_large() {
        // CONTRACT CHANGE (B2): same as above — no in-memory store to
        // inspect; the promise lives in the message text and the provenance
        // store behind it.
        let content = "x".repeat(40_000);
        let result = process_large_result(&content);
        assert!(result.contains("[Showing first 8000 of 40000 chars"));
        assert!(result.contains("recall"));
    }

    #[test]
    fn test_summarize_tool_result_error_preview_is_char_safe() {
        // B1 REGRESSION: a non-JSON error message whose byte 60 lands
        // mid-multibyte-char used to panic on `&content[..60]` and abort
        // the whole turn. The fallback preview must be char-boundary-safe.
        let content = "失敗".repeat(40); // 3 bytes per char — byte 60 is mid-char
        let summary = summarize_tool_result("web", None, &content, true);
        assert!(summary.starts_with("web: error — "));
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

    // CONTRACT CHANGE (B3): `test_uuid_hex8_format` is deleted with
    // `uuid_hex8` — it minted 32-bit timestamp-derived ids for the deleted
    // in-memory result store (B2); "uuid" overstated the guarantee and the
    // collision space was moot once the store went away. Durable records
    // use the provenance store's real ids.
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

        // CONTRACT CHANGE: `keep_last` is a FLOOR, not an exact count. This
        // fixture's message at the old split index is a `tool` result, and
        // cutting there strands it from the assistant message that called it —
        // a dangling `tool_call_id` the provider rejects. Compaction now walks
        // the boundary earlier until the pairing holds, so it retains one more
        // message here (summary + 3) rather than the previous summary + 2.
        // The property this test exists for — older messages collapse into a
        // summary at the head — is unchanged.
        assert_eq!(history.len(), 4);
        // CONTRACT CHANGE: the summary is a `user` message, not `system`.
        // History is appended AFTER the preamble, so a system role here sits
        // mid-array and providers refuse it outright — GLM with
        // `1214 messages 参数非法`, mlx-lm with "System message must be at the
        // beginning". Harmless while compaction only ran after twenty turns;
        // fatal once it runs mid-turn under token pressure.
        assert_eq!(history[0].role, "user");
        assert_ne!(
            history[0].role, "system",
            "a compaction summary must never be a mid-array system message"
        );
        assert_ne!(
            history[1].role, "tool",
            "compaction must not leave a tool result as the first message after \
             the summary: its calling assistant message would be gone"
        );
        assert!(
            history[0]
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("summary text")
        );
        // The retained tail now begins one message earlier, at the assistant
        // that owns the tool result rather than at the result itself.
        assert_eq!(history[1].content.as_deref(), Some("two"));
        assert_eq!(history[2].content.as_deref(), Some("three"));
        assert_eq!(history[3].content.as_deref(), Some("four"));
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
                { "name": "file", "description": "d", "input_schema": {"type":"object"} },
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
        assert!(names.iter().any(|n| n == "file"), "core tool kept");
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

    // ── VS1 / F1: is_error crux + richer failure summary ───────────────
    //
    // These tests lock in the contract that a wrapped Python-tool failure
    // (success:false under "result") is flagged as an error, while a
    // grep-no-match (exit 1 but success:true) is NOT. They also verify the
    // failure summary surfaces exit code / timed-out / error string.

    fn gate_is_error(resp: &serde_json::Value) -> bool {
        crate::tool_result::tool_result_is_error(resp)
    }

    #[test]
    fn f1_gate_wrapped_python_raise_is_error() {
        let resp = serde_json::json!({
            "result": {
                "exit_code": 1,
                "stdout": "",
                "stderr": "Traceback (most recent call last):\nValueError: boom",
                "success": false
            }
        });
        assert!(
            gate_is_error(&resp),
            "wrapped python raise must be an error"
        );
    }

    #[test]
    fn f1_gate_wrapped_bash_grep_no_match_is_not_error() {
        // THE REGRESSION GUARD: grep exit-1 with success:true must stay a
        // non-error. Over-flagging here would break every "no match" search.
        let resp = serde_json::json!({
            "result": {
                "success": true,
                "exit_code": 1,
                "stdout": "",
                "stderr": "",
                "return_code_interpretation": "No matches found"
            }
        });
        assert!(
            !gate_is_error(&resp),
            "grep no-match (success:true) must NOT be flagged as error"
        );
    }

    #[test]
    fn f1_gate_wrapped_timeout_is_error() {
        let resp = serde_json::json!({
            "result": { "success": false, "timed_out": true, "exit_code": 124 }
        });
        assert!(gate_is_error(&resp));
    }

    #[test]
    fn f1_gate_unwrapped_notebook_failure_with_string_result_is_error() {
        // notebook_exec: top-level "result" is the last-expr value (string),
        // NOT a wrapped payload — the gate must not be fooled by it.
        let resp = serde_json::json!({
            "success": false,
            "exit_code": 1,
            "result": "some last-expression string"
        });
        assert!(gate_is_error(&resp));
    }

    #[test]
    fn f1_summary_python_raise_shows_exit_code() {
        let content = serde_json::json!({
            "exit_code": 1,
            "stdout": "",
            "stderr": "ValueError: boom",
            "success": false
        })
        .to_string();
        let summary = summarize_tool_result("execute_python", None, &content, true);
        assert!(
            summary.contains("exit 1"),
            "python failure summary must show exit code: {summary}"
        );
        assert!(
            !summary.contains("completed"),
            "failed run must not be summarized as completed: {summary}"
        );
    }

    #[test]
    fn f1_summary_timeout_shows_timed_out() {
        let content = serde_json::json!({ "success": false, "timed_out": true, "exit_code": 124 })
            .to_string();
        let summary = summarize_tool_result("execute_python", None, &content, true);
        assert!(
            summary.contains("timed out"),
            "timeout summary must say timed out: {summary}"
        );
    }

    #[test]
    fn f1_summary_bash_error_shows_exit_and_first_error_line() {
        let content = serde_json::json!({
            "success": false,
            "exit_code": 127,
            "stderr": "bash: frobnicate: command not found",
            "error": "bash: frobnicate: command not found"
        })
        .to_string();
        let summary = summarize_tool_result("execute_bash", None, &content, true);
        assert!(
            summary.contains("exit 127"),
            "bash failure must show exit code: {summary}"
        );
        assert!(
            summary.contains("command not found"),
            "bash failure must surface the first error line: {summary}"
        );
    }

    #[test]
    fn f1_summary_grep_no_match_stays_non_error_completed() {
        // Guard the OTHER direction: success:true (grep no-match) summarized
        // on the non-error path must still read as a normal completion.
        let content = serde_json::json!({
            "success": true,
            "exit_code": 1,
            "return_code_interpretation": "No matches found"
        })
        .to_string();
        let summary = summarize_tool_result("execute_bash", None, &content, false);
        assert!(
            !summary.contains("error"),
            "grep no-match must not be summarized as an error: {summary}"
        );
    }

    /// Compaction must not orphan a tool result from the assistant message
    /// that requested it.
    ///
    /// The split was a blind index: with `keep_last` landing between an
    /// assistant carrying `tool_calls` and its `tool` replies, the replies
    /// survived and their parent was discarded, leaving dangling
    /// `tool_call_id`s that a provider rejects — and it fired exactly when
    /// context was already tight, so the request failed at the worst moment.
    #[test]
    fn compaction_never_orphans_a_tool_result_from_its_call() {
        let msg = |role: &str, calls: bool, id: Option<&str>| ChatMessage {
            role: role.to_string(),
            content: Some("x".to_string()),
            tool_calls: calls.then(Vec::new),
            tool_call_id: id.map(str::to_string),
        };
        // user, assistant(tool_calls), tool, tool, user
        let mut history = vec![
            msg("user", false, None),
            msg("assistant", true, None),
            msg("tool", false, Some("a")),
            msg("tool", false, Some("b")),
            msg("user", false, None),
        ];
        // keep_last = 3 would split right onto the first `tool`, stranding it.
        compact_history(&mut history, "summary", 3);

        let first_tool = history.iter().position(|m| m.role == "tool");
        if let Some(i) = first_tool {
            let parent = history[..i].iter().rev().find(|m| m.role == "assistant");
            assert!(
                parent.is_some_and(|m| m.tool_calls.is_some()),
                "a retained tool result lost the assistant message that called it"
            );
        }
    }
    // ── Saturation signal ──────────────────────────────────────────

    /// The exact envelope a command tool returns: JSON as a STRING in `stdout`.
    fn cli_envelope(payload: serde_json::Value) -> Value {
        serde_json::json!({
            "root": "papers",
            "invocation": "prism papers search",
            "success": true,
            "exit_code": 0,
            "stdout": payload.to_string(),
            "stderr": "",
        })
    }

    fn paper(doi: Option<&str>, source: &str, id: &str) -> Value {
        let mut p = serde_json::json!({"source": source, "source_id": id, "external_ids": {}});
        if let Some(doi) = doi {
            p["doi"] = serde_json::json!(doi);
        }
        p
    }

    fn search_args(query: &str) -> Value {
        serde_json::json!({"args": ["search", "--query", query, "--limit", "20"]})
    }

    /// The digest has to carry the handle INGESTION needs, not only the one
    /// CITATION needs.
    ///
    /// It used to emit title + dedup key and stop, while `papers_ingest`
    /// requires `url` or `pmc`. So the harness could tell a saturated run to
    /// ingest, and the model had no way to name a paper to ingest — its only
    /// route to a URL was `recall`, the budget sink that killed the run in the
    /// first place. A directive the model cannot follow is worse than none: it
    /// burns the turn proving it cannot comply.
    #[test]
    fn the_digest_carries_what_ingestion_needs() {
        let mut with_text = paper(Some("10.1/a"), "openalex", "W1");
        with_text["title"] = serde_json::json!("A paper with full text");
        with_text["fulltext_url"] = serde_json::json!("https://arxiv.org/pdf/1234.5678");
        // A publisher PDF that is measurably bot-blocked (MDPI 403) must NOT be
        // offered as a handle — every ingest spent on one is a wasted call.
        let mut without = paper(Some("10.1/b"), "openalex", "W2");
        without["title"] = serde_json::json!("A paywalled paper");
        without["fulltext_url"] = serde_json::json!("https://www.mdpi.com/1/2/3/pdf");

        let digest = search_digest(
            "papers",
            &cli_envelope(serde_json::json!({"papers": [with_text, without]})),
            2,
        )
        .expect("a search with results must produce a digest");

        assert!(
            digest.contains("papers_ingest url=https://arxiv.org/pdf/1234.5678"),
            "a fetchable paper must arrive with a callable handle: {digest}"
        );
        assert!(
            digest.contains("no fetchable full text"),
            "a blocked publisher URL must be named as unusable, not offered: {digest}"
        );
        assert!(
            digest.contains("1 of the above have full text"),
            "say how many can be ingested without a recall: {digest}"
        );
        // The paper with no full text must NOT get a handle it cannot honour.
        assert_eq!(
            digest.matches("papers_ingest url=").count(),
            1,
            "only papers that actually have full text get a handle: {digest}"
        );
        // And recall must be described as costly, not as the obvious next step.
        assert!(
            digest.contains("spends this turn's remaining budget"),
            "{digest}"
        );
    }

    #[test]
    fn a_search_is_counted_through_the_cli_envelope_not_past_it() {
        // THE trap: command tools wrap output as a string in `stdout`. A walker
        // looking for a top-level `papers[]` finds nothing on every call, which
        // renders as permanent saturation from search #2 onwards.
        let mut t = SaturationTracker::default();
        let payload = serde_json::json!({"papers": [
            paper(Some("10.1/a"), "arxiv", "1"),
            paper(None, "pubmed", "2"),
        ]});
        t.observe("papers", &search_args("q1"), &cli_envelope(payload), false);

        assert_eq!(
            t.seen.len(),
            2,
            "both papers were counted through the envelope"
        );
        assert_eq!(t.searches[0].returned, 2);
        assert_eq!(t.searches[0].fresh, 2);
        assert_eq!(t.searches[0].query, "q1", "the query is read from CLI args");
    }

    #[test]
    fn a_sweep_result_is_not_silently_excluded() {
        // `sweep` nests under `outcome` and is the biggest producer. Missing the
        // nesting would drop the most productive tool from the count entirely.
        let mut t = SaturationTracker::default();
        let payload = serde_json::json!({
            "state_path": "/tmp/x.json",
            "outcome": {"papers": [paper(Some("10.1/z"), "arxiv", "9")]},
        });
        t.observe(
            "papers",
            &search_args("sweep me"),
            &cli_envelope(payload),
            false,
        );
        assert_eq!(t.seen.len(), 1, "a sweep's papers count like any other");
    }

    #[test]
    fn a_truncated_result_is_unknown_yield_not_zero_yield() {
        // 30k truncation corrupts the JSON. Reading that as "0 new" turns a big
        // SUCCESSFUL search into evidence the well is dry.
        let mut t = SaturationTracker::default();
        let mut env = cli_envelope(serde_json::json!({"papers": []}));
        env["stdout"] = serde_json::json!("{\"papers\": [{\"doi\"\n\n[Output truncated]");
        t.observe("papers", &search_args("huge"), &env, false);

        assert!(t.searches[0].unreadable, "an uncountable result says so");
        assert!(
            t.block().unwrap().contains("yield unknown"),
            "and says so to the model too"
        );
        assert_eq!(
            t.recent_new_ratio(),
            None,
            "an unknown yield must never contribute to a saturation verdict"
        );
    }

    #[test]
    fn saturation_is_declared_only_when_searching_stops_paying() {
        let mut t = SaturationTracker::default();
        let twenty: Vec<Value> = (0..20)
            .map(|i| paper(Some(&format!("10.1/{i}")), "arxiv", "x"))
            .collect();
        // FOUR searches over the same twenty papers. The window is the last
        // three, so the productive opening search has to fall OUT of it before
        // saturation can be declared — three searches total is not enough, which
        // is the point: one good search followed by two repeats is not a dry well.
        for query in ["a", "b", "c", "d"] {
            t.observe(
                "papers",
                &search_args(query),
                &cli_envelope(serde_json::json!({"papers": twenty})),
                false,
            );
        }
        let ratio = t.recent_new_ratio().expect("four readable searches");
        assert!(
            ratio <= SATURATION_NEW_RATIO,
            "the last three searches returned 60 papers and 0 new: {ratio}"
        );
        let block = t.block().unwrap();
        assert!(block.contains("SATURATED"), "{block}");
        assert!(
            block.contains("allowed and will not be blocked"),
            "annotate, never refuse: {block}"
        );
    }

    /// Saturated AND nothing ingested is the moment the run is about to be
    /// wasted, and the block has to stop describing and start directing.
    ///
    /// Measured twice — glm-5.2 on 2026-08-19 and glm-5.3 on 2026-08-20 — a
    /// research turn saturated, ingested nothing, and spent the rest of its
    /// budget on `recall`: 18 `papers`, 5 `prior_art_search`, 9 `recall`, zero
    /// ingests, dead at 100% of a 200k window with 570 papers saved and not one
    /// fact extracted. "More searching is unlikely to pay" was true and useless;
    /// the model needed to be told the one action that converts what it has.
    #[test]
    fn saturated_with_nothing_ingested_names_the_action() {
        let mut t = SaturationTracker::default();
        // FOUR rounds of the same twenty papers. The ratio is computed over the
        // last three, so the first round has to be the one that supplies them —
        // otherwise round 0's 20 new papers sit inside the window and the run
        // reads as 33% new, not saturated.
        for round in 0..4 {
            let papers: Vec<Value> = (0..20)
                .map(|i| paper(Some(&format!("10.1/{i}")), "arxiv", "x"))
                .collect();
            t.observe(
                "papers",
                &search_args(&format!("q{round}")),
                &cli_envelope(serde_json::json!({"papers": papers})),
                false,
            );
        }
        let block = t.block().unwrap();

        assert!(block.contains("ACTION REQUIRED"), "{block}");
        assert!(block.contains("papers_ingest"), "{block}");
        // recall is the trap it actually fell into, so the block must name it.
        assert!(
            block.contains("`recall` does NOT persist"),
            "the block must say recall is not a substitute for ingesting: {block}"
        );
        // And it must NOT claim the papers were lost — identity IS persisted.
        assert!(
            !block.contains("nothing found this session has been persisted"),
            "identity is saved on every search; overstating the loss teaches \
             the model to discount this block: {block}"
        );
    }

    /// The helper existing is not the same as the loop USING it.
    ///
    /// Caught while mutation-testing: disabling the call site left the unit test
    /// green, because that test exercises the predicate and the filter, not the
    /// wiring between them. No type connects the two — so this reads the source,
    /// and checks ORDER: the withholding must happen BEFORE the request is sent,
    /// or it withholds nothing.
    #[test]
    fn the_loop_actually_withholds_before_it_calls_the_model() {
        const SOURCE: &str = include_str!("agent_loop.rs");
        let body = SOURCE
            .split_once("// ── 2h. Process each tool call")
            .map_or(SOURCE, |(before, _)| before);

        let guard = body
            .find("saturation.should_withhold_search()")
            .expect("the loop must consult the tracker");
        let apply = body
            .find("withhold_search_tools(relevant_tools)")
            .expect("the loop must apply the filter to the request tools");
        let send = body
            .find("chat_with_tools_streaming")
            .expect("the loop must send the request");

        assert!(
            guard < apply && apply < send,
            "withholding must be decided and applied BEFORE the model is called: \
             guard={guard} apply={apply} send={send}"
        );
    }

    /// A directive the model can decline is not a control.
    ///
    /// Measured 2026-08-20, third full run: 495 unique papers, 12 `papers`
    /// calls, ZERO ingests — with ACTION REQUIRED in the prompt on every turn
    /// from paper 40 onward. The block fired; the model kept searching. Google's
    /// ADK harness reached the same conclusion (arXiv 2608.17528): "Leave the
    /// tools attached and it keeps calling them."
    ///
    /// So withholding must fire on EXACTLY the condition that renders the
    /// directive — otherwise the prompt and the tool list disagree about what
    /// the run is being asked to do, which is worse than either alone.
    #[test]
    fn search_is_withheld_on_exactly_the_condition_that_demands_ingestion() {
        let mut t = SaturationTracker::default();
        assert!(
            !t.should_withhold_search(),
            "a turn with no searches keeps its tools"
        );

        // Under the threshold, still finding: keep searching.
        for round in 0..2 {
            let papers: Vec<Value> = (0..10)
                .map(|i| paper(Some(&format!("10.{round}/{i}")), "arxiv", "x"))
                .collect();
            t.observe(
                "papers",
                &search_args(&format!("q{round}")),
                &cli_envelope(serde_json::json!({"papers": papers})),
                false,
            );
        }
        assert!(t.seen.len() < INGEST_NUDGE_PAPERS);
        assert!(
            !t.should_withhold_search(),
            "20 papers is not yet enough to stop"
        );
        assert!(
            t.block().unwrap().contains("NOTE:"),
            "and it is only a note"
        );

        // Past the threshold with nothing ingested: stop offering search.
        for round in 2..5 {
            let papers: Vec<Value> = (0..10)
                .map(|i| paper(Some(&format!("10.{round}/{i}")), "arxiv", "x"))
                .collect();
            t.observe(
                "papers",
                &search_args(&format!("q{round}")),
                &cli_envelope(serde_json::json!({"papers": papers})),
                false,
            );
        }
        assert!(t.seen.len() >= INGEST_NUDGE_PAPERS);
        assert!(t.should_withhold_search());
        assert!(
            t.block().unwrap().contains("ACTION REQUIRED"),
            "the tool list and the prompt must agree"
        );

        // The withholding removes ONLY search. Everything else stays reachable.
        let defs: Vec<prism_llm::ToolDefinition> = [
            "papers",
            "prior_art_search",
            "papers_ingest",
            "recall",
            "query_local",
        ]
        .iter()
        .map(|n| prism_llm::ToolDefinition {
            tool_type: "function".to_string(),
            function: prism_llm::FunctionDef {
                name: (*n).to_string(),
                description: "d".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        })
        .collect();
        let kept: Vec<String> = withhold_search_tools(defs)
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        assert!(!kept.contains(&"papers".to_string()));
        assert!(!kept.contains(&"prior_art_search".to_string()));
        assert!(
            kept.contains(&"papers_ingest".to_string()),
            "ingest must remain: {kept:?}"
        );
        assert!(
            !kept.contains(&"recall".to_string()),
            "recall is re-reading — the behaviour this state exists to stop. Run 4 \
             answered the loss of search with six recalls, three against \
             hallucinated ids: {kept:?}"
        );
        assert!(
            kept.contains(&"query_local".to_string()),
            "graph tools must remain: {kept:?}"
        );
    }

    /// A run that never saturates must still be told to ingest.
    ///
    /// Measured 2026-08-20, run 2: 30 tool calls, 357 unique papers, ZERO
    /// ingests, budget exhausted — and it never once saturated, because on a
    /// broad question the literature keeps yielding genuinely new papers. A
    /// saturation-only trigger cannot fire on exactly the runs that need it
    /// most. Run 1 failed the same way for the opposite reason: it saturated,
    /// then spent everything left on `recall`.
    #[test]
    fn plenty_of_papers_and_no_ingest_is_a_directive_even_while_still_finding() {
        let mut t = SaturationTracker::default();
        // Every round returns entirely new papers, so this NEVER saturates.
        for round in 0..5 {
            let papers: Vec<Value> = (0..12)
                .map(|i| paper(Some(&format!("10.{round}/{i}")), "arxiv", "x"))
                .collect();
            t.observe(
                "papers",
                &search_args(&format!("q{round}")),
                &cli_envelope(serde_json::json!({"papers": papers})),
                false,
            );
        }
        assert!(
            t.seen.len() >= INGEST_NUDGE_PAPERS,
            "fixture must pass the threshold"
        );
        assert!(
            !matches!(t.recent_new_ratio(), Some(r) if r <= SATURATION_NEW_RATIO),
            "this fixture must NOT be saturated — that is the whole point"
        );

        let block = t.block().unwrap();
        assert!(block.contains("ACTION REQUIRED"), "{block}");
        assert!(block.contains("papers_ingest"), "{block}");
        assert!(
            block.contains("worth less now than the first ingest"),
            "the reason must be volume, not saturation: {block}"
        );
        assert!(
            block.contains("STILL FINDING"),
            "and it is still finding: {block}"
        );
    }

    /// Before saturation the same fact is a note, not a directive — there is
    /// still a reason to keep searching.
    #[test]
    fn still_finding_with_nothing_ingested_is_only_a_note() {
        let mut t = SaturationTracker::default();
        for round in 0..3 {
            let papers: Vec<Value> = (0..10)
                .map(|i| paper(Some(&format!("10.{round}/{i}")), "arxiv", "x"))
                .collect();
            t.observe(
                "papers",
                &search_args(&format!("q{round}")),
                &cli_envelope(serde_json::json!({"papers": papers})),
                false,
            );
        }
        let block = t.block().unwrap();
        assert!(block.contains("NOTE:"), "{block}");
        assert!(!block.contains("ACTION REQUIRED"), "{block}");
        assert!(block.contains("IDENTITY is already saved"), "{block}");
    }

    #[test]
    fn fresh_findings_do_not_read_as_saturation() {
        let mut t = SaturationTracker::default();
        for round in 0..3 {
            let papers: Vec<Value> = (0..10)
                .map(|i| paper(Some(&format!("10.{round}/{i}")), "arxiv", "x"))
                .collect();
            t.observe(
                "papers",
                &search_args(&format!("q{round}")),
                &cli_envelope(serde_json::json!({"papers": papers})),
                false,
            );
        }
        assert_eq!(t.seen.len(), 30);
        let block = t.block().unwrap();
        assert!(block.contains("STILL FINDING"), "{block}");
    }

    #[test]
    fn a_source_outage_is_never_reported_as_an_exhausted_literature() {
        let mut t = SaturationTracker::default();
        let payload = serde_json::json!({
            "papers": [],
            "source_status": [{"source": "pubmed", "ok": false, "error": "429"}],
        });
        t.observe("papers", &search_args("q"), &cli_envelope(payload), false);
        let block = t.block().unwrap();
        assert!(
            block.contains("pubmed"),
            "the failing source is named: {block}"
        );
        assert!(
            block.contains("rather than the literature being exhausted"),
            "{block}"
        );
    }

    #[test]
    fn the_dedup_key_matches_the_retrieval_crates_precedence() {
        // Mirrors prism_retrieval::Paper::dedup_key. DOI > arxiv > pmc > source.
        // NO url tier, and no fuzzy title match — guessed identity manufactures
        // wrong data.
        let mut both = paper(Some("10.1/a"), "arxiv", "1");
        both["external_ids"] = serde_json::json!({"arxiv": "2401.1", "pmc": "PMC1"});
        assert_eq!(paper_key(&both).as_deref(), Some("doi:10.1/a"));

        let mut no_doi = paper(None, "arxiv", "1");
        no_doi["external_ids"] = serde_json::json!({"arxiv": "2401.1", "pmc": "PMC1"});
        assert_eq!(paper_key(&no_doi).as_deref(), Some("arxiv:2401.1"));

        let mut pmc_only = paper(None, "pubmed", "1");
        pmc_only["external_ids"] = serde_json::json!({"pmc": "PMC1"});
        assert_eq!(paper_key(&pmc_only).as_deref(), Some("pmc:PMC1"));

        assert_eq!(
            paper_key(&paper(None, "doaj", "7")).as_deref(),
            Some("doaj:7")
        );
        assert_eq!(
            paper_key(&serde_json::json!({"url": "https://x"})),
            None,
            "a bare url is NOT an identity"
        );
    }

    #[test]
    fn ingesting_is_counted_from_where_the_command_actually_reports_it() {
        let mut t = SaturationTracker::default();
        // `written` sits under the command's own `stored` object.
        t.observe(
            "papers_ingest",
            &serde_json::json!({}),
            &cli_envelope(serde_json::json!({"stored": {"written": 23}})),
            false,
        );
        assert_eq!(t.ingested_ok, 1);
        assert_eq!(t.facts_written, 23);
    }

    #[test]
    fn nothing_persisted_is_stated_plainly() {
        let mut t = SaturationTracker::default();
        t.observe(
            "papers",
            &search_args("q"),
            &cli_envelope(serde_json::json!({"papers": [paper(Some("10.1/a"), "arxiv", "1")]})),
            false,
        );
        let block = t.block().unwrap();
        // The claim this used to assert — "nothing found this session has been
        // persisted" — stopped being true when `persist_paper_identities`
        // landed: identity is written to the graph on every search. What is
        // still missing is the FACTS, and that is what has to be said plainly.
        assert!(block.contains("0 ingested"), "{block}");
        assert!(block.contains("IDENTITY is already saved"), "{block}");
        assert!(block.contains("FACTS are not"), "{block}");
    }

    fn tool_msg(call_id: &str, chars: usize) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some("x".repeat(chars)),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
        }
    }

    fn assistant_calling(call_id: &str, tool: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![prism_llm::ToolCallResponse {
                id: call_id.to_string(),
                call_type: "function".to_string(),
                function: prism_llm::FunctionCall {
                    name: tool.to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            tool_call_id: None,
        }
    }

    fn user_msg() -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some("next".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// A long tail of individually-reasonable results is what actually kills a
    /// research turn, and no per-call cap can see it.
    ///
    /// Measured 2026-08-20: nine `recall`s, none oversized, took a 200k window
    /// from 87% to 100%. Capping one call bounds the worst call; only pruning
    /// reclaims the accumulated tail. Pattern from Google's ADK long-horizon
    /// harness, whose own note says dropping either mechanism "leaves a real
    /// session unbounded".
    #[test]
    fn stale_bulk_is_reclaimed_but_recent_and_expensive_results_survive() {
        let big = PRUNE_MIN_PART_TOKENS * prism_llm::CHARS_PER_TOKEN * 4; // ~2k tokens each
        let mut history = Vec::new();

        // Old, prunable bulk: 40 results, well past the recent window.
        for i in 0..40 {
            let id = format!("old-{i}");
            history.push(assistant_calling(&id, "papers"));
            history.push(tool_msg(&id, big));
        }
        // An expensive subagent report in the same stale region — never pruned.
        history.push(assistant_calling("sub-1", "spawn_subagent"));
        history.push(tool_msg("sub-1", big));
        // Recent work — protected by the token countdown, which is what a
        // single long research turn actually needs.
        for i in 0..3 {
            history.push(user_msg());
            let id = format!("recent-{i}");
            history.push(assistant_calling(&id, "papers"));
            history.push(tool_msg(
                &id,
                PRUNE_PROTECT_TOKEN_BUDGET * prism_llm::CHARS_PER_TOKEN / 3,
            ));
        }

        let before: Vec<Option<String>> = history.iter().map(|m| m.content.clone()).collect();
        let outcome = prune_stale_tool_results(&mut history);

        assert!(outcome.pruned > 0, "a long stale tail must be reclaimed");
        assert!(outcome.reclaimed_tokens >= PRUNE_MIN_RECLAIM_TOKENS);

        // The subagent report survives: "re-run it" is not a fair bargain.
        let sub = history
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("sub-1"))
            .expect("subagent result");
        assert!(
            !sub.content.as_deref().unwrap().starts_with(PRUNE_MARKER),
            "an expensive subagent report must never be pruned"
        );
        // The recent turns survive, so the model keeps the thread it is pulling.
        for i in 0..3 {
            let id = format!("recent-{i}");
            let recent = history
                .iter()
                .find(|m| m.tool_call_id.as_deref() == Some(id.as_str()))
                .expect("recent result");
            assert!(
                !recent.content.as_deref().unwrap().starts_with(PRUNE_MARKER),
                "recent result {id} must survive"
            );
        }
        // And the marker points at recall, because the body really is durable.
        let pruned_one = history
            .iter()
            .find(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|c| c.starts_with(PRUNE_MARKER))
            })
            .expect("something was pruned");
        assert!(pruned_one.content.as_deref().unwrap().contains("recall"));
        assert_ne!(before.len(), 0);
    }

    /// Anti-thrash: a short turn is never rewritten for a trivial gain.
    #[test]
    fn a_small_history_is_left_completely_alone() {
        let mut history = vec![
            assistant_calling("a", "papers"),
            tool_msg("a", 400),
            user_msg(),
        ];
        let snapshot = history.clone();
        let outcome = prune_stale_tool_results(&mut history);
        assert_eq!(outcome, PruneOutcome::default());
        for (before, after) in snapshot.iter().zip(history.iter()) {
            assert_eq!(before.content, after.content, "nothing may be rewritten");
        }
    }

    #[test]
    fn a_turn_with_no_search_renders_nothing_at_all() {
        let t = SaturationTracker::default();
        assert!(
            t.block().is_none(),
            "a chat turn is byte-for-byte unchanged"
        );
    }
    #[test]
    fn a_counted_search_collapses_to_a_digest_the_model_can_act_on() {
        let payload = serde_json::json!({"papers": (0..12).map(|i| {
            let mut p = paper(Some(&format!("10.1/{i}")), "arxiv", "x");
            p["title"] = serde_json::json!(format!("Paper number {i}"));
            p["abstract"] = serde_json::json!("word ".repeat(400));
            p
        }).collect::<Vec<_>>()});
        let envelope = cli_envelope(payload);
        let raw = serde_json::to_string(&envelope).unwrap();

        let digest = search_digest("papers", &envelope, 5).expect("a search collapses");
        assert!(
            digest.len() < raw.len() / 4,
            "the digest must be far smaller: {} vs {}",
            digest.len(),
            raw.len()
        );
        assert!(
            digest.contains("12 result(s), 5 not seen before"),
            "{digest}"
        );
        assert!(
            digest.contains("Paper number 0"),
            "titles survive: {digest}"
        );
        assert!(
            digest.contains("and 4 more"),
            "the tail is counted, not hidden: {digest}"
        );
        assert!(
            digest.contains("recall("),
            "and the full records are reachable: {digest}"
        );
        assert!(
            !digest.contains("word word word"),
            "abstracts do NOT survive: {digest}"
        );
    }

    #[test]
    fn a_non_search_result_is_left_to_the_normal_path() {
        let envelope = cli_envelope(serde_json::json!({"workflows": [{"name": "x"}]}));
        assert!(
            search_digest("workflow", &envelope, 0).is_none(),
            "only search results are digested"
        );
        assert!(
            search_digest(
                "papers",
                &cli_envelope(serde_json::json!({"papers": []})),
                0
            )
            .is_none(),
            "an empty search has nothing to digest and keeps its own message"
        );
    }
}
