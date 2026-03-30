use std::path::Path;

use anyhow::{bail, Result};
use polars::prelude::*;
use serde::{Deserialize, Serialize};
use tracing;

use crate::connectors::{CsvConnector, ParquetConnector};
use crate::embeddings::{QdrantVectorStore, VectorStore};
use crate::graph::{GraphStore, Neo4jGraphStore};
use crate::ontology::LlmOntologyConstructor;
use crate::schema::SchemaDetector;
use crate::validation::{self, ValidationReport};
use crate::{
    DataSource, EmbeddingBatch, EntitySet, GraphUpdate, LlmConfig, Neo4jConfig,
    OntologyConstructor, QdrantConfig, SchemaAnalysis,
};

/// Result of a complete ingest operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub source: DataSource,
    pub schema: SchemaAnalysis,
    pub validation: ValidationReport,
    pub row_count: usize,
    pub column_count: usize,
    /// Populated when LLM extraction runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entities: Option<EntitySet>,
    /// Populated when Neo4j graph upsert runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphUpdate>,
    /// Populated when Qdrant embedding upsert runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<EmbeddingBatch>,
}

/// Configuration for a full ingest pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// LLM config for entity extraction + embedding generation.
    pub llm: Option<LlmConfig>,
    /// Neo4j config for graph storage. If None, graph step is skipped.
    pub neo4j: Option<Neo4jConfig>,
    /// Qdrant config for vector storage. If None, embedding step is skipped.
    pub qdrant: Option<QdrantConfig>,
    /// Maximum sample rows to send to the LLM.
    pub max_sample_rows: usize,
    /// Custom ontology mapping rules (loaded from YAML).
    pub mapping: Option<crate::mapping::OntologyMapping>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            llm: Some(LlmConfig::default()),
            neo4j: Some(Neo4jConfig::default()),
            qdrant: Some(QdrantConfig::default()),
            max_sample_rows: 10,
            mapping: None,
        }
    }
}

/// Orchestrates the data ingestion pipeline:
///
/// ```text
/// Load → Schema Detection → Validation → LLM Extraction → Neo4j Graph → Qdrant Embeddings
/// ```
///
/// Each downstream step (LLM, Neo4j, Qdrant) is optional — controlled by `PipelineConfig`.
/// Without any configs, behaves like the original schema-only pipeline.
pub struct IngestPipeline {
    config: PipelineConfig,
}

impl IngestPipeline {
    pub fn new() -> Self {
        Self {
            config: PipelineConfig {
                llm: None,
                neo4j: None,
                qdrant: None,
                max_sample_rows: 10,
                mapping: None,
            },
        }
    }

    /// Create a pipeline with full end-to-end configuration.
    pub fn with_config(config: PipelineConfig) -> Self {
        Self { config }
    }

    /// Ingest a file (CSV or Parquet, detected from extension) through the full pipeline.
    pub async fn ingest_file(&self, path: &Path) -> Result<IngestResult> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "csv" | "tsv" => self.ingest_csv(path).await,
            "parquet" | "pq" => self.ingest_parquet(path).await,
            _ => bail!(
                "Unsupported file format: '.{ext}'. Supported: csv, tsv, parquet"
            ),
        }
    }

    /// Ingest a CSV file through the full pipeline.
    pub async fn ingest_csv(&self, path: &Path) -> Result<IngestResult> {
        let df = CsvConnector::load(path)?;
        let source = CsvConnector::to_data_source(path)?;
        self.run_pipeline(df, source).await
    }

    /// Ingest a Parquet file through the full pipeline.
    pub async fn ingest_parquet(&self, path: &Path) -> Result<IngestResult> {
        let df = ParquetConnector::load(path)?;
        let source = ParquetConnector::to_data_source(path)?;
        self.run_pipeline(df, source).await
    }

    /// Core pipeline: schema detection → validation → LLM extraction → graph → embeddings.
    async fn run_pipeline(&self, df: DataFrame, source: DataSource) -> Result<IngestResult> {
        let row_count = df.height();
        let column_count = df.width();

        // Step 1: Schema detection
        let schema = SchemaDetector::detect(&df)?;
        tracing::info!(
            columns = column_count,
            rows = row_count,
            "schema detected: {:?}",
            schema.columns
        );

        // Step 2: Validation
        let validation = validation::validate(&df);
        if !validation.passed {
            tracing::warn!(
                issues = validation.issues.len(),
                "data quality issues found"
            );
        }

        // Step 3: LLM entity extraction (if configured)
        let entities = if let Some(ref llm_config) = self.config.llm {
            let constructor = LlmOntologyConstructor::new(llm_config.clone());
            let sample_rows = extract_sample_rows(&df, self.config.max_sample_rows);

            tracing::info!(
                model = %llm_config.model,
                sample_rows = sample_rows.len(),
                "sending to LLM for entity extraction"
            );

            match constructor
                .extract_entities_with_mapping(&schema, &sample_rows, self.config.mapping.as_ref())
                .await
            {
                Ok(entities) => {
                    tracing::info!(
                        entities = entities.entities.len(),
                        relationships = entities.relationships.len(),
                        "LLM extraction complete"
                    );
                    Some(entities)
                }
                Err(e) => {
                    tracing::error!("LLM extraction failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Step 4: Neo4j graph upsert (if configured and entities exist)
        let graph = if let (Some(ref neo4j_config), Some(ref entity_set)) =
            (&self.config.neo4j, &entities)
        {
            let store = Neo4jGraphStore::new(neo4j_config.clone());
            match store.upsert(entity_set).await {
                Ok(update) => {
                    tracing::info!(
                        nodes = update.nodes_created,
                        edges = update.edges_created,
                        "graph upsert complete"
                    );
                    Some(update)
                }
                Err(e) => {
                    tracing::error!("Neo4j upsert failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Step 5: Qdrant embedding generation + upsert (if configured and entities exist)
        let embeddings = if let (Some(ref llm_config), Some(ref qdrant_config), Some(ref entity_set)) =
            (&self.config.llm, &self.config.qdrant, &entities)
        {
            let constructor = LlmOntologyConstructor::new(llm_config.clone());
            match constructor.generate_embeddings(entity_set).await {
                Ok(batch) if !batch.vectors.is_empty() => {
                    let vector_store = QdrantVectorStore::new(qdrant_config.clone());
                    match vector_store.upsert(&batch).await {
                        Ok(count) => {
                            tracing::info!(count, "vectors stored in Qdrant");
                            Some(batch)
                        }
                        Err(e) => {
                            tracing::error!("Qdrant upsert failed: {e}");
                            Some(batch) // still return embeddings even if store failed
                        }
                    }
                }
                Ok(batch) => Some(batch),
                Err(e) => {
                    tracing::error!("embedding generation failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        Ok(IngestResult {
            source,
            schema,
            validation,
            row_count,
            column_count,
            entities,
            graph,
            embeddings,
        })
    }
}

impl Default for IngestPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract up to `max_rows` sample rows from a DataFrame as `Vec<Vec<String>>`.
fn extract_sample_rows(df: &DataFrame, max_rows: usize) -> Vec<Vec<String>> {
    let n = df.height().min(max_rows);
    (0..n)
        .map(|i| {
            df.get_columns()
                .iter()
                .map(|col| {
                    col.get(i)
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|_| "null".into())
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sample_rows_from_dataframe() {
        let df = df!(
            "name" => &["Steel", "Copper", "Aluminum"],
            "density" => &[7.8, 8.96, 2.7]
        )
        .unwrap();

        let rows = extract_sample_rows(&df, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 2);
        assert!(rows[0][0].contains("Steel"));
    }

    #[test]
    fn extract_sample_rows_caps_at_df_height() {
        let df = df!("a" => &[1, 2]).unwrap();
        let rows = extract_sample_rows(&df, 100);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn extract_sample_rows_empty_df() {
        let df = DataFrame::empty();
        let rows = extract_sample_rows(&df, 10);
        assert!(rows.is_empty());
    }

    #[test]
    fn pipeline_config_default_has_all_backends() {
        let cfg = PipelineConfig::default();
        assert!(cfg.llm.is_some());
        assert!(cfg.neo4j.is_some());
        assert!(cfg.qdrant.is_some());
    }

    #[test]
    fn ingest_result_with_none_fields_serializes_cleanly() {
        let result = IngestResult {
            source: DataSource { path: "/tmp/test.csv".into(), format: "csv".into() },
            schema: SchemaAnalysis { columns: vec!["a".into()], detected_types: vec!["int".into()] },
            validation: crate::validation::ValidationReport { issues: vec![], passed: true },
            row_count: 10,
            column_count: 1,
            entities: None,
            graph: None,
            embeddings: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        // None fields should not appear in JSON.
        assert!(!json.contains("entities"));
        assert!(!json.contains("graph"));
        assert!(!json.contains("embeddings"));
    }
}
