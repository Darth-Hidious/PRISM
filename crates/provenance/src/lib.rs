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
    ActivityDecoding, AssertionClassification, ClassifiedFactNodes, ClassifiedNode, ConditionValue,
    EvidenceClass, EvidenceContribution, EvidenceSource, FactNodeLabels, FactPayload, GraphEdge,
    GraphNode, LOCAL_TENANT, LocalAssertion, LocalFact, LocalProvenance, MaterialFact,
    MeasurementCondition, OntologyClassification, QudtUnit, RecalledFact, RecalledMaterialFact,
    SemanticEntityHit, StoreBusy, TraversalResult, assertion_id, canonical_key,
    conditioned_assertion_id, evidence_for_result,
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
    /// Read paths deliberately do NOT take the lock: a read joins an open
    /// transaction harmlessly (it sees the writer's uncommitted rows, same
    /// as SQLite on one connection) and holds nothing that a rollback could
    /// destroy. SEPARATE handles need no help either: they serialize via
    /// the database busy wait, which is what the concurrency tests exercise.
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

        // Enforce the evidence-table foreign key (`prov_assertion_evidence`
        // → `prov_assertion`). Like SQLite, Turso leaves foreign keys OFF
        // unless each connection opts in, and an unenforced FK is a lie in
        // the schema. This is the only declared FK in the store, so turning
        // enforcement on changes nothing else. Set before `init_schema` so
        // the migrations run under the same rules as ordinary writes
        // (`ON UPDATE CASCADE` keeps evidence rows attached across id
        // re-keys).
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
}
