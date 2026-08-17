//! LLM-driven ontology INDUCTION — corpus → ontology artifact (Turtle).
//!
//! Everything else in this crate extracts graph INSTANCES against a fixed
//! vocabulary. This module is the missing other half: it reads a corpus of
//! source material and PRODUCES the vocabulary itself — classes, an is-a
//! hierarchy, and typed relations — as a versioned, provenance-stamped TTL
//! artifact (see [`ttl`]) that the OWL-loading side consumes.
//!
//! The pipeline is built around one hard assumption: the model is SMALL and
//! WILL misbehave. Malformed JSON, duplicate classes, hierarchy cycles and
//! dangling references are the normal case, not edge cases, and they are
//! handled in two distinct layers that must not be confused:
//!
//! 1. **The builder** ([`OntologyBuilder`]) normalises noisy model output
//!    while the draft is being accumulated: duplicates merge, cycle-creating
//!    parent links are dropped, referenced-but-undefined classes are
//!    declared by reference. Nothing is silent — every normalisation is
//!    counted and recorded in the artifact's provenance block.
//! 2. **The validator** ([`validate::validate`]) is a pure check over a
//!    FINISHED ontology (freshly induced or re-parsed from disk). A finished
//!    artifact that fails validation is rejected loudly with the specific
//!    violations — never repaired.
//!
//! A freshly induced ontology is a PROPOSAL: it carries `prism:status
//! "draft"` and [`register::register_induced`] refuses to put it on the
//! extraction vocabulary path until it has been deliberately promoted
//! ([`ttl::promote_artifact`]). This mirrors the evidence-class discipline in
//! `prism_provenance::emmo` — LLM output starts at the bottom of the trust
//! ladder and only a deliberate act moves it up. (`EvidenceClass` itself
//! grades individual FACTS and is deliberately not reused here: a vocabulary
//! is either allowed to govern writes or it is not — a two-state gate, not a
//! four-level fact grading.)

pub mod align;
pub mod corpus;
pub mod register;
pub mod ttl;
pub mod validate;

use anyhow::{Context, Result, bail};
use prism_provenance::QuantitySignDomain;
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::semantic_validation::{
    NearDuplicatePolicy, OntologyLabelProposal, OntologySemanticValidationReport,
};
use corpus::Corpus;

/// Version of the induction prompt. Bump on ANY change to
/// [`induction_prompt`]'s text so artifact differences stay attributable:
/// the value is stamped into every artifact as `prism:promptVersion`.
/// v2: the prompt's examples are domain-abstract placeholders instead of
/// metallurgy vocabulary — induction is the only door to a non-materials
/// ontology, and it must not lean on materials English.
pub const PROMPT_VERSION: &str = "2";

/// Namespace of the PRISM annotation vocabulary (status, provenance keys).
pub const PRISM_META_NS: &str = "https://prism.marc27.com/ontology/meta#";

/// The artifact literal for a quantity sign-domain declaration.
///
/// The annotation lives in the ARTIFACT (`prism:signDomain` on a class —
/// see [`ttl`]), so a promoted ontology supplies sign constraints with zero
/// Rust edits; these two functions are the only place the literal form and
/// [`QuantitySignDomain`] meet.
#[must_use]
pub fn sign_domain_as_stored(domain: QuantitySignDomain) -> &'static str {
    match domain {
        QuantitySignDomain::Unspecified => "unspecified",
        QuantitySignDomain::NonNegative => "non_negative",
        QuantitySignDomain::Signed => "signed",
    }
}

/// Strict parse of the artifact literal: an unknown value is `None`, which
/// callers turn into a loud refusal — a tampered or foreign annotation must
/// not silently read as silence.
#[must_use]
pub fn sign_domain_from_stored(value: &str) -> Option<QuantitySignDomain> {
    match value {
        "unspecified" => Some(QuantitySignDomain::Unspecified),
        "non_negative" => Some(QuantitySignDomain::NonNegative),
        "signed" => Some(QuantitySignDomain::Signed),
        _ => None,
    }
}

/// Which TYPED FACT SHAPE a declared relation fills in the store.
///
/// The store's typed fact kinds are a closed surface — `measurement`,
/// `phase`, `processing`, `contains` — but WHICH relation fills each one is
/// the ontology's statement, never Rust's. EMMO answers it by looking up its
/// own `HAS_PROPERTY`/`HAS_PHASE`/`PROCESSED_BY`/`CONTAINS` declarations; an
/// induced ontology answers it with a `prism:factKind` annotation on the
/// relation (see [`ttl`]).
///
/// Without this, a promoted ontology declares relations the store cannot
/// type, and every numeric value it extracts is reported as unstorable —
/// registration without parity. A relation carrying no annotation is simply
/// a generic edge, which is the honest default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InducedFactKind {
    /// Carries a measured quantity: a typed `value` plus its `unit`.
    Measurement,
    /// Relates a subject to a phase it exhibits.
    Phase,
    /// Relates a subject to the process that produced it.
    Processing,
    /// Relates a whole to a constituent it contains.
    Contains,
}

/// The artifact literal for a relation's fact kind. As with
/// [`sign_domain_as_stored`], these two functions are the only place the
/// literal form and the enum meet.
#[must_use]
pub fn fact_kind_as_stored(kind: InducedFactKind) -> &'static str {
    match kind {
        InducedFactKind::Measurement => "measurement",
        InducedFactKind::Phase => "phase",
        InducedFactKind::Processing => "processing",
        InducedFactKind::Contains => "contains",
    }
}

/// Strict parse of the artifact literal: an unknown value is `None`, which
/// callers turn into a loud refusal rather than silently reading as
/// "generic edge" — a tampered annotation must not quietly lose typing.
#[must_use]
pub fn fact_kind_from_stored(value: &str) -> Option<InducedFactKind> {
    match value {
        "measurement" => Some(InducedFactKind::Measurement),
        "phase" => Some(InducedFactKind::Phase),
        "processing" => Some(InducedFactKind::Processing),
        "contains" => Some(InducedFactKind::Contains),
        _ => None,
    }
}

/// Base IRI (no trailing `#`) of the ontology for `domain` — also the
/// ontology's own IRI. Classes and relations mint under `{base}#{slug}`.
#[must_use]
pub fn ontology_iri(domain: &str) -> String {
    format!("https://prism.marc27.com/ontology/{domain}")
}

/// The namespace classes/relations of `domain` mint under.
#[must_use]
pub fn domain_namespace(domain: &str) -> String {
    format!("{}#", ontology_iri(domain))
}

/// Lifecycle status of an induced ontology. A draft is a proposal: it can be
/// inspected, validated and aligned, but [`register::register_induced`]
/// refuses it. Promotion to `Accepted` is a deliberate act
/// ([`ttl::promote_artifact`]), never a side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OntologyStatus {
    Draft,
    Accepted,
}

impl OntologyStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Accepted => "accepted",
        }
    }

    /// Strict: an unknown status string is an error, never a silent default —
    /// a tampered or foreign artifact must not pass for either state.
    pub fn from_stored(value: &str) -> Result<Self> {
        match value {
            "draft" => Ok(Self::Draft),
            "accepted" => Ok(Self::Accepted),
            other => {
                bail!("unknown ontology status {other:?} (expected \"draft\" or \"accepted\")")
            }
        }
    }
}

/// One induced class.
#[derive(Debug, Clone, PartialEq)]
pub struct InducedClass {
    /// Human label, first surface form seen (`skos:prefLabel`).
    pub label: String,
    /// One-sentence definition from the model (`rdfs:comment`); may be empty.
    pub definition: String,
    /// Parent class LABEL (`rdfs:subClassOf` after IRI resolution), if any.
    pub parent: Option<String>,
    /// IRI of an equivalent class in a standard vocabulary
    /// (`skos:exactMatch`), when alignment found one.
    pub aligned_iri: Option<String>,
    /// True when the class was never proposed as a class but was referenced
    /// as a parent/domain/range, so the builder declared it to keep the
    /// artifact referentially closed. Recorded, never silent.
    pub declared_by_reference: bool,
    /// Optional `prism:signDomain` annotation: the sign constraint the
    /// domain declares for quantities of this kind (also served from a
    /// dimensional parent — the adapter walks declared ancestors). `None`
    /// is the artifact's silence and stays silence: the grounding sign
    /// check does not apply.
    pub sign_domain: Option<QuantitySignDomain>,
}

/// One induced relation (an `owl:ObjectProperty`).
#[derive(Debug, Clone, PartialEq)]
pub struct InducedRelation {
    pub label: String,
    pub definition: String,
    /// Domain class LABEL — must name a declared class (validated).
    pub domain: String,
    /// Range class LABEL — must name a declared class (validated).
    pub range: String,
    /// `skos:exactMatch` IRI from alignment, when found.
    pub aligned_iri: Option<String>,
    /// Which typed fact shape this relation fills in the store, when the
    /// artifact declares one. `None` means a generic edge — the honest
    /// default for a relation the ontology never typed.
    pub fact_kind: Option<InducedFactKind>,
}

/// Provenance of one induction run — stamped into the artifact so that two
/// differing artifacts over the same corpus are attributable to model,
/// prompt or corpus, never a mystery.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InductionProvenance {
    /// LLM model id, e.g. `qwen2.5:3b`.
    pub model: String,
    /// [`PROMPT_VERSION`] at induction time.
    pub prompt_version: String,
    /// `sha256:<hex>` over the corpus documents AS FED to the model.
    pub corpus_hash: String,
    /// Documents in the corpus.
    pub documents_total: usize,
    /// Documents whose model response stayed unusable after one retry.
    pub documents_failed: usize,
    /// Classes/relations dropped for empty or unusable labels.
    pub malformed_items: usize,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 promotion time, once accepted.
    pub promoted_at: Option<String>,
    /// Parent links the builder dropped to keep the hierarchy acyclic,
    /// formatted `child -> parent`. Loud in the artifact, not silent.
    pub dropped_parent_links: Vec<String>,
    /// Human-readable notes about merges the builder had to arbitrate
    /// (e.g. conflicting domain/range for one relation label).
    pub merge_notes: Vec<String>,
    /// Advisory geometry report over raw model-proposed class/relation
    /// labels, captured before deterministic lexical merging. Legacy
    /// artifacts default explicitly to `Unavailable`, never `Applied`.
    pub semantic_validation: OntologySemanticValidationReport,
}

/// A finished induced ontology — what [`ttl::to_turtle`] serialises and
/// [`ttl::parse_turtle`] reconstructs.
#[derive(Debug, Clone, PartialEq)]
pub struct InducedOntology {
    /// Domain id, e.g. `alloys` — same character rules as registry ids.
    pub domain: String,
    pub status: OntologyStatus,
    /// Sorted by [`class_slug`] — deterministic artifact order.
    pub classes: Vec<InducedClass>,
    /// Sorted by [`relation_slug`] — deterministic artifact order.
    pub relations: Vec<InducedRelation>,
    pub provenance: InductionProvenance,
}

impl InducedOntology {
    /// Version IRI: deterministic for a (domain, prompt, corpus) triple.
    /// The timestamp lives in provenance, not the version IRI, so a re-run
    /// over the same corpus with the same prompt claims the same version.
    #[must_use]
    pub fn version_iri(&self) -> String {
        let hash8: String = self
            .provenance
            .corpus_hash
            .trim_start_matches("sha256:")
            .chars()
            .take(8)
            .collect();
        format!(
            "{}/version/{}.{}",
            ontology_iri(&self.domain),
            self.provenance.prompt_version,
            hash8
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Label normalisation and IRI minting
// ─────────────────────────────────────────────────────────────────────────

/// Canonical merge/compare key for a label: camelCase is split, every
/// non-alphanumeric run becomes one space, everything lowercases.
/// `"HeatTreatment"`, `"heat-treatment"` and `"Heat Treatment"` all
/// normalise to `"heat treatment"` — the same concept however the model
/// (or EMMO) spelt it.
#[must_use]
pub fn normalize_label(label: &str) -> String {
    let chars: Vec<char> = label.chars().collect();
    let mut out = String::with_capacity(label.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && i > 0 {
                let prev = chars[i - 1];
                // Boundary: aB, 1B, or the last capital of a run followed by
                // lowercase ("BCCPhase" → "bcc phase").
                let boundary = prev.is_lowercase()
                    || prev.is_numeric()
                    || (prev.is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
                if boundary && !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

/// Stable IRI local name for a class label: PascalCase over the normalised
/// words, `N`-prefixed if it would start with a digit. Stable label → stable
/// IRI, run after run.
#[must_use]
pub fn class_slug(label: &str) -> Option<String> {
    let norm = normalize_label(label);
    if norm.is_empty() {
        return None;
    }
    let mut slug = String::with_capacity(norm.len());
    for word in norm.split(' ') {
        let mut cs = word.chars();
        if let Some(first) = cs.next() {
            slug.extend(first.to_uppercase());
            slug.extend(cs);
        }
    }
    if slug.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        slug.insert(0, 'N');
    }
    Some(slug)
}

/// Stable IRI local name for a relation label: camelCase (`"has property"`
/// → `"hasProperty"`).
#[must_use]
pub fn relation_slug(label: &str) -> Option<String> {
    let slug = class_slug(label)?;
    let mut cs = slug.chars();
    let first = cs.next()?;
    Some(first.to_lowercase().chain(cs).collect())
}

/// Relationship-type token for the extraction vocabulary
/// (`"has property"` → `"HAS_PROPERTY"`), matching the house style of the
/// built-in EMMO relationship types.
#[must_use]
pub fn rel_type_token(label: &str) -> Option<String> {
    let norm = normalize_label(label);
    if norm.is_empty() {
        return None;
    }
    Some(norm.split(' ').collect::<Vec<_>>().join("_").to_uppercase())
}

/// Domain ids obey the same rules as ontology registry ids (they become
/// one): non-empty lowercase ASCII alphanumerics plus `-`/`_`.
pub fn validate_domain_id(domain: &str) -> Result<()> {
    if domain.is_empty()
        || !domain
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        bail!(
            "domain {domain:?} must be non-empty lowercase ASCII alphanumerics \
             plus '-'/'_' (it becomes the ontology's registry id)"
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Model proposal wire format
// ─────────────────────────────────────────────────────────────────────────

/// What one LLM call is prompted to return.
#[derive(Debug, Deserialize)]
pub struct ModelProposal {
    #[serde(default)]
    pub classes: Vec<ProposedClass>,
    #[serde(default)]
    pub relations: Vec<ProposedRelation>,
}

#[derive(Debug, Deserialize)]
pub struct ProposedClass {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub definition: String,
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProposedRelation {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub definition: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub range: String,
}

/// Parse a model response into a proposal, surviving the routine small-model
/// failure of wrapping the object in prose: if the whole string is not JSON,
/// the outermost `{…}` span is tried once. Anything else is an error the
/// caller counts against the document — never against the run.
pub fn parse_proposal(raw: &str) -> Result<ModelProposal> {
    if let Ok(p) = serde_json::from_str::<ModelProposal>(raw) {
        return Ok(p);
    }
    let start = raw.find('{');
    let end = raw.rfind('}');
    if let (Some(s), Some(e)) = (start, end)
        && s < e
        && let Ok(p) = serde_json::from_str::<ModelProposal>(&raw[s..=e])
    {
        return Ok(p);
    }
    bail!(
        "model response is not a JSON proposal (first 120 chars: {:?})",
        raw.chars().take(120).collect::<String>()
    )
}

// ─────────────────────────────────────────────────────────────────────────
// Builder: normalise noisy proposals into one coherent draft
// ─────────────────────────────────────────────────────────────────────────

/// Accumulates per-document [`ModelProposal`]s into one draft, normalising
/// the three routine small-model failures — duplicates, dangling references,
/// hierarchy cycles — and RECORDING every normalisation. See the module doc
/// for why this layer repairs (input noise) while [`validate`] never does
/// (finished artifacts).
pub struct OntologyBuilder {
    domain: String,
    /// Keyed by [`normalize_label`]; insertion wins on conflicts so the
    /// merge is deterministic given document order (which is sorted).
    classes: BTreeMap<String, InducedClass>,
    relations: BTreeMap<String, InducedRelation>,
    malformed_items: usize,
    merge_notes: Vec<String>,
}

impl OntologyBuilder {
    pub fn new(domain: &str) -> Result<Self> {
        validate_domain_id(domain)?;
        Ok(Self {
            domain: domain.to_string(),
            classes: BTreeMap::new(),
            relations: BTreeMap::new(),
            malformed_items: 0,
            merge_notes: Vec::new(),
        })
    }

    /// Labels of every class absorbed so far, for prompt vocabulary reuse.
    #[must_use]
    pub fn known_class_labels(&self) -> Vec<&str> {
        self.classes.values().map(|c| c.label.as_str()).collect()
    }

    /// Merge one document's proposal into the draft.
    pub fn absorb(&mut self, proposal: ModelProposal) {
        for class in proposal.classes {
            let label = class.label.trim();
            let key = normalize_label(label);
            if key.is_empty() {
                self.malformed_items += 1;
                continue;
            }
            let parent = class
                .parent
                .as_deref()
                .map(str::trim)
                .filter(|p| !normalize_label(p).is_empty())
                .map(str::to_string);
            let definition = class.definition.trim().to_string();
            match self.classes.entry(key) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(InducedClass {
                        label: label.to_string(),
                        definition,
                        parent,
                        aligned_iri: None,
                        declared_by_reference: false,
                        sign_domain: None,
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    let existing = o.get_mut();
                    if existing.definition.is_empty() && !definition.is_empty() {
                        existing.definition = definition;
                    }
                    if existing.parent.is_none() && parent.is_some() {
                        existing.parent = parent;
                    }
                    // A re-proposal of a declared-by-reference class is the
                    // real declaration.
                    existing.declared_by_reference = false;
                }
            }
        }

        for rel in proposal.relations {
            let label = rel.label.trim();
            let key = normalize_label(label);
            let domain = rel.domain.trim().to_string();
            let range = rel.range.trim().to_string();
            if key.is_empty()
                || normalize_label(&domain).is_empty()
                || normalize_label(&range).is_empty()
            {
                self.malformed_items += 1;
                continue;
            }
            let definition = rel.definition.trim().to_string();
            match self.relations.entry(key) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(InducedRelation {
                        label: label.to_string(),
                        definition,
                        domain,
                        range,
                        aligned_iri: None,
                        fact_kind: None,
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    let existing = o.get_mut();
                    if existing.definition.is_empty() && !definition.is_empty() {
                        existing.definition = definition;
                    }
                    let same = normalize_label(&existing.domain) == normalize_label(&domain)
                        && normalize_label(&existing.range) == normalize_label(&range);
                    if !same {
                        self.merge_notes.push(format!(
                            "relation '{}': kept domain/range '{}'/'{}', ignored later \
                             conflicting proposal '{}'/'{}'",
                            existing.label, existing.domain, existing.range, domain, range
                        ));
                    }
                }
            }
        }
    }

    /// Close the draft: declare referenced-but-undefined classes, break
    /// hierarchy cycles deterministically (each cycle loses the parent link
    /// of its lexicographically first member), resolve parent/domain/range
    /// surface forms to the declared labels, and stamp provenance. The
    /// result is a DRAFT; callers still run [`validate::validate`] on it.
    #[must_use]
    pub fn finish(mut self, mut provenance: InductionProvenance) -> InducedOntology {
        // 1. Referential closure: every parent / domain / range must be a
        //    declared class. The model was instructed to declare them; when
        //    it did not, declare by reference — recorded on the class itself.
        let mut referenced: Vec<String> = Vec::new();
        for class in self.classes.values() {
            if let Some(p) = &class.parent {
                referenced.push(p.clone());
            }
        }
        for rel in self.relations.values() {
            referenced.push(rel.domain.clone());
            referenced.push(rel.range.clone());
        }
        for surface in referenced {
            let key = normalize_label(&surface);
            self.classes.entry(key).or_insert_with(|| InducedClass {
                label: surface.trim().to_string(),
                definition: String::new(),
                parent: None,
                aligned_iri: None,
                declared_by_reference: true,
                sign_domain: None,
            });
        }

        // 2. Canonicalise references to the declared surface form, so the
        //    artifact never carries two spellings of one class.
        let canonical: BTreeMap<String, String> = self
            .classes
            .iter()
            .map(|(k, c)| (k.clone(), c.label.clone()))
            .collect();
        for class in self.classes.values_mut() {
            if let Some(p) = &class.parent {
                class.parent = Some(canonical[&normalize_label(p)].clone());
            }
        }
        for rel in self.relations.values_mut() {
            rel.domain = canonical[&normalize_label(&rel.domain)].clone();
            rel.range = canonical[&normalize_label(&rel.range)].clone();
        }

        // 3. Break subClassOf cycles. Iterating in key order makes the
        //    outcome deterministic: the walk from the lexicographically
        //    first member of a cycle is the one that returns to its start.
        let mut dropped: Vec<String> = Vec::new();
        let keys: Vec<String> = self.classes.keys().cloned().collect();
        for key in &keys {
            let Some(parent) = self.classes[key].parent.clone() else {
                continue;
            };
            let mut seen = std::collections::BTreeSet::new();
            let mut current = normalize_label(&parent);
            let cycles = loop {
                if current == *key {
                    break true; // walking up returned to the child
                }
                if !seen.insert(current.clone()) {
                    break false; // a cycle above, not through, this child
                }
                match self.classes.get(&current).and_then(|c| c.parent.as_ref()) {
                    Some(p) => current = normalize_label(p),
                    None => break false,
                }
            };
            if cycles {
                let child_label = self.classes[key].label.clone();
                dropped.push(format!("{child_label} -> {parent}"));
                if let Some(c) = self.classes.get_mut(key) {
                    c.parent = None;
                }
            }
        }

        provenance.malformed_items += self.malformed_items;
        provenance.dropped_parent_links.extend(dropped);
        provenance.merge_notes.append(&mut self.merge_notes);

        // 4. Deterministic artifact order: sort by minted local name.
        let mut classes: Vec<InducedClass> = self.classes.into_values().collect();
        classes.sort_by_key(|c| class_slug(&c.label).unwrap_or_default());
        let mut relations: Vec<InducedRelation> = self.relations.into_values().collect();
        relations.sort_by_key(|r| relation_slug(&r.label).unwrap_or_default());

        InducedOntology {
            domain: self.domain,
            status: OntologyStatus::Draft,
            classes,
            relations,
            provenance,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The induction pass
// ─────────────────────────────────────────────────────────────────────────

/// Knobs for one induction run.
#[derive(Debug, Clone)]
pub struct InductionConfig {
    pub domain: String,
    /// Cap on document characters fed per prompt (small local models).
    pub max_doc_chars: usize,
    /// Cap on already-known class labels restated per prompt.
    pub max_known_labels: usize,
    /// Advisory near-duplicate policy for raw ontology labels. Typing and
    /// triple checks are intentionally not applied to class-label batches;
    /// instance/assertion geometry is not a reliable ontology-label prior.
    pub semantic_validation: NearDuplicatePolicy,
}

impl InductionConfig {
    pub fn new(domain: &str) -> Result<Self> {
        validate_domain_id(domain)?;
        Ok(Self {
            domain: domain.to_string(),
            max_doc_chars: 4000,
            max_known_labels: 60,
            semantic_validation: NearDuplicatePolicy::default(),
        })
    }
}

/// Build the per-document induction prompt. Bump [`PROMPT_VERSION`] on ANY
/// text change here.
#[must_use]
pub fn induction_prompt(
    domain: &str,
    known_labels: &[&str],
    doc_path: &str,
    doc_text: &str,
) -> String {
    let known = if known_labels.is_empty() {
        "(none yet)".to_string()
    } else {
        known_labels.join(", ")
    };
    format!(
        "You are an ontology engineer. From the document below, propose an ontology \
         fragment for the domain '{domain}': the general CLASSES of things discussed, \
         and the RELATIONS between classes.\n\
         \n\
         Rules:\n\
         - Class labels are general kinds within the domain — the name of a CATEGORY \
         of things the document discusses, NEVER a specific named individual or a \
         specific value, reading or quantity found in the document.\n\
         - Each class may name at most ONE parent class for its is-a hierarchy; \
         use null for top-level classes. The parent must itself appear in \"classes\".\n\
         - Every relation's \"domain\" and \"range\" MUST be labels that appear in \
         \"classes\".\n\
         - Reuse these already-known class labels where they fit instead of inventing \
         synonyms: {known}\n\
         - Keep every definition to one sentence.\n\
         \n\
         Return ONLY valid JSON with this structure:\n\
         {{\n\
           \"classes\": [{{\"label\": \"<general kind>\", \"definition\": \"...\", \"parent\": null}}],\n\
           \"relations\": [{{\"label\": \"<relation label>\", \"definition\": \"...\", \
         \"domain\": \"<general kind>\", \"range\": \"<general kind>\"}}]\n\
         }}\n\
         \n\
         ## Document ({doc_path})\n\
         {doc_text}\n"
    )
}

/// Run the induction pass: one LLM call per corpus document (plus one retry
/// on an unusable response), merged through [`OntologyBuilder`]. Fails only
/// when EVERY document fails — per-document failures are counted into
/// provenance and the run continues, because a small model failing on some
/// documents is the normal case this pipeline exists to survive.
pub async fn induce(
    client: &prism_llm::LlmClient,
    corpus: &Corpus,
    config: &InductionConfig,
) -> Result<InducedOntology> {
    if corpus.docs.is_empty() {
        bail!("corpus at {} contains no documents", corpus.root.display());
    }
    let mut builder = OntologyBuilder::new(&config.domain)?;
    let mut failed = 0usize;
    let mut last_error: Option<String> = None;
    let mut semantic_labels = Vec::new();

    for doc in &corpus.docs {
        let text: String = doc.text.chars().take(config.max_doc_chars).collect();
        let known: Vec<&str> = builder
            .known_class_labels()
            .into_iter()
            .take(config.max_known_labels)
            .collect();
        let prompt = induction_prompt(&config.domain, &known, &doc.rel_path, &text);

        let mut proposal = None;
        for attempt in 1..=2u8 {
            match client.generate_json(&prompt).await {
                Ok(raw) => match parse_proposal(&raw) {
                    Ok(p) => {
                        proposal = Some(p);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(doc = %doc.rel_path, attempt, "unusable proposal: {e:#}");
                        last_error = Some(format!("{e:#}"));
                    }
                },
                Err(e) => {
                    tracing::warn!(doc = %doc.rel_path, attempt, "LLM call failed: {e:#}");
                    last_error = Some(format!("{e:#}"));
                }
            }
        }
        match proposal {
            Some(p) => {
                semantic_labels.extend(
                    p.classes
                        .iter()
                        .filter(|class| !class.label.trim().is_empty())
                        .map(|class| OntologyLabelProposal {
                            label: class.label.clone(),
                            kind: "class".to_string(),
                        }),
                );
                semantic_labels.extend(
                    p.relations
                        .iter()
                        .filter(|relation| !relation.label.trim().is_empty())
                        .map(|relation| OntologyLabelProposal {
                            label: relation.label.clone(),
                            kind: "relation".to_string(),
                        }),
                );
                builder.absorb(p);
            }
            None => failed += 1,
        }
    }

    if failed == corpus.docs.len() {
        bail!(
            "induction produced nothing: all {failed} document(s) failed (model {}) — \
             check the LLM endpoint and model, then re-run. Last error: {}",
            client.config().model,
            last_error.as_deref().unwrap_or("none recorded")
        );
    }

    let provenance = InductionProvenance {
        model: client.config().model.clone(),
        prompt_version: PROMPT_VERSION.to_string(),
        corpus_hash: corpus.hash.clone(),
        documents_total: corpus.docs.len(),
        documents_failed: failed,
        malformed_items: 0,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        promoted_at: None,
        dropped_parent_links: Vec::new(),
        merge_notes: Vec::new(),
        semantic_validation: OntologySemanticValidationReport {
            policy: config.semantic_validation.clone(),
            proposals: semantic_labels,
            ..OntologySemanticValidationReport::default()
        },
    };
    Ok(builder.finish(provenance))
}

/// Parse + structurally validate an artifact file — the ONE production gate every
/// consumer goes through ([`ttl::promote_artifact`], the CLI `ontology
/// validate` surface, [`register::register_induced_from_path`]). A file
/// that parses but fails validation is REJECTED loudly with every specific
/// violation; nothing downstream ever sees it. Semantic status is preserved
/// verbatim in provenance and remains advisory; `Unavailable` is never
/// rewritten to `Applied` by this structural gate.
pub fn load_validated(path: &std::path::Path) -> Result<InducedOntology> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read ontology artifact {}", path.display()))?;
    let ontology = ttl::parse_turtle(&text)
        .with_context(|| format!("cannot parse ontology artifact {}", path.display()))?;
    let violations = validate::validate(&ontology);
    if !violations.is_empty() {
        let mut msg = format!(
            "ontology artifact {} REJECTED: {} violation(s):",
            path.display(),
            violations.len()
        );
        for v in &violations {
            msg.push_str(&format!("\n  [{}] {}", v.rule, v.message));
        }
        bail!(msg);
    }
    Ok(ontology)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CONTRACT CHANGE (PROMPT_VERSION 2): the induction prompt used to teach
    /// the model with metallurgy examples ("Alloy", "Heat Treatment",
    /// "Nb25Mo25Ta25W25", "1400 C", "has property"). Induction is the ONLY
    /// door to a non-materials ontology, and a pharma or legal customer was
    /// being shown a metallurgy lesson as the JSON template. The examples are
    /// now domain-abstract placeholders; this test pins that no materials
    /// vocabulary can creep back in.
    #[test]
    fn induction_prompt_is_domain_abstract() {
        let prompt = induction_prompt(
            "legal",
            &["Statute", "Obligation"],
            "corpus/doc.md",
            "Body text.",
        );
        for banned in [
            "Alloy",
            "Heat Treatment",
            "Nb25Mo25Ta25W25",
            "1400 C",
            "has property",
        ] {
            assert!(
                !prompt.contains(banned),
                "induction prompt carries domain vocabulary {banned:?} — it must stay \
                 domain-abstract so any domain induces without a Rust edit"
            );
        }
        // The structure is still taught, abstractly.
        assert!(prompt.contains("<general kind>"));
        assert!(prompt.contains("<relation label>"));
        assert!(prompt.contains("domain 'legal'"));
        assert!(prompt.contains("Statute, Obligation"), "{prompt}");
    }
}
