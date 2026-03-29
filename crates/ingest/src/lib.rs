//! Data ingestion and ontology pipeline for PRISM nodes.
//!
//! Converts raw data (CSV, Parquet, databases) into structured, queryable
//! knowledge through an LLM-driven pipeline:
//!
//! ```text
//! Raw Data → Schema Detection → Entity Extraction → Graph Construction → Embeddings
//!                                    (Ollama)           (Neo4j)           (Qdrant)
//! ```
//!
//! The core trait [`OntologyConstructor`] is pluggable — ships with an LLM-based
//! implementation and is designed so that a future DMMS (Differentiable Manifold
//! Materials Science) engine can slot in behind the same interface.

pub mod pipeline;
pub mod schema;
pub mod ontology;
pub mod graph;
pub mod embeddings;
pub mod connectors;
pub mod validation;
pub mod graph_validation;
pub mod nl_query;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Pluggable ontology construction — ships with LLM impl, DMMS slots in later.
#[async_trait]
pub trait OntologyConstructor: Send + Sync {
    async fn analyze_schema(&self, source: &DataSource) -> Result<SchemaAnalysis>;
    async fn extract_entities(&self, source: &DataSource, schema: &SchemaAnalysis) -> Result<EntitySet>;
    async fn build_graph(&self, entities: &EntitySet) -> Result<GraphUpdate>;
    async fn generate_embeddings(&self, entities: &EntitySet) -> Result<EmbeddingBatch>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSource {
    pub path: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaAnalysis {
    pub columns: Vec<String>,
    pub detected_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySet {
    pub entities: Vec<Entity>,
    pub relationships: Vec<Relationship>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_type: String,
    pub name: String,
    pub properties: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub from: String,
    pub rel_type: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphUpdate {
    pub nodes_created: usize,
    pub edges_created: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingBatch {
    pub vectors: Vec<Vec<f32>>,
    pub ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<usize>,
}

/// Configuration for connecting to an LLM backend (Ollama or platform API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Base URL of the LLM API (e.g. "http://localhost:11434" for Ollama).
    pub base_url: String,
    /// Model name to use (e.g. "qwen3.5-9b-prism" or "qwen2.5:7b").
    pub model: String,
    /// Maximum number of sample rows to include in the extraction prompt.
    #[serde(default = "default_max_sample_rows")]
    pub max_sample_rows: usize,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_max_sample_rows() -> usize { 10 }
fn default_timeout_secs() -> u64 { 120 }

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".into(),
            model: "qwen2.5:7b".into(),
            max_sample_rows: 10,
            timeout_secs: 120,
        }
    }
}

/// Configuration for connecting to Neo4j.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Neo4jConfig {
    /// HTTP transaction API endpoint (e.g. "http://localhost:7474").
    pub base_url: String,
    /// Database name (default: "neo4j").
    #[serde(default = "default_neo4j_db")]
    pub database: String,
    /// Basic auth username.
    pub username: String,
    /// Basic auth password.
    pub password: String,
}

fn default_neo4j_db() -> String { "neo4j".into() }

impl Default for Neo4jConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:7474".into(),
            database: "neo4j".into(),
            username: "neo4j".into(),
            password: "neo4j".into(),
        }
    }
}

/// Configuration for connecting to Qdrant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    /// Qdrant HTTP API endpoint (e.g. "http://localhost:6333").
    pub base_url: String,
    /// Collection name for PRISM embeddings.
    #[serde(default = "default_qdrant_collection")]
    pub collection: String,
    /// Optional API key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

fn default_qdrant_collection() -> String { "prism_embeddings".into() }

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:6333".into(),
            collection: "prism_embeddings".into(),
            api_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_set_roundtrip() {
        let set = EntitySet {
            entities: vec![Entity {
                entity_type: "Alloy".into(),
                name: "Nb25Mo25Ta25W25".into(),
                properties: serde_json::json!({"system": "NbMoTaW"}),
            }],
            relationships: vec![Relationship {
                from: "Nb25Mo25Ta25W25".into(),
                rel_type: "CONTAINS".into(),
                to: "Nb".into(),
                weight: Some(0.25),
                order: None,
            }],
        };
        let json = serde_json::to_string(&set).unwrap();
        let parsed: EntitySet = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entities.len(), 1);
        assert_eq!(parsed.relationships[0].weight, Some(0.25));
    }

    #[test]
    fn llm_config_defaults() {
        let cfg = LlmConfig::default();
        assert_eq!(cfg.base_url, "http://localhost:11434");
        assert_eq!(cfg.max_sample_rows, 10);
    }

    // --- LlmConfig serde ---

    #[test]
    fn llm_config_roundtrip() {
        let cfg = LlmConfig {
            base_url: "http://example.com".into(),
            model: "qwen3:9b".into(),
            max_sample_rows: 5,
            timeout_secs: 60,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: LlmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.base_url, cfg.base_url);
        assert_eq!(parsed.model, cfg.model);
        assert_eq!(parsed.max_sample_rows, 5);
        assert_eq!(parsed.timeout_secs, 60);
    }

    #[test]
    fn llm_config_minimal_json_fills_defaults() {
        // Only required fields — defaults must fill in max_sample_rows and timeout_secs.
        let json = r#"{"base_url":"http://localhost:11434","model":"qwen2.5:7b"}"#;
        let cfg: LlmConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_sample_rows, 10);
        assert_eq!(cfg.timeout_secs, 120);
    }

    // --- Neo4jConfig serde ---

    #[test]
    fn neo4j_config_roundtrip() {
        let cfg = Neo4jConfig {
            base_url: "http://neo4j.example.com:7474".into(),
            database: "materials".into(),
            username: "admin".into(),
            password: "secret".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: Neo4jConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.base_url, cfg.base_url);
        assert_eq!(parsed.database, "materials");
        assert_eq!(parsed.username, "admin");
    }

    #[test]
    fn neo4j_config_minimal_json_fills_default_database() {
        let json = r#"{"base_url":"http://localhost:7474","username":"neo4j","password":"neo4j"}"#;
        let cfg: Neo4jConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.database, "neo4j");
    }

    // --- QdrantConfig serde ---

    #[test]
    fn qdrant_config_roundtrip() {
        let cfg = QdrantConfig {
            base_url: "http://qdrant.example.com:6333".into(),
            collection: "my_embeddings".into(),
            api_key: Some("tok-abc123".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: QdrantConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.collection, "my_embeddings");
        assert_eq!(parsed.api_key, Some("tok-abc123".into()));
    }

    #[test]
    fn qdrant_config_minimal_json_fills_default_collection() {
        let json = r#"{"base_url":"http://localhost:6333"}"#;
        let cfg: QdrantConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.collection, "prism_embeddings");
        assert!(cfg.api_key.is_none());
    }

    // --- EntitySet edge cases ---

    #[test]
    fn entity_set_empty_entities_and_relationships() {
        let set = EntitySet {
            entities: vec![],
            relationships: vec![],
        };
        let json = serde_json::to_string(&set).unwrap();
        let parsed: EntitySet = serde_json::from_str(&json).unwrap();
        assert!(parsed.entities.is_empty());
        assert!(parsed.relationships.is_empty());
    }

    // --- Relationship skip_serializing_if ---

    #[test]
    fn relationship_weight_none_not_serialized() {
        let rel = Relationship {
            from: "A".into(),
            rel_type: "REL".into(),
            to: "B".into(),
            weight: None,
            order: None,
        };
        let json = serde_json::to_string(&rel).unwrap();
        assert!(!json.contains("\"weight\""));
        assert!(!json.contains("\"order\""));
    }

    #[test]
    fn relationship_order_none_not_serialized() {
        let rel = Relationship {
            from: "A".into(),
            rel_type: "PROCESSED_BY".into(),
            to: "B".into(),
            weight: None,
            order: None,
        };
        let json = serde_json::to_string(&rel).unwrap();
        assert!(!json.contains("\"order\""));
    }

    #[test]
    fn relationship_with_both_weight_and_order_roundtrip() {
        let rel = Relationship {
            from: "Mat".into(),
            rel_type: "PROCESSED_BY".into(),
            to: "Anneal".into(),
            weight: Some(1.0),
            order: Some(2),
        };
        let json = serde_json::to_string(&rel).unwrap();
        let parsed: Relationship = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.weight, Some(1.0));
        assert_eq!(parsed.order, Some(2));
    }

    // --- EmbeddingBatch serde ---

    #[test]
    fn embedding_batch_dimension_none_not_serialized() {
        let batch = EmbeddingBatch {
            vectors: vec![vec![0.1, 0.2]],
            ids: vec!["e1".into()],
            dimension: None,
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert!(!json.contains("\"dimension\""));
    }

    #[test]
    fn embedding_batch_with_dimension_roundtrip() {
        let batch = EmbeddingBatch {
            vectors: vec![vec![0.1f32, 0.9f32]],
            ids: vec!["entity-1".into()],
            dimension: Some(2),
        };
        let json = serde_json::to_string(&batch).unwrap();
        let parsed: EmbeddingBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.dimension, Some(2));
        assert_eq!(parsed.ids[0], "entity-1");
    }

    // --- DataSource serde ---

    #[test]
    fn data_source_roundtrip() {
        let ds = DataSource {
            path: "/data/alloys.csv".into(),
            format: "csv".into(),
        };
        let json = serde_json::to_string(&ds).unwrap();
        let parsed: DataSource = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, "/data/alloys.csv");
        assert_eq!(parsed.format, "csv");
    }

    // --- SchemaAnalysis serde ---

    #[test]
    fn schema_analysis_empty_columns_roundtrip() {
        let schema = SchemaAnalysis {
            columns: vec![],
            detected_types: vec![],
        };
        let json = serde_json::to_string(&schema).unwrap();
        let parsed: SchemaAnalysis = serde_json::from_str(&json).unwrap();
        assert!(parsed.columns.is_empty());
        assert!(parsed.detected_types.is_empty());
    }

    // --- GraphUpdate serde ---

    #[test]
    fn graph_update_roundtrip() {
        let gu = GraphUpdate {
            nodes_created: 42,
            edges_created: 17,
        };
        let json = serde_json::to_string(&gu).unwrap();
        let parsed: GraphUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.nodes_created, 42);
        assert_eq!(parsed.edges_created, 17);
    }

    #[test]
    fn graph_update_zero_values_roundtrip() {
        let gu = GraphUpdate {
            nodes_created: 0,
            edges_created: 0,
        };
        let json = serde_json::to_string(&gu).unwrap();
        let parsed: GraphUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.nodes_created, 0);
        assert_eq!(parsed.edges_created, 0);
    }
}
