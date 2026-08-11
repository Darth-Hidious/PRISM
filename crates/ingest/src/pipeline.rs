use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use polars::prelude::*;
use prism_provenance::{
    ClassifiedFactNodes, ClassifiedNode, EvidenceClass, LocalProvenance, OntologyClassification,
    ProvenanceStore,
};
use serde::{Deserialize, Serialize};
use tracing;

use crate::local_facts::{to_local_facts, to_semantic_local_facts};
use crate::ontology::LlmOntologyConstructor;
use crate::schema::SchemaDetector;
use crate::semantic_validation::{
    SemanticEntityProposal, SemanticValidationPolicy, SemanticValidationReport,
    validate_write_best_effort, validate_write_with_backend,
};
use crate::validation::{self, Severity, ValidationReport};
use crate::{
    DataSource, EmbeddingBatch, Entity, EntitySet, GraphUpdate, LlmConfig, Relationship,
    SchemaAnalysis,
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
    /// How the extraction request was decoded, when extraction ran: whether
    /// the endpoint enforced the ontology-derived JSON schema, why it
    /// degraded when it didn't (`degraded` non-None is a capability
    /// downgrade the caller MUST surface — a constraint that silently
    /// wasn't applied is a lie), and the seed/temperature actually sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_decoding: Option<prism_llm::JsonDecodingTrace>,
    /// SHACL-lite structural check on the extracted entities/relationships
    /// (orphan rels, unknown types, weight-sum sanity, etc.), run before the
    /// graph write. `None` only when no entities were extracted at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_validation: Option<crate::graph_validation::GraphValidationReport>,
    /// Advisory geometry reports for each graph batch that reached the write
    /// boundary. Every check carries an explicit applied/unavailable/
    /// disabled/failed status; findings never mutate or block the write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub semantic_validation: Vec<SemanticValidationReport>,
    /// Populated when the local EMMO graph write (bundled Turso store) runs.
    /// Counts are upsert attempts, not net-new rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphUpdate>,
    /// Always `None` since the Qdrant upsert step was removed (entity
    /// vectors live in the bundled Turso store); kept so the serialized
    /// result shape stays stable for older consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddings: Option<EmbeddingBatch>,
    /// Relationships dropped before the graph write because they referenced
    /// a name never declared in `entities` (referential containment): one
    /// entry per dropped relationship, naming the missing endpoint(s).
    /// NON-EMPTY is a PARTIAL result, not a step failure — every entity and
    /// every well-formed relationship was still stored, so this never joins
    /// `errors` (exit stays 0). It exists because the two alternatives are
    /// both worse: failing the whole ingest discards everything valid (the
    /// pre-containment behaviour that kept the graph empty), and inventing
    /// the missing entity would fabricate a type the extraction never
    /// stated. Callers MUST surface it — a drop the user cannot see is a
    /// silent drop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_relationships: Vec<String>,
    /// Entities dropped before the graph write because their declared type
    /// has no storage label in the active ontology (`Ontology::storage_label`
    /// returned `None`): one entry per dropped entity, naming the unmapped
    /// type. Same contract as `dropped_relationships` — a PARTIAL result,
    /// not a step failure (exit stays 0), because inventing a label for an
    /// undeclared type would store a vocabulary the ontology never stated,
    /// and failing the whole ingest would discard everything valid.
    /// Relationships referencing a dropped entity dangle and are dropped
    /// (and reported) with it. Callers MUST surface it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_entities: Vec<String>,
    /// Step failures. NON-EMPTY means configured pipeline steps did NOT
    /// complete — callers must surface these and exit non-zero. The old
    /// behavior (audit critical: log-and-None) made `prism ingest` print
    /// "Done." with exit 0 while storing nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    /// Rows actually sent through LLM extraction, summed over the batches
    /// that succeeded. `row_count` is what the source held; these two
    /// disagreeing is ALWAYS accompanied by a per-batch entry in `errors`
    /// naming the rows that were not processed and why. (History: exactly
    /// 10 rows of ANY dataset were extracted and the other rows were parsed
    /// and thrown away, reported nowhere.)
    #[serde(default)]
    pub rows_processed: usize,
    /// Extraction batches planned for this run (0 when extraction was not
    /// configured or refused). Batch size derives from the model's context
    /// window unless `[ingest] batch_rows` overrides it.
    #[serde(default)]
    pub batches: usize,
    /// Batches that did NOT complete (LLM failure, blocked validation, or a
    /// failed graph write) — each with a matching entry in `errors`. The
    /// other batches' facts are already stored: a mid-run failure costs the
    /// failed batch, never the run.
    #[serde(default)]
    pub batches_failed: usize,
    /// Token usage the backend reported, summed over every extraction call
    /// of the run. Output is metered and billed per token — this is what
    /// the run actually cost. `None` = the backend reported nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_usage: Option<prism_llm::UsageInfo>,
}

/// Progress sink for a pipeline run: one human-readable line per event
/// (plan, batch start, batch outcome). These runs are long by design — a
/// 12B local model takes minutes per batch — so a silent terminal is a bug;
/// the CLI prints these to stderr as they happen.
pub type ProgressFn = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// Configuration for a full ingest pipeline run.
#[derive(Clone)]
pub struct PipelineConfig {
    /// LLM config for entity extraction. If None, extraction is skipped.
    pub llm: Option<LlmConfig>,
    /// Rows per extraction batch (`[ingest] batch_rows`). `None` ⇒ batches
    /// are packed to a byte budget derived from the model's context window
    /// (`crate::batching`) — derived, not decreed. EVERY row is processed
    /// either way; this only shapes the batches. (History: this was
    /// `max_sample_rows: 10`, hardcoded in two places, and rows 11+ of any
    /// dataset were parsed and thrown away without a word.)
    pub batch_rows: Option<usize>,
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
    /// Advisory geometry policy applied before each graph write. Every
    /// threshold is declared here through a serializable policy object;
    /// callers may tune or disable checks without changing write semantics.
    pub semantic_validation: SemanticValidationPolicy,
    /// Progress sink; `None` = silent (library callers, tests).
    pub on_progress: Option<ProgressFn>,
}

impl std::fmt::Debug for PipelineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineConfig")
            .field("llm", &self.llm)
            .field("batch_rows", &self.batch_rows)
            .field("mapping", &self.mapping.is_some())
            .field("provenance_db", &self.provenance_db)
            .field("ontology", &self.ontology)
            .field("semantic_validation", &self.semantic_validation)
            .field("on_progress", &self.on_progress.is_some())
            .finish()
    }
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            llm: Some(LlmConfig::default()),
            batch_rows: None,
            mapping: None,
            provenance_db: None,
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
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
                batch_rows: None,
                mapping: None,
                provenance_db: None,
                ontology: None,
                semantic_validation: SemanticValidationPolicy::default(),
                on_progress: None,
            },
        }
    }

    /// Emit one progress line to the configured sink, if any.
    fn progress(&self, line: &str) {
        if let Some(sink) = &self.config.on_progress {
            sink(line);
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

        // Step 3: LLM entity extraction (if configured) — over EVERY row,
        // in successive batches sized to the model's context window. Each
        // batch is extracted, validated (SHACL-lite with the same contained
        // drop classes as before), and WRITTEN before the next one starts,
        // so a failure at batch 7 of 20 costs batch 7: everything already
        // written stays written and the failure lands on the errors spine.
        //
        // Error-severity input validation (empty frame, duplicate columns)
        // is a refusal to extract, not a warning to scroll past — see
        // `extraction_refusal`. Checked only when extraction is configured:
        // a schema-only run extracts nothing and already reports the full
        // `validation` field.
        let mut errors: Vec<String> = Vec::new();
        let refusal = self
            .config
            .llm
            .as_ref()
            .and_then(|_| extraction_refusal(&validation));
        if let Some(msg) = &refusal {
            tracing::error!("{msg}");
            errors.push(msg.clone());
        }
        let mut extraction_decoding: Option<prism_llm::JsonDecodingTrace> = None;
        // Facts land under the ontology's storage tenant: the default
        // ontology keeps the bare "local" tenant every existing store was
        // written with; any other ontology gets a composed tenant, which is
        // what keeps two vocabularies in one store from blending (the same
        // tenant-qualified isolation that separates local and peer knowledge).
        let tenant =
            crate::ontologies::storage_tenant(prism_provenance::LOCAL_TENANT, ontology.id());

        let mut merged: Option<EntitySet> = None;
        let mut dropped_relationships: Vec<String> = Vec::new();
        let mut dropped_entities: Vec<String> = Vec::new();
        let mut graph: Option<GraphUpdate> = None;
        let mut semantic_validation = Vec::new();
        let mut rows_processed = 0usize;
        let mut batches = 0usize;
        let mut batches_failed = 0usize;
        let mut llm_usage = None;

        if refusal.is_none()
            && let Some(ref llm_config) = self.config.llm
        {
            let constructor = LlmOntologyConstructor::new(llm_config.clone());
            let all_rows = extract_all_rows(&df);

            // Batch plan: the operator's `[ingest] batch_rows` override, or
            // a byte budget DERIVED from the model's context window — the
            // binding constraint, which the local runtime reports
            // (`probe_context_window`). An unknown window is a config gap:
            // it is SAID, and the documented fallback bridges it.
            let (plan, plan_note) = match self.config.batch_rows.filter(|n| *n > 0) {
                Some(n) => (
                    crate::batching::pack_row_batches(&all_rows, 0, Some(n)),
                    format!("{n} row(s) per batch ([ingest] batch_rows override)"),
                ),
                None => {
                    let context_window = constructor.probe_context_window().await;
                    let budget = crate::batching::input_byte_budget(context_window);
                    let note = match context_window {
                        Some(cw) => format!(
                            "batch budget {budget} bytes of row text, derived from the \
                             model's {cw}-token context window"
                        ),
                        None => format!(
                            "context window UNKNOWN (neither configured nor reported by \
                             the backend) — assuming the documented {}-token fallback; \
                             batch budget {budget} bytes. Set [ingest] batch_rows in \
                             prism.toml to override.",
                            crate::batching::FALLBACK_CONTEXT_TOKENS
                        ),
                    };
                    (
                        crate::batching::pack_row_batches(&all_rows, budget, None),
                        note,
                    )
                }
            };
            batches = plan.len();

            // Say what this run will roughly cost BEFORE it starts: row
            // bytes at the same ~4-bytes/token estimate the client budgets
            // with. Output is metered and billed per token — counting is
            // the control, so the actual usage is reported at the end.
            let row_bytes: usize = all_rows
                .iter()
                .map(|r| format!("{r:?}").len() + 12)
                .sum::<usize>();
            self.progress(&format!(
                "extraction plan: {} row(s) in {} batch(es); {}; ~{} tokens of row data \
                 will be sent (plus per-batch prompt scaffold); output is metered and \
                 billed per token",
                all_rows.len(),
                batches,
                plan_note,
                row_bytes / crate::batching::EST_BYTES_PER_TOKEN as usize,
            ));
            tracing::info!(
                model = %llm_config.model,
                rows = all_rows.len(),
                batches,
                "sending to LLM for entity extraction"
            );

            for (index, (start, end)) in plan.iter().enumerate() {
                let batch_no = index + 1;
                let rows = &all_rows[*start..*end];
                self.progress(&format!(
                    "batch {batch_no}/{batches}: extracting rows {}-{} of {}…",
                    start + 1,
                    end,
                    all_rows.len()
                ));
                let batch_tag = format!("batch {batch_no}/{batches} (rows {}-{end})", start + 1);
                let extracted = constructor
                    .extract_entities_traced(
                        ontology.as_ref(),
                        &schema,
                        rows,
                        self.config.mapping.as_ref(),
                    )
                    .await;
                let traced = match extracted {
                    Ok(traced) => traced,
                    Err(e) => {
                        tracing::error!("{batch_tag}: LLM extraction failed: {e:#}");
                        errors.push(format!("{batch_tag}: LLM extraction failed: {e:#}"));
                        batches_failed += 1;
                        self.progress(&format!(
                            "batch {batch_no}/{batches}: FAILED — continuing with the \
                             remaining batches"
                        ));
                        continue;
                    }
                };
                let batch_decoding = traced.decoding;
                let entity_set = traced.entities;
                // One trace describes the run for the summary. The first
                // batch claims the slot; a DEGRADED batch always overrides a
                // clean one — a capability downgrade anywhere in the run
                // must never be masked by an earlier batch that had it.
                let replace_trace = match &extraction_decoding {
                    None => true,
                    Some(prev) => prev.degraded.is_none() && batch_decoding.degraded.is_some(),
                };
                if replace_trace {
                    if let Some(reason) = &batch_decoding.degraded {
                        // Degradation is honest at every layer: logged here,
                        // carried on the result for the CLI summary.
                        tracing::warn!("constrained extraction degraded: {reason}");
                    }
                    extraction_decoding = Some(batch_decoding.clone());
                }

                // Step 3.5 per batch: graph quality validation (SHACL-lite)
                // — this used to be a documented "runs before writing to
                // Neo4j" gate with zero callers (AUDIT_BACKLOG 20 /
                // INGESTION_AUDIT #20), so LLM extraction output went
                // straight to the graph unchecked. Error-severity issues
                // refuse the batch's write, with TWO contained classes: a
                // relationship whose endpoint was never declared
                // (`orphan_rel`) invalidates THAT relationship, and an
                // entity whose name normalised to nothing invalidates THAT
                // entity — each is dropped and reported, and everything
                // valid is still stored. Failing wholesale here is what
                // kept the graph empty (2026-08-08: 17 orphan errors
                // discarded 13 good entities); inventing a missing endpoint
                // would fabricate a type.
                let (report, batch_plan) =
                    validate_before_graph_write(ontology.as_ref(), &entity_set);
                merge_extraction(&mut merged, &entity_set);
                match batch_plan {
                    GraphWritePlan::Blocked(msg) => {
                        tracing::error!(issues = report.issues.len(), "{batch_tag}: {msg}");
                        errors.push(format!("{batch_tag}: {msg}"));
                        batches_failed += 1;
                        self.progress(&format!(
                            "batch {batch_no}/{batches}: BLOCKED — nothing from this batch \
                             was written; continuing with the remaining batches"
                        ));
                    }
                    GraphWritePlan::Proceed {
                        set,
                        dropped,
                        dropped_entities: dropped_ents,
                    } => {
                        if !dropped_ents.is_empty() {
                            tracing::warn!(
                                dropped = dropped_ents.len(),
                                extracted = entity_set.entities.len(),
                                "entities of types the active ontology maps to no storage \
                                 label were dropped; the valid remainder is stored"
                            );
                        }
                        if !dropped.is_empty() {
                            tracing::warn!(
                                dropped = dropped.len(),
                                extracted = entity_set.relationships.len(),
                                "relationships referencing undeclared entities were \
                                 dropped; the valid remainder is stored"
                            );
                        }
                        dropped_relationships.extend(dropped);
                        dropped_entities.extend(dropped_ents);

                        // Step 4 per batch: local graph write into the
                        // bundled Turso store (replaced the Neo4j upsert —
                        // Neo4j retirement, step 1). Writing here, inside
                        // the loop, is what makes partial progress durable.
                        // Same-document batches CANNOT corroborate each
                        // other: every batch writes under this run's one
                        // provenance source (the file), and the store keys
                        // evidence independence on the origin source
                        // (`origin_source_key`), so a fact asserted by two
                        // batches counts ONCE.
                        match self
                            .write_local_graph(
                                ontology.as_ref(),
                                &set,
                                &source,
                                &tenant,
                                Some(&batch_decoding),
                            )
                            .await
                        {
                            Ok((update, fact_drops, semantic_report)) => {
                                // Facts the fact-mapping refused (a numeric
                                // value whose unit is missing or
                                // unresolvable — never stored unit-less)
                                // join the same reported drop list as
                                // referential containment: a PARTIAL result
                                // the caller must surface, never a silent
                                // drop.
                                if !fact_drops.is_empty() {
                                    tracing::warn!(
                                        dropped = fact_drops.len(),
                                        "numeric facts with unresolvable units were dropped; \
                                         the valid remainder is stored"
                                    );
                                }
                                dropped_relationships.extend(fact_drops);
                                semantic_validation.push(semantic_report);
                                rows_processed += end - start;
                                self.progress(&format!(
                                    "batch {batch_no}/{batches}: {} entities, {} \
                                     relationships → {} node(s), {} edge(s) written",
                                    set.entities.len(),
                                    set.relationships.len(),
                                    update.nodes_created,
                                    update.edges_created,
                                ));
                                let sum = graph.get_or_insert(GraphUpdate {
                                    nodes_created: 0,
                                    edges_created: 0,
                                });
                                sum.nodes_created += update.nodes_created;
                                sum.edges_created += update.edges_created;
                            }
                            Err(e) => {
                                tracing::error!("{batch_tag}: local graph write failed: {e:#}");
                                errors
                                    .push(format!("{batch_tag}: local graph write failed: {e:#}"));
                                batches_failed += 1;
                            }
                        }
                    }
                }
            }

            llm_usage = constructor.total_usage();
            if let Some(usage) = &llm_usage {
                self.progress(&format!(
                    "LLM usage (billed): {} prompt + {} completion = {} tokens",
                    usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                ));
            }
            tracing::info!(
                rows_processed,
                batches,
                batches_failed,
                "LLM extraction complete"
            );
        }

        // The validation record over EVERYTHING the model emitted this run
        // (merged, de-duplicated) — the honest record, orphans included.
        // Per-batch validation above is what GATED the writes; this field
        // reports.
        let graph_validation = merged
            .as_ref()
            .map(|set| crate::graph_validation::validate_graph(ontology.as_ref(), set));

        // Entity vectors are written to the bundled Turso store by
        // `write_local_graph`, reusing the validation batch; the old Qdrant
        // upsert step was redundant and has been removed.
        Ok(IngestResult {
            source,
            schema,
            validation,
            row_count,
            column_count,
            entities: merged,
            extraction_decoding,
            graph_validation,
            semantic_validation,
            graph,
            embeddings: None,
            dropped_relationships,
            dropped_entities,
            errors,
            rows_processed,
            batches,
            batches_failed,
            llm_usage,
        })
    }

    /// Write the extracted entities/relationships as typed facts (with one
    /// PROV-O activity for the run) into the bundled Turso provenance store,
    /// under the active ontology's storage tenant.
    ///
    /// EVERY node label written here is produced by the active ontology's
    /// declared storage mapping (`Ontology::storage_label`) over the
    /// entity types the extraction declared — the same declaration the
    /// prompt and validator read. Nothing on this path invents a label or
    /// falls back to one the declaration does not produce: an entity whose
    /// type has no storage label is refused loudly (the write plan drops
    /// and reports such entities before this runs, so hitting the refusal
    /// means a caller bypassed the plan). The one store-owned exception is
    /// the synthetic `Measurement` node a measurement fact mints — a fact
    /// shape, not an extracted entity.
    ///
    /// The second return value carries one reason per fact the mapping
    /// REFUSED to write — a numeric value whose unit is missing or resolves
    /// to no QUDT identifier is dropped whole, never stored unit-less (see
    /// `to_local_facts`). The caller must surface these alongside
    /// `dropped_relationships`. The dropped fact's endpoints still land as
    /// standalone typed nodes below: the entities exist, only the
    /// unit-less numeric claim is refused.
    async fn write_local_graph(
        &self,
        ontology: &dyn crate::ontologies::Ontology,
        entity_set: &EntitySet,
        source: &DataSource,
        tenant: &str,
        decoding: Option<&prism_llm::JsonDecodingTrace>,
    ) -> Result<(GraphUpdate, Vec<String>, SemanticValidationReport)> {
        self.write_local_graph_with_semantic_backend(
            ontology, entity_set, source, tenant, decoding, None,
        )
        .await
    }

    /// Shared write implementation with a deterministic embedding seam for
    /// integration tests. Production passes `None` and constructs the
    /// configured backend; tests may inject a backend so a real geometric
    /// finding crosses the complete pre-write/write boundary without network
    /// access or model downloads.
    async fn write_local_graph_with_semantic_backend(
        &self,
        ontology: &dyn crate::ontologies::Ontology,
        entity_set: &EntitySet,
        source: &DataSource,
        tenant: &str,
        decoding: Option<&prism_llm::JsonDecodingTrace>,
        semantic_backend: Option<&dyn prism_embed::EmbedBackend>,
    ) -> Result<(GraphUpdate, Vec<String>, SemanticValidationReport)> {
        // Declared name → ontology classification, from the ONE active
        // declaration. First declaration wins on a (rare)
        // same-name/different-type collision, matching the standalone-write
        // dedup below. The compatibility storage label remains the entity-key
        // input; the declared type and canonical IRI are additive metadata.
        let mut classifications: std::collections::HashMap<&str, ClassifiedNode<'_>> =
            std::collections::HashMap::new();
        for e in &entity_set.entities {
            let extraction_label = e.entity_type.trim();
            let class = ontology.class_for_label(extraction_label).ok_or_else(|| {
                anyhow::anyhow!(
                    "entity '{}' has type '{}', which ontology '{}' resolves to no class IRI — \
                     the write plan must drop and report it, never invent a canonical identity",
                    e.name,
                    e.entity_type,
                    ontology.id()
                )
            })?;
            let declared_type = class
                .extraction_labels
                .iter()
                .find(|label| label.as_str() == extraction_label)
                .map(String::as_str)
                .expect("class_for_label returned a declaration carrying the exact label");
            let storage_label = ontology.storage_label(declared_type).ok_or_else(|| {
                anyhow::anyhow!(
                    "entity '{}' has type '{}', which ontology '{}' maps to no storage label — \
                     the write plan must drop and report it, never store an undeclared label",
                    e.name,
                    e.entity_type,
                    ontology.id()
                )
            })?;
            classifications
                .entry(e.name.as_str())
                .or_insert(ClassifiedNode {
                    entity_type: declared_type,
                    storage_label,
                    class_iri: class.iri.as_str(),
                });
        }
        let classification_of = |name: &str| -> Result<ClassifiedNode<'_>> {
            classifications.get(name).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "no declared entity (and so no class IRI/storage label) for fact endpoint \
                     '{name}' — dangling relationships must be dropped before the write"
                )
            })
        };
        let db_path = match &self.config.provenance_db {
            Some(p) => p.clone(),
            None => dirs::home_dir()
                .map(|h| h.join(".prism/provenance.db"))
                .unwrap_or_else(|| PathBuf::from("provenance.db")),
        };
        let store = ProvenanceStore::open(&db_path).await?;

        let (facts, mut dropped_facts) = to_local_facts(entity_set);
        // The persisted facts keep the historical 0.8 fallback. Semantic
        // fusion uses the same mapper with raw optional confidence retained,
        // so repeated endpoint triples with different values/confidences
        // remain correctly paired and an omitted score stays honest.
        let (semantic_facts, semantic_dropped_facts) = to_semantic_local_facts(entity_set);
        debug_assert_eq!(
            dropped_facts, semantic_dropped_facts,
            "confidence handling cannot change the mapped write set"
        );
        let semantic_entities: Vec<SemanticEntityProposal> = entity_set
            .entities
            .iter()
            .map(|entity| {
                let class = ontology
                    .class_for_label(entity.entity_type.trim())
                    .expect("write plan retained only ontology-declared entity types");
                let storage_label = ontology
                    .storage_label(entity.entity_type.trim())
                    .expect("write plan retained only entity types with storage labels");
                SemanticEntityProposal {
                    name: entity.name.clone(),
                    entity_type: entity.entity_type.clone(),
                    storage_label: storage_label.to_string(),
                    class_iri: Some(class.iri.to_string()),
                }
            })
            .collect();
        // Geometry reads the PRE-WRITE graph. It is deliberately kept out of
        // `GraphWritePlan`: no distance or fused score below can merge, drop,
        // rewrite, or block model-proposed data.
        let semantic = match semantic_backend {
            Some(backend) => {
                validate_write_with_backend(
                    &store,
                    &semantic_entities,
                    &semantic_facts,
                    tenant,
                    &self.config.semantic_validation,
                    Some(backend),
                )
                .await
            }
            None => {
                validate_write_best_effort(
                    &store,
                    &semantic_entities,
                    &semantic_facts,
                    tenant,
                    &self.config.semantic_validation,
                )
                .await
            }
        };

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
        // Reproducibility record, on the SAME activity row: the seed and
        // temperature the extraction actually sent (None when the backend
        // offered no such knob) and the decoding mode that really applied —
        // so a difference between two runs is attributable to input, model,
        // or an honest capability downgrade, never to an unrecorded knob.
        if let Some(trace) = decoding {
            // The reasoning kill-switch is part of the record: a thinking
            // and a non-thinking run of the same seed are different
            // computations, and the difference must stay attributable.
            let mode = if trace.no_think {
                format!("{}+no_think", trace.mode.as_str())
            } else {
                trace.mode.as_str().to_string()
            };
            store
                .record_activity_decoding(
                    &prov.activity_id,
                    &prism_provenance::ActivityDecoding {
                        seed: trace.seed,
                        temperature: trace.temperature,
                        mode: Some(&mode),
                    },
                )
                .await?;
        }

        let ontology_classification = OntologyClassification {
            version_iri: ontology.version_iri().as_str(),
            artifact_sha256: ontology.artifact_sha256(),
        };

        for fact in &facts {
            let nodes = ClassifiedFactNodes {
                subject: classification_of(&fact.subject)?,
                object: classification_of(&fact.object)?,
            };
            store
                .write_classified_fact_with_evidence(
                    fact,
                    &prov,
                    EvidenceClass::Research,
                    nodes,
                    ontology_classification,
                )
                .await?;
        }

        // Count what the store actually received, not what the LLM proposed.
        // `to_local_facts` maps RELATIONSHIPS, so the fact writes above
        // upserted exactly the endpoint names.
        let mut written: std::collections::HashSet<&str> = facts
            .iter()
            .flat_map(|f| [f.subject.as_str(), f.object.as_str()])
            .collect();

        // Entities in no stored relationship land as standalone typed nodes:
        // the extraction asserted they exist, and an absent relationship —
        // or one dropped by referential containment — must not erase them.
        // Their label comes from the SAME declared storage mapping the fact
        // writes above used, so an entity lands under one identity whether
        // its edges survived or dangled. (They used to be dropped with a
        // warning; then stored under the raw extraction type, which split
        // them from their fact-written selves — `Alloy:X` standalone vs
        // `Matter:X` as a fact subject.)
        for e in &entity_set.entities {
            let props = match &e.properties {
                serde_json::Value::Object(map) if !map.is_empty() => Some(e.properties.to_string()),
                _ => None,
            };
            // An entity a relationship already wrote used to `continue` HERE,
            // before its properties were passed to the store — so the only
            // entities that kept their extracted properties were the ones in
            // no relationship at all. In a real paper the subject of every
            // fact ("Ti-6Al-4V") is exactly the entity whose properties were
            // thrown away, and the orphans nobody asked about kept theirs.
            //
            // Writing again is safe and is not a second node: the label comes
            // from the same `classification_of` the fact writes above used, so
            // the key is identical.
            //
            // But `props_json = COALESCE(excluded.props_json, …)` replaces the
            // WHOLE column — it is not a per-key JSON merge. `classifications`
            // is keyed by NAME ALONE (first declaration wins), and Check 2 in
            // graph_validation keys duplicate detection on (type, name), so two
            // entities sharing a name under DIFFERENT types reach here
            // unflagged and both resolve to the first-won class. Letting the
            // second one write would overwrite the first's properties with a
            // set the extractor itself attributed to another class — the
            // alloy's composition replaced by a phase's crystal structure,
            // unrecoverable, and then served as that alloy's properties.
            //
            // So: only the entity whose own declared type produced the winning
            // classification may write properties for that name. A loser is
            // dropped and REPORTED, never silently merged into the winner.
            let is_new_node = written.insert(e.name.as_str());
            if !is_new_node && props.is_none() {
                continue; // a fact wrote the node and there is nothing to add
            }
            let class = classification_of(&e.name)?;
            if props.is_some() && class.entity_type != e.entity_type.trim() {
                dropped_facts.push(format!(
                    "entity '{}' declared type '{}' collides with '{}' — another entity of that \
                     name was declared first and owns the stored identity, so these properties \
                     are dropped rather than overwriting a different class's properties",
                    e.name,
                    e.entity_type.trim(),
                    class.entity_type
                ));
                continue;
            }
            store
                .write_classified_entity(&e.name, class, props, &prov.tenant)
                .await?;
        }

        // Reuse the exact one-pass vectors semantic validation prepared. The
        // model is never called a second time after the write; storage remains
        // best-effort and cannot turn a successful graph write into failure.
        if let Some(model) = semantic.embedding_model() {
            match store
                .store_precomputed_name_embeddings(
                    semantic.embedding_names(),
                    semantic.embedding_vectors(),
                    &prov.tenant,
                    model,
                )
                .await
            {
                Ok(stored) => {
                    tracing::debug!(stored, tenant = %prov.tenant, model, "entity vectors stored in Turso")
                }
                Err(error) => tracing::warn!(
                    "entity embedding storage failed: {error:#} — graph write unaffected"
                ),
            }
        }

        Ok((
            GraphUpdate {
                nodes_created: written.len(),
                edges_created: facts.len(),
            },
            dropped_facts,
            semantic.report,
        ))
    }
}

impl Default for IngestPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// What pre-write graph validation decided may be written.
enum GraphWritePlan {
    /// Write `set` — the extracted set minus entities whose declared type
    /// the active ontology maps to no storage label, minus entities whose
    /// name normalised to nothing at the extraction boundary, and minus any
    /// dangling relationships (including ones dangling BECAUSE their
    /// endpoint was dropped). `dropped` carries one reason per dropped
    /// relationship, `dropped_entities` one per dropped entity. Containment,
    /// not repair: nothing is invented — not a missing endpoint, not a
    /// storage label — and every drop reaches the caller
    /// (`IngestResult::dropped_relationships` / `dropped_entities`).
    Proceed {
        set: EntitySet,
        dropped: Vec<String>,
        dropped_entities: Vec<String>,
    },
    /// Error-severity issues a targeted drop cannot repair: write nothing.
    Blocked(String),
}

/// Run graph-quality validation on extracted entities — against the ACTIVE
/// ontology's vocabulary — and decide what the graph write may store.
/// Returns the full report on the extraction AS THE MODEL EMITTED IT (the
/// honest record, orphans included) plus the plan.
///
/// Two error classes are containable, both by dropping exactly the claim
/// they invalidate: `orphan_rel` (a relationship whose `from`/`to` was
/// never declared — the edge is dropped) and `unit_kind_mismatch` (an
/// entity whose measurement unit contradicts the quantity its name states,
/// e.g. a density under `QUDT:GigaPA` — the entity is dropped, and any
/// relationship referencing it dangles and is dropped with it). Dropping
/// loses one claim; "repairing" the unit would fabricate a measurement the
/// extraction never made, and failing the whole ingest discards every
/// valid fact with it (the pre-containment behaviour that kept the graph
/// empty — the same wholesale destruction the strict QudtUnit deserialiser
/// inflicted on text ingest). Every other error-severity issue — empty
/// names, zero entities, an ontology's own domain errors — still blocks
/// the write, proven by RE-validating the reduced set rather than by
/// trusting issue categories.
/// Two error classes are containable. `orphan_rel`: a relationship whose
/// `from`/`to` was never declared — dropping it loses one claim; inventing
/// the endpoint would require fabricating a type, and failing the whole
/// ingest discards every valid fact with it (the pre-containment behaviour
/// that kept the graph empty). `empty_name`: an entity whose name
/// normalised to NOTHING at the extraction boundary (the model emitted only
/// quote characters or whitespace) — there is nothing to store or key on,
/// so it is dropped and reported via `dropped_entities`, and anything that
/// referenced it dangles and is dropped with it. Every other
/// error-severity issue — zero entities, an ontology's own domain errors —
/// still blocks the write, proven by RE-validating the reduced set rather
/// than by trusting issue categories.
fn validate_before_graph_write(
    ontology: &dyn crate::ontologies::Ontology,
    entity_set: &EntitySet,
) -> (
    crate::graph_validation::GraphValidationReport,
    GraphWritePlan,
) {
    let report = crate::graph_validation::validate_graph(ontology, entity_set);

    // Entities whose declared type the active ontology maps to no storage
    // label cannot be persisted without inventing a vocabulary the ontology
    // never stated (`unknown_type` is Warning severity, so `report.passed`
    // alone never catches this). Drop and report them; relationships that
    // referenced them dangle and are dropped (and reported) below.
    let (kept_entities, dropped_entities): (Vec<Entity>, Vec<String>) = {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        let declared = ontology
            .classes()
            .iter()
            .flat_map(|class| class.extraction_labels.iter())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        for e in &entity_set.entities {
            // A name the extraction boundary normalised to NOTHING (the
            // model emitted only quote characters or whitespace) is a
            // rejection, not a storable empty name — there is nothing to
            // key on. Drop-and-report, like the unmapped-type case below.
            if e.name.trim().is_empty() {
                dropped.push(format!(
                    "entity of type '{}': empty name after normalisation — rejected, not stored",
                    e.entity_type
                ));
                continue;
            }
            let extraction_label = e.entity_type.trim();
            if ontology.class_for_label(extraction_label).is_none()
                || ontology.storage_label(extraction_label).is_none()
            {
                dropped.push(format!(
                    "entity '{}': type '{}' has no storage label or canonical class IRI in ontology '{}' \
                     (declared: {})",
                    e.name,
                    e.entity_type,
                    ontology.id(),
                    declared
                ));
            } else if let Some(reason) = crate::graph_validation::unit_kind_mismatch(e) {
                // The quantity-kind contradiction (Check 11, Error): the
                // POISONED claim is dropped and reported, the rest of the
                // document is stored. The unit is never rewritten — that
                // would fabricate a measurement the extraction never made.
                dropped.push(reason);
            } else if let Some(reason) =
                crate::graph_validation::measurement_packed_in_name(ontology, e)
            {
                // The form-versus-field corruption (Check 12, Error): a
                // quantitative entity NAMED after its measurement
                // ("1100 MPa") is dropped and reported, never stored as a
                // fake Property — the name is an identity key, and a number
                // inside it is unqueryable text. Its relationships dangle
                // and are dropped (and reported) below.
                dropped.push(reason);
            } else {
                kept.push(e.clone());
            }
        }
        (kept, dropped)
    };

    if report.passed && dropped_entities.is_empty() {
        let plan = GraphWritePlan::Proceed {
            set: entity_set.clone(),
            dropped: Vec::new(),
            dropped_entities: Vec::new(),
        };
        return (report, plan);
    }

    let reduced = EntitySet {
        entities: kept_entities,
        relationships: entity_set.relationships.clone(),
    };
    let (kept, dropped) = partition_dangling_relationships(&reduced);
    if dropped.is_empty() && dropped_entities.is_empty() {
        // Nothing dangled and nothing was unmapped — the errors are of a
        // kind a drop cannot repair.
        let msg = blocking_message(&report);
        return (report, GraphWritePlan::Blocked(msg));
    }
    let set = EntitySet {
        entities: reduced.entities,
        relationships: kept,
    };
    // Fail-closed proof that the drops repaired EVERYTHING at error
    // severity: the reduced set must validate clean of errors, or the
    // write stays blocked exactly as before.
    let recheck = crate::graph_validation::validate_graph(ontology, &set);
    if recheck.passed {
        (
            report,
            GraphWritePlan::Proceed {
                set,
                dropped,
                dropped_entities,
            },
        )
    } else {
        (report, GraphWritePlan::Blocked(blocking_message(&recheck)))
    }
}

/// The refusal message for a report whose error-severity issues block the
/// graph write (unchanged wording from the pre-containment gate).
fn blocking_message(report: &crate::graph_validation::GraphValidationReport) -> String {
    let error_issues: Vec<&str> = report
        .issues
        .iter()
        .filter(|i| i.severity == crate::graph_validation::GraphSeverity::Error)
        .map(|i| i.message.as_str())
        .collect();
    format!(
        "graph validation failed ({} error-severity issue(s)): {}",
        error_issues.len(),
        error_issues.join("; ")
    )
}

/// Split relationships into those whose endpoints are all declared entities
/// and those referencing an undeclared name — one human-readable reason per
/// dropped relationship, naming exactly which endpoint(s) were missing.
/// Membership is by entity NAME, the same set `validate_graph`'s orphan
/// check (Check 5) tests.
fn partition_dangling_relationships(set: &EntitySet) -> (Vec<Relationship>, Vec<String>) {
    let names: std::collections::HashSet<&str> =
        set.entities.iter().map(|e| e.name.as_str()).collect();
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for r in &set.relationships {
        let mut missing = Vec::new();
        if !names.contains(r.from.as_str()) {
            missing.push(r.from.as_str());
        }
        if !names.contains(r.to.as_str()) && r.to != r.from {
            missing.push(r.to.as_str());
        }
        if missing.is_empty() {
            kept.push(r.clone());
        } else {
            dropped.push(format!(
                "{}-[{}]->{}: undeclared endpoint(s): {}",
                r.from,
                r.rel_type,
                r.to,
                missing.join(", ")
            ));
        }
    }
    (kept, dropped)
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

/// EVERY row of the DataFrame as `Vec<Vec<String>>`. Deliberately no `max`
/// parameter: the caller batches; a row limit here is exactly the silent
/// 10-row cap this replaced (a 10,000-row dataset lost 9,990 rows, reported
/// nowhere).
fn extract_all_rows(df: &DataFrame) -> Vec<Vec<String>> {
    (0..df.height())
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

/// Fold one batch's raw extraction into the run-level record, de-duplicated:
/// entities by name (first declaration wins, matching the write path's
/// collision rule), relationships by `(from, rel_type, to)`. This is the
/// REPORTING merge only — it never fabricates corroboration, and the store
/// independently refuses same-source corroboration by keying evidence on
/// the origin source (`origin_source_key`), so two batches of ONE file
/// asserting one fact count once at both layers.
fn merge_extraction(merged: &mut Option<EntitySet>, batch: &EntitySet) {
    let acc = merged.get_or_insert_with(|| EntitySet {
        entities: Vec::new(),
        relationships: Vec::new(),
    });
    for e in &batch.entities {
        if !acc.entities.iter().any(|x| x.name == e.name) {
            acc.entities.push(e.clone());
        }
    }
    for r in &batch.relationships {
        if !acc
            .relationships
            .iter()
            .any(|x| x.from == r.from && x.rel_type == r.rel_type && x.to == r.to)
        {
            acc.relationships.push(r.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DeterministicPipelineEmbed;

    #[async_trait::async_trait]
    impl prism_embed::EmbedBackend for DeterministicPipelineEmbed {
        async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
        }

        fn dimensions(&self) -> usize {
            3
        }

        fn id(&self) -> &str {
            "test:pipeline-semantic-v1"
        }
    }

    #[test]
    fn extract_all_rows_reads_every_row() {
        let df = df!(
            "name" => &["Steel", "Copper", "Aluminum"],
            "density" => &[7.8, 8.96, 2.7]
        )
        .unwrap();

        let rows = extract_all_rows(&df);
        assert_eq!(rows.len(), 3, "every row, not a sample");
        assert_eq!(rows[0].len(), 2);
        assert!(rows[0][0].contains("Steel"));
        assert!(rows[2][0].contains("Aluminum"));
    }

    #[test]
    fn extract_all_rows_empty_df() {
        let df = DataFrame::empty();
        assert!(extract_all_rows(&df).is_empty());
    }

    #[test]
    fn merge_extraction_deduplicates_across_batches() {
        use crate::{Entity, Relationship};
        let batch = |names: &[&str], rels: &[(&str, &str)]| EntitySet {
            entities: names
                .iter()
                .map(|n| Entity {
                    entity_type: "Element".into(),
                    name: (*n).into(),
                    properties: serde_json::json!({}),
                })
                .collect(),
            relationships: rels
                .iter()
                .map(|(f, t)| Relationship {
                    from: (*f).into(),
                    rel_type: "CONTAINS".into(),
                    to: (*t).into(),
                    weight: None,
                    order: None,
                    // Composition links, not measurements: the per-edge value
                    // and unit channel belongs to HAS_PROPERTY.
                    value: None,
                    unit: None,
                    confidence: None,
                })
                .collect(),
        };
        let mut merged = None;
        merge_extraction(&mut merged, &batch(&["Fe", "Steel"], &[("Steel", "Fe")]));
        merge_extraction(&mut merged, &batch(&["Fe", "Ni"], &[("Steel", "Fe")]));
        let merged = merged.unwrap();
        assert_eq!(merged.entities.len(), 3, "Fe must merge, not duplicate");
        assert_eq!(merged.relationships.len(), 1, "the repeated edge merges");
    }

    /// A dangling endpoint invalidates THAT relationship, not the ingest:
    /// the plan keeps every entity, drops exactly the dangling edge, and
    /// reports it — while the report stays an honest record of the raw
    /// extraction (`orphan_rel` Error, `passed == false`). The missing
    /// entity is NOT invented into the write set.
    #[test]
    fn validate_before_graph_write_contains_dangling_relationships() {
        use crate::{Entity, Relationship};
        // "Fe" is referenced by the relationship but never extracted as an
        // entity. This used to fail the whole ingest, storing nothing.
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
                value: None,
                unit: None,
                confidence: None,
            }],
        };
        let (report, plan) =
            validate_before_graph_write(&crate::ontologies::EmmoOntology, &entity_set);
        assert!(!report.passed, "the report keeps recording the orphan");
        assert!(report.issues.iter().any(|i| i.category == "orphan_rel"));
        let GraphWritePlan::Proceed {
            set,
            dropped,
            dropped_entities,
        } = plan
        else {
            panic!("a purely-dangling extraction must be contained, not blocked");
        };
        assert!(
            dropped_entities.is_empty(),
            "every declared type is mapped — nothing to drop: {dropped_entities:?}"
        );
        assert_eq!(set.entities.len(), 1, "every declared entity is kept");
        assert!(
            set.relationships.is_empty(),
            "the dangling relationship must not be written"
        );
        assert!(
            !set.entities.iter().any(|e| e.name == "Fe"),
            "the missing endpoint must never be auto-declared"
        );
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("Fe"), "{}", dropped[0]);
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
        let (report, plan) =
            validate_before_graph_write(&crate::ontologies::EmmoOntology, &entity_set);
        assert!(report.passed);
        let GraphWritePlan::Proceed {
            set,
            dropped,
            dropped_entities,
        } = plan
        else {
            panic!("clean entities must proceed");
        };
        assert_eq!(set.entities.len(), 1);
        assert!(dropped.is_empty());
        assert!(dropped_entities.is_empty());
    }

    /// The measured live defect, contained at the production gate: a
    /// Property NAMED "1100 MPa" (measurement packed into the name — the
    /// form-versus-field failure) is dropped and REPORTED, never stored as
    /// a fake Property; the relationship referencing it dangles and drops
    /// with it; and the properly-typed sibling measurement is kept whole.
    /// Removing the containment branch turns this Proceed into a Blocked —
    /// this test dies either way a mutation leans.
    #[test]
    fn validate_before_graph_write_drops_and_reports_measurement_named_properties() {
        use crate::{Entity, Relationship};
        let rel = |to: &str| Relationship {
            from: "Inconel 718".into(),
            rel_type: "HAS_PROPERTY".into(),
            to: to.into(),
            weight: None,
            order: None,
            value: None,
            unit: None,
            confidence: None,
        };
        let entity_set = EntitySet {
            entities: vec![
                Entity {
                    entity_type: "Alloy".into(),
                    name: "Inconel 718".into(),
                    properties: serde_json::json!({}),
                },
                // The defect shape: value and unit as TEXT inside the name.
                Entity {
                    entity_type: "Property".into(),
                    name: "1100 MPa".into(),
                    properties: serde_json::json!({}),
                },
                // The correct shape: name is the property NAME, value and
                // unit are typed fields.
                Entity {
                    entity_type: "Property".into(),
                    name: "density".into(),
                    properties: serde_json::json!({"value": 8.19, "unit": "g/cm3"}),
                },
            ],
            relationships: vec![rel("1100 MPa"), rel("density")],
        };
        let (report, plan) =
            validate_before_graph_write(&crate::ontologies::EmmoOntology, &entity_set);
        assert!(!report.passed, "the report records the packed name");
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.category == "measurement_in_name"),
            "{:?}",
            report.issues
        );
        let GraphWritePlan::Proceed {
            set,
            dropped,
            dropped_entities,
        } = plan
        else {
            panic!("a packed-name Property must be contained, not block the ingest");
        };
        assert!(
            !set.entities.iter().any(|e| e.name == "1100 MPa"),
            "a Property named after a measurement must never reach the write set"
        );
        assert_eq!(dropped_entities.len(), 1, "{dropped_entities:?}");
        assert!(
            dropped_entities[0].contains("1100 MPa"),
            "{}",
            dropped_entities[0]
        );
        // Its edge dangles and is dropped (and reported) with it…
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(dropped[0].contains("1100 MPa"), "{}", dropped[0]);
        // …while the typed sibling survives intact.
        assert!(set.entities.iter().any(|e| e.name == "density"));
        assert_eq!(set.relationships.len(), 1);
        assert_eq!(set.relationships[0].to, "density");
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
            batch_rows: None,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
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
                    value: None,
                    unit: None,
                    confidence: None,
                },
                Relationship {
                    from: "Steel".into(),
                    rel_type: "HAS_PROPERTY".into(),
                    to: "density".into(),
                    weight: None,
                    order: None,
                    value: None,
                    unit: None,
                    confidence: None,
                },
            ],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let emmo = crate::ontologies::EmmoOntology;
        let (update, dropped, _) = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local", None)
            .await
            .unwrap();
        assert!(dropped.is_empty(), "{dropped:?}");
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

    /// A real geometric collision crosses the production write boundary and
    /// remains advisory: both proposed nodes and their fact must still land.
    /// This fails if a future change turns the semantic report into an
    /// auto-merge, auto-drop, or write gate.
    #[tokio::test]
    async fn geometric_collision_never_mutates_the_pipeline_write_set() {
        use crate::{Entity, Relationship};

        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("semantic-write.db");
        let store = ProvenanceStore::open(&db_path).await.unwrap();
        store
            .write_classified_entity(
                "Ti ",
                ClassifiedNode {
                    entity_type: "Process",
                    storage_label: "Manufacturing",
                    class_iri: "https://w3id.org/emmo#Process",
                },
                None,
                "local",
            )
            .await
            .unwrap();
        store
            .store_precomputed_name_embeddings(
                &["Ti ".to_string()],
                &[vec![1.0, 0.0, 0.0]],
                "local",
                "test:pipeline-semantic-v1",
            )
            .await
            .unwrap();
        drop(store);

        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            batch_rows: None,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
        });
        let entity_set = EntitySet {
            entities: vec![
                Entity {
                    entity_type: "Alloy".into(),
                    name: "test alloy".into(),
                    properties: serde_json::json!({}),
                },
                Entity {
                    entity_type: "Element".into(),
                    name: "Ti".into(),
                    properties: serde_json::json!({}),
                },
            ],
            relationships: vec![Relationship {
                from: "test alloy".into(),
                rel_type: "CONTAINS".into(),
                to: "Ti".into(),
                weight: Some(1.0),
                order: None,
                value: None,
                unit: None,
                confidence: Some(0.9),
            }],
        };
        let source = DataSource {
            path: "/tmp/semantic.csv".into(),
            format: "csv".into(),
        };

        let (update, dropped, report) = pipeline
            .write_local_graph_with_semantic_backend(
                &crate::ontologies::EmmoOntology,
                &entity_set,
                &source,
                "local",
                None,
                Some(&DeterministicPipelineEmbed),
            )
            .await
            .unwrap();

        assert!(dropped.is_empty(), "semantic findings cannot create drops");
        assert_eq!(update.nodes_created, 2);
        assert_eq!(update.edges_created, 1);
        assert_eq!(
            report.near_duplicates.status,
            crate::semantic_validation::SemanticValidationStatus::Applied
        );
        assert!(
            report
                .near_duplicates
                .findings
                .iter()
                .any(|finding| finding.proposed_name == "Ti" && finding.colliding_name == "Ti "),
            "expected an auditable Ti/Ti-space collision: {report:?}"
        );

        let store = ProvenanceStore::open(&db_path).await.unwrap();
        let ti_hits = store.graph_search("Ti", "local", 10).await.unwrap();
        assert!(ti_hits.iter().any(|hit| hit.label == "Manufacturing"));
        assert!(ti_hits.iter().any(|hit| hit.label == "Element"));
        let facts = store
            .recall_with_context("test alloy", "local", 10)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "the reported triple must still be stored");
        assert_eq!(facts[0].object, "Ti");
    }

    /// An entity in no relationship still lands — as a standalone node under
    /// its DECLARED type — and `nodes_created` keeps counting what the store
    /// actually received, which now includes it. (History: these entities
    /// were first silently dropped while being counted, then honestly
    /// dropped with a warning; referential containment made the pipeline
    /// store everything valid instead.)
    #[tokio::test]
    async fn write_local_graph_stores_relationshipless_entities_as_typed_nodes() {
        use crate::{Entity, Relationship};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let db_path =
            std::env::temp_dir().join(format!("prism_pipeline_test_{}.db", uuid::Uuid::new_v4()));
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            batch_rows: None,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
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
                // Referenced by nothing — must land as a standalone node.
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
                value: None,
                unit: None,
                confidence: None,
            }],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let emmo = crate::ontologies::EmmoOntology;
        let (update, dropped, _) = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local", None)
            .await
            .unwrap();
        assert!(dropped.is_empty(), "{dropped:?}");

        assert_eq!(
            update.nodes_created, 3,
            "all three extracted entities must be stored (and counted)",
        );
        assert_eq!(update.edges_created, 1);

        // And prove the claim: the relationship-less entity really landed,
        // under the type the extraction DECLARED — never an invented one.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Nickel", "local", 10).await.unwrap();
        assert!(
            hits.iter()
                .any(|n| n.name == "Nickel" && n.label == "Element"),
            "the relationship-less entity is missing from the store (or mislabeled): {hits:?}",
        );

        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    /// Two entities sharing a NAME under DIFFERENT types reach the write loop
    /// unflagged: `classifications` is keyed by name alone (first wins) and
    /// graph_validation's duplicate check keys on (type, name). Both then
    /// resolve to the SAME storage key, and `props_json = COALESCE(excluded, …)`
    /// replaces the whole column rather than merging keys — so the loser's
    /// properties would overwrite the winner's with a set the extractor
    /// attributed to a different class.
    ///
    /// Adversarial review caught this as a regression introduced by the
    /// connected-entity properties fix; the original mutation test only ever
    /// used one entity per name, where the collision cannot occur.
    #[tokio::test]
    async fn a_same_name_different_type_entity_cannot_overwrite_the_winners_properties() {
        use crate::{Entity, Relationship};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let db_path =
            std::env::temp_dir().join(format!("prism_pipeline_test_{}.db", uuid::Uuid::new_v4()));
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            batch_rows: None,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
            on_progress: None,
            semantic_validation: Default::default(),
        });

        let entity_set = EntitySet {
            entities: vec![
                Entity {
                    entity_type: "Alloy".into(),
                    name: "Steel".into(),
                    properties: serde_json::json!({"composition": "Fe-C"}),
                },
                Entity {
                    entity_type: "Element".into(),
                    name: "Fe".into(),
                    properties: serde_json::json!({}),
                },
                // Same NAME, different TYPE, declared second.
                Entity {
                    entity_type: "Phase".into(),
                    name: "Steel".into(),
                    properties: serde_json::json!({"crystal_structure": "bcc"}),
                },
            ],
            relationships: vec![Relationship {
                from: "Steel".into(),
                rel_type: "CONTAINS".into(),
                to: "Fe".into(),
                weight: Some(0.98),
                order: None,
                value: None,
                unit: None,
                confidence: None,
            }],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let emmo = crate::ontologies::EmmoOntology;
        let (_, dropped, _) = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local", None)
            .await
            .unwrap();

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let props = store
            .entity_props_json("Steel", "local")
            .await
            .unwrap()
            .expect("the first-declared entity keeps its properties");
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(
            props["composition"].as_str(),
            Some("Fe-C"),
            "the winner's properties must survive the colliding write: {props}"
        );
        assert!(
            props.get("crystal_structure").is_none(),
            "a different class's properties must never land on this node: {props}"
        );
        assert!(
            dropped
                .iter()
                .any(|d| d.contains("Steel") && d.contains("collides")),
            "the dropped properties must be reported, not silently discarded: {dropped:?}"
        );

        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    /// An entity in a relationship kept NO extracted properties: the loop
    /// skipped it as already-written before its props reached the store, so
    /// the only entities that kept theirs were the ones connected to nothing.
    /// The subject of every fact in a real paper is exactly the entity whose
    /// properties were discarded.
    ///
    /// Drives the real `write_local_graph`, so restoring the early `continue`
    /// fails here.
    #[tokio::test]
    async fn a_connected_entity_keeps_its_extracted_properties() {
        use crate::{Entity, Relationship};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let db_path =
            std::env::temp_dir().join(format!("prism_pipeline_test_{}.db", uuid::Uuid::new_v4()));
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            batch_rows: None,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
        });

        let entity_set = EntitySet {
            entities: vec![
                Entity {
                    entity_type: "Alloy".into(),
                    name: "Steel".into(),
                    // Steel is a fact subject below — the case that lost props.
                    properties: serde_json::json!({"crystal_structure": "bcc"}),
                },
                Entity {
                    entity_type: "Element".into(),
                    name: "Fe".into(),
                    properties: serde_json::json!({}),
                },
            ],
            relationships: vec![Relationship {
                from: "Steel".into(),
                rel_type: "CONTAINS".into(),
                to: "Fe".into(),
                weight: Some(0.98),
                order: None,
                value: None,
                unit: None,
                confidence: None,
            }],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let emmo = crate::ontologies::EmmoOntology;
        let (_, dropped, _) = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local", None)
            .await
            .unwrap();
        assert!(dropped.is_empty(), "{dropped:?}");

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let props = store
            .entity_props_json("Steel", "local")
            .await
            .unwrap()
            .expect("a connected entity's extracted properties must be stored");
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(
            props["crystal_structure"].as_str(),
            Some("bcc"),
            "the property the extraction asserted must survive to the store"
        );

        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
    }

    /// THE one-vocabulary property, proven at the production write path and
    /// driven from the declaration rather than a frozen list of today's
    /// types: every node the tabular write stores lands under EXACTLY
    /// `storage_label(declared type)` of the active ontology — as a fact
    /// subject, as a fact object (every reachable arm: measurement, phase,
    /// contains, processing, and the generic fallback), and as a standalone
    /// containment-path node alike. Before this, the store kept a third,
    /// hardcoded vocabulary: every fact subject became `Matter` (live
    /// 2026-08-08: prompt said `Alloy`/`Material`, validator accepted them,
    /// store held `Matter` — a query for `Material` matched nothing ever
    /// stored), every generic-arm object became `Entity`, and a standalone
    /// entity kept its raw type — so ONE name could mint TWO nodes
    /// depending on whether its edges survived.
    #[tokio::test]
    async fn every_stored_label_is_the_declared_storage_label() {
        use crate::ontologies::Ontology;
        use crate::{Entity, Relationship};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let db_path =
            std::env::temp_dir().join(format!("prism_pipeline_test_{}.db", uuid::Uuid::new_v4()));
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            llm: None,
            batch_rows: None,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
        });

        let entity = |etype: &str, name: &str, props: serde_json::Value| Entity {
            entity_type: etype.into(),
            name: name.into(),
            properties: props,
        };
        let rel = |from: &str, rel_type: &str, to: &str| Relationship {
            from: from.into(),
            rel_type: rel_type.into(),
            to: to.into(),
            weight: None,
            order: None,
            value: None,
            unit: None,
            confidence: None,
        };

        let entity_set = EntitySet {
            entities: vec![
                // Fact subject (Alloy → Matter) across several arms.
                entity("Alloy", "Steel", serde_json::json!({})),
                // Fact OBJECT of contains AND fact SUBJECT of a measurement:
                // an Element must store as Element in both roles — this is
                // the assertion that dies if any arm hardcodes `Matter`
                // subjects again (the old behaviour split `Fe` into an
                // `Element` row and a `Matter` row).
                entity("Element", "Fe", serde_json::json!({})),
                // Raw paper spellings on purpose: the write path must
                // RESOLVE them to QUDT identifiers (asserted below), not
                // store them verbatim — this fixture once pinned verbatim
                // storage as expected behaviour (F10).
                entity(
                    "Property",
                    "density",
                    serde_json::json!({"value": 7.8, "unit": "g/cm3"}),
                ),
                entity(
                    "Property",
                    "melting point",
                    serde_json::json!({"value": 1811.0, "unit": "kelvin"}),
                ),
                entity("Phase", "BCC", serde_json::json!({})),
                // Process → Manufacturing: the store's one label for a step,
                // standalone or via PROCESSED_BY.
                entity("Process", "annealing", serde_json::json!({})),
                // Generic-arm subject and object (PART_OF has no typed arm):
                // declared labels, never `Matter`/`Entity`.
                entity("Paper", "Smith2020", serde_json::json!({})),
                entity("Paper", "Proceedings2020", serde_json::json!({})),
                // Standalone (containment-path) nodes: same mapping as the
                // fact writes — Material converges on Matter.
                entity("Dataset", "DS-1", serde_json::json!({})),
                entity("Material", "Ti-6Al-4V", serde_json::json!({})),
            ],
            relationships: vec![
                Relationship {
                    weight: Some(0.98),
                    ..rel("Steel", "CONTAINS", "Fe")
                },
                rel("Steel", "HAS_PROPERTY", "density"),
                rel("Fe", "HAS_PROPERTY", "melting point"),
                rel("Steel", "HAS_PHASE", "BCC"),
                Relationship {
                    order: Some(1),
                    ..rel("Steel", "PROCESSED_BY", "annealing")
                },
                rel("Smith2020", "PART_OF", "Proceedings2020"),
            ],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let emmo = crate::ontologies::EmmoOntology;
        let (update, dropped, _) = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local", None)
            .await
            .unwrap();
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(update.nodes_created, 10);
        assert_eq!(update.edges_created, 6);

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();

        // The raw fixture spellings landed RESOLVED — one unit vocabulary
        // in the store, never the paper spelling and never unit-less.
        for (property, expected_unit) in [
            ("density", "QUDT:GM-PER-CentiM3"),
            ("melting point", "QUDT:K"),
        ] {
            let recalled = store
                .recall_with_context(property, "local", 10)
                .await
                .unwrap();
            assert_eq!(recalled.len(), 1, "{property}: {recalled:?}");
            assert_eq!(
                recalled[0].unit.as_deref(),
                Some(expected_unit),
                "{property} must store the canonical QUDT identifier"
            );
        }

        for e in &entity_set.entities {
            let expected = emmo
                .storage_label(&e.entity_type)
                .expect("every declared type is storable");
            let hits = store.graph_search(&e.name, "local", 10).await.unwrap();
            let labels: Vec<&str> = hits
                .iter()
                .filter(|n| n.name == e.name)
                .map(|n| n.label.as_str())
                .collect();
            assert!(
                !labels.is_empty(),
                "'{}' ({}) was not stored at all",
                e.name,
                e.entity_type
            );
            assert!(
                labels.iter().all(|l| *l == expected),
                "'{}' declared {} must store under '{expected}' in EVERY role, got {labels:?}",
                e.name,
                e.entity_type,
            );
            let expected_iri = emmo
                .class_for_label(&e.entity_type)
                .expect("every declared test type resolves")
                .iri
                .as_str();
            assert!(
                hits.iter().filter(|n| n.name == e.name).all(|n| {
                    n.entity_type == e.entity_type && n.class_iri.as_deref() == Some(expected_iri)
                }),
                "'{}' must retain declared type '{}' and canonical IRI '{expected_iri}': {hits:?}",
                e.name,
                e.entity_type,
            );
        }

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
            extraction_decoding: None,
            graph_validation: None,
            semantic_validation: Vec::new(),
            graph: None,
            embeddings: None,
            dropped_relationships: Vec::new(),
            dropped_entities: Vec::new(),
            errors: Vec::new(),
            rows_processed: 10,
            batches: 1,
            batches_failed: 0,
            llm_usage: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        // Coverage reporting is always visible; unreported usage is absent,
        // never a fabricated zero.
        assert!(json.contains("rows_processed"));
        assert!(!json.contains("llm_usage"));
        // None fields should not appear in JSON.
        assert!(!json.contains("entities"));
        assert!(!json.contains("graph_validation"));
        assert!(!json.contains("semantic_validation"));
        assert!(!json.contains("graph"));
        assert!(!json.contains("embeddings"));
        // No drops ⇒ no dropped_relationships key (clean stays clean)…
        assert!(!json.contains("dropped_relationships"));
        assert!(!json.contains("dropped_entities"));
        // No errors ⇒ no errors key either (clean success stays clean)…
        assert!(!json.contains("errors"));
        // …but step failures MUST be visible in the JSON (the old shape hid
        // failed steps entirely — audit critical #2)…
        let failed = IngestResult {
            errors: vec!["local graph write failed: disk full".into()],
            ..result.clone()
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains("errors"));
        assert!(json.contains("disk full"));
        // …and so must contained drops — a drop the JSON hides is silent.
        let partial = IngestResult {
            dropped_relationships: vec!["A-[R]->B: undeclared endpoint(s): B".into()],
            ..result
        };
        let json = serde_json::to_string(&partial).unwrap();
        assert!(json.contains("dropped_relationships"));
        assert!(json.contains("undeclared endpoint"));
        // Dropped entities are the same contract: visible when non-empty.
        let partial = IngestResult {
            dropped_entities: vec!["entity 'X': type 'Gadget' has no storage label".into()],
            ..partial
        };
        let json = serde_json::to_string(&partial).unwrap();
        assert!(json.contains("dropped_entities"));
        assert!(json.contains("no storage label"));
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
            // One batch, no context probe: these tests pin single-call
            // behaviour; batch derivation has its own tests.
            batch_rows: Some(1000),
            mapping: None,
            provenance_db: Some(db_path),
            ontology: None,
            semantic_validation: SemanticValidationPolicy::default(),
            on_progress: None,
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

    // ── Referential containment, at PRODUCTION dispatch ────────────────
    //
    // These exercise `ingest_file` itself with deliberately NOVEL names and
    // relationship types: the property under test is containment of
    // dangling endpoints, not today's EMMO vocabulary.

    /// The containment deliverable: an extraction with dangling endpoints
    /// stores every entity and every well-formed relationship, drops ONLY
    /// the dangling edges, reports the drop in the result, and stays a
    /// clean run (`errors` empty ⇒ exit 0) — while the missing endpoints
    /// are NEVER invented into the store.
    #[tokio::test]
    async fn dangling_relationships_are_dropped_and_the_valid_remainder_is_stored() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Zorblatt-9", "properties": {}},
                {"type": "Property", "name": "squishiness", "properties": {}}
            ],
            "relationships": [
                {"from": "Zorblatt-9", "rel": "HAS_PROPERTY", "to": "squishiness"},
                {"from": "Zorblatt-9", "rel": "GLUED_TO", "to": "Phantomium"},
                {"from": "Ghostium", "rel": "GLUED_TO", "to": "Phantomium"}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // A contained drop is a partial SUCCESS: no step failure, exit 0.
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        // The report stays an honest record of the raw extraction.
        let report = result.graph_validation.expect("validation ran");
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.category == "orphan_rel"));

        // The drop is REPORTED: which relationships, and which endpoint(s)
        // were never declared.
        assert_eq!(
            result.dropped_relationships.len(),
            2,
            "{:?}",
            result.dropped_relationships
        );
        let drops = result.dropped_relationships.join("\n");
        assert!(drops.contains("Phantomium"), "{drops}");
        assert!(drops.contains("Ghostium"), "{drops}");
        assert!(
            !drops.contains("HAS_PROPERTY"),
            "the well-formed relationship was reported dropped: {drops}"
        );

        // Everything valid was stored: both entities, the one good edge.
        let graph = result.graph.expect("the valid remainder must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (2, 1));

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        for name in ["Zorblatt-9", "squishiness"] {
            let hits = store.graph_search(name, "local", 10).await.unwrap();
            assert!(hits.iter().any(|n| n.name == name), "{name} not stored");
        }
        // Dropping loses a claim; inventing corrupts the graph. Neither
        // undeclared endpoint may exist as a node…
        for phantom in ["Phantomium", "Ghostium"] {
            let hits = store.graph_search(phantom, "local", 10).await.unwrap();
            assert!(
                hits.is_empty(),
                "undeclared endpoint '{phantom}' was auto-declared into the store: {hits:?}"
            );
        }
        // …and the dangling edge may not exist either, while the good one does.
        let tr = store
            .get_neighbors("Zorblatt-9", None, "local", 10)
            .await
            .unwrap();
        assert!(tr.edges.iter().any(|e| e.rel_type == "HAS_PROPERTY"));
        assert!(
            !tr.edges.iter().any(|e| e.rel_type == "GLUED_TO"),
            "a dangling relationship reached the store: {:?}",
            tr.edges
        );
        server.verify().await;
    }

    /// THE unit rule at PRODUCTION dispatch on the tabular path (F10),
    /// through `ingest_file` itself: a numeric property whose unit is
    /// missing or unresolvable is dropped WHOLE and REPORTED in
    /// `dropped_relationships` (the surface the CLI prints), while a raw
    /// resolvable spelling lands RESOLVED to its QUDT identifier — never
    /// verbatim, never unit-less. Before this, the tabular path passed raw
    /// unit strings through and the store filled missing ones with `""`,
    /// re-opening the 880 GPa vs 880 MPa hazard the text path had closed.
    #[tokio::test]
    async fn tabular_numeric_facts_resolve_units_or_are_dropped_and_reported() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Ti-6Al-4V", "properties": {}},
                // Numeric value, NO unit — the CSV-shaped `{"value": 880}`.
                {"type": "Property", "name": "UTS", "properties": {"value": 880}},
                // Numeric value, unresolvable unit.
                {"type": "Property", "name": "hardness",
                 "properties": {"value": 349, "unit": "banana"}},
                // Numeric value, raw resolvable spelling.
                {"type": "Property", "name": "density",
                 "properties": {"value": 4.43, "unit": "g/cm3"}}
            ],
            "relationships": [
                {"from": "Ti-6Al-4V", "rel": "HAS_PROPERTY", "to": "UTS"},
                {"from": "Ti-6Al-4V", "rel": "HAS_PROPERTY", "to": "hardness"},
                {"from": "Ti-6Al-4V", "rel": "HAS_PROPERTY", "to": "density"}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,uts_mpa,hardness,density\nTi-6Al-4V,880,349,4.43\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // A contained drop is a partial SUCCESS: no step failure, exit 0…
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        // …and the drops are REPORTED, naming fact, value and cause.
        assert_eq!(
            result.dropped_relationships.len(),
            2,
            "{:?}",
            result.dropped_relationships
        );
        let drops = result.dropped_relationships.join("\n");
        assert!(
            drops.contains("UTS") && drops.contains("880") && drops.contains("no unit at all"),
            "{drops}"
        );
        assert!(
            drops.contains("hardness") && drops.contains("banana"),
            "{drops}"
        );

        // The count matches what the store received: 4 nodes (the dropped
        // facts' endpoints still exist as typed nodes), 1 surviving edge.
        let graph = result.graph.expect("the valid remainder must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (4, 1));

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        // The resolvable measurement landed with the CANONICAL identifier.
        let density = store
            .recall_with_context("density", "local", 10)
            .await
            .unwrap();
        assert_eq!(density.len(), 1, "{density:?}");
        assert_eq!(density[0].value, Some(4.43));
        assert_eq!(
            density[0].unit.as_deref(),
            Some("QUDT:GM-PER-CentiM3"),
            "the stored unit must be the resolved QUDT identifier, not the raw spelling"
        );
        // THE rule: neither refused number is anywhere in the store — not
        // with a raw unit, not with an empty one.
        for refused in ["UTS", "hardness"] {
            let facts = store
                .recall_with_context(refused, "local", 10)
                .await
                .unwrap();
            assert!(
                facts.is_empty(),
                "a numeric value without a resolvable unit must never be stored: {facts:?}"
            );
            // The entity itself still exists as a typed node — the claim
            // was refused, not the entity.
            let hits = store.graph_search(refused, "local", 10).await.unwrap();
            assert!(
                hits.iter()
                    .any(|n| n.name == refused && n.label == "Property"),
                "the dropped fact's endpoint must still land as a typed node: {hits:?}"
            );
        }
        server.verify().await;
    }

    /// EVERY relationship dangling is still a partial success: the declared
    /// entities land (that is what "stores everything valid" means when
    /// nothing else is), zero edges, the drop is reported, exit stays 0.
    /// (Zero ENTITIES remains a hard failure — pinned by
    /// `rows_with_zero_entities_found_is_not_a_refusal` above.)
    #[tokio::test]
    async fn all_relationships_dropped_still_stores_the_entities() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Klaxonite", "properties": {"note": "novel"}},
                {"type": "Phase", "name": "omega-weird", "properties": {}}
            ],
            "relationships": [
                {"from": "Klaxonite", "rel": "BONDED_WITH", "to": "Unseen-1"}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.dropped_relationships.len(),
            1,
            "{:?}",
            result.dropped_relationships
        );
        assert!(result.dropped_relationships[0].contains("Unseen-1"));

        let graph = result.graph.expect("entities alone are still a write");
        assert_eq!((graph.nodes_created, graph.edges_created), (2, 0));

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        // Stored under the ontology's DECLARED storage labels — the same
        // ones a fact write would have used (EMMO maps Alloy → Matter), so
        // an entity keeps ONE identity whether its edges survived or not.
        let hits = store.graph_search("Klaxonite", "local", 10).await.unwrap();
        assert!(
            hits.iter()
                .any(|n| n.name == "Klaxonite" && n.label == "Matter"),
            "{hits:?}"
        );
        let hits = store
            .graph_search("omega-weird", "local", 10)
            .await
            .unwrap();
        assert!(
            hits.iter()
                .any(|n| n.name == "omega-weird" && n.label == "Phase"),
            "{hits:?}"
        );
        assert!(
            store
                .graph_search("Unseen-1", "local", 10)
                .await
                .unwrap()
                .is_empty(),
            "the undeclared endpoint was invented into the store"
        );
    }

    /// An entity whose type the active ontology maps to no storage label is
    /// DROPPED AND REPORTED — never stored under an invented or passed-
    /// through label the declaration does not produce (the pre-fix
    /// behaviour stored it verbatim: `unknown_type` is only a Warning, so
    /// an undeclared vocabulary sailed into the store). Relationships that
    /// referenced it dangle and are dropped (and reported) with it; the
    /// declared remainder still lands; exit stays 0.
    #[tokio::test]
    async fn unmapped_entity_types_are_dropped_and_reported_never_stored() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Bloopium", "properties": {}},
                {"type": "Gadget", "name": "Sprocketium", "properties": {}}
            ],
            "relationships": [
                {"from": "Bloopium", "rel": "CONTAINS", "to": "Sprocketium", "weight": 0.5}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // A contained drop is a partial SUCCESS: no step failure, exit 0 —
        // and the raw validation record honestly shows only warnings
        // (unknown_type), which is exactly why `passed` alone could never
        // gate this.
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let report = result.graph_validation.expect("validation ran");
        assert!(report.passed, "unknown_type is Warning severity");
        assert!(report.issues.iter().any(|i| i.category == "unknown_type"));

        // Both drops are REPORTED: the unmapped entity, and the
        // relationship that dangled once it was gone.
        assert_eq!(
            result.dropped_entities.len(),
            1,
            "{:?}",
            result.dropped_entities
        );
        let dropped = &result.dropped_entities[0];
        assert!(dropped.contains("Sprocketium"), "{dropped}");
        assert!(dropped.contains("Gadget"), "{dropped}");
        assert!(dropped.contains("no storage label"), "{dropped}");
        assert_eq!(
            result.dropped_relationships.len(),
            1,
            "{:?}",
            result.dropped_relationships
        );
        assert!(result.dropped_relationships[0].contains("Sprocketium"));

        // The declared remainder was stored — under its declared storage
        // label — and the unmapped entity reached the store under NO label.
        let graph = result
            .graph
            .expect("the declared remainder must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (1, 0));
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Bloopium", "local", 10).await.unwrap();
        assert!(
            hits.iter()
                .any(|n| n.name == "Bloopium" && n.label == "Matter"),
            "{hits:?}"
        );
        assert!(
            store
                .graph_search("Sprocketium", "local", 10)
                .await
                .unwrap()
                .is_empty(),
            "an entity of an undeclared type reached the store"
        );
    }

    // ── Name normalisation at the extraction boundary ──────────────────
    //
    // The live defect (2026-08-08, qwen2.5:3b over a 5-row alloys CSV): the
    // model declared elements bare (`Ti`) and referenced them QUOTED
    // (`"Ti"`) in relationships. Exact-match referential integrity then
    // dropped ALL 15 edges — 11 nodes, ZERO edges, a disconnected graph —
    // and material names landed in the store with literal quote characters
    // (`"316L"`, `"Ti-6Al-4V"`, `"LPBF"`). These tests drive the REAL
    // `ingest_file` dispatch through a mock LLM emitting exactly that shape.

    /// Entity names and relationship endpoints are normalised by the SAME
    /// function at the extraction boundary, so quote-mismatched spellings of
    /// the same declared thing agree by construction: every edge survives,
    /// and no stored name carries a surrounding quote character.
    #[tokio::test]
    async fn quoted_names_and_endpoints_agree_by_construction_and_edges_survive() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                // Declared bare — referenced ASCII-quoted below.
                {"type": "Alloy", "name": "Zorblattium", "properties": {}},
                // Declared ASCII-quoted — referenced bare below.
                {"type": "Element", "name": "\"Quotium\"", "properties": {}},
                // Declared curly-quoted — referenced single-quoted below.
                {"type": "Phase", "name": "\u{201C}omega-quoted\u{201D}", "properties": {}}
            ],
            "relationships": [
                {"from": "\"Zorblattium\"", "rel": "CONTAINS", "to": "Quotium", "weight": 0.5},
                {"from": "Zorblattium", "rel": "HAS_PHASE", "to": "'omega-quoted'"}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // Nothing dangles, nothing is dropped, the run is clean.
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            result.dropped_relationships.is_empty(),
            "quote-mismatched endpoints still dangle: {:?}",
            result.dropped_relationships
        );
        assert!(
            result.dropped_entities.is_empty(),
            "{:?}",
            result.dropped_entities
        );

        // Both edges survive — the graph is CONNECTED, not nodes-only.
        let graph = result.graph.expect("the normalised set must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (3, 2));

        // Stored names are the bare spellings, with no quote characters.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        const QUOTES: &[char] = &['"', '\'', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}'];
        for name in ["Zorblattium", "Quotium", "omega-quoted"] {
            let hits = store.graph_search(name, "local", 10).await.unwrap();
            assert!(
                hits.iter().any(|n| n.name == name),
                "{name} not stored bare: {hits:?}"
            );
            assert!(
                hits.iter().all(|n| !n.name.contains(QUOTES)),
                "a stored name kept its quotes: {hits:?}"
            );
        }
        // Both edges are attached to the bare-named node — endpoints agree.
        let tr = store
            .get_neighbors("Zorblattium", None, "local", 10)
            .await
            .unwrap();
        for target in ["Quotium", "omega-quoted"] {
            assert!(
                tr.edges.iter().any(|e| e.target == target),
                "edge to {target} missing: {:?}",
                tr.edges
            );
        }
    }

    /// Only BALANCED SURROUNDING quotes come off. An interior quote is data
    /// — `6" nozzle extrusion` is a six-inch nozzle, not a quoting artefact
    /// — and must reach the store intact on the entity AND on the endpoint
    /// referencing it.
    #[tokio::test]
    async fn interior_quotes_are_data_and_reach_the_store_intact() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Pipium", "properties": {}},
                {"type": "Process", "name": "6\" nozzle extrusion", "properties": {}}
            ],
            "relationships": [
                {"from": "Pipium", "rel": "PROCESSED_BY", "to": "6\" nozzle extrusion", "order": 1}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert!(
            result.dropped_relationships.is_empty(),
            "{:?}",
            result.dropped_relationships
        );
        let graph = result.graph.expect("the set must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (2, 1));

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("nozzle", "local", 10).await.unwrap();
        assert!(
            hits.iter().any(|n| n.name == "6\" nozzle extrusion"),
            "the interior quote was stripped or the name mangled: {hits:?}"
        );
    }

    /// A name that is NOTHING BUT quotes (`""`) normalises to empty — a
    /// rejection, not an empty name: never stored, dropped and REPORTED via
    /// `dropped_entities`, while the valid remainder still lands and any
    /// relationship referencing the rejected entity is dropped with it.
    /// Nothing is invented to fill the hole.
    #[tokio::test]
    async fn empty_after_normalisation_names_are_dropped_and_reported() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "\"\"", "properties": {}},
                {"type": "Alloy", "name": "Solidium", "properties": {}}
            ],
            "relationships": [
                {"from": "Solidium", "rel": "CONTAINS", "to": "\"\"", "weight": 0.5}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // A contained drop is a partial SUCCESS — not a blocked write.
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.dropped_entities.len(),
            1,
            "{:?}",
            result.dropped_entities
        );
        assert!(
            result.dropped_entities[0].contains("empty name after normalisation"),
            "{}",
            result.dropped_entities[0]
        );
        assert_eq!(
            result.dropped_relationships.len(),
            1,
            "{:?}",
            result.dropped_relationships
        );

        // Only the real entity landed; no empty-named node, no edge to one.
        let graph = result.graph.expect("the valid remainder must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (1, 0));
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Solidium", "local", 10).await.unwrap();
        assert!(hits.iter().any(|n| n.name == "Solidium"), "{hits:?}");
        let tr = store
            .get_neighbors("Solidium", None, "local", 10)
            .await
            .unwrap();
        assert!(
            tr.edges.is_empty(),
            "an edge to a rejected entity reached the store: {:?}",
            tr.edges
        );
    }

    /// Containment must not blanket-weaken validation: an error a drop
    /// cannot repair (here ZERO entities extracted, while relationships
    /// still reference a world that was never declared) still blocks the
    /// ENTIRE write — nothing stored, nothing reported as "dropped and the
    /// rest written", store never opened. (An empty entity NAME was this
    /// test's original exemplar; since name normalisation it is the
    /// contained `empty_name` drop instead — pinned by
    /// `empty_after_normalisation_names_are_dropped_and_reported`.)
    #[tokio::test]
    async fn non_orphan_errors_still_block_the_whole_write() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [],
            "relationships": [
                {"from": "Bloopium", "rel": "MELDS_WITH", "to": "Nowhereium"}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("No entities were extracted"),
            "{}",
            result.errors[0]
        );
        assert!(result.graph.is_none(), "a blocked run must write nothing");
        assert!(
            result.dropped_relationships.is_empty(),
            "a blocked run drops nothing — nothing was written behind it: {:?}",
            result.dropped_relationships
        );
        assert!(
            !db_path.exists(),
            "a blocked ingest opened/created the provenance store"
        );
    }

    /// The invariant the validator enforces is STATED in the prompt that
    /// actually reaches the model — asserted on the request body the mock
    /// LLM received, for the shipped default (EMMO's legacy override, whose
    /// byte-identity was deliberately broken for exactly this line). The
    /// fragment is hardcoded so a reworded-away rule dies too. The trait
    /// default is covered in `ontologies::tests`.
    #[tokio::test]
    async fn the_prompt_sent_to_the_model_states_the_referential_rule() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(emmo_extraction()).await;
        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,element\nSteel,Fe\n");
        let pipeline = pipeline_against(server.uri(), scratch.db_path());

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 1);
        let sent = String::from_utf8_lossy(&requests[0].body).into_owned();
        assert!(
            sent.contains("MUST also appear as an entity in"),
            "the extraction prompt no longer states the referential-integrity \
             rule; the validator will refuse what the model was told to emit:\n{sent}"
        );
    }

    // ── Constrained decoding, at PRODUCTION dispatch ───────────────────
    //
    // These exercise `ingest_file` itself and assert on the REQUEST BODY the
    // mock endpoint received: they die if the pipeline stops sending the
    // schema, derives it from anything but the ACTIVE ontology, stops
    // sending the determinism knobs, or makes the unsupported-endpoint
    // fallback silent.

    /// The request that actually reaches the model carries (1) the
    /// ontology-derived JSON schema under `response_format: json_schema`,
    /// (2) `temperature: 0`, and (3) the recorded extraction seed — and the
    /// result's decoding trace says the constraint was enforced.
    #[tokio::test]
    async fn extraction_request_carries_the_ontology_schema_and_determinism_knobs() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(emmo_extraction()).await;
        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,element\nSteel,Fe\n");
        let pipeline = pipeline_against(server.uri(), scratch.db_path());

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("request body is JSON");
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["seed"], prism_llm::EXTRACTION_SEED);
        let schema = &body["response_format"]["json_schema"]["schema"];
        // EMMO declares quantitative classes, so the entity items on the
        // WIRE are per-type variants: the quantitative one (Property,
        // value/unit REQUIRED — the field-use half the enum lock alone
        // never gave) and the open one (everything else).
        let quant_enum = schema
            .pointer("/properties/entities/items/oneOf/0/properties/type/enum")
            .and_then(|v| v.as_array())
            .expect("quantitative entity type enum present");
        assert!(quant_enum.iter().any(|v| v == "Property"), "{quant_enum:?}");
        assert_eq!(
            schema.pointer("/properties/entities/items/oneOf/0/properties/properties/required"),
            Some(&serde_json::json!(["value", "unit"])),
            "the request no longer REQUIRES typed value/unit on Property \
             entities — the model may again pack the measurement into the name"
        );
        let entity_enum = schema
            .pointer("/properties/entities/items/oneOf/1/properties/type/enum")
            .and_then(|v| v.as_array())
            .expect("entity type enum present");
        assert!(entity_enum.iter().any(|v| v == "Alloy"), "{entity_enum:?}");
        let rel_enum = schema
            .pointer("/properties/relationships/items/oneOf/2/properties/rel/enum")
            .and_then(|v| v.as_array())
            .expect("relationship enum present");
        assert!(rel_enum.iter().any(|v| v == "CONTAINS"), "{rel_enum:?}");
        // The per-edge coupling reaches the wire: a measured edge must
        // state value AND unit (with `unit` optional the live model emitted
        // every value and no units — run 2), and it must offer no unitless
        // numeric slot (with `weight` available the model put every number
        // there — run 3).
        assert_eq!(
            schema.pointer("/properties/relationships/items/oneOf/0/required"),
            Some(&serde_json::json!(["from", "rel", "to", "value", "unit"])),
        );
        assert!(
            schema
                .pointer("/properties/relationships/items/oneOf/0/properties/weight")
                .is_none(),
        );
        let unit_enum = schema
            .pointer(
                "/properties/entities/items/oneOf/0\
                 /properties/properties/properties/unit/anyOf/0/enum",
            )
            .and_then(|v| v.as_array())
            .expect("unit enum present");
        assert!(
            unit_enum.iter().any(|v| v == "QUDT:MegaPA"),
            "{unit_enum:?}"
        );
        assert!(
            !unit_enum.iter().any(|v| v == "MPa"),
            "a bare unit spelling is legal to the schema: {unit_enum:?}"
        );

        let trace = result
            .extraction_decoding
            .expect("an extraction run must carry its decoding trace");
        assert_eq!(trace.mode, prism_llm::JsonDecodingMode::JsonSchema);
        assert_eq!(trace.degraded, None);
        assert_eq!(trace.seed, Some(prism_llm::EXTRACTION_SEED));
        assert_eq!(trace.temperature, Some(0.0));
    }

    /// The schema is derived from the ACTIVE ontology, not a frozen EMMO
    /// list: under a runtime-registered chemistry ontology the request's
    /// enums carry ITS vocabulary and none of EMMO's.
    #[tokio::test]
    async fn extraction_schema_follows_the_active_ontology_not_a_hardcoded_list() {
        use std::sync::Arc;

        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        crate::ontologies::register_ontology(Arc::new(ChemOntology {
            id: "chem-schema-req",
        }))
        .expect("a novel ontology must register");

        let server = mock_llm(chem_extraction()).await;
        let scratch = RefusalScratch::new();
        let csv = scratch.csv("molecule,reacts_with\nH2O,O3\n");
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            ontology: Some("chem-schema-req".into()),
            ..pipeline_against(server.uri(), scratch.db_path()).config
        });

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("request body is JSON");
        let schema = &body["response_format"]["json_schema"]["schema"];
        let entity_enum = schema
            .pointer("/properties/entities/items/properties/type/enum")
            .and_then(|v| v.as_array())
            .expect("entity type enum present");
        assert!(
            entity_enum.iter().any(|v| v == "Molecule"),
            "{entity_enum:?}"
        );
        assert!(
            !entity_enum.iter().any(|v| v == "Alloy"),
            "EMMO vocabulary leaked into a chem schema: {entity_enum:?}"
        );
        let rel_enum = schema
            .pointer("/properties/relationships/items/properties/rel/enum")
            .and_then(|v| v.as_array())
            .expect("relationship enum present");
        assert!(rel_enum.iter().any(|v| v == "REACTS_WITH"), "{rel_enum:?}");
        assert!(
            !rel_enum.iter().any(|v| v == "CONTAINS"),
            "EMMO relationships leaked into a chem schema: {rel_enum:?}"
        );
        assert_eq!(
            body["response_format"]["json_schema"]["name"],
            "chem-schema-req_tabular_extraction"
        );
    }

    /// Honest degradation: an endpoint that REJECTS `json_schema` gets the
    /// prompt-only fallback (same seed, same temperature) and the result
    /// SAYS SO — the trace carries the endpoint's rejection, the run stays
    /// a success, and the facts land. A silent fallback dies here.
    #[tokio::test]
    async fn schema_rejection_degrades_honestly_and_is_reported() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = MockServer::start().await;
        // The schema attempt: rejected the way servers without the
        // capability actually reject it (400 naming response_format).
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_string_contains("json_schema"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "{\"error\": \"response_format type json_schema is not supported\"}",
            ))
            .expect(1)
            .mount(&server)
            .await;
        // The fallback: json_object accepted.
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_string_contains("json_object"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"content": emmo_extraction().to_string()},
                    "finish_reason": "stop"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,element\nSteel,Fe\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // Degradation is a reported downgrade, not a failure…
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let trace = result
            .extraction_decoding
            .expect("a degraded run must still carry its trace");
        assert_eq!(trace.mode, prism_llm::JsonDecodingMode::JsonObject);
        let degraded = trace
            .degraded
            .expect("the fallback must NEVER be silent — the trace must say why");
        assert!(degraded.contains("rejected"), "{degraded}");
        assert!(degraded.contains("json_schema"), "{degraded}");
        // …the determinism knobs still rode the fallback request…
        assert_eq!(trace.seed, Some(prism_llm::EXTRACTION_SEED));
        assert_eq!(trace.temperature, Some(0.0));
        // …and the facts still landed.
        let graph = result.graph.expect("fallback extraction must store");
        assert_eq!((graph.nodes_created, graph.edges_created), (2, 1));
        // Both requests really happened, in the declared shapes.
        server.verify().await;

        let requests = server.received_requests().await.expect("recording on");
        let fallback: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(fallback["response_format"]["type"], "json_object");
        assert_eq!(fallback["seed"], prism_llm::EXTRACTION_SEED);
        assert_eq!(fallback["temperature"], 0.0);
    }

    /// The reasoning kill-switch is sent EXACTLY when requested: with
    /// `no_think` the request carries `chat_template_kwargs:
    /// {"enable_thinking": false}` and the trace records it; without it the
    /// field is ABSENT (OpenAI rejects unknown request fields with 400, so
    /// sending it unconditionally would break every OpenAI extraction).
    #[tokio::test]
    async fn no_think_kwarg_is_sent_only_when_requested() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"content": "{\"entities\": [], \"relationships\": []}"},
                    "finish_reason": "stop"
                }]
            })))
            .expect(2)
            .mount(&server)
            .await;

        let client = prism_llm::LlmClient::new(crate::LlmConfig {
            base_url: server.uri(),
            model: "test-model".into(),
            ..crate::LlmConfig::default()
        });
        let schema =
            crate::extraction_schema::extraction_json_schema(&crate::ontologies::EmmoOntology);

        let plain = client
            .generate_json_with_schema("extract", &schema, prism_llm::EXTRACTION_SEED, false)
            .await
            .unwrap();
        assert!(!plain.trace.no_think);
        let switched = client
            .generate_json_with_schema("extract", &schema, prism_llm::EXTRACTION_SEED, true)
            .await
            .unwrap();
        assert!(switched.trace.no_think, "the trace must record the switch");

        let requests = server.received_requests().await.expect("recording on");
        assert_eq!(requests.len(), 2);
        let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            first.get("chat_template_kwargs").is_none(),
            "no_think=false must not leak a vendor kwarg OpenAI would 400 on"
        );
        let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(
            second.pointer("/chat_template_kwargs/enable_thinking"),
            Some(&serde_json::Value::Bool(false)),
            "no_think=true must actually disable thinking on the wire"
        );
        server.verify().await;
    }

    /// The fallback gate must not over-fire: a 400 that has nothing to do
    /// with the schema (wrong model) propagates as an extraction FAILURE —
    /// no second request, no fake "degraded but stored" success.
    #[tokio::test]
    async fn unrelated_request_failures_do_not_masquerade_as_schema_degradation() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string("{\"error\": \"model 'wrong-model' not found\"}"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("alloy,element\nSteel,Fe\n");
        let pipeline = pipeline_against(server.uri(), scratch.db_path());

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("LLM extraction failed"),
            "{}",
            result.errors[0]
        );
        assert!(
            result.extraction_decoding.is_none(),
            "a failed extraction must not claim a decoding mode"
        );
        assert!(result.graph.is_none());
        server.verify().await;
    }

    /// The check constrained decoding cannot do, contained at production
    /// dispatch: a density tagged with a pressure unit (grammar-legal,
    /// false) is dropped and REPORTED, the rest of the document is stored,
    /// and the poisoned claim never reaches the store.
    #[tokio::test]
    async fn a_density_with_a_pressure_unit_is_dropped_and_reported_not_stored() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Inconel 718", "properties": {}},
                // The owner's live case: 8.19 g/cm³ tagged as gigapascals.
                {"type": "Property", "name": "density_g_cm3",
                 "properties": {"value": 8.19, "unit": "QUDT:GigaPA"}},
                {"type": "Property", "name": "yield_strength_mpa",
                 "properties": {"value": 1100.0, "unit": "QUDT:MegaPA"}}
            ],
            "relationships": [
                {"from": "Inconel 718", "rel": "HAS_PROPERTY", "to": "density_g_cm3"},
                {"from": "Inconel 718", "rel": "HAS_PROPERTY", "to": "yield_strength_mpa"}
            ]
        }))
        .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("material,yield_strength_mpa,density_g_cm3\nInconel 718,1100,8.19\n");
        let db_path = scratch.db_path();
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // Contained, not fatal: the run succeeds and says what it dropped.
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let report = result.graph_validation.expect("validation ran");
        assert!(!report.passed, "the honest report keeps the contradiction");
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.category == "unit_kind_mismatch"),
            "{:?}",
            report.issues
        );
        assert_eq!(
            result.dropped_entities.len(),
            1,
            "{:?}",
            result.dropped_entities
        );
        assert!(
            result.dropped_entities[0].contains("QUDT:GigaPA"),
            "{}",
            result.dropped_entities[0]
        );
        assert_eq!(
            result.dropped_relationships.len(),
            1,
            "the edge to the poisoned property dangles and is dropped: {:?}",
            result.dropped_relationships
        );

        // The valid remainder was stored; the falsehood was not.
        let graph = result.graph.expect("the valid remainder must be written");
        assert_eq!((graph.nodes_created, graph.edges_created), (2, 1));
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        assert!(
            !store
                .graph_search("yield_strength_mpa", "local", 10)
                .await
                .unwrap()
                .is_empty(),
            "the clean property must be stored"
        );
        assert!(
            store
                .graph_search("density_g_cm3", "local", 10)
                .await
                .unwrap()
                .is_empty(),
            "a density under a pressure unit reached the store"
        );
    }

    // ── Whole-dataset batching, at PRODUCTION dispatch ─────────────────
    //
    // The defect this replaces: `max_sample_rows: 10` was hardcoded in two
    // places, so TEN rows of any dataset were extracted and the rest were
    // parsed into memory and thrown away — reported nowhere. These tests
    // drive `ingest_file` itself and assert on the requests that actually
    // reached the (mock) model and on the facts that actually reached the
    // store.

    /// A CSV body with `n` data rows, each carrying a unique marker.
    fn csv_rows(n: usize, pad: usize) -> String {
        let mut body = String::from("alloy,uts_mpa\n");
        for i in 1..=n {
            body.push_str(&format!(
                "alloy_row_{i:02}{},{}\n",
                "x".repeat(pad),
                900 + i
            ));
        }
        body
    }

    /// The bodies of every extraction POST the mock model received.
    async fn extraction_request_bodies(server: &wiremock::MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .expect("recording on")
            .iter()
            .filter(|r| r.url.path().ends_with("/chat/completions"))
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect()
    }

    /// EVERY row reaches the model — there is no 10-row sample. Restoring
    /// the old fixed sample (`extract_sample_rows(&df, 10)` in the pipeline,
    /// or the constructor-side `max_sample_rows` truncation) kills this:
    /// rows 11-25 would never appear in any request, and `rows_processed`
    /// would misreport the coverage.
    #[tokio::test]
    async fn every_row_reaches_the_model_not_a_ten_row_sample() {
        // These tests assert EXACT row/batch counts read through the
        // process-wide connector registry; a sibling test replaces the csv
        // connector with a 1-row fake inside its lock window, so exact-count
        // tests must serialize behind the SAME shared lock.
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(emmo_extraction()).await;
        let scratch = RefusalScratch::new();
        let csv = scratch.csv(&csv_rows(25, 0));
        // batch_rows: None ⇒ the derived path. The mock serves no /props, so
        // the probe honestly reports UNKNOWN and the documented fallback
        // budget applies — far larger than 25 short rows, hence ONE batch.
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            batch_rows: None,
            ..pipeline_against(server.uri(), scratch.db_path()).config
        });

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.row_count, 25);
        assert_eq!(
            result.rows_processed, 25,
            "coverage must be reported over EVERY row"
        );
        assert_eq!((result.batches, result.batches_failed), (1, 0));

        let bodies = extraction_request_bodies(&server).await;
        assert_eq!(bodies.len(), 1, "25 short rows fit one derived batch");
        for i in 1..=25 {
            assert!(
                bodies[0].contains(&format!("alloy_row_{i:02}")),
                "row {i} of 25 never reached the model — the fixed sample is back"
            );
        }
    }

    /// Batch size DERIVES from the context window the serving runtime
    /// reports (`/props` → n_ctx), not from a number someone picked: a
    /// 2048-token window must split 30 fat rows into several batches, and
    /// the union of all batches must still cover every row. Also pins the
    /// observability contract: the plan and every batch emit progress lines.
    #[tokio::test]
    async fn batch_size_derives_from_the_reported_context_window() {
        // These tests assert EXACT row/batch counts read through the
        // process-wide connector registry; a sibling test replaces the csv
        // connector with a 1-row fake inside its lock window, so exact-count
        // tests must serialize behind the SAME shared lock.
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(emmo_extraction()).await;
        Mock::given(method("GET"))
            .and(path("/props"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "default_generation_settings": { "n_ctx": 2048 }
            })))
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv(&csv_rows(30, 120)); // ~140 bytes per row
        let progress: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let sink = std::sync::Arc::clone(&progress);
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            batch_rows: None,
            on_progress: Some(std::sync::Arc::new(move |line: &str| {
                sink.lock().unwrap().push(line.to_string());
            })),
            ..pipeline_against(server.uri(), scratch.db_path()).config
        });

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.rows_processed, 30);
        assert!(
            result.batches >= 2,
            "a 2048-token window cannot hold 30 fat rows in one batch \
             (batches = {}); the probed window did not drive the plan",
            result.batches
        );

        let bodies = extraction_request_bodies(&server).await;
        assert_eq!(bodies.len(), result.batches);
        for i in 1..=30 {
            let marker = format!("alloy_row_{i:02}");
            assert!(
                bodies.iter().any(|b| b.contains(&marker)),
                "row {i} of 30 fell between batches"
            );
        }

        // Observable while it runs: the plan names the probed window, and
        // every batch announces itself.
        let lines = progress.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("extraction plan") && l.contains("2048-token context window")),
            "no plan line naming the derived budget: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .filter(|l| l.contains(": extracting rows"))
                .count()
                >= 2,
            "per-batch progress lines missing: {lines:?}"
        );
    }

    /// Two batches of ONE document are not two independent sources. Both
    /// batches assert the same fact; the store must hold ONE evidence
    /// contribution for it and the aggregate confidence must stay at the
    /// single-sighting value — corroboration is keyed on the origin source
    /// (`origin_source_key`), which every batch of this run shares. Making
    /// batches corroborate each other (per-batch activity sources, a
    /// per-batch `origin_source_id`) inflates confidence across the whole
    /// corpus and kills this test.
    #[tokio::test]
    async fn two_batches_of_one_document_corroborate_nothing() {
        // These tests assert EXACT row/batch counts read through the
        // process-wide connector registry; a sibling test replaces the csv
        // connector with a 1-row fake inside its lock window, so exact-count
        // tests must serialize behind the SAME shared lock.
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        // The SAME extraction for every call, with usage reported per call.
        let extraction = serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Zorbium", "properties": {}},
                {"type": "Phase", "name": "omega-z", "properties": {}}
            ],
            "relationships": [
                {"from": "Zorbium", "rel": "HAS_PHASE", "to": "omega-z"}
            ]
        });
        let body = serde_json::json!({
            "choices": [{
                "message": {"content": extraction.to_string()},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
        });
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .expect(2)
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\nx2,y2\n");
        let db_path = scratch.db_path();
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            batch_rows: Some(1), // force 2 batches over 2 rows
            ..pipeline_against(server.uri(), db_path.clone()).config
        });

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!((result.batches, result.rows_processed), (2, 2));

        // The merged record reports the fact ONCE, not once per batch.
        let merged = result.entities.expect("extraction ran");
        assert_eq!(merged.entities.len(), 2);
        assert_eq!(merged.relationships.len(), 1);

        // And the STORE holds one evidence contribution — the second batch
        // did not count as a second source.
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let evidence = store
            .assertion_evidence("local", "Zorbium", "HAS_PHASE", "omega-z")
            .await
            .unwrap();
        assert_eq!(
            evidence.len(),
            1,
            "two batches of one file must contribute ONE evidence row: {evidence:?}"
        );
        let recalled = store
            .recall_with_context("Zorbium", "local", 10)
            .await
            .unwrap();
        assert_eq!(recalled.len(), 1);
        assert!(
            (recalled[0].confidence - 0.8).abs() < 1e-9,
            "confidence was inflated by same-document repetition: {}",
            recalled[0].confidence
        );

        // The run reports what it actually cost: usage summed over batches.
        let usage = result.llm_usage.expect("the mock reports usage");
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (200, 100, 300)
        );
        server.verify().await;
    }

    /// A failure at batch 2 of 2 costs batch 2: batch 1's facts are already
    /// stored, the failure lands on the errors spine (non-zero exit for the
    /// CLI), and the coverage report says exactly what was and was not
    /// processed. Reverting to all-or-nothing (extract everything, then
    /// write once) kills this test — nothing would be stored.
    #[tokio::test]
    async fn a_failed_batch_keeps_earlier_batches_facts() {
        // These tests assert EXACT row/batch counts read through the
        // process-wide connector registry; a sibling test replaces the csv
        // connector with a 1-row fake inside its lock window, so exact-count
        // tests must serialize behind the SAME shared lock.
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;

        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let good = serde_json::json!({
            "choices": [{
                "message": {"content": serde_json::json!({
                    "entities": [
                        {"type": "Alloy", "name": "Firstium", "properties": {}}
                    ],
                    "relationships": []
                }).to_string()},
                "finish_reason": "stop"
            }]
        });
        let server = MockServer::start().await;
        // First extraction call succeeds, every later one fails hard (400 is
        // not retried — see prism_runtime::retry).
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(good))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("boom"))
            .mount(&server)
            .await;

        let scratch = RefusalScratch::new();
        let csv = scratch.csv("a,b\nx,y\nx2,y2\n");
        let db_path = scratch.db_path();
        let pipeline = IngestPipeline::with_config(PipelineConfig {
            batch_rows: Some(1),
            ..pipeline_against(server.uri(), db_path.clone()).config
        });

        let result = pipeline.ingest_file(&csv).await.unwrap();

        // The failure is REPORTED, naming the batch and its rows…
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(
            result.errors[0].contains("batch 2/2") && result.errors[0].contains("rows 2-2"),
            "{}",
            result.errors[0]
        );
        assert_eq!((result.batches, result.batches_failed), (2, 1));
        assert_eq!(
            result.rows_processed, 1,
            "coverage must reflect the shortfall"
        );

        // …and batch 1's facts SURVIVED it.
        let graph = result.graph.expect("batch 1 must have been written");
        assert_eq!(graph.nodes_created, 1);
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        let hits = store.graph_search("Firstium", "local", 10).await.unwrap();
        assert!(
            hits.iter().any(|n| n.name == "Firstium"),
            "the failed batch discarded the earlier batch's stored facts: {hits:?}"
        );
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

    static CHEM_VERSION_IRI: std::sync::LazyLock<crate::ontologies::Iri> =
        std::sync::LazyLock::new(|| {
            crate::ontologies::Iri::new("https://example.invalid/chem/1".to_string())
                .expect("test version IRI is absolute")
        });
    static CHEM_CLASSES: std::sync::LazyLock<Vec<crate::ontologies::ClassDecl>> =
        std::sync::LazyLock::new(|| {
            vec![crate::ontologies::ClassDecl {
                iri: crate::ontologies::Iri::new(
                    "https://example.invalid/chem#Molecule".to_string(),
                )
                .expect("test class IRI is absolute"),
                pref_label: Some("Molecule".into()),
                parents: Vec::new(),
                extraction_labels: vec!["Molecule".into()],
            }]
        });
    static CHEM_RELATIONS: std::sync::LazyLock<Vec<crate::ontologies::RelationDecl>> =
        std::sync::LazyLock::new(|| {
            vec![crate::ontologies::RelationDecl {
                iri: crate::ontologies::Iri::new(
                    "https://example.invalid/chem#reactsWith".to_string(),
                )
                .expect("test property IRI is absolute"),
                pref_label: Some("reactsWith".into()),
                extraction_labels: vec!["REACTS_WITH".into()],
            }]
        });

    impl crate::ontologies::Ontology for ChemOntology {
        fn id(&self) -> &'static str {
            self.id
        }
        fn version_iri(&self) -> &crate::ontologies::Iri {
            &CHEM_VERSION_IRI
        }
        fn artifact_sha256(&self) -> &str {
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        }
        fn classes(&self) -> &[crate::ontologies::ClassDecl] {
            &CHEM_CLASSES
        }
        fn relations(&self) -> &[crate::ontologies::RelationDecl] {
            &CHEM_RELATIONS
        }
        fn is_a(&self, sub: &crate::ontologies::Iri, sup: &crate::ontologies::Iri) -> bool {
            sub == sup
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

    /// Production-dispatch proof for canonical class identity, the repaired
    /// HAS_PHASE declaration, and assertion classification provenance.
    ///
    /// The test deliberately enters through `ingest_file -> active(None)`;
    /// constructing a graph fixture directly would not detect a hardcoded
    /// production bypass (REQ-OWL-S1-CANONICAL-CLASS-IDENTITY,
    /// REQ-OWL-S1-CLASSIFICATION-PROVENANCE, REQ-OWL-S1-HAS-PHASE).
    #[tokio::test]
    async fn production_dispatch_persists_class_iris_and_ontology_stamp_for_has_phase() {
        unsafe { std::env::set_var("PRISM_EMBED_BACKEND", "off") };

        let server = mock_llm(serde_json::json!({
            "entities": [
                {"type": "Alloy", "name": "Steel", "properties": {}},
                {"type": "Phase", "name": "BCC", "properties": {}}
            ],
            "relationships": [
                {"from": "Steel", "rel": "HAS_PHASE", "to": "BCC"}
            ]
        }))
        .await;
        let scratch = RefusalScratch::new();
        let db_path = scratch.db_path();
        let csv = scratch.csv("alloy,phase\nSteel,BCC\n");
        let pipeline = pipeline_against(server.uri(), db_path.clone());

        let result = pipeline.ingest_file(&csv).await.unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let report = result.graph_validation.expect("validation ran");
        assert!(
            !report
                .issues
                .iter()
                .any(|issue| issue.category == "unknown_rel" && issue.message.contains("HAS_PHASE")),
            "HAS_PHASE drift returned: {:?}",
            report.issues
        );

        let ontology = crate::ontologies::active(None).expect("production EMMO resolves");
        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
        for (name, declared_type, storage_label) in
            [("Steel", "Alloy", "Matter"), ("BCC", "Phase", "Phase")]
        {
            let expected_iri = ontology
                .class_for_label(declared_type)
                .expect("production label resolver knows the extracted type")
                .iri
                .as_str();
            let nodes = store.graph_search(name, "local", 10).await.unwrap();
            assert!(
                nodes.iter().any(|node| {
                    node.name == name
                        && node.label == storage_label
                        && node.entity_type == declared_type
                        && node.class_iri.as_deref() == Some(expected_iri)
                }),
                "production persistence lost the declared type or canonical IRI: {nodes:?}"
            );
            assert!(
                nodes
                    .iter()
                    .filter_map(|node| node.class_iri.as_deref())
                    .all(|iri| {
                        ontology
                            .classes()
                            .iter()
                            .any(|class| class.iri.as_str() == iri)
                    }),
                "a stored class_iri is not declared by the loaded ontology: {nodes:?}"
            );
        }

        let traversal = store
            .get_neighbors("Steel", Some("HAS_PHASE"), "local", 10)
            .await
            .unwrap();
        assert!(
            traversal.edges.iter().any(|edge| edge.source == "Steel"
                && edge.target == "BCC"
                && edge.rel_type == "HAS_PHASE"),
            "HAS_PHASE did not reach the phase persistence arm: {traversal:?}"
        );

        let assertion = prism_provenance::assertion_id("local", "Steel", "HAS_PHASE", "BCC");
        let classifications = store.assertion_classifications(&assertion).await.unwrap();
        assert!(
            classifications.iter().any(|classification| {
                classification.version_iri == ontology.version_iri().as_str()
                    && classification.artifact_sha256 == ontology.artifact_sha256()
            }),
            "assertion has no matching ontology version/hash stamp: {classifications:?}"
        );
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
            report
                .issues
                .iter()
                .any(|i| i.category == "unknown_type" && i.message.contains("Molecule")),
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

        // The DEFAULT read scope DISCOVERS the second ontology's tenant —
        // that is what makes loaded reference vocabularies (MatKG) reachable
        // by `prism query` and the agent without a flag. Discovery is not
        // blending: every read above proved the subgraphs stay disjoint,
        // and each returned row names its owning tenant. (This deliberately
        // reverses the earlier "not absorbed" pin, which predates the MatKG
        // reference graph.)
        assert_eq!(
            store.default_read_tenants().await.unwrap(),
            ["local", "local@chem-coexist"]
        );
    }
}
