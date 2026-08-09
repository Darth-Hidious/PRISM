//! Graph quality validation — SHACL-lite checks on extracted EntitySets.
//!
//! Validates structural integrity of the knowledge graph before the graph
//! write. Catches issues the LLM entity extraction might produce:
//!
//! - Orphan relationships (reference non-existent entities)
//! - Missing required properties (e.g. Alloy without name)
//! - Invalid relationship types
//! - Duplicate entities
//! - Domain constraints the active ontology declares (for EMMO:
//!   weight/order rules on CONTAINS/PROCESSED_BY)
//!
//! Type membership is checked against the ACTIVE [`Ontology`]'s declared
//! vocabulary — the SAME declaration the extraction prompt is built from —
//! so the prompt and this validator cannot disagree about what is valid.
//! (The vocabulary used to live here as const arrays, which let the prompt
//! and the validator drift independently.)

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::EntitySet;
use crate::ontologies::Ontology;

/// A graph validation issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphIssue {
    pub severity: GraphSeverity,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphSeverity {
    Error,
    Warning,
    Info,
}

/// Result of graph validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphValidationReport {
    pub issues: Vec<GraphIssue>,
    pub entity_count: usize,
    pub relationship_count: usize,
    pub passed: bool,
}

/// Validate an EntitySet for structural integrity against the active
/// ontology's vocabulary. (The EMMO type lists that used to be consts here
/// are now [`crate::ontologies::EmmoOntology`]'s declaration.)
pub fn validate_graph(ontology: &dyn Ontology, entities: &EntitySet) -> GraphValidationReport {
    let mut issues = Vec::new();

    // Build entity name lookup
    let entity_names: HashSet<&str> = entities.entities.iter().map(|e| e.name.as_str()).collect();

    // Check 1: Empty graph
    if entities.entities.is_empty() {
        issues.push(GraphIssue {
            severity: GraphSeverity::Error,
            category: "empty".into(),
            message: "No entities were extracted".to_string(),
        });
    }

    // Check 2: Duplicate entity names within same type
    let mut type_names: HashMap<(&str, &str), usize> = HashMap::new();
    for e in &entities.entities {
        *type_names
            .entry((e.entity_type.as_str(), e.name.as_str()))
            .or_default() += 1;
    }
    for ((etype, name), count) in &type_names {
        if *count > 1 {
            issues.push(GraphIssue {
                severity: GraphSeverity::Warning,
                category: "duplicate".into(),
                message: format!("Duplicate entity: {etype}:{name} appears {count} times"),
            });
        }
    }

    // Check 3: Unknown entity types — against the active ontology's
    // declared vocabulary, the same declaration the prompt is built from.
    let entity_types = ontology.entity_types();
    for e in &entities.entities {
        if !entity_types.contains(&e.entity_type.as_str()) {
            issues.push(GraphIssue {
                severity: GraphSeverity::Warning,
                category: "unknown_type".into(),
                message: format!(
                    "Unknown entity type '{}' for '{}' — expected one of: {}",
                    e.entity_type,
                    e.name,
                    entity_types.join(", ")
                ),
            });
        }
    }

    // Check 4: Entities with empty names
    for e in &entities.entities {
        if e.name.trim().is_empty() {
            issues.push(GraphIssue {
                severity: GraphSeverity::Error,
                category: "empty_name".into(),
                message: format!("Entity of type '{}' has empty name", e.entity_type),
            });
        }
    }

    // Check 5: Orphan relationships — reference entities not in the set.
    // Deliberately KEPT at Error severity: a dangling edge must never be
    // written, and `passed` has to stay fail-closed for any consumer that
    // writes on it. The tabular pipeline CONTAINS this one class instead of
    // failing the whole ingest — it drops exactly the dangling relationships,
    // re-validates, and reports the drop (`pipeline::validate_before_graph_write`).
    for r in &entities.relationships {
        if !entity_names.contains(r.from.as_str()) {
            issues.push(GraphIssue {
                severity: GraphSeverity::Error,
                category: "orphan_rel".into(),
                message: format!(
                    "Relationship {}-[{}]->{}: source '{}' not in entity set",
                    r.from, r.rel_type, r.to, r.from
                ),
            });
        }
        if !entity_names.contains(r.to.as_str()) {
            issues.push(GraphIssue {
                severity: GraphSeverity::Error,
                category: "orphan_rel".into(),
                message: format!(
                    "Relationship {}-[{}]->{}: target '{}' not in entity set",
                    r.from, r.rel_type, r.to, r.to
                ),
            });
        }
    }

    // Check 6: Unknown relationship types — same source as check 3.
    let rel_types = ontology.relationship_types();
    for r in &entities.relationships {
        if !rel_types.contains(&r.rel_type.as_str()) {
            issues.push(GraphIssue {
                severity: GraphSeverity::Warning,
                category: "unknown_rel".into(),
                message: format!(
                    "Unknown relationship type '{}' ({} → {})",
                    r.rel_type, r.from, r.to
                ),
            });
        }
    }

    // Checks 7–10 moved into the ontology: EMMO's weight/order rules on
    // CONTAINS/PROCESSED_BY live in `EmmoOntology::validate_domain`, emitted
    // here in the position they always ran so reports are unchanged for
    // existing users. Another ontology contributes its own domain checks.
    issues.extend(ontology.validate_domain(entities));

    let has_errors = issues.iter().any(|i| i.severity == GraphSeverity::Error);

    GraphValidationReport {
        entity_count: entities.entities.len(),
        relationship_count: entities.relationships.len(),
        passed: !has_errors,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontologies::EmmoOntology;
    use crate::{Entity, Relationship};

    fn make_entity(etype: &str, name: &str) -> Entity {
        Entity {
            entity_type: etype.into(),
            name: name.into(),
            properties: serde_json::json!({}),
        }
    }

    fn make_rel(from: &str, rel: &str, to: &str) -> Relationship {
        Relationship {
            from: from.into(),
            rel_type: rel.into(),
            to: to.into(),
            weight: None,
            order: None,
        }
    }

    #[test]
    fn valid_graph_passes() {
        let es = EntitySet {
            entities: vec![
                make_entity("Alloy", "NbMoTaW"),
                make_entity("Element", "Nb"),
                make_entity("Element", "Mo"),
            ],
            relationships: vec![
                Relationship {
                    from: "NbMoTaW".into(),
                    rel_type: "CONTAINS".into(),
                    to: "Nb".into(),
                    weight: Some(0.5),
                    order: None,
                },
                Relationship {
                    from: "NbMoTaW".into(),
                    rel_type: "CONTAINS".into(),
                    to: "Mo".into(),
                    weight: Some(0.5),
                    order: None,
                },
            ],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(report.passed);
        assert!(
            report
                .issues
                .iter()
                .all(|i| i.severity != GraphSeverity::Error)
        );
    }

    #[test]
    fn detects_orphan_relationships() {
        let es = EntitySet {
            entities: vec![make_entity("Alloy", "Steel")],
            relationships: vec![make_rel("Steel", "CONTAINS", "Fe")], // Fe not in entities
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.category == "orphan_rel"));
    }

    #[test]
    fn detects_empty_entity_name() {
        let es = EntitySet {
            entities: vec![make_entity("Alloy", "")],
            relationships: vec![],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.category == "empty_name"));
    }

    #[test]
    fn detects_duplicate_entities() {
        let es = EntitySet {
            entities: vec![make_entity("Element", "Fe"), make_entity("Element", "Fe")],
            relationships: vec![],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(report.issues.iter().any(|i| i.category == "duplicate"));
    }

    #[test]
    fn detects_unknown_relationship_type() {
        let es = EntitySet {
            entities: vec![make_entity("Alloy", "A"), make_entity("Alloy", "B")],
            relationships: vec![make_rel("A", "MAGIC_LINK", "B")],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(report.issues.iter().any(|i| i.category == "unknown_rel"));
    }

    #[test]
    fn detects_weight_out_of_range() {
        let es = EntitySet {
            entities: vec![make_entity("Alloy", "X"), make_entity("Element", "Y")],
            relationships: vec![Relationship {
                from: "X".into(),
                rel_type: "CONTAINS".into(),
                to: "Y".into(),
                weight: Some(1.5),
                order: None,
            }],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(report.issues.iter().any(|i| i.category == "invalid_weight"));
    }

    #[test]
    fn detects_weight_sum_mismatch() {
        let es = EntitySet {
            entities: vec![
                make_entity("Alloy", "ABC"),
                make_entity("Element", "A"),
                make_entity("Element", "B"),
            ],
            relationships: vec![
                Relationship {
                    from: "ABC".into(),
                    rel_type: "CONTAINS".into(),
                    to: "A".into(),
                    weight: Some(0.3),
                    order: None,
                },
                Relationship {
                    from: "ABC".into(),
                    rel_type: "CONTAINS".into(),
                    to: "B".into(),
                    weight: Some(0.3),
                    order: None,
                },
            ],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(report.issues.iter().any(|i| i.category == "weight_sum"));
    }

    #[test]
    fn empty_graph_fails() {
        let es = EntitySet {
            entities: vec![],
            relationships: vec![],
        };
        let report = validate_graph(&EmmoOntology, &es);
        assert!(!report.passed);
    }

    /// The vocabulary the validator accepts is the ACTIVE ontology's, not
    /// EMMO's: a chemistry ontology's facts validate clean under it and are
    /// flagged foreign under EMMO — and vice versa. Domain checks travel
    /// with the ontology too: EMMO's CONTAINS weight rules must not fire
    /// for an ontology that never declared CONTAINS.
    #[test]
    fn validation_follows_the_active_ontologys_vocabulary_not_emmos() {
        use crate::ontologies::{Ontology, UnitVocabulary};

        struct Chem;
        impl Ontology for Chem {
            fn id(&self) -> &'static str {
                "chem-gv"
            }
            fn entity_types(&self) -> &'static [&'static str] {
                &["Molecule"]
            }
            fn relationship_types(&self) -> &'static [&'static str] {
                &["REACTS_WITH"]
            }
            fn unit_vocabulary(&self) -> UnitVocabulary {
                UnitVocabulary {
                    name: "FREE",
                    prefix: None,
                }
            }
        }

        let chem_set = EntitySet {
            entities: vec![
                make_entity("Molecule", "H2O"),
                make_entity("Molecule", "O3"),
            ],
            relationships: vec![make_rel("H2O", "REACTS_WITH", "O3")],
        };
        let emmo_set = EntitySet {
            entities: vec![make_entity("Alloy", "Steel"), make_entity("Element", "Fe")],
            relationships: vec![make_rel("Steel", "CONTAINS", "Fe")],
        };

        // Chem facts under chem: fully in-vocabulary.
        let report = validate_graph(&Chem, &chem_set);
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.category == "unknown_type" || i.category == "unknown_rel"),
            "{:?}",
            report.issues
        );

        // Chem facts under EMMO: every type and relationship is foreign.
        let report = validate_graph(&EmmoOntology, &chem_set);
        assert_eq!(
            report
                .issues
                .iter()
                .filter(|i| i.category == "unknown_type")
                .count(),
            2
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.category == "unknown_rel" && i.message.contains("REACTS_WITH"))
        );

        // EMMO facts under chem: foreign the other way — and chem raises no
        // CONTAINS weight domain issues, because those rules are EMMO's.
        let report = validate_graph(&Chem, &emmo_set);
        assert_eq!(
            report
                .issues
                .iter()
                .filter(|i| i.category == "unknown_type")
                .count(),
            2
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.category == "unknown_rel" && i.message.contains("CONTAINS"))
        );
        assert!(
            !report.issues.iter().any(|i| i.category == "missing_weight"),
            "EMMO's domain checks fired under a non-EMMO ontology: {:?}",
            report.issues
        );

        // And under EMMO the same set is in-vocabulary, with EMMO's own
        // domain check (weightless CONTAINS → Info) present.
        let report = validate_graph(&EmmoOntology, &emmo_set);
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.category == "unknown_type" || i.category == "unknown_rel")
        );
        assert!(report.issues.iter().any(|i| i.category == "missing_weight"));
    }
}
