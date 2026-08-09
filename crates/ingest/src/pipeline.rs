use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use polars::prelude::*;
use prism_provenance::{EvidenceClass, LocalProvenance, ProvenanceStore};
use serde::{Deserialize, Serialize};
use tracing;

use crate::local_facts::to_local_facts;
use crate::ontology::LlmOntologyConstructor;
use crate::schema::SchemaDetector;
use crate::validation::{self, Severity, ValidationReport};
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
    /// Id of the ontology this run extracts and validates with, resolved
    /// through the process-wide registry (`crate::ontologies`). `None` ⇒
    /// the built-in default (EMMO). An id nothing registered fails the run
    /// loudly — never a silent EMMO fallback.
    pub ontology: Option<String>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            llm: Some(LlmConfig::default()),
            max_sample_rows: 10,
            mapping: None,
            provenance_db: None,
            ontology: None,
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
                ontology: None,
            },
        }
    }

    /// Create a pipeline with full end-to-end configuration.
    pub fn with_config(config: PipelineConfig) -> Self {
        Self { config }
    }

    /// Ingest a file through the full pipeline. Which connector parses it is
    /// the connector registry's decision — no extension match lives here.
    pub async fn ingest_file(&self, path: &Path) -> Result<IngestResult> {
        // Resolve the connector inside a block: the registry read guard is
        // not `Send` and must be released before any `.await`.
        let connector = {
            let registry = crate::connectors::registry();
            match registry.connector_for(path) {
                Some(connector) => connector,
                None => {
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    bail!(
                        "Unsupported file format: '.{ext}'. Supported: {}",
                        registry.extensions().join(", ")
                    );
                }
            }
        };
        let df = connector.load(path)?;
        let source = connector.to_data_source(path)?;
        self.run_pipeline(df, source).await
    }

    /// Core pipeline: schema detection → validation → LLM extraction → graph → embeddings.
    async fn run_pipeline(&self, df: DataFrame, source: DataSource) -> Result<IngestResult> {
        let row_count = df.height();
        let column_count = df.width();

        // Resolve the ACTIVE ontology once, up front: the extraction prompt
        // and graph validation both read this ONE adapter, so what the model
        // is instructed to emit and what the validator accepts cannot
        // disagree. An unregistered configured id fails the whole run here —
        // ingesting under a silently substituted vocabulary would be worse.
        let ontology = crate::ontologies::active(self.config.ontology.as_deref())?;

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
        // Error-severity validation (empty frame, duplicate columns) is a
        // refusal to extract, not a warning to scroll past — see
        // `extraction_refusal`. Checked only when extraction is configured:
        // a schema-only run extracts nothing and already reports the full
        // `validation` field.
        let refusal = self
            .config
            .llm
            .as_ref()
            .and_then(|_| extraction_refusal(&validation));
        if let Some(msg) = &refusal {
            tracing::error!("{msg}");
            errors.push(msg.clone());
        }
        let entities = if refusal.is_none()
            && let Some(ref llm_config) = self.config.llm
        {
            let constructor = LlmOntologyConstructor::new(llm_config.clone());
            let sample_rows = extract_sample_rows(&df, self.config.max_sample_rows);

            tracing::info!(
                model = %llm_config.model,
                sample_rows = sample_rows.len(),
                "sending to LLM for entity extraction"
            );

            match constructor
                .extract_entities_with_mapping(
                    ontology.as_ref(),
                    &schema,
                    &sample_rows,
                    self.config.mapping.as_ref(),
                )
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
            let (report, blocking_error) =
                validate_before_graph_write(ontology.as_ref(), entity_set);
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
        // Facts land under the ontology's storage tenant: the default
        // ontology keeps the bare "local" tenant every existing store was
        // written with; any other ontology gets a composed tenant, which is
        // what keeps two vocabularies in one store from blending (the same
        // tenant-qualified isolation that separates local and peer knowledge).
        let tenant =
            crate::ontologies::storage_tenant(prism_provenance::LOCAL_TENANT, ontology.id());
        let graph = if graph_validation_passed && let Some(entity_set) = &entities {
            match self.write_local_graph(entity_set, &source, &tenant).await {
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

    /// Write the extracted entities/relationships as typed facts (with one
    /// PROV-O activity for the run) into the bundled Turso provenance store,
    /// under the active ontology's storage tenant.
    async fn write_local_graph(
        &self,
        entity_set: &EntitySet,
        source: &DataSource,
        tenant: &str,
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
            // "local" for the default ontology; "local@{id}" otherwise —
            // see `ontologies::storage_tenant`.
            tenant: tenant.to_string(),
            started_at: now.clone(),
            ended_at: now,
            locality: "local".into(),
            // Local ingest reads the source itself — the locator IS the origin.
            origin_source_id: None,
        };
        store.record_activity(&prov).await?;

        let facts = to_local_facts(entity_set);
        for fact in &facts {
            store
                .write_fact_with_evidence(fact, &prov, EvidenceClass::Research)
                .await?;
        }
        // Best-effort: vectorize the freshly written entity names into the
        // same Turso store so `prism query --semantic` works without Qdrant.
        // Failures are logged inside and never fail the ingest.
        store.embed_entities_best_effort(&facts, &prov.tenant).await;

        // Count what the store actually received, not what the LLM proposed.
        //
        // `to_local_facts` maps RELATIONSHIPS, so an extracted entity that is
        // not an endpoint of any relationship produces no fact and never
        // reaches the store. Reporting `entity_set.entities.len()` therefore
        // claimed nodes that were silently dropped: 50 entities with 3
        // relationships reported "50 nodes created" while the store saw at
        // most 6 names.
        let written: std::collections::HashSet<&str> = facts
            .iter()
            .flat_map(|f| [f.subject.as_str(), f.object.as_str()])
            .collect();

        let dropped = entity_set
            .entities
            .iter()
            .filter(|e| !written.contains(e.name.as_str()))
            .count();
        if dropped > 0 {
            // The drop is by design; being quiet about it was not.
            tracing::warn!(
                dropped,
                extracted = entity_set.entities.len(),
                stored = written.len(),
                "entities appearing in no relationship were not written to the graph"
            );
        }

        Ok(GraphUpdate {
            nodes_created: written.len(),
            edges_created: facts.len(),
        })
    }
}

impl Default for IngestPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Run graph-quality validation on extracted entities — against the ACTIVE
/// ontology's vocabulary — and decide whether the graph write should
/// proceed. Returns the full report plus, when Error-severity issues are
/// present, a message describing why the write was blocked (`None` means
/// the write may proceed).
fn validate_before_graph_write(
    ontology: &dyn crate::ontologies::Ontology,
    entity_set: &EntitySet,
) -> (
    crate::graph_validation::GraphValidationReport,
    Option<String>,
) {
    let report = crate::graph_validation::validate_graph(ontology, entity_set);
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

/// Decide whether entity extraction may run on this input at all. Returns
/// the refusal message for `IngestResult.errors` (→ FAILED STEPS → non-zero
/// exit), or `None` when extraction may proceed.
///
/// Error-severity input validation is a refusal to extract, not a warning to
/// log past: an empty DataFrame reaches the LLM with ZERO sample rows and the
/// model invents entities from the header names alone — self-consistent
/// enough to pass graph validation and land in the store at the default
/// confidence, under a clean "Done." exit 0. Duplicate column names poison
/// the same prompt differently: sampled values can no longer be attributed
/// to a column. "Nothing to extract from" is NOT "nothing was found" — an
/// extraction over real rows that honestly returns zero entities never
/// passes through here.
fn extraction_refusal(validation: &ValidationReport) -> Option<String> {
    let error_issues: Vec<&str> = validation
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.message.as_str())
        .collect();
    if error_issues.is_empty() {
        return None;
    }
    Some(format!(
        "refusing entity extraction: input validation found {} error-severity issue(s): {}. \
         Extracting from this input would produce fabricated entities from column names \
         alone — nothing was sent to the LLM and nothing was written to the graph.",
        error_issues.len(),
        error_issues.join("; ")
    ))
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
        let (report, blocking_error) =
            validate_before_graph_write(&crate::ontologies::EmmoOntology, &entity_set);
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
        let (report, blocking_error) =
            validate_before_graph_write(&crate::ontologies::EmmoOntology, &entity_set);
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
            ontology: None,
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
            .write_local_graph(&entity_set, &source, "local")
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
        let facts = store
            .recall_with_context("Steel", "local", 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.source == "/tmp/alloys.csv"));
        assert!(facts.iter().all(|f| f.agent == "prism-ingest"));
        assert!(
            facts
                .iter()
                .all(|f| f.evidence_class == EvidenceClass::Research),
            "LLM-extracted tabular facts must be ORANGE/research, never promoted by confidence",
        );

        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    /// `nodes_created` must count what the store received, not what the LLM
    /// proposed. `to_local_facts` maps relationships only, so an entity in no
    /// relationship is silently dropped — and the old count reported it as
    /// created anyway.
    #[tokio::test]
    async fn write_local_graph_does_not_count_entities_it_never_wrote() {
        use crate::{Entity, Relationship};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let db_path =
            std::env::temp_dir().join(format!("prism_pipeline_test_{}.db", uuid::Uuid::new_v4()));
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            max_sample_rows: 10,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
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
                // Referenced by nothing — never reaches the store.
                Entity {
                    entity_type: "Element".into(),
                    name: "Nickel".into(),
                    properties: serde_json::json!({}),
                },
            ],
            relationships: vec![Relationship {
                from: "Steel".into(),
                rel_type: "CONTAINS".into(),
                to: "Fe".into(),
                weight: Some(0.98),
                order: None,
            }],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let update = pipeline
            .write_local_graph(&entity_set, &source, "local")
            .await
            .unwrap();

        assert_eq!(
            update.nodes_created, 2,
            "counted an entity that was never written (3 extracted, only Steel and Fe stored)",
        );
        assert_eq!(update.edges_created, 1);

        // And prove the claim: the dropped entity really is absent.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Nickel", "local", 10).await.unwrap();
        assert!(
            !hits.iter().any(|n| n.name == "Nickel"),
            "Nickel was reported as created and is in the store after all",
        );

        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    /// The deliverable of the connector work, proven at PRODUCTION dispatch:
    /// a connector for a NOVEL extension, registered at runtime through
    /// `register_connector`, is reachable through `ingest_file` itself — no
    /// match arm, no enum variant, no `builtin()` edit, no consumer-list
    /// edit. This test dies if the pipeline stops consulting the
    /// process-wide registry.
    #[tokio::test]
    async fn registering_a_new_connector_needs_no_dispatch_edits() {
        use crate::connectors::{Connector, register_connector};
        use std::sync::Arc;

        // Mutates the process-wide registry: serialise with every other
        // global-registry test in this binary (one shared lock, one home).
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        struct Demo;
        impl Connector for Demo {
            fn id(&self) -> &'static str {
                "demo"
            }
            fn extensions(&self) -> &'static [&'static str] {
                &["demo"]
            }
            fn load(&self, _: &Path) -> Result<DataFrame> {
                Ok(df!("answer" => &[42i64]).expect("literal frame"))
            }
            fn to_data_source(&self, path: &Path) -> Result<DataSource> {
                Ok(DataSource {
                    path: path.display().to_string(),
                    format: "demo".into(),
                })
            }
        }

        register_connector(Arc::new(Demo)).expect("a novel connector must register");

        let path = std::env::temp_dir().join(format!("prism_demo_{}.demo", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"content irrelevant: Demo::load ignores it").unwrap();

        // Schema-only pipeline (no LLM, no store write) through the real
        // entry point.
        let result = IngestPipeline::new()
            .ingest_file(&path)
            .await
            .expect("the novel extension must dispatch through the registry");
        assert_eq!(result.source.format, "demo");
        assert_eq!((result.row_count, result.column_count), (1, 1));
        assert_eq!(result.schema.columns, vec!["answer"]);

        let _ = std::fs::remove_file(&path);
    }

    /// THE named requirement, at PRODUCTION dispatch: REPLACE the built-in
    /// csv connector in the process-wide registry, and `ingest_file` — the
    /// real entry point — must parse `.csv` with the replacement. Dies if
    /// the pipeline hardcodes csv anywhere or if `replace_connector` stops
    /// swapping the registry the pipeline reads.
    ///
    /// Registry state is process-global, so containment is enforced, not
    /// assumed: the replacement window is serialised behind the shared
    /// `GLOBAL_REGISTRY_TEST_LOCK` (sibling tests read the registry
    /// concurrently otherwise), the replacement claims exactly the
    /// built-in's extensions, and the built-in is restored by a drop guard
    /// that runs win, lose, or PANIC — an unwind out of `ingest_file` must
    /// not leave the fake csv connector installed for the rest of the
    /// process.
    #[tokio::test]
    async fn replacing_the_builtin_csv_connector_serves_ingest_file() {
        use crate::connectors::{Connector, CsvConnector, replace_connector};
        use std::sync::Arc;

        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        /// Restores the built-in csv connector on drop — including on
        /// panic/unwind, so a failure inside the pipeline can never poison
        /// the process-wide registry for sibling tests.
        struct RestoreCsv;
        impl Drop for RestoreCsv {
            fn drop(&mut self) {
                crate::connectors::replace_connector(Arc::new(CsvConnector))
                    .expect("restoring the built-in csv connector");
            }
        }

        struct MyCsvEngine;
        impl Connector for MyCsvEngine {
            fn id(&self) -> &'static str {
                "csv"
            }
            fn extensions(&self) -> &'static [&'static str] {
                &["csv", "tsv"]
            }
            fn load(&self, _: &Path) -> Result<DataFrame> {
                Ok(df!("my_own_column" => &[7i64]).expect("literal frame"))
            }
            fn to_data_source(&self, path: &Path) -> Result<DataSource> {
                Ok(DataSource {
                    path: path.display().to_string(),
                    format: "csv".into(),
                })
            }
        }

        let displaced =
            replace_connector(Arc::new(MyCsvEngine)).expect("the built-in csv must be replaceable");
        // From here on the guard owns the restore: it runs on success,
        // failed assertion, and panic alike.
        let _restore = RestoreCsv;
        assert_eq!(displaced.id(), "csv");

        let path = std::env::temp_dir().join(format!("prism_replace_{}.csv", uuid::Uuid::new_v4()));
        // Real csv content: a genuine csv parser would read one column "a".
        // Only the replacement produces "my_own_column".
        std::fs::write(&path, b"a\n1\n").unwrap();

        let result = IngestPipeline::new().ingest_file(&path).await;

        let result = result.expect("csv must still ingest through the replacement");
        assert_eq!(
            result.schema.columns,
            vec!["my_own_column"],
            "ingest_file must dispatch to the REPLACEMENT csv connector",
        );
        assert_eq!((result.row_count, result.column_count), (1, 1));
        let _ = std::fs::remove_file(&path);
    }

    /// The refusal path reads the same registry: unclaimed extensions bail
    /// with the supported list. (Containment asserts only — the sibling
    /// test above registers an extra connector in this same process.)
    #[tokio::test]
    async fn ingest_file_refuses_unclaimed_extensions_with_supported_list() {
        // Reads the process-wide registry (the supported list): serialise
        // with the sibling tests that mutate it, so the message is never
        // observed mid-replacement-window.
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        let err = IngestPipeline::new()
            .ingest_file(Path::new("/tmp/data.nope"))
            .await
            .expect_err("unclaimed extension must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("Unsupported file format: '.nope'"), "{msg}");
        assert!(msg.contains("csv") && msg.contains("parquet"), "{msg}");
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

    // ── Refusing extraction from empty / Error-severity input ─────────
    //
    // The defect: a CSV with headers and ZERO data rows sailed through —
    // the Error-severity ValidationReport only warned, extraction ran with
    // zero sample rows, and the model invented entities from the column
    // names, written at confidence 0.8 under "Done." exit 0.

    /// Per-test scratch dir + provenance path (same temp_dir+uuid convention
    /// as the sibling tests), removed on drop.
    struct RefusalScratch {
        dir: PathBuf,
    }

    impl RefusalScratch {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("prism_refusal_test_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self { dir }
        }

        fn csv(&self, body: &str) -> PathBuf {
            let p = self.dir.join("input.csv");
            std::fs::write(&p, body).expect("write csv fixture");
            p
        }

        fn db_path(&self) -> PathBuf {
            self.dir.join("provenance.db")
        }
    }

    impl Drop for RefusalScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A pipeline whose LLM points at the given mock server.
    fn pipeline_against(server_uri: String, db_path: PathBuf) -> IngestPipeline {
        IngestPipeline::with_config(PipelineConfig {
            llm: Some(crate::LlmConfig {
                base_url: server_uri,
                model: "test-model".into(),
                ..crate::LlmConfig::default()
            }),
            max_sample_rows: 10,
            mapping: None,
            provenance_db: Some(db_path),
            ontology: None,
        })
    }

    /// The refusal predicate: Error severity refuses, Warning/Info does not.
    #[test]
    fn extraction_refusal_fires_on_error_severity_only() {
        use crate::validation::ValidationIssue;

        // Empty frame — the real report from the real validator.
        let empty = validation::validate(&DataFrame::empty());
        let msg = extraction_refusal(&empty).expect("an empty frame must refuse extraction");
        assert!(msg.contains("refusing entity extraction"), "{msg}");
        assert!(msg.contains("DataFrame is empty"), "{msg}");

        // Duplicate columns (the other Error-severity issue). polars' safe
        // constructors and its CSV reader refuse/rename duplicate names, so
        // the report carries the exact issue `validation::validate` emits
        // for frames that arrive from other connectors.
        let dup = ValidationReport {
            issues: vec![ValidationIssue {
                severity: Severity::Error,
                column: Some("a".into()),
                message: "Duplicate column name: 'a'".into(),
            }],
            passed: false,
        };
        let msg = extraction_refusal(&dup).expect("duplicate columns must refuse extraction");
        assert!(msg.contains("Duplicate column name: 'a'"), "{msg}");

        // Warnings alone must NOT refuse: >50% nulls is Warning severity.
        let s = Series::new(
            "mostly_null".into(),
            &[Option::<i32>::None, None, Some(1), None],
        );
        let warn_only = validation::validate(&DataFrame::new(vec![s.into()]).unwrap());
        assert!(!warn_only.issues.is_empty(), "fixture must carry a warning");
        assert_eq!(extraction_refusal(&warn_only), None);
    }

    /// End-to-end: a header-only CSV is REFUSED — the reason lands in
    /// `errors` (→ FAILED STEPS → non-zero exit), no prompt reaches the
    /// model, and the graph store is untouched (never even created).
    #[tokio::test]
    async fn header_only_csv_refuses_extraction_and_leaves_graph_untouched() {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        // A live-looking LLM endpoint that must never be consulted:
        // `.expect(0)` fails the test if any prompt is sent.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,uts_mpa,phase\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        assert_eq!((result.row_count, result.column_count), (0, 3));
        assert!(!result.validation.passed);
        // The refusal is visible on the errors spine, with the reason.
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("refusing entity extraction"),
            "{}",
            result.errors[0]
        );
        assert!(
            result.errors[0].contains("DataFrame is empty"),
            "{}",
            result.errors[0]
        );
        // Nothing was extracted, validated, or written.
        assert!(result.entities.is_none());
        assert!(result.graph_validation.is_none());
        assert!(result.graph.is_none());
        // The graph is UNTOUCHED — not "zero facts written" but "the store
        // was never even opened": opening is what creates the file.
        assert!(
            !db_path.exists(),
            "a refused ingest opened/created the provenance store"
        );
        server.verify().await;
    }

    /// The guard must not over-fire: a normal CSV with data rows still
    /// extracts and still writes the graph. (This also pins the mock wire
    /// shape the refusal tests rely on being reachable.)
    #[tokio::test]
    async fn csv_with_rows_still_extracts_and_writes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let extraction = serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Ti-6Al-4V", "properties": {}},
                {"type": "Element", "name": "Al", "properties": {}}
            ],
            "relationships": [
                {"from": "Ti-6Al-4V", "rel": "CONTAINS", "to": "Al", "weight": 0.06}
            ]
        });
        let body = serde_json::json!({
            "choices": [{
                "message": {"content": extraction.to_string()},
                "finish_reason": "stop"
            }]
        });

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,al_frac\nTi-6Al-4V,0.06\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let entities = result.entities.expect("extraction must run on data rows");
        assert_eq!(entities.entities.len(), 2);
        let graph = result.graph.expect("graph write must run");
        assert_eq!((graph.nodes_created, graph.edges_created), (2, 1));
        // And the fact really landed.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Ti-6Al-4V", "local", 10).await.unwrap();
        assert!(hits.iter().any(|n| n.name == "Ti-6Al-4V"));
        server.verify().await;
    }

    /// "Nothing was FOUND" is not "nothing to extract FROM". Rows exist, the
    /// model runs and honestly returns zero entities — the refusal guard
    /// must NOT fire and the prompt must actually reach the model.
    ///
    /// NOTE: the run is still not clean today. The pre-existing
    /// `validate_before_graph_write` gate treats an empty extraction as
    /// Error severity ("No entities were extracted", graph_validation.rs
    /// check 1) and reports it on the same errors spine, so this case
    /// already exited non-zero BEFORE the refusal guard existed. That
    /// behaviour is out of scope here (graph validation is fenced off) and
    /// pinned as-is.
    #[tokio::test]
    async fn rows_with_zero_entities_found_is_not_a_refusal() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let body = serde_json::json!({
            "choices": [{
                "message": {"content": "{\"entities\": [], \"relationships\": []}"},
                "finish_reason": "stop"
            }]
        });

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,uts_mpa\nUnobtainium-X,9999\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // The refusal guard did not fire…
        assert!(
            !result
                .errors
                .iter()
                .any(|e| e.contains("refusing entity extraction")),
            "{:?}",
            result.errors
        );
        // …extraction ran (wiremock verifies the request) and honestly
        // returned zero entities…
        let entities = result.entities.expect("extraction must run on data rows");
        assert!(entities.entities.is_empty());
        // …and the only error is the pre-existing graph-validation gate on
        // the empty result, unchanged by the refusal work.
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("No entities were extracted"),
            "{}",
            result.errors[0]
        );
        assert!(result.graph.is_none());
        server.verify().await;
    }

    // ── Pluggable ontologies, at PRODUCTION dispatch ───────────────────
    //
    // These tests exercise `ingest_file` itself: the ontology is registered
    // in the PROCESS-WIDE registry at runtime and selected by id through
    // `PipelineConfig.ontology` — no local registry, no direct calls into
    // the adapter. They die if the pipeline stops consulting the registry,
    // stops building the prompt from the active ontology, stops validating
    // against it, or stops composing the storage tenant from its id.

    /// A minimal chemistry vocabulary, nothing like EMMO's.
    struct ChemOntology {
        id: &'static str,
    }

    impl crate::ontologies::Ontology for ChemOntology {
        fn id(&self) -> &'static str {
            self.id
        }
        fn entity_types(&self) -> &'static [&'static str] {
            &["Molecule"]
        }
        fn relationship_types(&self) -> &'static [&'static str] {
            &["REACTS_WITH"]
        }
        fn unit_vocabulary(&self) -> crate::ontologies::UnitVocabulary {
            crate::ontologies::UnitVocabulary {
                name: "FREE",
                prefix: None,
            }
        }
    }

    /// A mock LLM endpoint that returns `extraction` for every chat call.
    async fn mock_llm(extraction: serde_json::Value) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let body = serde_json::json!({
            "choices": [{
                "message": {"content": extraction.to_string()},
                "finish_reason": "stop"
            }]
        });
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    fn chem_extraction() -> serde_json::Value {
        serde_json::json!({
            "entities": [
                {"type": "Molecule", "name": "H2O", "properties": {}},
                {"type": "Molecule", "name": "O3", "properties": {}}
            ],
            "relationships": [
                {"from": "H2O", "rel": "REACTS_WITH", "to": "O3"}
            ]
        })
    }

    fn emmo_extraction() -> serde_json::Value {
        serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Steel", "properties": {}},
                {"type": "Element", "name": "Fe", "properties": {}}
            ],
            "relationships": [
                {"from": "Steel", "rel": "CONTAINS", "to": "Fe", "weight": 1.0}
            ]
        })
    }

    /// The core requirement, both directions, through the real pipeline: a
    /// SECOND ontology registered at runtime (1) instructs extraction from
    /// ITS vocabulary, (2) validates ITS facts as in-vocabulary and EMMO's
    /// as foreign, and (3) EMMO (the default) flags the second ontology's
    /// facts as foreign — prompt and validator both reading the ONE active
    /// adapter.
    #[tokio::test]
    async fn a_second_ontology_instructs_and_validates_from_its_own_vocabulary() {
        use std::sync::Arc;

        // Mutates/reads process-wide registries around other tests'
        // mutation windows: one shared lock for this binary.
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        crate::ontologies::register_ontology(Arc::new(ChemOntology {
            id: "chem-crossval",
        }))
        .expect("a novel ontology must register");

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("molecule,reacts_with\nH2O,O3\n");

        // ── Chem facts under the chem ontology: in-vocabulary. ──────────
        let server = mock_llm(chem_extraction()).await;
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            ontology: Some("chem-crossval".into()),
            ..pipeline_against(server.uri(), scratch.db_path()).config
        });
        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        // The PROMPT was built from the chem vocabulary, not EMMO's: the
        // request that actually reached the model names the chem types and
        // carries none of the EMMO instruction block.
        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 1);
        let sent = String::from_utf8_lossy(&requests[0].body).into_owned();
        assert!(
            sent.contains("Molecule"),
            "prompt lacks the chem vocabulary"
        );
        assert!(sent.contains("REACTS_WITH"), "prompt lacks the chem rels");
        assert!(
            !sent.contains("materials science data analyst"),
            "the EMMO preamble leaked into a chem extraction"
        );

        // Validation accepted the chem vocabulary…
        let report = result.graph_validation.expect("validation ran");
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.category == "unknown_type" || i.category == "unknown_rel"),
            "{:?}",
            report.issues
        );

        // ── The SAME chem facts under the DEFAULT ontology: foreign. ────
        let server = mock_llm(chem_extraction()).await;
        let pipeline = pipeline_against(server.uri(), scratch.db_path());
        let result = pipeline.ingest_file(&csv).await.unwrap();
        let report = result.graph_validation.expect("validation ran");
        assert!(
            report.issues.iter().any(|i| i.category == "unknown_type"
                && i.message.contains("Molecule")
                && i.message.contains("expected one of: Alloy")),
            "{:?}",
            report.issues
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.category == "unknown_rel" && i.message.contains("REACTS_WITH")),
            "{:?}",
            report.issues
        );

        // ── And EMMO-shaped facts under the chem ontology: foreign. ─────
        let server = mock_llm(emmo_extraction()).await;
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            ontology: Some("chem-crossval".into()),
            ..pipeline_against(server.uri(), scratch.db_path()).config
        });
        let result = pipeline.ingest_file(&csv).await.unwrap();
        let report = result.graph_validation.expect("validation ran");
        assert!(
            report.issues.iter().any(|i| i.category == "unknown_type"
                && i.message.contains("Alloy")
                && i.message.contains("expected one of: Molecule")),
            "{:?}",
            report.issues
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.category == "unknown_rel" && i.message.contains("CONTAINS")),
            "{:?}",
            report.issues
        );
    }

    /// Coexistence: EMMO and a second ontology ingested into ONE store land
    /// in disjoint, tenant-scoped subgraphs. Reads scoped to each tenant see
    /// only their own facts, and the default read scope (local + mesh) never
    /// picks up the second ontology's subgraph.
    #[tokio::test]
    async fn two_ontologies_coexist_in_one_store_without_blending() {
        use std::sync::Arc;

        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        crate::ontologies::register_ontology(Arc::new(ChemOntology { id: "chem-coexist" }))
            .expect("a novel ontology must register");

        let scratch = RefusalScratch::new();
        let db_path = scratch.db_path();
        let csv = scratch.csv("a,b\nx,y\n");

        // Run 1: default (EMMO) → bare "local" tenant, as always.
        let server = mock_llm(emmo_extraction()).await;
        let pipeline = pipeline_against(server.uri(), db_path.clone());
        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.graph.is_some());

        // Run 2: chem → composed "local@chem-coexist" tenant, SAME store.
        let server = mock_llm(chem_extraction()).await;
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            ontology: Some("chem-coexist".into()),
            ..pipeline_against(server.uri(), db_path.clone()).config
        });
        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(result.graph.is_some());

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();

        // EMMO's subgraph is visible under "local" and ONLY there.
        let hits = store.graph_search("Steel", "local", 10).await.unwrap();
        assert!(hits.iter().any(|n| n.name == "Steel"));
        let hits = store
            .graph_search("Steel", "local@chem-coexist", 10)
            .await
            .unwrap();
        assert!(hits.is_empty(), "EMMO facts leaked into the chem tenant");

        // Chem's subgraph is visible under its composed tenant and ONLY there.
        let hits = store
            .graph_search("H2O", "local@chem-coexist", 10)
            .await
            .unwrap();
        assert!(hits.iter().any(|n| n.name == "H2O"));
        let hits = store.graph_search("H2O", "local", 10).await.unwrap();
        assert!(
            hits.is_empty(),
            "chem facts blended into the default EMMO tenant"
        );

        // Assertions are tenant-scoped the same way.
        let facts = store
            .recall_with_context("H2O", "local@chem-coexist", 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].tenant, "local@chem-coexist");
        assert!(
            store
                .recall_with_context("H2O", "local", 10)
                .await
                .unwrap()
                .is_empty()
        );

        // The DEFAULT read scope (local + discovered mesh tenants) does not
        // silently absorb the second ontology's subgraph.
        assert_eq!(store.default_read_tenants().await.unwrap(), ["local"]);
    }
}
