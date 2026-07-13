use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use polars::prelude::*;
use prism_provenance::{LocalProvenance, ProvenanceStore};
use serde::{Deserialize, Serialize};
use tracing;

use crate::connectors::{CsvConnector, ParquetConnector};
use crate::local_facts::to_local_facts;
use crate::ontology::LlmOntologyConstructor;
use crate::schema::SchemaDetector;
use crate::validation::{self, ValidationReport};
use crate::{DataSource, EmbeddingBatch, EntitySet, GraphUpdate, LlmConfig, SchemaAnalysis};

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
    /// SHACL-lite structural check on the extracted entities/relationships
    /// (orphan rels, unknown types, weight-sum sanity, etc.), run before the
    /// graph write. `None` only when no entities were extracted at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_validation: Option<crate::graph_validation::GraphValidationReport>,
    /// Populated when the local EMMO graph write (bundled Turso store) runs.
    /// Counts are upsert attempts, not net-new rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphUpdate>,
    /// Always `None` since the Qdrant upsert step was removed (entity
    /// vectors live in the bundled Turso store); kept so the serialized
    /// result shape stays stable for older consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<EmbeddingBatch>,
    /// Step failures. NON-EMPTY means configured pipeline steps did NOT
    /// complete — callers must surface these and exit non-zero. The old
    /// behavior (audit critical: log-and-None) made `prism ingest` print
    /// "Done." with exit 0 while storing nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Configuration for a full ingest pipeline run.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// LLM config for entity extraction. If None, extraction is skipped.
    pub llm: Option<LlmConfig>,
    /// Maximum sample rows to send to the LLM.
    pub max_sample_rows: usize,
    /// Custom ontology mapping rules (loaded from YAML).
    pub mapping: Option<crate::mapping::OntologyMapping>,
    /// Path of the bundled Turso provenance store the extracted facts are
    /// written to. If None, defaults to `~/.prism/provenance.db`.
    pub provenance_db: Option<PathBuf>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            llm: Some(LlmConfig::default()),
            max_sample_rows: 10,
            mapping: None,
            provenance_db: None,
        }
    }
}

/// Orchestrates the data ingestion pipeline:
///
/// ```text
/// Load → Schema Detection → Validation → LLM Extraction → Local EMMO Graph (Turso)
/// ```
///
/// The LLM extraction step is optional — controlled by `PipelineConfig`;
/// the graph write (with entity vectors) runs whenever entities were
/// extracted (the store is bundled). Without any configs, behaves like the
/// original schema-only pipeline.
pub struct IngestPipeline {
    config: PipelineConfig,
}

impl IngestPipeline {
    pub fn new() -> Self {
        Self {
            config: PipelineConfig {
                llm: None,
                max_sample_rows: 10,
                mapping: None,
                provenance_db: None,
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
            _ => bail!("Unsupported file format: '.{ext}'. Supported: csv, tsv, parquet"),
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
        let mut errors: Vec<String> = Vec::new();
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
                    tracing::error!("LLM extraction failed: {e:#}");
                    errors.push(format!("LLM extraction failed: {e:#}"));
                    None
                }
            }
        } else {
            None
        };

        // Step 3.5: Graph quality validation (SHACL-lite) — this used to be a
        // documented "runs before writing to Neo4j" gate with zero callers
        // (AUDIT_BACKLOG 20 / INGESTION_AUDIT #20), so LLM extraction output
        // went straight to the graph unchecked. Run it whenever entities
        // exist, and refuse the graph write on Error-severity issues (orphan
        // relationships, empty names) rather than upserting garbage.
        let graph_validation = entities.as_ref().map(|entity_set| {
            let (report, blocking_error) = validate_before_graph_write(entity_set);
            if let Some(msg) = blocking_error {
                tracing::error!(issues = report.issues.len(), "{msg}");
                errors.push(msg);
            } else if !report.issues.is_empty() {
                tracing::warn!(
                    issues = report.issues.len(),
                    "graph validation found non-blocking issues"
                );
            }
            report
        });
        let graph_validation_passed = graph_validation.as_ref().is_none_or(|r| r.passed);

        // Step 4: local EMMO graph write into the bundled Turso store (if
        // entities exist and validation passed). This replaced the Neo4j
        // upsert (Neo4j retirement, step 1) — the store is bundled, so no
        // backend config gates the write.
        let graph = if graph_validation_passed && let Some(entity_set) = &entities {
            match self.write_local_graph(entity_set, &source).await {
                Ok(update) => {
                    tracing::info!(
                        nodes = update.nodes_created,
                        edges = update.edges_created,
                        "local graph write complete"
                    );
                    Some(update)
                }
                Err(e) => {
                    tracing::error!("local graph write failed: {e:#}");
                    errors.push(format!("local graph write failed: {e:#}"));
                    None
                }
            }
        } else {
            None
        };

        // Entity vectors are written to the bundled Turso store by
        // `write_local_graph` (embed_entities_best_effort); the old Qdrant
        // upsert step was redundant and has been removed.
        Ok(IngestResult {
            source,
            schema,
            validation,
            row_count,
            column_count,
            entities,
            graph_validation,
            graph,
            embeddings: None,
            errors,
        })
    }

    /// Write the extracted entities/relationships as EMMO facts (with one
    /// PROV-O activity for the run) into the bundled Turso provenance store.
    async fn write_local_graph(
        &self,
        entity_set: &EntitySet,
        source: &DataSource,
    ) -> Result<GraphUpdate> {
        let db_path = match &self.config.provenance_db {
            Some(p) => p.clone(),
            None => dirs::home_dir()
                .map(|h| h.join(".prism/provenance.db"))
                .unwrap_or_else(|| PathBuf::from("provenance.db")),
        };
        let store = ProvenanceStore::open(&db_path).await?;

        let now = chrono::Utc::now().to_rfc3339();
        let prov = LocalProvenance {
            activity_id: uuid::Uuid::new_v4().to_string(),
            agent_id: self
                .config
                .llm
                .as_ref()
                .map(|l| l.model.clone())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "prism-ingest".into()),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: source.path.clone(),
            source_kind: "Document".into(),
            // Local single-user store — no per-pipeline tenancy (yet).
            tenant: "local".into(),
            started_at: now.clone(),
            ended_at: now,
            locality: "local".into(),
        };
        store.record_activity(&prov).await?;

        let facts = to_local_facts(entity_set);
        for fact in &facts {
            store.write_fact(fact, &prov).await?;
        }
        // Best-effort: vectorize the freshly written entity names into the
        // same Turso store so `prism query --semantic` works without Qdrant.
        // Failures are logged inside and never fail the ingest.
        store.embed_entities_best_effort(&facts, &prov.tenant).await;
        Ok(GraphUpdate {
            nodes_created: entity_set.entities.len(),
            edges_created: facts.len(),
        })
    }
}

impl Default for IngestPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Run graph-quality validation on extracted entities and decide whether the
/// graph write should proceed. Returns the full report plus, when
/// Error-severity issues are present, a message describing why the write
/// was blocked (`None` means the write may proceed).
fn validate_before_graph_write(
    entity_set: &EntitySet,
) -> (
    crate::graph_validation::GraphValidationReport,
    Option<String>,
) {
    let report = crate::graph_validation::validate_graph(entity_set);
    if report.passed {
        return (report, None);
    }
    let error_issues: Vec<&str> = report
        .issues
        .iter()
        .filter(|i| i.severity == crate::graph_validation::GraphSeverity::Error)
        .map(|i| i.message.as_str())
        .collect();
    let msg = format!(
        "graph validation failed ({} error-severity issue(s)): {}",
        error_issues.len(),
        error_issues.join("; ")
    );
    (report, Some(msg))
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
    fn validate_before_graph_write_blocks_on_orphan_relationship() {
        use crate::{Entity, Relationship};
        // "Fe" is referenced by the relationship but never extracted as an
        // entity — this used to reach Neo4j unchecked (AUDIT_BACKLOG 20).
        let entity_set = EntitySet {
            entities: vec![Entity {
                entity_type: "Alloy".into(),
                name: "Steel".into(),
                properties: serde_json::json!({}),
            }],
            relationships: vec![Relationship {
                from: "Steel".into(),
                rel_type: "CONTAINS".into(),
                to: "Fe".into(),
                weight: None,
                order: None,
            }],
        };
        let (report, blocking_error) = validate_before_graph_write(&entity_set);
        assert!(!report.passed);
        let msg = blocking_error.expect("orphan relationship must block the graph write");
        assert!(msg.contains("graph validation failed"));
        assert!(msg.contains("Fe"));
    }

    #[test]
    fn validate_before_graph_write_allows_clean_entities() {
        use crate::Entity;
        let entity_set = EntitySet {
            entities: vec![Entity {
                entity_type: "Alloy".into(),
                name: "Steel".into(),
                properties: serde_json::json!({}),
            }],
            relationships: vec![],
        };
        let (report, blocking_error) = validate_before_graph_write(&entity_set);
        assert!(report.passed);
        assert!(blocking_error.is_none());
    }

    #[test]
    fn pipeline_config_default_has_llm_and_bundled_store() {
        let cfg = PipelineConfig::default();
        assert!(cfg.llm.is_some());
        // None ⇒ the bundled ~/.prism/provenance.db is used at write time.
        assert!(cfg.provenance_db.is_none());
    }

    #[tokio::test]
    async fn write_local_graph_lands_facts_in_turso_store() {
        use crate::{Entity, Relationship};

        // Keep the best-effort entity-embedding step inert: a unit test
        // must never init (or first-run download) the native ONNX model.
        // Process-global, but no other prism-ingest test reads this env.
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let db_path =
            std::env::temp_dir().join(format!("prism_pipeline_test_{}.db", uuid::Uuid::new_v4()));
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            max_sample_rows: 10,
            mapping: None,
            provenance_db: Some(db_path.clone()),
        });

        let entity_set = EntitySet {
            entities: vec![
                Entity {
                    entity_type: "Alloy".into(),
                    name: "Steel".into(),
                    properties: serde_json::json!({}),
                },
                Entity {
                    entity_type: "Element".into(),
                    name: "Fe".into(),
                    properties: serde_json::json!({}),
                },
                Entity {
                    entity_type: "Property".into(),
                    name: "density".into(),
                    properties: serde_json::json!({"value": 7.8, "unit": "g/cm3"}),
                },
            ],
            relationships: vec![
                Relationship {
                    from: "Steel".into(),
                    rel_type: "CONTAINS".into(),
                    to: "Fe".into(),
                    weight: Some(0.98),
                    order: None,
                },
                Relationship {
                    from: "Steel".into(),
                    rel_type: "HAS_PROPERTY".into(),
                    to: "density".into(),
                    weight: None,
                    order: None,
                },
            ],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let update = pipeline
            .write_local_graph(&entity_set, &source)
            .await
            .unwrap();
        assert_eq!(update.nodes_created, 3);
        assert_eq!(update.edges_created, 2);

        // Reopen the store and verify the facts actually landed, in the
        // shapes the read API serves.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Steel", "local", 10).await.unwrap();
        assert!(
            hits.iter()
                .any(|n| n.name == "Steel" && n.label == "Matter")
        );
        let tr = store
            .get_neighbors("Steel", None, "local", 10)
            .await
            .unwrap();
        assert!(tr.edges.iter().any(|e| e.rel_type == "CONTAINS_ELEMENT"));
        assert!(tr.edges.iter().any(|e| e.rel_type == "HAS_MEASUREMENT"));
        let facts = store.recall("Steel", "local", 10).await.unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.source == "/tmp/alloys.csv"));
        assert!(facts.iter().all(|f| f.agent == "prism-ingest"));

        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn ingest_result_with_none_fields_serializes_cleanly() {
        let result = IngestResult {
            source: DataSource {
                path: "/tmp/test.csv".into(),
                format: "csv".into(),
            },
            schema: SchemaAnalysis {
                columns: vec!["a".into()],
                detected_types: vec!["int".into()],
            },
            validation: crate::validation::ValidationReport {
                issues: vec![],
                passed: true,
            },
            row_count: 10,
            column_count: 1,
            entities: None,
            graph_validation: None,
            graph: None,
            embeddings: None,
            errors: Vec::new(),
        };
        let json = serde_json::to_string(&result).unwrap();
        // None fields should not appear in JSON.
        assert!(!json.contains("entities"));
        assert!(!json.contains("graph_validation"));
        assert!(!json.contains("graph"));
        assert!(!json.contains("embeddings"));
        // No errors ⇒ no errors key either (clean success stays clean)…
        assert!(!json.contains("errors"));
        // …but step failures MUST be visible in the JSON (the old shape hid
        // failed steps entirely — audit critical #2).
        let failed = IngestResult {
            errors: vec!["local graph write failed: disk full".into()],
            ..result
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains("errors"));
        assert!(json.contains("disk full"));
    }
}
