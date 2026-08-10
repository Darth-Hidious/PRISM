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

/// Split a leading decimal number off a string: `"1100 MPa"` → `(1100.0,
/// "MPa")`, `"8.19g/cm3"` → `(8.19, "g/cm3")`, `"1100"` → `(1100.0, "")`.
/// `None` when the string does not START with a number (`"high"`,
/// `"MPa 1100"`). Deliberately simple — unsigned decimals only, no
/// exponents — because what it recognises must stay auditable: it feeds
/// both the packed-VALUE parser below and the packed-NAME rejection in
/// [`crate::graph_validation::measurement_packed_in_name`], and the two
/// must recognise the same shapes.
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
/// - `"1100 MPa"` (number and unit packed into one string — the same
///   form-versus-field failure the per-type schema exists to prevent, split
///   deterministically, never guessed) → `(1100.0, Some("MPa"))`, but ONLY
///   when the tail resolves through the one controlled vocabulary
///   (`prism_provenance::units::resolve_unit`). A tail that resolves to no
///   unit (`"2nd phase dominant"`, `"542 HV"`) makes the whole value a
///   non-claim → `None`, same as `"high"`: this module cannot tell a
///   measurement in an unknown unit from prose, and inventing the split
///   would store a number under a unit nobody vouched for.
fn numeric_claim(value: &serde_json::Value) -> Option<(f64, Option<&str>)> {
    if let Some(v) = value.as_f64() {
        return Some((v, None));
    }
    let s = value.as_str()?.trim();
    if let Ok(v) = s.parse::<f64>() {
        return Some((v, None));
    }
    let (v, tail) = split_leading_number(s)?;
    (!tail.is_empty() && prism_provenance::units::resolve_unit(tail).is_some())
        .then_some((v, Some(tail)))
}

/// Map every extracted relationship to one `LocalFact` in the EMMO shape
/// `prism_provenance::ProvenanceStore::write_fact` expects:
///
/// - `HAS_PROPERTY` → `measurement` when a numeric value with a resolvable
///   unit is attributable to THIS subject. Two channels, in order:
///   the RELATIONSHIP's own `value`/`unit` (per-edge, attributable by
///   construction — the unit may also come from the target property node,
///   which legitimately carries the unit while each edge carries its
///   number); else the TARGET entity's freeform `value`/`unit` members (a
///   JSON number, a numeric string, or the packed `"1100 MPa"` form — see
///   [`numeric_claim`]), accepted ONLY when exactly one subject references
///   the target — a value on a SHARED property node attributes to nobody
///   and is reported and stored for no one (the edge stays generic).
///   Units always resolve through the same controlled vocabulary the text
///   path uses (`prism_provenance::units::resolve_unit` — the stored unit
///   is always the QUDT identifier, never the raw spelling); a numeric
///   value whose unit is missing or unresolvable DROPS the whole
///   relationship, reported in the second return value (a number stored
///   without its unit is a wrong number — unit-less floats once made 880
///   GPa indistinguishable from 880 MPa in this store). A target with no
///   numeric value at all stays a generic edge (kind `None`) so the
///   property is not dropped.
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

    // How many DISTINCT subjects state HAS_PROPERTY about each target — the
    // attribution gate for entity-level values: a value on a property node
    // referenced by several materials is one number claiming to be all of
    // their measurements (live 2026-08-10: one "yield strength" node's 880
    // was stored as five different alloys' yield strength — four
    // falsehoods).
    let mut property_subjects: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        std::collections::HashMap::new();
    for rel in &entity_set.relationships {
        if rel.rel_type == "HAS_PROPERTY" {
            property_subjects
                .entry(rel.to.as_str())
                .or_default()
                .insert(rel.from.as_str());
        }
    }

    let mut facts = Vec::with_capacity(entity_set.relationships.len());
    let mut dropped = Vec::new();
    for rel in &entity_set.relationships {
        let (kind, value, unit) = match rel.rel_type.as_str() {
            "HAS_PROPERTY" => {
                let target = by_name.get(rel.to.as_str());
                // The per-edge channel is authoritative: a value on the
                // RELATIONSHIP attributes to THIS subject by construction.
                // Unit precedence: the relationship's own unit, else the
                // target entity's declared unit (a property node
                // legitimately carries the unit while each edge carries its
                // number). The unit rule is unchanged either way: resolved
                // through the ONE vocabulary or dropped, never stored raw.
                if let Some(v) = rel.value {
                    let spelling = rel.unit.as_deref().or_else(|| {
                        target
                            .and_then(|e| e.properties.get("unit"))
                            .and_then(serde_json::Value::as_str)
                    });
                    match spelling.map(|sp| (sp, prism_provenance::units::resolve_unit(sp))) {
                        Some((_, Some(unit))) => {
                            // The quantity-kind gate on the per-edge channel
                            // — the semantic half the grammar cannot give,
                            // reading the SAME `qudt_units` claims as graph
                            // validation's entity-level Check 11 (which
                            // never sees relationship units). Live run 4,
                            // 2026-08-10: the enum-locked model tagged every
                            // density with QUDT:GigaPA — a pressure unit —
                            // on the RELATIONSHIP, the 2026-08-08 failure
                            // relocated to the new channel, and it reached
                            // the store. Both sides known and contradictory
                            // drops the fact whole; a value stored under a
                            // wrong-kind unit is a falsehood.
                            if let (Some(unit_kind), Some(stated_kind)) = (
                                crate::qudt_units::unit_quantity_kind(unit.as_str()),
                                crate::qudt_units::property_quantity_kind(&rel.to),
                            ) && unit_kind != stated_kind
                            {
                                dropped.push(format!(
                                    "{} -[HAS_PROPERTY]-> {}: the relationship states a {} \
                                     but carries unit {}, a {} unit — storing value {v} \
                                     under it would store a falsehood, so the fact is \
                                     dropped whole",
                                    rel.from,
                                    rel.to,
                                    stated_kind.label(),
                                    unit.as_str(),
                                    unit_kind.label(),
                                ));
                                continue;
                            }
                            facts.push(LocalFact {
                                subject: rel.from.clone(),
                                predicate: rel.rel_type.clone(),
                                object: rel.to.clone(),
                                value: Some(v),
                                unit: Some(unit.as_str().to_string()),
                                confidence: Some(DEFAULT_CONFIDENCE),
                                kind: Some("measurement".into()),
                            });
                            continue;
                        }
                        Some((sp, None)) => {
                            dropped.push(format!(
                                "{} -[HAS_PROPERTY]-> {}: unit {sp:?} on the relationship's \
                                 numeric value {v} is neither a QUDT identifier nor a \
                                 recognised unit spelling — a number stored without its unit \
                                 is a wrong number, so the fact is dropped whole, never \
                                 stored unit-less",
                                rel.from, rel.to
                            ));
                            continue;
                        }
                        None => {
                            dropped.push(format!(
                                "{} -[HAS_PROPERTY]-> {}: the relationship states numeric \
                                 value {v} with no unit anywhere (neither on the \
                                 relationship nor on the property) — a unit-less number is \
                                 a wrong number, so the fact is dropped whole, never stored \
                                 unit-less",
                                rel.from, rel.to
                            ));
                            continue;
                        }
                    }
                }
                // Entity-level fallback: the value + unit in the TARGET
                // entity's freeform properties JSON (a JSON number, a
                // numeric string, or the packed "1100 MPa" form — see
                // `numeric_claim`). Attributable ONLY when exactly one
                // subject references the target.
                let claim = target
                    .and_then(|e| e.properties.get("value"))
                    .and_then(numeric_claim);
                match claim {
                    Some((v, _))
                        if property_subjects
                            .get(rel.to.as_str())
                            .is_some_and(|subjects| subjects.len() > 1) =>
                    {
                        let shared = property_subjects[rel.to.as_str()].len();
                        // One number on a node shared by several subjects is
                        // unattributable — stored under each it fabricates
                        // data, stored under one it guesses. The property
                        // LINK itself is still true, so the edge is kept
                        // generic (no value) and the drop is reported.
                        dropped.push(format!(
                            "{} -[HAS_PROPERTY]-> {}: value {v} sits on the property node, \
                             which {shared} different materials reference, and this \
                             relationship states no value of its own — one shared number \
                             attributes to no single material, so it is stored for none of \
                             them; the property link is kept without a value (per-material \
                             values belong on each HAS_PROPERTY relationship)",
                            rel.from, rel.to
                        ));
                        (None, None, None)
                    }
                    Some((v, inline)) => {
                        let declared = target
                            .and_then(|e| e.properties.get("unit"))
                            .and_then(serde_json::Value::as_str);
                        // THE unit rule, same table as the text path: a
                        // numeric value is stored with a resolved QUDT unit
                        // or not at all. The explicit `unit` member is
                        // authoritative when present; a spelling packed
                        // into the value string is used only in its
                        // absence, and a CONFLICT between the two drops the
                        // fact — an ambiguous unit is no better than a
                        // missing one.
                        match (declared, inline) {
                            (Some(spelling), _) => {
                                match prism_provenance::units::resolve_unit(spelling) {
                                    Some(unit) => {
                                        if let Some(inline) = inline
                                            && let Some(inline_unit) =
                                                prism_provenance::units::resolve_unit(inline)
                                            && inline_unit.as_str() != unit.as_str()
                                        {
                                            dropped.push(format!(
                                                "{} -[HAS_PROPERTY]-> {}: value {v} carries \
                                                 unit {inline:?} inline while the unit field \
                                                 says {spelling:?} — the two resolve to \
                                                 different units ({} vs {}), and an ambiguous \
                                                 unit is a wrong number, so the fact is \
                                                 dropped whole",
                                                rel.from,
                                                rel.to,
                                                inline_unit.as_str(),
                                                unit.as_str()
                                            ));
                                            continue;
                                        }
                                        (
                                            Some("measurement"),
                                            Some(v),
                                            Some(unit.as_str().to_string()),
                                        )
                                    }
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
                            (None, Some(inline)) => {
                                let unit = prism_provenance::units::resolve_unit(inline).expect(
                                    "numeric_claim only returns inline spellings that resolve",
                                );
                                (
                                    Some("measurement"),
                                    Some(v),
                                    Some(unit.as_str().to_string()),
                                )
                            }
                            (None, None) => {
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
            value: None,
            unit: None,
        }
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
        let (facts, dropped) = to_local_facts(&set);
        assert!(dropped.is_empty(), "{dropped:?}");
        assert_eq!(facts.len(), 2);

        let ti = facts.iter().find(|f| f.subject == "Ti-6Al-4V").unwrap();
        assert_eq!(ti.kind.as_deref(), Some("measurement"));
        assert_eq!(ti.value, Some(880.0), "Ti keeps ITS value");
        // No unit on Ti's edge — the property node's unit serves it.
        assert_eq!(ti.unit.as_deref(), Some("QUDT:MegaPA"));

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
        let (facts, dropped) = to_local_facts(&set);

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
                reason.contains("880") && reason.contains("2 different materials"),
                "the report names the number and the ambiguity: {reason}"
            );
        }
    }

    /// The quantity-kind gate holds on the per-edge channel: a density
    /// stated under a pressure unit is dropped whole and reported (live run
    /// 4, 2026-08-10 — the enum-locked model tagged every density with
    /// QUDT:GigaPA on the relationship and the falsehoods reached the
    /// store), the RIGHT unit passes, and a property this codebase makes no
    /// kind-claim about is untouched by the gate.
    #[test]
    fn per_edge_units_of_the_wrong_quantity_kind_are_dropped_with_reason() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Inconel 718", serde_json::json!({})),
                entity("Property", "density", serde_json::json!({})),
                entity("Property", "yield strength", serde_json::json!({})),
                entity("Property", "Hardness_HV", serde_json::json!({})),
            ],
            relationships: vec![
                // The live corruption: a density under a pressure unit.
                Relationship {
                    value: Some(8.19),
                    unit: Some("QUDT:GigaPA".into()),
                    ..rel("Inconel 718", "HAS_PROPERTY", "density")
                },
                // Control: the right kind stores.
                Relationship {
                    value: Some(1100.0),
                    unit: Some("QUDT:MegaPA".into()),
                    ..rel("Inconel 718", "HAS_PROPERTY", "yield strength")
                },
                // No kind claim for hardness — the resolve rule alone applies.
                Relationship {
                    value: Some(542.0),
                    unit: Some("QUDT:MegaPA".into()),
                    ..rel("Inconel 718", "HAS_PROPERTY", "Hardness_HV")
                },
            ],
        };
        let (facts, dropped) = to_local_facts(&set);

        assert!(
            !facts.iter().any(|f| f.object == "density"),
            "a density under a pressure unit must never be stored: {facts:?}"
        );
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(
            dropped[0].contains("density")
                && dropped[0].contains("QUDT:GigaPA")
                && dropped[0].contains("pressure"),
            "the drop must name the property, the unit and the contradiction: {}",
            dropped[0]
        );

        let ys = facts.iter().find(|f| f.object == "yield strength").unwrap();
        assert_eq!(ys.kind.as_deref(), Some("measurement"));
        assert_eq!(ys.value, Some(1100.0));
        let hv = facts.iter().find(|f| f.object == "Hardness_HV").unwrap();
        assert_eq!(hv.kind.as_deref(), Some("measurement"));
    }

    /// The unit rule holds on the per-edge channel exactly as everywhere
    /// else: a relationship value with an unresolvable or absent unit is
    /// dropped whole and reported, never stored raw or unit-less.
    #[test]
    fn per_edge_values_obey_the_same_unit_rule() {
        let set = EntitySet {
            entities: vec![
                entity("Alloy", "Ti-6Al-4V", serde_json::json!({})),
                entity("Property", "hardness", serde_json::json!({})),
                entity("Property", "toughness", serde_json::json!({})),
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
            ],
        };
        let (facts, dropped) = to_local_facts(&set);
        assert!(facts.is_empty(), "{facts:?}");
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        assert!(
            dropped[0].contains("HV") && dropped[0].contains("542"),
            "{}",
            dropped[0]
        );
        assert!(
            dropped[1].contains("no unit anywhere") && dropped[1].contains("75"),
            "{}",
            dropped[1]
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

    /// Numeric claims arriving as STRINGS still become typed measurements —
    /// through the ONE unit vocabulary, never a second parser: a numeric
    /// string uses the explicit unit field; the packed "1100 MPa" form
    /// splits deterministically; a packed spelling that resolves to no unit
    /// ("542 HV") is prose, not a claim — generic edge, nothing stored
    /// wrong; and a packed unit CONFLICTING with the unit field drops the
    /// fact whole (ambiguous unit = wrong number).
    #[test]
    fn string_values_resolve_through_the_same_unit_rule() {
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
        let (facts, dropped) = to_local_facts(&set);

        let ys = facts.iter().find(|f| f.object == "yield strength").unwrap();
        assert_eq!(ys.kind.as_deref(), Some("measurement"));
        assert_eq!(ys.value, Some(1100.0));
        assert_eq!(ys.unit.as_deref(), Some("QUDT:MegaPA"));

        let density = facts.iter().find(|f| f.object == "density").unwrap();
        assert_eq!(density.kind.as_deref(), Some("measurement"));
        assert_eq!(density.value, Some(8.19));
        assert_eq!(density.unit.as_deref(), Some("QUDT:GM-PER-CentiM3"));

        // "542 HV": HV resolves to nothing, so the string is prose — a
        // generic edge with NO value, never 542 under a guessed unit.
        let hardness = facts.iter().find(|f| f.object == "hardness").unwrap();
        assert_eq!(hardness.kind, None);
        assert_eq!(hardness.value, None);
        assert_eq!(hardness.unit, None);

        // The conflict: inline GPa vs declared MPa — dropped whole.
        assert!(
            !facts.iter().any(|f| f.object == "tensile strength"),
            "{facts:?}"
        );
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert!(
            dropped[0].contains("tensile strength")
                && dropped[0].contains("QUDT:GigaPA")
                && dropped[0].contains("QUDT:MegaPA"),
            "the conflict drop must name the fact and both units: {}",
            dropped[0]
        );
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
