//! `EntitySet` → provenance fact mapping for the bundled Turso store.
//!
//! Bridges the LLM tabular extraction output (`Entity` / `Relationship`)
//! into the typed facts `prism_provenance::emmo` writes, replacing the
//! Neo4j upsert in the local pipeline (Neo4j retirement, step 1).

use prism_provenance::{
    EvidenceClass, FactPayload, LocalFact, MaterialFact, UnitTerm, VerificationStatus,
};

use crate::ontologies::Ontology;
use crate::{Entity, EntitySet};

/// Default evidence confidence for a tabular relationship whose extractor
/// confidence is missing or invalid.
///
/// `Relationship.weight` is never usable as confidence — it is a composition
/// fraction on `CONTAINS`. `0.8` preserves the historical
/// "LLM-extracted from structured data, unverified" prior. The raw absence
/// remains `None` on `Relationship`; this fallback is applied only while
/// building the write fact, so it cannot masquerade as a model judgement.
pub const DEFAULT_CONFIDENCE: f64 = 0.8;

/// Split a leading decimal number off a string: `"1100 source-unit"` →
/// `(1100.0, "source-unit")`, `"8.19customer:unit"` →
/// `(8.19, "customer:unit")`,
/// `"1100"` → `(1100.0, "")`.
/// `None` when the string does not START with a number (`"high"`,
/// `"MPa 1100"`). Deliberately simple — unsigned decimals only, no
/// exponents — because what it recognises must stay auditable: it feeds the
/// packed-VALUE parser below.
pub(crate) fn split_leading_number(s: &str) -> Option<(f64, &str)> {
    let numeric_end = s
        .bytes()
        .position(|b| !(b.is_ascii_digit() || b == b'.'))
        .unwrap_or(s.len());
    let (head, tail) = s.split_at(numeric_end);
    head.parse::<f64>().ok().map(|value| (value, tail.trim()))
}

/// The numeric measurement claim inside an extracted `value` member, if
/// any, with the unit spelling when the value string packed one inline:
///
/// - JSON number → `(v, None)`
/// - `"1100"` (a string that IS a number) → `(1100.0, None)`
/// - `"1100 source-unit"` (number and unit packed into one string — the same
///   form-versus-field failure the per-type schema exists to prevent, split
///   structurally) → `(1100.0, Some("source-unit"))`. The exact non-empty tail
///   is preserved; interpreting it belongs to the active ontology.
fn numeric_claim(value: &serde_json::Value) -> Option<(f64, Option<&str>)> {
    if let Some(v) = value.as_f64() {
        return Some((v, None));
    }
    let s = value.as_str()?.trim();
    if let Ok(v) = s.parse::<f64>() {
        return Some((v, None));
    }
    let (v, tail) = split_leading_number(s)?;
    (!tail.is_empty()).then_some((v, Some(tail)))
}

/// Map every extracted relationship to one [`MaterialFact`] in the shape
/// `prism_provenance::ProvenanceStore::write_fact` expects. WHICH edges
/// become typed facts is the ACTIVE ONTOLOGY's declaration, read through
/// the trait — never a literal matched in this file:
///
/// - [`Ontology::measurement_relations`] → `measurement` when a numeric
///   value is attributable to THIS subject. Two channels, in order:
///   the RELATIONSHIP's own `value`/`unit` (per-edge, attributable by
///   construction — the unit may also come from the target property node,
///   which legitimately carries the unit while each edge carries its
///   number); else the TARGET entity's freeform `value`/`unit` members (a
///   JSON number, a numeric string, or the packed `"1100 MPa"` form — see
///   [`numeric_claim`]), accepted ONLY when the target's entity type is one
///   of the ontology's [`Ontology::quantitative_labels`] AND exactly one
///   subject references the target — a value on a SHARED property node
///   attributes to nobody and is reported and stored for no one (the edge
///   stays generic). A supplied non-empty unit term is preserved exactly.
///   An absent term is semantically neutral; an explicitly blank term
///   receives a `UnitUnresolved` structural annotation. Neither case causes
///   a semantic drop. A target with no numeric value at all stays a generic
///   edge (kind `None`) so the property is not dropped.
/// - [`Ontology::phase_relations`] → `phase`.
/// - [`Ontology::processing_relations`] → `processing`, with the step order
///   in `value`.
/// - [`Ontology::contains_relations`] → `contains`, with the fraction in
///   `value`.
/// - Anything else → generic edge under its own predicate.
///
/// A numeric claim on an edge outside
/// [`Ontology::measurement_relations`] gets no measurement fact — and says
/// so in `dropped`, naming the ontology and the undeclared relation. When the
/// declaration is empty, the report says that directly. Silence or omission
/// from the declaration fails honestly; this mapper never falls back to a
/// frozen vocabulary.
///
/// Entities that appear in no relationship produce no facts. Returns
/// `(facts, dropped)` — one human-readable reason per dropped relationship,
/// same contract as the pipeline's `dropped_relationships`: NON-EMPTY is a
/// PARTIAL result the caller must surface, never a silent drop.
pub fn to_local_facts(
    entity_set: &EntitySet,
    ontology: &dyn Ontology,
) -> (Vec<MaterialFact>, Vec<String>) {
    map_local_facts(entity_set, ontology, Some(DEFAULT_CONFIDENCE))
}

/// Map the same write set while preserving missing extractor confidence as
/// `None` for semantic fusion. This shares the complete relationship mapper
/// with [`to_local_facts`], so duplicate endpoint triples with distinct
/// values/confidences remain positional peers and cannot inherit one
/// another's score. The returned facts are validation inputs only; the
/// persisted write still uses [`to_local_facts`] and its historical fallback.
pub(crate) fn to_semantic_local_facts(
    entity_set: &EntitySet,
    ontology: &dyn Ontology,
) -> (Vec<LocalFact>, Vec<String>) {
    let (facts, notes) = map_local_facts(entity_set, ontology, None);
    (
        facts.iter().map(FactPayload::to_local_fact).collect(),
        notes,
    )
}

fn map_local_facts(
    entity_set: &EntitySet,
    ontology: &dyn Ontology,
    missing_confidence: Option<f64>,
) -> (Vec<MaterialFact>, Vec<String>) {
    let by_name: std::collections::HashMap<&str, &Entity> = entity_set
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();

    // The ACTIVE ONTOLOGY's declaration decides which edges are typed.
    // Empty sets are an honest declaration of "none" — the mapper reports
    // what it cannot store as a result and never substitutes literals.
    let measurement: std::collections::HashSet<&str> =
        ontology.measurement_relations().into_iter().collect();
    let phase: std::collections::HashSet<&str> = ontology.phase_relations().into_iter().collect();
    let processing: std::collections::HashSet<&str> =
        ontology.processing_relations().into_iter().collect();
    let contains: std::collections::HashSet<&str> =
        ontology.contains_relations().into_iter().collect();
    let quantitative: std::collections::HashSet<&str> =
        ontology.quantitative_labels().into_iter().collect();

    // How many DISTINCT subjects state a declared measurement relation about
    // each target — the attribution gate for entity-level values: a value on
    // a property node referenced by several materials is one number claiming
    // to be all of their measurements (live 2026-08-10: one "yield strength"
    // node's 880 was stored as five different alloys' yield strength — four
    // falsehoods).
    let mut property_subjects: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        std::collections::HashMap::new();
    for rel in &entity_set.relationships {
        if measurement.contains(rel.rel_type.as_str()) {
            property_subjects
                .entry(rel.to.as_str())
                .or_default()
                .insert(rel.from.as_str());
        }
    }

    let mut facts = Vec::with_capacity(entity_set.relationships.len());
    let mut dropped = Vec::new();
    for rel in &entity_set.relationships {
        let confidence =
            crate::normalize_relationship_confidence(rel.confidence).or(missing_confidence);
        let (kind, value, unit, verification, verification_reason) =
            if measurement.contains(rel.rel_type.as_str()) {
                let target = by_name.get(rel.to.as_str());
                // The per-edge channel is authoritative: a value on the
                // RELATIONSHIP attributes to THIS subject by construction.
                // Unit precedence: the relationship's own unit, else the
                // target entity's declared unit (a property node
                // legitimately carries the unit while each edge carries its
                // number). A non-empty unit term is preserved exactly.
                if let Some(v) = rel.value {
                    let spelling = rel.unit.as_deref().or_else(|| {
                        target
                            .and_then(|e| e.properties.get("unit"))
                            .and_then(serde_json::Value::as_str)
                    });
                    let (unit, verification, reason) =
                        numeric_unit_term(spelling, &rel.from, &rel.to, &rel.rel_type, v);
                    (Some("measurement"), Some(v), unit, verification, reason)
                } else {
                    // Entity-level fallback: the value + unit in the TARGET
                    // entity's freeform properties JSON. Attributable only
                    // when the target is a declared QUANTITATIVE type and
                    // exactly one subject references it.
                    let claim = target
                        .and_then(|e| e.properties.get("value"))
                        .and_then(numeric_claim);
                    let target_is_quantitative =
                        target.is_some_and(|e| quantitative.contains(e.entity_type.as_str()));
                    match claim {
                        Some((v, _))
                            if property_subjects
                                .get(rel.to.as_str())
                                .is_some_and(|subjects| subjects.len() > 1) =>
                        {
                            let shared = property_subjects[rel.to.as_str()].len();
                            dropped.push(format!(
                                "{} -[{}]-> {}: value {v} sits on a property node \
                                 referenced by {shared} subjects, so it cannot be attributed \
                                 to this edge; the property link is retained without a value",
                                rel.from, rel.rel_type, rel.to
                            ));
                            (None, None, None, None, None)
                        }
                        Some((v, _)) if !target_is_quantitative => {
                            // A declared measurement edge whose target type the
                            // ontology does not declare quantitative has no
                            // attributable value channel. Reported, never
                            // guessed: the link survives without a value.
                            let entity_type =
                                target.map(|e| e.entity_type.as_str()).unwrap_or("<absent>");
                            dropped.push(format!(
                                "{} -[{}]-> {}: value {v} sits on entity type {entity_type:?}, \
                                 which ontology '{}' does not declare quantitative; the edge \
                                 is retained without a value",
                                rel.from,
                                rel.rel_type,
                                rel.to,
                                ontology.id()
                            ));
                            (None, None, None, None, None)
                        }
                        Some((v, inline)) => {
                            let declared = target
                                .and_then(|e| e.properties.get("unit"))
                                .and_then(serde_json::Value::as_str);
                            let selected = declared.or(inline);
                            let (unit, mut verification, mut reason) =
                                numeric_unit_term(selected, &rel.from, &rel.to, &rel.rel_type, v);
                            if let (Some(declared), Some(inline)) = (declared, inline)
                                && declared != inline
                            {
                                verification = Some(VerificationStatus::ModelAsserted);
                                reason = Some(format!(
                                    "{} -[{}]-> {}: the explicit unit term \
                                     {declared:?} differs from the inline term {inline:?}; \
                                     the explicit field was preserved without interpreting \
                                     either spelling",
                                    rel.from, rel.rel_type, rel.to
                                ));
                            }
                            (Some("measurement"), Some(v), unit, verification, reason)
                        }
                        None => (None, None, None, None, None),
                    }
                }
            } else if phase.contains(rel.rel_type.as_str()) {
                (Some("phase"), None, None, None, None)
            } else if processing.contains(rel.rel_type.as_str()) {
                (
                    Some("processing"),
                    rel.order.map(f64::from),
                    None,
                    None,
                    None,
                )
            } else if contains.contains(rel.rel_type.as_str()) {
                (Some("contains"), rel.weight, None, None, None)
            } else if let Some(v) = rel.value.filter(|v| v.is_finite())
                && rel.unit.as_deref().is_some_and(|u| !u.trim().is_empty())
            {
                // GROUNDING: a claim carrying BOTH a finite number and a unit is a
                // measurement, whatever the model called the relation.
                //
                // The chain above binds only when the predicate string is itself a
                // declared relation token. Extraction emits the PROPERTY NAME there
                // — "crack-growth resistance", "laser absorptivity" — so nothing
                // matched, and the branch below recorded the number as dropped:
                // measured live, 881 of 1047 edges carried free-text predicates
                // across 697 distinct relation types, and their values were lost.
                //
                // The predicate names the property; the RELATION is "has
                // measurement". Binding on the value+unit shape rather than on the
                // spelling is what the ontology can actually decide, and it infers
                // nothing about meaning: the predicate is preserved verbatim as the
                // property, and unit interpretation stays with `numeric_unit_term`.
                let (unit, verification, reason) =
                    numeric_unit_term(rel.unit.as_deref(), &rel.from, &rel.to, &rel.rel_type, v);
                (Some("measurement"), Some(v), unit, verification, reason)
            } else {
                // Generic edge under its own predicate. A claim with no unit has
                // nowhere typed to go unless THIS relation
                // edge or its target has nowhere typed to go unless THIS relation
                // is declared as measurement-carrying. Report every such loss,
                // whether the ontology declares zero measurement relations or a
                // different set; never discard a value silently or infer meaning
                // from the predicate spelling.
                let undeclared_reason = || {
                    if measurement.is_empty() {
                        format!(
                            "ontology '{}' declares no measurement relations",
                            ontology.id()
                        )
                    } else {
                        format!(
                            "relation '{}' is not declared measurement-carrying by ontology '{}'",
                            rel.rel_type,
                            ontology.id()
                        )
                    }
                };
                if let Some(v) = rel.value {
                    dropped.push(format!(
                        "{} -[{}]-> {}: value {v} was not stored as a measurement — {}",
                        rel.from,
                        rel.rel_type,
                        rel.to,
                        undeclared_reason()
                    ));
                } else if let Some((v, _)) = by_name
                    .get(rel.to.as_str())
                    .and_then(|e| e.properties.get("value"))
                    .and_then(numeric_claim)
                {
                    dropped.push(format!(
                        "{} -[{}]-> {}: value {v} on '{}' was not stored as a measurement — {}",
                        rel.from,
                        rel.rel_type,
                        rel.to,
                        rel.to,
                        undeclared_reason()
                    ));
                }
                (None, None, None, None, None)
            };
        facts.push(MaterialFact {
            subject: rel.from.clone(),
            predicate: rel.rel_type.clone(),
            object: rel.to.clone(),
            value,
            unit,
            conditions: Vec::new(),
            confidence,
            kind: kind.map(str::to_string),
            evidence_class: EvidenceClass::Research,
            verification,
            verification_reason,
        });
    }
    (facts, dropped)
}

fn numeric_unit_term(
    spelling: Option<&str>,
    subject: &str,
    object: &str,
    rel_type: &str,
    value: f64,
) -> (Option<UnitTerm>, Option<VerificationStatus>, Option<String>) {
    match spelling {
        None => (None, None, None),
        Some(spelling) => match UnitTerm::new(spelling.to_string()) {
            Ok(term) => (Some(term), None, None),
            Err(_) => (
                None,
                Some(VerificationStatus::UnitUnresolved),
                Some(format!(
                    "{subject} -[{rel_type}]-> {object}: numeric value {value} carried an explicitly blank unit term"
                )),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Relationship;
    use crate::ontologies::Iri;

    fn entity(entity_type: &str, name: &str, properties: serde_json::Value) -> Entity {
        Entity {
            entity_type: entity_type.into(),
            name: name.into(),
            properties,
        }
    }

    fn rel(from: &str, rel_type: &str, to: &str) -> Relationship {
        Relationship {
            from: from.into(),
            rel_type: rel_type.into(),
            to: to.into(),
            weight: None,
            order: None,
            value: None,
            unit: None,
            confidence: None,
        }
    }

    #[test]
    fn parsed_relationship_confidence_reaches_local_facts_and_absence_falls_back() {
        let set: EntitySet = serde_json::from_value(serde_json::json!({
            "entities": [],
            "relationships": [
                {"from": "A", "rel_type": "RELATED_TO", "to": "B", "confidence": 0.37},
                {"from": "C", "rel_type": "RELATED_TO", "to": "D"},
                {"from": "E", "rel_type": "RELATED_TO", "to": "F", "confidence": 1.7},
                {"from": "G", "rel_type": "RELATED_TO", "to": "H", "confidence": "unknown"}
            ]
        }))
        .unwrap();

        assert_eq!(set.relationships[0].confidence, Some(0.37));
        assert_eq!(set.relationships[1].confidence, None);
        assert_eq!(set.relationships[2].confidence, None);
        assert_eq!(set.relationships[3].confidence, None);

        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 4);
        assert_eq!(facts[0].confidence, Some(0.37));
        for fact in &facts[1..] {
            assert_eq!(
                fact.confidence,
                Some(DEFAULT_CONFIDENCE),
                "missing or invalid model confidence must use the explicit fallback: {fact:?}"
            );
        }
    }

    #[test]
    fn programmatically_invalid_confidence_also_falls_back_at_the_write_boundary() {
        let set = EntitySet {
            entities: vec![],
            relationships: vec![Relationship {
                confidence: Some(f64::NAN),
                ..rel("A", "RELATED_TO", "B")
            }],
        };

        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts[0].confidence, Some(DEFAULT_CONFIDENCE));
    }

    #[test]
    fn semantic_mapping_keeps_confidence_attached_to_duplicate_endpoint_assertions() {
        let set = EntitySet {
            entities: vec![],
            relationships: vec![
                Relationship {
                    weight: Some(0.10),
                    confidence: Some(0.21),
                    ..rel("alloy", "CONTAINS", "Ti")
                },
                Relationship {
                    weight: Some(0.90),
                    confidence: Some(0.87),
                    ..rel("alloy", "CONTAINS", "Ti")
                },
            ],
        };

        let (facts, dropped) = to_semantic_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 2);
        assert_eq!(
            (facts[0].value, facts[0].confidence),
            (Some(0.10), Some(0.21))
        );
        assert_eq!(
            (facts[1].value, facts[1].confidence),
            (Some(0.90), Some(0.87))
        );
    }

    /// THE anti-smearing contract, at the mapper: per-edge values attribute
    /// per subject (five alloys, five different numbers, one shared
    /// property node — each fact carries ITS alloy's number), the unit may
    /// live once on the property node, and a value sitting only on the
    /// SHARED node is stored for NO ONE — reported, edges kept generic.
    /// This is the exact live failure of 2026-08-10: one "yield strength"
    /// node's 880 was stored as five alloys' yield strength.
    #[test]
    fn per_edge_values_attribute_per_subject_and_shared_node_values_never_smear() {
        let shared_node_value = serde_json::json!({"value": 880.0, "unit": "MPa"});
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                entity("Alloy", "Inconel 718", serde_json::json!({})),
                // The property node: carries the UNIT (legitimate — one
                // unit, many edges) and row 1's value (unattributable).
                entity("Property", "yield strength", shared_node_value),
            ],
            relationships: vec![
                Relationship {
                    value: Some(880.0),
                    ..rel("Ti-6Al-4V", "HAS_PROPERTY", "yield strength")
                },
                Relationship {
                    value: Some(1100.0),
                    unit: Some("QUDT:MegaPA".into()),
                    ..rel("Inconel 718", "HAS_PROPERTY", "yield strength")
                },
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 2);

        let ti = facts.iter().find(|f| f.subject == "Ti-6Al-4V").unwrap();
        assert_eq!(ti.kind.as_deref(), Some("measurement"));
        assert_eq!(ti.value, Some(880.0), "Ti keeps ITS value");
        // CONTRACT CHANGE: source spellings are no longer translated by a
        // Rust table. The property node's exact term serves this edge.
        assert_eq!(ti.unit.as_deref(), Some("MPa"));

        let inconel = facts.iter().find(|f| f.subject == "Inconel 718").unwrap();
        assert_eq!(inconel.kind.as_deref(), Some("measurement"));
        assert_eq!(
            inconel.value,
            Some(1100.0),
            "Inconel keeps ITS value — never the shared node's 880"
        );
        assert_eq!(inconel.unit.as_deref(), Some("QUDT:MegaPA"));
    }

    /// The containment half: when NO relationship states a value and the
    /// shared node carries one, nothing is stored numerically for anyone —
    /// each affected relationship is reported, and the property links
    /// survive as generic edges (the link is true; the number is
    /// unattributable).
    #[test]
    fn a_value_on_a_shared_property_node_is_reported_and_stored_for_no_one() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                entity("Alloy", "Inconel 718", serde_json::json!({})),
                entity(
                    "Property",
                    "yield strength",
                    serde_json::json!({"value": 880.0, "unit": "MPa"}),
                ),
            ],
            relationships: vec![
                rel("Ti-6Al-4V", "HAS_PROPERTY", "yield strength"),
                rel("Inconel 718", "HAS_PROPERTY", "yield strength"),
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);

        assert_eq!(facts.len(), 2, "the links survive: {facts:?}");
        for fact in &facts {
            assert_eq!(fact.kind, None, "generic edge, never a measurement");
            assert_eq!(
                fact.value, None,
                "880 must not be stored as {}'s value — it attributes to nobody",
                fact.subject
            );
        }
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        for reason in &dropped {
            assert!(
                reason.contains("880") && reason.contains("2 subjects"),
                "the report names the number and the ambiguity: {reason}"
            );
        }
    }

    #[test]
    fn per_edge_unit_terms_are_preserved_without_property_word_dispatch() {
        // CONTRACT CHANGE: Rust no longer derives semantics from an English
        // property label or judges a model-selected ontology term.
        let set = EntitySet {
            entities: vec![
                entity("Subject", "sample", serde_json::json!({})),
                entity("Property", "property-a", serde_json::json!({})),
                entity("Property", "property-b", serde_json::json!({})),
                entity("Property", "property-c", serde_json::json!({})),
            ],
            relationships: vec![
                Relationship {
                    value: Some(8.19),
                    unit: Some("customer:unit-a".into()),
                    ..rel("sample", "HAS_PROPERTY", "property-a")
                },
                Relationship {
                    value: Some(1100.0),
                    unit: Some("https://example.test/unit/b".into()),
                    ..rel("sample", "HAS_PROPERTY", "property-b")
                },
                Relationship {
                    value: Some(542.0),
                    unit: Some("paper spelling".into()),
                    ..rel("sample", "HAS_PROPERTY", "property-c")
                },
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].unit.as_deref(), Some("customer:unit-a"));
        assert_eq!(
            facts[1].unit.as_deref(),
            Some("https://example.test/unit/b")
        );
        assert_eq!(facts[2].unit.as_deref(), Some("paper spelling"));
    }

    #[test]
    fn per_edge_values_preserve_terms_leave_absence_neutral_and_annotate_blank() {
        // CONTRACT CHANGE: every non-empty term is representable; absence is
        // neutral, while an explicitly blank term is a structural note.
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                entity("Property", "hardness", serde_json::json!({})),
                entity("Property", "toughness", serde_json::json!({})),
                entity("Property", "ductility", serde_json::json!({})),
            ],
            relationships: vec![
                Relationship {
                    value: Some(542.0),
                    unit: Some("HV".into()),
                    ..rel("Ti-6Al-4V", "HAS_PROPERTY", "hardness")
                },
                Relationship {
                    value: Some(75.0),
                    ..rel("Ti-6Al-4V", "HAS_PROPERTY", "toughness")
                },
                Relationship {
                    value: Some(12.0),
                    unit: Some("   ".into()),
                    ..rel("Ti-6Al-4V", "HAS_PROPERTY", "ductility")
                },
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].unit.as_deref(), Some("HV"));
        assert_eq!(facts[0].verification, None);
        assert_eq!(facts[1].unit, None);
        assert_eq!(facts[1].verification, None);
        assert_eq!(facts[2].unit, None);
        assert_eq!(
            facts[2].verification,
            Some(VerificationStatus::UnitUnresolved)
        );
    }

    #[test]
    fn has_property_with_numeric_value_becomes_measurement() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                entity(
                    "Property",
                    "UTS",
                    serde_json::json!({"value": 1140.0, "unit": "MPa"}),
                ),
            ],
            relationships: vec![rel("Ti-6Al-4V", "HAS_PROPERTY", "UTS")],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind.as_deref(), Some("measurement"));
        assert_eq!(facts[0].value, Some(1140.0));
        // CONTRACT CHANGE: the exact source/ontology term is retained.
        assert_eq!(facts[0].unit.as_deref(), Some("MPa"));
        assert_eq!(facts[0].subject, "Ti-6Al-4V");
        assert_eq!(facts[0].object, "UTS");
        assert_eq!(facts[0].predicate, "HAS_PROPERTY");
        assert_eq!(facts[0].confidence, Some(DEFAULT_CONFIDENCE));
    }

    #[test]
    fn numeric_values_preserve_exact_terms_and_leave_absence_neutral() {
        // CONTRACT CHANGE: Rust no longer maintains a resolvable-unit list.
        // Present non-empty terms survive exactly; absence is neutral.
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                // No unit at all on a numeric value.
                entity("Property", "UTS", serde_json::json!({"value": 880.0})),
                // An arbitrary non-empty source term.
                entity(
                    "Property",
                    "hardness",
                    serde_json::json!({"value": 349.0, "unit": "banana"}),
                ),
                // A second exact source spelling.
                entity(
                    "Property",
                    "density",
                    serde_json::json!({"value": 7.8, "unit": "g/cm3"}),
                ),
            ],
            relationships: vec![
                rel("Ti-6Al-4V", "HAS_PROPERTY", "UTS"),
                rel("Ti-6Al-4V", "HAS_PROPERTY", "hardness"),
                rel("Ti-6Al-4V", "HAS_PROPERTY", "density"),
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);

        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 3, "{facts:?}");
        let missing = facts.iter().find(|f| f.object == "UTS").unwrap();
        assert_eq!(missing.unit, None);
        assert_eq!(missing.verification, None);
        let arbitrary = facts.iter().find(|f| f.object == "hardness").unwrap();
        assert_eq!(arbitrary.unit.as_deref(), Some("banana"));
        let printed = facts.iter().find(|f| f.object == "density").unwrap();
        assert_eq!(printed.unit.as_deref(), Some("g/cm3"));
    }

    #[test]
    fn string_values_preserve_explicit_or_inline_unit_terms() {
        // CONTRACT CHANGE: parsing only separates structure. It does not
        // translate or judge supplied terms; conflicts become annotations.
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Inconel 718", serde_json::json!({})),
                entity(
                    "Property",
                    "yield strength",
                    serde_json::json!({"value": "1100", "unit": "MPa"}),
                ),
                entity(
                    "Property",
                    "density",
                    serde_json::json!({"value": "8.19 g/cm3", "unit": null}),
                ),
                entity(
                    "Property",
                    "hardness",
                    serde_json::json!({"value": "542 HV", "unit": null}),
                ),
                entity(
                    "Property",
                    "tensile strength",
                    serde_json::json!({"value": "1200 GPa", "unit": "MPa"}),
                ),
            ],
            relationships: vec![
                rel("Inconel 718", "HAS_PROPERTY", "yield strength"),
                rel("Inconel 718", "HAS_PROPERTY", "density"),
                rel("Inconel 718", "HAS_PROPERTY", "hardness"),
                rel("Inconel 718", "HAS_PROPERTY", "tensile strength"),
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);

        let ys = facts.iter().find(|f| f.object == "yield strength").unwrap();
        assert_eq!(ys.kind.as_deref(), Some("measurement"));
        assert_eq!(ys.value, Some(1100.0));
        assert_eq!(ys.unit.as_deref(), Some("MPa"));

        let density = facts.iter().find(|f| f.object == "density").unwrap();
        assert_eq!(density.kind.as_deref(), Some("measurement"));
        assert_eq!(density.value, Some(8.19));
        assert_eq!(density.unit.as_deref(), Some("g/cm3"));

        let hardness = facts.iter().find(|f| f.object == "hardness").unwrap();
        assert_eq!(hardness.kind.as_deref(), Some("measurement"));
        assert_eq!(hardness.value, Some(542.0));
        assert_eq!(hardness.unit.as_deref(), Some("HV"));

        let conflict = facts
            .iter()
            .find(|f| f.object == "tensile strength")
            .unwrap();
        assert_eq!(conflict.unit.as_deref(), Some("MPa"));
        assert_eq!(
            conflict.verification,
            Some(VerificationStatus::ModelAsserted)
        );
        assert!(
            conflict
                .verification_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("GPa") && reason.contains("MPa"))
        );
        assert!(dropped.is_empty(), "{dropped:?}");
    }

    #[test]
    fn split_leading_number_recognises_packed_forms_only() {
        assert_eq!(split_leading_number("1100 MPa"), Some((1100.0, "MPa")));
        assert_eq!(split_leading_number("8.19g/cm3"), Some((8.19, "g/cm3")));
        assert_eq!(split_leading_number("1100"), Some((1100.0, "")));
        assert_eq!(
            split_leading_number("0.2% proof stress"),
            Some((0.2, "% proof stress"))
        );
        assert_eq!(split_leading_number("high"), None);
        assert_eq!(split_leading_number("MPa 1100"), None);
        assert_eq!(split_leading_number(""), None);
        assert_eq!(split_leading_number("."), None);
    }

    #[test]
    fn has_property_without_numeric_value_stays_generic_not_dropped() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                entity(
                    "Property",
                    "corrosion resistance",
                    serde_json::json!({"value": "high"}),
                ),
            ],
            relationships: vec![rel("Ti-6Al-4V", "HAS_PROPERTY", "corrosion resistance")],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 1);
        // Generic edge under HAS_PROPERTY — a measurement kind without a
        // value would be dropped by write_fact.
        assert_eq!(facts[0].kind, None);
        assert_eq!(facts[0].value, None);
        assert_eq!(facts[0].predicate, "HAS_PROPERTY");
    }

    #[test]
    fn contains_maps_fraction_and_processed_by_maps_order() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Nb25Mo25Ta25W25", serde_json::json!({})),
                entity("Element", "Nb", serde_json::json!({})),
                entity("Process", "annealing", serde_json::json!({})),
            ],
            relationships: vec![
                Relationship {
                    weight: Some(0.25),
                    ..rel("Nb25Mo25Ta25W25", "CONTAINS", "Nb")
                },
                Relationship {
                    order: Some(2),
                    ..rel("Nb25Mo25Ta25W25", "PROCESSED_BY", "annealing")
                },
                rel("Nb25Mo25Ta25W25", "HAS_PHASE", "BCC"),
            ],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].kind.as_deref(), Some("contains"));
        assert_eq!(facts[0].value, Some(0.25));
        assert_eq!(facts[1].kind.as_deref(), Some("processing"));
        assert_eq!(facts[1].value, Some(2.0));
        assert_eq!(facts[2].kind.as_deref(), Some("phase"));
        // The CONTAINS weight is a fraction, never evidence confidence.
        assert_eq!(facts[0].confidence, Some(DEFAULT_CONFIDENCE));
    }

    #[test]
    fn unknown_rel_type_maps_to_generic_edge() {
        let set = EntitySet {
            entities: vec![],
            relationships: vec![rel("A", "DERIVED_FROM", "B")],
        };
        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind, None);
        assert_eq!(facts[0].predicate, "DERIVED_FROM");
    }

    /// A `measurement` fact must always carry its value.
    ///
    /// This pins an invariant that spans two crates. `ProvenanceStore`'s
    /// writer drops a `measurement` whose `value` is `None` and returns
    /// `Ok(())` without writing anything (`prism-provenance`,
    /// `emmo.rs` — "a measurement without a value fails schema validation and
    /// is dropped"). `IngestPipeline::write_local_graph` derives
    /// `nodes_created` from the mapped facts, so if this mapper ever emitted a
    /// valueless measurement the count would silently overstate again — the
    /// exact bug that count was fixed for.
    ///
    /// Nothing in the type system enforces it: `kind` is an
    /// `Option<String>` and `value` an unrelated `Option<f64>`. Hence this
    /// test rather than a comment.
    #[test]
    fn a_measurement_fact_always_carries_its_value() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                // Numeric value present -> measurement.
                entity(
                    "Property",
                    "UTS",
                    serde_json::json!({"value": 1140.0, "unit": "MPa"}),
                ),
                // No numeric value -> must NOT be classified as a measurement,
                // or the writer would drop it while the pipeline counted it.
                entity("Property", "colour", serde_json::json!({"note": "grey"})),
            ],
            relationships: vec![
                rel("Ti-6Al-4V", "HAS_PROPERTY", "UTS"),
                rel("Ti-6Al-4V", "HAS_PROPERTY", "colour"),
            ],
        };

        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);

        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 2);
        for fact in &facts {
            if fact.kind.as_deref() == Some("measurement") {
                assert!(
                    fact.value.is_some(),
                    "{} -> {} is kind=measurement with no value; the store would \
                     drop it and nodes_created would overstate",
                    fact.subject,
                    fact.object,
                );
            }
        }
        // And specifically: the valueless property degraded to a generic edge
        // rather than a measurement.
        let colour = facts.iter().find(|f| f.object == "colour").unwrap();
        assert_eq!(colour.kind, None);
        assert_eq!(colour.value, None);
    }

    /// The mapper's ontology dependency at its quietest: no declared
    /// classes, no declared relations, and (optionally) a chosen set of
    /// measurement-carrying tokens. An empty declaration must make the
    /// mapper fail honestly — report what it cannot store — never fall
    /// back to a frozen vocabulary.
    struct DeclaringOntology {
        measurement: Vec<&'static str>,
    }

    impl DeclaringOntology {
        fn silent() -> Self {
            Self {
                measurement: Vec::new(),
            }
        }

        fn measuring(tokens: Vec<&'static str>) -> Self {
            Self {
                measurement: tokens,
            }
        }
    }

    static TEST_VERSION: std::sync::OnceLock<Iri> = std::sync::OnceLock::new();

    impl Ontology for DeclaringOntology {
        fn id(&self) -> &'static str {
            "mapper-test"
        }

        fn version_iri(&self) -> &Iri {
            TEST_VERSION.get_or_init(|| {
                Iri::new("https://example.test/ontology/mapper-test/1".to_string()).unwrap()
            })
        }

        fn artifact_sha256(&self) -> &str {
            "0000000000000000000000000000000000000000000000000000000000000000"
        }

        fn classes(&self) -> &[crate::ontologies::ClassDecl] {
            &[]
        }

        fn relations(&self) -> &[crate::ontologies::RelationDecl] {
            &[]
        }

        fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
            sub == sup
        }

        fn measurement_relations(&self) -> Vec<&str> {
            self.measurement.clone()
        }
    }

    /// CONTRACT CHANGE: the mapper used to string-match EMMO's literals, so
    /// an ontology declaring nothing sent every numeric value to the generic
    /// edge silently. Its silence is now reported: the edge survives, the
    /// value does not, and `dropped` names the ontology that declared no
    /// measurement relations.
    #[test]
    fn an_ontology_declaring_no_measurement_relations_fails_honestly() {
        let set = EntitySet {
            entities: vec![
                entity("Subject", "sample", serde_json::json!({})),
                entity(
                    "Target",
                    "Messgröße",
                    serde_json::json!({"value": 42.0, "unit": "kPa"}),
                ),
                entity("Target", "Druck", serde_json::json!({})),
            ],
            relationships: vec![
                Relationship {
                    value: Some(3.5),
                    ..rel("sample", "MISST", "Druck")
                },
                rel("sample", "HAT_WERT", "Messgröße"),
            ],
        };

        let (facts, dropped) = to_local_facts(&set, &DeclaringOntology::silent());

        assert_eq!(facts.len(), 2, "both links survive: {facts:?}");
        for fact in &facts {
            assert_eq!(fact.kind, None, "no typed kind without a declaration");
            assert_eq!(fact.value, None, "no typed value without a declaration");
        }
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        for reason in &dropped {
            assert!(
                reason.contains("mapper-test")
                    && reason.contains("declares no measurement relations"),
                "the report must name the silent ontology: {reason}"
            );
        }
        assert!(
            dropped.iter().any(|r| r.contains("3.5")),
            "the per-edge value is reported: {dropped:?}"
        );
        assert!(
            dropped.iter().any(|r| r.contains("42")),
            "the entity-level value is reported: {dropped:?}"
        );
    }

    /// CONTRACT CHANGE: measurement mapping used to follow English literals,
    /// so a non-English ontology's numbers became untyped edges while an
    /// English one inherited mapping by lexical accident. Mapping now
    /// follows the declaration: a German token the ontology names carries
    /// the measurement, and nothing maps by spelling alone.
    #[test]
    fn measurement_mapping_follows_the_declaration_not_an_english_literal() {
        let set = EntitySet {
            entities: vec![
                entity("Subject", "Probe", serde_json::json!({})),
                entity("Target", "Druck", serde_json::json!({})),
                entity("Target", "Wert", serde_json::json!({"value": 7.0})),
            ],
            relationships: vec![
                Relationship {
                    value: Some(3.5),
                    unit: Some("kPa".into()),
                    ..rel("Probe", "MISST", "Druck")
                },
                rel("Probe", "HAT_WERT", "Wert"),
            ],
        };

        let (facts, dropped) = to_local_facts(&set, &DeclaringOntology::measuring(vec!["MISST"]));

        let measured = facts.iter().find(|f| f.predicate == "MISST").unwrap();
        assert_eq!(measured.kind.as_deref(), Some("measurement"));
        assert_eq!(measured.value, Some(3.5));
        assert_eq!(measured.unit.as_deref(), Some("kPa"));

        // An undeclared relation type stays generic even though the target
        // carries a number: this ontology's measurement channel is MISST.
        let generic = facts.iter().find(|f| f.predicate == "HAT_WERT").unwrap();
        assert_eq!(generic.kind, None);
        assert_eq!(generic.value, None);
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(
            dropped[0].contains("HAT_WERT")
                && dropped[0].contains("7")
                && dropped[0].contains("not declared measurement-carrying")
                && dropped[0].contains("mapper-test"),
            "an ontology declaring a different measurement relation must not make the \
             undeclared edge's number disappear silently: {}",
            dropped[0]
        );
    }

    /// The entity-level value channel belongs to the ontology's declared
    /// QUANTITATIVE types: a value sitting on a target of any other type is
    /// reported and retained as a generic edge, never attributed by shape.
    #[test]
    fn entity_level_values_attribute_only_to_declared_quantitative_types() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                // A value on an entity whose TYPE is not quantitative —
                // here an Alloy — has no attributable channel.
                entity(
                    "Alloy",
                    "yield strength",
                    serde_json::json!({"value": 880.0, "unit": "MPa"}),
                ),
            ],
            relationships: vec![rel("Ti-6Al-4V", "HAS_PROPERTY", "yield strength")],
        };

        let (facts, dropped) = to_local_facts(&set, &crate::ontologies::EmmoOntology);

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind, None, "the link survives as a generic edge");
        assert_eq!(facts[0].value, None);
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(
            dropped[0].contains("880") && dropped[0].contains("not declare quantitative"),
            "the report names the value and the missing declaration: {}",
            dropped[0]
        );
    }
}
