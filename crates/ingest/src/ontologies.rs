//! The ontology-vocabulary surface.
//!
//! An ontology is a plugin implementing [`Ontology`]: a stable id, the entity
//! and relationship types its facts are allowed to carry, the unit vocabulary
//! those facts cite, and the domain checks that validate a fact of its shape.
//! The tabular ingest pipeline consults the ACTIVE ontology for BOTH the
//! extraction prompt and graph validation, so the vocabulary the model is
//! instructed with and the vocabulary the validator accepts come from one
//! source and cannot drift apart per ontology.
//!
//! Adding an ontology means implementing [`Ontology`], then ONE registration
//! call: [`register_ontology`] into the process-wide registry at runtime
//! (built-ins use a `register(...)` line in [`OntologyRegistry::builtin`]).
//! Selection is `[ontology] id = "..."` in `prism.toml`, carried to
//! [`crate::pipeline::PipelineConfig::ontology`].
//!
//! Swapping a built-in for your own implementation is [`replace_ontology`].
//! The two-call contract shared by every adapter plane: `register` refuses a
//! taken id (an accidental collision fails loudly; nothing silently wins),
//! `replace` refuses a free id (a typo cannot silently ADD while the ontology
//! you meant to displace keeps running) and returns what it displaced.
//!
//! What is deliberately NOT pluggable here: the provenance store's typed EMMO
//! shapes and [`prism_provenance::QudtUnit`]. Facts from a non-default
//! ontology coexist with EMMO facts by landing under a composed storage
//! tenant (see [`storage_tenant`]) — the store's tenant-qualified keys are
//! what keep the two subgraphs from blending, exactly as they keep local and
//! mesh-peer knowledge apart. A non-QUDT ontology would additionally need its
//! own typed-unit path (today's `MaterialFact`/text extraction is QUDT by
//! construction).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard};

use anyhow::{Result, bail};

use crate::EntitySet;
use crate::graph_validation::{GraphIssue, GraphSeverity};

/// The built-in default ontology — what every existing store was written
/// with, and what an absent `[ontology] id` selects.
pub const DEFAULT_ONTOLOGY_ID: &str = "emmo";

/// The referential-integrity rule, stated in EVERY extraction prompt: graph
/// validation refuses a relationship whose `from`/`to` names no declared
/// entity (`orphan_rel`), so a prompt that never states the rule instructs
/// the model into unstorable output — the exact declaration-vs-enforcement
/// drift this module exists to close (live case 2026-08-08: 13 entities and
/// 13 relationships extracted, 17 `orphan_rel` errors, nothing stored).
/// One constant shared by the trait default AND [`EmmoOntology`]'s legacy
/// override, so the two prompts cannot drift on the invariant.
const REFERENTIAL_INTEGRITY_RULE: &str =
    "Every name used in \"from\" or \"to\" MUST also appear as an entity in \"entities\".";

/// The unit vocabulary an ontology's facts cite. A declaration, not an
/// enforcement point: on the tabular path units are free-form strings, and
/// the typed enforcement that exists (text extraction → `MaterialFact`) is
/// QUDT-shaped by construction via `prism_provenance::QudtUnit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitVocabulary {
    /// Vocabulary name, e.g. `"QUDT"`.
    pub name: &'static str,
    /// Identifier prefix unit strings carry on the typed path, e.g.
    /// `"QUDT:"`. `None` means free-form unit strings.
    pub prefix: Option<&'static str>,
}

/// One ontology vocabulary. The contract the ingest pipeline depends on:
/// the extraction prompt is built from [`Ontology::extraction_preamble`] and
/// [`Ontology::extraction_instructions`], and graph validation accepts
/// exactly [`Ontology::entity_types`] / [`Ontology::relationship_types`]
/// plus whatever [`Ontology::validate_domain`] enforces — so instructing and
/// validating read the SAME declaration.
pub trait Ontology: Send + Sync {
    /// Stable machine id, e.g. `"emmo"`. Lowercase ASCII alphanumerics plus
    /// `-`/`_` (validated at registration): the id is composed into the
    /// storage tenant (see [`storage_tenant`]), so separator characters are
    /// refused.
    fn id(&self) -> &'static str;

    /// Entity types extraction may emit and validation accepts.
    fn entity_types(&self) -> &'static [&'static str];

    /// Relationship types extraction may emit and validation accepts.
    fn relationship_types(&self) -> &'static [&'static str];

    /// The unit vocabulary this ontology's facts cite.
    fn unit_vocabulary(&self) -> UnitVocabulary;

    /// Opening sentence of the tabular extraction prompt.
    fn extraction_preamble(&self) -> String {
        format!(
            "You are a data analyst extracting facts under the '{}' ontology. \
             Given a dataset schema and sample rows, extract all entities and \
             relationships into a structured JSON format.",
            self.id()
        )
    }

    /// The `## Instructions` block of the tabular extraction prompt. The
    /// default derives it from the SAME declared vocabulary validation reads,
    /// so for an ontology that does not override this, prompt and validator
    /// cannot disagree. An override owns keeping the two aligned.
    fn extraction_instructions(&self) -> String {
        format!(
            "## Instructions\n\
             Identify ALL entities and relationships present in the data.\n\
             Every entity \"type\" MUST be one of: {}.\n\
             Every relationship \"rel\" MUST be one of: {}.\n\
             {REFERENTIAL_INTEGRITY_RULE}\n\
             Return ONLY valid JSON with this structure:\n\
             {{\n\
             \"entities\": [{{\"type\": \"...\", \"name\": \"...\", \"properties\": {{...}}}}],\n\
             \"relationships\": [{{\"from\": \"...\", \"rel\": \"...\", \"to\": \"...\", \"weight\": null, \"order\": null}}]\n\
             }}\n",
            self.entity_types().join(", "),
            self.relationship_types().join(", "),
        )
    }

    /// Ontology-specific fact checks beyond type membership (which
    /// `graph_validation` performs from the declared lists). Default: none.
    fn validate_domain(&self, _entities: &EntitySet) -> Vec<GraphIssue> {
        Vec::new()
    }
}

/// The storage tenant facts of `ontology_id` are written under, given the
/// base ownership tenant (`"local"` for single-user local ingest).
///
/// The default ontology keeps the BARE base tenant — every pre-existing
/// store holds its EMMO facts under `"local"`, and suffixing the default
/// would orphan all of them. Any other ontology gets `{base}@{id}`: the
/// store's keys (`entity_key`, `assertion_id`) and every read are
/// tenant-qualified, so two ontologies land in disjoint subgraphs with no
/// store changes — the same isolation that keeps local and mesh-peer
/// knowledge apart. Composition is unambiguous because ontology ids cannot
/// contain `@` (refused at registration), so the suffix after the last `@`
/// is always the ontology id.
#[must_use]
pub fn storage_tenant(base: &str, ontology_id: &str) -> String {
    if ontology_id == DEFAULT_ONTOLOGY_ID {
        base.to_string()
    } else {
        format!("{base}@{ontology_id}")
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Built-in: EMMO
// ─────────────────────────────────────────────────────────────────────────

/// Entity types of the built-in EMMO materials vocabulary (formerly
/// `graph_validation::VALID_ENTITY_TYPES`).
const EMMO_ENTITY_TYPES: &[&str] = &[
    "Alloy", "Element", "Property", "Process", "Phase", "Paper", "Author", "Dataset", "Material",
];

/// Relationship types of the built-in EMMO materials vocabulary (formerly
/// `graph_validation::VALID_REL_TYPES`).
const EMMO_REL_TYPES: &[&str] = &[
    "CONTAINS",
    "HAS_PROPERTY",
    "PROCESSED_BY",
    "OBSERVED_IN",
    "PUBLISHED_IN",
    "AUTHORED_BY",
    "PART_OF",
    "CITES",
];

/// The built-in EMMO materials-science ontology — the vocabulary this
/// codebase always extracted and validated with, now declared through the
/// same trait any other ontology plugs in through. Behaviour is deliberately
/// byte-identical to the pre-trait hardcoding, with ONE deliberate
/// exception: the instruction block now also states
/// [`REFERENTIAL_INTEGRITY_RULE`]. The legacy text instructed relationships
/// without requiring their endpoints be declared, while the validator
/// refuses exactly that (`orphan_rel`, Error) — so the byte-identical
/// prompt reliably produced unstorable extractions (2026-08-08 live run:
/// 17 orphan errors, zero facts stored). Preserving those bytes preserved
/// the defect; the rule is added, everything else stays verbatim.
///
/// The one known harmless drift IS still preserved: the legacy prompt
/// instructs `HAS_PHASE`, which the legacy validator list never contained
/// (so it warns as `unknown_rel`). Fixing that would change validation
/// reports for existing users, so it is preserved, not repaired, here.
pub struct EmmoOntology;

impl Ontology for EmmoOntology {
    fn id(&self) -> &'static str {
        DEFAULT_ONTOLOGY_ID
    }

    fn entity_types(&self) -> &'static [&'static str] {
        EMMO_ENTITY_TYPES
    }

    fn relationship_types(&self) -> &'static [&'static str] {
        EMMO_REL_TYPES
    }

    fn unit_vocabulary(&self) -> UnitVocabulary {
        UnitVocabulary {
            name: "QUDT",
            prefix: Some("QUDT:"),
        }
    }

    /// The legacy prompt opening, verbatim (byte-identity contract).
    fn extraction_preamble(&self) -> String {
        "You are a materials science data analyst. Given a dataset schema and sample rows, \
         extract all entities and relationships into a structured JSON format."
            .to_string()
    }

    /// The legacy `## Instructions` block, verbatim EXCEPT for the added
    /// [`REFERENTIAL_INTEGRITY_RULE`] line — the deliberate byte-identity
    /// break documented on [`EmmoOntology`]: the verbatim text is what
    /// produced extractions the validator then refused wholesale.
    fn extraction_instructions(&self) -> String {
        format!(
            "## Instructions\n\
             Identify ALL materials science entities:\n\
             - Alloy/Material compositions (type: \"Alloy\" or \"Material\")\n\
             - Elements with fractions (type: \"Element\")\n\
             - Processing steps with parameters (type: \"Process\")\n\
             - Measured properties with values and units (type: \"Property\")\n\
             - Phases or crystal structures (type: \"Phase\")\n\n\
             Identify ALL relationships:\n\
             - CONTAINS (material → element, with weight = fraction)\n\
             - PROCESSED_BY (material → process, with order)\n\
             - HAS_PROPERTY (material → property)\n\
             - HAS_PHASE (material → phase)\n\n\
             {REFERENTIAL_INTEGRITY_RULE}\n\n\
             Return ONLY valid JSON with this structure:\n\
             {{\n\
               \"entities\": [{{\"type\": \"...\", \"name\": \"...\", \"properties\": {{...}}}}],\n\
               \"relationships\": [{{\"from\": \"...\", \"rel\": \"...\", \"to\": \"...\", \"weight\": null, \"order\": null}}]\n\
             }}\n"
        )
    }

    /// EMMO's domain checks, moved verbatim from `graph_validation`
    /// (checks 7–10 there), in the same order they always ran.
    fn validate_domain(&self, entities: &EntitySet) -> Vec<GraphIssue> {
        let mut issues = Vec::new();

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
            if r.rel_type == "CONTAINS"
                && let Some(w) = r.weight
                && !(0.0..=1.0).contains(&w)
            {
                issues.push(GraphIssue {
                    severity: GraphSeverity::Warning,
                    category: "invalid_weight".into(),
                    message: format!("CONTAINS {} → {}: weight {w} not in [0, 1]", r.from, r.to),
                });
            }
        }

        // Check 9: PROCESSED_BY should have order
        for r in &entities.relationships {
            if r.rel_type == "PROCESSED_BY" && r.order.is_none() {
                issues.push(GraphIssue {
                    severity: GraphSeverity::Info,
                    category: "missing_order".into(),
                    message: format!("PROCESSED_BY {} → {} has no order", r.from, r.to),
                });
            }
        }

        // Check 10: CONTAINS weights for an alloy should sum to ~1.0
        let mut alloy_weights: HashMap<&str, f64> = HashMap::new();
        for r in &entities.relationships {
            if r.rel_type == "CONTAINS"
                && let Some(w) = r.weight
            {
                *alloy_weights.entry(r.from.as_str()).or_default() += w;
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

        issues
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Registry
// ─────────────────────────────────────────────────────────────────────────

/// Validate an ontology's declaration and capture its id — trait methods are
/// called ONCE, outside any lock, so adapter-supplied code never runs while
/// the process-wide registry lock is held.
fn validated_id(ontology: &dyn Ontology) -> Result<&'static str> {
    let id = ontology.id();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        bail!(
            "ontology id {id:?} must be non-empty lowercase ASCII \
             alphanumerics plus '-'/'_' (it is composed into storage tenants)"
        );
    }
    let entity_types = ontology.entity_types();
    if entity_types.is_empty() {
        bail!("ontology '{id}' declares no entity types — it could validate nothing");
    }
    for (label, list) in [
        ("entity", entity_types),
        ("relationship", ontology.relationship_types()),
    ] {
        for (i, t) in list.iter().enumerate() {
            if t.trim().is_empty() {
                bail!("ontology '{id}' declares an empty {label} type");
            }
            if list[..i].contains(t) {
                bail!("ontology '{id}' declares {label} type '{t}' twice");
            }
        }
    }
    Ok(id)
}

/// Ordered registry of ontologies — the ONE place an ontology id resolves to
/// its vocabulary. Iteration order is registration order, which keeps
/// derived lists (error messages) deterministic.
pub struct OntologyRegistry {
    ontologies: Vec<Arc<dyn Ontology>>,
    /// Captured ids, parallel to `ontologies`. All bookkeeping reads these —
    /// never the trait method — so no adapter code runs under the
    /// process-wide registry lock.
    ids: Vec<&'static str>,
    by_id: HashMap<&'static str, usize>,
}

impl OntologyRegistry {
    pub fn new() -> Self {
        Self {
            ontologies: Vec::new(),
            ids: Vec::new(),
            by_id: HashMap::new(),
        }
    }

    /// The built-in ontologies, in canonical order.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(EmmoOntology))
            .expect("built-in ontology declarations are valid and unique");
        reg
    }

    /// Add an ontology under a FREE id. The two-call contract shared by
    /// every adapter plane: an accidental id collision fails loudly —
    /// taking over a registered ontology is a deliberate act with its own
    /// call, [`OntologyRegistry::replace`]. On refusal nothing changes.
    pub fn register(&mut self, ontology: Arc<dyn Ontology>) -> Result<()> {
        let id = validated_id(ontology.as_ref())?;
        self.insert_new(id, ontology)
    }

    /// Deliberately swap the ontology registered under the SAME id. The id
    /// must be taken (a typo'd id cannot silently ADD an ontology while the
    /// one you meant to displace keeps running). Returns the displaced
    /// ontology — hand it back to this function to restore the original —
    /// and logs what was displaced.
    pub fn replace(&mut self, ontology: Arc<dyn Ontology>) -> Result<Arc<dyn Ontology>> {
        let id = validated_id(ontology.as_ref())?;
        let displaced = self.swap(id, ontology)?;
        tracing::info!(id, "ontology deliberately replaced");
        Ok(displaced)
    }

    /// Registration body. Uses only the pre-captured `id` — no adapter
    /// trait calls — so it is safe to run under the process-wide write lock.
    fn insert_new(&mut self, id: &'static str, ontology: Arc<dyn Ontology>) -> Result<()> {
        if self.by_id.contains_key(id) {
            bail!(
                "ontology id '{id}' is already registered; swap it deliberately \
                 with OntologyRegistry::replace (replace_ontology for the \
                 process-wide registry)"
            );
        }
        self.by_id.insert(id, self.ontologies.len());
        self.ids.push(id);
        self.ontologies.push(ontology);
        Ok(())
    }

    /// Replacement body. Uses only the pre-captured `id` — no adapter
    /// trait calls — so it is safe to run under the process-wide write lock.
    fn swap(&mut self, id: &'static str, ontology: Arc<dyn Ontology>) -> Result<Arc<dyn Ontology>> {
        let Some(&idx) = self.by_id.get(id) else {
            bail!(
                "no ontology '{id}' registered to replace; add it with \
                 OntologyRegistry::register (register_ontology for the \
                 process-wide registry)"
            );
        };
        Ok(std::mem::replace(&mut self.ontologies[idx], ontology))
    }

    /// Look up an ontology by its stable id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Ontology>> {
        self.by_id.get(id).map(|&idx| self.ontologies[idx].clone())
    }

    /// All registered ids, in registration order.
    pub fn ids(&self) -> Vec<&'static str> {
        self.ids.clone()
    }

    /// All registered ontologies, in registration order.
    pub fn all(&self) -> &[Arc<dyn Ontology>] {
        &self.ontologies
    }
}

impl Default for OntologyRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

/// The process-wide registry: starts as [`OntologyRegistry::builtin`] and is
/// extendable at runtime through [`register_ontology`]. The ingest pipeline
/// resolves the configured ontology id through [`active`], which reads this.
static REGISTRY: LazyLock<RwLock<OntologyRegistry>> =
    LazyLock::new(|| RwLock::new(OntologyRegistry::builtin()));

/// Read access to the process-wide registry. Hold the guard only for the
/// query — never across an `.await` (the guard is not `Send`) and never
/// while calling [`register_ontology`] on the same thread.
pub fn registry() -> RwLockReadGuard<'static, OntologyRegistry> {
    REGISTRY.read().expect("ontology registry lock poisoned")
}

/// Register an ontology in the process-wide registry. Refusal semantics are
/// [`OntologyRegistry::register`]'s: a malformed declaration and a taken id
/// are both `Err` — swapping a registered ontology is [`replace_ontology`].
/// The declaration is captured BEFORE the write lock is taken.
pub fn register_ontology(ontology: Arc<dyn Ontology>) -> Result<()> {
    let id = validated_id(ontology.as_ref())?;
    REGISTRY
        .write()
        .expect("ontology registry lock poisoned")
        .insert_new(id, ontology)
}

/// Deliberately swap an ontology registered in the process-wide registry —
/// how a caller takes over the built-in ("emmo") with their own
/// implementation. Semantics are [`OntologyRegistry::replace`]'s. Returns
/// the displaced ontology — hand it back to this function to restore it.
pub fn replace_ontology(ontology: Arc<dyn Ontology>) -> Result<Arc<dyn Ontology>> {
    let id = validated_id(ontology.as_ref())?;
    let displaced = REGISTRY
        .write()
        .expect("ontology registry lock poisoned")
        .swap(id, ontology)?;
    tracing::info!(
        id,
        "ontology deliberately replaced in the process-wide registry"
    );
    Ok(displaced)
}

/// Resolve the ACTIVE ontology: the configured id, or the built-in default
/// when none is configured. An id nothing registered is a LOUD error naming
/// what is registered — never a silent fallback to EMMO, which would ingest
/// under the wrong vocabulary while looking configured.
pub fn active(id: Option<&str>) -> Result<Arc<dyn Ontology>> {
    let id = id.unwrap_or(DEFAULT_ONTOLOGY_ID);
    let registry = registry();
    registry.get(id).ok_or_else(|| {
        anyhow::anyhow!(
            "no ontology '{id}' is registered (registered: {}). Set [ontology] id \
             to a registered ontology, or register yours with \
             prism_ingest::ontologies::register_ontology",
            registry.ids().join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test double with a configurable declaration.
    struct Fake {
        id: &'static str,
        entities: &'static [&'static str],
        rels: &'static [&'static str],
    }

    impl Ontology for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn entity_types(&self) -> &'static [&'static str] {
            self.entities
        }
        fn relationship_types(&self) -> &'static [&'static str] {
            self.rels
        }
        fn unit_vocabulary(&self) -> UnitVocabulary {
            UnitVocabulary {
                name: "FREE",
                prefix: None,
            }
        }
    }

    fn fake(id: &'static str) -> Arc<dyn Ontology> {
        Arc::new(Fake {
            id,
            entities: &["Molecule"],
            rels: &["REACTS_WITH"],
        })
    }

    #[test]
    fn builtin_has_emmo_with_the_legacy_vocabulary() {
        let reg = OntologyRegistry::builtin();
        let emmo = reg.get("emmo").expect("emmo is built in");
        // The exact legacy const arrays, pinned: validation behaviour for
        // existing users depends on these values and this order (the
        // unknown-type message joins the list).
        assert_eq!(
            emmo.entity_types(),
            [
                "Alloy", "Element", "Property", "Process", "Phase", "Paper", "Author", "Dataset",
                "Material",
            ]
        );
        assert_eq!(
            emmo.relationship_types(),
            [
                "CONTAINS",
                "HAS_PROPERTY",
                "PROCESSED_BY",
                "OBSERVED_IN",
                "PUBLISHED_IN",
                "AUTHORED_BY",
                "PART_OF",
                "CITES",
            ]
        );
        assert_eq!(emmo.unit_vocabulary().name, "QUDT");
        assert_eq!(emmo.unit_vocabulary().prefix, Some("QUDT:"));
    }

    /// The loud half of the two-call contract: `register` refuses a taken
    /// id and a refusal changes nothing.
    #[test]
    fn register_refuses_a_taken_id() {
        let mut reg = OntologyRegistry::builtin();
        let err = reg
            .register(fake("emmo"))
            .expect_err("a taken id must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("already registered"), "{msg}");
        assert!(msg.contains("replace_ontology"), "{msg}");
        // Nothing changed: the built-in still serves "emmo".
        assert_eq!(
            reg.get("emmo").expect("emmo registered").entity_types()[0],
            "Alloy"
        );
        assert_eq!(reg.ids(), ["emmo"]);
    }

    /// The strict half: `replace` refuses a FREE id, and a deliberate
    /// replacement swaps in place and returns the displaced ontology.
    #[test]
    fn replace_refuses_a_free_id_and_returns_the_displaced() {
        let mut reg = OntologyRegistry::builtin();

        let err = match reg.replace(fake("chem")) {
            Err(e) => e,
            Ok(_) => panic!("replacing an unregistered id must be refused"),
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("no ontology 'chem'"), "{msg}");
        assert!(msg.contains("register_ontology"), "{msg}");
        assert!(reg.get("chem").is_none(), "a refused replace must not ADD");

        let displaced = reg
            .replace(Arc::new(Fake {
                id: "emmo",
                entities: &["Molecule"],
                rels: &["REACTS_WITH"],
            }))
            .expect("a registered id must be replaceable");
        assert_eq!(
            displaced.entity_types()[0],
            "Alloy",
            "the built-in came back"
        );
        assert_eq!(
            reg.get("emmo").expect("emmo registered").entity_types(),
            ["Molecule"],
            "get() must return the replacement",
        );
        assert_eq!(reg.all().len(), 1, "replaced in place, not appended");
    }

    #[test]
    fn malformed_declarations_are_refused() {
        let mut reg = OntologyRegistry::new();
        let malformed: &[(
            &'static str,
            &'static [&'static str],
            &'static [&'static str],
        )] = &[
            ("", &["A"], &[]),                // empty id
            ("Upper", &["A"], &[]),           // uppercase id
            ("has space", &["A"], &[]),       // whitespace id
            ("at@sign", &["A"], &[]),         // '@' collides with storage_tenant composition
            ("pipe|char", &["A"], &[]),       // '|' collides with the store's entity_key
            ("no-entities", &[], &[]),        // nothing to validate with
            ("dup-entity", &["A", "A"], &[]), // duplicate entity type
            ("dup-rel", &["A"], &["R", "R"]), // duplicate relationship type
            ("empty-type", &[""], &[]),       // empty entity type
        ];
        for &(id, entities, rels) in malformed {
            assert!(
                reg.register(Arc::new(Fake { id, entities, rels })).is_err(),
                "declaration id={id:?} entities={entities:?} rels={rels:?} must be refused",
            );
        }
        assert!(
            reg.all().is_empty(),
            "refused registrations must leave nothing behind"
        );
    }

    /// Facts of the default ontology keep the bare base tenant (every
    /// pre-existing store holds EMMO facts under "local"); any other
    /// ontology gets a composed tenant, which is what keeps its subgraph
    /// disjoint from EMMO's in one store.
    #[test]
    fn storage_tenant_composes_for_non_default_ontologies_only() {
        assert_eq!(storage_tenant("local", "emmo"), "local");
        assert_eq!(storage_tenant("local", "chem"), "local@chem");
        // Never mistakable for a mesh tenant (`is_relay` matches "mesh"
        // and "mesh:*"): the base is preserved verbatim up front.
        assert!(!storage_tenant("local", "chem").starts_with("mesh"));
    }

    /// The single-source property for ontologies that do NOT override the
    /// prompt: every declared type appears verbatim in the derived
    /// instructions, so the model is instructed with exactly the vocabulary
    /// the validator will accept.
    #[test]
    fn default_instructions_derive_from_the_declared_vocabulary() {
        let onto = Fake {
            id: "chem",
            entities: &["Molecule", "Reaction"],
            rels: &["REACTS_WITH", "CATALYZED_BY"],
        };
        let instructions = onto.extraction_instructions();
        for t in onto.entity_types().iter().chain(onto.relationship_types()) {
            assert!(instructions.contains(t), "missing {t}: {instructions}");
        }
        assert!(instructions.contains("Return ONLY valid JSON"));
        // And the wire shape the parser expects.
        assert!(instructions.contains("\"entities\""));
        assert!(instructions.contains("\"relationships\""));
        let preamble = onto.extraction_preamble();
        assert!(preamble.contains("'chem'"), "{preamble}");
    }

    /// Every prompt states the invariant the validator enforces: a `from`/
    /// `to` name must be a declared entity (`orphan_rel` is Error severity).
    /// Checked on BOTH instruction builders — the trait default any new
    /// ontology inherits, and EMMO's legacy override (whose byte-identity
    /// was deliberately broken for exactly this line: the verbatim text
    /// produced unstorable extractions). The fragment is hardcoded here so
    /// a reworded-away rule fails too.
    #[test]
    fn every_instruction_builder_states_the_referential_integrity_rule() {
        let default_flavour = Fake {
            id: "chem",
            entities: &["Molecule"],
            rels: &["REACTS_WITH"],
        }
        .extraction_instructions();
        let emmo = EmmoOntology.extraction_instructions();
        for (who, text) in [("trait default", default_flavour), ("emmo", emmo)] {
            assert!(
                text.contains("MUST also appear as an entity in"),
                "{who} instructions no longer state the referential-integrity rule:\n{text}"
            );
        }
    }

    /// `active` resolves the default when unconfigured, the named id when
    /// configured, and refuses an unregistered id LOUDLY naming what is
    /// registered — never a silent EMMO fallback.
    #[test]
    fn active_resolves_default_and_refuses_unknown_ids() {
        assert_eq!(active(None).expect("default resolves").id(), "emmo");
        assert_eq!(active(Some("emmo")).expect("emmo resolves").id(), "emmo");
        let err = match active(Some("zzz-not-registered")) {
            Err(e) => e,
            Ok(_) => panic!("unknown id must be refused"),
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("zzz-not-registered"), "{msg}");
        assert!(msg.contains("registered: "), "{msg}");
        assert!(msg.contains("emmo"), "{msg}");
    }
}
