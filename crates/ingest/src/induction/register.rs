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
use prism_provenance::QuantitySignDomain;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{
    InducedFactKind, InducedOntology, OntologyStatus, class_slug, domain_namespace, rel_type_token,
    relation_slug, validate,
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
    /// `prism:signDomain` annotations from the artifact, keyed by class IRI.
    sign_domains: HashMap<String, QuantitySignDomain>,
    /// Class IRI by every name a reader may bind — the IRI itself, the
    /// prefLabel and the extraction label — so [`Ontology::quantity_sign_domain`]
    /// answers whichever identity the fact carried.
    class_iri_by_name: HashMap<String, String>,
    /// Extraction tokens of the relations the artifact typed, grouped by the
    /// store's typed fact shape. Empty for a kind the ontology never
    /// declared — those relations stay generic edges, which is honest.
    fact_kind_relations: HashMap<InducedFactKind, Vec<String>>,
    /// The RANGE class extraction label of the first relation declaring each
    /// typed fact shape — the ontology's own class for that shape's object
    /// node, served through [`Ontology::fact_graph_shape`].
    fact_kind_ranges: HashMap<InducedFactKind, String>,
    /// Extraction labels of the classes that STATE a measured quantity: the
    /// ranges of the declared measurement relations, plus everything below
    /// them in the declared hierarchy — the same at-or-below rule the
    /// built-in vocabulary applies to its own property root.
    quantitative_labels: Vec<String>,
}

impl InducedVocabulary {
    /// The declared extraction tokens for one typed fact shape.
    fn relations_of_kind(&self, kind: InducedFactKind) -> Vec<&str> {
        self.fact_kind_relations
            .get(&kind)
            .map(|tokens| tokens.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
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

    /// Serves the artifact's `prism:factKind` declarations. WHICH relation
    /// fills the store's `measurement` shape is the ontology's statement; if
    /// it declares none, this is empty and every numeric claim is reported as
    /// unstorable rather than guessed into a frozen English token.
    fn measurement_relations(&self) -> Vec<&str> {
        self.relations_of_kind(InducedFactKind::Measurement)
    }

    fn phase_relations(&self) -> Vec<&str> {
        self.relations_of_kind(InducedFactKind::Phase)
    }

    fn processing_relations(&self) -> Vec<&str> {
        self.relations_of_kind(InducedFactKind::Processing)
    }

    fn contains_relations(&self) -> Vec<&str> {
        self.relations_of_kind(InducedFactKind::Contains)
    }

    /// The artifact's own `prism:factKind` declarations, shaped from its own
    /// classes and relation tokens: the object node falls back to the
    /// relation's declared RANGE class, and the typed edge carries the
    /// relation's own token. The store's edge-prop conventions for the
    /// value channel (`fraction`, `order`) are wire-format keys, not domain
    /// vocabulary. A kind the artifact never declared answers `None` — the
    /// honest generic edge, never a frozen built-in table.
    fn fact_graph_shape(&self, kind: &str) -> Option<prism_provenance::FactGraphShape> {
        let induced = super::fact_kind_from_stored(kind)?;
        let edge_rel_type = self.fact_kind_relations.get(&induced)?.first()?.clone();
        let object_storage_label = self.fact_kind_ranges.get(&induced)?.clone();
        Some(match induced {
            InducedFactKind::Measurement => prism_provenance::FactGraphShape {
                object_storage_label,
                edge_rel_type,
                reified_measurement: true,
                object_text_prop: None,
                edge_value_prop: None,
            },
            InducedFactKind::Phase => prism_provenance::FactGraphShape {
                object_storage_label,
                edge_rel_type,
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: None,
            },
            InducedFactKind::Processing => prism_provenance::FactGraphShape {
                object_storage_label,
                edge_rel_type,
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: Some("order".into()),
            },
            InducedFactKind::Contains => prism_provenance::FactGraphShape {
                object_storage_label,
                edge_rel_type,
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: Some("fraction".into()),
            },
        })
    }

    /// Only the `contains` shape carries an unconditioned fraction this
    /// ontology declared; a kind the artifact never typed is not eligible.
    fn numeric_prior_fact_kinds(&self) -> Vec<String> {
        if self
            .fact_kind_relations
            .contains_key(&InducedFactKind::Contains)
        {
            vec!["contains".into()]
        } else {
            Vec::new()
        }
    }

    /// Derived from the declaration, never a frozen list: the range classes
    /// of the declared measurement relations and their declared descendants.
    fn quantitative_labels(&self) -> Vec<&str> {
        self.quantitative_labels
            .iter()
            .map(String::as_str)
            .collect()
    }

    /// Serves the artifact's optional `prism:signDomain` annotations — the
    /// sign constraint lives in the ontology, not in Rust. The query may
    /// carry any identity the reader bound (class IRI, prefLabel or
    /// extraction label), and a declaration on a DIMENSIONAL PARENT also
    /// applies: the walk follows declared ancestors until one carries the
    /// annotation. No declaration anywhere on that path answers `None` —
    /// silence, never a guess.
    ///
    /// Name resolution is NORMALIZED ([`normalize_label`](super::normalize_label)),
    /// exactly like every other induction lookup (duplicate detection,
    /// validate): "Yield Strength", "yield strength" and "yield_strength"
    /// are ONE class here, so a fact whose object spelling differs in
    /// case/separators still resolves to the declared sign domain. The old
    /// exact-match map was a silent false negative on this guard — duplicate
    /// detection folded the spellings while the sign-domain lookup did not.
    fn quantity_sign_domain(&self, quantity: &str) -> Option<QuantitySignDomain> {
        // DEFECT FIX (Part 2): normalized lookup, like validate.rs. Keys in
        // `class_iri_by_name` are inserted under `normalize_label` (see the
        // adapter builder); the query folds the same way.
        let normalized = super::normalize_label(quantity);
        let root = self
            .class_iri_by_name
            .get(&normalized)
            .map_or(quantity, String::as_str);
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = vec![root];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            if let Some(domain) = self.sign_domains.get(current) {
                return Some(*domain);
            }
            if let Some(parents) = self.parents.get(current) {
                for parent in parents {
                    stack.push(parent.as_str());
                }
            }
        }
        None
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
    let mut sign_domains: HashMap<String, QuantitySignDomain> = HashMap::new();
    let mut class_iri_by_name: HashMap<String, String> = HashMap::new();
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
        // DEFECT FIX (Part 2): keys are NORMALIZED so the lookup in
        // `quantity_sign_domain` folds case/separators exactly like the
        // rest of induction (duplicate detection, validate). Inserting the
        // raw spellings made "yield strength" miss "Yield Strength"'s
        // declared sign domain — a silent false negative on a guard.
        for name in [
            iri.as_str(),
            class.label.as_str(),
            extraction_label.as_str(),
        ] {
            class_iri_by_name.insert(super::normalize_label(name), iri.as_str().to_string());
        }
        if let Some(domain) = class.sign_domain {
            sign_domains.insert(iri.as_str().to_string(), domain);
        }
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
    // Extraction tokens grouped by the typed fact shape the artifact declares
    // for each relation, plus the range classes of the MEASUREMENT relations —
    // those are the ontology's quantity classes, so `quantitative_labels`
    // follows from the same declaration instead of a second annotation.
    let mut by_fact_kind: HashMap<InducedFactKind, Vec<String>> = HashMap::new();
    let mut fact_kind_ranges: HashMap<InducedFactKind, String> = HashMap::new();
    let mut measured_range_labels: HashSet<String> = HashSet::new();
    for rel in &ontology.relations {
        let slug = relation_slug(&rel.label)
            .ok_or_else(|| anyhow::anyhow!("relation label {:?} mints no IRI", rel.label))?;
        let token = rel_type_token(&rel.label)
            .ok_or_else(|| anyhow::anyhow!("relation label {:?} mints no type token", rel.label))?;
        if let Some(kind) = rel.fact_kind {
            by_fact_kind.entry(kind).or_default().push(token.clone());
            if let Some(range_label) = class_slug(&rel.range) {
                fact_kind_ranges.entry(kind).or_insert(range_label.clone());
            }
            if kind == InducedFactKind::Measurement
                && let Some(range_label) = class_slug(&rel.range)
            {
                measured_range_labels.insert(range_label);
            }
        }
        relations.push(RelationDecl {
            iri: Iri::new(format!("{ns}{slug}")).map_err(|e| {
                anyhow::anyhow!("relation label {:?} mints an invalid IRI: {e}", rel.label)
            })?,
            pref_label: Some(rel.label.clone()),
            parents: Vec::new(),
            domains: Vec::new(),
            ranges: Vec::new(),
            extraction_labels: vec![token],
        });
    }

    let version_iri = Iri::new(ontology.version_iri()).map_err(|e| {
        anyhow::anyhow!(
            "induced ontology '{}' has an invalid version IRI: {e}",
            ontology.domain
        )
    })?;

    // A quantity class is the range of a measurement relation, OR anything
    // declared below one — so an ontology that refines its quantity classes
    // inherits the typed-value contract without re-annotating each child.
    let mut quantitative_labels: Vec<String> = Vec::new();
    for class in &classes {
        let mut current = Some(class.iri.as_str().to_string());
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(iri) = current {
            if !seen.insert(iri.clone()) {
                break;
            }
            let is_quantity = class_slug_of_iri(&iri, &classes)
                .is_some_and(|slug| measured_range_labels.contains(&slug));
            if is_quantity {
                quantitative_labels.extend(class.extraction_labels.iter().cloned());
                break;
            }
            current = parents.get(&iri).and_then(|ps| ps.first().cloned());
        }
    }
    quantitative_labels.sort();
    quantitative_labels.dedup();

    Ok(Arc::new(InducedVocabulary {
        id: Box::leak(ontology.domain.clone().into_boxed_str()),
        version_iri,
        artifact_sha256,
        classes,
        relations,
        parents,
        sign_domains,
        class_iri_by_name,
        fact_kind_relations: by_fact_kind,
        fact_kind_ranges,
        quantitative_labels,
    }))
}

/// The extraction label (mint slug) of the class an IRI names, if it is one
/// of `classes`. Used to test a class against the measurement-relation
/// ranges, which are recorded as slugs.
fn class_slug_of_iri(iri: &str, classes: &[ClassDecl]) -> Option<String> {
    classes
        .iter()
        .find(|class| class.iri.as_str() == iri)
        .and_then(|class| class.extraction_labels.first().cloned())
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
    crate::ontologies::register_ontology(accepted_adapter_with_sha(ontology, artifact_sha256)?)
}

/// Build the registry adapter only after applying the same ACCEPTED-status
/// and structural-validation gates used by process-wide registration.
///
/// Keeping this separate lets a fresh [`crate::ontologies::OntologyRegistry`]
/// load an installed artifact in tests (and in embedders) without touching
/// process-global state. There is still one adapter and one vocabulary path.
fn accepted_adapter_with_sha(
    ontology: &InducedOntology,
    artifact_sha256: String,
) -> Result<Arc<dyn Ontology>> {
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
    adapter(ontology, artifact_sha256)
}

/// Load one accepted artifact as the EXISTING [`Ontology`] adapter without
/// choosing a registry. Callers can then register it either process-wide or
/// in an explicitly owned [`crate::ontologies::OntologyRegistry`].
pub fn load_induced_from_path(path: &std::path::Path) -> Result<Arc<dyn Ontology>> {
    let ontology = super::load_validated(path)?;
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot re-read ontology artifact {}", path.display()))?;
    accepted_adapter_with_sha(&ontology, hex::encode(Sha256::digest(&bytes)))
}

/// Load an artifact file and register it — the production dispatch for
/// "put this induced vocabulary on the extraction path". Parsing and
/// validation run inside [`super::load_validated`]; the draft gate runs in
/// [`load_induced_from_path`]. The registered `artifact_sha256` is the
/// hash of the FILE BYTES as they exist on disk — the artifact as shipped,
/// not a re-serialisation of it.
pub fn register_induced_from_path(path: &std::path::Path) -> Result<()> {
    crate::ontologies::register_ontology(load_induced_from_path(path)?)
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
                    sign_domain: None,
                },
                InducedClass {
                    label: "Polymer".into(),
                    definition: "A macromolecular material.".into(),
                    parent: Some("Material".into()),
                    aligned_iri: None,
                    declared_by_reference: false,
                    sign_domain: None,
                },
                InducedClass {
                    label: "Glass Transition Temperature".into(),
                    definition: String::new(),
                    parent: None,
                    aligned_iri: None,
                    declared_by_reference: false,
                    sign_domain: None,
                },
            ],
            relations: vec![InducedRelation {
                label: "has property".into(),
                definition: String::new(),
                domain: "Polymer".into(),
                range: "Glass Transition Temperature".into(),
                aligned_iri: None,
                fact_kind: None,
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

    /// CONTRACT CHANGE: promotion used to be useful only when the same
    /// process immediately called `register_induced_from_path`. A promoted
    /// project artifact is now sufficient state: two independent registries
    /// (standing in for separate CLI processes) resolve the configured id
    /// through the same accepted-artifact adapter and retain its exact hash.
    #[test]
    fn promoted_project_artifact_reloads_in_fresh_registries() {
        // CONTRACT CHANGE: registration no longer depends on the promotion
        // process still being alive; a later registry reloads the accepted
        // artifact through the production adapter.
        let project = tempfile::tempdir().unwrap();
        let path =
            crate::ontologies::project_ontology_artifact_path(project.path(), "indtest-persisted")
                .unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_artifact(&path, &ontology("indtest-persisted")).unwrap();
        promote_artifact(&path).unwrap();

        let expected_sha = {
            let mut first_process = crate::ontologies::OntologyRegistry::builtin();
            let first = first_process
                .load_project(project.path(), "indtest-persisted")
                .expect("first process loads the promoted project artifact");
            first.artifact_sha256().to_string()
        };

        let mut later_process = crate::ontologies::OntologyRegistry::builtin();
        let reloaded = later_process
            .load_project(project.path(), "indtest-persisted")
            .expect("later process reloads from project state only");
        assert_eq!(reloaded.id(), "indtest-persisted");
        assert_eq!(reloaded.artifact_sha256(), expected_sha);
        assert!(
            reloaded.class_for_label("Polymer").is_some(),
            "the reloaded adapter exposes the promoted vocabulary"
        );
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

    /// A promoted ontology must reach the store's TYPED fact shapes, not just
    /// registration. Proven end-to-end through the artifact bytes: the TTL
    /// carries `prism:factKind` on a relation, and the adapter promotion
    /// installs serves it via `measurement_relations`, with
    /// `quantitative_labels` following from that relation's RANGE class.
    ///
    /// REGRESSION: removing the hardcoded `"HAS_PROPERTY"` literal from
    /// `local_facts` without giving induced ontologies a way to declare a
    /// replacement made every promoted ontology store `value: None` — every
    /// number in the document silently dropped while the run reported
    /// success. Registration must buy parity, not just an entry.
    #[test]
    fn a_promoted_artifact_serves_its_declared_fact_kinds() {
        let mut o = ontology("indtest-factkind");
        o.relations[0].fact_kind = Some(InducedFactKind::Measurement);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factkind.ttl");
        write_artifact(&path, &o).unwrap();
        promote_artifact(&path).unwrap();
        let loaded =
            load_induced_from_path(&path).expect("a promoted artifact with fact kinds loads");

        assert_eq!(
            loaded.measurement_relations(),
            vec!["HAS_PROPERTY"],
            "the declared measurement relation must reach the typed shape"
        );
        // The range of a measurement relation IS a quantity class, so the
        // typed-value contract follows the declaration with no second
        // annotation.
        assert!(
            loaded
                .quantitative_labels()
                .contains(&"GlassTransitionTemperature"),
            "the measurement relation's range must be a quantity class, got {:?}",
            loaded.quantitative_labels()
        );
        // Shapes the ontology never declared stay empty — silence, not a guess.
        assert!(loaded.phase_relations().is_empty());
        assert!(loaded.processing_relations().is_empty());
        assert!(loaded.contains_relations().is_empty());

        // CONTRACT CHANGE: the store no longer hardcodes the kind→(class,
        // edge) table — the promoted artifact's own declaration shapes the
        // typed write. The measurement shape reifies through the ontology's
        // OWN class vocabulary, so a non-materials ontology's typed facts
        // reach a correctly-typed graph with zero Rust edits.
        let shape = loaded
            .fact_graph_shape("measurement")
            .expect("the declared measurement kind carries a graph shape");
        assert!(shape.reified_measurement);
        assert_eq!(
            shape.object_storage_label, "GlassTransitionTemperature",
            "the object falls back to the relation's declared RANGE class, not an EMMO label"
        );
        assert_eq!(shape.edge_rel_type, "HAS_PROPERTY");
        // A kind the artifact never declared is nobody's shape.
        assert!(loaded.fact_graph_shape("phase").is_none());
        assert!(loaded.fact_graph_shape("obligation").is_none());
        // And the numeric prior reads the declaration, not a Rust default.
        assert!(loaded.numeric_prior_fact_kinds().is_empty());
    }

    /// A promoted artifact that declares a CONTAINS-kind relation serves the
    /// contains graph shape from its own classes/tokens AND declares the
    /// contains kind eligible for the numeric prior — the declaration is the
    /// single source for both, exactly as EMMO's adapter is for its kinds.
    #[test]
    fn a_promoted_artifact_serves_contains_shapes_and_prior_kinds() {
        let mut o = ontology("indtest-contains-kind");
        o.relations[0].fact_kind = Some(InducedFactKind::Contains);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contains-kind.ttl");
        write_artifact(&path, &o).unwrap();
        promote_artifact(&path).unwrap();
        let loaded = load_induced_from_path(&path).unwrap();

        let shape = loaded
            .fact_graph_shape("contains")
            .expect("the declared contains kind carries a graph shape");
        assert!(!shape.reified_measurement);
        assert_eq!(shape.object_storage_label, "GlassTransitionTemperature");
        assert_eq!(shape.edge_rel_type, "HAS_PROPERTY");
        assert_eq!(shape.edge_value_prop.as_deref(), Some("fraction"));
        assert_eq!(
            loaded.numeric_prior_fact_kinds(),
            vec!["contains".to_string()]
        );
        // Measurement was never declared by this artifact.
        assert!(loaded.fact_graph_shape("measurement").is_none());
    }

    /// The honest half: an ontology that declares NO fact kinds types
    /// nothing. It must not inherit a frozen English token by accident — the
    /// lexical coincidence that used to make `"has property"` work.
    #[test]
    fn an_undeclared_relation_types_nothing() {
        let o = ontology("indtest-nofactkind");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nofactkind.ttl");
        write_artifact(&path, &o).unwrap();
        promote_artifact(&path).unwrap();
        let loaded = load_induced_from_path(&path).expect("a promoted artifact loads");

        assert!(
            loaded.measurement_relations().is_empty(),
            "a relation labelled 'has property' must NOT be typed by its spelling"
        );
        assert!(loaded.quantitative_labels().is_empty());
    }

    /// A tampered or foreign `prism:factKind` is a loud refusal, never a
    /// silent downgrade to an untyped edge.
    #[test]
    fn an_unknown_fact_kind_literal_is_refused() {
        let mut o = ontology("indtest-badfactkind");
        o.relations[0].fact_kind = Some(InducedFactKind::Measurement);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ttl");
        write_artifact(&path, &o).unwrap();
        let tampered = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"measurement\"", "\"teleportation\"");
        std::fs::write(&path, tampered).unwrap();

        // `Arc<dyn Ontology>` is not Debug, so match rather than unwrap_err.
        let Err(err) = load_induced_from_path(&path) else {
            panic!("an unknown prism:factKind literal must be refused, not loaded");
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("factKind"), "{msg}");
        assert!(msg.contains("teleportation"), "{msg}");
    }

    /// The sign-domain channel the trait promises, proven end-to-end through
    /// the artifact bytes: a promoted TTL carries optional `prism:signDomain`
    /// annotations and the SAME adapter promotion installs serves them — by
    /// prefLabel, by minted extraction label and by class IRI, including a
    /// declaration inherited from a dimensional parent. Zero Rust edits.
    #[test]
    fn a_promoted_artifact_serves_its_declared_sign_domains() {
        let mut o = ontology("indtest-signdomain");
        // Material (dimensional parent) declares non-negative; Polymer
        // declares nothing and must INHERIT it; Glass Transition
        // Temperature declares signed directly.
        o.classes[0].sign_domain = Some(QuantitySignDomain::NonNegative);
        o.classes[2].sign_domain = Some(QuantitySignDomain::Signed);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signdomain.ttl");
        write_artifact(&path, &o).unwrap();
        promote_artifact(&path).unwrap();
        let loaded =
            load_induced_from_path(&path).expect("a promoted artifact with sign annotations loads");

        // Direct declaration, answered by every identity a reader may bind.
        let gtt_iri =
            "https://prism.marc27.com/ontology/indtest-signdomain#GlassTransitionTemperature";
        for identity in [
            "Glass Transition Temperature",
            "GlassTransitionTemperature",
            gtt_iri,
        ] {
            assert_eq!(
                loaded.quantity_sign_domain(identity),
                Some(QuantitySignDomain::Signed),
                "identity {identity:?}"
            );
        }
        // Inheritance from the dimensional parent.
        assert_eq!(
            loaded.quantity_sign_domain("Polymer"),
            Some(QuantitySignDomain::NonNegative),
            "an unannotated quantity inherits its dimensional parent's declaration"
        );
        // The annotated parent answers for itself too.
        assert_eq!(
            loaded.quantity_sign_domain("Material"),
            Some(QuantitySignDomain::NonNegative)
        );
        // Silence where nothing on the path declares.
        assert_eq!(loaded.quantity_sign_domain("UnbekanntenGroesse"), None);
    }

    /// DEFECT FIX (Part 2): the identity lookup used to be EXACT-MATCH
    /// while every other induction lookup (duplicate detection, validate)
    /// folds labels with `normalize_label`. "glass transition temperature"
    /// (lowercase) or "glass_transition_temperature" (underscored) missed
    /// the declared sign domain entirely — a silent false negative on a
    /// guard. Both now resolve like their canonical spellings.
    #[test]
    fn sign_domain_lookup_folds_label_spellings_like_the_rest_of_induction() {
        let mut o = ontology("indtest-signdomain-fold");
        o.classes[2].sign_domain = Some(QuantitySignDomain::Signed);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signdomain-fold.ttl");
        write_artifact(&path, &o).unwrap();
        promote_artifact(&path).unwrap();
        let loaded =
            load_induced_from_path(&path).expect("a promoted artifact with sign annotations loads");

        for identity in [
            "glass transition temperature",
            "Glass_Transition_Temperature",
            "glass-transition-temperature",
        ] {
            assert_eq!(
                loaded.quantity_sign_domain(identity),
                Some(QuantitySignDomain::Signed),
                "identity {identity:?} must fold to the declared class like \
                 duplicate detection folds it"
            );
        }
        // A fully-concatenated lowercase word ("glasstransitiontemperature")
        // does NOT fold — normalize_label splits only at case/separator
        // boundaries — and that is consistent with duplicate detection's
        // fold; not a regression of this fix.
        // A truly unknown quantity is still silence, never a guess.
        assert_eq!(loaded.quantity_sign_domain("Unbekannte Groesse"), None);
    }
}
