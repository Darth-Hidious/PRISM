//! LLM-driven ontology construction — provider-agnostic.
//!
//! Sends schema + sample rows to any LLM backend (Ollama, OpenAI, MARC27, vLLM),
//! parses the structured JSON entity/relationship output, and returns typed results
//! ready for the local EMMO graph write (bundled Turso store).

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use tracing;

use crate::{
    DataSource, EmbeddingBatch, Entity, EntitySet, GraphUpdate, LlmConfig, OntologyConstructor,
    Relationship, SchemaAnalysis,
};

/// LLM-based ontology constructor — works with any provider via [`crate::llm::LlmClient`].
pub struct LlmOntologyConstructor {
    client: crate::llm::LlmClient,
    #[allow(dead_code)] // retained for parity with `new(config)` callers/tests
    config: LlmConfig,
    /// Token usage accumulated across every extraction call this
    /// constructor made, when the backend reports it. Output is metered and
    /// billed per token — counting is the control — so a batched run must
    /// be able to report what it actually cost.
    usage: std::sync::Mutex<Option<prism_llm::UsageInfo>>,
}

// Wire format structs removed — LlmClient handles provider-specific APIs.

/// Raw extraction output the LLM is prompted to produce.
#[derive(Deserialize)]
struct ExtractionOutput {
    entities: Vec<RawEntity>,
    relationships: Vec<RawRelationship>,
}

#[derive(Deserialize)]
struct RawEntity {
    #[serde(rename = "type")]
    entity_type: String,
    name: String,
    #[serde(default)]
    properties: serde_json::Value,
}

#[derive(Deserialize)]
struct RawRelationship {
    from: String,
    rel: String,
    to: String,
    #[serde(default, deserialize_with = "lenient_f64")]
    weight: Option<f64>,
    #[serde(default, deserialize_with = "lenient_u32")]
    order: Option<u32>,
    /// Per-edge measured value — the attribution channel for measurements
    /// (a value on a SHARED target entity cannot say whose value it is; see
    /// `Relationship::value`). Lenient like `weight`: models emit numeric
    /// strings.
    #[serde(default, deserialize_with = "lenient_f64")]
    value: Option<f64>,
    /// Unit spelling for `value`; resolved through the one controlled
    /// vocabulary at fact mapping, never trusted raw.
    #[serde(default)]
    unit: Option<String>,
    /// Optional extractor judgement. The shared deserializer accepts the
    /// numeric-string shape seen from prompt-only backends, but retains only
    /// finite probabilities in `[0, 1]`; invalid input becomes honest
    /// absence and receives the fact mapper's documented fallback later.
    #[serde(
        default,
        deserialize_with = "crate::deserialize_relationship_confidence"
    )]
    confidence: Option<f64>,
}

/// Accept a number, a numeric string ("3.5"), or anything else → None.
///
/// Alloy compositions routinely carry NON-numeric weights for the remainder
/// element — "balance", "bal.", "trace" — and LLM extractors faithfully emit
/// them (live failure: claude-sonnet-5 returned `weight: "balance"` for the
/// Ni fraction of Inconel 718 and the strict f64 field failed the WHOLE
/// document). One odd value must not sink an extraction.
fn lenient_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// Same leniency for integer fields (e.g. `order: "2"`).
fn lenient_u32<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// Normalise one extracted name at the extraction boundary — the single
/// place [`ExtractionOutput`] becomes the internal [`EntitySet`]. Applied to
/// entity `name` and relationship `from`/`to` by the same function, so both
/// sides of every later exact-match comparison (referential integrity,
/// fact-write keying) are produced identically and agree by construction.
///
/// The live failure this exists for (2026-08-08, qwen2.5:3b over a 5-row
/// alloys CSV): the model declared elements bare (`Ti`) and referenced them
/// QUOTED (`"Ti"`) in relationships — every edge dangled and the stored
/// graph had 11 nodes and ZERO edges. The raw model JSON really contained
/// `"name": "\"Ti-6Al-4V\""` for an unquoted CSV cell.
///
/// What comes off: surrounding whitespace, and BALANCED surrounding quote
/// pairs — ASCII `"…"` / `'…'` and the Unicode curly forms `“…”` / `‘…’` —
/// stripped repeatedly with re-trimming between layers, so `"'Ti'"` fully
/// unwraps. What is deliberately preserved: interior quotes (`6" pipe`,
/// `Ni-"free" steel` are data), unbalanced quotes (`"Ti`), and mismatched
/// ends (`“Ti"`). Idempotent: a normalised name passes through unchanged.
///
/// A name that normalises to EMPTY is not repaired here: it flows on and
/// the graph-write plan rejects it — dropped and reported via
/// `dropped_entities` — because an empty name is a rejection, not a name
/// (`pipeline::validate_before_graph_write`).
fn normalise_extracted_name(raw: &str) -> String {
    const PAIRS: [(char, char); 4] = [
        ('"', '"'),
        ('\'', '\''),
        ('\u{201C}', '\u{201D}'), // “ … ”
        ('\u{2018}', '\u{2019}'), // ‘ … ’
    ];
    let mut name = raw.trim();
    loop {
        let mut chars = name.chars();
        let (Some(first), Some(last)) = (chars.next(), chars.next_back()) else {
            break; // zero or one char left — nothing strippable
        };
        if !PAIRS.contains(&(first, last)) {
            break;
        }
        name = name[first.len_utf8()..name.len() - last.len_utf8()].trim();
    }
    name.to_string()
}

/// Convert the extractor wire shape into the public graph shape at one
/// auditable boundary. Name and confidence normalisation both happen here
/// before validation or fact mapping can observe the proposal.
fn materialise_extraction_output(
    raw: ExtractionOutput,
    mapping: Option<&crate::mapping::OntologyMapping>,
) -> EntitySet {
    let mut entities: Vec<Entity> = raw
        .entities
        .into_iter()
        .map(|entity| Entity {
            entity_type: entity.entity_type,
            name: normalise_extracted_name(&entity.name),
            properties: if entity.properties.is_null() {
                serde_json::Value::Object(Default::default())
            } else {
                entity.properties
            },
        })
        .collect();

    let mut relationships: Vec<Relationship> = raw
        .relationships
        .into_iter()
        .map(|relationship| Relationship {
            from: normalise_extracted_name(&relationship.from),
            rel_type: relationship.rel,
            to: normalise_extracted_name(&relationship.to),
            weight: relationship.weight,
            order: relationship.order,
            value: relationship.value,
            unit: relationship.unit,
            confidence: relationship.confidence,
        })
        .collect();

    // Apply alias expansion (e.g. "Nb" -> "Niobium") to entity names and
    // to every relationship endpoint referencing those names, so the two
    // stay consistent — expanding only one side would silently turn a real
    // relationship into an orphan.
    if let Some(mapping) = mapping {
        for entity in &mut entities {
            entity.name = mapping.expand_alias(&entity.name);
        }
        for relationship in &mut relationships {
            relationship.from = mapping.expand_alias(&relationship.from);
            relationship.to = mapping.expand_alias(&relationship.to);
        }
    }

    EntitySet {
        entities,
        relationships,
    }
}

impl LlmOntologyConstructor {
    pub fn new(config: LlmConfig) -> Self {
        let client = crate::llm::LlmClient::new(config.clone());
        Self {
            client,
            config,
            usage: std::sync::Mutex::new(None),
        }
    }

    /// Check that the LLM backend is reachable.
    pub async fn health_check(&self) -> Result<()> {
        self.client.health_check().await
    }

    /// The model's context window in tokens (configured, GGUF-derived, or
    /// probed from the serving runtime) — what the pipeline derives batch
    /// sizes from. `None` is a config gap the pipeline reports loudly.
    pub async fn probe_context_window(&self) -> Option<u64> {
        self.client.probe_context_window().await
    }

    /// Token usage summed over every extraction call so far, when the
    /// backend reported any. `None` = nothing was ever reported (not zero).
    pub fn total_usage(&self) -> Option<prism_llm::UsageInfo> {
        self.usage.lock().expect("usage lock poisoned").clone()
    }

    /// Fold one call's reported token usage into the running total.
    fn fold_usage(&self, usage: Option<prism_llm::UsageInfo>) {
        if let Some(u) = usage {
            let mut total = self.usage.lock().expect("usage lock poisoned");
            let t = total.get_or_insert(prism_llm::UsageInfo {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });
            t.prompt_tokens += u.prompt_tokens;
            t.completion_tokens += u.completion_tokens;
            t.total_tokens += u.total_tokens;
        }
    }

    /// Call the LLM with a prompt and expect JSON output, folding reported
    /// token usage into the running total.
    async fn generate(&self, prompt: &str) -> Result<String> {
        let (text, usage) = self.client.generate_json_with_usage(prompt).await?;
        self.fold_usage(usage);
        Ok(text)
    }

    /// Embed a single text string. Returns the embedding vector.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        self.client.embed_text(text).await
    }

    /// Batch embedding.
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.client.embed(texts).await
    }

    /// Build the tabular extraction prompt. The preamble and the
    /// `## Instructions` block come from the ACTIVE ontology — the same
    /// adapter whose declared vocabulary `graph_validation` accepts — so
    /// what the model is told to emit and what the validator accepts
    /// cannot drift apart per ontology.
    fn build_extraction_prompt_with_mapping(
        ontology: &dyn crate::ontologies::Ontology,
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
        mapping: Option<&crate::mapping::OntologyMapping>,
    ) -> String {
        let mut prompt = String::with_capacity(2048);
        prompt.push_str(&ontology.extraction_preamble());
        prompt.push_str("\n\n");

        prompt.push_str("## Schema\n");
        prompt.push_str("Columns: ");
        for (i, col) in schema.columns.iter().enumerate() {
            if i > 0 {
                prompt.push_str(", ");
            }
            prompt.push_str(&format!("{} ({})", col, schema.detected_types[i]));
        }
        prompt.push_str("\n\n");

        prompt.push_str("## Sample Rows\n");
        for (i, row) in sample_rows.iter().enumerate() {
            prompt.push_str(&format!("Row {}: {:?}\n", i + 1, row));
        }
        prompt.push('\n');

        prompt.push_str(&ontology.extraction_instructions());

        // Append custom mapping rules if provided
        if let Some(m) = mapping {
            prompt.push_str(&m.to_prompt_supplement());
        }

        prompt
    }

    /// Build text representations of entities for embedding.
    fn entity_to_text(entity: &Entity) -> String {
        let props = if entity.properties.is_object() {
            let obj = entity.properties.as_object().unwrap();
            obj.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            String::new()
        };

        if props.is_empty() {
            format!("{}: {}", entity.entity_type, entity.name)
        } else {
            format!("{}: {} ({})", entity.entity_type, entity.name, props)
        }
    }
}

#[async_trait]
impl OntologyConstructor for LlmOntologyConstructor {
    async fn analyze_schema(&self, source: &DataSource) -> Result<SchemaAnalysis> {
        // Schema analysis is done by the polars-based SchemaDetector.
        // This method exists on the trait for backends that do LLM-based
        // schema inference (e.g. unstructured text). For tabular data,
        // callers should use SchemaDetector directly and pass the result
        // to extract_entities.
        tracing::info!(path = %source.path, format = %source.format, "LLM schema analysis requested");

        let prompt = format!(
            "Analyze this data source and describe its schema.\n\
             Path: {}\nFormat: {}\n\
             Return JSON: {{\"columns\": [...], \"detected_types\": [...]}}",
            source.path, source.format
        );

        let response = self.generate(&prompt).await?;
        let analysis: SchemaAnalysis =
            serde_json::from_str(&response).context("LLM returned invalid schema JSON")?;
        Ok(analysis)
    }

    async fn extract_entities(
        &self,
        _source: &DataSource,
        schema: &SchemaAnalysis,
    ) -> Result<EntitySet> {
        // Blind extraction (zero sample rows) made the LLM invent entities
        // from column names alone — plausible-looking garbage that callers
        // couldn't distinguish from real extraction (audit 1.1). The pipeline
        // path is `extract_entities_with_samples`, which feeds real rows.
        // Refuse honestly instead of guessing.
        anyhow::bail!(
            "blind entity extraction (no sample rows) is not supported — it produces \
             fabricated entities from column names alone. Use \
             extract_entities_with_samples with real rows from the connector \
             (schema has {} columns).",
            schema.columns.len()
        )
    }

    async fn build_graph(&self, entities: &EntitySet) -> Result<GraphUpdate> {
        // Graph construction is handled by the GraphStore implementation.
        // This method on the OntologyConstructor trait is a convenience
        // that returns a summary of what would be written.
        Ok(GraphUpdate {
            nodes_created: entities.entities.len(),
            edges_created: entities.relationships.len(),
        })
    }

    async fn generate_embeddings(&self, entities: &EntitySet) -> Result<EmbeddingBatch> {
        if entities.entities.is_empty() {
            return Ok(EmbeddingBatch {
                vectors: vec![],
                ids: vec![],
                dimension: None,
            });
        }

        let texts: Vec<String> = entities.entities.iter().map(Self::entity_to_text).collect();
        let ids: Vec<String> = entities.entities.iter().map(|e| e.name.clone()).collect();

        tracing::info!(count = texts.len(), "generating embeddings via Ollama");
        let vectors = self.embed(texts).await?;
        let dimension = vectors.first().map(|v| v.len());

        Ok(EmbeddingBatch {
            vectors,
            ids,
            dimension,
        })
    }
}

/// One tabular extraction plus the honest record of how it was decoded:
/// whether the endpoint enforced the ontology-derived schema, why it
/// degraded when it didn't, and the seed/temperature actually sent
/// (recorded into the provenance activity by the pipeline).
#[derive(Debug, Clone)]
pub struct TracedExtraction {
    pub entities: EntitySet,
    pub decoding: prism_llm::JsonDecodingTrace,
}

/// Whether extraction requests should carry the reasoning kill-switch
/// (`LLM_NO_THINK=1`/`true` in the environment — the same `LLM_*` surface
/// the other model knobs use). Opt-in because the kwarg is a
/// llama-server/vLLM extension OpenAI rejects with 400; needed because a
/// thinking-mode model can burn ANY output budget on reasoning before the
/// constrained JSON starts (measured 2026-08-10, Gemma-4-12B: 46.7k chars
/// of reasoning against a 16384-token budget, zero JSON). The flag is
/// recorded in the decoding trace and the provenance activity either way.
fn extraction_no_think() -> bool {
    prism_llm::no_think_requested()
}

/// Convenience method: extract entities with explicit sample rows (bypasses DataSource).
impl LlmOntologyConstructor {
    pub async fn extract_entities_with_samples(
        &self,
        ontology: &dyn crate::ontologies::Ontology,
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
    ) -> Result<EntitySet> {
        self.extract_entities_with_mapping(ontology, schema, sample_rows, None)
            .await
    }

    /// Source-compatible wrapper over [`Self::extract_entities_traced`] for
    /// callers that do not consume the decoding trace.
    pub async fn extract_entities_with_mapping(
        &self,
        ontology: &dyn crate::ontologies::Ontology,
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
        mapping: Option<&crate::mapping::OntologyMapping>,
    ) -> Result<EntitySet> {
        Ok(self
            .extract_entities_traced(ontology, schema, sample_rows, mapping)
            .await?
            .entities)
    }

    pub async fn extract_entities_traced(
        &self,
        ontology: &dyn crate::ontologies::Ontology,
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
        mapping: Option<&crate::mapping::OntologyMapping>,
    ) -> Result<TracedExtraction> {
        // Zero sample rows means there is nothing real to extract from: the
        // prompt would carry only the header names, and the model invents
        // plausible-looking entities from them — the same failure mode the
        // trait's `extract_entities` refuses as blind extraction. This is
        // the last gate before the prompt is built, so no caller can reach
        // the model without data.
        if sample_rows.is_empty() {
            anyhow::bail!(
                "refusing entity extraction with zero sample rows — it produces \
                 fabricated entities from column names alone (schema has {} \
                 columns, no data rows). Feed real rows from the connector.",
                schema.columns.len()
            );
        }
        if let Some(mapping) = mapping {
            mapping.validate_for(ontology)?;
        }
        // NO truncation here. This used to silently cap the rows at
        // `config.max_sample_rows` (default 10) — the second of two places
        // the same literal lived, so even a caller that sized its batch
        // honestly lost everything past row 10 without a word. The CALLER
        // owns batch sizing (`pipeline` derives it from the model's context
        // window); this function extracts from exactly what it is given.
        let rows = sample_rows;

        // Drop ignore_columns from what the LLM actually sees, instead of
        // only mentioning them in the prompt as a hint the model was free
        // to disregard (AUDIT_BACKLOG 21 / INGESTION_AUDIT #21 —
        // `should_ignore` had zero production callers).
        let (owned_schema, owned_rows);
        let (schema, rows): (&SchemaAnalysis, &[Vec<String>]) = if let Some(m) = mapping {
            let (s, r) = m.filter_ignored_columns(schema, rows);
            owned_schema = s;
            owned_rows = r;
            (&owned_schema, owned_rows.as_slice())
        } else {
            (schema, rows)
        };

        tracing::info!(
            columns = schema.columns.len(),
            sample_rows = rows.len(),
            "extracting entities via LLM with samples"
        );

        let prompt = Self::build_extraction_prompt_with_mapping(ontology, schema, rows, mapping);
        // Constrained decoding, derived from the SAME active-ontology
        // declaration the prompt above and graph validation read: the model
        // is structurally unable to emit an undeclared class, an undeclared
        // relation, or a non-QUDT unit. Deterministic knobs (temperature 0,
        // recorded seed) ride the same request; an endpoint that rejects the
        // schema degrades honestly and the trace says so.
        let extraction_schema = crate::extraction_schema::extraction_json_schema(ontology);
        let constrained = self
            .client
            .generate_json_with_schema(
                &prompt,
                &extraction_schema,
                prism_llm::EXTRACTION_SEED,
                extraction_no_think(),
            )
            .await?;
        // Output is metered and billed per token — counting is the control —
        // so every extraction call's reported usage lands on the running
        // total the pipeline reports at the end of a batched run.
        self.fold_usage(constrained.usage);
        let response = constrained.text;

        let raw: ExtractionOutput =
            serde_json::from_str(&response).context("LLM returned invalid extraction JSON")?;

        // Names and confidence are normalised HERE — the one place raw model
        // output becomes the internal EntitySet — before graph validation or
        // fact mapping can mistake malformed model output for a judgement.
        let entities = materialise_extraction_output(raw, mapping);

        Ok(TracedExtraction {
            entities,
            decoding: constrained.trace,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_extraction_prompt_includes_schema() {
        let schema = SchemaAnalysis {
            columns: vec!["Composition".into(), "Hardness_HV".into()],
            detected_types: vec!["string".into(), "float".into()],
        };
        let rows = vec![vec!["Nb25Mo25Ta25W25".into(), "542".into()]];
        let prompt = LlmOntologyConstructor::build_extraction_prompt_with_mapping(
            &crate::ontologies::EmmoOntology,
            &schema,
            &rows,
            None,
        );

        assert!(prompt.contains("Composition (string)"));
        assert!(prompt.contains("Hardness_HV (float)"));
        assert!(prompt.contains("Nb25Mo25Ta25W25"));
        assert!(prompt.contains("CONTAINS"));
        assert!(prompt.contains("PROCESSED_BY"));
    }

    /// The byte-identity contract, amended for explicit ingest invariants:
    /// ontology active, the extraction prompt is EXACTLY the string the
    /// pre-trait hardcoded builder produced PLUS the referential-integrity
    /// line ("Every name used in \"from\" or \"to\" MUST also appear…")
    /// PLUS the typed-value rule ("For \"Property\" entities: \"name\" is
    /// the property NAME…") and the optional bounded relationship-confidence
    /// rule. These divergences are deliberate: the verbatim
    /// legacy text told the model nothing about declaring relationship
    /// endpoints, while graph validation refuses undeclared endpoints
    /// (`orphan_rel`, Error severity) — so the byte-identical prompt
    /// reliably produced extractions that could not be stored (live
    /// 2026-08-08: 13 entities, 13 relationships, 17 orphan errors, nothing
    /// written). And it told the model nothing about which FIELD a
    /// measurement belongs in, so the model satisfied the schema by naming
    /// Properties after their measurements (live 2026-08-08: "1100 MPa" as
    /// an entity name, `prov_assertion.value` null, nothing queryable as a
    /// number). Everything else must still not shift by a byte.
    #[test]
    fn emmo_prompt_is_the_legacy_prompt_plus_only_the_declared_ingest_rules() {
        let schema = SchemaAnalysis {
            columns: vec!["Composition".into(), "Hardness_HV".into()],
            detected_types: vec!["string".into(), "float".into()],
        };
        let rows = vec![vec!["Nb25Mo25Ta25W25".into(), "542".into()]];
        let prompt = LlmOntologyConstructor::build_extraction_prompt_with_mapping(
            &crate::ontologies::EmmoOntology,
            &schema,
            &rows,
            None,
        );

        let expected = concat!(
            "You are a materials science data analyst. Given a dataset schema and sample rows, \
             extract all entities and relationships into a structured JSON format.\n\n",
            "## Schema\n",
            "Columns: Composition (string), Hardness_HV (float)\n\n",
            "## Sample Rows\n",
            "Row 1: [\"Nb25Mo25Ta25W25\", \"542\"]\n",
            "\n",
            "## Instructions\n\
             Identify ALL materials science entities:\n\
             - Alloy/Material compositions (type: \"Alloy\" or \"Material\")\n\
             - Elements with fractions (type: \"Element\")\n\
             - Processing steps with parameters (type: \"Process\")\n\
             - Measured properties with values and units (type: \"Property\")\n\
             - Phases or crystal structures (type: \"Phase\")\n\n\
             Identify ALL relationships:\n\
             - CONTAINS (material → element, with weight = fraction)\n\
             - PROCESSED_BY (material → process, with order)\n\
             - HAS_PROPERTY (material → property)\n\
             - HAS_PHASE (material → phase)\n\n\
             Every name used in \"from\" or \"to\" MUST also appear as an entity in \"entities\".\n\
             For each relationship, optionally set \"confidence\" to your estimated probability that the relationship is correct, as a finite number from 0 to 1. Omit it when you cannot assess the relationship; never invent a score just to fill the field.\n\
             For \"Property\" entities: \"name\" is the property NAME (e.g. \"yield strength\"), \
             NEVER the measured value — an entity named like \"1100 MPa\" is rejected, not \
             stored. Each material's measured number goes on that material's OWN relationship \
             to the property: set the relationship's \"value\" to the number and \"unit\" to \
             one of the listed units (one value per material — never one shared number for \
             several materials; if no listed unit fits, leave \"value\" and \"unit\" out of \
             the relationship entirely). Units: QUDT:PA/QUDT:KiloPA/QUDT:MegaPA/QUDT:GigaPA \
             for pressure/stress; QUDT:KiloGM-PER-M3/QUDT:GM-PER-CentiM3 for density; \
             QUDT:K/QUDT:DEG_C for temperature; QUDT:W-PER-M-K for thermal conductivity; \
             QUDT:PERCENT for fraction. Use the unit that measures the SAME quantity as the \
             property — a density belongs in QUDT:GM-PER-CentiM3 or QUDT:KiloGM-PER-M3, \
             never in a pressure unit.\n\n\
             Return ONLY valid JSON with this structure:\n\
             {\n\
               \"entities\": [{\"type\": \"...\", \"name\": \"...\", \"properties\": {...}}],\n\
               \"relationships\": [{\"from\": \"...\", \"rel\": \"...\", \"to\": \"...\", \"weight\": null, \"order\": null, \"confidence\": null}]\n\
             }\n",
        );
        assert_eq!(prompt, expected);
    }

    #[test]
    fn entity_to_text_with_properties() {
        let entity = Entity {
            entity_type: "Alloy".into(),
            name: "Nb25Mo25Ta25W25".into(),
            properties: serde_json::json!({"system": "NbMoTaW"}),
        };
        let text = LlmOntologyConstructor::entity_to_text(&entity);
        assert!(text.contains("Alloy: Nb25Mo25Ta25W25"));
        assert!(text.contains("system"));
    }

    #[test]
    fn entity_to_text_without_properties() {
        let entity = Entity {
            entity_type: "Element".into(),
            name: "Nb".into(),
            properties: serde_json::Value::Object(Default::default()),
        };
        let text = LlmOntologyConstructor::entity_to_text(&entity);
        assert_eq!(text, "Element: Nb");
    }

    #[test]
    fn default_config_is_empty() {
        let cfg = LlmConfig::default();
        // Defaults are empty — real values from prism.toml or server config
        assert!(cfg.base_url.is_empty());
        assert!(cfg.model.is_empty());
    }

    #[test]
    fn extraction_output_parses() {
        let json = r#"{
            "entities": [
                {"type": "Alloy", "name": "NbMoTaW", "properties": {"system": "refractory"}},
                {"type": "Element", "name": "Nb"}
            ],
            "relationships": [
                {"from": "NbMoTaW", "rel": "CONTAINS", "to": "Nb", "weight": 0.25,
                 "confidence": 0.93}
            ]
        }"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.entities.len(), 2);
        assert_eq!(parsed.relationships[0].weight, Some(0.25));
        assert_eq!(parsed.relationships[0].confidence, Some(0.93));
    }

    #[test]
    fn extractor_confidence_reaches_local_fact_and_invalid_or_missing_uses_fallback() {
        let json = r#"{
            "entities": [],
            "relationships": [
                {"from": "A", "rel": "RELATED_TO", "to": "B", "confidence": "0.36"},
                {"from": "C", "rel": "RELATED_TO", "to": "D", "confidence": 2.0},
                {"from": "E", "rel": "RELATED_TO", "to": "F"}
            ]
        }"#;
        let raw: ExtractionOutput = serde_json::from_str(json).unwrap();
        let set = materialise_extraction_output(raw, None);

        assert_eq!(set.relationships[0].confidence, Some(0.36));
        assert_eq!(set.relationships[1].confidence, None);
        assert_eq!(set.relationships[2].confidence, None);

        let (facts, dropped) = crate::local_facts::to_local_facts(&set);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts[0].confidence, Some(0.36));
        assert_eq!(
            facts[1].confidence,
            Some(crate::local_facts::DEFAULT_CONFIDENCE)
        );
        assert_eq!(
            facts[2].confidence,
            Some(crate::local_facts::DEFAULT_CONFIDENCE)
        );
    }

    // --- build_extraction_prompt edge cases ---

    #[test]
    fn build_extraction_prompt_with_empty_sample_rows() {
        let schema = SchemaAnalysis {
            columns: vec!["Composition".into()],
            detected_types: vec!["string".into()],
        };
        let prompt = LlmOntologyConstructor::build_extraction_prompt_with_mapping(
            &crate::ontologies::EmmoOntology,
            &schema,
            &[],
            None,
        );
        // Must still include schema and instructions — just no row data.
        assert!(prompt.contains("Composition (string)"));
        assert!(prompt.contains("CONTAINS"));
        // No "Row 1" line since there are no rows.
        assert!(!prompt.contains("Row 1:"));
    }

    #[test]
    fn build_extraction_prompt_with_many_columns() {
        let columns: Vec<String> = (0..12).map(|i| format!("col_{i}")).collect();
        let types: Vec<String> = (0..12).map(|_| "float".into()).collect();
        let schema = SchemaAnalysis {
            columns,
            detected_types: types,
        };
        let prompt = LlmOntologyConstructor::build_extraction_prompt_with_mapping(
            &crate::ontologies::EmmoOntology,
            &schema,
            &[],
            None,
        );
        // All 12 columns must appear.
        for i in 0..12 {
            assert!(prompt.contains(&format!("col_{i}")));
        }
    }

    #[test]
    fn build_extraction_prompt_contains_instructions_section() {
        let schema = SchemaAnalysis {
            columns: vec!["X".into()],
            detected_types: vec!["int".into()],
        };
        let prompt = LlmOntologyConstructor::build_extraction_prompt_with_mapping(
            &crate::ontologies::EmmoOntology,
            &schema,
            &[],
            None,
        );
        assert!(prompt.contains("## Instructions"));
        assert!(prompt.contains("Return ONLY valid JSON"));
    }

    // --- entity_to_text edge cases ---

    #[test]
    fn entity_to_text_with_null_properties() {
        // Value::Null is not an object — the function should fall back to no-props format.
        let entity = Entity {
            entity_type: "Material".into(),
            name: "Ti6Al4V".into(),
            properties: serde_json::Value::Null,
        };
        let text = LlmOntologyConstructor::entity_to_text(&entity);
        assert_eq!(text, "Material: Ti6Al4V");
    }

    #[test]
    fn entity_to_text_with_array_properties() {
        // An array is not an object — should produce the no-props format.
        let entity = Entity {
            entity_type: "Phase".into(),
            name: "BCC".into(),
            properties: serde_json::json!([1, 2, 3]),
        };
        let text = LlmOntologyConstructor::entity_to_text(&entity);
        assert_eq!(text, "Phase: BCC");
    }

    #[test]
    fn entity_to_text_with_multiple_properties() {
        let entity = Entity {
            entity_type: "Alloy".into(),
            name: "Ti6Al4V".into(),
            properties: serde_json::json!({"hardness": 300, "density": 4.43}),
        };
        let text = LlmOntologyConstructor::entity_to_text(&entity);
        assert!(text.starts_with("Alloy: Ti6Al4V ("));
        assert!(text.contains("hardness"));
        assert!(text.contains("density"));
    }

    // --- ExtractionOutput edge cases ---

    #[test]
    fn extraction_output_empty_entities_and_relationships() {
        let json = r#"{"entities": [], "relationships": []}"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert!(parsed.entities.is_empty());
        assert!(parsed.relationships.is_empty());
    }

    #[test]
    fn extraction_output_relationship_with_null_weight_and_order() {
        let json = r#"{
            "entities": [],
            "relationships": [
                {"from": "A", "rel": "RELATED", "to": "B", "weight": null, "order": null}
            ]
        }"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert!(parsed.relationships[0].weight.is_none());
        assert!(parsed.relationships[0].order.is_none());
    }

    #[test]
    fn extraction_output_relationship_missing_optional_fields() {
        // weight and order fields completely absent — serde default should give None.
        let json = r#"{
            "entities": [],
            "relationships": [
                {"from": "A", "rel": "RELATED", "to": "B"}
            ]
        }"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert!(parsed.relationships[0].weight.is_none());
        assert!(parsed.relationships[0].order.is_none());
    }

    #[test]
    fn extraction_output_tolerates_non_numeric_weight() {
        // The live failure: composition remainders come back as "balance"
        // (Ni-bal.) — a domain-correct string in a numeric slot. It must
        // parse as None, not fail the whole document.
        let json = r#"{
            "entities": [],
            "relationships": [
                {"from": "IN718", "rel": "CONTAINS", "to": "Ni", "weight": "balance"},
                {"from": "IN718", "rel": "CONTAINS", "to": "Cr", "weight": "19.0"},
                {"from": "IN718", "rel": "CONTAINS", "to": "Nb", "weight": 5.1, "order": "2"}
            ]
        }"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert!(
            parsed.relationships[0].weight.is_none(),
            "\"balance\" → None"
        );
        assert_eq!(
            parsed.relationships[1].weight,
            Some(19.0),
            "numeric string parses"
        );
        assert_eq!(parsed.relationships[2].weight, Some(5.1));
        assert_eq!(
            parsed.relationships[2].order,
            Some(2),
            "string order parses"
        );
    }

    /// The per-edge measurement channel parses off the wire with the same
    /// leniency as `weight` (numeric strings happen), the unit rides along
    /// verbatim (the ONE vocabulary resolves it at fact mapping), and both
    /// default to None when absent — old extractions parse unchanged.
    #[test]
    fn relationship_value_and_unit_parse_leniently_and_default_off() {
        let json = r#"{
            "entities": [],
            "relationships": [
                {"from": "IN718", "rel": "HAS_PROPERTY", "to": "yield strength",
                 "value": 1100.0, "unit": "QUDT:MegaPA"},
                {"from": "IN718", "rel": "HAS_PROPERTY", "to": "density",
                 "value": "8.19", "unit": "g/cm3"},
                {"from": "IN718", "rel": "CONTAINS", "to": "Ni"}
            ]
        }"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.relationships[0].value, Some(1100.0));
        assert_eq!(parsed.relationships[0].unit.as_deref(), Some("QUDT:MegaPA"));
        assert_eq!(
            parsed.relationships[1].value,
            Some(8.19),
            "numeric string parses"
        );
        assert_eq!(parsed.relationships[1].unit.as_deref(), Some("g/cm3"));
        assert_eq!(parsed.relationships[2].value, None);
        assert_eq!(parsed.relationships[2].unit, None);
    }

    /// The live-path guard: zero sample rows are refused BEFORE any prompt
    /// is built or any request leaves the process — the model would
    /// otherwise invent entities from the column names alone.
    #[tokio::test]
    async fn extract_entities_with_zero_sample_rows_refuses() {
        let schema = SchemaAnalysis {
            columns: vec!["Composition".into(), "Hardness_HV".into()],
            detected_types: vec!["string".into(), "float".into()],
        };
        // Deliberately unconfigured client: if the guard were missing, this
        // would fail with a transport error instead — the message assertion
        // below distinguishes the refusal from any such failure.
        let constructor = LlmOntologyConstructor::new(LlmConfig::default());
        let err = constructor
            .extract_entities_with_samples(&crate::ontologies::EmmoOntology, &schema, &[])
            .await
            .expect_err("zero sample rows must be refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("fabricated entities from column names alone"),
            "{msg}"
        );
        assert!(msg.contains("2 columns"), "{msg}");
    }

    /// Custom prompt rules are checked at the production extraction dispatch,
    /// before an LLM request can carry a label with no canonical IRI.
    #[tokio::test]
    async fn extraction_dispatch_refuses_unresolved_mapping_labels_before_llm_call() {
        let schema = SchemaAnalysis {
            columns: vec!["Composition".into()],
            detected_types: vec!["string".into()],
        };
        let mapping: crate::mapping::OntologyMapping = serde_yaml::from_str(
            r#"
entity_rules:
  - column_pattern: "Composition"
    entity_type: ImaginaryClass
"#,
        )
        .unwrap();
        let constructor = LlmOntologyConstructor::new(LlmConfig::default());

        let error = constructor
            .extract_entities_with_mapping(
                &crate::ontologies::EmmoOntology,
                &schema,
                &[vec!["Ti-6Al-4V".into()]],
                Some(&mapping),
            )
            .await
            .expect_err("an unresolved prompt label must be refused before dispatch");
        let message = format!("{error:#}");
        assert!(
            message.contains("unknown entity type \"ImaginaryClass\""),
            "{message}"
        );
    }

    /// `LLM_NO_THINK` is the documented spellings only — "1"/"true" (any
    /// case) enable, everything else (including absence) stays off, because
    /// the kwarg it adds is a vendor extension OpenAI rejects with 400.
    /// This is the only test in the workspace touching this env var; it
    /// restores the prior state either way.
    #[test]
    fn extraction_no_think_reads_the_documented_spellings_only() {
        let prior = std::env::var_os("LLM_NO_THINK");
        // SAFETY: single test-threaded mutation of a var only this test and
        // the production reader consult; restored below.
        unsafe { std::env::remove_var("LLM_NO_THINK") };
        assert!(!extraction_no_think(), "absent must mean OFF");
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("0", false),
            ("false", false),
            ("yes", false),
            ("", false),
        ] {
            unsafe { std::env::set_var("LLM_NO_THINK", value) };
            assert_eq!(extraction_no_think(), expected, "value {value:?}");
        }
        match prior {
            Some(value) => unsafe { std::env::set_var("LLM_NO_THINK", value) },
            None => unsafe { std::env::remove_var("LLM_NO_THINK") },
        }
    }

    #[test]
    fn raw_entity_null_properties_defaults_to_null_value() {
        // When "properties" key is absent, serde default gives Value::Null.
        let json = r#"{"type": "Element", "name": "W"}"#;
        let entity: RawEntity = serde_json::from_str(json).unwrap();
        assert_eq!(entity.entity_type, "Element");
        assert!(entity.properties.is_null());
    }

    #[test]
    fn raw_entity_explicit_null_properties() {
        let json = r#"{"type": "Element", "name": "Mo", "properties": null}"#;
        let entity: RawEntity = serde_json::from_str(json).unwrap();
        assert!(entity.properties.is_null());
    }

    // --- normalise_extracted_name: the extraction-boundary name contract ---

    #[test]
    fn normalisation_strips_balanced_surrounding_quotes_and_whitespace() {
        for (raw, want) in [
            ("\"Ti\"", "Ti"),
            ("'Al'", "Al"),
            ("\u{201C}316L\u{201D}", "316L"),
            ("\u{2018}LPBF\u{2019}", "LPBF"),
            ("  \"Ti-6Al-4V\"  ", "Ti-6Al-4V"),
            ("\" Ti \"", "Ti"),             // whitespace inside the quotes
            ("\"'Ti'\"", "Ti"),             // nested layers unwrap fully
            ("'\u{201C}Fe\u{201D}'", "Fe"), // mixed nesting too
            ("  bare name  ", "bare name"), // no quotes: trim only
        ] {
            assert_eq!(normalise_extracted_name(raw), want, "raw: {raw:?}");
        }
    }

    #[test]
    fn normalisation_preserves_interior_and_unbalanced_quotes() {
        for keep in [
            "6\" pipe",          // interior ASCII quote is data
            "Ni-\"free\" steel", // interior pair is data
            "d'Arcy alloy",      // interior apostrophe is data
            "\"Ti",              // leading only — unbalanced
            "Ti\"",              // trailing only — unbalanced
            "\u{201C}Ti\"",      // mismatched ends stay
            "\"",                // a single quote char is not a pair
        ] {
            assert_eq!(
                normalise_extracted_name(keep),
                keep,
                "must survive: {keep:?}"
            );
        }
        // A balanced OUTER pair comes off; the interior quote survives.
        assert_eq!(normalise_extracted_name("\"6\" pipe\""), "6\" pipe");
    }

    /// The brief's pinned property: normalise(normalise(x)) == normalise(x).
    #[test]
    fn normalisation_is_idempotent() {
        for raw in [
            "\"Ti\"",
            "'Al'",
            "\u{201C}316L\u{201D}",
            "\"'Ti'\"",
            "6\" pipe",
            "Ni-\"free\" steel",
            "\"Ti",
            "\u{201C}Ti\"",
            "\"6\" pipe\"",
            "\"\"",
            "''",
            "  spaced  ",
            "",
            "\"",
        ] {
            let once = normalise_extracted_name(raw);
            assert_eq!(
                normalise_extracted_name(&once),
                once,
                "not idempotent for raw: {raw:?}"
            );
        }
    }

    /// Quote-only and whitespace-only names normalise to empty — the value
    /// is passed through, and the REJECTION happens downstream in the
    /// graph-write plan (dropped + reported via `dropped_entities`), pinned
    /// at production dispatch by
    /// `pipeline::tests::empty_after_normalisation_names_are_dropped_and_reported`.
    #[test]
    fn normalisation_can_empty_a_name() {
        for raw in ["\"\"", "''", "   ", "\u{201C}\u{201D}", "\"  \"", "\"''\""] {
            assert_eq!(normalise_extracted_name(raw), "", "raw: {raw:?}");
        }
    }
}
