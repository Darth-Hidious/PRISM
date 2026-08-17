//! The extraction JSON schema, derived from the ACTIVE ontology.
//!
//! Entity and relationship enums come from the same active-ontology
//! declarations the prompt and validator read. Unit terms remain open strings:
//! the active ontology and model own their vocabulary, not a Rust enum.
//!
//! What the schema deliberately does NOT lock: entity `properties` stays an
//! open object (extra keys as primitive values) because compositions and
//! process parameters are legitimately free-form; only its `value` and
//! `unit` members are typed. The one field-use rule the schema DOES enforce
//! is per-type: the active ontology's quantitative classes
//! ([`Ontology::quantitative_labels`]) get a `oneOf` variant REQUIRING the
//! `value` member (nullable — the authoritative home for a
//! measured number is the per-edge relationship channel), because "the
//! vocabulary of each field" was never enough — the model satisfied the
//! enum-locked schema by putting a numeric claim in an entity name with an
//! empty properties bag. A grammar only constrains form; semantic
//! interpretation remains the ontology-aware reader's job.
//!
//! Interop note: `additionalProperties` as a typed sub-schema is what
//! llama-server and vLLM's converters accept; OpenAI's `strict` mode
//! rejects it (it requires `additionalProperties: false` throughout), which
//! surfaces as an honest degradation to prompt-guided JSON via
//! [`prism_llm::LlmClient::generate_json_with_schema`] — reported, never
//! silent.

use prism_llm::JsonSchemaSpec;

use crate::ontologies::Ontology;

/// Build the tabular-extraction JSON schema for the ACTIVE ontology. The
/// shape mirrors the wire format `crate::ontology`'s parser expects
/// (`entities` / `relationships`, `rel` for the relationship type,
/// string-tolerant `weight`/`order`, and optional bounded relationship
/// `confidence`).
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
    // One entity-item builder for both variants below: `types` locks the
    // `type` enum, `required_members` is what the `properties` object must
    // carry. The value/unit member schemas and the primitive-extras rule
    // are IDENTICAL across variants — only the requirement differs.
    let entity_item = |types: &[&str], required_members: &[&str]| {
        serde_json::json!({
            "type": "object",
            "properties": {
                "type": {"type": "string", "enum": types},
                "name": {"type": "string"},
                "properties": {
                    "type": "object",
                    "properties": {
                        "value": {"type": ["number", "string", "null"]},
                        "unit": {"anyOf": [
                            {"type": "string", "minLength": 1},
                            {"type": "null"}
                        ]},
                    },
                    "required": required_members,
                    // Free-form extras (composition systems, process params)
                    // stay allowed — as primitives, so `unit`/`value` cannot be
                    // smuggled in as nested structure.
                    "additionalProperties": {"type": ["string", "number", "boolean", "null"]},
                },
            },
            "required": ["type", "name", "properties"],
            "additionalProperties": false,
        })
    };

    // Per-type variants — the field-use half the enum lock alone cannot
    // give. The active ontology's quantitative classes (its declaration,
    // `Ontology::quantitative_labels`) REQUIRE the `value` member: the model
    // can no longer satisfy the schema by naming a
    // Property "1100 MPa" with an empty properties bag (measured live
    // 2026-08-08 — value and unit stored as text inside the entity NAME,
    // nothing queryable as a number). Both members stay NULLABLE: the
    // authoritative channel for a measured number is the per-edge
    // relationship `value`/`unit` below (a value on a SHARED property node
    // attributes to nobody — live 2026-08-10: one node's 880 was stored as
    // five alloys' yield strength), so a shared property node must be able
    // to say `null` honestly. Everything else keeps the historical open shape, so an
    // ontology with no quantitative classes emits a byte-identical schema.
    let quantitative: Vec<&str> = ontology.quantitative_labels();
    let other_types: Vec<&str> = entity_types
        .iter()
        .copied()
        .filter(|t| !quantitative.contains(t))
        .collect();
    let entity_items = if quantitative.is_empty() {
        entity_item(&entity_types, &[])
    } else {
        let quantitative_item = entity_item(&quantitative, &["value"]);
        if other_types.is_empty() {
            quantitative_item
        } else {
            serde_json::json!({"oneOf": [
                quantitative_item,
                entity_item(&other_types, &[]),
            ]})
        }
    };

    // An ontology may legitimately declare zero relations; an empty `enum`
    // is invalid JSON Schema, so lock the array shut instead of guessing.
    //
    // Relationship variants are split BY RELATION: the declared measurement
    // relations (`Ontology::measurement_relations`) become two dedicated
    // variants — a measured edge whose typed `value` is required, whose
    // optional `unit` is either null or a non-empty exact term, and which has
    // NO `weight`/`order` members at all, plus a
    // bare property link with endpoints plus optional confidence — while
    // every other relation keeps its historical data fields (weight/order,
    // no value/unit) plus optional confidence. Each
    // exclusion is a measured escape hatch, closed: with `weight` available
    // on the same edge a model can put every
    // number THERE and satisfied the grammar without ever entering the
    // measured variant (run 3 — all ten values silently unmappable). A
    // measured quantity on an edge has exactly one domain-value channel. Unit
    // absence is representable without a semantic verdict; a supplied term
    // must be non-empty. The separately named probability field is bounded.
    let measurement_rels: Vec<&str> = ontology.measurement_relations();
    let plain_rels: Vec<&str> = relationship_types
        .iter()
        .copied()
        .filter(|r| !measurement_rels.contains(r))
        .collect();
    let plain_edge = |rels: &[&str]| {
        serde_json::json!({
            "type": "object",
            "properties": {
                "from": {"type": "string"},
                "rel": {"type": "string", "enum": rels},
                "to": {"type": "string"},
                // The parser is deliberately lenient here ("balance",
                // "19.0") — the schema must not be stricter than what
                // the pipeline accepts, or it would forbid domain-real
                // values like a remainder fraction.
                "weight": {"type": ["number", "string", "null"]},
                "order": {"type": ["integer", "string", "null"]},
                // Optional model judgement, never a required invented score.
                // Degraded prompt-only endpoints may still return numeric
                // strings; the raw parser accepts those, but constrained
                // decoding emits an actual probability or null.
                "confidence": {"anyOf": [
                    {"type": "number", "minimum": 0.0, "maximum": 1.0},
                    {"type": "null"}
                ]},
            },
            "required": ["from", "rel", "to"],
            "additionalProperties": false,
        })
    };
    let relationship_items = if relationship_types.is_empty() {
        serde_json::json!({"type": "array", "maxItems": 0, "items": {"type": "object"}})
    } else if measurement_rels.is_empty() {
        serde_json::json!({"type": "array", "items": plain_edge(&relationship_types)})
    } else {
        let measured_edge = serde_json::json!({
            "type": "object",
            "properties": {
                "from": {"type": "string"},
                "rel": {"type": "string", "enum": measurement_rels},
                "to": {"type": "string"},
                "value": {"type": ["number", "string"]},
                "unit": {"anyOf": [
                    {"type": "string", "minLength": 1},
                    {"type": "null"}
                ]},
                "confidence": {"anyOf": [
                    {"type": "number", "minimum": 0.0, "maximum": 1.0},
                    {"type": "null"}
                ]},
            },
            "required": ["from", "rel", "to", "value"],
            "additionalProperties": false,
        });
        let bare_property_link = serde_json::json!({
            "type": "object",
            "properties": {
                "from": {"type": "string"},
                "rel": {"type": "string", "enum": measurement_rels},
                "to": {"type": "string"},
                "confidence": {"anyOf": [
                    {"type": "number", "minimum": 0.0, "maximum": 1.0},
                    {"type": "null"}
                ]},
            },
            "required": ["from", "rel", "to"],
            "additionalProperties": false,
        });
        let mut variants = vec![measured_edge, bare_property_link];
        if !plain_rels.is_empty() {
            variants.push(plain_edge(&plain_rels));
        }
        serde_json::json!({"type": "array", "items": {"oneOf": variants}})
    };

    JsonSchemaSpec {
        name: format!("{}_tabular_extraction", ontology.id()),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "entities": {"type": "array", "items": entity_items},
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
                        parents: Vec::new(),
                        domains: Vec::new(),
                        ranges: Vec::new(),
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

    /// Every entity-type enum in the schema, across whichever item shape the
    /// ontology produced: the single historical item, or the per-type
    /// `oneOf` variants a quantitative declaration adds.
    fn entity_type_enums(schema: &serde_json::Value) -> Vec<Vec<String>> {
        let items = "/properties/entities/items";
        if let Some(variants) = schema
            .pointer(&format!("{items}/oneOf"))
            .and_then(|v| v.as_array())
        {
            (0..variants.len())
                .map(|i| enum_values(schema, &format!("{items}/oneOf/{i}/properties/type/enum")))
                .collect()
        } else {
            vec![enum_values(
                schema,
                &format!("{items}/properties/type/enum"),
            )]
        }
    }

    /// The schema is the DECLARATION, not a frozen list: every extraction
    /// label the active ontology declares appears in exactly ONE entity-type
    /// enum, and nothing else does. Run against two ontologies with disjoint
    /// vocabularies so a hardcoded derivation cannot satisfy both.
    #[test]
    fn schema_enums_are_exactly_the_active_ontologys_declarations() {
        let chem = Chem::new(&["REACTS_WITH", "CATALYZED_BY"]);
        for ontology in [&chem as &dyn Ontology, &EmmoOntology] {
            let spec = extraction_json_schema(ontology);
            let variant_enums = entity_type_enums(&spec.schema);
            let mut entity_enum: Vec<String> = variant_enums.iter().flatten().cloned().collect();
            let unique: std::collections::HashSet<&String> = entity_enum.iter().collect();
            assert_eq!(
                unique.len(),
                entity_enum.len(),
                "a label appears in more than one variant for ontology '{}': {variant_enums:?}",
                ontology.id()
            );
            let mut declared: Vec<String> = ontology
                .classes()
                .iter()
                .flat_map(|d| d.extraction_labels.iter().cloned())
                .collect();
            entity_enum.sort();
            declared.sort();
            assert_eq!(
                entity_enum,
                declared,
                "entity enums drifted from ontology '{}'",
                ontology.id()
            );

            let declared_rels: std::collections::BTreeSet<String> = ontology
                .relations()
                .iter()
                .flat_map(|d| d.extraction_labels.iter().cloned())
                .collect();
            // Across however many edge variants the ontology produced
            // (single historical shape, or measured/bare/plain), the UNION
            // of rel enums is exactly the declared relationship vocabulary.
            let items = "/properties/relationships/items";
            let variant_count = spec
                .schema
                .pointer(&format!("{items}/oneOf"))
                .and_then(|v| v.as_array())
                .map(Vec::len);
            let rel_union: std::collections::BTreeSet<String> = match variant_count {
                Some(n) => (0..n)
                    .flat_map(|i| {
                        enum_values(
                            &spec.schema,
                            &format!("{items}/oneOf/{i}/properties/rel/enum"),
                        )
                    })
                    .collect(),
                None => enum_values(&spec.schema, &format!("{items}/properties/rel/enum"))
                    .into_iter()
                    .collect(),
            };
            assert_eq!(
                rel_union,
                declared_rels,
                "relationship enums drifted from ontology '{}'",
                ontology.id()
            );
            assert_eq!(spec.name, format!("{}_tabular_extraction", ontology.id()));
        }
    }

    #[test]
    fn unit_terms_are_nonempty_and_vocabulary_neutral() {
        // CONTRACT CHANGE: the schema used to enumerate a Rust-owned unit
        // vocabulary. Both entity variants now admit any non-empty term so a
        // promoted customer ontology needs no Rust edit.
        let spec = extraction_json_schema(&EmmoOntology);
        for variant in 0..2 {
            let unit = spec
                .schema
                .pointer(&format!(
                    "/properties/entities/items/oneOf/{variant}\
                     /properties/properties/properties/unit/anyOf/0"
                ))
                .expect("entity unit string schema");
            assert_eq!(unit["type"], "string", "variant {variant}");
            assert_eq!(unit["minLength"], 1, "variant {variant}");
            assert!(unit.get("enum").is_none(), "variant {variant}: {unit}");
        }
    }

    /// THE per-type contract, pinned at the production schema builder: the
    /// quantitative variant's `type` enum is exactly the active ontology's
    /// quantitative declaration, its `properties` requires `value`, and its
    /// optional unit stays vocabulary-neutral — while the open variant
    /// keeps the historical no-requirement shape and excludes the
    /// quantitative labels. Dropping the requirement (the mutation that
    /// re-opens "Property named 1100 MPa with an empty bag") kills this.
    #[test]
    fn quantitative_types_require_value_but_leave_unit_absence_neutral() {
        // CONTRACT CHANGE: an omitted/null unit is semantically neutral, so
        // only the value field is required on this structural variant.
        let spec = extraction_json_schema(&EmmoOntology);
        let quant = spec
            .schema
            .pointer("/properties/entities/items/oneOf/0")
            .expect("EMMO declares quantitative classes, so the items are per-type variants");

        assert_eq!(
            enum_values(quant, "/properties/type/enum"),
            EmmoOntology.quantitative_labels(),
            "the quantitative variant's enum is the declaration"
        );
        let required: Vec<String> = quant
            .pointer("/properties/properties/required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            required,
            ["value"],
            "a quantitative entity must carry value without requiring a unit"
        );
        // Both members stay NULLABLE: the per-edge relationship channel is
        // the authoritative home for a measured number (a shared property
        // node must be able to say null honestly).
        assert_eq!(
            quant.pointer("/properties/properties/properties/value/type"),
            Some(&serde_json::json!(["number", "string", "null"])),
        );

        let open = spec
            .schema
            .pointer("/properties/entities/items/oneOf/1")
            .expect("the non-quantitative variant");
        let open_enum = enum_values(open, "/properties/type/enum");
        assert!(
            !open_enum.iter().any(|t| t == "Property"),
            "the open variant must exclude the quantitative labels: {open_enum:?}"
        );
        assert_eq!(
            open.pointer("/properties/properties/required"),
            Some(&serde_json::json!([])),
            "the open variant keeps the historical no-requirement shape"
        );
    }

    /// The per-edge measurement channel is on the wire: the measured-edge
    /// variant carries the declared measurement relations and requires a
    /// non-null `value`; an absent/null unit is neutral, while a supplied
    /// term must be non-empty.
    /// neither the measured edge nor the bare property link declares
    /// `weight`/`order` at all (with `weight` available, the same model put
    /// every number there and never entered the measured variant — run 3);
    /// and ordinary relations keep the historical weight/order data fields
    /// with no value/unit. All variants additionally expose the independently
    /// tested optional bounded confidence field. Weakening any of these is
    /// the mutation this test exists to kill.
    #[test]
    fn measured_edges_keep_units_optional_and_vocabulary_neutral() {
        // CONTRACT CHANGE: a measured value no longer universally requires a
        // unit. Present terms are structurally non-empty and never enumerated.
        let spec = extraction_json_schema(&EmmoOntology);
        let items = spec
            .schema
            .pointer("/properties/relationships/items")
            .expect("relationship items present");

        let measured = items.pointer("/oneOf/0").expect("measured-edge variant");
        assert_eq!(
            enum_values(measured, "/properties/rel/enum"),
            EmmoOntology.measurement_relations(),
            "the measured edge carries the declared measurement relations only"
        );
        assert_eq!(
            measured.pointer("/required"),
            Some(&serde_json::json!(["from", "rel", "to", "value"])),
            "the schema must not turn unit absence into a semantic verdict"
        );
        assert_eq!(
            measured.pointer("/properties/value/type"),
            Some(&serde_json::json!(["number", "string"])),
            "a measured value admits no null"
        );
        let unit = measured
            .pointer("/properties/unit/anyOf/0")
            .expect("measured unit string schema");
        assert_eq!(unit["type"], "string");
        assert_eq!(unit["minLength"], 1);
        assert!(unit.get("enum").is_none(), "{unit}");
        assert_eq!(
            measured.pointer("/properties/unit/anyOf/1/type"),
            Some(&serde_json::json!("null"))
        );
        assert!(
            measured.pointer("/properties/weight").is_none()
                && measured.pointer("/properties/order").is_none(),
            "a measured edge must offer NO unitless numeric slot"
        );

        let bare = items.pointer("/oneOf/1").expect("bare property link");
        assert_eq!(
            enum_values(bare, "/properties/rel/enum"),
            EmmoOntology.measurement_relations(),
        );
        for member in ["value", "unit", "weight", "order"] {
            assert!(
                bare.pointer(&format!("/properties/{member}")).is_none(),
                "the bare property link must not declare {member}"
            );
        }

        let plain = items.pointer("/oneOf/2").expect("ordinary-edge variant");
        let plain_enum = enum_values(plain, "/properties/rel/enum");
        assert!(
            plain_enum.iter().any(|r| r == "CONTAINS")
                && !plain_enum.iter().any(|r| r == "HAS_PROPERTY"),
            "ordinary edges carry everything but the measurement relations: {plain_enum:?}"
        );
        assert!(
            plain.pointer("/properties/weight").is_some()
                && plain.pointer("/properties/value").is_none(),
            "ordinary edges keep weight/order and gain no value channel"
        );

        // An ontology declaring no measurement relations keeps ONE
        // historical data-field shape — no variants, no value/unit anywhere;
        // the universal optional confidence member is independently tested.
        let chem = extraction_json_schema(&Chem::new(&["REACTS_WITH"]));
        let chem_items = chem
            .schema
            .pointer("/properties/relationships/items")
            .expect("chem relationship items");
        assert!(chem_items.pointer("/oneOf").is_none());
        assert!(chem_items.pointer("/properties/value").is_none());
        assert!(chem_items.pointer("/properties/weight").is_some());
    }

    #[test]
    fn relationship_confidence_is_optional_and_bounded_on_every_edge_shape() {
        fn assert_confidence_contract(edge: &serde_json::Value) {
            let confidence = edge
                .pointer("/properties/confidence")
                .expect("every relationship shape must expose confidence");
            assert_eq!(
                confidence.pointer("/anyOf/0/type"),
                Some(&serde_json::json!("number"))
            );
            assert_eq!(
                confidence.pointer("/anyOf/0/minimum"),
                Some(&serde_json::json!(0.0))
            );
            assert_eq!(
                confidence.pointer("/anyOf/0/maximum"),
                Some(&serde_json::json!(1.0))
            );
            assert_eq!(
                confidence.pointer("/anyOf/1/type"),
                Some(&serde_json::json!("null"))
            );
            let required = edge
                .pointer("/required")
                .and_then(serde_json::Value::as_array)
                .expect("relationship required list");
            assert!(
                !required.iter().any(|member| member == "confidence"),
                "confidence must stay optional: {edge}"
            );
        }

        let emmo = extraction_json_schema(&EmmoOntology);
        let variants = emmo
            .schema
            .pointer("/properties/relationships/items/oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("EMMO relationship variants");
        assert_eq!(variants.len(), 3);
        for variant in variants {
            assert_confidence_contract(variant);
        }

        let chem = extraction_json_schema(&Chem::new(&["REACTS_WITH"]));
        let plain = chem
            .schema
            .pointer("/properties/relationships/items")
            .expect("single ordinary relationship shape");
        assert_confidence_contract(plain);
    }

    /// An ontology declaring NO quantitative classes keeps the single item
    /// shape byte-for-byte — no `oneOf`, no requirement — so nothing changes
    /// for MatKG, induced ontologies, or any third-party vocabulary that
    /// never opted in.
    #[test]
    fn no_quantitative_declaration_keeps_the_single_open_item() {
        let spec = extraction_json_schema(&Chem::new(&["REACTS_WITH"]));
        assert!(
            spec.schema
                .pointer("/properties/entities/items/oneOf")
                .is_none(),
            "no quantitative declaration must mean no per-type variants"
        );
        assert_eq!(
            spec.schema
                .pointer("/properties/entities/items/properties/properties/required"),
            Some(&serde_json::json!([])),
            "and no requirement on the open bag"
        );
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
