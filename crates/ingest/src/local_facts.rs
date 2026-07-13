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
///   `properties` carry a numeric `value` (with optional `unit`); otherwise
///   a generic edge (kind `None`) so the property is not dropped.
/// - `HAS_PHASE` → `phase`.
/// - `PROCESSED_BY` → `processing`, with the step order in `value`.
/// - `CONTAINS` → `contains`, with the fraction in `value`.
/// - Anything else → generic edge under its own predicate.
///
/// Entities that appear in no relationship produce no facts.
pub fn to_local_facts(entity_set: &EntitySet) -> Vec<LocalFact> {
    let by_name: std::collections::HashMap<&str, &Entity> = entity_set
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();

    entity_set
        .relationships
        .iter()
        .map(|rel| {
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
                            let unit = target
                                .and_then(|e| e.properties.get("unit"))
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string);
                            (Some("measurement"), Some(v), unit)
                        }
                        None => (None, None, None),
                    }
                }
                "HAS_PHASE" => (Some("phase"), None, None),
                "PROCESSED_BY" => (Some("processing"), rel.order.map(f64::from), None),
                "CONTAINS" => (Some("contains"), rel.weight, None),
                _ => (None, None, None),
            };
            LocalFact {
                subject: rel.from.clone(),
                predicate: rel.rel_type.clone(),
                object: rel.to.clone(),
                value,
                unit,
                confidence: Some(DEFAULT_CONFIDENCE),
                kind: kind.map(str::to_string),
            }
        })
        .collect()
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
        let facts = to_local_facts(&set);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind.as_deref(), Some("measurement"));
        assert_eq!(facts[0].value, Some(1140.0));
        assert_eq!(facts[0].unit.as_deref(), Some("MPa"));
        assert_eq!(facts[0].subject, "Ti-6Al-4V");
        assert_eq!(facts[0].object, "UTS");
        assert_eq!(facts[0].predicate, "HAS_PROPERTY");
        assert_eq!(facts[0].confidence, Some(DEFAULT_CONFIDENCE));
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
        let facts = to_local_facts(&set);
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
        let facts = to_local_facts(&set);
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
        let facts = to_local_facts(&set);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind, None);
        assert_eq!(facts[0].predicate, "DERIVED_FROM");
    }
}
