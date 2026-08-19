//! Seeding induction from ontologies that already exist.
//!
//! Induction used to start from two empty maps, so every run produced a tree
//! disconnected from anything the project already owned. Measured 19 Aug 2026
//! on a three-paper corpus: 122 items, every one `prism:alignment "unmatched"`,
//! zero `skos:exactMatch`, and no reference to EMMO or MatKG anywhere — while
//! every other path (`papers claims`, the paper reader, the extraction schema,
//! graph validation) already resolved the active ontology. Induction was the
//! one path that ignored the plugin plane.
//!
//! # Growth is by VALUE, not by reference
//!
//! An induced artifact cannot say `:MyClass rdfs:subClassOf emmo:Material`.
//! [`crate::induction::InducedClass::parent`] holds a LABEL, not an IRI;
//! `to_turtle` always writes the parent under the artifact's own namespace;
//! and `parse_turtle` degrades a foreign parent IRI to its bare local name,
//! which then fails `undeclared_parent`. Promotion rewrites the file again, so
//! even a hand-written foreign IRI would not survive.
//!
//! So a base is COPIED IN, and its identity is preserved the one way that does
//! round-trip: `aligned_iri`, emitted as `skos:exactMatch`. That is how a grown
//! ontology states that its `:Material` *is* EMMO's `EMMO_4207e895…` rather
//! than claiming sole authorship of it.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Result, bail};

use super::{InducedClass, InducedRelation, normalize_label};
use crate::ontologies::Ontology;

/// What one run inherited from one base ontology.
///
/// Stamped into provenance so a grown artifact can always answer "which of
/// these classes did the model actually contribute?" — without it the artifact
/// silently takes credit for its base.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedRef {
    /// Registry id of the base, e.g. `emmo`.
    pub id: String,
    /// The base's `owl:versionIRI` at the time it was read.
    pub version_iri: String,
    /// The base's artifact digest, so a changed base is detectable.
    pub artifact_sha256: String,
    /// Classes inherited from this base.
    pub classes: usize,
    /// Relations inherited from this base.
    pub relations: usize,
}

/// Everything a seeded run starts from.
#[derive(Debug, Clone, Default)]
pub struct Seed {
    pub classes: Vec<InducedClass>,
    pub relations: Vec<InducedRelation>,
    pub refs: Vec<SeedRef>,
    /// Multi-parent losses and other lossy conversions, carried into
    /// provenance rather than dropped.
    pub notes: Vec<String>,
}

impl Seed {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.relations.is_empty()
    }

    /// Labels of every seeded class, for the prompt's "already exists" block.
    #[must_use]
    pub fn class_labels(&self) -> Vec<&str> {
        self.classes.iter().map(|c| c.label.as_str()).collect()
    }
}

/// The name to carry a foreign declaration under.
///
/// `pref_label` is what a human curated. `extraction_labels` is what the
/// vocabulary tells a model to emit, and the registry guarantees at least one.
/// The IRI's local name is the last resort — for EMMO that is an opaque
/// `EMMO_4207e895_…` token, which is why it ranks last rather than first.
fn display_name(pref_label: Option<&str>, extraction_labels: &[String], iri: &str) -> String {
    if let Some(label) = pref_label.map(str::trim).filter(|l| !l.is_empty()) {
        return label.to_string();
    }
    if let Some(label) = extraction_labels.iter().find(|l| !l.trim().is_empty()) {
        return label.trim().to_string();
    }
    iri.rsplit(['#', '/'])
        .next()
        .unwrap_or(iri)
        .trim()
        .to_string()
}

/// Build the starting classes and relations from one or more base ontologies.
///
/// Reads `ontology_classes()` / `ontology_properties()` rather than
/// `classes()` / `relations()`. Those are the FULL navigable sets: for EMMO
/// that is 50 classes against the 8 extraction-facing ones, and the ancestors
/// are exactly the hierarchy a grown tree needs to hang from. For an ontology
/// that does not override them the two are identical, so this is never worse.
///
/// When two bases use the same word for different things, the second one is
/// QUALIFIED with its base id rather than fused or refused. EMMO and MatKG both
/// declare `Chemical`, and researching materials alongside another domain is the
/// point of seeding more than one base — so blocking on the collision would
/// forbid the main use case, while merging the two would silently claim that
/// EMMO's `Chemical` and MatKG's `Chemical` are the same concept. Keeping both
/// under distinguishable names asserts neither.
pub fn seed_from(bases: &[Arc<dyn Ontology>]) -> Result<Seed> {
    let mut seed = Seed::default();
    // normalised label -> (base id, surface form), for collision detection and
    // for resolving a parent IRI back to the label its class was seeded under.
    let mut claimed: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut iri_to_label: BTreeMap<String, String> = BTreeMap::new();

    for base in bases {
        let base_id = base.id().to_string();
        let mut classes = 0usize;
        let mut relations = 0usize;

        for decl in base.ontology_classes() {
            let iri = decl.iri.as_str().to_string();
            let label = display_name(decl.pref_label.as_deref(), &decl.extraction_labels, &iri);
            let key = normalize_label(&label);
            if key.is_empty() {
                seed.notes.push(format!(
                    "base {base_id}: class {iri} has no usable label and was not seeded"
                ));
                continue;
            }
            let (label, key) = match claimed.get(&key) {
                // The same base repeating itself: nothing to disambiguate.
                Some((owner, _)) if owner == &base_id => continue,
                Some((owner, _)) => {
                    let qualified = format!("{label} ({base_id})");
                    let qualified_key = normalize_label(&qualified);
                    if claimed.contains_key(&qualified_key) {
                        bail!(
                            "base {base_id} declares {label:?}, which base {owner} already \
                             claims, and the disambiguated form {qualified:?} is taken too. \
                             Rename one, or seed these bases in separate runs."
                        );
                    }
                    seed.notes.push(format!(
                        "class {qualified:?}: {label:?} was already claimed by base {owner}, \
                         so this one is qualified — the two are NOT asserted to be the same \
                         concept"
                    ));
                    (qualified, qualified_key)
                }
                None => (label, key),
            };
            claimed.insert(key, (base_id.clone(), label.clone()));
            iri_to_label.insert(iri.clone(), label.clone());
            seed.classes.push(InducedClass {
                label,
                definition: String::new(),
                // Resolved in a second pass: a parent IRI may name a class this
                // loop has not reached yet.
                parent: None,
                aligned_iri: Some(iri),
                declared_by_reference: false,
                sign_domain: None,
            });
            classes += 1;
        }

        for decl in base.ontology_properties() {
            let iri = decl.iri.as_str().to_string();
            let label = display_name(decl.pref_label.as_deref(), &decl.extraction_labels, &iri);
            if normalize_label(&label).is_empty() {
                continue;
            }
            let domain = decl
                .domains
                .first()
                .and_then(|d| iri_to_label.get(d.as_str()))
                .cloned();
            let range = decl
                .ranges
                .first()
                .and_then(|r| iri_to_label.get(r.as_str()))
                .cloned();
            // A relation whose endpoints are not seeded classes would fail
            // `undeclared_domain`/`undeclared_range`. Report it rather than
            // emitting an artifact that cannot validate.
            let (Some(domain), Some(range)) = (domain, range) else {
                seed.notes.push(format!(
                    "base {base_id}: relation {label:?} skipped — its domain or range is \
                     not among the seeded classes"
                ));
                continue;
            };
            seed.relations.push(InducedRelation {
                label,
                definition: String::new(),
                domain,
                range,
                aligned_iri: Some(iri),
                fact_kind: None,
            });
            relations += 1;
        }

        seed.refs.push(SeedRef {
            id: base_id,
            version_iri: base.version_iri().as_str().to_string(),
            artifact_sha256: base.artifact_sha256().to_string(),
            classes,
            relations,
        });
    }

    resolve_parents(bases, &iri_to_label, &mut seed);
    Ok(seed)
}

/// Second pass: turn each base's parent IRIs into the labels the classes were
/// seeded under.
///
/// `ClassDecl.parents` is a `Vec` — a class may have several. The artifact's
/// `InducedClass.parent` holds exactly one, so the first resolvable parent is
/// asserted and the rest are recorded as notes. Dropping them silently would
/// lose real structure from the customer's own ontology.
fn resolve_parents(
    bases: &[Arc<dyn Ontology>],
    iri_to_label: &BTreeMap<String, String>,
    seed: &mut Seed,
) {
    let mut parents: BTreeMap<String, (Option<String>, Vec<String>)> = BTreeMap::new();
    for base in bases {
        for decl in base.ontology_classes() {
            let resolved: Vec<String> = decl
                .parents
                .iter()
                .filter_map(|p| iri_to_label.get(p.as_str()).cloned())
                .collect();
            let unresolved: Vec<String> = decl
                .parents
                .iter()
                .filter(|p| !iri_to_label.contains_key(p.as_str()))
                .map(|p| p.as_str().to_string())
                .collect();
            let mut chosen = resolved.into_iter();
            let first = chosen.next();
            let extra: Vec<String> = chosen.chain(unresolved).collect();
            parents.insert(decl.iri.as_str().to_string(), (first, extra));
        }
    }

    for class in &mut seed.classes {
        let Some(iri) = class.aligned_iri.as_deref() else {
            continue;
        };
        let Some((first, extra)) = parents.get(iri) else {
            continue;
        };
        class.parent = first.clone();
        for other in extra {
            seed.notes.push(format!(
                "class {:?}: additional parent {other} not asserted — the artifact holds one \
                 parent per class",
                class.label
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_taken_from_the_most_human_source_available() {
        assert_eq!(
            display_name(
                Some("Material"),
                &["MATERIAL".into()],
                "https://x#EMMO_4207"
            ),
            "Material",
            "a curated prefLabel wins"
        );
        assert_eq!(
            display_name(None, &["MATERIAL".into()], "https://x#EMMO_4207"),
            "MATERIAL",
            "the extraction label is the fallback the registry guarantees"
        );
        assert_eq!(
            display_name(None, &[], "https://x#EMMO_4207"),
            "EMMO_4207",
            "the opaque local name is a last resort, never a first choice"
        );
        assert_eq!(
            display_name(Some("   "), &["Alloy".into()], "https://x#a"),
            "Alloy",
            "a blank prefLabel is not a label"
        );
    }
    use crate::ontologies::Ontology;
    use prism_ontology::{ClassDecl, Iri};

    struct Fake {
        id: &'static str,
        classes: Vec<ClassDecl>,
    }

    impl Ontology for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn version_iri(&self) -> &Iri {
            static V: std::sync::LazyLock<Iri> = std::sync::LazyLock::new(|| {
                Iri::new("https://example.org/v1".to_string()).unwrap()
            });
            &V
        }
        fn artifact_sha256(&self) -> &str {
            "b".repeat(64).leak()
        }
        fn classes(&self) -> &[ClassDecl] {
            &self.classes
        }
        fn relations(&self) -> &[crate::ontologies::RelationDecl] {
            &[]
        }
        fn is_a(&self, _sub: &Iri, _sup: &Iri) -> bool {
            false
        }
    }

    fn class(iri: &str, label: &str, parents: Vec<&str>) -> ClassDecl {
        ClassDecl {
            iri: Iri::new(iri.to_string()).unwrap(),
            pref_label: Some(label.to_string()),
            parents: parents
                .into_iter()
                .map(|p| Iri::new(p.to_string()).unwrap())
                .collect(),
            extraction_labels: vec![label.to_string()],
        }
    }

    #[test]
    fn a_seeded_class_keeps_its_base_identity_and_its_parent() {
        let base: Arc<dyn Ontology> = Arc::new(Fake {
            id: "fake",
            classes: vec![
                class("https://example.org#Material", "Material", vec![]),
                class(
                    "https://example.org#Alloy",
                    "Alloy",
                    vec!["https://example.org#Material"],
                ),
            ],
        });
        let seed = seed_from(&[base]).expect("seeds");

        assert_eq!(seed.classes.len(), 2);
        let alloy = seed.classes.iter().find(|c| c.label == "Alloy").unwrap();
        assert_eq!(
            alloy.aligned_iri.as_deref(),
            Some("https://example.org#Alloy"),
            "identity travels through aligned_iri — the one IRI channel that round-trips"
        );
        assert_eq!(
            alloy.parent.as_deref(),
            Some("Material"),
            "the base hierarchy is carried as labels, because a parent IRI cannot survive"
        );
        assert_eq!(seed.refs.len(), 1);
        assert_eq!(seed.refs[0].classes, 2);
    }

    #[test]
    fn two_bases_using_one_word_for_different_things_keep_both() {
        // The real case: EMMO and MatKG both declare "Chemical". Researching
        // two domains together is the reason to seed two bases, so this must
        // not block — and it must not quietly assert that the two are one.
        let a: Arc<dyn Ontology> = Arc::new(Fake {
            id: "materials",
            classes: vec![class("https://a#Cell", "Cell", vec![])],
        });
        let b: Arc<dyn Ontology> = Arc::new(Fake {
            id: "biology",
            classes: vec![class("https://b#Cell", "Cell", vec![])],
        });
        let seed = seed_from(&[a, b]).expect("two domains may be grown together");

        assert_eq!(seed.classes.len(), 2, "neither concept is dropped");
        let labels: Vec<&str> = seed.classes.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"Cell"),
            "the first keeps the plain name: {labels:?}"
        );
        assert!(
            labels.contains(&"Cell (biology)"),
            "the second is qualified, not fused: {labels:?}"
        );
        // Each still points at its OWN base, so neither claims to be the other.
        let biology = seed
            .classes
            .iter()
            .find(|c| c.label == "Cell (biology)")
            .unwrap();
        assert_eq!(biology.aligned_iri.as_deref(), Some("https://b#Cell"));
        assert!(
            seed.notes.iter().any(|n| n.contains("NOT asserted")),
            "and the decision is recorded: {:?}",
            seed.notes
        );
    }

    #[test]
    fn an_extra_parent_is_recorded_rather_than_dropped() {
        let base: Arc<dyn Ontology> = Arc::new(Fake {
            id: "fake",
            classes: vec![
                class("https://example.org#A", "A", vec![]),
                class("https://example.org#B", "B", vec![]),
                class(
                    "https://example.org#C",
                    "C",
                    vec!["https://example.org#A", "https://example.org#B"],
                ),
            ],
        });
        let seed = seed_from(&[base]).expect("seeds");
        let c = seed.classes.iter().find(|x| x.label == "C").unwrap();
        assert_eq!(c.parent.as_deref(), Some("A"), "one parent is asserted");
        assert!(
            seed.notes.iter().any(|n| n.contains("additional parent")),
            "and the other is SAID, not lost: {:?}",
            seed.notes
        );
    }
}
