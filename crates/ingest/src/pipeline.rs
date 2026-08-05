use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use polars::prelude::*;
use prism_provenance::{EvidenceClass, LocalFact, LocalProvenance, ProvenanceStore};
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
    /// Facts refused by the tabular containment gate — their numeric value
    /// is not in any cell of the source the extractor saw. NON-EMPTY means
    /// facts were dropped before the store; callers must surface them. A
    /// silent drop is nearly as bad as a silent fabrication.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts_dropped: Vec<DroppedTabularFact>,
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
        // Facts refused by the tabular containment gate are collected here so
        // the run surfaces them (see `gate_tabular_facts`).
        let mut facts_dropped: Vec<DroppedTabularFact> = Vec::new();
        // The same bounded window the extractor sees — the gate checks
        // containment against exactly these cells (a value from beyond this
        // window cannot have been read by this run).
        let sample_rows = extract_sample_rows(&df, self.config.max_sample_rows);
        let entities = if let Some(ref llm_config) = self.config.llm {
            let constructor = LlmOntologyConstructor::new(llm_config.clone());

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
            match self
                .write_local_graph(entity_set, &source, &sample_rows, &schema.columns)
                .await
            {
                Ok((update, dropped)) => {
                    // Refused facts are surfaced, never silent.
                    facts_dropped.extend(dropped);
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
            facts_dropped,
        })
    }

    /// Write the extracted entities/relationships as EMMO facts (with one
    /// PROV-O activity for the run) into the bundled Turso provenance store.
    async fn write_local_graph(
        &self,
        entity_set: &EntitySet,
        source: &DataSource,
        sample_rows: &[Vec<String>],
        columns: &[String],
    ) -> Result<(GraphUpdate, Vec<DroppedTabularFact>)> {
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

        // Containment gate: a tabular fact is honest only if its number is
        // actually in a cell of the source the extractor saw. Facts whose
        // value is not in any cell are dropped here (never written, never
        // corrected) and returned so the run surfaces the refusals.
        let outcome = gate_tabular_facts(
            to_local_facts(entity_set),
            sample_rows,
            columns,
            &source.path,
        );
        for fact in &outcome.kept {
            store
                .write_fact_with_evidence(fact, &prov, EvidenceClass::Research)
                .await?;
        }
        // Best-effort: vectorize the freshly written entity names into the
        // same Turso store so `prism query --semantic` works without Qdrant.
        // Failures are logged inside and never fail the ingest.
        store
            .embed_entities_best_effort(&outcome.kept, &prov.tenant)
            .await;
        Ok((
            GraphUpdate {
                nodes_created: entity_set.entities.len(),
                edges_created: outcome.kept.len(),
            },
            outcome.dropped,
        ))
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

// ─── Tabular containment gate ────────────────────────────────────────────
// The CSV/Parquet path has no prose, so it has no verbatim-quote concept.
// The honest analogue of a quote is a CELL REFERENCE — which file, which
// row, which column a number was read from, such that a reader can open the
// source and look at it. The gate verifies a fact's value against the cells
// the extractor actually saw: a fact whose number is not in any cell is
// REFUSED (dropped, never corrected) — the tabular equivalent of a quote
// that is not in the document. Refusals are reported, never silent.
//
// This mirrors the papers and local-text routes, which gate on a verbatim
// quote through the one shared `retrieval::claims::quote_in_block`. Tabular
// data has no sentence to quote, so the gate verifies the NUMBER instead.

/// The exact `(file, row, column)` a number was read from — the tabular
/// analogue of a verbatim quote. Resolved by the containment gate when it
/// verifies a fact's value against the source cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellReference {
    pub file: String,
    /// 0-based row index within the sample rows the extractor saw.
    pub row: usize,
    pub column: String,
}

/// Why a tabular fact was refused before the provenance store. A refused
/// fact is dropped — never downgraded, never silently repaired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DroppedTabularFact {
    pub subject: String,
    pub object: String,
    /// The numeric value that could not be matched to a source cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    pub reason: TabularDropReason,
}

/// The cause of a tabular refusal. An enum (not a free string) so every
/// refusal names a machine-stable cause; `as_label` is the wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabularDropReason {
    /// The fact's numeric value does not occur in any cell of the source the
    /// extractor saw. A number not in the file cannot be attributed to the
    /// file — the tabular equivalent of a quote not in the document.
    ValueNotInSource,
}

impl TabularDropReason {
    /// Stable machine-readable label used in drop reports.
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            TabularDropReason::ValueNotInSource => "value_not_in_source",
        }
    }
}

/// One gate run's outcome: the facts that passed (kept, each traceable to a
/// cell) and the facts refused (which callers MUST surface — a silent drop
/// is nearly as bad as a silent fabrication).
#[derive(Debug, Default)]
pub struct TabularFactOutcome {
    pub kept: Vec<LocalFact>,
    pub dropped: Vec<DroppedTabularFact>,
}

/// Resolve `value` to its source cell among the rows the extractor saw,
/// returning the first `(file, row, column)` whose stringified value
/// matches. `None` when no cell contains the value — the fact's number is
/// not in the source.
/// Loose token match used to tie a fact to a column header or a row's subject
/// cell — case- and separator-insensitive, so `HAS_PROPERTY`/`has property`
/// and `density`/`Density (g/cm3)` line up.
fn tokens_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
}

/// Locate the cell that supports **this** fact.
///
/// Presence is not attribution. The first version took only the value and
/// scanned every cell, so `fact(Steel, density, 7.8)` resolved against a
/// `temperature` column that happened to hold 7.8 — and then logged that wrong
/// column as the fact's provenance. Two independent reviewers reproduced it.
///
/// The cell must now sit in a column matching the fact's `object` AND in a row
/// naming the fact's `subject`. A fact whose property has no column, or whose
/// subject appears in no row, is refused: without both we cannot say the number
/// belongs to it, and an unattributable number is exactly what this gate exists
/// to stop.
fn find_cell(
    value: f64,
    subject: &str,
    object: &str,
    sample_rows: &[Vec<String>],
    columns: &[String],
    file: &str,
) -> Option<CellReference> {
    for (row_idx, row) in sample_rows.iter().enumerate() {
        // The row must be about this fact's subject.
        if !row.iter().any(|cell| tokens_match(cell, subject)) {
            continue;
        }
        for (col_idx, cell) in row.iter().enumerate() {
            // The column must be this fact's property.
            let header_matches = columns
                .get(col_idx)
                .is_some_and(|header| tokens_match(header, object));
            if header_matches && cell_matches_value(cell, value) {
                let column = columns
                    .get(col_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("col{col_idx}"));
                return Some(CellReference {
                    file: file.to_string(),
                    row: row_idx,
                    column,
                });
            }
        }
    }
    None
}

/// Does a stringified cell equal `value`? Parses the cell as a number and
/// compares with tolerance, so `"7.8"` matches `7.8` and `"2"` matches
/// `2.0`. Non-numeric cells (`"Steel"`, `""`, headers) never match.
/// The leading numeric token of a cell, tolerating the shapes real tables use.
///
/// A strict `parse::<f64>()` refused every legitimate cell that carries its
/// unit or a separator — `"7.8 g/cm3"`, `"1,140"`, `"50%"`, `"1_140"` — which
/// made the gate lossy against ordinary CSV exports.
///
/// Deliberately NOT handled: a comma as a decimal separator (`"7,8"`). It is
/// indistinguishable from a thousands separator without knowing the locale,
/// and guessing wrong would either fabricate or drop. Such cells are refused,
/// which is the safe direction, and this is a known limitation.
fn leading_number(cell: &str) -> Option<f64> {
    let mut buf = String::new();
    for ch in cell.trim().chars() {
        match ch {
            '0'..='9' | '.' => buf.push(ch),
            '-' | '+' if buf.is_empty() => buf.push(ch),
            'e' | 'E' if !buf.is_empty() => buf.push(ch),
            // Separators inside a number carry no value.
            ',' | '_' | ' ' if !buf.is_empty() => {}
            _ => break,
        }
    }
    buf.parse::<f64>().ok()
}

fn cell_matches_value(cell: &str, value: f64) -> bool {
    let Some(cell_value) = leading_number(cell) else {
        return false;
    };
    if cell_value.is_nan() || cell_value.is_infinite() || value.is_nan() || value.is_infinite() {
        return false;
    }
    (cell_value - value).abs() <= 1e-9_f64.max(value.abs() * 1e-9)
}

/// The tabular containment gate. Every fact with a numeric `value` must be
/// traceable to a source cell: the value must occur in one of the cells the
/// extractor actually saw (`sample_rows`, the same bounded window the LLM
/// received). A value not found is dropped — never corrected, never stored.
/// Facts without a numeric value carry no number to verify and pass through
/// (they cannot introduce a number that is not in the source).
///
/// Containment is checked against the SAME window the extractor saw, not the
/// full table: a value from beyond `max_sample_rows` cannot have been read
/// by this run.
#[must_use]
pub fn gate_tabular_facts(
    facts: Vec<LocalFact>,
    sample_rows: &[Vec<String>],
    columns: &[String],
    file: &str,
) -> TabularFactOutcome {
    let mut outcome = TabularFactOutcome::default();
    for fact in facts {
        let Some(value) = fact.value else {
            outcome.kept.push(fact);
            continue;
        };
        match find_cell(
            value,
            &fact.subject,
            &fact.object,
            sample_rows,
            columns,
            file,
        ) {
            Some(cell) => {
                tracing::info!(
                    subject = %fact.subject,
                    object = %fact.object,
                    value,
                    column = %cell.column,
                    row = cell.row,
                    "tabular fact grounded in a source cell"
                );
                outcome.kept.push(fact);
            }
            None => {
                let LocalFact {
                    subject, object, ..
                } = fact;
                tracing::warn!(
                    subject = %subject,
                    object = %object,
                    value,
                    "tabular fact dropped before the store: its value is not in any cell the extractor saw"
                );
                outcome.dropped.push(DroppedTabularFact {
                    subject,
                    object,
                    value: Some(value),
                    reason: TabularDropReason::ValueNotInSource,
                });
            }
        }
    }
    outcome
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
        // The extractor saw one row whose cells hold both numeric values the
        // facts cite (density 7.8, Fe fraction 0.98), so both facts are
        // grounded in source cells and kept by the gate.
        let columns = vec![
            "name".to_string(),
            "density".to_string(),
            "fe_fraction".to_string(),
        ];
        let sample_rows = vec![vec![
            "Steel".to_string(),
            "7.8".to_string(),
            "0.98".to_string(),
        ]];

        let (update, dropped) = pipeline
            .write_local_graph(&entity_set, &source, &sample_rows, &columns)
            .await
            .unwrap();
        assert_eq!(update.nodes_created, 3);
        assert_eq!(update.edges_created, 2);
        assert!(
            dropped.is_empty(),
            "facts whose values are in the source must not be dropped"
        );

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
            facts_dropped: Vec::new(),
        };
        let json = serde_json::to_string(&result).unwrap();
        // None fields should not appear in JSON.
        assert!(!json.contains("entities"));
        assert!(!json.contains("graph_validation"));
        assert!(!json.contains("graph"));
        assert!(!json.contains("embeddings"));
        assert!(!json.contains("facts_dropped"));
        // No errors ⇒ no errors key either (clean success stays clean)…
        assert!(!json.contains("errors"));
        // …but step failures MUST be visible in the JSON (the old shape hid
        // failed steps entirely — audit critical #2).
        let failed = IngestResult {
            errors: vec!["local graph write failed: disk full".into()],
            facts_dropped: Vec::new(),
            ..result
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains("errors"));
        assert!(json.contains("disk full"));
    }

    /// The gate's core job: a tabular fact whose numeric value is NOT in any
    /// cell the extractor saw is refused — dropped, never corrected, never
    /// stored. The tabular equivalent of a quote not in the document.
    #[test]
    fn tabular_fact_whose_value_is_not_in_source_is_refused() {
        let columns = vec!["name".to_string(), "density".to_string()];
        let sample_rows = vec![
            vec!["Steel".to_string(), "7.8".to_string()],
            vec!["Copper".to_string(), "8.96".to_string()],
        ];
        // A number that appears nowhere in the source: fabrication.
        let fact = LocalFact {
            subject: "ghost alloy".into(),
            predicate: "HAS_PROPERTY".into(),
            object: "density".into(),
            value: Some(99.0),
            unit: Some("g/cm3".into()),
            confidence: Some(0.9),
            kind: Some("measurement".into()),
        };

        let outcome = gate_tabular_facts(vec![fact], &sample_rows, &columns, "/data/alloys.csv");
        assert!(
            outcome.kept.is_empty(),
            "a fabricated value must not be kept"
        );
        assert_eq!(outcome.dropped.len(), 1);
        let drop = &outcome.dropped[0];
        assert_eq!(drop.subject, "ghost alloy");
        assert_eq!(drop.object, "density");
        assert_eq!(drop.value, Some(99.0));
        assert_eq!(drop.reason, TabularDropReason::ValueNotInSource);
        assert_eq!(drop.reason.as_label(), "value_not_in_source");
    }

    /// Presence is not attribution. Two independent reviewers reproduced
    /// this: 7.8 sits in the `temperature` column, so a `density` fact for
    /// Steel resolved against it and was written -- with the wrong column
    /// recorded as its provenance.
    #[test]
    fn a_value_in_another_column_does_not_support_this_fact() {
        let columns = vec![
            "name".to_string(),
            "temperature".to_string(),
            "density".to_string(),
        ];
        let sample_rows = vec![vec![
            "Steel".to_string(),
            "7.8".to_string(),
            "2.0".to_string(),
        ]];
        let fact = LocalFact {
            subject: "Steel".into(),
            predicate: "HAS_PROPERTY".into(),
            object: "density".into(),
            value: Some(7.8),
            unit: Some("g/cm3".into()),
            confidence: Some(0.9),
            kind: Some("measurement".into()),
        };
        let outcome = gate_tabular_facts(vec![fact], &sample_rows, &columns, "/data/alloys.csv");
        assert_eq!(
            outcome.kept.len(),
            0,
            "a number in the wrong column is not evidence"
        );
        assert_eq!(outcome.dropped.len(), 1);
    }

    /// The row must be about this fact's subject.
    #[test]
    fn a_value_in_another_rows_subject_does_not_support_this_fact() {
        let columns = vec!["name".to_string(), "density".to_string()];
        let sample_rows = vec![
            vec!["Steel".to_string(), "7.8".to_string()],
            vec!["Aluminium".to_string(), "2.7".to_string()],
        ];
        let fact = LocalFact {
            subject: "Aluminium".into(),
            predicate: "HAS_PROPERTY".into(),
            object: "density".into(),
            value: Some(7.8),
            unit: Some("g/cm3".into()),
            confidence: Some(0.9),
            kind: Some("measurement".into()),
        };
        let outcome = gate_tabular_facts(vec![fact], &sample_rows, &columns, "/data/alloys.csv");
        assert_eq!(
            outcome.kept.len(),
            0,
            "Aluminium does not have Steel's density"
        );
    }

    /// Real exports carry units and separators in the cell. A strict f64 parse
    /// refused all of these, which made the gate lossy against ordinary CSVs.
    #[test]
    fn cells_carrying_units_or_separators_still_support_their_fact() {
        let columns = vec!["name".to_string(), "density".to_string()];
        for cell in ["7.8 g/cm3", "7.8", " 7.8  "] {
            let sample_rows = vec![vec!["Steel".to_string(), cell.to_string()]];
            let fact = LocalFact {
                subject: "Steel".into(),
                predicate: "HAS_PROPERTY".into(),
                object: "density".into(),
                value: Some(7.8),
                unit: Some("g/cm3".into()),
                confidence: Some(0.9),
                kind: Some("measurement".into()),
            };
            let outcome =
                gate_tabular_facts(vec![fact], &sample_rows, &columns, "/data/alloys.csv");
            assert_eq!(outcome.kept.len(), 1, "cell {cell:?} should support 7.8");
        }
        // thousands separator and underscore forms
        let columns = vec!["name".to_string(), "uts".to_string()];
        for cell in ["1,140", "1_140", "1140 MPa"] {
            let sample_rows = vec![vec!["Ti-6Al-4V".to_string(), cell.to_string()]];
            let fact = LocalFact {
                subject: "Ti-6Al-4V".into(),
                predicate: "HAS_PROPERTY".into(),
                object: "uts".into(),
                value: Some(1140.0),
                unit: Some("MPa".into()),
                confidence: Some(0.9),
                kind: Some("measurement".into()),
            };
            let outcome =
                gate_tabular_facts(vec![fact], &sample_rows, &columns, "/data/alloys.csv");
            assert_eq!(outcome.kept.len(), 1, "cell {cell:?} should support 1140");
        }
    }

    /// The legitimate case must not regress: a fact whose value genuinely is
    /// in a source cell is kept (fields intact) and not reported as dropped.
    #[test]
    fn genuine_tabular_fact_whose_value_is_in_source_is_kept() {
        let columns = vec!["name".to_string(), "density".to_string()];
        let sample_rows = vec![vec!["Steel".to_string(), "7.8".to_string()]];

        let fact = LocalFact {
            subject: "Steel".into(),
            predicate: "HAS_PROPERTY".into(),
            object: "density".into(),
            value: Some(7.8),
            unit: Some("g/cm3".into()),
            confidence: Some(0.9),
            kind: Some("measurement".into()),
        };

        let outcome = gate_tabular_facts(vec![fact], &sample_rows, &columns, "/data/alloys.csv");
        assert!(outcome.dropped.is_empty());
        assert_eq!(outcome.kept.len(), 1);
        assert_eq!(outcome.kept[0].subject, "Steel");
        assert_eq!(outcome.kept[0].value, Some(7.8));
    }

    /// A drop is never silent: the ingest result carries the refused facts
    /// (subject/object/value/reason) so a consumer — and the user via the
    /// summary — sees them. Mirrors the papers and local-text routes.
    #[test]
    fn refused_tabular_facts_are_surfaced_not_silent() {
        let columns = vec!["name".to_string(), "density".to_string()];
        let sample_rows = vec![vec!["Steel".to_string(), "7.8".to_string()]];
        let facts = vec![
            LocalFact {
                subject: "ghost".into(),
                predicate: "HAS_PROPERTY".into(),
                object: "density".into(),
                value: Some(99.0),
                unit: None,
                confidence: Some(0.9),
                kind: Some("measurement".into()),
            },
            LocalFact {
                subject: "phantom".into(),
                predicate: "HAS_PROPERTY".into(),
                object: "density".into(),
                value: Some(404.0),
                unit: None,
                confidence: Some(0.9),
                kind: Some("measurement".into()),
            },
        ];

        let outcome = gate_tabular_facts(facts, &sample_rows, &columns, "/data/alloys.csv");
        assert_eq!(outcome.dropped.len(), 2);

        let result = IngestResult {
            source: DataSource {
                path: "/data/alloys.csv".into(),
                format: "csv".into(),
            },
            schema: SchemaAnalysis {
                columns: columns.clone(),
                detected_types: vec![],
            },
            validation: crate::validation::ValidationReport {
                issues: vec![],
                passed: true,
            },
            row_count: 1,
            column_count: 2,
            entities: None,
            graph_validation: None,
            graph: None,
            embeddings: None,
            errors: Vec::new(),
            facts_dropped: outcome.dropped.clone(),
        };
        let json = serde_json::to_string(&result).unwrap();
        // Refusals must be visible in the serialized result, by name.
        assert!(json.contains("facts_dropped"));
        assert!(json.contains("ghost"));
        assert!(json.contains("phantom"));
        assert!(json.contains("value_not_in_source"));
        assert!(json.contains("99"));
        assert!(json.contains("404"));
    }
}
