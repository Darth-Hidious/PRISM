// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Data ingestion and ontology pipeline for PRISM nodes.
//!
//! Converts delimited and columnar files (CSV, TSV, Parquet) into structured,
//! queryable knowledge through an LLM-driven pipeline:
//!
//! ```text
//! Raw Data → Schema Detection → Entity Extraction → Graph + Embeddings
//!                                     (LLM)         (bundled Turso store)
//! ```
//!
//! There is no database connector: [`connectors`] contains exactly
//! [`connectors::CsvConnector`] and [`connectors::ParquetConnector`],
//! dispatched through [`connectors::ConnectorRegistry`] — the one place
//! extensions are mapped to connectors. This header previously claimed
//! "databases" as a supported source.
//!
//! [`OntologyConstructor`] describes the intended plug point for a future DMMS
//! (Differentiable Manifold Materials Science) engine, but nothing consumes it
//! yet: the pipeline calls [`ontology::LlmOntologyConstructor`]'s inherent
//! `extract_entities_with_mapping` directly, and the workspace has no
//! `dyn OntologyConstructor` holder. Implementing the trait alone will not put
//! a new engine on the ingest path.
//!
//! The ontology VOCABULARY, by contrast, IS pluggable: [`ontologies`] holds
//! the process-wide registry of [`ontologies::Ontology`] adapters. The
//! pipeline resolves the ACTIVE ontology (`[ontology] id` in `prism.toml`)
//! and reads both the extraction prompt and graph validation from that one
//! adapter, so instructing and validating cannot drift apart.

pub mod batching;
pub mod classify;
pub mod connectors;
pub mod document;
pub mod extraction_schema;
pub mod graph_validation;
pub mod induction;
/// Re-export LLM client from the standalone `prism-llm` crate.
/// This keeps backward compatibility — existing code using `prism_ingest::llm::*`
/// and `prism_ingest::LlmConfig` continues to work.
pub use prism_llm as llm;
pub use prism_llm::LlmConfig;
pub mod local_facts;
pub mod mapping;
pub mod matkg;
pub mod ontologies;
pub mod ontology;
pub mod pipeline;
pub mod qudt_units;
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
    /// The measured value THIS relationship states — the per-subject
    /// attribution channel for measurements. A value on the target entity
    /// cannot say WHOSE value it is once several subjects reference the
    /// same property node (measured live 2026-08-10: one "yield strength"
    /// node carrying 880 was referenced by five alloys, and 880 MPa was
    /// stored as every alloy's yield strength — four falsehoods); a value
    /// on the relationship is per-edge by construction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// Unit spelling for `value`, resolved through the ONE controlled
    /// vocabulary (`prism_provenance::units::resolve_unit`) at fact
    /// mapping — never stored raw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
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
                value: None,
                unit: None,
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
        // Only required fields — defaults must fill in max_sample_rows and
        // timeout_secs. timeout_secs defaults to 0, meaning NO read deadline:
        // extracting facts from a paper with a reasoning model takes minutes,
        // and a default deadline does not make the science faster, it discards
        // the run partway through. An operator may still set one.
        let json = r#"{"base_url":"http://localhost:11434","model":"qwen2.5:7b"}"#;
        let cfg: LlmConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_sample_rows, 10);
        assert_eq!(cfg.timeout_secs, 0);
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
            value: None,
            unit: None,
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
            value: None,
            unit: None,
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
            value: None,
            unit: None,
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
