//! `EntitySet` → `LocalFact` mapping for the bundled Turso EMMO store.
//!
//! Bridges the LLM tabular extraction output (`Entity` / `Relationship`)
//! into the typed facts `prism_provenance::emmo` writes, replacing the
//! Neo4j upsert in the local pipeline (Neo4j retirement, step 1).

use prism_provenance::LocalFact;

use crate::{Entity, EntitySet};

/// Default evidence confidence for facts mapped from tabular extraction.
/// `Relationship.weight` is never usable as confidence here — the extractor
/// emits it as a composition fraction on `CONTAINS` and leaves it unset
/// elsewhere — so every mapped fact carries this flat "LLM-extracted from
/// structured data, unverified" prior.
const DEFAULT_CONFIDENCE: f64 = 0.8;

/// Map every extracted relationship to one `LocalFact` in the EMMO shape
/// `prism_provenance::ProvenanceStore::write_fact` expects:
///
/// - `HAS_PROPERTY` → `measurement` when the target entity's freeform
///   `properties` carry a numeric `value` AND a unit that resolves through
///   the same controlled vocabulary the text path uses
///   (`prism_provenance::units::resolve_unit` — the stored unit is always
///   the QUDT identifier, never the raw spelling); a numeric value whose
///   unit is missing or unresolvable DROPS the whole relationship, reported
///   in the second return value (a number stored without its unit is a
///   wrong number — unit-less floats once made 880 GPa indistinguishable
///   from 880 MPa in this store). A target with no numeric value at all
///   stays a generic edge (kind `None`) so the property is not dropped.
/// - `HAS_PHASE` → `phase`.
/// - `PROCESSED_BY` → `processing`, with the step order in `value`.
/// - `CONTAINS` → `contains`, with the fraction in `value`.
/// - Anything else → generic edge under its own predicate.
///
/// Entities that appear in no relationship produce no facts. Returns
/// `(facts, dropped)` — one human-readable reason per dropped relationship,
/// same contract as the pipeline's `dropped_relationships`: NON-EMPTY is a
/// PARTIAL result the caller must surface, never a silent drop.
pub fn to_local_facts(entity_set: &EntitySet) -> (Vec<LocalFact>, Vec<String>) {
    let by_name: std::collections::HashMap<&str, &Entity> = entity_set
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();

    let mut facts = Vec::with_capacity(entity_set.relationships.len());
    let mut dropped = Vec::new();
    for rel in &entity_set.relationships {
        let (kind, value, unit) = match rel.rel_type.as_str() {
            "HAS_PROPERTY" => {
                // The numeric value + unit live in the TARGET entity's
                // freeform properties JSON, not on the relationship.
                let target = by_name.get(rel.to.as_str());
                let value = target
                    .and_then(|e| e.properties.get("value"))
                    .and_then(serde_json::Value::as_f64);
                match value {
                    Some(v) => {
                        let spelling = target
                            .and_then(|e| e.properties.get("unit"))
                            .and_then(serde_json::Value::as_str);
                        // THE unit rule, same table as the text path: a
                        // numeric value is stored with a resolved QUDT unit
                        // or not at all.
                        match spelling {
                            Some(spelling) => {
                                match prism_provenance::units::resolve_unit(spelling) {
                                    Some(unit) => (
                                        Some("measurement"),
                                        Some(v),
                                        Some(unit.as_str().to_string()),
                                    ),
                                    None => {
                                        dropped.push(format!(
                                            "{} -[HAS_PROPERTY]-> {}: unit {spelling:?} on \
                                             numeric value {v} is neither a QUDT identifier \
                                             nor a recognised unit spelling — a number stored \
                                             without its unit is a wrong number, so the fact \
                                             is dropped whole, never stored unit-less",
                                            rel.from, rel.to
                                        ));
                                        continue;
                                    }
                                }
                            }
                            None => {
                                dropped.push(format!(
                                    "{} -[HAS_PROPERTY]-> {}: numeric value {v} arrived \
                                     with no unit at all — a unit-less number is a wrong \
                                     number, so the fact is dropped whole, never stored \
                                     unit-less",
                                    rel.from, rel.to
                                ));
                                continue;
                            }
                        }
                    }
                    None => (None, None, None),
                }
            }
            "HAS_PHASE" => (Some("phase"), None, None),
            "PROCESSED_BY" => (Some("processing"), rel.order.map(f64::from), None),
            "CONTAINS" => (Some("contains"), rel.weight, None),
            _ => (None, None, None),
        };
        facts.push(LocalFact {
            subject: rel.from.clone(),
            predicate: rel.rel_type.clone(),
            object: rel.to.clone(),
            value,
            unit,
            confidence: Some(DEFAULT_CONFIDENCE),
            kind: kind.map(str::to_string),
        });
    }
    (facts, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Relationship;

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
        }
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
        let (facts, dropped) = to_local_facts(&set);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind.as_deref(), Some("measurement"));
        assert_eq!(facts[0].value, Some(1140.0));
        // The unit is stored RESOLVED — the QUDT identifier, never the raw
        // spelling (the text path and the store hold the same vocabulary).
        assert_eq!(facts[0].unit.as_deref(), Some("QUDT:MegaPA"));
        assert_eq!(facts[0].subject, "Ti-6Al-4V");
        assert_eq!(facts[0].object, "UTS");
        assert_eq!(facts[0].predicate, "HAS_PROPERTY");
        assert_eq!(facts[0].confidence, Some(DEFAULT_CONFIDENCE));
    }

    /// THE unit rule on the tabular path: a numeric value whose unit is
    /// missing or resolves to no QUDT identifier drops the WHOLE
    /// relationship, with a reason — never a measurement with a raw or
    /// empty unit string (F10: the store once held `unit: ""` for exactly
    /// this shape, re-opening the 880 GPa vs 880 MPa hazard the text path
    /// had closed).
    #[test]
    fn numeric_value_with_missing_or_unresolvable_unit_is_dropped_with_reason() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                // No unit at all on a numeric value.
                entity("Property", "UTS", serde_json::json!({"value": 880.0})),
                // A unit that resolves to nothing.
                entity(
                    "Property",
                    "hardness",
                    serde_json::json!({"value": 349.0, "unit": "banana"}),
                ),
                // Control: resolvable spelling → stored, resolved.
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
        let (facts, dropped) = to_local_facts(&set);

        assert_eq!(facts.len(), 1, "{facts:?}");
        assert_eq!(facts[0].object, "density");
        assert_eq!(facts[0].unit.as_deref(), Some("QUDT:GM-PER-CentiM3"));

        assert_eq!(dropped.len(), 2, "{dropped:?}");
        let drops = dropped.join("\n");
        assert!(
            drops.contains("UTS") && drops.contains("880") && drops.contains("no unit at all"),
            "the unit-less drop must name the fact, the value and the cause: {drops}"
        );
        assert!(
            drops.contains("hardness") && drops.contains("banana"),
            "the unresolvable-unit drop must name the fact and the spelling: {drops}"
        );
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
        let (facts, dropped) = to_local_facts(&set);
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
        let (facts, dropped) = to_local_facts(&set);
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
        let (facts, dropped) = to_local_facts(&set);
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

        let (facts, dropped) = to_local_facts(&set);

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
}
