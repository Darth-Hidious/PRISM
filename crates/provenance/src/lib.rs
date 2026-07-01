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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    ToolCall,
    LlmCall,
    Ingest,
    Query,
    Gflownet,
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
            Self::Gflownet => "gflownet",
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

pub struct ProvenanceStore {
    conn: turso::Connection,
}

impl ProvenanceStore {
    pub async fn open(path: &Path) -> Result<Self> {
        let path_str = path.to_str().unwrap_or(":memory:");
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
                tags TEXT
            )"#,
            (),
        )
        .await?;

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
                    parent_id, material_ref, confidence, tags)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
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
        "gflownet" => ActionType::Gflownet,
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
    })
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
            ActionType::Gflownet,
            Actor::Agent,
            Some("gfn_sample"),
            Some("gemma-4-12b"),
            serde_json::json!({"n_samples": 64, "elements": ["Ni", "Cr", "Co"]}),
        );
        rec.material_ref = Some("Ni0.3 Cr0.4 Co0.3".to_string());
        rec.output_json = Some(serde_json::json!({"top_alloys": []}));
        rec.confidence = 0.85;

        store.record(&rec).await.unwrap();

        let results = store.query_by_session("test-session").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_name.as_deref(), Some("gfn_sample"));
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
            ActionType::Gflownet,
            Actor::Agent,
            Some("gfn_sample"),
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
}
