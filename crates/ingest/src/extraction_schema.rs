//! The extraction JSON schema, derived from the ACTIVE ontology.
//!
//! Everything enum-locked here comes from the same declarations the prompt
//! and the validator already read: entity types and relationship types from
//! [`Ontology::classes`] / [`Ontology::relations`], the unit vocabulary from
//! [`crate::qudt_units::EXTRACTION_UNITS`]. With `response_format:
//! json_schema` the decoder becomes the third leg of the one-declaration
//! contract — the model is structurally incapable of emitting an undeclared
//! class, an undeclared relation, or a non-QUDT unit, instead of being asked
//! nicely in prose and repaired downstream.
//!
//! What the schema deliberately does NOT lock: entity `properties` stays an
//! open object (extra keys as primitive values) because compositions and
//! process parameters are legitimately free-form; only its `value` and
//! `unit` members are typed. And form is all a grammar can give — a density
//! carrying a legal-but-wrong pressure unit still decodes, which is why
//! [`crate::graph_validation`] checks quantity kinds after the fact.
//!
//! Interop note: `additionalProperties` as a typed sub-schema is what
//! llama-server and vLLM's converters accept; OpenAI's `strict` mode
//! rejects it (it requires `additionalProperties: false` throughout), which
//! surfaces as an honest degradation to prompt-guided JSON via
//! [`prism_llm::LlmClient::generate_json_with_schema`] — reported, never
//! silent.

use prism_llm::JsonSchemaSpec;

use crate::ontologies::Ontology;
use crate::qudt_units;

/// Build the tabular-extraction JSON schema for the ACTIVE ontology. The
/// shape mirrors the wire format `crate::ontology`'s parser expects
/// (`entities` / `relationships`, `rel` for the relationship type,
/// string-tolerant `weight`/`order`).
#[must_use]
pub fn extraction_json_schema(ontology: &dyn Ontology) -> JsonSchemaSpec {
    let entity_types: Vec<&str> = ontology
        .classes()
        .iter()
        .flat_map(|decl| decl.extraction_labels.iter())
        .map(String::as_str)
        .collect();
    let relationship_types: Vec<&str> = ontology
        .relations()
        .iter()
        .flat_map(|decl| decl.extraction_labels.iter())
        .map(String::as_str)
        .collect();
    let units: Vec<&str> = qudt_units::EXTRACTION_UNITS
        .iter()
        .map(|(id, _)| *id)
        .collect();

    let entity_item = serde_json::json!({
        "type": "object",
        "properties": {
            "type": {"type": "string", "enum": entity_types},
            "name": {"type": "string"},
            "properties": {
                "type": "object",
                "properties": {
                    "value": {"type": ["number", "string", "null"]},
                    "unit": {"anyOf": [
                        {"type": "string", "enum": units},
                        {"type": "null"}
                    ]},
                },
                // Free-form extras (composition systems, process params)
                // stay allowed — as primitives, so `unit`/`value` cannot be
                // smuggled in as nested structure.
                "additionalProperties": {"type": ["string", "number", "boolean", "null"]},
            },
        },
        "required": ["type", "name", "properties"],
        "additionalProperties": false,
    });

    // An ontology may legitimately declare zero relations; an empty `enum`
    // is invalid JSON Schema, so lock the array shut instead of guessing.
    let relationship_items = if relationship_types.is_empty() {
        serde_json::json!({"type": "array", "maxItems": 0, "items": {"type": "object"}})
    } else {
        serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "rel": {"type": "string", "enum": relationship_types},
                    "to": {"type": "string"},
                    // The parser is deliberately lenient here ("balance",
                    // "19.0") — the schema must not be stricter than what
                    // the pipeline accepts, or it would forbid domain-real
                    // values like a remainder fraction.
                    "weight": {"type": ["number", "string", "null"]},
                    "order": {"type": ["integer", "string", "null"]},
                },
                "required": ["from", "rel", "to"],
                "additionalProperties": false,
            },
        })
    };

    JsonSchemaSpec {
        name: format!("{}_tabular_extraction", ontology.id()),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "entities": {"type": "array", "items": entity_item},
                "relationships": relationship_items,
            },
            "required": ["entities", "relationships"],
            "additionalProperties": false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontologies::{ClassDecl, EmmoOntology, Iri, RelationDecl};

    /// A throwaway ontology whose vocabulary shares nothing with EMMO's —
    /// the fixture that makes "derived from the ACTIVE ontology" testable
    /// against a hardcoded-list mutation.
    struct Chem {
        classes: Vec<ClassDecl>,
        relations: Vec<RelationDecl>,
        version_iri: Iri,
    }

    impl Chem {
        fn new(relations: &[&str]) -> Self {
            Self {
                classes: vec![ClassDecl {
                    iri: Iri::new("https://example.test/chem#Molecule".to_string()).unwrap(),
                    pref_label: Some("Molecule".into()),
                    parents: Vec::new(),
                    extraction_labels: vec!["Molecule".into()],
                }],
                relations: relations
                    .iter()
                    .enumerate()
                    .map(|(index, label)| RelationDecl {
                        iri: Iri::new(format!("https://example.test/chem#rel{index}")).unwrap(),
                        pref_label: Some((*label).into()),
                        extraction_labels: vec![(*label).into()],
                    })
                    .collect(),
                version_iri: Iri::new("https://example.test/chem/1".to_string()).unwrap(),
            }
        }
    }

    impl Ontology for Chem {
        fn id(&self) -> &'static str {
            "chem-schema"
        }
        fn version_iri(&self) -> &Iri {
            &self.version_iri
        }
        fn artifact_sha256(&self) -> &str {
            "0000000000000000000000000000000000000000000000000000000000000000"
        }
        fn classes(&self) -> &[ClassDecl] {
            &self.classes
        }
        fn relations(&self) -> &[RelationDecl] {
            &self.relations
        }
        fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
            sub == sup
        }
    }

    fn enum_values(schema: &serde_json::Value, pointer: &str) -> Vec<String> {
        schema
            .pointer(pointer)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The schema is the DECLARATION, not a frozen list: every extraction
    /// label the active ontology declares appears in the matching enum, and
    /// nothing else does. Run against two ontologies with disjoint
    /// vocabularies so a hardcoded derivation cannot satisfy both.
    #[test]
    fn schema_enums_are_exactly_the_active_ontologys_declarations() {
        let chem = Chem::new(&["REACTS_WITH", "CATALYZED_BY"]);
        for ontology in [&chem as &dyn Ontology, &EmmoOntology] {
            let spec = extraction_json_schema(ontology);
            let entity_enum = enum_values(
                &spec.schema,
                "/properties/entities/items/properties/type/enum",
            );
            let declared: Vec<String> = ontology
                .classes()
                .iter()
                .flat_map(|d| d.extraction_labels.iter().cloned())
                .collect();
            assert_eq!(
                entity_enum,
                declared,
                "entity enum drifted from ontology '{}'",
                ontology.id()
            );

            let rel_enum = enum_values(
                &spec.schema,
                "/properties/relationships/items/properties/rel/enum",
            );
            let declared_rels: Vec<String> = ontology
                .relations()
                .iter()
                .flat_map(|d| d.extraction_labels.iter().cloned())
                .collect();
            assert_eq!(
                rel_enum,
                declared_rels,
                "relationship enum drifted from ontology '{}'",
                ontology.id()
            );
            assert_eq!(spec.name, format!("{}_tabular_extraction", ontology.id()));
        }
    }

    /// The unit enum is the ONE declared QUDT vocabulary — same table the
    /// quantity-kind validator reads, so decoder and validator cannot drift.
    #[test]
    fn unit_enum_is_the_shared_qudt_declaration() {
        let spec = extraction_json_schema(&EmmoOntology);
        let unit_enum = enum_values(
            &spec.schema,
            "/properties/entities/items/properties/properties/properties/unit/anyOf/0/enum",
        );
        let declared: Vec<String> = qudt_units::EXTRACTION_UNITS
            .iter()
            .map(|(id, _)| (*id).to_string())
            .collect();
        assert_eq!(unit_enum, declared);
        // And the identifiers the wild emits unconstrained are NOT legal.
        assert!(!unit_enum.iter().any(|u| u == "MPa" || u == "g/cm3"));
    }

    /// Zero declared relations must lock the array shut, not emit an
    /// invalid empty enum (which grammar conversion rejects wholesale).
    #[test]
    fn zero_relations_locks_the_relationships_array() {
        let spec = extraction_json_schema(&Chem::new(&[]));
        let rels = spec.schema.pointer("/properties/relationships").unwrap();
        assert_eq!(rels.get("maxItems").and_then(|v| v.as_u64()), Some(0));
        assert!(
            rels.pointer("/items/properties/rel/enum").is_none(),
            "an empty enum must never be emitted: {rels}"
        );
    }
}
