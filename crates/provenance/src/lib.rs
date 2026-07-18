// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! PRISM Provenance Layer
//!
//! Every action in the materials discovery pipeline is recorded with
//! full traceability: which tool was called, with what parameters, by
//! which LLM, what data was read, what data was produced, and what
//! the chain of reasoning was.
//!
//! Backed by Turso — a ground-up Rust rewrite of SQLite. Local-first,
//! async-native, optionally synced to Turso Cloud for cross-device
//! provenance sharing. Every PRISM session gets its own Turso database
//! file (the "many-database architecture" — databases are files, not
//! processes, so there's no cold start).

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use turso::Value;
use uuid::Uuid;

pub mod emmo;
pub use emmo::{
    GraphEdge, GraphNode, LocalAssertion, LocalFact, LocalProvenance, RecalledFact,
    TraversalResult, assertion_id, canonical_key,
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
    User,
    System,
    Scheduler,
}

impl Actor {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::System => "system",
            Self::Scheduler => "scheduler",
        }
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

pub struct ProvenanceStore {
    conn: turso::Connection,
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
        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .context("failed to open Turso database")?;
        let conn = db.connect()?;

        Self::init_schema(&conn).await?;
        Ok(Self { conn })
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

    pub async fn query_chain(&self, record_id: &str) -> Result<Vec<ProvenanceRecord>> {
        let mut chain = Vec::new();
        let mut current_id = Some(record_id.to_string());
        while let Some(id) = current_id {
            let mut rows = self
                .conn
                .query(
                    "SELECT * FROM provenance_records WHERE id = ?1",
                    [Value::Text(id)],
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

    /// Brute-force cosine search over stored vectors (fine at session scale).
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

    pub async fn stats(&self) -> Result<ProvenanceStats> {
        let mut rows = self
            .conn
            .query("SELECT COUNT(*) FROM provenance_records", ())
            .await?;
        let total = if let Some(row) = rows.next().await? {
            row.get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0) as usize
        } else {
            0
        };
        Ok(ProvenanceStats {
            total_records: total,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct ProvenanceStats {
    pub total_records: usize,
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

        let chain = store.query_chain(&child.id).await.unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, parent.id);
        assert_eq!(chain[1].id, child.id);
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
}
