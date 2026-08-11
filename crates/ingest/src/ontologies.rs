//! The ontology-vocabulary surface.
//!
//! An ontology is a plugin implementing [`Ontology`]: a stable id, versioned
//! class and object-property declarations with canonical IRIs, and the domain
//! checks that validate a fact of its shape.
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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard};

use anyhow::{Result, bail};
pub use prism_ontology::{ClassDecl, Iri, PropDecl as RelationDecl};
use prism_ontology::{OntologyGraph, load_bundled_emmo, load_bundled_matkg};

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

/// One confidence instruction shared by every ontology prompt. Confidence is
/// explicitly optional: forcing a number when the extractor cannot assess an
/// edge would manufacture certainty. The parser retains only finite values in
/// `[0, 1]`; absence reaches the fact mapper's documented fallback without
/// being represented as a model-supplied score.
const RELATIONSHIP_CONFIDENCE_RULE: &str = "For each relationship, optionally set \"confidence\" to your estimated probability that \
     the relationship is correct, as a finite number from 0 to 1. Omit it when you cannot \
     assess the relationship; never invent a score just to fill the field.";

/// The typed-measurement rule for the prompt, derived from the SAME
/// [`Ontology::quantitative_labels`] declaration the extraction schema's
/// per-type variant enforces structurally. The schema locks the SHAPE;
/// this line states the INTENT the shape exists for — a schema cannot say
/// which field a thing belongs in, which is exactly how a model came to
/// satisfy it by naming a Property `"1100 MPa"` (live 2026-08-08: value and
/// unit stored as text inside the entity NAME, `prov_assertion.value` null,
/// nothing queryable as a number). And it states WHERE the number belongs:
/// on the RELATIONSHIP, per material — a value on a shared property node
/// attributes to nobody (live 2026-08-10: one node's 880 was stored as five
/// alloys' yield strength). One builder shared by the trait default AND
/// [`EmmoOntology`]'s legacy override, so the two prompts cannot drift on
/// the invariant.
fn typed_value_rule(labels: &[&str]) -> String {
    let types = labels
        .iter()
        .map(|label| format!("\"{label}\""))
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "For {types} entities: \"name\" is the property NAME (e.g. \"yield strength\"), \
         NEVER the measured value — an entity named like \"1100 MPa\" is rejected, not \
         stored. Each material's measured number goes on that material's OWN relationship \
         to the property: set the relationship's \"value\" to the number and \"unit\" to \
         one of the listed units (one value per material — never one shared number for \
         several materials; if no listed unit fits, leave \"value\" and \"unit\" out of \
         the relationship entirely). Units: {}. Use the unit that measures the SAME \
         quantity as the property — a density belongs in QUDT:GM-PER-CentiM3 or \
         QUDT:KiloGM-PER-M3, never in a pressure unit.",
        unit_roster()
    )
}

/// Human-readable roster of the declared extraction units, grouped by the
/// quantity kind each measures — derived from the ONE
/// [`crate::qudt_units::EXTRACTION_UNITS`] table, never a second list. The
/// prompt must carry this because the grammar shows the model NOTHING: the
/// enum lock constrains the unit to the vocabulary but cannot say which
/// member measures what, and the live enum-locked model completed its
/// `g/cm3` intent as `QUDT:GigaPA` — a pressure unit on every density
/// (2026-08-10 run 4; the quantity-kind gate dropped nothing because it
/// did not yet cover the edge channel, and the falsehoods reached the
/// store).
fn unit_roster() -> String {
    let mut groups: Vec<(crate::qudt_units::QuantityKind, Vec<&'static str>)> = Vec::new();
    for (id, kind) in crate::qudt_units::EXTRACTION_UNITS {
        match groups.iter_mut().find(|(existing, _)| existing == kind) {
            Some((_, ids)) => ids.push(id),
            None => groups.push((*kind, vec![id])),
        }
    }
    groups
        .iter()
        .map(|(kind, ids)| format!("{} for {}", ids.join("/"), kind.label()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// One ontology vocabulary. The contract the ingest pipeline depends on:
/// the extraction prompt is built from [`Ontology::extraction_preamble`] and
/// [`Ontology::extraction_instructions`], and graph validation accepts
/// exactly [`Ontology::classes`] / [`Ontology::relations`]
/// plus whatever [`Ontology::validate_domain`] enforces — so instructing and
/// validating read the SAME declaration.
pub trait Ontology: Send + Sync {
    /// Stable machine id, e.g. `"emmo"`. Lowercase ASCII alphanumerics plus
    /// `-`/`_` (validated at registration): the id is composed into the
    /// storage tenant (see [`storage_tenant`]), so separator characters are
    /// refused.
    fn id(&self) -> &'static str;

    /// Version IRI under which this ontology classifies extracted facts.
    ///
    /// Satisfies `REQ-OWL-S1-CLASSIFICATION-PROVENANCE`.
    fn version_iri(&self) -> &Iri;

    /// SHA-256 of the exact materialised ontology artifact backing this
    /// declaration.
    ///
    /// Satisfies `REQ-OWL-S1-SUPPLY-CHAIN`.
    fn artifact_sha256(&self) -> &str;

    /// Class declarations extraction may emit and validation accepts. This
    /// slice contains only explicitly mapped extraction declarations, not
    /// ancestor-only classes retained by the ontology graph for closure.
    ///
    /// Satisfies `REQ-OWL-S1-CANONICAL-CLASS-IDENTITY`.
    fn classes(&self) -> &[ClassDecl];

    /// Object-property declarations extraction may emit and validation
    /// accepts.
    ///
    /// Satisfies `REQ-OWL-S1-CANONICAL-RELATION-IDENTITY`.
    fn relations(&self) -> &[RelationDecl];

    /// Resolve an exact extraction label to its canonical class declaration.
    fn class_for_label(&self, label: &str) -> Option<&ClassDecl> {
        self.classes().iter().find(|decl| {
            decl.extraction_labels
                .iter()
                .any(|candidate| candidate == label)
        })
    }

    /// Resolve an exact extraction label to its canonical object-property
    /// declaration.
    fn relation_for_label(&self, label: &str) -> Option<&RelationDecl> {
        self.relations().iter().find(|decl| {
            decl.extraction_labels
                .iter()
                .any(|candidate| candidate == label)
        })
    }

    /// Whether `sub` is equal to or transitively below `sup` in the loaded
    /// `rdfs:subClassOf` closure. Implementations must be cycle-safe.
    ///
    /// Satisfies `REQ-OWL-S1-SUBSUMPTION`.
    fn is_a(&self, sub: &Iri, sup: &Iri) -> bool;

    /// The node label entities of declared extraction type `entity_type`
    /// are PERSISTED under in the graph store. The label is part of the
    /// store's entity key (`{tenant}|{label}:{name}`), so this mapping is
    /// identity, not decoration — and it is the third leg of the
    /// one-declaration contract: the prompt instructs extraction labels, the
    /// validator resolves those labels to declared IRIs, and the store persists
    /// `storage_label(entity_type)`. Before this method existed the store
    /// hardcoded its own third vocabulary (every fact subject became
    /// `Matter`), so a query for the declared `Material` type matched
    /// nothing that was ever stored.
    ///
    /// `None` means the type is not declared — the pipeline drops such an
    /// entity and REPORTS the drop; it never invents or passes through a
    /// label the declaration does not produce.
    ///
    /// The default stores every declared type under itself. An override may
    /// map several extraction synonyms onto one storage label (EMMO: `Alloy`
    /// and `Material` are both matter, so both store as `Matter` and stay
    /// one identity across runs whichever synonym the model picks) but MUST
    /// stay total over every extraction label in [`Ontology::classes`]:
    /// registration refuses an
    /// ontology that instructs a type it cannot store.
    fn storage_label(&self, entity_type: &str) -> Option<&str> {
        self.class_for_label(entity_type)?
            .extraction_labels
            .iter()
            .find(|label| label.as_str() == entity_type)
            .map(String::as_str)
    }

    /// Extraction labels whose entities state a MEASURED QUANTITY — the
    /// vocabulary's property/measurement classes. THREE consumers read this
    /// one declaration, so they cannot drift: the extraction JSON schema
    /// emits a per-type variant REQUIRING typed `value`/`unit` members on
    /// these types ([`crate::extraction_schema`]), the extraction prompt
    /// states the same rule in prose ([`typed_value_rule`]), and graph
    /// validation rejects such an entity whose NAME is itself a measurement
    /// ([`crate::graph_validation::measurement_packed_in_name`]) — the
    /// form-versus-field failure where the model satisfies the schema by
    /// naming a Property `"1100 MPa"` and the graph holds the number as
    /// unqueryable text.
    ///
    /// Default: none — an ontology without measurement classes keeps the
    /// single unconstrained entity shape and an unchanged prompt.
    fn quantitative_labels(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Extraction labels of the relationships that CARRY a measurement —
    /// the edges [`crate::local_facts`] maps to `measurement` facts. The
    /// extraction schema builds these as dedicated variants: a measured
    /// edge (typed `value` + enum-locked `unit` REQUIRED, and NO
    /// `weight`/`order` members at all) or a bare property link (endpoints
    /// plus the universal optional bounded confidence field). The exclusions
    /// are measured necessity: given any optional DOMAIN-VALUE slot on the
    /// edge, the live 12B model put every per-row
    /// number there and stated no unit (2026-08-10 run 3: all ten values
    /// landed in `weight` on the plain variant, silently unmappable) — a
    /// measured quantity must have exactly one domain-value channel, and
    /// that channel demands its unit.
    ///
    /// Default: none — an ontology without measurement relations keeps the
    /// single historical edge shape.
    fn measurement_relations(&self) -> Vec<&str> {
        Vec::new()
    }

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
        // The typed-value rule appears exactly when the declaration names
        // quantitative classes — an ontology without them keeps this prompt
        // byte-identical to what it always produced.
        let quantitative = self.quantitative_labels();
        let typed_rule = if quantitative.is_empty() {
            String::new()
        } else {
            format!("{}\n", typed_value_rule(&quantitative))
        };
        format!(
            "## Instructions\n\
             Identify ALL entities and relationships present in the data.\n\
             Every entity \"type\" MUST be one of: {}.\n\
             Every relationship \"rel\" MUST be one of: {}.\n\
             {REFERENTIAL_INTEGRITY_RULE}\n\
             {RELATIONSHIP_CONFIDENCE_RULE}\n\
             {typed_rule}\
             Return ONLY valid JSON with this structure:\n\
             {{\n\
             \"entities\": [{{\"type\": \"...\", \"name\": \"...\", \"properties\": {{...}}}}],\n\
             \"relationships\": [{{\"from\": \"...\", \"rel\": \"...\", \"to\": \"...\", \"weight\": null, \"order\": null, \"confidence\": null}}]\n\
             }}\n",
            self.classes()
                .iter()
                .flat_map(|decl| decl.extraction_labels.iter())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            self.relations()
                .iter()
                .flat_map(|decl| decl.extraction_labels.iter())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
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

/// The bundled graph is parsed and integrity-checked once, at first use. A
/// malformed or hash-mismatched built-in artifact is a process-start failure:
/// continuing under an unverified ontology would make every subsequent
/// classification unauditable.
static EMMO_GRAPH: LazyLock<OntologyGraph> = LazyLock::new(|| {
    load_bundled_emmo().unwrap_or_else(|error| {
        panic!("bundled EMMO 1.0.3 ontology failed integrity validation: {error}")
    })
});

/// Extraction-facing declarations only. The graph also retains ancestor-only
/// classes so `is_a` can answer over the full materialised closure, but those
/// ancestors must not silently expand the LLM vocabulary.
static EMMO_CLASSES: LazyLock<Vec<ClassDecl>> = LazyLock::new(|| {
    EMMO_GRAPH
        .classes()
        .iter()
        .filter(|decl| !decl.extraction_labels.is_empty())
        .cloned()
        .collect()
});

/// Extraction-facing object properties only.
static EMMO_RELATIONS: LazyLock<Vec<RelationDecl>> = LazyLock::new(|| {
    EMMO_GRAPH
        .properties()
        .iter()
        .filter(|decl| !decl.extraction_labels.is_empty())
        .cloned()
        .collect()
});

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
/// `HAS_PHASE`, which the legacy validator list omitted, is now backed by a
/// declared object-property IRI. The prompt bytes do not need to change, but
/// the relationship is no longer reported as foreign.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmmoOntology;

impl Ontology for EmmoOntology {
    fn id(&self) -> &'static str {
        DEFAULT_ONTOLOGY_ID
    }

    fn version_iri(&self) -> &Iri {
        EMMO_GRAPH.version_iri()
    }

    fn artifact_sha256(&self) -> &str {
        EMMO_GRAPH.sha256()
    }

    fn classes(&self) -> &[ClassDecl] {
        EMMO_CLASSES.as_slice()
    }

    fn relations(&self) -> &[RelationDecl] {
        EMMO_RELATIONS.as_slice()
    }

    fn class_for_label(&self, label: &str) -> Option<&ClassDecl> {
        EMMO_GRAPH.class_for_label(label)
    }

    fn relation_for_label(&self, label: &str) -> Option<&RelationDecl> {
        EMMO_GRAPH.property_for_label(label)
    }

    fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
        EMMO_GRAPH.is_a(sub, sup)
    }

    /// EMMO's storage mapping — the remap the store used to hardcode,
    /// declared. `Alloy` and `Material` now resolve to distinct canonical
    /// classes, but both still persist under the compatibility label `Matter`:
    /// the entity key remains byte-identical while `class_iri` carries the
    /// distinction. `Process`
    /// persists as `Manufacturing` for the same reason — that is the label
    /// the store's `processing` fact shape (and the text path) already
    /// gives every process step, so a standalone process entity and one
    /// reached through PROCESSED_BY converge on one node instead of two.
    /// Everything else stores under itself.
    fn storage_label(&self, entity_type: &str) -> Option<&str> {
        match entity_type {
            "Alloy" | "Material" => Some("Matter"),
            "Process" => Some("Manufacturing"),
            other => self
                .class_for_label(other)?
                .extraction_labels
                .iter()
                .find(|label| label.as_str() == other)
                .map(String::as_str),
        }
    }

    /// Derived from the DECLARATION, not a frozen list: every declared
    /// extraction class at-or-below the class the `Property` label resolves
    /// to in the loaded closure. Today that is exactly `["Property"]`; an
    /// ontology update declaring subclasses of it inherits the typed-value
    /// contract (schema variant + prompt rule + packed-name rejection)
    /// automatically.
    fn quantitative_labels(&self) -> Vec<&str> {
        let Some(root) = self.class_for_label("Property") else {
            return Vec::new();
        };
        self.classes()
            .iter()
            .filter(|class| self.is_a(&class.iri, &root.iri))
            .flat_map(|class| class.extraction_labels.iter().map(String::as_str))
            .collect()
    }

    /// Rooted in the declaration like `quantitative_labels`: the extraction
    /// labels of the declared `HAS_PROPERTY` object property — the one
    /// relationship `local_facts` maps to `measurement` facts. Empty if the
    /// declaration ever stops carrying it.
    fn measurement_relations(&self) -> Vec<&str> {
        self.relation_for_label("HAS_PROPERTY")
            .map(|relation| {
                relation
                    .extraction_labels
                    .iter()
                    .map(String::as_str)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The legacy prompt opening, verbatim (byte-identity contract).
    fn extraction_preamble(&self) -> String {
        "You are a materials science data analyst. Given a dataset schema and sample rows, \
         extract all entities and relationships into a structured JSON format."
            .to_string()
    }

    /// The legacy `## Instructions` block, verbatim EXCEPT for three added
    /// rules — deliberate byte-identity breaks documented on
    /// [`EmmoOntology`]: the [`REFERENTIAL_INTEGRITY_RULE`] (the verbatim
    /// text produced extractions the validator refused wholesale) and the
    /// [`typed_value_rule`] (the verbatim text let the model satisfy the
    /// schema by naming a Property `"1100 MPa"`, storing the number as
    /// unqueryable text — live 2026-08-08), and the
    /// [`RELATIONSHIP_CONFIDENCE_RULE`] that asks for an optional bounded
    /// model judgement without forcing fabricated certainty.
    fn extraction_instructions(&self) -> String {
        let typed_rule = typed_value_rule(&self.quantitative_labels());
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
             {REFERENTIAL_INTEGRITY_RULE}\n\
             {RELATIONSHIP_CONFIDENCE_RULE}\n\
             {typed_rule}\n\n\
             Return ONLY valid JSON with this structure:\n\
             {{\n\
               \"entities\": [{{\"type\": \"...\", \"name\": \"...\", \"properties\": {{...}}}}],\n\
               \"relationships\": [{{\"from\": \"...\", \"rel\": \"...\", \"to\": \"...\", \"weight\": null, \"order\": null, \"confidence\": null}}]\n\
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
// Built-in: MatKG
// ─────────────────────────────────────────────────────────────────────────

/// Stable id of the built-in MatKG vocabulary. Composed into the storage
/// tenant, so MatKG facts land under `storage_tenant(base, "matkg")` —
/// `"local@matkg"` for local loads — and can never blend with or corroborate
/// the user's own `"local"` facts (tenant is part of every entity key and
/// assertion id).
pub const MATKG_ONTOLOGY_ID: &str = "matkg";

/// Same first-use integrity gate as EMMO's graph: a hash-mismatched or
/// malformed bundled artifact is a process failure, not a degraded load.
static MATKG_GRAPH: LazyLock<OntologyGraph> = LazyLock::new(|| {
    load_bundled_matkg().unwrap_or_else(|error| {
        panic!("bundled MatKG 1.4 ontology failed integrity validation: {error}")
    })
});

static MATKG_CLASSES: LazyLock<Vec<ClassDecl>> = LazyLock::new(|| {
    MATKG_GRAPH
        .classes()
        .iter()
        .filter(|decl| !decl.extraction_labels.is_empty())
        .cloned()
        .collect()
});

static MATKG_RELATIONS: LazyLock<Vec<RelationDecl>> = LazyLock::new(|| {
    MATKG_GRAPH
        .properties()
        .iter()
        .filter(|decl| !decl.extraction_labels.is_empty())
        .cloned()
        .collect()
});

/// The built-in MatKG 1.4 vocabulary (Venugopal & Olivetti, Scientific Data
/// 11:217, 2024; CC BY 4.0): the seven NER entity categories as classes and
/// the one statistical relationship the SUBRELOBJ distribution actually
/// carries, `COOCCURS_WITH`. Primarily consumed by the bulk loader in
/// [`crate::matkg`]; it has no text extractor, and selecting it as the
/// active ontology for text ingest is refused honestly by that path.
///
/// Storage labels are the trait-default identity mapping: every declared
/// type persists under itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatKgOntology;

impl Ontology for MatKgOntology {
    fn id(&self) -> &'static str {
        MATKG_ONTOLOGY_ID
    }

    fn version_iri(&self) -> &Iri {
        MATKG_GRAPH.version_iri()
    }

    fn artifact_sha256(&self) -> &str {
        MATKG_GRAPH.sha256()
    }

    fn classes(&self) -> &[ClassDecl] {
        MATKG_CLASSES.as_slice()
    }

    fn relations(&self) -> &[RelationDecl] {
        MATKG_RELATIONS.as_slice()
    }

    fn class_for_label(&self, label: &str) -> Option<&ClassDecl> {
        MATKG_GRAPH.class_for_label(label)
    }

    fn relation_for_label(&self, label: &str) -> Option<&RelationDecl> {
        MATKG_GRAPH.property_for_label(label)
    }

    fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
        MATKG_GRAPH.is_a(sub, sup)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Registry
// ─────────────────────────────────────────────────────────────────────────

/// Validate an ontology's declaration and capture its id. Every adapter method
/// runs outside the registry lock, so adapter-supplied code can never execute
/// while the process-wide registry is locked.
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
    if ontology.version_iri().as_str().trim().is_empty() {
        bail!("ontology '{id}' declares an empty version IRI");
    }
    let sha256 = ontology.artifact_sha256();
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!(
            "ontology '{id}' artifact SHA-256 must be exactly 64 lowercase hexadecimal characters"
        );
    }

    let classes = ontology.classes();
    if classes.is_empty() {
        bail!("ontology '{id}' declares no entity types — it could validate nothing");
    }

    let mut class_iris = HashSet::new();
    let mut class_labels = HashSet::new();
    for class in classes {
        if !class_iris.insert(class.iri.as_str()) {
            bail!("ontology '{id}' declares class IRI '{}' twice", class.iri);
        }
        if class.extraction_labels.is_empty() {
            bail!(
                "ontology '{id}' declares class IRI '{}' with no extraction label",
                class.iri
            );
        }
        for label in &class.extraction_labels {
            if label.trim().is_empty() {
                bail!("ontology '{id}' declares an empty entity type");
            }
            if !class_labels.insert(label.as_str()) {
                bail!("ontology '{id}' declares entity type '{label}' twice");
            }
            match ontology.class_for_label(label) {
                Some(resolved) if resolved.iri.as_str() == class.iri.as_str() => {}
                Some(resolved) => bail!(
                    "ontology '{id}' resolves entity type '{label}' to '{}' instead of declared '{}'",
                    resolved.iri,
                    class.iri
                ),
                None => bail!(
                    "ontology '{id}' declares entity type '{label}' but its resolver cannot find it"
                ),
            }
            match ontology.storage_label(label) {
                Some(storage_label) if !storage_label.trim().is_empty() => {}
                _ => bail!(
                    "ontology '{id}' declares entity type '{label}' but maps it to no \
                     storage label — it would instruct the model in a type that \
                     cannot be stored"
                ),
            }
        }
    }

    let mut relation_iris = HashSet::new();
    let mut relation_labels = HashSet::new();
    for relation in ontology.relations() {
        if !relation_iris.insert(relation.iri.as_str()) {
            bail!(
                "ontology '{id}' declares object-property IRI '{}' twice",
                relation.iri
            );
        }
        if relation.extraction_labels.is_empty() {
            bail!(
                "ontology '{id}' declares object-property IRI '{}' with no extraction label",
                relation.iri
            );
        }
        for label in &relation.extraction_labels {
            if label.trim().is_empty() {
                bail!("ontology '{id}' declares an empty relationship type");
            }
            if !relation_labels.insert(label.as_str()) {
                bail!("ontology '{id}' declares relationship type '{label}' twice");
            }
            match ontology.relation_for_label(label) {
                Some(resolved) if resolved.iri.as_str() == relation.iri.as_str() => {}
                Some(resolved) => bail!(
                    "ontology '{id}' resolves relationship type '{label}' to '{}' instead of declared '{}'",
                    resolved.iri,
                    relation.iri
                ),
                None => bail!(
                    "ontology '{id}' declares relationship type '{label}' but its resolver cannot find it"
                ),
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
        reg.register(Arc::new(MatKgOntology))
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
        classes: Vec<ClassDecl>,
        relations: Vec<RelationDecl>,
        version_iri: Iri,
        artifact_sha256: String,
    }

    impl Fake {
        fn new(id: &'static str, entities: &[&str], relations: &[&str]) -> Self {
            Self {
                id,
                classes: entities
                    .iter()
                    .enumerate()
                    .map(|(index, label)| ClassDecl {
                        iri: Iri::new(format!("https://example.test/class/{index}"))
                            .expect("test class IRI is valid"),
                        pref_label: Some((*label).to_string()),
                        parents: Vec::new(),
                        extraction_labels: vec![(*label).to_string()],
                    })
                    .collect(),
                relations: relations
                    .iter()
                    .enumerate()
                    .map(|(index, label)| RelationDecl {
                        iri: Iri::new(format!("https://example.test/property/{index}"))
                            .expect("test property IRI is valid"),
                        pref_label: Some((*label).to_string()),
                        extraction_labels: vec![(*label).to_string()],
                    })
                    .collect(),
                version_iri: Iri::new("https://example.test/ontology/1".to_string())
                    .expect("test version IRI is valid"),
                artifact_sha256: "0".repeat(64),
            }
        }
    }

    impl Ontology for Fake {
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
        fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
            sub == sup
        }
    }

    fn fake(id: &'static str) -> Arc<dyn Ontology> {
        Arc::new(Fake::new(id, &["Molecule"], &["REACTS_WITH"]))
    }

    /// Every extraction declaration served by the built-in registry resolves
    /// back to the exact class/property present in the integrity-checked
    /// artifact. This is a mechanism property, not a frozen label list.
    #[test]
    fn builtin_declarations_resolve_to_artifact_iris() {
        let reg = OntologyRegistry::builtin();
        let emmo = reg.get("emmo").expect("emmo is built in");
        assert_eq!(
            emmo.version_iri().as_str(),
            "https://w3id.org/emmo/1.0.3/emmo"
        );
        assert_eq!(emmo.artifact_sha256().len(), 64);
        assert!(!emmo.classes().is_empty());
        for class in emmo.classes() {
            assert!(
                EMMO_GRAPH.class(&class.iri).is_some(),
                "declared class '{}' is absent from the bundled graph",
                class.iri
            );
            assert!(!class.extraction_labels.is_empty());
            for label in &class.extraction_labels {
                let resolved = emmo
                    .class_for_label(label)
                    .unwrap_or_else(|| panic!("declared class label '{label}' did not resolve"));
                assert_eq!(resolved.iri.as_str(), class.iri.as_str());
            }
        }
        for relation in emmo.relations() {
            assert!(
                EMMO_GRAPH.property(&relation.iri).is_some(),
                "declared property '{}' is absent from the bundled graph",
                relation.iri
            );
            assert!(!relation.extraction_labels.is_empty());
            for label in &relation.extraction_labels {
                let resolved = emmo.relation_for_label(label).unwrap_or_else(|| {
                    panic!("declared relationship label '{label}' did not resolve")
                });
                assert_eq!(resolved.iri.as_str(), relation.iri.as_str());
            }
        }
        assert!(
            emmo.class_for_label("Author").is_none(),
            "Author has no defensible class IRI in the vendored vocabularies"
        );
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
        assert!(
            reg.get("emmo")
                .expect("emmo registered")
                .class_for_label("Alloy")
                .is_some()
        );
        assert_eq!(reg.ids(), ["emmo", "matkg"]);
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
            .replace(Arc::new(Fake::new("emmo", &["Molecule"], &["REACTS_WITH"])))
            .expect("a registered id must be replaceable");
        assert!(
            displaced.class_for_label("Alloy").is_some(),
            "the built-in came back"
        );
        assert!(
            reg.get("emmo")
                .expect("emmo registered")
                .class_for_label("Molecule")
                .is_some(),
            "get() must return the replacement"
        );
        assert_eq!(reg.all().len(), 2, "replaced in place, not appended");
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
                reg.register(Arc::new(Fake::new(id, entities, rels)))
                    .is_err(),
                "declaration id={id:?} entities={entities:?} rels={rels:?} must be refused",
            );
        }
        assert!(
            reg.all().is_empty(),
            "refused registrations must leave nothing behind"
        );
    }

    /// Registration verifies that declaration aliases, resolvers, and the
    /// supply-chain identity exposed by an adapter are one coherent contract.
    #[test]
    fn inconsistent_resolvers_and_artifact_identity_are_refused() {
        let mut reg = OntologyRegistry::new();

        let mut bad_sha = Fake::new("bad-sha", &["Molecule"], &["REACTS_WITH"]);
        bad_sha.artifact_sha256 = "deadbeef".to_string();
        let error = reg
            .register(Arc::new(bad_sha))
            .expect_err("a truncated artifact digest must be refused");
        assert!(format!("{error:#}").contains("64 lowercase hexadecimal"));

        let mut duplicate_iri = Fake::new("dup-iri", &["Molecule", "Reaction"], &[]);
        duplicate_iri.classes[1].iri = duplicate_iri.classes[0].iri.clone();
        let error = reg
            .register(Arc::new(duplicate_iri))
            .expect_err("one class IRI cannot be declared twice");
        assert!(format!("{error:#}").contains("class IRI"));

        struct BrokenResolver(Fake);
        impl Ontology for BrokenResolver {
            fn id(&self) -> &'static str {
                self.0.id()
            }
            fn version_iri(&self) -> &Iri {
                self.0.version_iri()
            }
            fn artifact_sha256(&self) -> &str {
                self.0.artifact_sha256()
            }
            fn classes(&self) -> &[ClassDecl] {
                self.0.classes()
            }
            fn relations(&self) -> &[RelationDecl] {
                self.0.relations()
            }
            fn class_for_label(&self, _label: &str) -> Option<&ClassDecl> {
                None
            }
            fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
                self.0.is_a(sub, sup)
            }
        }

        let error = reg
            .register(Arc::new(BrokenResolver(Fake::new(
                "broken-resolver",
                &["Molecule"],
                &[],
            ))))
            .expect_err("a declaration its own resolver cannot find must be refused");
        assert!(format!("{error:#}").contains("resolver cannot find"));
        assert!(reg.all().is_empty(), "every invalid adapter was refused");
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
        let onto = Fake::new(
            "chem",
            &["Molecule", "Reaction"],
            &["REACTS_WITH", "CATALYZED_BY"],
        );
        let instructions = onto.extraction_instructions();
        for label in onto
            .classes()
            .iter()
            .flat_map(|decl| decl.extraction_labels.iter())
            .chain(
                onto.relations()
                    .iter()
                    .flat_map(|decl| decl.extraction_labels.iter()),
            )
        {
            assert!(
                instructions.contains(label),
                "missing {label}: {instructions}"
            );
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
        let default_flavour =
            Fake::new("chem", &["Molecule"], &["REACTS_WITH"]).extraction_instructions();
        let emmo = EmmoOntology.extraction_instructions();
        for (who, text) in [("trait default", default_flavour), ("emmo", emmo)] {
            assert!(
                text.contains("MUST also appear as an entity in"),
                "{who} instructions no longer state the referential-integrity rule:\n{text}"
            );
        }
    }

    /// EMMO's storage mapping, pinned value by value: the two extraction
    /// synonyms `Alloy`/`Material` converge on `Matter` (one identity per
    /// material name whichever synonym the model picked), `Process`
    /// converges on the store's `Manufacturing` label (one identity for a
    /// step whether it arrived standalone or through PROCESSED_BY), every
    /// other declared type stores under itself, and an undeclared type maps
    /// to NOTHING — never a guess. `Author` is now deliberately undeclared:
    /// no defensible class IRI for it exists in the vendored vocabularies.
    /// Note `Matter` itself is a storage label, not an instructable extraction
    /// type.
    #[test]
    fn emmo_storage_mapping_converges_synonyms_and_refuses_undeclared() {
        let emmo = EmmoOntology;
        assert_eq!(emmo.storage_label("Alloy"), Some("Matter"));
        assert_eq!(emmo.storage_label("Material"), Some("Matter"));
        assert_eq!(emmo.storage_label("Process"), Some("Manufacturing"));
        for identity in ["Element", "Property", "Phase", "Paper", "Dataset"] {
            assert_eq!(emmo.storage_label(identity), Some(identity));
        }
        assert_eq!(emmo.storage_label("Author"), None);
        assert_eq!(emmo.storage_label("Matter"), None);
        assert_eq!(emmo.storage_label("Widget"), None);
        assert_eq!(emmo.storage_label(""), None);
    }

    /// The property the whole plane guarantees, driven from the declaration
    /// (not a frozen list): EVERY type an ontology declares — and therefore
    /// every type its derived prompt may instruct — has a storage label, on
    /// the trait default and on EMMO's override alike. An ontology that adds
    /// a type tomorrow inherits the guarantee, it does not redden this test.
    #[test]
    fn every_declared_type_is_storable() {
        let fake = Fake::new(
            "prop",
            &["Molecule", "Reaction", "Solvent"],
            &["REACTS_WITH"],
        );
        for onto in [&fake as &dyn Ontology, &EmmoOntology] {
            for entity_type in onto
                .classes()
                .iter()
                .flat_map(|decl| decl.extraction_labels.iter())
            {
                let label = onto.storage_label(entity_type);
                assert!(
                    matches!(label, Some(l) if !l.trim().is_empty()),
                    "ontology '{}' declares type '{entity_type}' but maps it to {label:?} — \
                     it would instruct the model in a type that cannot be stored",
                    onto.id(),
                );
            }
            // And the default is identity: an undeclared type maps to nothing.
            assert_eq!(fake.storage_label("Unicorn"), None);
        }
    }

    /// The mechanical half of "impossible to instruct a type that cannot be
    /// stored": an ontology whose storage mapping is NOT total over its
    /// declared types is refused at registration, so it can never become
    /// the active ontology whose vocabulary builds the prompt.
    #[test]
    fn registration_refuses_an_ontology_that_instructs_an_unstorable_type() {
        struct Unstorable(Fake);
        impl Ontology for Unstorable {
            fn id(&self) -> &'static str {
                "unstorable"
            }
            fn version_iri(&self) -> &Iri {
                self.0.version_iri()
            }
            fn artifact_sha256(&self) -> &str {
                self.0.artifact_sha256()
            }
            fn classes(&self) -> &[ClassDecl] {
                self.0.classes()
            }
            fn relations(&self) -> &[RelationDecl] {
                self.0.relations()
            }
            fn is_a(&self, sub: &Iri, sup: &Iri) -> bool {
                self.0.is_a(sub, sup)
            }
            // "Reaction" is instructable but unmapped — the drift this refusal exists for.
            fn storage_label(&self, entity_type: &str) -> Option<&str> {
                (entity_type == "Molecule").then_some("Molecule")
            }
        }

        let mut reg = OntologyRegistry::new();
        let err = reg
            .register(Arc::new(Unstorable(Fake::new(
                "unstorable",
                &["Molecule", "Reaction"],
                &["REACTS_WITH"],
            ))))
            .expect_err("an unstorable declared type must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("Reaction"), "{msg}");
        assert!(msg.contains("cannot be stored"), "{msg}");
        assert!(reg.all().is_empty(), "a refused registration must not land");
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

    /// Resolve and classify through the process-wide production dispatch, not
    /// through a graph fixture constructed by the test. This test is intended
    /// to fail if either label resolution or the production `is_a` delegation
    /// is mutated (`REQ-OWL-S1-PRODUCTION-DISPATCH`).
    #[test]
    fn active_builtin_resolves_numeric_iri_and_multiple_inheritance() {
        let ontology = active(None).expect("the built-in ontology resolves");
        let material = ontology
            .class_for_label("Material")
            .expect("Material is an extraction declaration");
        assert_eq!(
            material.iri.as_str(),
            "https://w3id.org/emmo#EMMO_4207e895_8b83_4318_996a_72cfb32acd94"
        );
        assert!(
            material.parents.len() >= 2,
            "the materialised Material declaration lost its multiple inheritance: {:?}",
            material.parents
        );
        for parent in &material.parents {
            assert!(
                ontology.is_a(&material.iri, parent),
                "Material is no longer classified below direct parent '{parent}'"
            );
        }

        let transitive_ancestor = EMMO_GRAPH
            .ancestors(&material.iri)
            .expect("Material has an ancestry entry")
            .iter()
            .find(|ancestor| !material.parents.contains(ancestor))
            .expect("Material has at least one transitive ancestor");
        assert!(
            ontology.is_a(&material.iri, transitive_ancestor),
            "Material is no longer classified below transitive ancestor '{transitive_ancestor}'"
        );
    }

    /// MatKG resolves through the SAME production dispatch as EMMO: the
    /// process-wide registry serves it by id, all seven declared classes and
    /// the single relationship resolve to artifact IRIs, and its facts land
    /// under a composed tenant disjoint from both "local" and every mesh
    /// tenant.
    #[test]
    fn active_matkg_resolves_declared_vocabulary_and_composed_tenant() {
        let ontology = active(Some(MATKG_ONTOLOGY_ID)).expect("matkg is built in");
        assert_eq!(
            ontology.version_iri().as_str(),
            "https://marc27.com/ontology/matkg/1.4"
        );
        let labels: Vec<&str> = ontology
            .classes()
            .iter()
            .flat_map(|decl| decl.extraction_labels.iter())
            .map(String::as_str)
            .collect();
        assert_eq!(
            labels,
            [
                "Application",
                "CharacterisationMethod",
                "Chemical",
                "Descriptor",
                "Property",
                "SymmetryPhaseLabel",
                "SynthesisMethod",
            ],
            "the seven MatKG NER categories, deterministic order"
        );
        for label in &labels {
            let class = ontology
                .class_for_label(label)
                .unwrap_or_else(|| panic!("class label {label} did not resolve"));
            assert!(
                class
                    .iri
                    .as_str()
                    .starts_with("https://marc27.com/ontology/matkg#"),
                "MatKG identities are PRISM-minted, never http://example.com: {}",
                class.iri
            );
            // Storage mapping is identity: what the loader stores is what
            // the declaration names.
            assert_eq!(ontology.storage_label(label), Some(*label));
        }
        let relation = ontology
            .relation_for_label("COOCCURS_WITH")
            .expect("the one MatKG relationship resolves");
        assert_eq!(
            relation.iri.as_str(),
            "https://marc27.com/ontology/matkg#cooccursWith"
        );

        let tenant = storage_tenant("local", MATKG_ONTOLOGY_ID);
        assert_eq!(tenant, "local@matkg");
        assert_ne!(tenant, "local", "MatKG facts must never blend with local");
        assert!(!tenant.starts_with("mesh"), "and never look like a peer");
    }

    /// EMMO's quantitative declaration is DERIVED (classes at-or-below the
    /// class the `Property` label resolves to), and today that is exactly
    /// `["Property"]`. An ontology that declares no Property-like class —
    /// the trait default — declares nothing quantitative, so its schema and
    /// prompt stay byte-identical to what they always were.
    #[test]
    fn quantitative_labels_derive_from_the_declaration() {
        assert_eq!(EmmoOntology.quantitative_labels(), ["Property"]);
        let fake = Fake::new("chem-q", &["Molecule"], &["REACTS_WITH"]);
        assert!(fake.quantitative_labels().is_empty());
    }

    /// The measurement-relation declaration mirrors the quantitative one:
    /// EMMO's is rooted in its declared HAS_PROPERTY object property; an
    /// ontology declaring no such relationship declares no measured edges,
    /// so its relationship schema keeps the single historical shape.
    #[test]
    fn measurement_relations_derive_from_the_declaration() {
        assert_eq!(EmmoOntology.measurement_relations(), ["HAS_PROPERTY"]);
        let fake = Fake::new("chem-m", &["Molecule"], &["REACTS_WITH"]);
        assert!(fake.measurement_relations().is_empty());
    }

    /// Both instruction builders state the typed-value rule exactly when the
    /// declaration names quantitative classes: EMMO (and any default-flavour
    /// ontology declaring them) must say the name is never the measurement;
    /// an ontology without quantitative classes must NOT gain the line (its
    /// prompt has no field the rule could bind to). Fragments are hardcoded
    /// here so a reworded-away rule fails too.
    #[test]
    fn instruction_builders_state_the_typed_value_rule_iff_quantitative() {
        let emmo = EmmoOntology.extraction_instructions();
        assert!(
            emmo.contains("NEVER the measured value") && emmo.contains("relationship's \"value\""),
            "emmo instructions no longer state the typed-value rule:\n{emmo}"
        );
        let none = Fake::new("chem-q2", &["Molecule"], &["REACTS_WITH"]).extraction_instructions();
        assert!(
            !none.contains("NEVER the measured value"),
            "an ontology with no quantitative classes must not gain the rule:\n{none}"
        );
    }

    #[test]
    fn every_instruction_builder_requests_optional_bounded_relationship_confidence() {
        let emmo = EmmoOntology.extraction_instructions();
        let default =
            Fake::new("chem-confidence", &["Molecule"], &["REACTS_WITH"]).extraction_instructions();
        for instructions in [emmo, default] {
            assert!(
                instructions.contains("optionally set \"confidence\"")
                    && instructions.contains("finite number from 0 to 1")
                    && instructions.contains("Omit it when you cannot assess"),
                "relationship confidence instruction drifted:\n{instructions}"
            );
            assert!(
                instructions.contains("\"confidence\": null"),
                "the output example must expose the optional field:\n{instructions}"
            );
        }
    }

    /// `HAS_PHASE` is now one declared, IRI-backed object property. It was
    /// formerly the prompt's only relationship absent from validation.
    #[test]
    fn active_builtin_resolves_has_phase_to_an_artifact_property() {
        let ontology = active(None).expect("the built-in ontology resolves");
        let relation = ontology
            .relation_for_label("HAS_PHASE")
            .expect("HAS_PHASE must be a declared extraction relationship");
        assert!(
            EMMO_GRAPH.property(&relation.iri).is_some(),
            "HAS_PHASE resolved to a property absent from the bundled artifact"
        );
    }
}
