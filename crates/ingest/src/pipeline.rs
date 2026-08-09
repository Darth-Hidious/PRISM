use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use polars::prelude::*;
use prism_provenance::{
    ClassifiedFactNodes, ClassifiedNode, EvidenceClass, LocalProvenance, OntologyClassification,
    ProvenanceStore,
};
use serde::{Deserialize, Serialize};
use tracing;

use crate::local_facts::to_local_facts;
use crate::ontology::LlmOntologyConstructor;
use crate::schema::SchemaDetector;
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
        // exist. Error-severity issues refuse the graph write, with TWO
        // contained classes: a relationship whose endpoint was never declared
        // (`orphan_rel`) invalidates THAT relationship, and an entity whose
        // name normalised to nothing invalidates THAT entity — each is
        // dropped and reported (`dropped_relationships`/`dropped_entities`),
        // and everything valid is still stored. Failing wholesale here is
        // what kept the graph empty (2026-08-08: 17 orphan errors discarded
        // 13 good entities); inventing a missing endpoint would fabricate a
        // type.
        let mut dropped_relationships: Vec<String> = Vec::new();
        let mut dropped_entities: Vec<String> = Vec::new();
        let mut write_set: Option<EntitySet> = None;
        let graph_validation = entities.as_ref().map(|entity_set| {
            let (report, plan) = validate_before_graph_write(ontology.as_ref(), entity_set);
            match plan {
                GraphWritePlan::Blocked(msg) => {
                    tracing::error!(issues = report.issues.len(), "{msg}");
                    errors.push(msg);
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
                            "relationships referencing undeclared entities were dropped; \
                             the valid remainder is stored"
                        );
                    } else if dropped_ents.is_empty() && !report.issues.is_empty() {
                        tracing::warn!(
                            issues = report.issues.len(),
                            "graph validation found non-blocking issues"
                        );
                    }
                    dropped_relationships = dropped;
                    dropped_entities = dropped_ents;
                    write_set = Some(set);
                }
            }
            report
        });

        // Step 4: local EMMO graph write into the bundled Turso store (if
        // entities exist and validation produced a writable set). This
        // replaced the Neo4j upsert (Neo4j retirement, step 1) — the store
        // is bundled, so no backend config gates the write.
        // Facts land under the ontology's storage tenant: the default
        // ontology keeps the bare "local" tenant every existing store was
        // written with; any other ontology gets a composed tenant, which is
        // what keeps two vocabularies in one store from blending (the same
        // tenant-qualified isolation that separates local and peer knowledge).
        let tenant =
            crate::ontologies::storage_tenant(prism_provenance::LOCAL_TENANT, ontology.id());
        let graph = if let Some(entity_set) = &write_set {
            match self
                .write_local_graph(ontology.as_ref(), entity_set, &source, &tenant)
                .await
            {
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
        // `write_local_graph` (embed_names_best_effort); the old Qdrant
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
            dropped_relationships,
            dropped_entities,
            errors,
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
    async fn write_local_graph(
        &self,
        ontology: &dyn crate::ontologies::Ontology,
        entity_set: &EntitySet,
        source: &DataSource,
        tenant: &str,
    ) -> Result<GraphUpdate> {
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

        let ontology_classification = OntologyClassification {
            version_iri: ontology.version_iri().as_str(),
            artifact_sha256: ontology.artifact_sha256(),
        };

        let facts = to_local_facts(entity_set);
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
            if !written.insert(e.name.as_str()) {
                continue; // already a node via some relationship (or a duplicate name)
            }
            let props = match &e.properties {
                serde_json::Value::Object(map) if !map.is_empty() => Some(e.properties.to_string()),
                _ => None,
            };
            store
                .write_classified_entity(&e.name, classification_of(&e.name)?, props, &prov.tenant)
                .await?;
        }

        // Best-effort: vectorize every node name this write landed (endpoint
        // AND standalone) into the same Turso store so `prism query
        // --semantic` works without Qdrant. Failures are logged inside and
        // never fail the ingest.
        let names: Vec<String> = written.iter().map(|s| s.to_string()).collect();
        store.embed_names_best_effort(&names, &prov.tenant).await;

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
            if ontology.class_for_label(extraction_label).is_some()
                && ontology.storage_label(extraction_label).is_some()
            {
                kept.push(e.clone());
            } else {
                dropped.push(format!(
                    "entity '{}': type '{}' has no storage label or canonical class IRI in ontology '{}' \
                     (declared: {})",
                    e.name,
                    e.entity_type,
                    ontology.id(),
                    declared
                ));
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

        let emmo = crate::ontologies::EmmoOntology;
        let update = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local")
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
            }],
        };
        let source = DataSource {
            path: "/tmp/alloys.csv".into(),
            format: "csv".into(),
        };

        let emmo = crate::ontologies::EmmoOntology;
        let update = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local")
            .await
            .unwrap();

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
            max_sample_rows: 10,
            mapping: None,
            provenance_db: Some(db_path.clone()),
            ontology: None,
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
                entity(
                    "Property",
                    "density",
                    serde_json::json!({"value": 7.8, "unit": "g/cm3"}),
                ),
                entity(
                    "Property",
                    "atomic mass",
                    serde_json::json!({"value": 55.8, "unit": "u"}),
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
                rel("Fe", "HAS_PROPERTY", "atomic mass"),
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
        let update = pipeline
            .write_local_graph(&emmo, &entity_set, &source, "local")
            .await
            .unwrap();
        assert_eq!(update.nodes_created, 10);
        assert_eq!(update.edges_created, 6);

        let store = prism_provenance::ProvenanceStore::open(&db_path)
            .await
            .unwrap();
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
            graph_validation: None,
            graph: None,
            embeddings: None,
            dropped_relationships: Vec::new(),
            dropped_entities: Vec::new(),
            errors: Vec::new(),
        };
        let json = serde_json::to_string(&result).unwrap();
        // None fields should not appear in JSON.
        assert!(!json.contains("entities"));
        assert!(!json.contains("graph_validation"));
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

        // The DEFAULT read scope (local + discovered mesh tenants) does not
        // silently absorb the second ontology's subgraph.
        assert_eq!(store.default_read_tenants().await.unwrap(), ["local"]);
    }
}
