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
    config: LlmConfig,
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

impl LlmOntologyConstructor {
    pub fn new(config: LlmConfig) -> Self {
        let client = crate::llm::LlmClient::new(config.clone());
        Self { client, config }
    }

    /// Check that the LLM backend is reachable.
    pub async fn health_check(&self) -> Result<()> {
        self.client.health_check().await
    }

    /// Call the LLM with a prompt and expect JSON output.
    async fn generate(&self, prompt: &str) -> Result<String> {
        self.client.generate_json(prompt).await
    }

    /// Embed a single text string. Returns the embedding vector.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        self.client.embed_text(text).await
    }

    /// Batch embedding.
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.client.embed(texts).await
    }

    fn build_extraction_prompt_with_mapping(
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
        mapping: Option<&crate::mapping::OntologyMapping>,
    ) -> String {
        let mut prompt = String::with_capacity(2048);
        prompt.push_str(
            "You are a materials science data analyst. Given a dataset schema and sample rows, \
             extract all entities and relationships into a structured JSON format.\n\n",
        );

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

        prompt.push_str(
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
             Return ONLY valid JSON with this structure:\n\
             {\n\
               \"entities\": [{\"type\": \"...\", \"name\": \"...\", \"properties\": {...}}],\n\
               \"relationships\": [{\"from\": \"...\", \"rel\": \"...\", \"to\": \"...\", \"weight\": null, \"order\": null}]\n\
             }\n",
        );

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

/// Convenience method: extract entities with explicit sample rows (bypasses DataSource).
impl LlmOntologyConstructor {
    pub async fn extract_entities_with_samples(
        &self,
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
    ) -> Result<EntitySet> {
        self.extract_entities_with_mapping(schema, sample_rows, None)
            .await
    }

    pub async fn extract_entities_with_mapping(
        &self,
        schema: &SchemaAnalysis,
        sample_rows: &[Vec<String>],
        mapping: Option<&crate::mapping::OntologyMapping>,
    ) -> Result<EntitySet> {
        let max_rows = self.config.max_sample_rows;
        let rows = if sample_rows.len() > max_rows {
            &sample_rows[..max_rows]
        } else {
            sample_rows
        };

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

        let prompt = Self::build_extraction_prompt_with_mapping(schema, rows, mapping);
        let response = self.generate(&prompt).await?;

        let raw: ExtractionOutput =
            serde_json::from_str(&response).context("LLM returned invalid extraction JSON")?;

        let mut entities: Vec<Entity> = raw
            .entities
            .into_iter()
            .map(|e| Entity {
                entity_type: e.entity_type,
                name: e.name,
                properties: if e.properties.is_null() {
                    serde_json::Value::Object(Default::default())
                } else {
                    e.properties
                },
            })
            .collect();

        let mut relationships: Vec<Relationship> = raw
            .relationships
            .into_iter()
            .map(|r| Relationship {
                from: r.from,
                rel_type: r.rel,
                to: r.to,
                weight: r.weight,
                order: r.order,
            })
            .collect();

        // Apply alias expansion (e.g. "Nb" -> "Niobium") to entity names and
        // to every relationship endpoint referencing those names, so the two
        // stay consistent — expanding only one side would silently turn a
        // real relationship into an orphan (AUDIT_BACKLOG 21 / INGESTION_AUDIT
        // #21 — `expand_alias` had zero production callers).
        if let Some(m) = mapping {
            for entity in &mut entities {
                entity.name = m.expand_alias(&entity.name);
            }
            for rel in &mut relationships {
                rel.from = m.expand_alias(&rel.from);
                rel.to = m.expand_alias(&rel.to);
            }
        }

        Ok(EntitySet {
            entities,
            relationships,
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
        let prompt =
            LlmOntologyConstructor::build_extraction_prompt_with_mapping(&schema, &rows, None);

        assert!(prompt.contains("Composition (string)"));
        assert!(prompt.contains("Hardness_HV (float)"));
        assert!(prompt.contains("Nb25Mo25Ta25W25"));
        assert!(prompt.contains("CONTAINS"));
        assert!(prompt.contains("PROCESSED_BY"));
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
                {"from": "NbMoTaW", "rel": "CONTAINS", "to": "Nb", "weight": 0.25}
            ]
        }"#;
        let parsed: ExtractionOutput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.entities.len(), 2);
        assert_eq!(parsed.relationships[0].weight, Some(0.25));
    }

    // --- build_extraction_prompt edge cases ---

    #[test]
    fn build_extraction_prompt_with_empty_sample_rows() {
        let schema = SchemaAnalysis {
            columns: vec!["Composition".into()],
            detected_types: vec!["string".into()],
        };
        let prompt =
            LlmOntologyConstructor::build_extraction_prompt_with_mapping(&schema, &[], None);
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
        let prompt =
            LlmOntologyConstructor::build_extraction_prompt_with_mapping(&schema, &[], None);
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
        let prompt =
            LlmOntologyConstructor::build_extraction_prompt_with_mapping(&schema, &[], None);
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
}
