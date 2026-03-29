//! Graph quality validation — SHACL-lite checks on extracted EntitySets.
//!
//! Validates structural integrity of the knowledge graph before/after
//! writing to Neo4j. Catches issues the LLM entity extraction might produce:
//!
//! - Orphan relationships (reference non-existent entities)
//! - Missing required properties (e.g. Alloy without name)
//! - Invalid relationship types
//! - Duplicate entities
//! - Weight/order constraints on relationships

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::EntitySet;

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

/// Known valid relationship types in the PRISM materials science ontology.
const VALID_REL_TYPES: &[&str] = &[
    "CONTAINS",
    "HAS_PROPERTY",
    "PROCESSED_BY",
    "OBSERVED_IN",
    "PUBLISHED_IN",
    "AUTHORED_BY",
    "PART_OF",
    "CITES",
];

/// Known valid entity types.
const VALID_ENTITY_TYPES: &[&str] = &[
    "Alloy", "Element", "Property", "Process", "Phase",
    "Paper", "Author", "Dataset", "Material",
];

/// Validate an EntitySet for structural integrity.
pub fn validate_graph(entities: &EntitySet) -> GraphValidationReport {
    let mut issues = Vec::new();

    // Build entity name lookup
    let entity_names: HashSet<&str> = entities
        .entities
        .iter()
        .map(|e| e.name.as_str())
        .collect();

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

    // Check 3: Unknown entity types
    for e in &entities.entities {
        if !VALID_ENTITY_TYPES.contains(&e.entity_type.as_str()) {
            issues.push(GraphIssue {
                severity: GraphSeverity::Warning,
                category: "unknown_type".into(),
                message: format!(
                    "Unknown entity type '{}' for '{}' — expected one of: {}",
                    e.entity_type,
                    e.name,
                    VALID_ENTITY_TYPES.join(", ")
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

    // Check 5: Orphan relationships — reference entities not in the set
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

    // Check 6: Unknown relationship types
    for r in &entities.relationships {
        if !VALID_REL_TYPES.contains(&r.rel_type.as_str()) {
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

    // Check 7: CONTAINS relationships should have weight
    for r in &entities.relationships {
        if r.rel_type == "CONTAINS" && r.weight.is_none() {
            issues.push(GraphIssue {
                severity: GraphSeverity::Info,
                category: "missing_weight".into(),
                message: format!(
                    "CONTAINS relationship {} → {} has no weight fraction",
                    r.from, r.to
                ),
            });
        }
    }

    // Check 8: CONTAINS weights should be 0.0..=1.0
    for r in &entities.relationships {
        if r.rel_type == "CONTAINS" {
            if let Some(w) = r.weight {
                if !(0.0..=1.0).contains(&w) {
                    issues.push(GraphIssue {
                        severity: GraphSeverity::Warning,
                        category: "invalid_weight".into(),
                        message: format!(
                            "CONTAINS {} → {}: weight {w} not in [0, 1]",
                            r.from, r.to
                        ),
                    });
                }
            }
        }
    }

    // Check 9: PROCESSED_BY should have order
    for r in &entities.relationships {
        if r.rel_type == "PROCESSED_BY" && r.order.is_none() {
            issues.push(GraphIssue {
                severity: GraphSeverity::Info,
                category: "missing_order".into(),
                message: format!(
                    "PROCESSED_BY {} → {} has no order",
                    r.from, r.to
                ),
            });
        }
    }

    // Check 10: CONTAINS weights for an alloy should sum to ~1.0
    let mut alloy_weights: HashMap<&str, f64> = HashMap::new();
    for r in &entities.relationships {
        if r.rel_type == "CONTAINS" {
            if let Some(w) = r.weight {
                *alloy_weights.entry(r.from.as_str()).or_default() += w;
            }
        }
    }
    for (alloy, total) in &alloy_weights {
        if *total > 0.0 && (*total - 1.0).abs() > 0.05 {
            issues.push(GraphIssue {
                severity: GraphSeverity::Warning,
                category: "weight_sum".into(),
                message: format!(
                    "Alloy '{alloy}' CONTAINS weights sum to {total:.3} (expected ~1.0)"
                ),
            });
        }
    }

    let has_errors = issues
        .iter()
        .any(|i| i.severity == GraphSeverity::Error);

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
        let report = validate_graph(&es);
        assert!(report.passed);
        assert!(report.issues.iter().all(|i| i.severity != GraphSeverity::Error));
    }

    #[test]
    fn detects_orphan_relationships() {
        let es = EntitySet {
            entities: vec![make_entity("Alloy", "Steel")],
            relationships: vec![make_rel("Steel", "CONTAINS", "Fe")], // Fe not in entities
        };
        let report = validate_graph(&es);
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.category == "orphan_rel"));
    }

    #[test]
    fn detects_empty_entity_name() {
        let es = EntitySet {
            entities: vec![make_entity("Alloy", "")],
            relationships: vec![],
        };
        let report = validate_graph(&es);
        assert!(!report.passed);
        assert!(report.issues.iter().any(|i| i.category == "empty_name"));
    }

    #[test]
    fn detects_duplicate_entities() {
        let es = EntitySet {
            entities: vec![
                make_entity("Element", "Fe"),
                make_entity("Element", "Fe"),
            ],
            relationships: vec![],
        };
        let report = validate_graph(&es);
        assert!(report.issues.iter().any(|i| i.category == "duplicate"));
    }

    #[test]
    fn detects_unknown_relationship_type() {
        let es = EntitySet {
            entities: vec![
                make_entity("Alloy", "A"),
                make_entity("Alloy", "B"),
            ],
            relationships: vec![make_rel("A", "MAGIC_LINK", "B")],
        };
        let report = validate_graph(&es);
        assert!(report.issues.iter().any(|i| i.category == "unknown_rel"));
    }

    #[test]
    fn detects_weight_out_of_range() {
        let es = EntitySet {
            entities: vec![
                make_entity("Alloy", "X"),
                make_entity("Element", "Y"),
            ],
            relationships: vec![Relationship {
                from: "X".into(),
                rel_type: "CONTAINS".into(),
                to: "Y".into(),
                weight: Some(1.5),
                order: None,
            }],
        };
        let report = validate_graph(&es);
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
        let report = validate_graph(&es);
        assert!(report.issues.iter().any(|i| i.category == "weight_sum"));
    }

    #[test]
    fn empty_graph_fails() {
        let es = EntitySet {
            entities: vec![],
            relationships: vec![],
        };
        let report = validate_graph(&es);
        assert!(!report.passed);
    }
}
