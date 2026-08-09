//! Putting an induced ontology on the extraction vocabulary path — through
//! the EXISTING process-wide registry in [`crate::ontologies`], not a
//! parallel one.
//!
//! This is where the draft gate bites: [`register_induced`] refuses a
//! `draft` artifact outright. A freshly induced ontology is a proposal; the
//! only way onto the path that governs production writes is the deliberate
//! promotion step ([`super::ttl::promote_artifact`]) followed by
//! registration. Both refusals are loud and name the next step.

use anyhow::{Result, bail};
use std::sync::Arc;

use super::{InducedOntology, OntologyStatus, class_slug, rel_type_token, validate};
use crate::ontologies::{Ontology, UnitVocabulary};

/// An induced vocabulary as an [`Ontology`] adapter. Entity types are the
/// classes' minted local names (PascalCase), relationship types the
/// UPPER_SNAKE tokens of the relation labels — the same house style as the
/// built-in EMMO vocabulary, and both deterministic from the labels.
///
/// The extraction prompt is the trait default, which derives its
/// instructions from exactly these declared lists — so an induced ontology
/// inherits the instruct-validate-store single-source contract for free.
struct InducedVocabulary {
    id: &'static str,
    entity_types: &'static [&'static str],
    relationship_types: &'static [&'static str],
}

impl Ontology for InducedVocabulary {
    fn id(&self) -> &'static str {
        self.id
    }
    fn entity_types(&self) -> &'static [&'static str] {
        self.entity_types
    }
    fn relationship_types(&self) -> &'static [&'static str] {
        self.relationship_types
    }
    fn unit_vocabulary(&self) -> UnitVocabulary {
        // Induced ontologies have no typed-unit path (that is QUDT-shaped by
        // construction on the text path); tabular units stay free-form.
        UnitVocabulary {
            name: "FREE",
            prefix: None,
        }
    }
}

/// Build the registry adapter for a VALID, ACCEPTED ontology. The strings
/// are leaked: the registry contract wants `&'static str`, and a registered
/// vocabulary lives for the process anyway — this is a bounded, once-per-
/// registration leak, not a per-call one.
fn adapter(ontology: &InducedOntology) -> Result<Arc<dyn Ontology>> {
    let id: &'static str = Box::leak(ontology.domain.clone().into_boxed_str());
    let entity_types: Vec<&'static str> = ontology
        .classes
        .iter()
        .filter_map(|c| class_slug(&c.label))
        .map(|s| &*Box::leak(s.into_boxed_str()))
        .collect();
    let relationship_types: Vec<&'static str> = ontology
        .relations
        .iter()
        .filter_map(|r| rel_type_token(&r.label))
        .map(|s| &*Box::leak(s.into_boxed_str()))
        .collect();
    if entity_types.is_empty() {
        bail!(
            "induced ontology '{}' declares no mintable classes — nothing to register",
            ontology.domain
        );
    }
    Ok(Arc::new(InducedVocabulary {
        id,
        entity_types: Box::leak(entity_types.into_boxed_slice()),
        relationship_types: Box::leak(relationship_types.into_boxed_slice()),
    }))
}

/// Register an induced ontology in the process-wide registry.
///
/// Three gates, all loud:
/// 1. **Status** — a `draft` is refused: it must not govern writes until
///    someone deliberately promotes it.
/// 2. **Validation** — the same [`validate::validate`] pass as everywhere
///    else; violations are listed, never repaired.
/// 3. **The registry's own two-call contract** — `register` refuses a taken
///    id; displacing a registered ontology stays a deliberate act via
///    [`crate::ontologies::replace_ontology`].
pub fn register_induced(ontology: &InducedOntology) -> Result<()> {
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
    crate::ontologies::register_ontology(adapter(ontology)?)
}

/// Load an artifact file and register it — the production dispatch for
/// "put this induced vocabulary on the extraction path". Parsing and
/// validation run inside [`super::load_validated`]; the draft gate runs in
/// [`register_induced`].
pub fn register_induced_from_path(path: &std::path::Path) -> Result<()> {
    let ontology = super::load_validated(path)?;
    register_induced(&ontology)
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
                    label: "Polymer".into(),
                    definition: "A macromolecular material.".into(),
                    parent: None,
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
    /// extraction vocabularies from) serves it with the minted vocabulary.
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
        assert_eq!(
            registered.entity_types(),
            ["GlassTransitionTemperature", "Polymer"],
            "entity types are the minted class local names"
        );
        assert_eq!(
            registered.relationship_types(),
            ["HAS_PROPERTY"],
            "relationship types are UPPER_SNAKE tokens"
        );
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
        o.classes[0].parent = Some("Polymer".into()); // self-cycle
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
