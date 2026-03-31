//! LLM-driven ontology construction — provider-agnostic.
//!
//! Sends schema + sample rows to any LLM backend (Ollama, OpenAI, MARC27, vLLM),
//! parses the structured JSON entity/relationship output, and returns typed results
//! ready for Neo4j graph construction and Qdrant embedding storage.

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
    #[serde(default)]
    weight: Option<f64>,
    #[serde(default)]
    order: Option<u32>,
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

    /// Build the extraction prompt from schema + sample rows.
    fn build_extraction_prompt(schema: &SchemaAnalysis, sample_rows: &[Vec<String>]) -> String {
        Self::build_extraction_prompt_with_mapping(schema, sample_rows, None)
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
        // In a real pipeline, sample rows come from the DataFrame via the connector.
        // For now we pass an empty sample — callers should use extract_entities_with_samples.
        tracing::info!(
            columns = schema.columns.len(),
            "extracting entities via LLM"
        );

        let prompt = Self::build_extraction_prompt(schema, &[]);
        let response = self.generate(&prompt).await?;

        let raw: ExtractionOutput =
            serde_json::from_str(&response).context("LLM returned invalid extraction JSON")?;

        let entities = raw
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

        let relationships = raw
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

        Ok(EntitySet {
            entities,
            relationships,
        })
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

        tracing::info!(
            columns = schema.columns.len(),
            sample_rows = rows.len(),
            "extracting entities via LLM with samples"
        );

        let prompt = Self::build_extraction_prompt_with_mapping(schema, rows, mapping);
        let response = self.generate(&prompt).await?;

        let raw: ExtractionOutput =
            serde_json::from_str(&response).context("LLM returned invalid extraction JSON")?;

        let entities = raw
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

        let relationships = raw
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
        let prompt = LlmOntologyConstructor::build_extraction_prompt(&schema, &rows);

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
    fn default_config_points_to_ollama() {
        let cfg = LlmConfig::default();
        assert!(cfg.base_url.contains("11434"));
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
        let prompt = LlmOntologyConstructor::build_extraction_prompt(&schema, &[]);
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
        let prompt = LlmOntologyConstructor::build_extraction_prompt(&schema, &[]);
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
        let prompt = LlmOntologyConstructor::build_extraction_prompt(&schema, &[]);
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
