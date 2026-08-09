//! Putting an induced ontology on the extraction vocabulary path — through
//! the EXISTING process-wide registry in [`crate::ontologies`], not a
//! parallel one.
//!
//! This is where the draft gate bites: [`register_induced`] refuses a
//! `draft` artifact outright. A freshly induced ontology is a proposal; the
//! only way onto the path that governs production writes is the deliberate
//! promotion step ([`super::ttl::promote_artifact`]) followed by
//! registration. Both refusals are loud and name the next step.
//!
//! The trait's identity questions are answered HONESTLY, not stubbed: an
//! induced ontology already mints real IRIs, already carries a version IRI
//! derived from (prompt version, corpus hash), and its artifact is a
//! concrete Turtle document — so `version_iri()` is the artifact's own
//! `owl:versionIRI` and `artifact_sha256()` is the SHA-256 of the exact
//! materialised TTL backing the declaration.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{
    InducedOntology, OntologyStatus, class_slug, domain_namespace, rel_type_token, relation_slug,
    validate,
};
use crate::ontologies::{ClassDecl, Iri, Ontology, RelationDecl};

/// An induced vocabulary as an [`Ontology`] adapter.
///
/// Each induced class becomes a [`ClassDecl`]: canonical PRISM IRI, the
/// model's label as `pref_label`, its (validated-acyclic) parent, and the
/// minted PascalCase local name as the single extraction label. Relations
/// follow the same shape with UPPER_SNAKE extraction tokens — the house
/// style of the built-in EMMO vocabulary — so the trait-default extraction
/// prompt instructs exactly the vocabulary the validator and store accept.
struct InducedVocabulary {
    /// Leaked once per registration: the registry keys on `&'static str`.
    id: &'static str,
    version_iri: Iri,
    /// 64 lowercase hex chars over the exact materialised TTL artifact.
    artifact_sha256: String,
    classes: Vec<ClassDecl>,
    relations: Vec<RelationDecl>,
    /// Direct-parent map (IRI string → parent IRI strings) for `is_a`.
    parents: HashMap<String, Vec<String>>,
}

impl Ontology for InducedVocabulary {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version_iri(&self) -> &Iri {
        &self.version_iri
    }

    fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    fn classes(&self) -> &[ClassDecl] {
        &self.classes
    }

    fn relations(&self) -> &[RelationDecl] {
        &self.relations
    }

    /// Equal-or-transitively-below over the induced `rdfs:subClassOf`
    /// links. The registered hierarchy is validated acyclic, but the walk
    /// carries a visited set anyway — the trait demands cycle safety, not
    /// cycle absence.
    fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
        if sub.as_str() == sup.as_str() {
            return true;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = vec![sub.as_str()];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            if let Some(parents) = self.parents.get(current) {
                for parent in parents {
                    if parent == sup.as_str() {
                        return true;
                    }
                    stack.push(parent);
                }
            }
        }
        false
    }
}

/// Build the registry adapter for a VALID, ACCEPTED ontology.
/// `artifact_sha256` is supplied by the caller because only the caller
/// knows which materialisation backs this registration (the file bytes on
/// disk, or the canonical serialisation of an in-memory ontology).
///
/// Only the id string is leaked — the registry contract wants a
/// `&'static str` key; everything else is owned by the adapter.
fn adapter(ontology: &InducedOntology, artifact_sha256: String) -> Result<Arc<dyn Ontology>> {
    let ns = domain_namespace(&ontology.domain);
    let class_iri = |label: &str| -> Result<Iri> {
        let slug = class_slug(label)
            .ok_or_else(|| anyhow::anyhow!("class label {label:?} mints no IRI local name"))?;
        Iri::new(format!("{ns}{slug}"))
            .map_err(|e| anyhow::anyhow!("class label {label:?} mints an invalid IRI: {e}"))
    };

    let mut classes = Vec::with_capacity(ontology.classes.len());
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    for class in &ontology.classes {
        let iri = class_iri(&class.label)?;
        let mut parent_iris = Vec::new();
        if let Some(parent) = &class.parent {
            let parent_iri = class_iri(parent)?;
            parents
                .entry(iri.as_str().to_string())
                .or_default()
                .push(parent_iri.as_str().to_string());
            parent_iris.push(parent_iri);
        }
        let extraction_label = class_slug(&class.label)
            .expect("class_iri above already proved the label mints a local name");
        classes.push(ClassDecl {
            iri,
            pref_label: Some(class.label.clone()),
            parents: parent_iris,
            extraction_labels: vec![extraction_label],
        });
    }
    if classes.is_empty() {
        bail!(
            "induced ontology '{}' declares no mintable classes — nothing to register",
            ontology.domain
        );
    }

    let mut relations = Vec::with_capacity(ontology.relations.len());
    for rel in &ontology.relations {
        let slug = relation_slug(&rel.label)
            .ok_or_else(|| anyhow::anyhow!("relation label {:?} mints no IRI", rel.label))?;
        let token = rel_type_token(&rel.label)
            .ok_or_else(|| anyhow::anyhow!("relation label {:?} mints no type token", rel.label))?;
        relations.push(RelationDecl {
            iri: Iri::new(format!("{ns}{slug}")).map_err(|e| {
                anyhow::anyhow!("relation label {:?} mints an invalid IRI: {e}", rel.label)
            })?,
            pref_label: Some(rel.label.clone()),
            extraction_labels: vec![token],
        });
    }

    let version_iri = Iri::new(ontology.version_iri()).map_err(|e| {
        anyhow::anyhow!(
            "induced ontology '{}' has an invalid version IRI: {e}",
            ontology.domain
        )
    })?;

    Ok(Arc::new(InducedVocabulary {
        id: Box::leak(ontology.domain.clone().into_boxed_str()),
        version_iri,
        artifact_sha256,
        classes,
        relations,
        parents,
    }))
}

/// SHA-256 (64 lowercase hex chars, the registry's required shape) of the
/// canonical Turtle materialisation of `ontology`.
fn canonical_artifact_sha256(ontology: &InducedOntology) -> String {
    hex::encode(Sha256::digest(super::ttl::to_turtle(ontology).as_bytes()))
}

/// Register an induced ontology in the process-wide registry, backed by the
/// SHA-256 of its canonical serialisation.
///
/// Three gates, all loud:
/// 1. **Status** — a `draft` is refused: it must not govern writes until
///    someone deliberately promotes it.
/// 2. **Validation** — the same [`validate::validate`] pass as everywhere
///    else; violations are listed, never repaired.
/// 3. **The registry's own contracts** — declaration well-formedness is
///    re-checked by registration itself, and `register` refuses a taken id;
///    displacing a registered ontology stays a deliberate act via
///    [`crate::ontologies::replace_ontology`].
pub fn register_induced(ontology: &InducedOntology) -> Result<()> {
    register_induced_with_sha(ontology, canonical_artifact_sha256(ontology))
}

fn register_induced_with_sha(ontology: &InducedOntology, artifact_sha256: String) -> Result<()> {
    match ontology.status {
        OntologyStatus::Draft => bail!(
            "ontology '{}' is a DRAFT — a freshly induced ontology is a proposal and \
             must not govern production writes. Review the artifact, then promote it \
             deliberately: `prism ontology promote <artifact.ttl>`",
            ontology.domain
        ),
        OntologyStatus::Accepted => {}
    }
    let violations = validate::validate(ontology);
    if !violations.is_empty() {
        let mut msg = format!(
            "induced ontology '{}' REJECTED at registration: {} violation(s):",
            ontology.domain,
            violations.len()
        );
        for v in &violations {
            msg.push_str(&format!("\n  [{}] {}", v.rule, v.message));
        }
        bail!(msg);
    }
    crate::ontologies::register_ontology(adapter(ontology, artifact_sha256)?)
}

/// Load an artifact file and register it — the production dispatch for
/// "put this induced vocabulary on the extraction path". Parsing and
/// validation run inside [`super::load_validated`]; the draft gate runs in
/// [`register_induced_with_sha`]. The registered `artifact_sha256` is the
/// hash of the FILE BYTES as they exist on disk — the artifact as shipped,
/// not a re-serialisation of it.
pub fn register_induced_from_path(path: &std::path::Path) -> Result<()> {
    let ontology = super::load_validated(path)?;
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot re-read ontology artifact {}", path.display()))?;
    register_induced_with_sha(&ontology, hex::encode(Sha256::digest(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::super::ttl::{promote_artifact, write_artifact};
    use super::super::{InducedClass, InducedRelation, InductionProvenance};
    use super::*;

    /// Unique-per-test domains: the registry is process-wide and
    /// registration is permanent for the process, so every test uses ids
    /// nothing else claims.
    fn ontology(domain: &str) -> InducedOntology {
        InducedOntology {
            domain: domain.into(),
            status: OntologyStatus::Draft,
            classes: vec![
                InducedClass {
                    label: "Material".into(),
                    definition: "Physical substance.".into(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                },
                InducedClass {
                    label: "Polymer".into(),
                    definition: "A macromolecular material.".into(),
                    parent: Some("Material".into()),
                    aligned_iri: None,
                    declared_by_reference: false,
                },
                InducedClass {
                    label: "Glass Transition Temperature".into(),
                    definition: String::new(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                },
            ],
            relations: vec![InducedRelation {
                label: "has property".into(),
                definition: String::new(),
                domain: "Polymer".into(),
                range: "Glass Transition Temperature".into(),
                aligned_iri: None,
            }],
            provenance: InductionProvenance {
                corpus_hash: "sha256:deadbeef00".into(),
                prompt_version: "1".into(),
                ..Default::default()
            },
        }
    }

    /// THE draft gate, driven through the production file path: a draft
    /// artifact on disk must be refused by `register_induced_from_path`,
    /// and the process-wide registry must remain untouched.
    #[test]
    fn draft_artifact_cannot_govern_writes_without_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("draft.ttl");
        write_artifact(&path, &ontology("indtest-draftgate")).unwrap();

        let err = register_induced_from_path(&path).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("DRAFT"), "{msg}");
        assert!(msg.contains("prism ontology promote"), "{msg}");

        // The production registry never saw it.
        assert!(
            crate::ontologies::registry()
                .get("indtest-draftgate")
                .is_none(),
            "a refused draft must not reach the process-wide registry"
        );
    }

    /// The full promotion path: promote the artifact file, register it, and
    /// the PRODUCTION registry (the same one `ontologies::active` resolves
    /// extraction vocabularies from) serves it — with real IRIs, an honest
    /// version IRI and artifact hash, and a working subsumption answer.
    #[test]
    fn promoted_artifact_registers_into_the_production_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accepted.ttl");
        write_artifact(&path, &ontology("indtest-promoted")).unwrap();

        promote_artifact(&path).unwrap();
        register_induced_from_path(&path).unwrap();

        let registered = crate::ontologies::active(Some("indtest-promoted"))
            .expect("an accepted, registered ontology resolves as an active vocabulary");
        assert_eq!(registered.id(), "indtest-promoted");

        // Entity types are the minted class local names, each backed by a
        // canonical PRISM IRI carrying the model's label as prefLabel.
        let mut labels: Vec<&str> = registered
            .classes()
            .iter()
            .flat_map(|c| c.extraction_labels.iter())
            .map(String::as_str)
            .collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            ["GlassTransitionTemperature", "Material", "Polymer"],
            "entity types are the minted class local names"
        );
        let polymer = registered
            .class_for_label("Polymer")
            .expect("extraction label resolves to its declaration")
            .clone();
        assert_eq!(
            polymer.iri.as_str(),
            "https://prism.marc27.com/ontology/indtest-promoted#Polymer",
            "classes carry real PRISM-namespace IRIs"
        );
        assert_eq!(polymer.pref_label.as_deref(), Some("Polymer"));

        // Relationship types are UPPER_SNAKE tokens.
        let rel_labels: Vec<&str> = registered
            .relations()
            .iter()
            .flat_map(|r| r.extraction_labels.iter())
            .map(String::as_str)
            .collect();
        assert_eq!(rel_labels, ["HAS_PROPERTY"]);

        // version_iri and artifact_sha256 are answered honestly, not stubbed:
        // the version IRI is the artifact's own owl:versionIRI (prompt
        // version + corpus hash), and the hash is 64 lowercase hex over the
        // file bytes that were registered.
        assert_eq!(
            registered.version_iri().as_str(),
            "https://prism.marc27.com/ontology/indtest-promoted/version/1.deadbeef",
        );
        let sha = registered.artifact_sha256();
        assert_eq!(sha.len(), 64, "{sha}");
        assert!(
            sha.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let file_sha = hex::encode(Sha256::digest(std::fs::read(&path).unwrap()));
        assert_eq!(
            sha, file_sha,
            "the registered hash is the artifact file's hash"
        );

        // Subsumption over the induced hierarchy: Polymer is-a Material,
        // never the reverse, and is_a is reflexive.
        let material = registered.class_for_label("Material").unwrap().clone();
        assert!(
            registered.is_a(&polymer.iri, &material.iri),
            "Polymer is-a Material"
        );
        assert!(
            !registered.is_a(&material.iri, &polymer.iri),
            "not the reverse"
        );
        assert!(registered.is_a(&polymer.iri, &polymer.iri), "reflexive");

        // The derived extraction prompt instructs exactly this vocabulary.
        let instructions = registered.extraction_instructions();
        assert!(instructions.contains("Polymer"), "{instructions}");
        assert!(instructions.contains("HAS_PROPERTY"), "{instructions}");
    }

    /// Registration honours the registry's two-call contract: a taken id is
    /// refused by `register`, not silently displaced.
    #[test]
    fn registration_refuses_a_taken_id() {
        let mut o = ontology("indtest-taken");
        o.status = OntologyStatus::Accepted;
        register_induced(&o).unwrap();
        let err = register_induced(&o).unwrap_err();
        assert!(format!("{err:#}").contains("already registered"), "{err:#}");
    }

    /// An accepted-but-invalid ontology is still rejected at registration:
    /// promotion and registration each run the validator — no path around it.
    #[test]
    fn invalid_accepted_ontology_is_rejected_with_violations() {
        let mut o = ontology("indtest-invalid");
        o.status = OntologyStatus::Accepted;
        o.classes[0].parent = Some("Material".into()); // self-cycle on Material
        let err = register_induced(&o).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("REJECTED"), "{msg}");
        assert!(msg.contains("subclass_cycle"), "{msg}");
        assert!(
            crate::ontologies::registry()
                .get("indtest-invalid")
                .is_none()
        );
    }
}
