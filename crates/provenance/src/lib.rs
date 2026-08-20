// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PRISM Provenance Layer
//!
//! Every action in the materials discovery pipeline is recorded with
//! full traceability: which tool was called, with what parameters, by
//! which LLM, what data was read, what data was produced, and what
//! the chain of reasoning was.
//!
//! # Two distinct concerns share this crate
//!
//! 1. **Provenance ledger** (this file): an append-only Turso log of every
//!    agent/tool action — the compliance/traceability record of *what
//!    happened*.
//! 2. **Materials knowledge graph** ([`emmo`], ~1.7k LOC — larger than the
//!    ledger beside it): EMMO-ontology entities + PROV-O assertions on the
//!    same Turso store. This is a queryable graph of materials *facts*
//!    (matter, measurements, properties, phases) with noisy-OR
//!    corroboration, i.e. a knowledge base, not an audit trail.
//!
//! They are deliberately separate: the ledger records *what happened*; the
//! graph records *what is believed to be true about materials*. Both persist
//! to the same database file but use disjoint tables.
//!
//! Backed by Turso — a ground-up Rust rewrite of SQLite. Local-first,
//! async-native, optionally synced to Turso Cloud for cross-device
//! provenance sharing. Every PRISM session gets its own Turso database
//! file (the "many-database architecture" — databases are files, not
//! processes, so there's no cold start).

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use turso::Value;
use uuid::Uuid;

pub mod emmo;
pub mod units;
pub use emmo::{
    ActivityDecoding, AssertionClassification, ClassRegionDistance, ClassifiedFactNodes,
    ClassifiedNode, ConditionValue, EmbeddingPartition, EntityGeometryCoverage,
    EntityGeometryNeighbor, EntityGeometryProbe, EvidenceClass, EvidenceContribution,
    EvidenceSource, FactGraphShape, FactNodeLabels, FactPayload, GraphEdge, GraphNode,
    LOCAL_TENANT, LocalAssertion, LocalFact, LocalProvenance, MaterialFact, MeasurementCondition,
    OntologyBoundFactNodes, OntologyClassification, QuantitySignDomain, QudtUnit, RecalledFact,
    RecalledMaterialFact, SemanticEntityHit, SourceCitation, StoreBusy, StoredAssertion,
    TraversalResult, TripleGeometryNeighbor, TripleGeometryProbe, UnitTerm, VerificationFilter,
    VerificationStatus, assertion_id, canonical_key, conditioned_assertion_id, evidence_for_result,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    pub id: String,
    pub timestamp: String,
    pub session_id: String,
    pub action_type: ActionType,
    pub actor: Actor,
    pub tool_name: Option<String>,
    pub llm_model: Option<String>,
    pub input_json: serde_json::Value,
    pub output_json: Option<serde_json::Value>,
    pub parent_id: Option<String>,
    pub material_ref: Option<String>,
    pub confidence: f64,
    pub tags: Vec<String>,
    /// VS1/F5: structured outcome flag — "ok" | "error" | None.
    /// None means "unknown" (a legacy row written before this field existed,
    /// or a non-tool record where the notion does not apply). Honest
    /// "unknown" rather than a defaulted lie. Lets "which runs failed" be a
    /// real query, not something inferred by grepping output_json.
    pub status: Option<String>,
    /// VS1/F5: the tool's exit code when one was reported. None when absent
    /// or non-numeric. Same signal as the F1 is_error gate, captured here for
    /// queryability.
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    ToolCall,
    LlmCall,
    Ingest,
    Query,
    Generative,
    Workflow,
    Compute,
    Mesh,
}

impl ActionType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::LlmCall => "llm_call",
            Self::Ingest => "ingest",
            Self::Query => "query",
            Self::Generative => "generative",
            Self::Workflow => "workflow",
            Self::Compute => "compute",
            Self::Mesh => "mesh",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Agent,
    /// Legacy unqualified user actor. New request-bound records should use
    /// [`Actor::AnonymousLocal`] or [`Actor::AuthenticatedUser`].
    User,
    /// A local caller whose identity was not established by a session.
    AnonymousLocal,
    /// A caller whose identity was established by a validated node session.
    AuthenticatedUser,
    /// A caller whose validated identity matched the node owner.
    AuthenticatedOwner,
    System,
    Scheduler,
}

impl Actor {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::AnonymousLocal => "anonymous_local",
            Self::AuthenticatedUser => "authenticated_user",
            Self::AuthenticatedOwner => "authenticated_owner",
            Self::System => "system",
            Self::Scheduler => "scheduler",
        }
    }
}

/// Lifecycle state for a durable agent run.
///
/// `stuck` is deliberately absent: it is derived by comparing the heartbeat
/// timestamp of a [`AgentRunStatus::Running`] row with an
/// [`AgentRunStalenessPolicy`]. A process that disappears cannot reliably
/// write one last state transition about itself.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl AgentRunStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_db(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => anyhow::bail!("unknown agent run status `{other}`"),
        }
    }
}

/// One durable top-level or delegated agent turn.
///
/// `parent_run_id` is the spawn edge. It is intentionally unrelated to
/// [`ProvenanceRecord::parent_id`], whose meaning remains the retry/repair
/// chain between tool-call audit records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRun {
    pub id: String,
    pub parent_run_id: Option<String>,
    pub session_id: String,
    /// Stable execution class, currently `agent` or `subagent`.
    pub role: String,
    /// Human-readable task text, clipped by the writer before persistence.
    pub label: String,
    pub status: AgentRunStatus,
    pub started_at: String,
    /// Last successful lifecycle write or heartbeat.
    pub updated_at: String,
    pub ended_at: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_usd: f64,
    pub last_error: Option<String>,
}

/// Filters for [`ProvenanceStore::list_agent_runs`].
#[derive(Debug, Clone)]
pub struct AgentRunFilter {
    pub session_id: Option<String>,
    pub status: Option<AgentRunStatus>,
    /// Select the direct children of this run. `None` leaves parent unfiltered.
    pub parent_run_id: Option<String>,
    /// Maximum rows returned, clamped by the store to a safe upper bound.
    pub limit: usize,
}

const DEFAULT_AGENT_RUN_QUERY_LIMIT: usize = 100;
const MAX_AGENT_RUN_QUERY_LIMIT: usize = 1_000;

impl Default for AgentRunFilter {
    fn default() -> Self {
        Self {
            session_id: None,
            status: None,
            parent_run_id: None,
            limit: DEFAULT_AGENT_RUN_QUERY_LIMIT,
        }
    }
}

/// Resource policy for one transitive agent-run spawn traversal.
///
/// The default follows at most 64 spawn edges below the requested root and
/// accepts at most 1,000 stored child rows. One additional row may be inspected
/// as a truncation probe. Reaching either bound is reported through
/// [`AgentRunTraversalOutcome::Incomplete`]; it is never presented as a
/// complete answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRunTraversalPolicy {
    /// Deepest spawn edge included below the root (`1` means direct children).
    pub max_depth: usize,
    /// Maximum stored child rows accepted into the traversal.
    ///
    /// The store may inspect one additional row as a truncation probe.
    pub max_rows: usize,
}

impl Default for AgentRunTraversalPolicy {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_rows: MAX_AGENT_RUN_QUERY_LIMIT,
        }
    }
}

/// Why a transitive agent-run traversal could not claim completeness.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentRunTraversalIssue {
    /// A row pointed back to a run already visited on the same spawn path.
    CycleDetected { repeated_run_id: String },
    /// At least one descendant exists below the policy's maximum depth.
    DepthLimitReached { max_depth: usize },
    /// At least one traversal row exists beyond the policy's row budget.
    RowLimitReached { max_rows: usize },
}

/// Completeness of a transitive agent-run traversal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentRunTraversalOutcome {
    /// Every reachable spawn edge was inspected without a cycle or bound hit.
    Complete,
    /// Returned runs are partial or the stored topology is cyclic.
    Incomplete { issues: Vec<AgentRunTraversalIssue> },
}

/// Transitive descendants of one agent run plus an explicit completeness claim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRunDescendants {
    /// Reachable runs excluding the root, in stable breadth-first order.
    pub runs: Vec<AgentRun>,
    pub outcome: AgentRunTraversalOutcome,
}

/// Rebuildable search metadata for one durable agent session.
///
/// The session JSONL remains the source of truth. This row is only a mirror
/// that can be discarded and reconstructed from `source_path`; consequently
/// it deliberately contains no foreign key into the provenance ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionIndexEntry {
    pub session_id: String,
    /// Directory containing the source session JSONL files.
    pub source_path: String,
    /// Full path to this session's JSONL file.
    pub file_path: String,
    /// Durable project scope captured by the agent when it is known.
    pub project_cwd: Option<String>,
    pub created_at: f64,
    pub updated_at: f64,
    pub model: String,
    pub turn_count: u64,
    pub compaction_count: u64,
    pub parent_session_id: Option<String>,
    pub branch_name: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub title_source: Option<String>,
    pub title_turn: u64,
    pub models: Vec<String>,
    /// Short derived excerpt used by the history UI and token search.
    pub preview: Option<String>,
    pub size_bytes: u64,
}

/// One refused fact awaiting repair.
///
/// `item_id` identifies the REFUSAL — the fact's identity plus its class —
/// not the run that produced it, so re-ingesting a document does not enqueue
/// the same refusal twice.
#[derive(Debug, Clone, PartialEq)]
pub struct RepairItem {
    pub item_id: String,
    /// The source document this refusal came from.
    pub document: String,
    pub tenant: String,
    /// `RejectionClass::as_str()` from the ingest crate. Stored as text so
    /// the store does not depend on the extractor's enum.
    pub class: String,
    /// The refused fact, or the raw extraction when conversion itself failed.
    pub subject_json: String,
    /// The human-readable reason the fact was refused.
    pub detail: String,
    pub enqueued_at: f64,
    pub attempts: i64,
}

/// One recorded decision about a queued item.
///
/// Append-only. `outcome` is `accept` (with the correction and the evidence
/// that verified it) or `withdraw` (with the reason). There is deliberately
/// no third state: an item that was looked at and left undecided would be
/// exactly the silent outcome this ledger exists to prevent.
#[derive(Debug, Clone, PartialEq)]
pub struct RepairDisposition {
    pub item_id: String,
    /// Which attempt this decision belongs to. A second decision on the same
    /// item is a new row, never an overwrite.
    pub attempt: i64,
    pub document: String,
    pub class: String,
    /// `accept` or `withdraw`.
    pub outcome: String,
    /// The corrected fact, when accepted.
    pub corrected_json: Option<String>,
    /// What verified the correction — the verbatim span, or the code rule.
    pub evidence: Option<String>,
    pub reason: String,
    /// Who decided: `code:<rule>` or `model:<id>`. An audit must be able to
    /// tell a deterministic repair from a model's judgement.
    pub dispositioner: String,
    pub decided_at: f64,
}

/// One pending ontology extension proposal — the durable half of what the
/// paper reader proposes. `proposal_json` is the reader's complete proposal
/// record (label, description, parent or endpoint IRIs); the CITATIONS live
/// in [`OntologyProposalSighting`] rows keyed by `item_id`, because
/// evidence accumulates across documents while identity does not.
#[derive(Debug, Clone, PartialEq)]
pub struct OntologyProposalItem {
    /// Stable identity of the PROPOSAL CONTENT (kind + label + parents or
    /// endpoints) — NOT of the run or the document. Same concept re-proposed
    /// from another paper is the same item with one more sighting.
    pub item_id: String,
    /// `class` or `relation`.
    pub kind: String,
    pub label: String,
    /// The document whose reader first proposed this identity.
    pub document: String,
    pub tenant: String,
    /// The complete proposal record as JSON (the paper agent's class or
    /// relation proposal without its citation).
    pub proposal_json: String,
    pub enqueued_at: f64,
}

/// One citation backing a proposal: the exact lines the reader had read when
/// it made the proposal. A proposal without its evidence is worthless for
/// governance, so every sighting is retained, including sightings of an
/// already-dispositioned proposal (the audit trail of what was proposed
/// where survives the decision).
#[derive(Debug, Clone, PartialEq)]
pub struct OntologyProposalSighting {
    pub item_id: String,
    pub document: String,
    /// The reader's citation record as JSON: source revision id, line range,
    /// and the quoted lines.
    pub citation_json: String,
    pub sighted_at: f64,
}

/// One recorded governance decision about an ontology proposal.
///
/// Append-only, mirroring [`RepairDisposition`]. `outcome` is `accepted` or
/// `rejected`. Accepting feeds the existing induction promotion path — the
/// draft artifact path is recorded in `artifact_path` — and REJECTING IS
/// FINAL for the identity: the enqueue path refuses to re-queue any identity
/// with a recorded disposition, so a rejected concept is not re-proposed
/// forever.
#[derive(Debug, Clone, PartialEq)]
pub struct OntologyProposalDisposition {
    pub item_id: String,
    pub document: String,
    /// `class` or `relation`.
    pub kind: String,
    pub label: String,
    /// `accepted` or `rejected`.
    pub outcome: String,
    /// The DRAFT ontology artifact an acceptance wrote (if any). Acceptance
    /// never promotes — promotion is a separate deliberate act.
    pub artifact_path: Option<String>,
    pub reason: String,
    /// Who decided: `human:<id>` or `agent:<model>`. An audit must be able
    /// to tell a human governance decision from an agent's.
    pub dispositioner: String,
    pub decided_at: f64,
}

/// One recorded re-verification verdict about a stored assertion's source
/// witness — the durable half of retrieval re-reading.
///
/// Append-only, mirroring [`RepairDisposition`]: one row per (assertion,
/// source witness, run), never updated in place, so the history of a
/// re-check is readable rather than replaced. `verdict` is `affirmed`,
/// `denied`, `uncertain`, or `not_ready` — a witness that could not be
/// safely reopened (source moved, cited lines changed, legacy citation)
/// records `not_ready` with the reason, because a re-check that silently
/// skipped a witness would report a clean bill of health it never gave.
///
/// Verdicts NEVER rewrite `prov_assertion.verification_status`: the status
/// axis records what ingest-time checks established over real document
/// witnesses (worst-wins per sighting, best-wins per assertion), and a
/// post-hoc UPDATE would bypass exactly that laundering protection. The
/// ledger is the audit axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReverifyVerdict {
    pub assertion_id: String,
    /// Which source witness (`prov_assertion_evidence.source_key`) this
    /// verdict examined.
    pub source_key: String,
    /// `affirmed`, `denied`, `uncertain`, or `not_ready`.
    pub verdict: String,
    /// The model's reason tied to the cited lines, or the deterministic
    /// reason a witness was not ready.
    pub reason: String,
    /// Who decided: `model:<id>` for affirmations, `code:reread` for
    /// not-ready outcomes. An audit must be able to tell a model's
    /// judgement from the re-reader's own refusal.
    pub reviewer: String,
    pub decided_at: f64,
}

/// What `enqueue_ontology_proposal` did, so callers can report honestly
/// instead of counting rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OntologyProposalEnqueue {
    /// First sighting of this identity: a queue row was inserted.
    Queued,
    /// The identity was already queued; this citation was recorded as a new
    /// sighting (`true`) or had already been seen from this document
    /// (`false` — a re-ingest).
    Existing { new_sighting: bool },
    /// The identity already has a recorded disposition (accepted or
    /// rejected). NOTHING was stored: a rejected proposal must stay
    /// rejected, and an accepted one is already on the governance path.
    SupersededByDisposition,
}

/// Filters for [`ProvenanceStore::list_session_metadata`].
#[derive(Debug, Clone)]
pub struct SessionIndexQuery {
    pub source_path: Option<String>,
    pub project_cwd: Option<String>,
    /// Exact token query over title, summary, and preview. All normalized
    /// tokens must be present in a matching session. Oversized input is
    /// rejected rather than truncated so the all-token contract stays true.
    pub text: Option<String>,
    pub updated_after: Option<f64>,
    pub updated_before: Option<f64>,
    /// Maximum rows returned, clamped by the store to a safe upper bound.
    pub limit: usize,
    /// Number of rows to skip after applying filters and stable ordering.
    pub offset: usize,
}

const DEFAULT_SESSION_INDEX_QUERY_LIMIT: usize = 100;
/// Global cap applied to indexed and JSONL-fallback session queries.
pub const MAX_SESSION_INDEX_QUERY_LIMIT: usize = 1_000;
/// Maximum accepted UTF-8 byte length for one session text query.
pub const MAX_SESSION_INDEX_SEARCH_BYTES: usize = 8 * 1024;
/// Maximum accepted number of distinct normalized session search terms.
pub const MAX_SESSION_INDEX_SEARCH_TERMS: usize = 32;

impl Default for SessionIndexQuery {
    fn default() -> Self {
        Self {
            source_path: None,
            project_cwd: None,
            text: None,
            updated_after: None,
            updated_before: None,
            limit: DEFAULT_SESSION_INDEX_QUERY_LIMIT,
            offset: 0,
        }
    }
}

/// Policy for deriving which running agents are stale.
///
/// Staleness is an operator policy, not a lifecycle state. The default treats
/// a running row as stale after five minutes without a heartbeat and bounds a
/// single read to 1,000 rows. Callers with different turn latency or display
/// limits should provide an explicit policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRunStalenessPolicy {
    /// Maximum age of `updated_at` before a running row is considered stale.
    pub stale_after: std::time::Duration,
    /// Maximum stale rows returned, clamped by the store's global query cap.
    pub max_results: usize,
}

impl Default for AgentRunStalenessPolicy {
    fn default() -> Self {
        Self {
            stale_after: std::time::Duration::from_secs(5 * 60),
            max_results: MAX_AGENT_RUN_QUERY_LIMIT,
        }
    }
}

/// Construct a new running row with a caller-visible id for spawn topology.
pub fn new_agent_run(
    session_id: &str,
    role: &str,
    label: &str,
    parent_run_id: Option<&str>,
) -> AgentRun {
    let now = Utc::now().to_rfc3339();
    AgentRun {
        id: Uuid::new_v4().to_string(),
        parent_run_id: parent_run_id.map(str::to_string),
        session_id: session_id.to_string(),
        role: role.to_string(),
        label: label.to_string(),
        status: AgentRunStatus::Running,
        started_at: now.clone(),
        updated_at: now,
        ended_at: None,
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: 0.0,
        last_error: None,
    }
}

/// Convert an Option<String> to a turso Value (None → Null).
fn opt_to_value(s: &Option<String>) -> Value {
    match s {
        Some(v) => Value::Text(v.clone()),
        None => Value::Null,
    }
}

/// VS1/F5 migration helper: add a column only if it is not already present.
///
/// There is no migration framework here — only `CREATE TABLE IF NOT EXISTS`,
/// which does NOT upgrade an existing table's schema. On a legacy database
/// the ALTER raises "duplicate column name"; we swallow that specific case so
/// the migration is idempotent across opens. Any OTHER error (e.g. the table
/// itself missing — which would indicate a corrupted schema) is propagated.
async fn add_column_if_absent(
    conn: &turso::Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<()> {
    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {col_type}");
    match conn.execute(sql, ()).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            // SQLite/Turso: "duplicate column name: <col>" when it already
            // exists. Tolerate it; propagate everything else.
            if msg.contains("duplicate column name") {
                Ok(())
            } else {
                Err(anyhow::anyhow!(e).context(format!("failed to add column {column} to {table}")))
            }
        }
    }
}

fn normalized_session_terms(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn entry_search_terms(entry: &SessionIndexEntry) -> std::collections::BTreeSet<String> {
    let mut terms = std::collections::BTreeSet::new();
    for text in [&entry.title, &entry.summary, &entry.preview]
        .into_iter()
        .flatten()
    {
        terms.extend(normalized_session_terms(text));
    }
    terms
}

fn session_index_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn validate_session_source_path(source_path: &str) -> Result<()> {
    anyhow::ensure!(
        !source_path.trim().is_empty(),
        "session metadata source_path must not be empty"
    );
    Ok(())
}

fn validate_session_index_entry(
    entry: &SessionIndexEntry,
    expected_source_path: Option<&str>,
) -> Result<()> {
    validate_session_source_path(&entry.source_path)?;
    if let Some(expected) = expected_source_path {
        anyhow::ensure!(
            entry.source_path == expected,
            "session {} belongs to source_path {:?}, expected {:?}",
            entry.session_id,
            entry.source_path,
            expected
        );
    }
    anyhow::ensure!(
        !entry.session_id.trim().is_empty(),
        "session metadata session_id must not be empty"
    );
    anyhow::ensure!(
        !entry.file_path.trim().is_empty(),
        "session metadata file_path must not be empty"
    );
    anyhow::ensure!(
        entry.created_at.is_finite(),
        "session metadata created_at must be finite"
    );
    anyhow::ensure!(
        entry.updated_at.is_finite(),
        "session metadata updated_at must be finite"
    );
    for (field, value) in [
        ("turn_count", entry.turn_count),
        ("compaction_count", entry.compaction_count),
        ("title_turn", entry.title_turn),
        ("size_bytes", entry.size_bytes),
    ] {
        anyhow::ensure!(
            i64::try_from(value).is_ok(),
            "session metadata {field} exceeds i64"
        );
    }
    Ok(())
}

async fn begin_session_index_txn(
    conn: &turso::Connection,
) -> Result<turso::transaction::Transaction<'_>> {
    turso::transaction::Transaction::new_unchecked(
        conn,
        turso::transaction::TransactionBehavior::Immediate,
    )
    .await
    .context("failed to begin session metadata transaction")
}

async fn finish_session_index_txn(
    txn: turso::transaction::Transaction<'_>,
    result: Result<()>,
) -> Result<()> {
    match result {
        Ok(()) => txn
            .commit()
            .await
            .context("failed to commit session metadata transaction"),
        Err(error) => {
            let _ = txn.rollback().await;
            Err(error)
        }
    }
}

/// Turso's local pager cannot initialize the same brand-new SQLite file from
/// two independent `Database` handles concurrently. Agent turns can start in
/// parallel, so serialize the short open/schema phase within this process;
/// ordinary reads and writes remain concurrent after `open` returns.
static STORE_OPEN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct ProvenanceStore {
    conn: turso::Connection,
    /// Serializes EVERY write issued through this one handle — the raw
    /// `BEGIN IMMEDIATE` transactions of the fact writers AND each
    /// single-statement writer (`record`, `record_activity`,
    /// `embed_and_store`, the entity-vector upserts). Two reasons, both on
    /// the one shared connection:
    ///
    /// 1. A raw `BEGIN IMMEDIATE` cannot nest, so without the mutex a
    ///    `tokio::join!` on one `Arc<ProvenanceStore>` races into "cannot
    ///    start a transaction within a transaction" — an opaque error, not
    ///    `StoreBusy`, and the losing fact is silently not stored.
    /// 2. A single-statement writer that skips the lock silently JOINS
    ///    whatever transaction is currently open: if that transaction rolls
    ///    back, the bystander's row vanishes even though its caller was
    ///    already told `Ok(())` — a silent lost write.
    ///
    /// Most single-table read paths deliberately do NOT take the lock: a read
    /// joins an open transaction harmlessly and holds nothing that a rollback
    /// could destroy. The session-metadata reads are exceptions because they
    /// must not observe a partially staged multi-table source rebuild. The
    /// agent-run descendant traversal is another: it opens a read transaction
    /// so its multi-query completeness claim refers to one stable snapshot,
    /// and no same-handle writer may silently join that transaction.
    write_lock: tokio::sync::Mutex<()>,
}

impl ProvenanceStore {
    pub async fn open(path: &Path) -> Result<Self> {
        // A non-UTF-8 path must be a hard error, never a silent ":memory:"
        // fallback — that would make the whole session's provenance vanish on
        // exit with no warning (AUDIT_BACKLOG 0.4). A caller that genuinely
        // wants in-memory passes the UTF-8 string ":memory:", which is fine.
        let path_str = path.to_str().ok_or_else(|| {
            anyhow::anyhow!("provenance database path is not valid UTF-8: {path:?}")
        })?;
        // The directory is this function's job, not the caller's.
        //
        // Three of fifteen call sites created it; the rest did not, so on a
        // fresh install with no `~/.prism` those three worked and the others
        // failed to open — and the agent's copies degrade that failure to a
        // warn!, so the visible symptom is provenance quietly not being
        // recorded. Every ingest test pre-creates the directory, which is
        // exactly why the suite never showed it: the guarantee lived in test
        // setup rather than in the code under test.
        //
        // `:memory:` has no parent to make, and a bare relative filename
        // yields an empty parent — skip both rather than calling create_dir_all("").
        if path_str != ":memory:"
            && let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create the provenance store directory {}",
                    parent.display()
                )
            })?;
        }
        let _open_guard = STORE_OPEN_LOCK.lock().await;
        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .context("failed to open Turso database")?;
        let conn = db.connect()?;
        // Keep local provenance portable on Lustre/GPFS: WAL requires
        // cross-client shared-memory coordination and creates -wal/-shm
        // sidecars that are not reliable on parallel filesystems.
        let mut journal_mode = conn.query("PRAGMA journal_mode=DELETE", ()).await?;
        while journal_mode.next().await?.is_some() {}

        // Wait for a competing writer instead of failing the open.
        //
        // Nothing holds this store: the agent loop opens it once per turn and
        // hooks open it per tool call in a spawned task, so two writers
        // colliding is ordinary operation, not an edge case. With no busy
        // timeout the loser errors immediately, and the callers that swallow
        // that error drop the record — one at `agent_loop.rs` with no log at
        // all. Five seconds is long enough to outlast any single write here
        // and short enough that a genuinely stuck lock still surfaces.
        let mut busy = conn.query("PRAGMA busy_timeout=5000", ()).await?;
        while busy.next().await?.is_some() {}

        // Enforce the store's declared foreign keys (provenance assertion
        // evidence and the rebuildable session search terms). Like SQLite,
        // Turso leaves foreign keys OFF unless each connection opts in, and
        // an unenforced FK is a lie in the schema. Set before `init_schema` so
        // migrations run under the same rules as ordinary writes (`ON UPDATE
        // CASCADE` keeps evidence rows attached across assertion id re-keys).
        let mut foreign_keys = conn.query("PRAGMA foreign_keys=ON", ()).await?;
        while foreign_keys.next().await?.is_some() {}

        Self::init_schema(&conn).await?;
        Ok(Self {
            conn,
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    async fn init_schema(conn: &turso::Connection) -> Result<()> {
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS provenance_records (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                session_id TEXT NOT NULL,
                action_type TEXT NOT NULL,
                actor TEXT NOT NULL,
                tool_name TEXT,
                llm_model TEXT,
                input_json TEXT NOT NULL,
                output_json TEXT,
                parent_id TEXT,
                material_ref TEXT,
                confidence REAL DEFAULT 0,
                tags TEXT,
                status TEXT,
                exit_code INTEGER
            )"#,
            (),
        )
        .await?;

        // VS1/F5 migration: add `status` + `exit_code` to pre-existing user
        // databases. CREATE TABLE IF NOT EXISTS will NOT add columns to a
        // table that already exists, so an older ~/.prism/provenance.db would
        // be missing these columns and the INSERT below would fail at runtime.
        // Each ALTER is guarded: re-running on an already-migrated DB raises
        // "duplicate column name", which we swallow. Both fresh and legacy
        // DBs converge on the same 15-column shape.
        add_column_if_absent(conn, "provenance_records", "status", "TEXT").await?;
        add_column_if_absent(conn, "provenance_records", "exit_code", "INTEGER").await?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_prov_session ON provenance_records(session_id)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_prov_material ON provenance_records(material_ref)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_prov_parent ON provenance_records(parent_id)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_prov_action ON provenance_records(action_type)",
            (),
        )
        .await?;

        // Durable lifecycle ledger for top-level and delegated agent turns.
        // There is deliberately no FK on parent_run_id: if a best-effort parent
        // write fails, retaining the child's claimed edge is more useful than
        // rejecting the child row as well.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS agent_runs (
                id TEXT PRIMARY KEY,
                parent_run_id TEXT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                label TEXT NOT NULL,
                status TEXT NOT NULL CHECK (
                    status IN ('running', 'completed', 'failed', 'cancelled')
                ),
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                ended_at TEXT,
                tokens_in INTEGER NOT NULL DEFAULT 0 CHECK (tokens_in >= 0),
                tokens_out INTEGER NOT NULL DEFAULT 0 CHECK (tokens_out >= 0),
                cost_usd REAL NOT NULL DEFAULT 0 CHECK (cost_usd >= 0),
                last_error TEXT
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_agent_runs_session_started \
             ON agent_runs(session_id, started_at DESC)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_agent_runs_status_updated \
             ON agent_runs(status, updated_at)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_agent_runs_parent_started \
             ON agent_runs(parent_run_id, started_at)",
            (),
        )
        .await?;

        // Rebuildable mirror of agent session metadata. Session ids are only
        // unique within a source directory: tests and alternate profiles may
        // legitimately point at independent session collections.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS session_metadata (
                source_path TEXT NOT NULL,
                session_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                project_cwd TEXT,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                model TEXT NOT NULL,
                turn_count INTEGER NOT NULL CHECK (turn_count >= 0),
                compaction_count INTEGER NOT NULL CHECK (compaction_count >= 0),
                parent_session_id TEXT,
                branch_name TEXT,
                title TEXT,
                summary TEXT,
                title_source TEXT,
                title_turn INTEGER NOT NULL CHECK (title_turn >= 0),
                models TEXT NOT NULL,
                preview TEXT,
                size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
                indexed_at REAL NOT NULL,
                PRIMARY KEY (source_path, session_id)
            )"#,
            (),
        )
        .await?;
        // Early development builds created this disposable table before the
        // observation boundary was added. Keep those local mirrors openable;
        // rows with the default are safely eligible for the next rebuild.
        add_column_if_absent(
            conn,
            "session_metadata",
            "indexed_at",
            "REAL NOT NULL DEFAULT 0",
        )
        .await?;
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS session_metadata_reconciliation (
                source_path TEXT PRIMARY KEY,
                reconciled_at REAL NOT NULL
            )"#,
            (),
        )
        .await?;
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS session_search_terms (
                source_path TEXT NOT NULL,
                session_id TEXT NOT NULL,
                term TEXT NOT NULL,
                PRIMARY KEY (source_path, session_id, term),
                FOREIGN KEY (source_path, session_id)
                    REFERENCES session_metadata(source_path, session_id)
                    ON DELETE CASCADE
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_updated_at \
             ON session_metadata(updated_at DESC)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_source_updated \
             ON session_metadata(source_path, updated_at DESC)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_project_updated \
             ON session_metadata(project_cwd, updated_at DESC)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_parent \
             ON session_metadata(parent_session_id)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_metadata_source_indexed \
             ON session_metadata(source_path, indexed_at)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_search_terms_term_session \
             ON session_search_terms(term, session_id, source_path)",
            (),
        )
        .await?;

        // Semantic memory: one vector per record (little-endian f32 blob),
        // written lazily by `embed_and_store` — never on the `record()` path.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS provenance_embeddings (
                record_id TEXT PRIMARY KEY,
                model TEXT,
                dim INTEGER,
                vector BLOB
            )"#,
            (),
        )
        .await?;

        // ── Repair queue and disposition ledger ──────────────────────────
        //
        // A refused fact is not thrown away: it is queued, and Phase 2 works
        // the queue AFTER the graph is built. Two tables because they answer
        // different questions — `repair_queue` is current state (what is
        // still owed), `repair_disposition` is an append-only ledger (what
        // was decided, by whom, on what evidence). Deleting a queue row must
        // never erase the record that it was judged.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS repair_queue (
                item_id TEXT PRIMARY KEY,
                document TEXT NOT NULL,
                tenant TEXT NOT NULL,
                class TEXT NOT NULL,
                subject_json TEXT NOT NULL,
                detail TEXT NOT NULL,
                enqueued_at REAL NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0)
            )"#,
            (),
        )
        .await?;
        // Append-only: one row per decision, never updated in place. The
        // `attempt` column makes a second decision on the same item a new
        // row rather than an overwrite, so the history of a repair is
        // readable rather than replaced.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS repair_disposition (
                item_id TEXT NOT NULL,
                attempt INTEGER NOT NULL CHECK (attempt >= 0),
                document TEXT NOT NULL,
                class TEXT NOT NULL,
                outcome TEXT NOT NULL CHECK (outcome IN ('accept', 'withdraw')),
                corrected_json TEXT,
                evidence TEXT,
                reason TEXT NOT NULL,
                dispositioner TEXT NOT NULL,
                decided_at REAL NOT NULL,
                PRIMARY KEY (item_id, attempt)
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_repair_queue_document \
             ON repair_queue(document, enqueued_at)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_repair_disposition_document \
             ON repair_disposition(document, decided_at)",
            (),
        )
        .await?;

        // ── Ontology extension proposal queue and disposition ledger ─────
        //
        // Same shape as the repair queue above, answering the same two
        // questions: `ontology_proposal_queue` is current state (what is
        // still awaiting governance), `ontology_proposal_disposition` is an
        // append-only ledger (what was decided, by whom, when). Population
        // proposes; governance disposes — extraction NEVER mutates the
        // active ontology, and these tables are the durable record that
        // lets a later deliberate act do so.
        //
        // A third table, `ontology_proposal_sighting`, holds the CITATIONS.
        // A proposal's identity is its content (kind, label, parents or
        // endpoints) — the same concept proposed from two papers is ONE
        // proposal, and its evidence ACCUMULATES exactly the way a
        // `prov_assertion` accumulates `prov_assertion_evidence`
        // contributions. A proposal without its citation is worthless for
        // governance, so the sighting rows are the review surface's evidence
        // view, never an afterthought.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS ontology_proposal_queue (
                item_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL CHECK (kind IN ('class', 'relation')),
                label TEXT NOT NULL,
                document TEXT NOT NULL,
                tenant TEXT NOT NULL,
                proposal_json TEXT NOT NULL,
                enqueued_at REAL NOT NULL
            )"#,
            (),
        )
        .await?;
        // Append-only: one row per (proposal, document, citation) sighting.
        // Re-ingesting the same document does not duplicate a citation; a
        // DIFFERENT document proposing the same identity adds evidence.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS ontology_proposal_sighting (
                item_id TEXT NOT NULL,
                document TEXT NOT NULL,
                citation_json TEXT NOT NULL,
                sighted_at REAL NOT NULL,
                PRIMARY KEY (item_id, document, citation_json)
            )"#,
            (),
        )
        .await?;
        // Append-only disposition ledger, mirroring repair_disposition:
        // one row per decision, never updated in place. `outcome` is
        // 'accepted' or 'rejected'. A rejected identity STAYS rejected —
        // enqueue refuses to re-queue any identity with a recorded
        // disposition, so a concept cannot be re-proposed forever.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS ontology_proposal_disposition (
                item_id TEXT NOT NULL,
                document TEXT NOT NULL,
                kind TEXT NOT NULL,
                label TEXT NOT NULL,
                outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'rejected')),
                artifact_path TEXT,
                reason TEXT NOT NULL,
                dispositioner TEXT NOT NULL,
                decided_at REAL NOT NULL,
                PRIMARY KEY (item_id, decided_at)
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ontology_proposal_queue_enqueued \
             ON ontology_proposal_queue(enqueued_at, item_id)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ontology_proposal_sighting_item \
             ON ontology_proposal_sighting(item_id)",
            (),
        )
        .await?;

        // ── Re-verification verdict ledger ─────────────────────────────
        //
        // The audit trail of retrieval re-reading: what a model affirmed,
        // denied, or could not assess when shown the EXACT cited lines of a
        // stored assertion, per source witness. Append-only, mirroring
        // `repair_disposition` — verdicts never rewrite the assertion's
        // verification status (see `ReverifyVerdict`). No queue table:
        // there is nothing to dequeue, because a re-check renders no
        // store-mutating decision.
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS reverify_verdict (
                assertion_id TEXT NOT NULL,
                source_key TEXT NOT NULL,
                verdict TEXT NOT NULL CHECK (verdict IN ('affirmed', 'denied', 'uncertain', 'not_ready')),
                reason TEXT NOT NULL,
                reviewer TEXT NOT NULL,
                decided_at REAL NOT NULL,
                PRIMARY KEY (assertion_id, source_key, decided_at)
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_reverify_verdict_assertion \
             ON reverify_verdict(assertion_id, decided_at)",
            (),
        )
        .await?;

        // EMMO materials ontology + PROV-O assertion tables (same store).
        emmo::init_schema(conn).await?;

        Ok(())
    }

    pub async fn record(&self, rec: &ProvenanceRecord) -> Result<()> {
        let tags_json = serde_json::to_string(&rec.tags)?;
        let output_json = rec
            .output_json
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        // Under the shared write lock: on the one shared connection an
        // unlocked INSERT silently joins whatever raw transaction another
        // task has open, and that transaction's rollback erases this record
        // AFTER the caller was told `Ok(())` (see `write_lock`).
        let _same_handle_guard = self.write_lock.lock().await;
        self.conn
            .execute(
                r#"INSERT INTO provenance_records
                   (id, timestamp, session_id, action_type, actor,
                    tool_name, llm_model, input_json, output_json,
                    parent_id, material_ref, confidence, tags, status, exit_code)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"#,
                [
                    Value::Text(rec.id.clone()),
                    Value::Text(rec.timestamp.clone()),
                    Value::Text(rec.session_id.clone()),
                    Value::Text(rec.action_type.as_str().to_string()),
                    Value::Text(rec.actor.as_str().to_string()),
                    opt_to_value(&rec.tool_name),
                    opt_to_value(&rec.llm_model),
                    Value::Text(serde_json::to_string(&rec.input_json)?),
                    match &output_json {
                        Some(s) => Value::Text(s.clone()),
                        None => Value::Null,
                    },
                    opt_to_value(&rec.parent_id),
                    opt_to_value(&rec.material_ref),
                    Value::Real(rec.confidence),
                    Value::Text(tags_json),
                    opt_to_value(&rec.status),
                    match rec.exit_code {
                        Some(c) => Value::Integer(c),
                        None => Value::Null,
                    },
                ],
            )
            .await?;

        Ok(())
    }

    /// Insert the initial `running` row for an agent turn.
    pub async fn start_agent_run(&self, run: &AgentRun) -> Result<()> {
        anyhow::ensure!(
            run.status == AgentRunStatus::Running,
            "a new agent run must start in running state"
        );
        anyhow::ensure!(
            run.ended_at.is_none(),
            "a new agent run cannot already have ended_at"
        );
        anyhow::ensure!(
            run.tokens_in == 0 && run.tokens_out == 0 && run.cost_usd == 0.0,
            "a new agent run must start with zero usage and cost"
        );

        let _same_handle_guard = self.write_lock.lock().await;
        let changed = self
            .conn
            .execute(
                r#"INSERT INTO agent_runs
                   (id, parent_run_id, session_id, role, label, status,
                    started_at, updated_at, ended_at, tokens_in, tokens_out,
                    cost_usd, last_error)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
                [
                    Value::Text(run.id.clone()),
                    opt_to_value(&run.parent_run_id),
                    Value::Text(run.session_id.clone()),
                    Value::Text(run.role.clone()),
                    Value::Text(run.label.clone()),
                    Value::Text(run.status.as_str().to_string()),
                    Value::Text(run.started_at.clone()),
                    Value::Text(run.updated_at.clone()),
                    Value::Null,
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Real(0.0),
                    Value::Null,
                ],
            )
            .await?;
        anyhow::ensure!(changed == 1, "agent run start did not insert a row");
        Ok(())
    }

    /// Refresh the heartbeat of a running row.
    pub async fn heartbeat_agent_run(&self, run_id: &str) -> Result<()> {
        let updated_at = Utc::now().to_rfc3339();
        let _same_handle_guard = self.write_lock.lock().await;
        let changed = self
            .conn
            .execute(
                "UPDATE agent_runs SET updated_at = ?1 \
                 WHERE id = ?2 AND status = 'running'",
                [Value::Text(updated_at), Value::Text(run_id.to_string())],
            )
            .await?;
        anyhow::ensure!(
            changed == 1,
            "agent run heartbeat did not update one running row"
        );
        Ok(())
    }

    /// Close a running row with its final usage, cost, and optional error.
    pub async fn finish_agent_run(
        &self,
        run_id: &str,
        status: AgentRunStatus,
        tokens_in: u64,
        tokens_out: u64,
        cost_usd: f64,
        last_error: Option<&str>,
    ) -> Result<()> {
        anyhow::ensure!(
            status != AgentRunStatus::Running,
            "finishing an agent run requires a terminal status"
        );
        anyhow::ensure!(
            cost_usd.is_finite() && cost_usd >= 0.0,
            "agent run cost must be finite and non-negative"
        );
        let tokens_in = i64::try_from(tokens_in).context("agent input token count exceeds i64")?;
        let tokens_out =
            i64::try_from(tokens_out).context("agent output token count exceeds i64")?;
        let ended_at = Utc::now().to_rfc3339();
        let last_error = last_error.map(str::to_string);

        let _same_handle_guard = self.write_lock.lock().await;
        let changed = self
            .conn
            .execute(
                r#"UPDATE agent_runs
                   SET status = ?1, updated_at = ?2, ended_at = ?2,
                       tokens_in = ?3, tokens_out = ?4, cost_usd = ?5,
                       last_error = ?6
                   WHERE id = ?7 AND status = 'running'"#,
                [
                    Value::Text(status.as_str().to_string()),
                    Value::Text(ended_at),
                    Value::Integer(tokens_in),
                    Value::Integer(tokens_out),
                    Value::Real(cost_usd),
                    match last_error {
                        Some(error) => Value::Text(error),
                        None => Value::Null,
                    },
                    Value::Text(run_id.to_string()),
                ],
            )
            .await?;
        anyhow::ensure!(
            changed == 1,
            "agent run finish did not update one running row"
        );
        Ok(())
    }

    /// List agent runs newest first, optionally filtered by session, status,
    /// and direct parent.
    pub async fn list_agent_runs(&self, filter: &AgentRunFilter) -> Result<Vec<AgentRun>> {
        let mut clauses = Vec::new();
        let mut params = Vec::new();

        if let Some(session_id) = &filter.session_id {
            params.push(Value::Text(session_id.clone()));
            clauses.push(format!("session_id = ?{}", params.len()));
        }
        if let Some(status) = filter.status {
            params.push(Value::Text(status.as_str().to_string()));
            clauses.push(format!("status = ?{}", params.len()));
        }
        if let Some(parent_run_id) = &filter.parent_run_id {
            params.push(Value::Text(parent_run_id.clone()));
            clauses.push(format!("parent_run_id = ?{}", params.len()));
        }

        let mut sql = format!("SELECT {AGENT_RUN_COLUMNS} FROM agent_runs");
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        params.push(Value::Integer(
            filter.limit.min(MAX_AGENT_RUN_QUERY_LIMIT) as i64
        ));
        sql.push_str(&format!(
            " ORDER BY started_at DESC LIMIT ?{}",
            params.len()
        ));

        let mut rows = self.conn.query(&sql, params).await?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next().await? {
            runs.push(row_to_agent_run(&row)?);
        }
        Ok(runs)
    }

    /// List every run transitively spawned below `root_run_id`.
    ///
    /// This follows only [`AgentRun::parent_run_id`] spawn edges. It does not
    /// use session threading or [`ProvenanceRecord::parent_id`], and the root
    /// row itself need not exist because orphaned child edges are intentionally
    /// retained by the ledger. Results exclude the root and are stable by
    /// breadth-first depth, with every parent's children ordered by run id.
    ///
    /// The visited set and depth policy independently stop cycles, while a
    /// one-row probe makes row-budget truncation observable in
    /// [`AgentRunTraversalOutcome`]. The whole walk uses one deferred read
    /// transaction, so `Complete` describes a single stable database snapshot.
    pub async fn list_agent_run_descendants(
        &self,
        root_run_id: &str,
        policy: &AgentRunTraversalPolicy,
    ) -> Result<AgentRunDescendants> {
        let _same_handle_guard = self.write_lock.lock().await;
        let txn = turso::transaction::Transaction::new_unchecked(
            &self.conn,
            turso::transaction::TransactionBehavior::Deferred,
        )
        .await
        .context("failed to begin agent run traversal snapshot")?;

        let result: Result<AgentRunDescendants> = async {
            let mut runs = Vec::new();
            let mut observed_rows = 0usize;
            let mut depth_limit_reached = false;
            let mut row_limit_reached = false;
            let mut repeated_run_ids = std::collections::BTreeSet::new();
            let mut visited = std::collections::BTreeSet::from([root_run_id.to_string()]);
            let mut pending = std::collections::VecDeque::from([(root_run_id.to_string(), 0usize)]);

            // SQLite supports recursive CTEs, but the Turso 0.7 parser used by
            // this crate rejects `WITH RECURSIVE` with "Recursive CTEs are not
            // yet supported". Execute the same recurrence breadth-first in
            // Rust until Turso exposes it, retaining independent visited,
            // depth, and row guards. Each expansion still uses the parent
            // index and a bound query.
            'traversal: while let Some((parent_run_id, parent_depth)) = pending.pop_front() {
                let child_depth = parent_depth
                    .checked_add(1)
                    .context("agent run traversal depth overflowed usize")?;
                if child_depth > policy.max_depth {
                    let mut rows = txn
                        .query(
                            "SELECT EXISTS(\
                                 SELECT 1 FROM agent_runs WHERE parent_run_id = ?1\
                             )",
                            [Value::Text(parent_run_id)],
                        )
                        .await?;
                    let row = rows
                        .next()
                        .await?
                        .context("agent run depth probe returned no row")?;
                    depth_limit_reached |= get_agent_run_traversal_bool(&row, 0, "depth probe")?;
                    continue;
                }

                let probe_rows = policy
                    .max_rows
                    .saturating_sub(observed_rows)
                    .checked_add(1)
                    .context("agent run traversal max_rows cannot be probed safely")?;
                let probe_rows = i64::try_from(probe_rows)
                    .context("agent run traversal max_rows exceeds SQLite integer range")?;
                let sql = format!(
                    "SELECT {AGENT_RUN_COLUMNS} FROM agent_runs \
                     WHERE parent_run_id = ?1 ORDER BY id ASC LIMIT ?2"
                );
                let mut rows = txn
                    .query(
                        &sql,
                        [Value::Text(parent_run_id), Value::Integer(probe_rows)],
                    )
                    .await?;
                while let Some(row) = rows.next().await? {
                    observed_rows += 1;
                    let run_id = get_str(&row, 0)?;
                    let is_cycle = visited.contains(&run_id);
                    if is_cycle {
                        repeated_run_ids.insert(run_id.clone());
                    }
                    if observed_rows > policy.max_rows {
                        row_limit_reached = true;
                        break 'traversal;
                    }
                    if is_cycle {
                        continue;
                    }

                    let run = row_to_agent_run(&row)?;
                    visited.insert(run_id.clone());
                    pending.push_back((run_id, child_depth));
                    runs.push(run);
                }
            }

            let mut issues = Vec::new();
            if depth_limit_reached {
                issues.push(AgentRunTraversalIssue::DepthLimitReached {
                    max_depth: policy.max_depth,
                });
            }
            if row_limit_reached {
                issues.push(AgentRunTraversalIssue::RowLimitReached {
                    max_rows: policy.max_rows,
                });
            }
            issues.extend(
                repeated_run_ids.into_iter().map(|repeated_run_id| {
                    AgentRunTraversalIssue::CycleDetected { repeated_run_id }
                }),
            );

            let outcome = if issues.is_empty() {
                AgentRunTraversalOutcome::Complete
            } else {
                AgentRunTraversalOutcome::Incomplete { issues }
            };
            Ok(AgentRunDescendants { runs, outcome })
        }
        .await;

        match result {
            Ok(descendants) => {
                txn.commit()
                    .await
                    .context("failed to close agent run traversal snapshot")?;
                Ok(descendants)
            }
            Err(error) => {
                let _ = txn.rollback().await;
                Err(error)
            }
        }
    }

    /// Return running rows whose last heartbeat is older than `policy` permits.
    pub async fn stale_running_agent_runs(
        &self,
        policy: &AgentRunStalenessPolicy,
    ) -> Result<Vec<AgentRun>> {
        let stale_after = ChronoDuration::from_std(policy.stale_after)
            .context("agent run staleness threshold exceeds chrono range")?;
        let cutoff = Utc::now()
            .checked_sub_signed(stale_after)
            .context("agent run staleness cutoff is outside the timestamp range")?
            .to_rfc3339();
        let limit = policy.max_results.min(MAX_AGENT_RUN_QUERY_LIMIT) as i64;
        let mut rows = self
            .conn
            .query(
                &format!(
                    "SELECT {AGENT_RUN_COLUMNS} FROM agent_runs \
                     WHERE status = 'running' AND updated_at < ?1 \
                     ORDER BY updated_at ASC LIMIT ?2"
                ),
                [Value::Text(cutoff), Value::Integer(limit)],
            )
            .await?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next().await? {
            runs.push(row_to_agent_run(&row)?);
        }
        Ok(runs)
    }

    /// Insert or refresh one row in the rebuildable session metadata mirror.
    ///
    /// The metadata row and its normalized search terms are replaced in one
    /// transaction. This incremental path intentionally does not advance the
    /// source reconciliation timestamp; only a complete source rebuild can do
    /// that honestly.
    pub async fn upsert_session_metadata(&self, entry: &SessionIndexEntry) -> Result<()> {
        validate_session_index_entry(entry, None)?;
        let indexed_at = session_index_timestamp();
        let _same_handle_guard = self.write_lock.lock().await;
        let txn = begin_session_index_txn(&self.conn).await?;
        let result = self
            .upsert_session_metadata_in_open_txn(entry, indexed_at)
            .await;
        finish_session_index_txn(txn, result).await
    }

    /// Atomically replace every mirrored row for `source_path` and record the
    /// time at which that full filesystem reconciliation completed.
    ///
    /// All entries are validated before the existing source is purged, so a
    /// mixed-source or otherwise malformed rebuild cannot destroy a good
    /// mirror. Rows incrementally observed after `scan_started_at` are kept,
    /// preventing a concurrent write-through from being erased by a stale
    /// filesystem snapshot. An empty slice is a valid reconciliation of an
    /// empty source.
    pub async fn replace_session_metadata_source(
        &self,
        source_path: &str,
        entries: &[SessionIndexEntry],
        preserve_session_ids: &[String],
        scan_started_at: f64,
        reconciled_at: f64,
    ) -> Result<()> {
        validate_session_source_path(source_path)?;
        anyhow::ensure!(
            scan_started_at.is_finite() && reconciled_at.is_finite(),
            "session metadata reconciliation timestamps must be finite"
        );
        anyhow::ensure!(
            reconciled_at >= scan_started_at,
            "session metadata reconciled_at precedes scan_started_at"
        );
        let mut session_ids = std::collections::HashSet::new();
        for entry in entries {
            validate_session_index_entry(entry, Some(source_path))?;
            anyhow::ensure!(
                session_ids.insert(entry.session_id.as_str()),
                "duplicate session_id {:?} in source replacement",
                entry.session_id
            );
        }
        let mut preserved_ids = std::collections::HashSet::new();
        for session_id in preserve_session_ids {
            anyhow::ensure!(
                !session_id.trim().is_empty(),
                "preserved session_id must not be empty"
            );
            anyhow::ensure!(
                preserved_ids.insert(session_id.as_str()),
                "duplicate preserved session_id {session_id:?}"
            );
            anyhow::ensure!(
                !session_ids.contains(session_id.as_str()),
                "session_id {session_id:?} is both rebuilt and preserved"
            );
        }

        let _same_handle_guard = self.write_lock.lock().await;
        let txn = begin_session_index_txn(&self.conn).await?;
        let result: Result<()> = async {
            // An unreadable or actively-written JSONL candidate is
            // indeterminate, not absent. Keep its last good row and advance
            // the observation boundary so the source-wide delete skips it.
            for session_id in preserve_session_ids {
                self.conn
                    .execute(
                        "UPDATE session_metadata SET indexed_at = \
                         CASE WHEN indexed_at < ?1 THEN ?1 ELSE indexed_at END \
                         WHERE source_path = ?2 AND session_id = ?3",
                        [
                            Value::Real(reconciled_at),
                            Value::Text(source_path.to_string()),
                            Value::Text(session_id.clone()),
                        ],
                    )
                    .await?;
            }
            self.conn
                .execute(
                    "DELETE FROM session_search_terms \
                     WHERE source_path = ?1 AND session_id IN (\
                         SELECT session_id FROM session_metadata \
                         WHERE source_path = ?1 AND indexed_at < ?2\
                     )",
                    [
                        Value::Text(source_path.to_string()),
                        Value::Real(scan_started_at),
                    ],
                )
                .await?;
            self.conn
                .execute(
                    "DELETE FROM session_metadata \
                     WHERE source_path = ?1 AND indexed_at < ?2",
                    [
                        Value::Text(source_path.to_string()),
                        Value::Real(scan_started_at),
                    ],
                )
                .await?;
            for entry in entries {
                self.upsert_session_metadata_in_open_txn(entry, scan_started_at)
                    .await?;
            }
            self.conn
                .execute(
                    r#"INSERT INTO session_metadata_reconciliation
                       (source_path, reconciled_at) VALUES (?1, ?2)
                       ON CONFLICT(source_path) DO UPDATE SET
                           reconciled_at = excluded.reconciled_at"#,
                    [
                        Value::Text(source_path.to_string()),
                        Value::Real(reconciled_at),
                    ],
                )
                .await?;
            Ok(())
        }
        .await;
        finish_session_index_txn(txn, result).await
    }

    // ── Repair queue ────────────────────────────────────────────────────
    //
    // Phase 1 builds the graph; Phase 2 works these. Enqueue is idempotent
    // on `item_id`, so re-ingesting the same document does not duplicate an
    // item that was already judged.

    /// Queue one refused fact for later repair.
    ///
    /// `item_id` must identify the refusal (the fact's identity plus its
    /// class), NOT the run — the same refusal seen twice is one item.
    /// Re-enqueueing an existing item leaves its attempt count alone.
    pub async fn enqueue_repair(&self, item: &RepairItem) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT INTO repair_queue
                   (item_id, document, tenant, class, subject_json, detail,
                    enqueued_at, attempts)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
                   ON CONFLICT(item_id) DO NOTHING"#,
                vec![
                    Value::Text(item.item_id.clone()),
                    Value::Text(item.document.clone()),
                    Value::Text(item.tenant.clone()),
                    Value::Text(item.class.clone()),
                    Value::Text(item.subject_json.clone()),
                    Value::Text(item.detail.clone()),
                    Value::Real(item.enqueued_at),
                ],
            )
            .await?;
        Ok(())
    }

    /// Items still owed for a document, oldest first.
    pub async fn pending_repairs(&self, document: &str, limit: i64) -> Result<Vec<RepairItem>> {
        let mut rows = self
            .conn
            .query(
                "SELECT item_id, document, tenant, class, subject_json, detail, \
                 enqueued_at, attempts FROM repair_queue \
                 WHERE document = ?1 ORDER BY enqueued_at, item_id LIMIT ?2",
                vec![Value::Text(document.to_string()), Value::Integer(limit)],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(RepairItem {
                item_id: get_str(&row, 0)?,
                document: get_str(&row, 1)?,
                tenant: get_str(&row, 2)?,
                class: get_str(&row, 3)?,
                subject_json: get_str(&row, 4)?,
                detail: get_str(&row, 5)?,
                enqueued_at: row
                    .get_value(6)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or_default(),
                attempts: row
                    .get_value(7)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Count one FAILED attempt against an item that stays queued.
    ///
    /// The model tier gets one attempt per item per run — never a retry
    /// loop — so a failed attempt is recorded here instead of being
    /// re-tried in place. When the count reaches the worker's declared
    /// attempt limit, the worker's next failure becomes a final WITHDRAW
    /// ledger row instead of another bump.
    pub async fn bump_repair_attempts(&self, item_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE repair_queue SET attempts = attempts + 1 WHERE item_id = ?1",
                vec![Value::Text(item_id.to_string())],
            )
            .await?;
        Ok(())
    }

    /// Record a decision and remove the item from the queue, atomically in
    /// intent: the ledger row is written FIRST, so a crash between the two
    /// leaves a judged item still queued (it will be re-judged and produce a
    /// second attempt row) rather than an item silently dropped with no
    /// record of why.
    pub async fn record_repair_disposition(&self, d: &RepairDisposition) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT INTO repair_disposition
                   (item_id, attempt, document, class, outcome, corrected_json,
                    evidence, reason, dispositioner, decided_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                   ON CONFLICT(item_id, attempt) DO NOTHING"#,
                vec![
                    Value::Text(d.item_id.clone()),
                    Value::Integer(d.attempt),
                    Value::Text(d.document.clone()),
                    Value::Text(d.class.clone()),
                    Value::Text(d.outcome.clone()),
                    d.corrected_json.clone().map_or(Value::Null, Value::Text),
                    d.evidence.clone().map_or(Value::Null, Value::Text),
                    Value::Text(d.reason.clone()),
                    Value::Text(d.dispositioner.clone()),
                    Value::Real(d.decided_at),
                ],
            )
            .await?;
        self.conn
            .execute(
                "DELETE FROM repair_queue WHERE item_id = ?1",
                vec![Value::Text(d.item_id.clone())],
            )
            .await?;
        Ok(())
    }

    /// Every decision recorded for a document, oldest first. The audit trail:
    /// what was decided, by whom, on what evidence.
    pub async fn repair_dispositions(&self, document: &str) -> Result<Vec<RepairDisposition>> {
        let mut rows = self
            .conn
            .query(
                "SELECT item_id, attempt, document, class, outcome, corrected_json, \
                 evidence, reason, dispositioner, decided_at FROM repair_disposition \
                 WHERE document = ?1 ORDER BY decided_at, item_id",
                vec![Value::Text(document.to_string())],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(RepairDisposition {
                item_id: get_str(&row, 0)?,
                attempt: row
                    .get_value(1)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .unwrap_or_default(),
                document: get_str(&row, 2)?,
                class: get_str(&row, 3)?,
                outcome: get_str(&row, 4)?,
                corrected_json: get_opt_str(&row, 5)?,
                evidence: get_opt_str(&row, 6)?,
                reason: get_str(&row, 7)?,
                dispositioner: get_str(&row, 8)?,
                decided_at: row
                    .get_value(9)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    // ── Re-verification verdicts ─────────────────────────────────────

    /// Append one re-verification verdict to the ledger. Append-only by
    /// construction: a second run over the same witness is a new row at a
    /// new `decided_at`, never an overwrite.
    pub async fn record_reverify_verdict(&self, v: &ReverifyVerdict) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT INTO reverify_verdict
                   (assertion_id, source_key, verdict, reason, reviewer, decided_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
                vec![
                    Value::Text(v.assertion_id.clone()),
                    Value::Text(v.source_key.clone()),
                    Value::Text(v.verdict.clone()),
                    Value::Text(v.reason.clone()),
                    Value::Text(v.reviewer.clone()),
                    Value::Real(v.decided_at),
                ],
            )
            .await?;
        Ok(())
    }

    /// Every verdict recorded for one assertion, oldest first — the audit
    /// trail a reviewer reads before (and after) re-checking.
    pub async fn reverify_verdicts(&self, assertion_id: &str) -> Result<Vec<ReverifyVerdict>> {
        let mut rows = self
            .conn
            .query(
                "SELECT assertion_id, source_key, verdict, reason, reviewer, decided_at \
                 FROM reverify_verdict WHERE assertion_id = ?1 \
                 ORDER BY decided_at, source_key",
                vec![Value::Text(assertion_id.to_string())],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(ReverifyVerdict {
                assertion_id: get_str(&row, 0)?,
                source_key: get_str(&row, 1)?,
                verdict: get_str(&row, 2)?,
                reason: get_str(&row, 3)?,
                reviewer: get_str(&row, 4)?,
                decided_at: row
                    .get_value(5)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    // ── Ontology extension proposals ───────────────────────────────────

    /// Queue one ontology extension proposal with its citation.
    ///
    /// Idempotent on `item_id` (the proposal's CONTENT identity, not the
    /// run): re-ingesting the same document adds no rows; a different
    /// document proposing the same identity adds a SIGHTING, not a second
    /// queue item. An identity with ANY recorded disposition is never
    /// re-queued — a rejected proposal must stay rejected, and an accepted
    /// one is already on the governance path — and that suppression is
    /// returned as [`OntologyProposalEnqueue::SupersededByDisposition`] so
    /// the caller can count it loudly instead of losing it.
    pub async fn enqueue_ontology_proposal(
        &self,
        item: &OntologyProposalItem,
        citation_json: &str,
        sighted_at: f64,
    ) -> Result<OntologyProposalEnqueue> {
        let _same_handle_guard = self.write_lock.lock().await;
        let mut rows = self
            .conn
            .query(
                "SELECT 1 FROM ontology_proposal_disposition WHERE item_id = ?1 LIMIT 1",
                vec![Value::Text(item.item_id.clone())],
            )
            .await?;
        let already_dispositioned = rows.next().await?.is_some();
        drop(rows);
        if already_dispositioned {
            return Ok(OntologyProposalEnqueue::SupersededByDisposition);
        }

        let newly_queued = self
            .conn
            .execute(
                r#"INSERT INTO ontology_proposal_queue
                   (item_id, kind, label, document, tenant, proposal_json, enqueued_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                   ON CONFLICT(item_id) DO NOTHING"#,
                vec![
                    Value::Text(item.item_id.clone()),
                    Value::Text(item.kind.clone()),
                    Value::Text(item.label.clone()),
                    Value::Text(item.document.clone()),
                    Value::Text(item.tenant.clone()),
                    Value::Text(item.proposal_json.clone()),
                    Value::Real(item.enqueued_at),
                ],
            )
            .await?
            == 1;

        let new_sighting = self
            .conn
            .execute(
                r#"INSERT INTO ontology_proposal_sighting
                   (item_id, document, citation_json, sighted_at)
                   VALUES (?1, ?2, ?3, ?4)
                   ON CONFLICT(item_id, document, citation_json) DO NOTHING"#,
                vec![
                    Value::Text(item.item_id.clone()),
                    Value::Text(item.document.clone()),
                    Value::Text(citation_json.to_string()),
                    Value::Real(sighted_at),
                ],
            )
            .await?
            == 1;

        Ok(if newly_queued {
            OntologyProposalEnqueue::Queued
        } else {
            OntologyProposalEnqueue::Existing { new_sighting }
        })
    }

    /// Pending proposals, oldest first, with the number of citations backing
    /// each. The review surface's work queue.
    pub async fn pending_ontology_proposals(
        &self,
        limit: i64,
    ) -> Result<Vec<(OntologyProposalItem, i64)>> {
        let mut rows = self
            .conn
            .query(
                "SELECT q.item_id, q.kind, q.label, q.document, q.tenant, q.proposal_json, \
                 q.enqueued_at, COUNT(s.item_id) AS sightings \
                 FROM ontology_proposal_queue q \
                 LEFT JOIN ontology_proposal_sighting s ON s.item_id = q.item_id \
                 GROUP BY q.item_id \
                 ORDER BY q.enqueued_at, q.item_id LIMIT ?1",
                vec![Value::Integer(limit)],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let sightings = row
                .get_value(7)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or_default();
            out.push((
                OntologyProposalItem {
                    item_id: get_str(&row, 0)?,
                    kind: get_str(&row, 1)?,
                    label: get_str(&row, 2)?,
                    document: get_str(&row, 3)?,
                    tenant: get_str(&row, 4)?,
                    proposal_json: get_str(&row, 5)?,
                    enqueued_at: row
                        .get_value(6)
                        .ok()
                        .and_then(|v| v.as_real().copied())
                        .unwrap_or_default(),
                },
                sightings,
            ));
        }
        Ok(out)
    }

    /// One pending proposal by id, for the show/accept/reject surface.
    pub async fn ontology_proposal_by_id(
        &self,
        item_id: &str,
    ) -> Result<Option<OntologyProposalItem>> {
        let mut rows = self
            .conn
            .query(
                "SELECT item_id, kind, label, document, tenant, proposal_json, enqueued_at \
                 FROM ontology_proposal_queue WHERE item_id = ?1",
                vec![Value::Text(item_id.to_string())],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(OntologyProposalItem {
            item_id: get_str(&row, 0)?,
            kind: get_str(&row, 1)?,
            label: get_str(&row, 2)?,
            document: get_str(&row, 3)?,
            tenant: get_str(&row, 4)?,
            proposal_json: get_str(&row, 5)?,
            enqueued_at: row
                .get_value(6)
                .ok()
                .and_then(|v| v.as_real().copied())
                .unwrap_or_default(),
        }))
    }

    /// Every citation recorded for one proposal identity, oldest first —
    /// including sightings of an already-dispositioned proposal: the
    /// evidence trail outlives the decision.
    pub async fn ontology_proposal_sightings(
        &self,
        item_id: &str,
    ) -> Result<Vec<OntologyProposalSighting>> {
        let mut rows = self
            .conn
            .query(
                "SELECT item_id, document, citation_json, sighted_at \
                 FROM ontology_proposal_sighting WHERE item_id = ?1 \
                 ORDER BY sighted_at, document, citation_json",
                vec![Value::Text(item_id.to_string())],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(OntologyProposalSighting {
                item_id: get_str(&row, 0)?,
                document: get_str(&row, 1)?,
                citation_json: get_str(&row, 2)?,
                sighted_at: row
                    .get_value(3)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Record a governance decision and remove the item from the queue,
    /// ledger-first exactly like [`Self::record_repair_disposition`]: a
    /// crash between the two statements leaves a decided item still queued
    /// (re-decidable, producing a second ledger row) rather than an item
    /// silently dropped with no record of why. Sightings are deliberately
    /// NOT deleted — they are the evidence trail of what was proposed
    /// where, and they outlive the decision.
    pub async fn record_ontology_proposal_disposition(
        &self,
        d: &OntologyProposalDisposition,
    ) -> Result<()> {
        let _same_handle_guard = self.write_lock.lock().await;
        self.conn
            .execute(
                r#"INSERT INTO ontology_proposal_disposition
                   (item_id, document, kind, label, outcome, artifact_path,
                    reason, dispositioner, decided_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                   ON CONFLICT(item_id, decided_at) DO NOTHING"#,
                vec![
                    Value::Text(d.item_id.clone()),
                    Value::Text(d.document.clone()),
                    Value::Text(d.kind.clone()),
                    Value::Text(d.label.clone()),
                    Value::Text(d.outcome.clone()),
                    d.artifact_path.clone().map_or(Value::Null, Value::Text),
                    Value::Text(d.reason.clone()),
                    Value::Text(d.dispositioner.clone()),
                    Value::Real(d.decided_at),
                ],
            )
            .await?;
        self.conn
            .execute(
                "DELETE FROM ontology_proposal_queue WHERE item_id = ?1",
                vec![Value::Text(d.item_id.clone())],
            )
            .await?;
        Ok(())
    }

    /// Every governance decision recorded for one proposal identity, oldest
    /// first. The audit trail: what was decided, by whom, on what artifact.
    pub async fn ontology_proposal_dispositions(
        &self,
        item_id: &str,
    ) -> Result<Vec<OntologyProposalDisposition>> {
        let mut rows = self
            .conn
            .query(
                "SELECT item_id, document, kind, label, outcome, artifact_path, \
                 reason, dispositioner, decided_at FROM ontology_proposal_disposition \
                 WHERE item_id = ?1 ORDER BY decided_at",
                vec![Value::Text(item_id.to_string())],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(OntologyProposalDisposition {
                item_id: get_str(&row, 0)?,
                document: get_str(&row, 1)?,
                kind: get_str(&row, 2)?,
                label: get_str(&row, 3)?,
                outcome: get_str(&row, 4)?,
                artifact_path: get_opt_str(&row, 5)?,
                reason: get_str(&row, 6)?,
                dispositioner: get_str(&row, 7)?,
                decided_at: row
                    .get_value(8)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Return the last successful full-rebuild timestamp for a source.
    pub async fn session_metadata_reconciled_at(&self, source_path: &str) -> Result<Option<f64>> {
        validate_session_source_path(source_path)?;
        let _same_handle_guard = self.write_lock.lock().await;
        let mut rows = self
            .conn
            .query(
                "SELECT reconciled_at FROM session_metadata_reconciliation \
                 WHERE source_path = ?1",
                [Value::Text(source_path.to_string())],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(get_f64(&row, 0, "reconciled_at")?)),
            None => Ok(None),
        }
    }

    /// List mirrored sessions newest first, with indexed project/source/time
    /// filters and exact-token AND search over title, summary, and preview.
    pub async fn list_session_metadata(
        &self,
        query: &SessionIndexQuery,
    ) -> Result<Vec<SessionIndexEntry>> {
        if let Some(source_path) = &query.source_path {
            validate_session_source_path(source_path)?;
        }
        if let Some(updated_after) = query.updated_after {
            anyhow::ensure!(
                updated_after.is_finite(),
                "session metadata updated_after must be finite"
            );
        }
        if let Some(updated_before) = query.updated_before {
            anyhow::ensure!(
                updated_before.is_finite(),
                "session metadata updated_before must be finite"
            );
        }
        let search_terms = match &query.text {
            Some(text) => {
                anyhow::ensure!(
                    text.len() <= MAX_SESSION_INDEX_SEARCH_BYTES,
                    "session metadata search text exceeds {} bytes",
                    MAX_SESSION_INDEX_SEARCH_BYTES
                );
                let terms = normalized_session_terms(text);
                anyhow::ensure!(
                    terms.len() <= MAX_SESSION_INDEX_SEARCH_TERMS,
                    "session metadata search has more than {} distinct terms",
                    MAX_SESSION_INDEX_SEARCH_TERMS
                );
                terms
            }
            None => std::collections::BTreeSet::new(),
        };
        // Unlike independent ledger reads, this joins metadata to its derived
        // terms. Wait for any same-handle rebuild so callers cannot observe an
        // uncommitted purge or only part of the replacement inserts.
        let _same_handle_guard = self.write_lock.lock().await;

        let mut clauses = Vec::new();
        let mut params = Vec::new();
        if let Some(source_path) = &query.source_path {
            params.push(Value::Text(source_path.clone()));
            clauses.push(format!("m.source_path = ?{}", params.len()));
        }
        if let Some(project_cwd) = &query.project_cwd {
            params.push(Value::Text(project_cwd.clone()));
            clauses.push(format!("m.project_cwd = ?{}", params.len()));
        }
        if let Some(updated_after) = query.updated_after {
            params.push(Value::Real(updated_after));
            clauses.push(format!("m.updated_at >= ?{}", params.len()));
        }
        if let Some(updated_before) = query.updated_before {
            params.push(Value::Real(updated_before));
            clauses.push(format!("m.updated_at <= ?{}", params.len()));
        }
        for term in search_terms {
            params.push(Value::Text(term));
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM session_search_terms AS search_term \
                 WHERE search_term.term = ?{} \
                   AND search_term.session_id = m.session_id \
                   AND search_term.source_path = m.source_path)",
                params.len()
            ));
        }

        let mut sql = format!("SELECT {SESSION_INDEX_COLUMNS} FROM session_metadata AS m");
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        params.push(Value::Integer(
            query.limit.min(MAX_SESSION_INDEX_QUERY_LIMIT) as i64,
        ));
        params.push(Value::Integer(
            i64::try_from(query.offset).unwrap_or(i64::MAX),
        ));
        sql.push_str(&format!(
            " ORDER BY m.updated_at DESC, m.session_id ASC, m.source_path ASC \
             LIMIT ?{} OFFSET ?{}",
            params.len() - 1,
            params.len(),
        ));

        let mut rows = self.conn.query(&sql, params).await?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            entries.push(row_to_session_index_entry(&row)?);
        }
        Ok(entries)
    }

    /// Purge one rebuildable source, including its reconciliation marker.
    pub async fn delete_session_metadata_source(&self, source_path: &str) -> Result<()> {
        validate_session_source_path(source_path)?;
        let _same_handle_guard = self.write_lock.lock().await;
        let txn = begin_session_index_txn(&self.conn).await?;
        let result: Result<()> = async {
            self.conn
                .execute(
                    "DELETE FROM session_search_terms WHERE source_path = ?1",
                    [Value::Text(source_path.to_string())],
                )
                .await?;
            self.conn
                .execute(
                    "DELETE FROM session_metadata WHERE source_path = ?1",
                    [Value::Text(source_path.to_string())],
                )
                .await?;
            self.conn
                .execute(
                    "DELETE FROM session_metadata_reconciliation WHERE source_path = ?1",
                    [Value::Text(source_path.to_string())],
                )
                .await?;
            Ok(())
        }
        .await;
        finish_session_index_txn(txn, result).await
    }

    /// Write one row and rebuild its token set. The caller must hold
    /// `write_lock` and an open session metadata transaction.
    async fn upsert_session_metadata_in_open_txn(
        &self,
        entry: &SessionIndexEntry,
        indexed_at: f64,
    ) -> Result<()> {
        let models = serde_json::to_string(&entry.models)?;
        let changed = self
            .conn
            .execute(
                r#"INSERT INTO session_metadata
                   (source_path, session_id, file_path, project_cwd,
                    created_at, updated_at, model, turn_count,
                    compaction_count, parent_session_id, branch_name, title,
                    summary, title_source, title_turn, models, preview,
                    size_bytes, indexed_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                           ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                   ON CONFLICT(source_path, session_id) DO UPDATE SET
                       file_path = excluded.file_path,
                       project_cwd = excluded.project_cwd,
                       created_at = excluded.created_at,
                       updated_at = excluded.updated_at,
                       model = excluded.model,
                       turn_count = excluded.turn_count,
                       compaction_count = excluded.compaction_count,
                       parent_session_id = excluded.parent_session_id,
                       branch_name = excluded.branch_name,
                       title = excluded.title,
                       summary = excluded.summary,
                       title_source = excluded.title_source,
                       title_turn = excluded.title_turn,
                       models = excluded.models,
                       preview = excluded.preview,
                       size_bytes = excluded.size_bytes,
                       indexed_at = excluded.indexed_at
                   WHERE excluded.indexed_at > session_metadata.indexed_at
                     AND excluded.updated_at >= session_metadata.updated_at"#,
                [
                    Value::Text(entry.source_path.clone()),
                    Value::Text(entry.session_id.clone()),
                    Value::Text(entry.file_path.clone()),
                    opt_to_value(&entry.project_cwd),
                    Value::Real(entry.created_at),
                    Value::Real(entry.updated_at),
                    Value::Text(entry.model.clone()),
                    Value::Integer(entry.turn_count as i64),
                    Value::Integer(entry.compaction_count as i64),
                    opt_to_value(&entry.parent_session_id),
                    opt_to_value(&entry.branch_name),
                    opt_to_value(&entry.title),
                    opt_to_value(&entry.summary),
                    opt_to_value(&entry.title_source),
                    Value::Integer(entry.title_turn as i64),
                    Value::Text(models),
                    opt_to_value(&entry.preview),
                    Value::Integer(entry.size_bytes as i64),
                    Value::Real(indexed_at),
                ],
            )
            .await?;
        if changed == 0 {
            return Ok(());
        }
        self.conn
            .execute(
                "DELETE FROM session_search_terms \
                 WHERE source_path = ?1 AND session_id = ?2",
                [
                    Value::Text(entry.source_path.clone()),
                    Value::Text(entry.session_id.clone()),
                ],
            )
            .await?;
        for term in entry_search_terms(entry) {
            self.conn
                .execute(
                    r#"INSERT INTO session_search_terms
                       (source_path, session_id, term) VALUES (?1, ?2, ?3)"#,
                    [
                        Value::Text(entry.source_path.clone()),
                        Value::Text(entry.session_id.clone()),
                        Value::Text(term),
                    ],
                )
                .await?;
        }
        Ok(())
    }

    pub async fn query_by_session(&self, session_id: &str) -> Result<Vec<ProvenanceRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT * FROM provenance_records WHERE session_id = ?1 ORDER BY timestamp",
                [Value::Text(session_id.to_string())],
            )
            .await?;

        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            records.push(row_to_record(&row)?);
        }
        Ok(records)
    }

    /// Return records from every session, oldest first.
    ///
    /// This materializes the store-wide scope for an explicitly requested
    /// cross-session keyword scan; callers should not use it as a default.
    pub async fn query_all(&self) -> Result<Vec<ProvenanceRecord>> {
        let mut rows = self
            .conn
            .query("SELECT * FROM provenance_records ORDER BY timestamp", ())
            .await?;

        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            records.push(row_to_record(&row)?);
        }
        Ok(records)
    }

    pub async fn query_by_material(&self, material_ref: &str) -> Result<Vec<ProvenanceRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT * FROM provenance_records WHERE material_ref = ?1 ORDER BY timestamp",
                [Value::Text(material_ref.to_string())],
            )
            .await?;

        let mut records = Vec::new();
        while let Some(row) = rows.next().await? {
            records.push(row_to_record(&row)?);
        }
        Ok(records)
    }

    /// Return a record's parent chain without crossing a session boundary.
    ///
    /// The session is caller-provided scope; the record id alone never crosses
    /// that boundary. A missing record in the selected session returns an empty
    /// chain, so an id from a different session is indistinguishable from an
    /// unknown id.
    pub async fn query_chain(
        &self,
        record_id: &str,
        session_id: &str,
    ) -> Result<Vec<ProvenanceRecord>> {
        let mut chain = Vec::new();
        let mut current_id = Some(record_id.to_string());
        while let Some(id) = current_id {
            let mut rows = self
                .conn
                .query(
                    "SELECT * FROM provenance_records WHERE id = ?1 AND session_id = ?2",
                    [Value::Text(id), Value::Text(session_id.to_string())],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let rec = row_to_record(&row)?;
                current_id = rec.parent_id.clone();
                chain.push(rec);
            } else {
                break;
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// Embed `text` with `backend` and persist the vector for `record_id`.
    ///
    /// Deliberately NOT part of `record()`: provenance writes must never
    /// wait on (or fail because of) an embedding model. Callers spawn this
    /// after the record write succeeds and log-and-drop any error.
    pub async fn embed_and_store(
        &self,
        record_id: &str,
        text: &str,
        backend: &dyn prism_embed::EmbedBackend,
    ) -> Result<()> {
        let vectors = backend
            .embed(std::slice::from_ref(&text.to_string()))
            .await?;
        let vector = vectors
            .into_iter()
            .next()
            .context("embedding backend returned no vector")?;
        // Locked only around the INSERT — the embedding pass above must
        // never hold the store's write lock (see `write_lock`).
        let _same_handle_guard = self.write_lock.lock().await;
        self.conn
            .execute(
                r#"INSERT OR REPLACE INTO provenance_embeddings
                   (record_id, model, dim, vector) VALUES (?1, ?2, ?3, ?4)"#,
                [
                    Value::Text(record_id.to_string()),
                    Value::Text(backend.id().to_string()),
                    Value::Integer(vector.len() as i64),
                    Value::Blob(prism_embed::vec_to_le_bytes(&vector)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Brute-force cosine search over stored vectors in the selected scope.
    /// `None` selects every session and can be slower than the default scoped
    /// search.
    /// Returns up to `limit` records scored in `[-1, 1]`, best first.
    /// Vectors whose dimensionality differs from the query (mixed models)
    /// are skipped.
    pub async fn semantic_search(
        &self,
        query_vec: &[f32],
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(ProvenanceRecord, f32)>> {
        const RECORD_COLS: &str = "r.id, r.timestamp, r.session_id, r.action_type, r.actor, \
             r.tool_name, r.llm_model, r.input_json, r.output_json, \
             r.parent_id, r.material_ref, r.confidence, r.tags, r.status, r.exit_code";
        let mut rows = match session_id {
            Some(sid) => {
                self.conn
                    .query(
                        &format!(
                            "SELECT {RECORD_COLS}, e.vector FROM provenance_embeddings e \
                             JOIN provenance_records r ON r.id = e.record_id \
                             WHERE r.session_id = ?1"
                        ),
                        [Value::Text(sid.to_string())],
                    )
                    .await?
            }
            None => {
                self.conn
                    .query(
                        &format!(
                            "SELECT {RECORD_COLS}, e.vector FROM provenance_embeddings e \
                             JOIN provenance_records r ON r.id = e.record_id"
                        ),
                        (),
                    )
                    .await?
            }
        };

        let mut scored = Vec::new();
        while let Some(row) = rows.next().await? {
            let record = row_to_record(&row)?;
            // RECORD_COLS now selects 15 columns (0..14), so the joined
            // e.vector sits at index 15.
            let vector = match row.get_value(15)? {
                Value::Blob(bytes) => prism_embed::le_bytes_to_vec(&bytes),
                _ => continue,
            };
            if vector.len() != query_vec.len() {
                continue; // different embedding model — not comparable
            }
            scored.push((record, prism_embed::cosine_similarity(query_vec, &vector)));
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Aggregate counts over the store, broken out by outcome.
    ///
    /// VS3: the whole point of the structured `status` field is that "which
    /// runs failed" is a queryable question. `stats()` previously returned
    /// only a flat `total_records`, so the failure rate was invisible. Now it
    /// groups by `status` into the three buckets the record knows:
    /// `"ok"`, `"error"`, and everything else (legacy rows or non-tool records
    /// where the notion does not apply — `LlmCall`/`Ingest`/...). `total_records`
    /// is preserved (== ok + error + other) so existing callers are unaffected.
    pub async fn stats(&self) -> Result<ProvenanceStats> {
        let mut ok = 0usize;
        let mut error = 0usize;
        let mut other = 0usize;
        let mut rows = self
            .conn
            .query(
                "SELECT COALESCE(status, '') AS s, COUNT(*) FROM provenance_records GROUP BY s",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let bucket = get_str(&row, 0)?;
            let count = row
                .get_value(1)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0) as usize;
            match bucket.as_str() {
                "ok" => ok += count,
                "error" => error += count,
                _ => other += count,
            }
        }
        Ok(ProvenanceStats {
            total_records: ok + error + other,
            ok_records: ok,
            error_records: error,
            other_records: other,
        })
    }

    /// Return the records of failed **tool** runs — VS3's direct "which tool
    /// runs failed?" query. Filters on the structured `status` column (set by
    /// [`crate::hooks`] via `classify_for_provenance`), newest-first, bounded
    /// by `limit`. Optionally scoped to a `session_id` (None = across all
    /// sessions). Reuses [`row_to_record`] over the same 15-column `SELECT *`
    /// ordering that the other readers use.
    ///
    /// `limit` is CLAMPED to `FAILURE_QUERY_MAX_LIMIT` before it reaches SQL.
    /// Without this, `limit as i64` wraps NEGATIVE for `limit > i64::MAX`, and
    /// SQLite/Turso treats a negative `LIMIT n` as UNBOUNDED — so a hostile or
    /// oversized caller input could dump the whole store. Any caller value
    /// above the cap is silently truncated; callers wanting a smaller window
    /// should pass a smaller `limit`.
    ///
    /// **VS3 scope limitation (flagged for owner):** this answers "which TOOL
    /// runs failed", NOT "which runs failed" in full. A failed LLM call is
    /// ABSENT from the ledger entirely — `agent_loop.rs` returns at the `?`
    /// BEFORE the LLM-turn recorder runs, so no record is ever written for it.
    /// (It is not `status=None`; it simply does not exist.) Extending VS3 to
    /// record LLM-call failures requires writing BEFORE the `?` — a separate,
    /// larger change deferred for now.
    pub async fn query_failures(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProvenanceRecord>> {
        // Clamp BEFORE the `as i64` cast so the SQL `LIMIT` is always a sane,
        // non-negative number. 1000 is far above any reasonable single-response
        // window yet bounded enough to keep a runaway dump cheap.
        const FAILURE_QUERY_MAX_LIMIT: usize = 1000;
        let limit = limit.min(FAILURE_QUERY_MAX_LIMIT);
        let mut records = Vec::new();
        let mut rows = match session_id {
            Some(sid) => {
                self.conn
                    .query(
                        "SELECT * FROM provenance_records \
                         WHERE status = 'error' AND session_id = ?1 \
                         ORDER BY timestamp DESC LIMIT ?2",
                        [Value::Text(sid.to_string()), Value::Integer(limit as i64)],
                    )
                    .await?
            }
            None => {
                self.conn
                    .query(
                        "SELECT * FROM provenance_records WHERE status = 'error' \
                         ORDER BY timestamp DESC LIMIT ?1",
                        [Value::Integer(limit as i64)],
                    )
                    .await?
            }
        };
        while let Some(row) = rows.next().await? {
            records.push(row_to_record(&row)?);
        }
        Ok(records)
    }
}

#[derive(Debug, Serialize)]
pub struct ProvenanceStats {
    pub total_records: usize,
    /// VS3: rows whose structured `status` is `"ok"`.
    pub ok_records: usize,
    /// VS3: rows whose structured `status` is `"error"` — the answer to
    /// "which runs failed". This is what `query_failures` enumerates.
    pub error_records: usize,
    /// Rows with no `status` (legacy, or non-tool records like `LlmCall`/`Ingest`
    /// where pass/fail does not apply). `total == ok + error + other`.
    pub other_records: usize,
}

fn get_str(row: &turso::Row, idx: usize) -> Result<String> {
    let val = row.get_value(idx)?;
    Ok(match val {
        Value::Text(s) => s,
        _ => String::new(),
    })
}

fn get_opt_str(row: &turso::Row, idx: usize) -> Result<Option<String>> {
    let val = row.get_value(idx)?;
    Ok(match val {
        Value::Text(s) => Some(s),
        Value::Null => None,
        _ => None,
    })
}

fn get_f64(row: &turso::Row, idx: usize, field: &str) -> Result<f64> {
    match row.get_value(idx)? {
        Value::Real(value) => Ok(value),
        Value::Integer(value) => Ok(value as f64),
        value => anyhow::bail!("session metadata {field} is not numeric: {value:?}"),
    }
}

fn get_session_u64(row: &turso::Row, idx: usize, field: &str) -> Result<u64> {
    match row.get_value(idx)? {
        Value::Integer(value) => {
            u64::try_from(value).with_context(|| format!("session metadata {field} is negative"))
        }
        value => anyhow::bail!("session metadata {field} is not an integer: {value:?}"),
    }
}

const SESSION_INDEX_COLUMNS: &str = "m.source_path, m.session_id, m.file_path, m.project_cwd, \
    m.created_at, m.updated_at, m.model, m.turn_count, m.compaction_count, \
    m.parent_session_id, m.branch_name, m.title, m.summary, m.title_source, \
    m.title_turn, m.models, m.preview, m.size_bytes";

fn row_to_session_index_entry(row: &turso::Row) -> Result<SessionIndexEntry> {
    let models = serde_json::from_str(&get_str(row, 15)?)
        .context("session metadata models contains invalid JSON")?;
    Ok(SessionIndexEntry {
        session_id: get_str(row, 1)?,
        source_path: get_str(row, 0)?,
        file_path: get_str(row, 2)?,
        project_cwd: get_opt_str(row, 3)?,
        created_at: get_f64(row, 4, "created_at")?,
        updated_at: get_f64(row, 5, "updated_at")?,
        model: get_str(row, 6)?,
        turn_count: get_session_u64(row, 7, "turn_count")?,
        compaction_count: get_session_u64(row, 8, "compaction_count")?,
        parent_session_id: get_opt_str(row, 9)?,
        branch_name: get_opt_str(row, 10)?,
        title: get_opt_str(row, 11)?,
        summary: get_opt_str(row, 12)?,
        title_source: get_opt_str(row, 13)?,
        title_turn: get_session_u64(row, 14, "title_turn")?,
        models,
        preview: get_opt_str(row, 16)?,
        size_bytes: get_session_u64(row, 17, "size_bytes")?,
    })
}

const AGENT_RUN_COLUMNS: &str = "id, parent_run_id, session_id, role, label, status, \
    started_at, updated_at, ended_at, tokens_in, tokens_out, cost_usd, last_error";

fn get_u64(row: &turso::Row, idx: usize, field: &str) -> Result<u64> {
    match row.get_value(idx)? {
        Value::Integer(value) => {
            u64::try_from(value).with_context(|| format!("agent run {field} is negative"))
        }
        value => anyhow::bail!("agent run {field} is not an integer: {value:?}"),
    }
}

fn get_agent_run_traversal_bool(row: &turso::Row, idx: usize, field: &str) -> Result<bool> {
    match row.get_value(idx)? {
        Value::Integer(0) => Ok(false),
        Value::Integer(1) => Ok(true),
        value => anyhow::bail!("agent run traversal {field} is not a boolean: {value:?}"),
    }
}

fn row_to_agent_run(row: &turso::Row) -> Result<AgentRun> {
    let cost_usd = match row.get_value(11)? {
        Value::Real(value) => value,
        Value::Integer(value) => value as f64,
        value => anyhow::bail!("agent run cost_usd is not numeric: {value:?}"),
    };
    Ok(AgentRun {
        id: get_str(row, 0)?,
        parent_run_id: get_opt_str(row, 1)?,
        session_id: get_str(row, 2)?,
        role: get_str(row, 3)?,
        label: get_str(row, 4)?,
        status: AgentRunStatus::from_db(&get_str(row, 5)?)?,
        started_at: get_str(row, 6)?,
        updated_at: get_str(row, 7)?,
        ended_at: get_opt_str(row, 8)?,
        tokens_in: get_u64(row, 9, "tokens_in")?,
        tokens_out: get_u64(row, 10, "tokens_out")?,
        cost_usd,
        last_error: get_opt_str(row, 12)?,
    })
}

fn row_to_record(row: &turso::Row) -> Result<ProvenanceRecord> {
    let action_type = match get_str(row, 3)?.as_str() {
        "tool_call" => ActionType::ToolCall,
        "llm_call" => ActionType::LlmCall,
        "ingest" => ActionType::Ingest,
        "query" => ActionType::Query,
        "generative" => ActionType::Generative,
        "workflow" => ActionType::Workflow,
        "compute" => ActionType::Compute,
        "mesh" => ActionType::Mesh,
        _ => ActionType::ToolCall,
    };

    let actor = match get_str(row, 4)?.as_str() {
        "agent" => Actor::Agent,
        "user" => Actor::User,
        "anonymous_local" => Actor::AnonymousLocal,
        "authenticated_user" => Actor::AuthenticatedUser,
        "authenticated_owner" => Actor::AuthenticatedOwner,
        "system" => Actor::System,
        "scheduler" => Actor::Scheduler,
        _ => Actor::System,
    };

    let input_json: serde_json::Value =
        serde_json::from_str(&get_str(row, 7)?).unwrap_or(serde_json::json!({}));

    let output_json = get_opt_str(row, 8)?
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str(&s).ok());

    let tags: Vec<String> = get_opt_str(row, 12)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let confidence = row
        .get_value(11)
        .ok()
        .and_then(|v| v.as_real().copied())
        .unwrap_or(0.0);

    // VS1/F5: status (col 13) and exit_code (col 14). SELECT * returns them in
    // table-definition order, after `tags` (col 12). For the explicit-column
    // semantic_search path, RECORD_COLS lists them in the same order.
    let status = get_opt_str(row, 13)?.filter(|s| !s.is_empty());
    let exit_code = row.get_value(14).ok().and_then(|v| v.as_integer().copied());

    Ok(ProvenanceRecord {
        id: get_str(row, 0)?,
        timestamp: get_str(row, 1)?,
        session_id: get_str(row, 2)?,
        action_type,
        actor,
        tool_name: get_opt_str(row, 5)?,
        llm_model: get_opt_str(row, 6)?,
        input_json,
        output_json,
        parent_id: get_opt_str(row, 9)?,
        material_ref: get_opt_str(row, 10)?,
        confidence,
        tags,
        status,
        exit_code,
    })
}

/// Cap (chars) on the text sent to the embedding backend per record.
const EMBED_TEXT_CHARS: usize = 2_000;

/// Canonical text to embed for a record: tool name + flattened input +
/// output, truncated to ~2000 chars so one record is one model pass.
pub fn embedding_text(rec: &ProvenanceRecord) -> String {
    let mut text = String::new();
    if let Some(tool) = &rec.tool_name {
        text.push_str(tool);
        text.push(' ');
    }
    text.push_str(&rec.input_json.to_string());
    if let Some(output) = &rec.output_json {
        text.push(' ');
        text.push_str(&output.to_string());
    }
    if text.chars().count() > EMBED_TEXT_CHARS {
        text = text.chars().take(EMBED_TEXT_CHARS).collect();
    }
    text
}

pub fn new_record(
    session_id: &str,
    action_type: ActionType,
    actor: Actor,
    tool_name: Option<&str>,
    llm_model: Option<&str>,
    input: serde_json::Value,
) -> ProvenanceRecord {
    ProvenanceRecord {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        action_type,
        actor,
        tool_name: tool_name.map(|s| s.to_string()),
        llm_model: llm_model.map(|s| s.to_string()),
        input_json: input,
        output_json: None,
        parent_id: None,
        material_ref: None,
        confidence: 0.0,
        tags: Vec::new(),
        status: None,
        exit_code: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repair_item(id: &str, class: &str) -> RepairItem {
        RepairItem {
            item_id: id.into(),
            document: "paper.pdf".into(),
            tenant: "local".into(),
            class: class.into(),
            subject_json: r#"{"subject":"Ti-6Al-4V"}"#.into(),
            detail: "unit QUDT:MM-PER-S is not in the vocabulary".into(),
            enqueued_at: 1.0,
            attempts: 0,
        }
    }

    /// The same refusal seen twice is ONE item. Re-ingesting a document must
    /// not re-queue work that was already judged, or the queue grows without
    /// bound on every re-run.
    #[tokio::test]
    async fn enqueueing_the_same_refusal_twice_yields_one_item() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let item = repair_item("fact-1|unresolved_unit", "unresolved_unit");
        store.enqueue_repair(&item).await.unwrap();
        store.enqueue_repair(&item).await.unwrap();
        let pending = store.pending_repairs("paper.pdf", 10).await.unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].item_id, item.item_id);
    }

    /// Deciding an item clears it from the queue but the DECISION survives.
    /// A queue that could be drained without leaving a record is exactly the
    /// silent outcome this ledger exists to prevent.
    #[tokio::test]
    async fn a_decision_leaves_the_queue_empty_and_the_record_intact() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        store
            .enqueue_repair(&repair_item("fact-1|unresolved_unit", "unresolved_unit"))
            .await
            .unwrap();

        store
            .record_repair_disposition(&RepairDisposition {
                item_id: "fact-1|unresolved_unit".into(),
                attempt: 0,
                document: "paper.pdf".into(),
                class: "unresolved_unit".into(),
                outcome: "accept".into(),
                corrected_json: Some(r#"{"unit":"QUDT:MilliM-PER-SEC"}"#.into()),
                evidence: Some("scanned at 1250 mm/s".into()),
                reason: "the document states the unit; the vocabulary resolves it".into(),
                dispositioner: "code:unit_span_lookup".into(),
                decided_at: 2.0,
            })
            .await
            .unwrap();

        assert!(
            store
                .pending_repairs("paper.pdf", 10)
                .await
                .unwrap()
                .is_empty(),
            "a decided item must leave the queue"
        );
        let ledger = store.repair_dispositions("paper.pdf").await.unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].outcome, "accept");
        assert_eq!(
            ledger[0].dispositioner, "code:unit_span_lookup",
            "an audit must be able to tell a code repair from a model's judgement"
        );
        assert_eq!(ledger[0].evidence.as_deref(), Some("scanned at 1250 mm/s"));
    }

    /// A second decision on the same item is a NEW row, not an overwrite —
    /// the history of a repair stays readable.
    #[tokio::test]
    async fn a_second_attempt_is_appended_never_replacing_the_first() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let base = RepairDisposition {
            item_id: "fact-1|review_missing".into(),
            attempt: 0,
            document: "paper.pdf".into(),
            class: "review_missing".into(),
            outcome: "withdraw".into(),
            corrected_json: None,
            evidence: None,
            reason: "no verdict on the first pass".into(),
            dispositioner: "model:gemma-4-12b".into(),
            decided_at: 2.0,
        };
        store.record_repair_disposition(&base).await.unwrap();
        store
            .record_repair_disposition(&RepairDisposition {
                attempt: 1,
                outcome: "accept".into(),
                reason: "verdict obtained on retry".into(),
                ..base.clone()
            })
            .await
            .unwrap();

        let ledger = store.repair_dispositions("paper.pdf").await.unwrap();
        assert_eq!(ledger.len(), 2, "both attempts must survive: {ledger:?}");
        assert!(
            ledger
                .iter()
                .any(|d| d.attempt == 0 && d.outcome == "withdraw")
        );
        assert!(
            ledger
                .iter()
                .any(|d| d.attempt == 1 && d.outcome == "accept")
        );
    }

    // ── Ontology extension proposal queue ───────────────────────────────

    fn proposal_item(id: &str, kind: &str, label: &str, document: &str) -> OntologyProposalItem {
        OntologyProposalItem {
            item_id: id.into(),
            kind: kind.into(),
            label: label.into(),
            document: document.into(),
            tenant: "local".into(),
            proposal_json: format!(r#"{{"label":"{label}"}}"#),
            enqueued_at: 1.0,
        }
    }

    /// A proposal is identified by its CONTENT, not its citation: the same
    /// identity proposed from two documents is one queue item with TWO
    /// citations — evidence accumulates exactly the way assertion evidence
    /// contributions do. This is the storage half of "the ontology is the
    /// product": the loop can grow one only if proposals survive their run.
    #[tokio::test]
    async fn the_same_proposal_from_two_documents_is_one_item_with_two_citations() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let first = proposal_item(
            "class|laser powder bed fusion",
            "class",
            "Laser Powder Bed Fusion",
            "a.pdf",
        );
        assert_eq!(
            store
                .enqueue_ontology_proposal(&first, r#"{"from_line":3,"to_line":3}"#, 1.0)
                .await
                .unwrap(),
            OntologyProposalEnqueue::Queued
        );
        // Re-ingest of the SAME document: same citation, nothing new.
        assert_eq!(
            store
                .enqueue_ontology_proposal(&first, r#"{"from_line":3,"to_line":3}"#, 2.0)
                .await
                .unwrap(),
            OntologyProposalEnqueue::Existing {
                new_sighting: false
            }
        );
        // A DIFFERENT document proposing the same identity: a second
        // citation on the same item, never a second queue row.
        let second = proposal_item(
            "class|laser powder bed fusion",
            "class",
            "Laser Powder Bed Fusion",
            "b.pdf",
        );
        assert_eq!(
            store
                .enqueue_ontology_proposal(&second, r#"{"from_line":9,"to_line":9}"#, 3.0)
                .await
                .unwrap(),
            OntologyProposalEnqueue::Existing { new_sighting: true }
        );

        let pending = store.pending_ontology_proposals(10).await.unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].1, 2, "both citations must back the one item");
        let sightings = store
            .ontology_proposal_sightings("class|laser powder bed fusion")
            .await
            .unwrap();
        assert_eq!(sightings.len(), 2, "{sightings:?}");
        let documents: Vec<&str> = sightings.iter().map(|s| s.document.as_str()).collect();
        assert!(documents.contains(&"a.pdf") && documents.contains(&"b.pdf"));
    }

    /// A REJECTED proposal stays rejected: re-proposing the same identity
    /// after the decision stores nothing and says so, so a concept cannot
    /// be re-proposed forever. The suppression is a returned value, not a
    /// silent drop — the ingest summary counts it.
    #[tokio::test]
    async fn a_rejected_proposal_is_never_re_queued() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let item = proposal_item(
            "relation| processed by laser",
            "relation",
            "processed by laser",
            "a.pdf",
        );
        store
            .enqueue_ontology_proposal(&item, r#"{"from_line":1,"to_line":1}"#, 1.0)
            .await
            .unwrap();
        store
            .record_ontology_proposal_disposition(&OntologyProposalDisposition {
                item_id: item.item_id.clone(),
                document: "a.pdf".into(),
                kind: "relation".into(),
                label: "processed by laser".into(),
                outcome: "rejected".into(),
                artifact_path: None,
                reason: "already expressible with the existing process vocabulary".into(),
                dispositioner: "human:reviewer".into(),
                decided_at: 2.0,
            })
            .await
            .unwrap();

        assert!(
            store
                .pending_ontology_proposals(10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .enqueue_ontology_proposal(&item, r#"{"from_line":1,"to_line":1}"#, 3.0)
                .await
                .unwrap(),
            OntologyProposalEnqueue::SupersededByDisposition,
            "a dispositioned identity must not be re-queued"
        );
        assert!(
            store
                .pending_ontology_proposals(10)
                .await
                .unwrap()
                .is_empty(),
            "the suppressed enqueue must have stored no queue row"
        );
        // The decision itself is still readable — rejection is an audit
        // record, not a deletion.
        let ledger = store
            .ontology_proposal_dispositions(&item.item_id)
            .await
            .unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].outcome, "rejected");
        // And the sighting from the ORIGINAL proposal survived the decision.
        assert_eq!(
            store
                .ontology_proposal_sightings(&item.item_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Accepting records the artifact the acceptance produced (feeding the
    /// existing promotion path) and clears the queue — the same
    /// ledger-first, decision-survives contract as the repair queue.
    #[tokio::test]
    async fn an_acceptance_records_its_artifact_and_leaves_the_ledger_readable() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let item = proposal_item(
            "class| feedstock powder",
            "class",
            "Feedstock Powder",
            "c.pdf",
        );
        store
            .enqueue_ontology_proposal(&item, r#"{"from_line":2,"to_line":2}"#, 1.0)
            .await
            .unwrap();
        store
            .record_ontology_proposal_disposition(&OntologyProposalDisposition {
                item_id: item.item_id.clone(),
                document: "c.pdf".into(),
                kind: "class".into(),
                label: "Feedstock Powder".into(),
                outcome: "accepted".into(),
                artifact_path: Some("ontology-customer-ext.ttl".into()),
                reason: "corpus needs a powder-feed concept; parents resolve".into(),
                dispositioner: "human:reviewer".into(),
                decided_at: 4.0,
            })
            .await
            .unwrap();

        assert!(
            store
                .pending_ontology_proposals(10)
                .await
                .unwrap()
                .is_empty()
        );
        let ledger = store
            .ontology_proposal_dispositions(&item.item_id)
            .await
            .unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].outcome, "accepted");
        assert_eq!(
            ledger[0].artifact_path.as_deref(),
            Some("ontology-customer-ext.ttl")
        );
    }

    /// `ontology_proposal_by_id` is the accept/reject surface's loader: it
    /// must return the pending row, and honestly `None` for an unknown or
    /// already-dispositioned id.
    #[tokio::test]
    async fn a_proposal_is_loadable_by_id_until_decided() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let item = proposal_item("class| porosity", "class", "Porosity", "d.pdf");
        store
            .enqueue_ontology_proposal(&item, r#"{}"#, 1.0)
            .await
            .unwrap();
        let loaded = store
            .ontology_proposal_by_id(&item.item_id)
            .await
            .unwrap()
            .expect("a queued proposal must be loadable");
        assert_eq!(loaded.kind, "class");
        assert_eq!(loaded.label, "Porosity");
        assert!(
            store
                .ontology_proposal_by_id("no-such-id")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fresh_file_accepts_concurrent_store_opens() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("concurrent-open.db");
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = tokio::task::JoinSet::new();

        for _ in 0..8 {
            let path = path.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                ProvenanceStore::open(&path).await.map(drop)
            });
        }

        while let Some(result) = tasks.join_next().await {
            result.expect("store open task must not panic").unwrap();
        }

        let store = ProvenanceStore::open(&path).await.unwrap();
        assert!(
            store
                .list_agent_runs(&AgentRunFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn agent_run_lifecycle_and_filters_round_trip() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let parent = new_agent_run("session-a", "agent", "root task", None);
        let child = new_agent_run("session-a", "subagent", "delegated task", Some(&parent.id));
        let other = new_agent_run("session-b", "agent", "other task", None);
        for run in [&parent, &child, &other] {
            store.start_agent_run(run).await.unwrap();
        }
        store
            .finish_agent_run(
                &child.id,
                AgentRunStatus::Completed,
                1_200,
                340,
                0.0125,
                None,
            )
            .await
            .unwrap();
        store
            .finish_agent_run(
                &parent.id,
                AgentRunStatus::Failed,
                10,
                2,
                0.001,
                Some("provider disconnected"),
            )
            .await
            .unwrap();
        store
            .finish_agent_run(&other.id, AgentRunStatus::Cancelled, 0, 0, 0.0, None)
            .await
            .unwrap();

        let completed = store
            .list_agent_runs(&AgentRunFilter {
                session_id: Some("session-a".to_string()),
                status: Some(AgentRunStatus::Completed),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, child.id);
        assert_eq!(
            completed[0].parent_run_id.as_deref(),
            Some(parent.id.as_str())
        );
        assert_eq!(completed[0].tokens_in, 1_200);
        assert_eq!(completed[0].tokens_out, 340);
        assert!((completed[0].cost_usd - 0.0125).abs() < f64::EPSILON);
        assert!(completed[0].ended_at.is_some());

        let children = store
            .list_agent_runs(&AgentRunFilter {
                parent_run_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);

        let failed = store
            .list_agent_runs(&AgentRunFilter {
                status: Some(AgentRunStatus::Failed),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, parent.id);
        assert_eq!(
            failed[0].last_error.as_deref(),
            Some("provider disconnected")
        );
        let cancelled = store
            .list_agent_runs(&AgentRunFilter {
                status: Some(AgentRunStatus::Cancelled),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].id, other.id);
    }

    #[tokio::test]
    async fn agent_run_descendants_returns_all_three_spawn_levels() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let root = new_agent_run("session-a", "agent", "root", None);
        let child = new_agent_run("session-a", "subagent", "child", Some(&root.id));
        let grandchild = new_agent_run("session-a", "subagent", "grandchild", Some(&child.id));
        let great_grandchild = new_agent_run(
            "session-a",
            "subagent",
            "great-grandchild",
            Some(&grandchild.id),
        );
        for run in [&root, &child, &grandchild, &great_grandchild] {
            store.start_agent_run(run).await.unwrap();
        }

        let result = store
            .list_agent_run_descendants(&root.id, &AgentRunTraversalPolicy::default())
            .await
            .unwrap();
        assert_eq!(result.outcome, AgentRunTraversalOutcome::Complete);
        assert_eq!(
            result
                .runs
                .iter()
                .map(|run| run.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                child.id.as_str(),
                grandchild.id.as_str(),
                great_grandchild.id.as_str()
            ]
        );
    }

    #[tokio::test]
    async fn agent_run_descendants_reports_a_cycle_instead_of_hanging() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let root = new_agent_run("session-a", "agent", "root", None);
        let child = new_agent_run("session-a", "subagent", "child", Some(&root.id));
        let grandchild = new_agent_run("session-a", "subagent", "grandchild", Some(&child.id));
        for run in [&root, &child, &grandchild] {
            store.start_agent_run(run).await.unwrap();
        }
        store
            .conn
            .execute(
                "UPDATE agent_runs SET parent_run_id = ?1 WHERE id = ?2",
                [
                    Value::Text(grandchild.id.clone()),
                    Value::Text(root.id.clone()),
                ],
            )
            .await
            .unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.list_agent_run_descendants(
                &root.id,
                &AgentRunTraversalPolicy {
                    max_depth: 10,
                    max_rows: 10,
                },
            ),
        )
        .await
        .expect("cycle-safe traversal must terminate")
        .unwrap();
        assert_eq!(
            result
                .runs
                .iter()
                .map(|run| run.id.as_str())
                .collect::<Vec<_>>(),
            vec![child.id.as_str(), grandchild.id.as_str()]
        );
        assert_eq!(
            result.outcome,
            AgentRunTraversalOutcome::Incomplete {
                issues: vec![AgentRunTraversalIssue::CycleDetected {
                    repeated_run_id: root.id.clone(),
                }]
            }
        );
    }

    #[tokio::test]
    async fn agent_run_descendants_reports_depth_truncation() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let root = new_agent_run("session-a", "agent", "root", None);
        let child = new_agent_run("session-a", "subagent", "child", Some(&root.id));
        let grandchild = new_agent_run("session-a", "subagent", "grandchild", Some(&child.id));
        for run in [&root, &child, &grandchild] {
            store.start_agent_run(run).await.unwrap();
        }

        let result = store
            .list_agent_run_descendants(
                &root.id,
                &AgentRunTraversalPolicy {
                    max_depth: 1,
                    max_rows: 10,
                },
            )
            .await
            .unwrap();
        assert_eq!(result.runs, vec![child]);
        assert_eq!(
            result.outcome,
            AgentRunTraversalOutcome::Incomplete {
                issues: vec![AgentRunTraversalIssue::DepthLimitReached { max_depth: 1 }]
            }
        );
    }

    #[tokio::test]
    async fn agent_run_descendants_reports_row_truncation_but_not_an_exact_boundary() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let root = new_agent_run("session-a", "agent", "root", None);
        let child = new_agent_run("session-a", "subagent", "child", Some(&root.id));
        let grandchild = new_agent_run("session-a", "subagent", "grandchild", Some(&child.id));
        for run in [&root, &child, &grandchild] {
            store.start_agent_run(run).await.unwrap();
        }

        let exact = store
            .list_agent_run_descendants(
                &root.id,
                &AgentRunTraversalPolicy {
                    max_depth: 10,
                    max_rows: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(exact.runs, vec![child.clone(), grandchild]);
        assert_eq!(exact.outcome, AgentRunTraversalOutcome::Complete);

        let truncated = store
            .list_agent_run_descendants(
                &root.id,
                &AgentRunTraversalPolicy {
                    max_depth: 10,
                    max_rows: 1,
                },
            )
            .await
            .unwrap();
        assert_eq!(truncated.runs, vec![child]);
        assert_eq!(
            truncated.outcome,
            AgentRunTraversalOutcome::Incomplete {
                issues: vec![AgentRunTraversalIssue::RowLimitReached { max_rows: 1 }]
            }
        );
    }

    #[tokio::test]
    async fn stale_running_query_returns_old_heartbeat_not_fresh() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut stale = new_agent_run("stale-session", "agent", "old task", None);
        stale.started_at = "2000-01-01T00:00:00+00:00".to_string();
        stale.updated_at = stale.started_at.clone();
        let fresh = new_agent_run("stale-session", "agent", "fresh task", None);
        store.start_agent_run(&stale).await.unwrap();
        store.start_agent_run(&fresh).await.unwrap();

        let policy = AgentRunStalenessPolicy {
            stale_after: std::time::Duration::from_secs(60),
            max_results: 10,
        };
        let runs = store.stale_running_agent_runs(&policy).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, stale.id);
        assert_ne!(runs[0].id, fresh.id);
    }

    #[tokio::test]
    async fn stale_running_query_rejects_unrepresentable_cutoff_without_panicking() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let policy = AgentRunStalenessPolicy {
            stale_after: std::time::Duration::from_secs(8_500_000_000_000),
            max_results: 10,
        };

        let error = store.stale_running_agent_runs(&policy).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("staleness cutoff is outside the timestamp range")
        );
    }

    #[tokio::test]
    async fn agent_run_schema_declares_operator_query_indexes() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut rows = store
            .conn
            .query(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = 'agent_runs'",
                (),
            )
            .await
            .unwrap();
        let mut names = std::collections::HashSet::new();
        while let Some(row) = rows.next().await.unwrap() {
            names.insert(get_str(&row, 0).unwrap());
        }
        for expected in [
            "idx_agent_runs_session_started",
            "idx_agent_runs_status_updated",
            "idx_agent_runs_parent_started",
        ] {
            assert!(names.contains(expected), "missing index {expected}");
        }
    }

    #[tokio::test]
    async fn test_record_and_query() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut rec = new_record(
            "test-session",
            ActionType::Generative,
            Actor::Agent,
            Some("generate"),
            Some("gemma-4-12b"),
            serde_json::json!({"n_samples": 64, "elements": ["Ni", "Cr", "Co"]}),
        );
        rec.material_ref = Some("Ni0.3 Cr0.4 Co0.3".to_string());
        rec.output_json = Some(serde_json::json!({"top_alloys": []}));
        rec.confidence = 0.85;

        store.record(&rec).await.unwrap();

        let results = store.query_by_session("test-session").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_name.as_deref(), Some("generate"));
        assert_eq!(
            results[0].material_ref.as_deref(),
            Some("Ni0.3 Cr0.4 Co0.3")
        );
        assert!((results[0].confidence - 0.85).abs() < 0.01);

        let mat_results = store.query_by_material("Ni0.3 Cr0.4 Co0.3").await.unwrap();
        assert_eq!(mat_results.len(), 1);
    }

    #[tokio::test]
    async fn test_chain() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();

        let parent = new_record(
            "s1",
            ActionType::Query,
            Actor::User,
            None,
            None,
            serde_json::json!({"q": "nickel superalloys"}),
        );
        store.record(&parent).await.unwrap();

        let mut child = new_record(
            "s1",
            ActionType::Generative,
            Actor::Agent,
            Some("generate"),
            Some("gemma-4-12b"),
            serde_json::json!({"elements": ["Ni", "Cr", "Co"]}),
        );
        child.parent_id = Some(parent.id.clone());
        child.material_ref = Some("Ni0.5 Cr0.3 Co0.2".to_string());
        store.record(&child).await.unwrap();

        let chain = store.query_chain(&child.id, "s1").await.unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, parent.id);
        assert_eq!(chain[1].id, child.id);

        // A caller in another session cannot use a record id to traverse this
        // chain. The boundary is enforced at every parent lookup.
        assert!(store.query_chain(&child.id, "s2").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn actor_scope_round_trips_without_unqualified_identity() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        for actor in [Actor::AnonymousLocal, Actor::AuthenticatedOwner] {
            let rec = new_record(
                "actor-session",
                ActionType::ToolCall,
                actor.clone(),
                Some("test_tool"),
                None,
                serde_json::json!({}),
            );
            store.record(&rec).await.unwrap();
        }

        let records = store.query_by_session("actor-session").await.unwrap();
        assert!(
            records
                .iter()
                .any(|record| record.actor == Actor::AnonymousLocal)
        );
        assert!(
            records
                .iter()
                .any(|record| record.actor == Actor::AuthenticatedOwner)
        );
    }

    /// Deterministic test backend: axis 0 counts "alloy", axis 1 counts
    /// "kafka", axis 2 is constant so no vector is ever all-zero.
    struct KeywordAxes;

    #[async_trait::async_trait]
    impl prism_embed::EmbedBackend for KeywordAxes {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    vec![
                        t.matches("alloy").count() as f32,
                        t.matches("kafka").count() as f32,
                        0.1,
                    ]
                })
                .collect())
        }
        fn dimensions(&self) -> usize {
            3
        }
        fn id(&self) -> &str {
            "test:keyword-axes"
        }
    }

    #[tokio::test]
    async fn test_embed_and_semantic_search() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let backend = KeywordAxes;

        let mut alloy = new_record(
            "s1",
            ActionType::ToolCall,
            Actor::Agent,
            Some("generate"),
            None,
            serde_json::json!({"task": "alloy alloy alloy search"}),
        );
        alloy.output_json = Some(serde_json::json!("alloy candidates"));
        let kafka = new_record(
            "s1",
            ActionType::ToolCall,
            Actor::Agent,
            Some("shell"),
            None,
            serde_json::json!({"cmd": "kafka kafka restart"}),
        );
        let other_session = new_record(
            "s2",
            ActionType::ToolCall,
            Actor::Agent,
            Some("file"),
            None,
            serde_json::json!({"path": "alloy.csv"}),
        );
        for rec in [&alloy, &kafka, &other_session] {
            store.record(rec).await.unwrap();
            store
                .embed_and_store(&rec.id, &embedding_text(rec), &backend)
                .await
                .unwrap();
        }

        // "alloy"-directed query vector: alloy record must win within s1.
        let query = vec![1.0, 0.0, 0.0];
        let hits = store.semantic_search(&query, Some("s1"), 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0.id, alloy.id);
        assert!(hits[0].1 > hits[1].1);
        // Session filter: s2's record never appears.
        assert!(hits.iter().all(|(r, _)| r.session_id == "s1"));

        // Unfiltered search sees all three sessions.
        let all = store.semantic_search(&query, None, 10).await.unwrap();
        assert_eq!(all.len(), 3);

        // Limit is respected.
        let top1 = store.semantic_search(&query, Some("s1"), 1).await.unwrap();
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0].0.id, alloy.id);
    }

    #[tokio::test]
    async fn test_embed_and_store_is_idempotent_per_record() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let backend = KeywordAxes;
        let rec = new_record(
            "s1",
            ActionType::ToolCall,
            Actor::Agent,
            Some("t"),
            None,
            serde_json::json!({"q": "alloy"}),
        );
        store.record(&rec).await.unwrap();
        store
            .embed_and_store(&rec.id, "alloy", &backend)
            .await
            .unwrap();
        // Re-embedding the same record replaces, not duplicates.
        store
            .embed_and_store(&rec.id, "alloy alloy", &backend)
            .await
            .unwrap();
        let hits = store
            .semantic_search(&[1.0, 0.0, 0.0], Some("s1"), 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn test_embedding_text_shape_and_truncation() {
        let mut rec = new_record(
            "s",
            ActionType::ToolCall,
            Actor::Agent,
            Some("generate"),
            None,
            serde_json::json!({"elements": ["Ni", "Cr"]}),
        );
        rec.output_json = Some(serde_json::json!({"result": "ok"}));
        let text = embedding_text(&rec);
        assert!(text.starts_with("generate "));
        assert!(text.contains("Ni"));
        assert!(text.contains("result"));

        rec.output_json = Some(serde_json::Value::String("x".repeat(10_000)));
        assert_eq!(embedding_text(&rec).chars().count(), 2_000);
    }

    // ── VS1 / F5: structured status + exit_code round-trip + migration ──

    #[tokio::test]
    async fn f5_status_and_exit_code_round_trip() {
        // A failed tool call's record must persist status:error + exit_code
        // so "which runs failed" is a real query.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut rec = new_record(
            "sess-f5",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_python"),
            None,
            serde_json::json!({"code": "raise ValueError('boom')"}),
        );
        rec.output_json = Some(serde_json::json!({
            "success": false, "exit_code": 1, "stderr": "ValueError: boom"
        }));
        rec.status = Some("error".to_string());
        rec.exit_code = Some(1);

        store.record(&rec).await.unwrap();
        let results = store.query_by_session("sess-f5").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status.as_deref(), Some("error"));
        assert_eq!(results[0].exit_code, Some(1));
    }

    #[tokio::test]
    async fn f5_status_ok_round_trips_too() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut rec = new_record(
            "sess-f5-ok",
            ActionType::ToolCall,
            Actor::Agent,
            Some("execute_bash"),
            None,
            serde_json::json!({"command": "echo hi"}),
        );
        rec.output_json = Some(serde_json::json!({"success": true, "exit_code": 0}));
        rec.status = Some("ok".to_string());
        rec.exit_code = Some(0);

        store.record(&rec).await.unwrap();
        let results = store.query_by_session("sess-f5-ok").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status.as_deref(), Some("ok"));
        assert_eq!(results[0].exit_code, Some(0));
    }

    #[tokio::test]
    async fn f5_legacy_record_without_status_round_trips_as_none() {
        // A legacy row (or a non-tool record where the notion doesn't apply)
        // has status None and exit_code None. Must round-trip honestly as
        // None, not a defaulted lie.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let rec = new_record(
            "sess-f5-legacy",
            ActionType::LlmCall,
            Actor::Agent,
            None,
            None,
            serde_json::json!({"prompt": "hi"}),
        );
        // Deliberately leave status / exit_code at their None defaults.
        store.record(&rec).await.unwrap();
        let results = store.query_by_session("sess-f5-legacy").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, None, "legacy record stays None");
        assert_eq!(results[0].exit_code, None);
    }

    #[tokio::test]
    async fn f5_schema_migration_is_idempotent_on_reopen() {
        // Simulate a pre-existing user DB: create it once, then open it again.
        // The guarded ALTER must tolerate "duplicate column name" on the second
        // open rather than erroring. Use a temp file (not :memory:) so the DB
        // persists across two ProvenanceStore::open calls.
        let tmp = std::env::temp_dir().join(format!(
            "prism-f5-migration-{}.db",
            Uuid::new_v4().as_simple()
        ));
        // First open creates the 15-column table + runs the (fresh) ALTERs.
        {
            let store = ProvenanceStore::open(&tmp).await.unwrap();
            let rec = new_record(
                "sess-mig",
                ActionType::ToolCall,
                Actor::Agent,
                Some("web"),
                None,
                serde_json::json!({"q": "x"}),
            );
            store.record(&rec).await.unwrap();
        }
        // Second open must not fail on the already-present columns.
        {
            let store = ProvenanceStore::open(&tmp).await.unwrap();
            let results = store.query_by_session("sess-mig").await.unwrap();
            assert_eq!(results.len(), 1, "data survives reopen");
            // And a fresh write after reopen still works.
            let rec2 = new_record(
                "sess-mig",
                ActionType::ToolCall,
                Actor::Agent,
                Some("web"),
                None,
                serde_json::json!({"q": "y"}),
            );
            store.record(&rec2).await.unwrap();
        }
        let _ = std::fs::remove_file(&tmp);

        // Third check: opening :memory: (fresh each time) also works — the
        // ALTER against a just-created table must not raise either.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let rec = new_record(
            "sess-fresh",
            ActionType::ToolCall,
            Actor::Agent,
            Some("read_file"),
            None,
            serde_json::json!({"path": "a.txt"}),
        );
        store.record(&rec).await.unwrap();
        assert_eq!(store.query_by_session("sess-fresh").await.unwrap().len(), 1);
    }

    // ── VS3: queryable failures + outcome-aware stats ──────────────────

    /// Helper: a tool-call record with an explicit outcome.
    fn outcome_record(
        session: &str,
        tool: &str,
        status: Option<&str>,
        exit_code: Option<i64>,
    ) -> ProvenanceRecord {
        let mut rec = new_record(
            session,
            ActionType::ToolCall,
            Actor::Agent,
            Some(tool),
            None,
            serde_json::json!({}),
        );
        rec.status = status.map(str::to_string);
        rec.exit_code = exit_code;
        rec
    }

    #[tokio::test]
    async fn stats_counts_ok_error_and_unknown() {
        // VS3: stats() must answer the failure-rate question, not just a flat
        // total. 1 ok + 2 error + 1 status-less -> the three buckets.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        store
            .record(&outcome_record("s", "execute_bash", Some("ok"), Some(0)))
            .await
            .unwrap();
        store
            .record(&outcome_record(
                "s",
                "execute_python",
                Some("error"),
                Some(1),
            ))
            .await
            .unwrap();
        store
            .record(&outcome_record(
                "s",
                "execute_python",
                Some("error"),
                Some(-11),
            ))
            .await
            .unwrap();
        // A legacy / non-tool row: no status.
        let legacy = new_record(
            "s",
            ActionType::LlmCall,
            Actor::Agent,
            None,
            None,
            serde_json::json!({"prompt": "hi"}),
        );
        store.record(&legacy).await.unwrap();

        let s = store.stats().await.unwrap();
        assert_eq!(s.total_records, 4);
        assert_eq!(s.ok_records, 1, "ok bucket");
        assert_eq!(
            s.error_records, 2,
            "error bucket — the 'which runs failed' count"
        );
        assert_eq!(s.other_records, 1, "status-less rows land in other");
        // Invariant: the buckets partition the total.
        assert_eq!(
            s.ok_records + s.error_records + s.other_records,
            s.total_records
        );
    }

    #[tokio::test]
    async fn stats_empty_store_is_all_zeros() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let s = store.stats().await.unwrap();
        assert_eq!(s.total_records, 0);
        assert_eq!(s.ok_records, 0);
        assert_eq!(s.error_records, 0);
        assert_eq!(s.other_records, 0);
    }

    #[tokio::test]
    async fn query_failures_returns_only_errors_ordered_desc() {
        // The direct "which runs failed?" query: only status='error' rows, and
        // newest first so the most recent failure (the one to debug) leads.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        // newest-first is by timestamp; give each an explicit, increasing ts.
        let mut first = outcome_record("s", "execute_python", Some("error"), Some(1));
        first.timestamp = "2026-01-01T00:00:00+00:00".to_string();
        let mut ok = outcome_record("s", "execute_bash", Some("ok"), Some(0));
        ok.timestamp = "2026-01-02T00:00:00+00:00".to_string();
        let mut second = outcome_record("s", "execute_python", Some("error"), Some(-11));
        second.timestamp = "2026-01-03T00:00:00+00:00".to_string();
        for rec in [&first, &ok, &second] {
            store.record(rec).await.unwrap();
        }

        let failures = store.query_failures(None, 100).await.unwrap();
        assert_eq!(failures.len(), 2, "only the two error rows return");
        assert!(
            failures
                .iter()
                .all(|r| r.status.as_deref() == Some("error"))
        );
        // Descending by timestamp -> second (Jan 3) before first (Jan 1).
        assert_eq!(failures[0].tool_name.as_deref(), Some("execute_python"));
        assert_eq!(failures[0].exit_code, Some(-11));
        assert_eq!(failures[1].exit_code, Some(1));
    }

    #[tokio::test]
    async fn query_failures_scoped_to_session() {
        // Scoped query must not leak another session's failures.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        store
            .record(&outcome_record(
                "session-a",
                "execute_python",
                Some("error"),
                Some(1),
            ))
            .await
            .unwrap();
        store
            .record(&outcome_record(
                "session-b",
                "execute_python",
                Some("error"),
                Some(2),
            ))
            .await
            .unwrap();

        let a = store.query_failures(Some("session-a"), 100).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].session_id, "session-a");
        assert_eq!(a[0].exit_code, Some(1));

        let all = store.query_failures(None, 100).await.unwrap();
        assert_eq!(all.len(), 2, "None spans every session");
    }

    #[tokio::test]
    async fn query_failures_respects_limit() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        for i in 0..5 {
            let mut rec = outcome_record("s", "execute_python", Some("error"), Some(i));
            // Distinct timestamps so ordering is deterministic; DESC keeps the
            // last-written (latest ts) when limited to 2.
            rec.timestamp = format!("2026-01-0{}T00:00:00+00:00", i + 1);
            store.record(&rec).await.unwrap();
        }
        let top = store.query_failures(None, 2).await.unwrap();
        assert_eq!(top.len(), 2, "limit caps the result");
    }

    #[tokio::test]
    async fn query_failures_clamps_oversized_limit() {
        // SECURITY: a caller (meta-tool / HTTP input) can pass any usize. The
        // store must clamp it before the `as i64` cast — otherwise a huge value
        // wraps to a NEGATIVE i64 and SQLite/Turso reads a negative LIMIT as
        // UNBOUNDED, dumping the whole store. Seed 1200 failures and request
        // usize::MAX; the capped result must be exactly 1000, not 1200.
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        for i in 0..1200 {
            let mut rec = outcome_record("s", "execute_python", Some("error"), Some(i));
            rec.timestamp = format!("2026-01-{:04}T00:00:00+00:00", i);
            store.record(&rec).await.unwrap();
        }
        let capped = store.query_failures(None, usize::MAX).await.unwrap();
        assert_eq!(
            capped.len(),
            1000,
            "oversized limit is clamped to 1000, never unbounded"
        );
        // A modest limit still works through the same clamp (it's a min()).
        assert_eq!(store.query_failures(None, 5).await.unwrap().len(), 5);
    }

    fn session_index_entry(
        session_id: &str,
        source_path: &str,
        project_cwd: Option<&str>,
        updated_at: f64,
    ) -> SessionIndexEntry {
        SessionIndexEntry {
            session_id: session_id.to_string(),
            source_path: source_path.to_string(),
            file_path: format!("{source_path}/{session_id}.jsonl"),
            project_cwd: project_cwd.map(str::to_string),
            created_at: updated_at - 10.0,
            updated_at,
            model: "gpt-5.6".to_string(),
            turn_count: 12,
            compaction_count: 2,
            parent_session_id: Some("parent-session".to_string()),
            branch_name: Some("candidate-branch".to_string()),
            title: Some("Nickel nickel Alloy".to_string()),
            summary: Some("Creep-resistant screening".to_string()),
            title_source: Some("model".to_string()),
            title_turn: 8,
            models: vec!["gpt-5.5".to_string(), "gpt-5.6".to_string()],
            preview: Some("Turbine blade candidates".to_string()),
            size_bytes: 4_096,
        }
    }

    #[tokio::test]
    async fn session_metadata_schema_declares_all_query_indexes() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut rows = store
            .conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND \
                 tbl_name IN ('session_metadata', 'session_search_terms')",
                (),
            )
            .await
            .unwrap();
        let mut names = std::collections::HashSet::new();
        while let Some(row) = rows.next().await.unwrap() {
            names.insert(get_str(&row, 0).unwrap());
        }
        for (index, expected_columns) in [
            ("idx_session_metadata_updated_at", vec!["updated_at"]),
            (
                "idx_session_metadata_source_updated",
                vec!["source_path", "updated_at"],
            ),
            (
                "idx_session_metadata_project_updated",
                vec!["project_cwd", "updated_at"],
            ),
            ("idx_session_metadata_parent", vec!["parent_session_id"]),
            (
                "idx_session_metadata_source_indexed",
                vec!["source_path", "indexed_at"],
            ),
            (
                "idx_session_search_terms_term_session",
                vec!["term", "session_id", "source_path"],
            ),
        ] {
            assert!(names.contains(index), "missing index {index}");
            let mut column_rows = store
                .conn
                .query(&format!("PRAGMA index_info({index})"), ())
                .await
                .unwrap();
            let mut actual_columns = Vec::new();
            while let Some(row) = column_rows.next().await.unwrap() {
                actual_columns.push(get_str(&row, 2).unwrap());
            }
            assert_eq!(
                actual_columns, expected_columns,
                "wrong columns for {index}"
            );
        }

        let mut tables = store
            .conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND \
                 name IN ('session_metadata', 'session_metadata_reconciliation', \
                          'session_search_terms')",
                (),
            )
            .await
            .unwrap();
        let mut table_names = std::collections::HashSet::new();
        while let Some(row) = tables.next().await.unwrap() {
            table_names.insert(get_str(&row, 0).unwrap());
        }
        assert_eq!(table_names.len(), 3, "all session mirror tables exist");
    }

    #[tokio::test]
    async fn session_metadata_round_trips_and_filters_with_exact_and_search() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let alpha = session_index_entry("alpha", "/sessions/main", Some("/work/alloys"), 150.0);
        let mut beta = session_index_entry("beta", "/sessions/main", Some("/work/alloys"), 220.0);
        beta.title = Some("Nickel phase diagram".to_string());
        beta.summary = Some("Equilibrium calculations".to_string());
        beta.preview = None;
        let mut gamma = session_index_entry("gamma", "/sessions/other", None, 310.0);
        gamma.title = Some("Ceramic toughness".to_string());
        gamma.summary = None;
        gamma.preview = Some("Fracture test".to_string());

        for entry in [&alpha, &beta, &gamma] {
            store.upsert_session_metadata(entry).await.unwrap();
        }

        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    limit: 1,
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![gamma.clone()],
            "limit applies after newest-first ordering"
        );
        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    limit: 1,
                    offset: 1,
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![beta.clone()],
            "offset pages through the stable newest-first ordering"
        );

        let alpha_rows = store
            .list_session_metadata(&SessionIndexQuery {
                source_path: Some("/sessions/main".to_string()),
                project_cwd: Some("/work/alloys".to_string()),
                updated_after: Some(100.0),
                updated_before: Some(200.0),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(alpha_rows, vec![alpha.clone()]);
        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    updated_after: Some(alpha.updated_at),
                    updated_before: Some(alpha.updated_at),
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![alpha.clone()],
            "update-window bounds are inclusive"
        );

        let searched = store
            .list_session_metadata(&SessionIndexQuery {
                text: Some("NICKEL creep nickel".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(searched, vec![alpha.clone()]);
        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    text: Some("turbine candidates".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![alpha.clone()],
            "preview tokens are searchable"
        );
        assert!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    text: Some("nickel missing".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .is_empty(),
            "every search token is required"
        );
        assert!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    text: Some("nick".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .is_empty(),
            "search is exact-token rather than substring matching"
        );

        let excessive_terms = (0..=MAX_SESSION_INDEX_SEARCH_TERMS)
            .map(|index| format!("term{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let error = store
            .list_session_metadata(&SessionIndexQuery {
                text: Some(excessive_terms),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("distinct terms"));
        let error = store
            .list_session_metadata(&SessionIndexQuery {
                text: Some("x".repeat(MAX_SESSION_INDEX_SEARCH_BYTES + 1)),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"));

        let mut term_count = store
            .conn
            .query(
                "SELECT COUNT(*) FROM session_search_terms \
                 WHERE source_path = ?1 AND session_id = ?2",
                [
                    Value::Text(alpha.source_path.clone()),
                    Value::Text(alpha.session_id.clone()),
                ],
            )
            .await
            .unwrap();
        let count = term_count
            .next()
            .await
            .unwrap()
            .unwrap()
            .get_value(0)
            .unwrap()
            .as_integer()
            .copied();
        assert_eq!(count, Some(8), "repeated title tokens are deduplicated");

        let mut refreshed = alpha.clone();
        refreshed.title = Some("Cobalt study".to_string());
        refreshed.summary = None;
        refreshed.preview = None;
        refreshed.updated_at = 400.0;
        store.upsert_session_metadata(&refreshed).await.unwrap();
        assert!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    text: Some("creep".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .is_empty(),
            "upsert removes stale terms"
        );
        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    text: Some("cobalt".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![refreshed]
        );
    }

    #[tokio::test]
    async fn session_metadata_source_replacement_and_purge_are_isolated() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let old = session_index_entry("old", "/sessions/main", Some("/work/old"), 100.0);
        let other = session_index_entry("other", "/sessions/other", Some("/work/other"), 200.0);
        store.upsert_session_metadata(&old).await.unwrap();
        store.upsert_session_metadata(&other).await.unwrap();

        let mut replacement =
            session_index_entry("new", "/sessions/main", Some("/work/new"), 300.0);
        replacement.title = Some("Replacement marker".to_string());
        replacement.summary = None;
        replacement.preview = None;
        let scan_started_at = session_index_timestamp();
        let reconciled_at = scan_started_at + 0.001;
        store
            .replace_session_metadata_source(
                "/sessions/main",
                std::slice::from_ref(&replacement),
                &[],
                scan_started_at,
                reconciled_at,
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .session_metadata_reconciled_at("/sessions/main")
                .await
                .unwrap(),
            Some(reconciled_at)
        );
        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    source_path: Some("/sessions/main".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![replacement.clone()]
        );
        assert!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    text: Some("creep".to_string()),
                    source_path: Some("/sessions/main".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .is_empty(),
            "replacement removes the old source's search terms"
        );

        let mut wrong_source = replacement.clone();
        wrong_source.source_path = "/sessions/other".to_string();
        let error = store
            .replace_session_metadata_source(
                "/sessions/main",
                std::slice::from_ref(&wrong_source),
                &[],
                reconciled_at + 1.0,
                reconciled_at + 2.0,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("expected"));
        assert_eq!(
            store
                .session_metadata_reconciled_at("/sessions/main")
                .await
                .unwrap(),
            Some(reconciled_at),
            "validation happens before the purge transaction"
        );

        store
            .delete_session_metadata_source("/sessions/main")
            .await
            .unwrap();
        assert!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    source_path: Some("/sessions/main".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .session_metadata_reconciled_at("/sessions/main")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .list_session_metadata(&SessionIndexQuery {
                    source_path: Some("/sessions/other".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap(),
            vec![other],
            "purging one source does not touch another"
        );
    }

    #[tokio::test]
    async fn session_rebuild_preserves_rows_indexed_after_its_scan_started() {
        let store = ProvenanceStore::open(Path::new(":memory:")).await.unwrap();
        let mut scanned = session_index_entry("scanned", "/sessions/main", None, 100.0);
        scanned.title = Some("Stale snapshot".to_string());
        let concurrent = session_index_entry("concurrent", "/sessions/main", None, 200.0);
        store.upsert_session_metadata(&scanned).await.unwrap();
        store.upsert_session_metadata(&concurrent).await.unwrap();
        let mut updated_while_scanning = scanned.clone();
        updated_while_scanning.updated_at = 300.0;
        updated_while_scanning.title = Some("Concurrent update".to_string());
        store
            .upsert_session_metadata(&updated_while_scanning)
            .await
            .unwrap();

        // Pin deterministic observation times: both rows changed after the
        // snapshot began. The missing row and the newer same-id row must both
        // survive the stale replacement input.
        store
            .conn
            .execute(
                "UPDATE session_metadata SET indexed_at = 150.0 \
                 WHERE source_path = '/sessions/main'",
                (),
            )
            .await
            .unwrap();
        store
            .replace_session_metadata_source(
                "/sessions/main",
                std::slice::from_ref(&scanned),
                &[],
                100.0,
                200.0,
            )
            .await
            .unwrap();

        let listed = store
            .list_session_metadata(&SessionIndexQuery {
                source_path: Some("/sessions/main".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let ids = listed
            .iter()
            .map(|entry| entry.session_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            ids,
            std::collections::BTreeSet::from(["concurrent", "scanned"])
        );
        assert_eq!(
            listed
                .iter()
                .find(|entry| entry.session_id == "scanned")
                .and_then(|entry| entry.title.as_deref()),
            Some("Concurrent update"),
            "a stale rebuild snapshot must not overwrite a newer same-id row"
        );
    }
}

/// The provenance store this process must open.
///
/// `$PRISM_PROVENANCE_DB` wins when set and non-empty; otherwise
/// `$HOME/.prism/provenance.db`.
///
/// EVERY OPEN RESOLVES HERE. Six call sites built this path by hand and so
/// silently ignored the override: `cli::ontology_cmd`, `ingest::pipeline`,
/// `workflows`, and `server::handlers::query` twice. Only the agent's copy
/// honoured it, which is how `prism query` came to read an empty default store
/// while the corpus the operator had selected sat elsewhere — the agent then
/// reported, truthfully and wrongly, that the corpus did not contain what it
/// was asked about.
///
/// It is not a hypothetical: pointing the ontology tools at a corpus with
/// `PRISM_PROVENANCE_DB` opened the DEFAULT store instead, collided with the
/// running node's lock, and failed. A resolver that only some callers use is
/// not a resolver.
#[must_use]
pub fn store_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("PRISM_PROVENANCE_DB")
        && !path.is_empty()
    {
        return std::path::PathBuf::from(path);
    }
    default_store_path()
}

/// `$HOME/.prism/provenance.db`, or a relative fallback when HOME is unset.
#[must_use]
pub fn default_store_path() -> std::path::PathBuf {
    std::env::var_os("HOME").map_or_else(
        || std::path::PathBuf::from(".prism/provenance.db"),
        |home| std::path::PathBuf::from(home).join(".prism/provenance.db"),
    )
}

#[cfg(test)]
mod store_path_tests {
    /// The override exists so an operator can point PRISM at a chosen corpus.
    /// A caller that builds the path by hand silently ignores it, and the
    /// symptom is not an error — it is a confident answer about the wrong
    /// database.
    #[test]
    fn the_override_wins_and_an_empty_value_does_not() {
        // Serialised against other env-touching tests in this crate.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("PRISM_PROVENANCE_DB");

        unsafe { std::env::set_var("PRISM_PROVENANCE_DB", "/tmp/chosen-corpus.db") };
        assert_eq!(
            super::store_path(),
            std::path::PathBuf::from("/tmp/chosen-corpus.db")
        );

        // Empty must NOT shadow the default, or `VAR=` would break every open.
        unsafe { std::env::set_var("PRISM_PROVENANCE_DB", "") };
        assert_eq!(super::store_path(), super::default_store_path());

        unsafe { std::env::remove_var("PRISM_PROVENANCE_DB") };
        assert_eq!(super::store_path(), super::default_store_path());

        if let Some(value) = previous {
            unsafe { std::env::set_var("PRISM_PROVENANCE_DB", value) };
        }
    }

    /// The override only works if EVERY writer honours it. Eight sites
    /// hand-built `$HOME/.prism/provenance.db`, so with the override set,
    /// ingest / papers / repair / matkg / mesh-pull wrote the home store while
    /// `prism query` read the chosen one. Nothing errored — the operator got a
    /// confident "0 results" about a database nothing had written to.
    ///
    /// No type can catch a path built by hand, so this greps the workspace.
    /// Doc comments and tests may still name the default (they describe it);
    /// what must not exist is a `.join(".prism/provenance.db")` outside the
    /// resolver, which is a path being CONSTRUCTED.
    #[test]
    fn no_crate_hand_builds_the_default_store_path() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ is the parent of this crate");

        let mut offenders = Vec::new();
        let mut stack = vec![crates.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // This file IS the resolver; `tests/` legitimately builds
                // scratch paths under a TempDir to prove isolation.
                let display = path.display().to_string();
                if display.ends_with("provenance/src/lib.rs") || display.contains("/tests/") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                // Only PRODUCTION code. An inline `#[cfg(test)]` module builds
                // this path on purpose — under a `TempDir`, to prove isolation
                // — and every such site in the workspace today does exactly
                // that. Tests are laid out at the bottom of the file, so
                // stopping at the first attribute is enough and keeps the lint
                // a grep rather than a parser.
                let production = text
                    .split_once("\n#[cfg(test)]")
                    .map_or(text.as_str(), |(before, _)| before);
                for (offset, line) in production.lines().enumerate() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") || trimmed.starts_with("///") {
                        continue;
                    }
                    if line.contains(".join(\".prism/provenance.db\")") {
                        offenders.push(format!("{display}:{}", offset + 1));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these sites build the default store path by hand and so ignore \
             $PRISM_PROVENANCE_DB — call prism_provenance::store_path() instead:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// A9/A33 together: the override is honoured AND the directory it names
    /// does not have to exist yet. A fresh install has no `~/.prism`, and
    /// three of fifteen callers created the parent while the rest did not.
    #[tokio::test]
    async fn open_creates_a_missing_parent_directory() {
        let temp = std::env::temp_dir().join(format!("prism_open_{}", uuid::Uuid::new_v4()));
        let db = temp.join("nested/deeper/provenance.db");
        assert!(!temp.exists(), "the fixture must start absent");

        let store = super::ProvenanceStore::open(&db)
            .await
            .expect("open must create the directory it was given, not fail on it");
        drop(store);

        assert!(
            db.exists(),
            "the store file must exist after a successful open"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }
}
