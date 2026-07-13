// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Data ingestion and ontology pipeline for PRISM nodes.
//!
//! Converts raw data (CSV, Parquet, databases) into structured, queryable
//! knowledge through an LLM-driven pipeline:
//!
//! ```text
//! Raw Data → Schema Detection → Entity Extraction → Graph + Embeddings
//!                                     (LLM)         (bundled Turso store)
//! ```
//!
//! The core trait [`OntologyConstructor`] is pluggable — ships with an LLM-based
//! implementation and is designed so that a future DMMS (Differentiable Manifold
//! Materials Science) engine can slot in behind the same interface.

pub mod connectors;
pub mod graph_validation;
/// Re-export LLM client from the standalone `prism-llm` crate.
/// This keeps backward compatibility — existing code using `prism_ingest::llm::*`
/// and `prism_ingest::LlmConfig` continues to work.
pub use prism_llm as llm;
pub use prism_llm::LlmConfig;
pub mod local_facts;
pub mod mapping;
pub mod ontology;
pub mod pipeline;
pub mod schema;
pub mod text_extract;
pub mod validation;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Pluggable ontology construction — ships with LLM impl, DMMS slots in later.
#[async_trait]
pub trait OntologyConstructor: Send + Sync {
    async fn analyze_schema(&self, source: &DataSource) -> Result<SchemaAnalysis>;
    async fn extract_entities(
        &self,
        source: &DataSource,
        schema: &SchemaAnalysis,
    ) -> Result<EntitySet>;
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
        // Defaults are empty — real values come from prism.toml or server config
        assert!(cfg.base_url.is_empty());
        assert!(cfg.model.is_empty());
        assert_eq!(cfg.max_sample_rows, 10);
    }

    // --- LlmConfig serde ---

    #[test]
    fn llm_config_roundtrip() {
        let cfg = LlmConfig {
            base_url: "http://example.com".into(),
            model: "gemma-3-27b".into(),
            api_key: None,
            embedding_model: Some("nomic-embed-text".into()),
            max_sample_rows: 5,
            timeout_secs: 60,
            context_window: None,
            max_output_tokens: None,
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
        assert_eq!(cfg.timeout_secs, 300);
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
