//! Alignment of induced labels against standard vocabularies.
//!
//! Where an induced class already exists in a standard vocabulary (EMMO,
//! QUDT, …), the artifact should SAY SO rather than mint a rival identity.
//! Matching is by `skos:prefLabel`, compared through the same
//! [`super::normalize_label`] the induction merge uses, so `"HeatTreatment"`
//! (EMMO style) matches `"heat treatment"` (model style).
//!
//! A match becomes `skos:exactMatch` on the induced class — a mapping
//! claim, deliberately weaker than `owl:equivalentClass`: an LLM-induced
//! label agreeing with a curated label is evidence of sameness, not proof
//! of logical equivalence, and a wrong `exactMatch` misleads a reader
//! while a wrong `equivalentClass` corrupts every reasoner downstream.
//! Unmatched classes keep their PRISM IRI and are RECORDED as unmatched
//! (`prism:alignment "unmatched"`), so alignment is a visible later step,
//! never a silent omission.
//!
//! The alignment source is always an explicit input (a `.ttl` file or a
//! directory of them). Nothing here hard-codes a vendored ontology path.

use anyhow::{Context, Result, anyhow, bail};
use sophia::api::ns::Namespace;
use sophia::api::prelude::*;
use sophia::api::term::Term;
use sophia::api::term::matcher::Any;
use sophia::inmem::graph::FastGraph;
use sophia::turtle::parser::turtle;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{InducedOntology, normalize_label};

/// prefLabel → IRI index over one or more reference vocabularies.
#[derive(Debug, Default)]
pub struct AlignmentIndex {
    /// Normalised prefLabel → IRI. First declaration wins deterministically
    /// (files are read in sorted order, labels compared sorted).
    by_label: BTreeMap<String, String>,
    /// Where the index came from, for reporting.
    pub sources: Vec<String>,
}

/// Outcome of aligning one ontology, for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignmentOutcome {
    pub matched: usize,
    /// Labels that stayed unmatched — also visible per-class in the artifact.
    pub unmatched: Vec<String>,
}

impl AlignmentIndex {
    /// Look up a label (normalised) in the index.
    #[must_use]
    pub fn lookup(&self, label: &str) -> Option<&str> {
        self.by_label
            .get(&normalize_label(label))
            .map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_label.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_label.is_empty()
    }
}

/// Build an index from `.ttl` files. Each path may be a file or a directory
/// (searched recursively, sorted). A file that does not parse is an error —
/// silently skipping a reference vocabulary would fake "unmatched".
pub fn load_alignment(paths: &[PathBuf]) -> Result<AlignmentIndex> {
    let mut files: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
        } else if path.is_dir() {
            collect_ttl(path, &mut files)?;
        } else {
            bail!("alignment source {} does not exist", path.display());
        }
    }
    files.sort();
    if files.is_empty() {
        bail!("no .ttl files found in the given alignment source(s)");
    }

    let skos = Namespace::new_unchecked("http://www.w3.org/2004/02/skos/core#");
    let skos_pref = skos.get_unchecked("prefLabel");
    let mut index = AlignmentIndex::default();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("cannot read alignment source {}", file.display()))?;
        let graph: FastGraph = turtle::parse_str(&text).collect_triples().map_err(|e| {
            anyhow!(
                "alignment source {} is not valid Turtle: {e}",
                file.display()
            )
        })?;

        // Collect (label, iri) pairs, then insert in sorted order so which
        // IRI wins a duplicate label is deterministic, not hash-ordered.
        let mut pairs: Vec<(String, String)> = graph
            .triples_matching(Any, [skos_pref], Any)
            .filter_map(|t| {
                let t = t.ok()?;
                let iri = t.s().iri()?.as_str().to_string();
                let label = t.o().lexical_form()?.to_string();
                Some((normalize_label(&label), iri))
            })
            .filter(|(label, _)| !label.is_empty())
            .collect();
        pairs.sort();
        for (label, iri) in pairs {
            index.by_label.entry(label).or_insert(iri);
        }
        index.sources.push(file.display().to_string());
    }
    Ok(index)
}

fn collect_ttl(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("cannot read alignment directory {}", dir.display()))?
    {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            collect_ttl(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ttl"))
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Align classes and relations in place; returns the outcome for reporting.
/// Only fills `aligned_iri` — never renames, never removes, never merges.
pub fn align(ontology: &mut InducedOntology, index: &AlignmentIndex) -> AlignmentOutcome {
    let mut matched = 0usize;
    let mut unmatched = Vec::new();
    for class in &mut ontology.classes {
        match index.lookup(&class.label) {
            Some(iri) => {
                class.aligned_iri = Some(iri.to_string());
                matched += 1;
            }
            None => unmatched.push(class.label.clone()),
        }
    }
    for rel in &mut ontology.relations {
        match index.lookup(&rel.label) {
            Some(iri) => {
                rel.aligned_iri = Some(iri.to_string());
                matched += 1;
            }
            None => unmatched.push(rel.label.clone()),
        }
    }
    AlignmentOutcome { matched, unmatched }
}

#[cfg(test)]
mod tests {
    use super::super::{
        InducedClass, InducedOntology, InducedRelation, InductionProvenance, OntologyStatus,
    };
    use super::*;

    const REFERENCE_TTL: &str = r#"
        @prefix skos: <http://www.w3.org/2004/02/skos/core#> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        <https://w3id.org/emmo#EMMO_alloy> a owl:Class ;
            skos:prefLabel "Alloy"@en .
        <https://w3id.org/emmo#EMMO_ht> a owl:Class ;
            skos:prefLabel "HeatTreatment"@en .
        <https://w3id.org/emmo#EMMO_hasProperty> a owl:ObjectProperty ;
            skos:prefLabel "hasProperty"@en .
    "#;

    fn ontology() -> InducedOntology {
        InducedOntology {
            domain: "alloys".into(),
            status: OntologyStatus::Draft,
            classes: vec![
                InducedClass {
                    label: "Alloy".into(),
                    definition: String::new(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                },
                InducedClass {
                    label: "heat treatment".into(),
                    definition: String::new(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                },
                InducedClass {
                    label: "Unobtainium Widget".into(),
                    definition: String::new(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                },
            ],
            relations: vec![InducedRelation {
                label: "has property".into(),
                definition: String::new(),
                domain: "Alloy".into(),
                range: "Alloy".into(),
                aligned_iri: None,
            }],
            provenance: InductionProvenance::default(),
        }
    }

    #[test]
    fn labels_match_across_camel_case_and_spacing() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ref.ttl");
        std::fs::write(&f, REFERENCE_TTL).unwrap();
        let index = load_alignment(&[f]).unwrap();
        assert_eq!(index.len(), 3);

        let mut o = ontology();
        let outcome = align(&mut o, &index);
        assert_eq!(outcome.matched, 3);
        assert_eq!(outcome.unmatched, vec!["Unobtainium Widget".to_string()]);
        assert_eq!(
            o.classes[0].aligned_iri.as_deref(),
            Some("https://w3id.org/emmo#EMMO_alloy")
        );
        assert_eq!(
            o.classes[1].aligned_iri.as_deref(),
            Some("https://w3id.org/emmo#EMMO_ht"),
            "camelCase EMMO label matches spaced induced label"
        );
        assert_eq!(o.classes[2].aligned_iri, None, "no invented matches");
        assert_eq!(
            o.relations[0].aligned_iri.as_deref(),
            Some("https://w3id.org/emmo#EMMO_hasProperty")
        );
    }

    #[test]
    fn unparseable_alignment_source_is_an_error_not_a_skip() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("broken.ttl");
        std::fs::write(&f, "not turtle at all {{{").unwrap();
        let err = load_alignment(&[f]).unwrap_err();
        assert!(format!("{err:#}").contains("not valid Turtle"), "{err:#}");
    }

    #[test]
    fn missing_alignment_source_is_an_error() {
        let err = load_alignment(&[PathBuf::from("/nonexistent/emmo.ttl")]).unwrap_err();
        assert!(format!("{err:#}").contains("does not exist"), "{err:#}");
    }

    #[test]
    fn directory_source_finds_ttl_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("ref.ttl"), REFERENCE_TTL).unwrap();
        std::fs::write(dir.path().join("notes.md"), "not a ttl").unwrap();
        let index = load_alignment(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(index.len(), 3);
    }
}
