//! Validation of a FINISHED induced ontology.
//!
//! This is the strict layer: it never repairs. The builder already
//! normalised model noise while the draft was accumulating (and recorded
//! every normalisation), so a finished artifact that still violates these
//! rules — hand-edited, foreign, or produced by a builder bug — is rejected
//! loudly with the specific violations by [`crate::induction::load_validated`]
//! and by [`crate::induction::register::register_induced`].

use std::collections::{BTreeMap, BTreeSet};

use super::{InducedOntology, class_slug, normalize_label, rel_type_token, relation_slug};

/// One specific violation. `rule` is a stable machine id; `message` names
/// the offending labels so a rejection is actionable, not a shrug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: &'static str,
    pub message: String,
}

fn violation(rule: &'static str, message: String) -> Violation {
    Violation { rule, message }
}

/// Check a finished ontology. Empty result = valid. Checks, in order:
///
/// 1. `empty_label` — every class and relation label must normalise to
///    something non-empty.
/// 2. `duplicate_class_label` / `duplicate_relation_label` — no two classes
///    (or relations) may share a normalised prefLabel.
/// 3. `iri_collision` — two DIFFERENT normalised labels must not mint the
///    same IRI local name (e.g. "3d printing" vs "n3d printing").
/// 4. `undeclared_parent` / `undeclared_domain` / `undeclared_range` —
///    every reference must name a declared class.
/// 5. `subclass_cycle` — the `rdfs:subClassOf` graph must be acyclic; the
///    violation message spells out the cycle path.
#[must_use]
pub fn validate(ontology: &InducedOntology) -> Vec<Violation> {
    let mut violations = Vec::new();

    // 1. Empty labels.
    for class in &ontology.classes {
        if normalize_label(&class.label).is_empty() {
            violations.push(violation(
                "empty_label",
                format!("class with empty label (raw: {:?})", class.label),
            ));
        }
    }
    for rel in &ontology.relations {
        if normalize_label(&rel.label).is_empty() {
            violations.push(violation(
                "empty_label",
                format!("relation with empty label (raw: {:?})", rel.label),
            ));
        }
    }

    // 2. Duplicate prefLabels (normalised, so "HeatTreatment" duplicates
    //    "heat treatment").
    let mut seen_classes: BTreeMap<String, &str> = BTreeMap::new();
    for class in &ontology.classes {
        let key = normalize_label(&class.label);
        if key.is_empty() {
            continue; // already reported as empty_label
        }
        if let Some(first) = seen_classes.get(key.as_str()) {
            violations.push(violation(
                "duplicate_class_label",
                format!(
                    "classes {:?} and {:?} share the prefLabel {key:?}",
                    first, class.label
                ),
            ));
        } else {
            seen_classes.insert(key, &class.label);
        }
    }
    let mut seen_rels: BTreeMap<String, &str> = BTreeMap::new();
    for rel in &ontology.relations {
        let key = normalize_label(&rel.label);
        if key.is_empty() {
            continue;
        }
        if let Some(first) = seen_rels.get(key.as_str()) {
            violations.push(violation(
                "duplicate_relation_label",
                format!(
                    "relations {:?} and {:?} share the prefLabel {key:?}",
                    first, rel.label
                ),
            ));
        } else {
            seen_rels.insert(key, &rel.label);
        }
    }

    // 3. IRI local-name collisions across DISTINCT normalised labels.
    let mut class_slugs: BTreeMap<String, String> = BTreeMap::new();
    for key in seen_classes.keys() {
        if let Some(slug) = class_slug(key) {
            if let Some(other) = class_slugs.get(&slug) {
                violations.push(violation(
                    "iri_collision",
                    format!("classes {other:?} and {key:?} both mint the IRI local name {slug:?}"),
                ));
            } else {
                class_slugs.insert(slug, key.clone());
            }
        }
    }
    let mut rel_slugs: BTreeMap<String, String> = BTreeMap::new();
    for key in seen_rels.keys() {
        let minted = relation_slug(key).zip(rel_type_token(key));
        if let Some((slug, token)) = minted {
            let composite = format!("{slug}/{token}");
            if let Some(other) = rel_slugs.get(&composite) {
                violations.push(violation(
                    "iri_collision",
                    format!(
                        "relations {other:?} and {key:?} both mint the IRI local name {slug:?}"
                    ),
                ));
            } else {
                rel_slugs.insert(composite, key.clone());
            }
        }
    }

    // 4. Referential closure: parents, domains, ranges name declared classes.
    let declared: BTreeSet<String> = ontology
        .classes
        .iter()
        .map(|c| normalize_label(&c.label))
        .collect();
    for class in &ontology.classes {
        if let Some(parent) = &class.parent
            && !declared.contains(&normalize_label(parent))
        {
            violations.push(violation(
                "undeclared_parent",
                format!(
                    "class {:?} names parent {parent:?}, which is not a declared class",
                    class.label
                ),
            ));
        }
    }
    for rel in &ontology.relations {
        if !declared.contains(&normalize_label(&rel.domain)) {
            violations.push(violation(
                "undeclared_domain",
                format!(
                    "relation {:?} has domain {:?}, which is not a declared class",
                    rel.label, rel.domain
                ),
            ));
        }
        if !declared.contains(&normalize_label(&rel.range)) {
            violations.push(violation(
                "undeclared_range",
                format!(
                    "relation {:?} has range {:?}, which is not a declared class",
                    rel.label, rel.range
                ),
            ));
        }
    }

    // 5. subClassOf cycles. Walk up from every class; a walk that returns to
    //    its start is a cycle, reported once (for its lexicographically
    //    first member) with the full path spelled out.
    let parent_of: BTreeMap<String, String> = ontology
        .classes
        .iter()
        .filter_map(|c| {
            c.parent
                .as_ref()
                .map(|p| (normalize_label(&c.label), normalize_label(p)))
        })
        .collect();
    let mut reported: BTreeSet<String> = BTreeSet::new();
    for start in parent_of.keys() {
        let mut path = vec![start.clone()];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(start.clone());
        let mut current = start.clone();
        while let Some(next) = parent_of.get(&current) {
            path.push(next.clone());
            if next == start {
                // Report each cycle once: only from its smallest member.
                if path.iter().min() == Some(start) && reported.insert(start.clone()) {
                    violations.push(violation(
                        "subclass_cycle",
                        format!("subClassOf cycle: {}", path.join(" -> ")),
                    ));
                }
                break;
            }
            if !seen.insert(next.clone()) {
                break; // joined a cycle that does not pass through `start`
            }
            current = next.clone();
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::super::{
        InducedClass, InducedOntology, InducedRelation, InductionProvenance, OntologyStatus,
    };
    use super::*;

    fn class(label: &str, parent: Option<&str>) -> InducedClass {
        InducedClass {
            label: label.to_string(),
            definition: format!("def of {label}"),
            parent: parent.map(str::to_string),
            aligned_iri: None,
            declared_by_reference: false,
        }
    }

    fn relation(label: &str, domain: &str, range: &str) -> InducedRelation {
        InducedRelation {
            label: label.to_string(),
            definition: String::new(),
            domain: domain.to_string(),
            range: range.to_string(),
            aligned_iri: None,
        }
    }

    fn ontology(classes: Vec<InducedClass>, relations: Vec<InducedRelation>) -> InducedOntology {
        InducedOntology {
            domain: "testdom".into(),
            status: OntologyStatus::Draft,
            classes,
            relations,
            provenance: InductionProvenance::default(),
        }
    }

    #[test]
    fn valid_ontology_has_no_violations() {
        let o = ontology(
            vec![
                class("Alloy", Some("Material")),
                class("Material", None),
                class("Property", None),
            ],
            vec![relation("has property", "Alloy", "Property")],
        );
        assert_eq!(validate(&o), Vec::new());
    }

    #[test]
    fn two_node_cycle_is_named_in_the_violation() {
        let o = ontology(
            vec![
                class("Alloy", Some("Material")),
                class("Material", Some("Alloy")),
            ],
            vec![],
        );
        let v = validate(&o);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].rule, "subclass_cycle");
        assert!(
            v[0].message.contains("alloy -> material -> alloy"),
            "{}",
            v[0].message
        );
    }

    #[test]
    fn three_node_cycle_reported_once_with_full_path() {
        let o = ontology(
            vec![
                class("B", Some("C")),
                class("C", Some("A")),
                class("A", Some("B")),
            ],
            vec![],
        );
        let v = validate(&o);
        let cycles: Vec<_> = v.iter().filter(|v| v.rule == "subclass_cycle").collect();
        assert_eq!(cycles.len(), 1, "one cycle, one report: {v:?}");
        assert!(
            cycles[0].message.contains("a -> b -> c -> a"),
            "{}",
            cycles[0].message
        );
    }

    #[test]
    fn self_parent_is_a_cycle() {
        let o = ontology(vec![class("Ouroboros", Some("Ouroboros"))], vec![]);
        let v = validate(&o);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "subclass_cycle");
    }

    #[test]
    fn duplicate_class_pref_label_is_reported_even_across_spellings() {
        let o = ontology(
            vec![class("Heat Treatment", None), class("HeatTreatment", None)],
            vec![],
        );
        let v = validate(&o);
        assert!(
            v.iter().any(|v| v.rule == "duplicate_class_label"
                && v.message.contains("Heat Treatment")
                && v.message.contains("HeatTreatment")),
            "{v:?}"
        );
    }

    #[test]
    fn duplicate_relation_label_is_reported() {
        let o = ontology(
            vec![class("A", None), class("B", None)],
            vec![
                relation("has part", "A", "B"),
                relation("HAS_PART", "B", "A"),
            ],
        );
        let v = validate(&o);
        assert!(
            v.iter().any(|v| v.rule == "duplicate_relation_label"),
            "{v:?}"
        );
    }

    #[test]
    fn undeclared_domain_range_and_parent_are_reported_by_name() {
        let o = ontology(
            vec![class("Alloy", Some("Phantom"))],
            vec![
                relation("contains", "Alloy", "Ghost"),
                relation("made of", "Wraith", "Alloy"),
            ],
        );
        let v = validate(&o);
        let rules: Vec<&str> = v.iter().map(|v| v.rule).collect();
        assert!(rules.contains(&"undeclared_parent"), "{v:?}");
        assert!(rules.contains(&"undeclared_range"), "{v:?}");
        assert!(rules.contains(&"undeclared_domain"), "{v:?}");
        assert!(v.iter().any(|v| v.message.contains("Phantom")));
        assert!(v.iter().any(|v| v.message.contains("Ghost")));
        assert!(v.iter().any(|v| v.message.contains("Wraith")));
    }

    #[test]
    fn empty_labels_are_reported() {
        let o = ontology(vec![class("  ", None)], vec![relation("--", "x", "x")]);
        let v = validate(&o);
        assert!(
            v.iter().filter(|v| v.rule == "empty_label").count() >= 2,
            "{v:?}"
        );
    }

    #[test]
    fn distinct_labels_minting_one_iri_are_reported() {
        let o = ontology(
            vec![class("3d printing", None), class("N3d printing", None)],
            vec![],
        );
        let v = validate(&o);
        assert!(v.iter().any(|v| v.rule == "iri_collision"), "{v:?}");
    }
}
