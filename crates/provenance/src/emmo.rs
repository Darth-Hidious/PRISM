//! EMMO materials ontology + PROV-O assertions on the same Turso store.
//!
//! Local mirror of marc27-core's cloud ontology writers (`ontology/schema.rs`,
//! `ontology/holistic.rs`, `ontology/prov.rs`) expressed as SQL tables instead
//! of a property graph. Typed entities and edges follow the EMMO taxonomy
//! (Matter, Measurement, Property, Phase, …); every written fact is also
//! reified as a PROV-O assertion with noisy-OR corroboration, so the graph
//! and the audit trail stay consistent.
//!
//! The read API returns the exact shapes the cloud research LLM consumes
//! (`GraphNode` / `GraphEdge` / `TraversalResult` / `RecalledFact`), so a
//! federated fetch from this local store is a drop-in.

use anyhow::{Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use turso::Value;

use crate::{ProvenanceStore, get_opt_str, get_str};

// ─────────────────────────────────────────────────────────────────────────
// Write-side types (mirror core's `ExtractedFact` / `Provenance`)
// ─────────────────────────────────────────────────────────────────────────

/// One extracted fact in the EMMO-aligned shape (mirrors core's
/// `ExtractedFact`, holistic.rs). `kind` routes to the right typed
/// node/edge structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// EMMO shape hint: measurement | phase | composition | contains |
    /// processing | structure | application. Unknown/None falls back to a
    /// generic edge.
    #[serde(default)]
    pub kind: Option<String>,
}

/// A QUDT unit identifier such as `QUDT:K` or `QUDT:W-PER-M-K`.
///
/// This is deliberately an identifier newtype, not a PRISM-specific unit
/// enum: QUDT is the vocabulary, and accepting its open identifier space
/// avoids creating a second, inevitably incomplete unit taxonomy here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct QudtUnit(String);

impl QudtUnit {
    pub fn new(identifier: impl Into<String>) -> Result<Self> {
        let identifier = identifier.into();
        if !identifier.starts_with("QUDT:") || identifier.len() == "QUDT:".len() {
            anyhow::bail!("unit must be a QUDT identifier such as QUDT:K");
        }
        Ok(Self(identifier))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for QudtUnit {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let identifier = String::deserialize(deserializer)?;
        Self::new(identifier).map_err(serde::de::Error::custom)
    }
}

/// A numerical or categorical boundary-condition value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionValue {
    Number(f64),
    Text(String),
}

/// One solver-consumable measurement condition. Numerical conditions carry
/// a QUDT unit; categorical conditions (for example atmosphere=`air`) carry
/// `unit: null` rather than smuggling the condition into prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementCondition {
    pub name: String,
    pub value: ConditionValue,
    #[serde(default)]
    pub unit: Option<QudtUnit>,
}

/// The shared four-level evidence vocabulary, aligned with RHEA-JAX
/// `ClaimStatus`. Colors are presentation labels; these serialized values are
/// the stable machine contract used by facts and computed results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// RED: model assertion with no grounding.
    #[default]
    Indeterminate,
    /// ORANGE: extracted from literature, not independently verified.
    Research,
    /// YELLOW: computed by a cited method.
    Screening,
    /// GREEN: executed or measured with reference evidence.
    ReferenceValidated,
}

impl EvidenceClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Indeterminate => "indeterminate",
            Self::Research => "research",
            Self::Screening => "screening",
            Self::ReferenceValidated => "reference_validated",
        }
    }

    #[must_use]
    pub fn color(self) -> &'static str {
        match self {
            Self::Indeterminate => "red",
            Self::Research => "orange",
            Self::Screening => "yellow",
            Self::ReferenceValidated => "green",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Indeterminate => 0,
            Self::Research => 1,
            Self::Screening => 2,
            Self::ReferenceValidated => 3,
        }
    }

    fn from_stored(value: &str) -> Self {
        match value {
            "research" => Self::Research,
            "screening" => Self::Screening,
            "reference_validated" => Self::ReferenceValidated,
            _ => Self::Indeterminate,
        }
    }
}

/// How a result was produced. This sets the best class the producer is
/// allowed to claim before input evidence is considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    Execution,
    CitedComputation,
    LiteratureExtraction,
    ModelAssertion,
}

/// Classify a result conservatively: only execution has a GREEN ceiling, and
/// the result can never outrank its worst input.
#[must_use]
pub fn evidence_for_result(
    source: EvidenceSource,
    inputs: impl IntoIterator<Item = EvidenceClass>,
) -> EvidenceClass {
    let ceiling = match source {
        EvidenceSource::Execution => EvidenceClass::ReferenceValidated,
        EvidenceSource::CitedComputation => EvidenceClass::Screening,
        EvidenceSource::LiteratureExtraction => EvidenceClass::Research,
        EvidenceSource::ModelAssertion => EvidenceClass::Indeterminate,
    };
    inputs.into_iter().fold(ceiling, |worst, input| {
        if input.rank() < worst.rank() {
            input
        } else {
            worst
        }
    })
}

/// New extraction/storage contract. The legacy [`LocalFact`] remains source
/// compatible for CLI/server/mesh callers, while all new text extraction uses
/// this type so conditions and QUDT units cannot be omitted from the path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<QudtUnit>,
    #[serde(default)]
    pub conditions: Vec<MeasurementCondition>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub evidence_class: EvidenceClass,
}

/// The node labels ONE tabular fact write persists its subject and object
/// under. Supplied by the ingest pipeline from the ACTIVE ontology's
/// declared storage mapping (`Ontology::storage_label`), so the labels the
/// store persists come from the same declaration the extraction prompt and
/// the graph validator read — the store never invents a label on this path.
/// The label is part of the entity KEY (`entity_key`), so it is identity,
/// not decoration.
///
/// The synthetic `Measurement` node a `measurement`-kind fact mints is the
/// one label this does not govern: it is the store's own fact shape, not an
/// extracted entity.
#[derive(Debug, Clone, Copy)]
pub struct FactNodeLabels<'a> {
    pub subject: &'a str,
    pub object: &'a str,
}

/// One ontology-classified entity at the persistence boundary.
///
/// `storage_label` remains the compatibility identity used by [`entity_key`];
/// `entity_type` records what extraction declared, and `class_iri` records the
/// canonical vocabulary identity without re-keying an existing graph.
///
/// @req REQ-OWL-1.4 - Persist declared type and canonical class IRI additively.
#[derive(Debug, Clone, Copy)]
pub struct ClassifiedNode<'a> {
    pub entity_type: &'a str,
    pub storage_label: &'a str,
    pub class_iri: &'a str,
}

/// The classified subject and object of one extracted fact.
///
/// @req REQ-OWL-1.4 - Carry ontology identity through the production fact dispatch.
#[derive(Debug, Clone, Copy)]
pub struct ClassifiedFactNodes<'a> {
    pub subject: ClassifiedNode<'a>,
    pub object: ClassifiedNode<'a>,
}

/// Immutable ontology artifact identity used to classify one assertion.
///
/// @req REQ-OWL-1.5 - Record ontology version IRI and artifact SHA-256.
#[derive(Debug, Clone, Copy)]
pub struct OntologyClassification<'a> {
    pub version_iri: &'a str,
    pub artifact_sha256: &'a str,
}

/// Common storage view implemented by both the additive conditioned contract
/// and the source-compatible legacy fact.
pub trait FactPayload {
    fn to_local_fact(&self) -> LocalFact;
    fn conditions(&self) -> &[MeasurementCondition];
    fn evidence_class(&self) -> EvidenceClass;
}

impl FactPayload for LocalFact {
    fn to_local_fact(&self) -> LocalFact {
        self.clone()
    }

    fn conditions(&self) -> &[MeasurementCondition] {
        &[]
    }

    fn evidence_class(&self) -> EvidenceClass {
        EvidenceClass::Indeterminate
    }
}

fn validate_conditions(conditions: &[MeasurementCondition]) -> Result<()> {
    for condition in conditions {
        if condition.name.trim().is_empty() {
            anyhow::bail!("measurement condition name cannot be empty");
        }
        if matches!(&condition.value, ConditionValue::Number(_)) && condition.unit.is_none() {
            anyhow::bail!(
                "numerical measurement condition '{}' requires a QUDT unit",
                condition.name
            );
        }
    }
    Ok(())
}

impl FactPayload for MaterialFact {
    fn to_local_fact(&self) -> LocalFact {
        LocalFact {
            subject: self.subject.clone(),
            predicate: self.predicate.clone(),
            object: self.object.clone(),
            value: self.value,
            unit: self.unit.as_ref().map(|unit| unit.as_str().to_string()),
            confidence: self.confidence,
            kind: self.kind.clone(),
        }
    }

    fn conditions(&self) -> &[MeasurementCondition] {
        &self.conditions
    }

    fn evidence_class(&self) -> EvidenceClass {
        self.evidence_class
    }
}

/// Who ran the extraction and over what (mirrors core's `Provenance`, plus
/// `locality` = "local" | "cloud" recording where the write happened).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalProvenance {
    pub activity_id: String,
    pub agent_id: String,
    pub agent_kind: String,
    pub source_entity_id: String,
    pub source_kind: String,
    pub tenant: String,
    pub started_at: String,
    pub ended_at: String,
    pub locality: String,
    /// Locator of the ORIGINAL source when this write relays someone else's
    /// knowledge (a mesh peer forwarding what it read elsewhere). `None`
    /// means "derive the independence key from `source_entity_id` as
    /// always" — for a relay that conservatively collapses to
    /// `mesh:unattributed`. When set on a relay, corroboration is keyed on
    /// the origin (namespaced `mesh:…`, see [`origin_source_key_for`]), so
    /// two peers relaying two genuinely different origin sources count as
    /// two pieces of evidence instead of one.
    ///
    /// `#[serde(default)]` keeps previously serialized forms deserializable.
    #[serde(default)]
    pub origin_source_id: Option<String>,
}

/// The decoding/sampling record of one extraction activity, written onto
/// the SAME `prov_activity` row by
/// [`ProvenanceStore::record_activity_decoding`]. `None` fields mean the
/// backend offered no such knob (embedded GGUF, MARC27 `/stream`) — the
/// honest NULL, never a guessed default.
#[derive(Debug, Clone, Copy)]
pub struct ActivityDecoding<'a> {
    /// Sampling seed the request carried.
    pub seed: Option<i64>,
    /// Sampling temperature the request carried.
    pub temperature: Option<f64>,
    /// JSON decoding mode that actually applied:
    /// `json_schema` | `json_object` | `prompt_only`.
    pub mode: Option<&'a str>,
}

/// A subject/predicate/object triple to reify as a PROV-O assertion
/// (mirrors core's `Assertion`; the stable id is derived, not carried).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAssertion {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub confidence: Option<f64>,
}

// ─────────────────────────────────────────────────────────────────────────
// Read-side types — field names must match the cloud shapes EXACTLY
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphNode {
    pub name: String,
    pub entity_type: String,
    pub label: String,
    /// Canonical ontology class identity. `None` is honest legacy/unclassified
    /// data written before the OWL layer or received without a defensible IRI.
    #[serde(default)]
    pub class_iri: Option<String>,
    pub tenant: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub rel_type: String,
    pub count: i64,
    /// Tenant that owns the edge row, so a union read can attribute a
    /// relationship to the node it arrived from. `#[serde(default)]`
    /// keeps payloads serialized before this field deserializable (they
    /// read as `""`, which renderers treat as unattributed).
    #[serde(default)]
    pub tenant: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TraversalResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecalledFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f64,
    pub source: String,
    pub agent: String,
    /// Tenant that owns the assertion. Without this, a union read over
    /// local + mesh tenants returns facts that cannot be attributed to
    /// the node they came from. `#[serde(default)]` keeps previously
    /// serialized payloads deserializable (they read as `""`).
    #[serde(default)]
    pub tenant: String,
}

/// Additive read shape for conditioned, evidence-classed facts. The legacy
/// [`RecalledFact`] remains unchanged so external struct literals and old
/// consumers continue to compile; new scientific reads use this complete
/// shape and therefore never render a fact without its class.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecalledMaterialFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub conditions: Vec<MeasurementCondition>,
    pub evidence_class: EvidenceClass,
    pub confidence: f64,
    pub source: String,
    pub agent: String,
    /// Tenant that owns the assertion (see [`RecalledFact::tenant`]).
    #[serde(default)]
    pub tenant: String,
}

/// One semantic entity hit, attributed to the tenant whose entity row it
/// scored. Similarity is cosine, in `[-1, 1]`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SemanticEntityHit {
    pub name: String,
    pub tenant: String,
    pub similarity: f32,
}

/// One stored per-source evidence contribution for an assertion.
///
/// `recall` reports only the immutable FIRST attribution on the parent row;
/// every corroborating source lives here (see
/// [`ProvenanceStore::assertion_evidence`]).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EvidenceContribution {
    /// Canonical independence key (`doi:…` / `url:…` / `file:…` /
    /// `document:…` / `opaque:…`; relays are `mesh:<origin key>` when the
    /// peer conveyed the origin, `mesh:unattributed` when it did not).
    pub source_key: String,
    /// The locator/display string exactly as this contribution supplied it.
    pub source_entity_id: String,
    pub activity_id: String,
    pub agent_id: String,
    pub confidence: f64,
    pub evidence_class: EvidenceClass,
    /// `"source"` for a real per-source contribution; `"legacy_aggregate"`
    /// for a pre-v5 row whose confidence may contain phantom
    /// self-corroboration (old count preserved in `legacy_corroborations`).
    pub confidence_kind: String,
    pub legacy_corroborations: Option<i64>,
}

/// One auditable ontology classification event for an assertion.
///
/// A repeated assertion may appear here under several ontology versions; the
/// assertion's stable digest is deliberately not affected.
///
/// @req REQ-OWL-1.5 - Answer which ontology artifact classified a fact.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AssertionClassification {
    pub activity_id: String,
    pub version_iri: String,
    pub artifact_sha256: String,
}

// ─────────────────────────────────────────────────────────────────────────
// Canonicalization + assertion identity
// ─────────────────────────────────────────────────────────────────────────

/// Deterministic canonical key: trim, lowercase, collapse whitespace.
/// Self-consistent locally (need not match the cloud's resolver).
#[must_use]
pub fn canonical_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Tenant- and label-qualified entity key
/// ("{tenant}|{label}:{canonical name}").
///
/// Qualifying by label keeps one node per (label, name) — the same name
/// extracted as e.g. both a Phase and a Matter stays two nodes instead of
/// one label-churning row (mirrors core, which keeps a node per label).
///
/// Qualifying by TENANT is what keeps tenants from destroying each other.
/// Every read filters `WHERE tenant = ?`, and `upsert_entity` merges on
/// this key, so a tenant-blind key meant whichever tenant wrote last owned
/// the row and the other one's entity silently disappeared from its own
/// view. `upsert_edge` has always qualified its id by tenant; entities
/// were the outlier.
fn entity_key(tenant: &str, label: &str, name: &str) -> String {
    format!("{tenant}|{label}:{}", canonical_key(name))
}

/// Stable assertion id: SHA-256 of
/// `tenant|canonical(subject)|predicate|canonical(object)`, so re-extraction
/// corroborates one row instead of duplicating facts.
///
/// `tenant` is part of the key, and must stay that way. `prov_assertion` is
/// keyed on this id alone, and `record_assertion_with_context` corroborates
/// by id with no tenant filter — so while the id omitted the tenant, two
/// tenants asserting the same triple shared one row and each raised the
/// other's `confidence` (noisy-OR) and `corroborations`. That is the same
/// cross-tenant ownership problem `entity_key` already fixed for
/// `emmo_entity`; the assertion table had not had the fix applied.
///
/// The tenant is hashed raw rather than through `canonical_key`: a tenant is
/// an exact identifier, and case-folding it would merge two distinct tenants
/// that differ only in case. `predicate` is likewise hashed raw.
#[must_use]
pub fn assertion_id(tenant: &str, subject: &str, predicate: &str, object: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    hash_field(&mut h, tenant.as_bytes());
    hash_field(&mut h, canonical_key(subject).as_bytes());
    hash_field(&mut h, predicate.as_bytes());
    hash_field(&mut h, canonical_key(object).as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Feed one field into the digest, length-prefixed.
///
/// A bare `|` separator is ambiguous, and `canonical_key` does not strip or
/// escape `|` — it only collapses whitespace and lowercases. So with plain
/// separators these two hash identically:
///
/// ```text
/// tenant "acme|steel", subject "UTS"        -> acme|steel|uts|...
/// tenant "acme",       subject "steel|UTS"  -> acme|steel|uts|...
/// ```
///
/// which is one tenant reading and corroborating another tenant's assertion —
/// exactly what putting the tenant in the key was meant to prevent. Prefixing
/// each field with its byte length makes the encoding unambiguous, so no
/// arrangement of separators inside a field can imitate a field boundary.
fn hash_field(h: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest;
    // `u64`, not `usize`: `usize::to_le_bytes()` is 8 bytes on a 64-bit target
    // and 4 on a 32-bit one, so a `usize` prefix would make every id
    // architecture-dependent. Move the database between targets and the same
    // fact hashes differently, the lookup misses, and a duplicate row is
    // inserted with corroborations reset to 1.
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Hash an optional field so that ABSENT and PRESENT-BUT-EMPTY differ.
///
/// A bare `unwrap_or` collapses them: `None` and `Some(0.0)` both become eight
/// zero bytes, and `None` and `Some("")` both become nothing. A measurement of
/// exactly 0.0 is real data in materials science, and it must not share an id
/// with a fact carrying no value at all.
fn hash_optional_field(h: &mut sha2::Sha256, bytes: Option<&[u8]>) {
    use sha2::Digest;
    match bytes {
        None => h.update([0u8]),
        Some(b) => {
            h.update([1u8]);
            hash_field(h, b);
        }
    }
}

/// Stable id of a CONDITIONED assertion — one carrying a value, unit, or
/// measurement conditions, which are part of its identity (see
/// [`assertion_id`] for the bare-triple form the function reduces to when
/// all three are absent). Public so callers that wrote a valued fact (the
/// MatKG loader, tests pinning classification stamps) can locate its
/// assertion row; the hashing itself stays this module's single
/// implementation.
pub fn conditioned_assertion_id(
    tenant: &str,
    subject: &str,
    predicate: &str,
    object: &str,
    value: Option<f64>,
    unit: Option<&str>,
    conditions: &[MeasurementCondition],
) -> Result<String> {
    if value.is_none() && unit.is_none() && conditions.is_empty() {
        return Ok(assertion_id(tenant, subject, predicate, object));
    }

    use sha2::{Digest, Sha256};
    let mut canonical_conditions = conditions.to_vec();
    canonical_conditions.sort_by(|left, right| left.name.cmp(&right.name));
    let mut h = Sha256::new();
    hash_field(&mut h, tenant.as_bytes());
    hash_field(&mut h, canonical_key(subject).as_bytes());
    hash_field(&mut h, predicate.as_bytes());
    hash_field(&mut h, canonical_key(object).as_bytes());
    let value_bytes = value.map(|v| v.to_bits().to_le_bytes());
    hash_optional_field(&mut h, value_bytes.as_ref().map(|b| &b[..]));
    hash_optional_field(&mut h, unit.map(str::as_bytes));
    hash_field(&mut h, &serde_json::to_vec(&canonical_conditions)?);
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ─────────────────────────────────────────────────────────────────────────
// Origin source identity — what counts as the SAME source
// ─────────────────────────────────────────────────────────────────────────

/// Canonical independence key for one origin source.
///
/// `source_entity_id` stays what it always was — a locator/display string —
/// but it is NOT the key corroboration independence is decided on: the same
/// paper reached as `doi:10.x/y` and as `https://doi.org/10.x/y` is one
/// source, and twelve ingests of one file are one observation, not twelve.
/// This function collapses the obvious aliases. It cannot establish true
/// epistemic independence (publisher mirrors without DOI metadata, papers
/// copying each other's numbers, moved files under the path fallback) — it
/// only prevents repeat-ingest and relay double counting.
///
/// `relay` marks a write that carries someone ELSE's knowledge — a mesh peer
/// is an agent/relay, not an independent source. A relayed contribution
/// whose ORIGIN is unknown collapses onto the single conservative key
/// `mesh:unattributed`: all unattributed relays of one assertion count
/// ONCE, never once per peer. Undercounting genuinely different unknown
/// sources is the accepted cost; letting N peers echo one fact into N
/// "corroborations" is exactly the defect this key exists to prevent. A
/// relay that DOES carry its origin is keyed on that origin instead — see
/// [`origin_source_key_for`], which is where `LocalProvenance` writes
/// derive their key.
fn origin_source_key(source_entity_id: &str, relay: bool) -> String {
    if relay {
        return "mesh:unattributed".to_string();
    }
    let source = source_entity_id.trim();
    if let Some(doi) = doi_suffix(source) {
        return format!("doi:{doi}");
    }
    if strip_prefix_ignore_ascii_case(source, "http://").is_some()
        || strip_prefix_ignore_ascii_case(source, "https://").is_some()
    {
        return format!("url:{}", canonical_url(source));
    }
    if let Some(rest) = strip_prefix_ignore_ascii_case(source, "file://") {
        // RFC 8089: an empty authority and `localhost` both mean this
        // machine, so `file:///x`, `file://localhost/x` and the bare path
        // `/x` are one source. A genuine remote authority
        // (`file://server/share/x`) keeps its own `file://host` namespace:
        // merging it with the local path `/server/share/x` would collapse
        // two different sources and silently drop evidence, which is worse
        // than splitting. Local `file:` keys can never collide with it,
        // because `//` never survives `canonical_file_path`.
        let (authority, path) = match rest.find('/') {
            Some(slash) => rest.split_at(slash),
            None => (rest, ""),
        };
        if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
            return format!("file:{}", canonical_file_path(path));
        }
        return format!(
            "file://{}{}",
            authority.to_ascii_lowercase(),
            canonical_file_path(path)
        );
    }
    if source.starts_with('/') {
        return format!("file:{}", canonical_file_path(source));
    }
    if let Some(id) = strip_prefix_ignore_ascii_case(source, "document:") {
        // Importer-assigned document UUIDs are case-insensitive identifiers.
        return format!("document:{}", id.trim().to_ascii_lowercase());
    }
    // Opaque identifiers ("doc:test_paper", bare relative filenames, …):
    // stable per trimmed string, with the file branch's lexical `.`/`..`/
    // `//`/trailing-slash cleanup so `data/x.pdf`, `./data/x.pdf` and
    // `data//x.pdf` are one key. Defensive only: the live ingest path hands
    // over `canonicalize()`d absolute paths, which take the `/` branch.
    // Identifiers without dot/empty segments ("doc:test_paper") pass
    // through unchanged.
    let cleaned = canonical_file_path(source);
    let cleaned = cleaned.strip_prefix('/').unwrap_or(&cleaned);
    format!("opaque:{cleaned}")
}

/// True when this write relays someone else's knowledge rather than reading
/// the source itself. `crates/mesh/src/sync.rs` marks its writes with
/// `locality = "mesh"` and a per-peer tenant `mesh:{publisher node id}`
/// (historically the single shared tenant `"mesh"`); any of the three alone
/// is treated as a relay so a partially-filled provenance errs on the
/// conservative side.
///
/// Takes the two markers rather than a `LocalProvenance` so the v5 migration
/// — which recovers `locality` from the stored activity row — classifies
/// with the SAME rule as the live path instead of an approximation of it.
fn is_relay(locality: &str, tenant: &str) -> bool {
    locality == "mesh" || is_mesh_tenant(tenant)
}

/// Whether `tenant` is a mesh tenant — the legacy shared `"mesh"` or a
/// per-peer `"mesh:{node_id}"`. Shared by [`is_relay`] and the peer-echo
/// tripwire so "what counts as a peer" cannot drift between the two.
fn is_mesh_tenant(tenant: &str) -> bool {
    tenant == "mesh" || tenant.starts_with("mesh:")
}

/// Independence key for one write, honouring an explicit origin when the
/// provenance carries one ([`LocalProvenance::origin_source_id`]).
///
/// - **No explicit origin**: exactly the historical derivation from
///   `source_entity_id` — locals normalize per [`origin_source_key`],
///   relays collapse to `mesh:unattributed`.
/// - **Local write with an explicit origin**: the writer read the source
///   itself and is trusted; the key derives from the stated origin.
/// - **Relay with an explicit origin**: the origin string is PEER-SUPPLIED
///   input — the peer chooses it. It gets the same alias-collapsing
///   normalization (so two peers naming one DOI two ways still count once),
///   but the result is then namespaced under `mesh:` so an attacker-chosen
///   origin can never equal a locally-derived key. Local derivation can
///   never produce a `mesh:…` key either — a literal `mesh:…` locator falls
///   through to the opaque branch and becomes `opaque:mesh:…` — so the two
///   namespaces are disjoint by construction: a relay cannot claim `local`
///   origin, and a local write cannot be mistaken for a relay. Even if a
///   future bug wrote a relay under a non-mesh tenant (today `is_relay`
///   implies tenant "mesh" or "mesh:{node id}", whose assertion ids are
///   tenant-separated anyway), its evidence key still could not collide
///   with — or suppress, via the same-source dedupe — any local source's
///   contribution.
fn origin_source_key_for(prov: &LocalProvenance) -> String {
    let origin = prov
        .origin_source_id
        .as_deref()
        .map(str::trim)
        .filter(|origin| !origin.is_empty());
    match (origin, is_relay(&prov.locality, &prov.tenant)) {
        (Some(origin), true) => format!("mesh:{}", origin_source_key(origin, false)),
        (Some(origin), false) => origin_source_key(origin, false),
        (None, relay) => origin_source_key(&prov.source_entity_id, relay),
    }
}

/// The DOI when `source` is one, in normalized form: prefix stripped,
/// percent-decoded, trimmed, lowercased (DOIs are case-insensitive by spec).
///
/// Percent-decoding is applied to EVERY form, exactly ONCE. DOI suffixes
/// containing `/` commonly travel `%2F`-encoded in the `doi:` form too, and
/// one decode pass is precisely what the doi.org resolver applies to an
/// incoming URL. Decoding to a fixpoint would merge distinct DOIs: a
/// doubly-encoded `%252F` names a DOI whose suffix contains the literal
/// characters `%2F` (DOIs may contain `%`), not the plain-slash DOI.
fn doi_suffix(source: &str) -> Option<String> {
    if let Some(doi) = strip_prefix_ignore_ascii_case(source, "doi:") {
        return Some(percent_decode(doi).trim().to_lowercase());
    }
    for resolver in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
    ] {
        if let Some(doi) = strip_prefix_ignore_ascii_case(source, resolver) {
            return Some(percent_decode(doi).trim().to_lowercase());
        }
    }
    None
}

fn strip_prefix_ignore_ascii_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Decode `%XX` escapes; malformed escapes pass through literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes.get(i), bytes.get(i + 1), bytes.get(i + 2)) {
            (Some(b'%'), Some(&hi), Some(&lo)) => {
                let decode = |b: u8| char::from(b).to_digit(16).map(|digit| digit as u8);
                if let (Some(hi), Some(lo)) = (decode(hi), decode(lo)) {
                    out.push(hi * 16 + lo);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Conservative URL canonicalization: lowercase scheme and host, drop the
/// fragment (it never reaches the server, so it cannot distinguish
/// resources), drop the scheme's default port, normalize `.`/`..` path
/// segments. The query string and its ORDER are preserved — aggressive query
/// stripping merges genuinely different resources, which is the wrong
/// direction for an independence key. For the same reason a trailing slash
/// on a NON-root path (`/a` vs `/a/`) and a bare empty `?` stay distinct:
/// a server may legitimately serve different content for them, and this key
/// must only ever split too much, never merge two real sources.
fn canonical_url(url: &str) -> String {
    let url = url.split('#').next().unwrap_or(url);
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let scheme = scheme.to_ascii_lowercase();
    let (authority, path, query) = match (rest.find('/'), rest.find('?')) {
        (Some(slash), Some(qmark)) if qmark < slash => {
            (&rest[..qmark], "", Some(&rest[qmark + 1..]))
        }
        (Some(slash), _) => match rest[slash..].split_once('?') {
            Some((path, query)) => (&rest[..slash], path, Some(query)),
            None => (&rest[..slash], &rest[slash..], None),
        },
        (None, Some(qmark)) => (&rest[..qmark], "", Some(&rest[qmark + 1..])),
        (None, None) => (rest, "", None),
    };
    let mut authority = authority.to_ascii_lowercase();
    let default_port = if scheme == "https" { ":443" } else { ":80" };
    if let Some(bare) = authority.strip_suffix(default_port) {
        authority = bare.to_string();
    }
    // An absent path and `/` are the same resource for http(s).
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        remove_dot_segments(path)
    };
    match query {
        Some(query) => format!("{scheme}://{authority}{path}?{query}"),
        None => format!("{scheme}://{authority}{path}"),
    }
}

/// Lexical `.`/`..`/`//` normalization for an absolute path. Deliberately no
/// filesystem access: the file may not exist where the key is derived, and a
/// key must not depend on local disk state.
fn canonical_file_path(path: &str) -> String {
    let normalized = remove_dot_segments(path.trim());
    match normalized.strip_suffix('/') {
        Some(bare) if !bare.is_empty() => bare.to_string(),
        _ => normalized,
    }
}

/// RFC 3986-style dot-segment removal over `/`-separated segments. Empty
/// segments (`//`) collapse too: for an independence key, treating `a//b`
/// and `a/b` as one resource errs toward merging aliases, never splitting.
fn remove_dot_segments(path: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                kept.pop();
            }
            segment => kept.push(segment),
        }
    }
    let mut out = String::from("/");
    out.push_str(&kept.join("/"));
    if (path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..")) && out != "/" {
        out.push('/');
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────
// Write transactions
// ─────────────────────────────────────────────────────────────────────────

/// Typed, retriable "the store's write lock could not be acquired" error.
///
/// Raised when another writer holds the lock past the connection's busy
/// timeout. The fact was NOT committed — no partial EMMO/activity/assertion/
/// evidence rows exist — and the caller may retry. This must never be
/// converted into silent success or a fake duplicate: the caller has to know
/// the fact is not in the store. Detect it with
/// `err.downcast_ref::<StoreBusy>()` anywhere in the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreBusy;

impl std::fmt::Display for StoreBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "provenance store is busy: another writer held the write lock past the busy \
             timeout; nothing was committed — retry the write",
        )
    }
}

impl std::error::Error for StoreBusy {}

/// Tag engine-level busy errors with [`StoreBusy`] so callers can classify
/// without string-matching. Anything else passes through untouched.
fn classify_busy(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<turso::Error>() {
        Some(turso::Error::Busy(_) | turso::Error::BusySnapshot(_)) => error.context(StoreBusy),
        _ => error,
    }
}

/// `BEGIN IMMEDIATE`, not deferred: the write lock is taken up front, so a
/// concurrent writer of the same rows waits here (up to `busy_timeout`)
/// instead of both reading, both writing, and one dying on a key conflict
/// mid-document.
///
/// Returns the engine's RAII transaction guard rather than `()`, so the
/// transaction cannot outlive the scope that opened it. If the guard is
/// dropped without [`finish_write_txn`] — async cancellation dropping the
/// future between two statements, or a panic unwinding through them — its
/// `Drop` marks the connection, and the NEXT operation on this handle rolls
/// the abandoned transaction back before doing anything else
/// (`turso::Connection::maybe_handle_dangling_tx`, run at the top of every
/// `query`/`execute`). Rollback is async and `Drop` is not, so an eager
/// rollback in `Drop` is impossible; deferring it to the next operation is
/// the engine's own resolution, and it is sufficient because NOTHING can
/// observe the abandoned state — any read rolls back before returning rows,
/// and any write rolls back before its own `BEGIN`. Residual, stated
/// honestly: until some operation touches this handle (or the connection
/// closes, which discards the transaction), the database file lock stays
/// held and writers on OTHER handles wait out their busy timeout.
async fn begin_immediate(conn: &turso::Connection) -> Result<turso::transaction::Transaction<'_>> {
    turso::transaction::Transaction::new_unchecked(
        conn,
        turso::transaction::TransactionBehavior::Immediate,
    )
    .await
    .map_err(|e| classify_busy(anyhow::Error::new(e)))
}

/// COMMIT the open transaction on success; ROLLBACK on failure so no
/// partial fact survives. Every error out of here is busy-classified.
/// If a terminal statement itself fails, the guard's drop flag still heals
/// the connection on its next operation (see [`begin_immediate`]).
async fn finish_write_txn(
    txn: turso::transaction::Transaction<'_>,
    result: Result<()>,
) -> Result<()> {
    match result {
        Ok(()) => txn.commit().await.map_err(|e| {
            classify_busy(anyhow::Error::new(e)).context(
                "commit failed; the transaction rolls back on the connection's next operation",
            )
        }),
        Err(e) => {
            // Explicit so the failure path releases the file lock NOW; on a
            // rollback error the deferred drop-flag path takes over.
            let _ = txn.rollback().await;
            Err(classify_busy(e))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Schema (called from `ProvenanceStore::init_schema`)
// ─────────────────────────────────────────────────────────────────────────

/// Move existing `prov_assertion` rows onto tenant-scoped ids.
///
/// Without this, adding the tenant to [`assertion_id`] would silently orphan
/// every row already in a user's `~/.prism/provenance.db`: the next write of
/// the same triple computes a different id, misses the old row, and inserts a
/// duplicate whose `corroborations` restarts at 1. The row's own columns carry
/// everything the id is derived from, so the new id is recomputable in place.
///
/// Idempotent: after one pass every id already equals the recomputed value, so
/// a second run finds nothing to do. `UPDATE OR IGNORE` rather than `UPDATE` so
/// a row whose target id somehow exists is left alone instead of aborting
/// `open()` on a primary-key violation.
///
/// **What it cannot repair:** rows that already merged across tenants before
/// the fix are a single row with one tenant recorded. That history cannot be
/// split back apart — this assigns such a row wholly to the tenant it stores.
/// Caller must hold the one-shot guard: this is invoked only from
/// [`run_key_migrations`], which owns the `user_version` check and the stamp.
async fn rekey_assertions_by_tenant(conn: &turso::Connection) -> Result<()> {
    // Collect every re-key before issuing any write. Turso is sensitive to
    // interleaved statements on one connection, which is why the read paths
    // in this file drain their cursors before writing.
    // (old_id, new_id, resolved_tenant)
    let mut pending: Vec<(String, String, String)> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT id, subject, predicate, object, value, unit, conditions_json, tenant \
                 FROM prov_assertion",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let old_id = crate::get_str(&row, 0)?;
            let conditions_json = crate::get_str(&row, 6)?;
            // A row with unreadable conditions is left exactly as it is: a
            // best-effort re-key must never destroy a fact it cannot parse.
            let Ok(conditions) =
                serde_json::from_str::<Vec<MeasurementCondition>>(if conditions_json.is_empty() {
                    "[]"
                } else {
                    &conditions_json
                })
            else {
                continue;
            };
            let unit = match row.get_value(5)? {
                Value::Text(unit) if !unit.is_empty() => Some(unit),
                _ => None,
            };
            let tenant = legacy_tenant(&crate::get_str(&row, 7)?).to_string();
            let new_id = conditioned_assertion_id(
                &tenant,
                &crate::get_str(&row, 1)?,
                &crate::get_str(&row, 2)?,
                &crate::get_str(&row, 3)?,
                row.get_value(4).ok().and_then(|v| v.as_real().copied()),
                unit.as_deref(),
                &conditions,
            )?;
            pending.push((old_id, new_id, tenant));
        }
    }

    let count = pending.len();
    let mut skipped = 0usize;
    for (old_id, new_id, tenant) in pending {
        // `OR IGNORE` so an unexpected id collision cannot abort `open()`.
        // But a collision means the row keeps its OLD tenant-less id forever
        // and every later write forks a new row beside it, so it must not pass
        // unnoticed — `execute` returns rows-affected, and 0 is that case.
        // `tenant` is written back as well as the id. Re-keying alone does not
        // rescue a row whose tenant column is NULL: every read path filters
        // `tenant = ?`, so it would stay invisible under a tenant nothing
        // queries. For a row that already had a tenant this writes the same
        // value back and is a no-op.
        let affected = conn
            .execute(
                "UPDATE OR IGNORE prov_assertion SET id = ?1, tenant = ?2 WHERE id = ?3",
                [
                    Value::Text(new_id.clone()),
                    Value::Text(tenant),
                    Value::Text(old_id.clone()),
                ],
            )
            .await?;
        if affected == 0 {
            skipped += 1;
            tracing::warn!(
                old_id,
                new_id,
                "assertion could not be re-keyed: the tenant-scoped id already exists. \
                 This row keeps its pre-tenant id and will not corroborate future writes."
            );
        }
    }
    if count > 0 {
        tracing::info!(
            rekeyed = count - skipped,
            skipped,
            "re-keyed prov_assertion rows onto tenant-scoped assertion ids"
        );
    }

    Ok(())
}

/// Run every one-shot key migration, exactly once per database.
///
/// `init_schema` runs from `ProvenanceStore::open`, and `open` is called on hot
/// paths — the agent loop re-opens per turn and hooks re-open per tool call
/// rather than holding a store. Anything unguarded here is paid on every one of
/// those opens.
///
/// The two migrations share a stamp but NOT a threshold. The assertion re-key
/// is gated on its own generation and the EMMO key migration on the current
/// one, because a database already at v3 has the right assertion digests and
/// only needs its EMMO keys qualified — running the re-key anyway would
/// SHA-256 every assertion in the store for a version that changes no digest,
/// which is precisely the per-open cost this guard exists to remove, just paid
/// once and expensively.
///
/// One stamp, written after both, because the v3-stamped-with-unqualified-keys
/// state is REAL: the old code let `rekey_assertions_by_tenant` stamp v3 and
/// then ran `migrate_keys_to_tenant_qualified` unguarded later in
/// `init_schema`, so a process that died in between left exactly that on disk.
/// Stamping v4 only after the key migration is what lets such a database
/// finish the job on its next open.
///
/// Downgrade hazard, stated rather than hidden: an older PRISM build knows
/// nothing about `user_version` and would write tenant-less ids again; a newer
/// build then sees the version already set and skips them. Recovering from that
/// needs the version reset by hand.
async fn run_key_migrations(conn: &turso::Connection) -> Result<()> {
    // Cheap unlocked pre-check: almost every open is of an already-stamped
    // database and must not pay for a write transaction.
    if read_user_version(conn).await? >= PROV_EVIDENCE_VERSION {
        return Ok(());
    }

    // One writer migrates. `BEGIN IMMEDIATE` serializes concurrent openers,
    // and the version is RE-READ under the lock: two openers can both pass
    // the unlocked pre-check, and without the re-read the loser would redo
    // the whole migration over freshly migrated rows. Everything up to and
    // including the stamp commits atomically — a crash mid-migration leaves
    // the database exactly pre-migration, never half re-keyed.
    let txn = begin_immediate(conn).await?;
    let result = async {
        let version = read_user_version(conn).await?;
        if version >= PROV_EVIDENCE_VERSION {
            return Ok(());
        }

        // Each generation is gated on its OWN threshold, not on the latest.
        // A database already at v3 has the right assertion digests and must
        // not be re-keyed — that scan SHA-256s every assertion in the store,
        // which is the hot-path cost this guard exists to remove. Likewise a
        // v4 database needs only the evidence backfill.
        if version < ASSERTION_TENANT_KEY_VERSION {
            rekey_assertions_by_tenant(conn).await?;
        }
        if version < EMMO_KEY_MIGRATION_VERSION {
            migrate_keys_to_tenant_qualified(conn).await?;
        }
        if version < PROV_EVIDENCE_VERSION {
            migrate_corroborations_to_evidence(conn).await?;
        }

        // Stamp even when nothing needed changing — a fresh store has empty
        // tables, and returning without stamping would make every subsequent
        // open repeat the scans this guard exists to avoid. One stamp, after
        // all generations, so a crash between them re-runs from the last
        // committed generation rather than skipping one.
        conn.execute(
            &format!("PRAGMA user_version = {PROV_EVIDENCE_VERSION}"),
            (),
        )
        .await?;
        Ok(())
    }
    .await;
    finish_write_txn(txn, result).await
}

/// v5 backfill: give every pre-evidence assertion its one still-identifiable
/// evidence contribution.
///
/// A pre-v5 row stores only its LATEST source, and its `corroborations`
/// counted ingest observations, not independent sources — so the honest
/// reconstruction is: one evidence row for the stored source, parent
/// `corroborations` reset to 1, and the stored confidence KEPT rather than
/// replaced (an nth root assumes equal per-observation confidence, a fixed
/// reset fabricates evidence, zero discards valid single-source evidence).
/// Rows whose old count exceeded one are marked `legacy_aggregate` on both
/// the contribution (`confidence_kind`, with the old count preserved in
/// `legacy_corroborations`) and the parent (`confidence_basis`), because the
/// retained confidence may contain phantom self-corroboration and stays
/// tainted when combined with future evidence. The marker is never cleared
/// automatically — exact repair needs a user-directed re-ingest of the
/// original corpus, which no migration can do. The stored source may also be
/// the latest rather than the first; freezing it is honest, claiming
/// recovered first-seen attribution would not be.
///
/// Caller must hold the one-shot guard and the open transaction — see
/// [`run_key_migrations`]. Runs AFTER the id re-keys so evidence rows are
/// born under final assertion ids.
async fn migrate_corroborations_to_evidence(conn: &turso::Connection) -> Result<()> {
    struct LegacyRow {
        id: String,
        confidence: f64,
        corroborations: i64,
        activity_id: String,
        source: String,
        agent: String,
        class: EvidenceClass,
        tenant: String,
        locality: String,
    }

    // Drain every row before writing (Turso pre-release mishandles
    // interleaved statements on one connection). The activity row is JOINed
    // back in because it carries the relay marker (`locality`) the assertion
    // row itself never stored — see the relay classification below.
    let mut pending: Vec<LegacyRow> = Vec::new();
    {
        let mut rows = conn
            .query(
                "SELECT a.id, a.confidence, a.corroborations, a.activity_id, a.source, \
                        a.agent, a.evidence_class, a.tenant, act.locality \
                 FROM prov_assertion a \
                 LEFT JOIN prov_activity act ON act.id = a.activity_id",
                (),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            // NULL/unreadable confidence reads 0.0 — a claim whose belief is
            // unknown must not be resurrected as a strong one.
            let confidence = row
                .get_value(1)
                .ok()
                .and_then(|v| v.as_real().copied())
                .unwrap_or(0.0);
            pending.push(LegacyRow {
                id: crate::get_str(&row, 0)?,
                confidence: if confidence.is_finite() {
                    confidence.clamp(0.0, 1.0)
                } else {
                    0.0
                },
                corroborations: row
                    .get_value(2)
                    .ok()
                    .and_then(|v| v.as_integer().copied())
                    .unwrap_or(1),
                activity_id: crate::get_str(&row, 3)?,
                source: crate::get_str(&row, 4)?,
                agent: crate::get_str(&row, 5)?,
                class: EvidenceClass::from_stored(&crate::get_str(&row, 6)?),
                tenant: crate::get_str(&row, 7)?,
                // NULL (no matching activity row) reads as "".
                locality: crate::get_str(&row, 8)?,
            });
        }
    }

    for legacy in pending {
        // Same derivation AND same relay rule as every future write: a relay
        // is `locality == "mesh" || tenant == "mesh"` ([`is_relay`]). The
        // locality was never persisted on the assertion row, but the
        // assertion's activity row was, and carries it — so it is recovered
        // from there rather than approximated by the tenant alone. Tenant-only
        // classification split one relayed source across two keys (the
        // migration's `url:…` vs the live path's `mesh:unattributed`), so
        // replaying the same relay after migration counted as a second
        // "source" and INFLATED confidence — phantom corroboration, the exact
        // defect this table exists to prevent.
        //
        // A row whose activity row is missing reads an empty locality and
        // falls back to the tenant marker alone. That is the only signal
        // left, and it is the deliberate direction: treating "unknown" as a
        // relay instead would collapse every such row's REAL per-source
        // evidence onto `mesh:unattributed`, destroying genuine
        // corroboration. (In practice the activity row exists: it is written
        // in the same transaction as the assertion.)
        let source_key =
            origin_source_key(&legacy.source, is_relay(&legacy.locality, &legacy.tenant));
        let aggregate = legacy.corroborations > 1;
        let inserted = conn
            .execute(
                r#"INSERT INTO prov_assertion_evidence
                   (assertion_id, source_key, source_entity_id, source_revision_id,
                    activity_id, agent_id, confidence, evidence_class,
                    confidence_kind, legacy_corroborations)
                   VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9)
                   ON CONFLICT(assertion_id, source_key) DO NOTHING"#,
                [
                    Value::Text(legacy.id.clone()),
                    Value::Text(source_key),
                    Value::Text(legacy.source),
                    Value::Text(legacy.activity_id),
                    Value::Text(legacy.agent),
                    Value::Real(legacy.confidence),
                    Value::Text(legacy.class.as_str().to_string()),
                    Value::Text(
                        if aggregate {
                            "legacy_aggregate"
                        } else {
                            "source"
                        }
                        .to_string(),
                    ),
                    if aggregate {
                        Value::Integer(legacy.corroborations)
                    } else {
                        Value::Null
                    },
                ],
            )
            .await?;
        // `inserted == 0` means this assertion already has an evidence row
        // for that source — it was written by the post-v5 path, so its
        // parent aggregates are real. Normalizing it here (a re-run after a
        // hand-rewound `user_version`) would destroy correct counts.
        if inserted == 1 {
            conn.execute(
                "UPDATE prov_assertion \
                 SET confidence = ?2, corroborations = 1, confidence_basis = ?3 \
                 WHERE id = ?1",
                [
                    Value::Text(legacy.id),
                    Value::Real(legacy.confidence),
                    Value::Text(
                        if aggregate {
                            "legacy_aggregate"
                        } else {
                            "native"
                        }
                        .to_string(),
                    ),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

/// Schema generation for this store. Bumped when a migration must run once and
/// then never again; `PRAGMA user_version` is otherwise unused here.
///
/// v1: tenant added to the assertion id.
/// v2: id fields length-prefixed (see `hash_field`), which changes every id
///     again, so the re-key has to run a second time on a v1 database.
/// v3: length prefix widened to `u64` (was architecture-dependent `usize`) and
///     optional fields tagged so absent and empty stop colliding. Both change
///     the digest, so the re-key runs once more.
///
/// Generations above this one do not change the digest, so a database already
/// at v3 must NOT be re-keyed: the scan SHA-256s every assertion in the store,
/// which on a large graph is the exact hot-path cost the guard exists to avoid.
const ASSERTION_TENANT_KEY_VERSION: i64 = 3;

/// Schema generation covering the EMMO key qualification.
///
/// v4: no assertion-digest change. `migrate_keys_to_tenant_qualified` joined
///     the guard — it had been running on every open, rewriting `emmo_edge.id`
///     for every tenanted row each time. Held separate from
///     [`ASSERTION_TENANT_KEY_VERSION`] so a v3 database qualifies its EMMO
///     keys without paying for an assertion re-key it does not need.
const EMMO_KEY_MIGRATION_VERSION: i64 = 4;

/// Schema generation covering per-source evidence, and the value actually
/// stamped once every migration has run.
///
/// v5: no assertion-digest change. Corroboration became per-origin-source
///     instead of per-ingest: `prov_assertion_evidence` records one row per
///     distinct origin source of each assertion, and the parent's
///     `confidence` / `corroborations` become caches over those rows. Pre-v5
///     rows inflated both on every re-ingest of the SAME source, and only
///     their latest source survives — so the migration collapses each row to
///     that one still-identifiable source (`corroborations = 1`), keeps the
///     stored confidence rather than inventing a replacement, and marks rows
///     whose old count exceeded one as `legacy_aggregate` so phantom
///     self-corroboration is never mistaken for independent evidence.
///
/// Held separate from the two above for the same reason they are separate
/// from each other: a v4 database needs only this backfill, and must not pay
/// for an assertion re-key or a key qualification that are already done.
const PROV_EVIDENCE_VERSION: i64 = 5;

/// Tenant to attribute a row to when the stored value is absent.
///
/// `prov_assertion.tenant` is NULL only on a database predating the column,
/// which also predates the mesh tenant entirely — every row in such a store
/// was written by local ingest. Re-keying them under `""` would move them to a
/// tenant no read path ever queries (`recall_with_context` filters
/// `tenant = ?`), silently orphaning exactly the history this migration exists
/// to preserve.
fn legacy_tenant(stored: &str) -> &str {
    if stored.is_empty() { "local" } else { stored }
}

async fn read_user_version(conn: &turso::Connection) -> Result<i64> {
    let mut rows = conn.query("PRAGMA user_version", ()).await?;
    let version = match rows.next().await? {
        Some(row) => row
            .get_value(0)
            .ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(0),
        None => 0,
    };
    // Drain before any write: Turso dislikes interleaved statements.
    while rows.next().await?.is_some() {}
    Ok(version)
}

pub(crate) async fn init_schema(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_entity (
            key TEXT PRIMARY KEY,
            name TEXT,
            label TEXT,
            entity_type TEXT,
            class_iri TEXT,
            tenant TEXT,
            props_json TEXT,
            created_at TEXT
        )"#,
        (),
    )
    .await?;
    // Additive OWL identity for databases created before Stage 1. A legacy
    // row remains NULL: inventing an IRI from an old display label would be
    // less honest than recording that its canonical class is unknown.
    crate::add_column_if_absent(conn, "emmo_entity", "class_iri", "TEXT").await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_tenant ON emmo_entity(tenant)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_label ON emmo_entity(label)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_name ON emmo_entity(name)",
        (),
    )
    .await?;

    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_edge (
            id TEXT PRIMARY KEY,
            source_key TEXT,
            target_key TEXT,
            rel_type TEXT,
            predicate TEXT,
            confidence REAL,
            tenant TEXT,
            props_json TEXT
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_edge_source ON emmo_edge(source_key)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_edge_target ON emmo_edge(target_key)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_edge_tenant ON emmo_edge(tenant)",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS prov_agent (id TEXT PRIMARY KEY, kind TEXT)",
        (),
    )
    .await?;

    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_activity (
            id TEXT PRIMARY KEY,
            agent_id TEXT,
            source_entity_id TEXT,
            tenant TEXT,
            started_at TEXT,
            ended_at TEXT,
            locality TEXT
        )"#,
        (),
    )
    .await?;
    // Additive reproducibility columns (NULL on rows written before they
    // existed, and on runs whose backend offered no such knob): the sampling
    // seed and temperature an LLM extraction actually sent, plus the JSON
    // decoding mode that really applied ('json_schema' = grammar-constrained
    // to the active ontology, 'json_object' / 'prompt_only' = honest
    // degradation). Alongside `agent_id` (the model id) these make a
    // difference between two runs attributable — the model id alone cannot
    // say whether two runs even sampled the same way.
    crate::add_column_if_absent(conn, "prov_activity", "seed", "INTEGER").await?;
    crate::add_column_if_absent(conn, "prov_activity", "temperature", "REAL").await?;
    crate::add_column_if_absent(conn, "prov_activity", "decoding", "TEXT").await?;

    // `prov_assertion` is the query-optimized AGGREGATE row: `confidence`,
    // `corroborations`, and `evidence_class` are caches over
    // `prov_assertion_evidence`, updated in the same transaction as the
    // evidence rows. `activity_id`/`source`/`agent` are the FIRST-committed
    // attribution and are never overwritten after insert.
    // `confidence_basis` is 'native' unless the v5 migration retained a
    // pre-evidence confidence that may contain phantom self-corroboration,
    // in which case it is permanently 'legacy_aggregate'.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_assertion (
            id TEXT PRIMARY KEY,
            subject TEXT,
            predicate TEXT,
            object TEXT,
            value REAL,
            unit TEXT,
            conditions_json TEXT NOT NULL DEFAULT '[]',
            evidence_class TEXT NOT NULL DEFAULT 'indeterminate',
            confidence REAL,
            -- Order-independent sufficient statistics behind `confidence`:
            -- the running PRODUCT of (1 - c_i) over distinct sources and the
            -- running MAX single-source c_i. NULL on rows written before the
            -- columns existed; the aggregate UPDATE lazily seeds them from
            -- the stored confidence.
            confidence_doubt REAL,
            confidence_max REAL,
            corroborations INTEGER,
            confidence_basis TEXT NOT NULL DEFAULT 'native',
            activity_id TEXT,
            source TEXT,
            agent TEXT,
            tenant TEXT
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_tenant ON prov_assertion(tenant)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_subject ON prov_assertion(subject)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_object ON prov_assertion(object)",
        (),
    )
    .await?;

    // Additive migration for databases created before conditioned facts and
    // evidence classes existed. Defaults keep every legacy row readable and
    // conservatively RED; no old value is rewritten or dropped.
    crate::add_column_if_absent(conn, "prov_assertion", "value", "REAL").await?;
    crate::add_column_if_absent(conn, "prov_assertion", "unit", "TEXT").await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion",
        "conditions_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )
    .await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion",
        "evidence_class",
        "TEXT NOT NULL DEFAULT 'indeterminate'",
    )
    .await?;
    // Databases predating multi-tenancy have no `tenant` column at all, and
    // the re-key reads it. The migrations themselves run at the end of this
    // function, once every table they touch exists.
    crate::add_column_if_absent(conn, "prov_assertion", "tenant", "TEXT").await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion",
        "confidence_basis",
        "TEXT NOT NULL DEFAULT 'native'",
    )
    .await?;
    // NULL (not a default) on legacy rows: the aggregate UPDATE seeds both
    // from the stored confidence on the next contribution, treating the
    // pre-existing aggregate as one pseudo-contribution — the same freeze
    // the v5 evidence migration chose.
    crate::add_column_if_absent(conn, "prov_assertion", "confidence_doubt", "REAL").await?;
    crate::add_column_if_absent(conn, "prov_assertion", "confidence_max", "REAL").await?;

    // One row per (assertion, distinct origin source) — the AUTHORITATIVE
    // record corroboration is computed from. Keyed by `source_key`
    // (see `origin_source_key`), NOT by ingestion activity: activity UUIDs
    // identify runs, and counting runs is exactly the self-corroboration
    // defect the v5 migration exists to fix. `source_revision_id` (e.g. a
    // content SHA-256) is attribution metadata, deliberately OUTSIDE the
    // primary key: a file edited in place stays the same source.
    // Rows are never updated (except an evidence-class downgrade) or
    // deleted through the API — subtracting a contribution from noisy-OR
    // needs a full recompute, so mutation is prohibited rather than half
    // supported. The FK keeps evidence attached to its assertion across the
    // id re-key migrations (ON UPDATE CASCADE); it is enforced because
    // `open()` sets `PRAGMA foreign_keys=ON` on every connection.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_assertion_evidence (
            assertion_id TEXT NOT NULL,
            source_key TEXT NOT NULL,

            source_entity_id TEXT NOT NULL,
            source_revision_id TEXT,
            activity_id TEXT NOT NULL,
            agent_id TEXT NOT NULL,

            confidence REAL NOT NULL
                CHECK (confidence >= 0.0 AND confidence <= 1.0),
            evidence_class TEXT NOT NULL
                CHECK (evidence_class IN (
                    'indeterminate',
                    'research',
                    'screening',
                    'reference_validated'
                )),

            confidence_kind TEXT NOT NULL DEFAULT 'source'
                CHECK (confidence_kind IN ('source', 'legacy_aggregate')),
            legacy_corroborations INTEGER
                CHECK (
                    legacy_corroborations IS NULL
                    OR legacy_corroborations >= 1
                ),

            PRIMARY KEY (assertion_id, source_key),

            FOREIGN KEY (assertion_id)
                REFERENCES prov_assertion(id)
                ON UPDATE CASCADE
                ON DELETE CASCADE
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_evidence_source_key \
         ON prov_assertion_evidence(source_key, assertion_id)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_evidence_activity \
         ON prov_assertion_evidence(activity_id)",
        (),
    )
    .await?;

    // Ontology classification is a separate, additive provenance relation.
    // It MUST stay outside `prov_assertion.id`: one stable assertion can be
    // classified under several ontology releases without re-keying the fact.
    // One activity may classify the same assertion only once per artifact;
    // repeated ingests and later ontology versions therefore remain auditable.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_assertion_classification (
            assertion_id TEXT NOT NULL,
            activity_id TEXT NOT NULL,
            ontology_version_iri TEXT NOT NULL
                CHECK (length(trim(ontology_version_iri)) > 0),
            artifact_sha256 TEXT NOT NULL
                CHECK (
                    length(artifact_sha256) = 64
                    AND artifact_sha256 NOT GLOB '*[^0-9a-f]*'
                ),

            PRIMARY KEY (
                assertion_id,
                activity_id,
                ontology_version_iri,
                artifact_sha256
            ),

            FOREIGN KEY (assertion_id)
                REFERENCES prov_assertion(id)
                ON UPDATE CASCADE
                ON DELETE CASCADE,

            FOREIGN KEY (activity_id)
                REFERENCES prov_activity(id)
                ON UPDATE CASCADE
                ON DELETE CASCADE
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_classification_version \
         ON prov_assertion_classification(ontology_version_iri, artifact_sha256)",
        (),
    )
    .await?;

    // Entity vectors for local semantic search: one little-endian f32 blob
    // per emmo_entity key (same encoding as `provenance_embeddings`),
    // written lazily by `embed_and_store_entities` — never on the
    // `write_fact` path. Turso-side counterpart of the Qdrant collection so
    // a local ingest is semantically searchable without any services.
    //
    // `vector` is a plain BLOB on purpose, and it is already Turso's native
    // vector wire format: the engine reads the vector type off the blob
    // ("even-sized blobs are always float32"), not off the column
    // declaration, so `vector_distance_cos(vector, ?)` scores these rows
    // directly. Declaring `F32_BLOB(384)` instead would buy nothing —
    // Turso 0.7 attaches no meaning to it — while baking one embedding
    // model's dimensionality into the schema, which is exactly the thing
    // `semantic_search_entities` has to stay honest about when the backend
    // changes. There is likewise no vector index: `libsql_vector_idx` does
    // not exist in this engine, whose only index method is an experimental
    // sparse-only one, so ranking is a scan — correct, and fine at
    // local-ingest scale.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_embedding (
            key TEXT PRIMARY KEY,
            tenant TEXT,
            dim INTEGER,
            vector BLOB
        )"#,
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_embedding_tenant ON emmo_embedding(tenant)",
        (),
    )
    .await?;

    run_key_migrations(conn).await?;

    Ok(())
}

/// Rewrite pre-existing `{label}:{name}` keys to `{tenant}|{label}:{name}`.
///
/// Entity keys used to omit the tenant, which let one tenant's write take
/// ownership of another's row. Now that the tenant is part of the key, a
/// legacy database would keep its old rows under the old keys: the next
/// re-ingest would write a SECOND row for the same entity, and edges would
/// split across the two key spaces. Rewriting them keeps one row per
/// (tenant, label, name) across the change.
///
/// Idempotent: keys already containing `|` are left alone, so reopening a
/// migrated database is a no-op. Rows whose tenant is NULL/empty are also
/// left alone — there is no tenant to qualify them with, and inventing one
/// would be a worse guess than leaving them where the old readers expect.
///
/// Caller must hold the one-shot guard — see [`run_key_migrations`]. This ran
/// unguarded from `init_schema` for a while, and the edge-id recompute below
/// carried no already-migrated predicate, so every `open()` rewrote the primary
/// key of every tenanted edge row. On the agent's hot path that is a full-table
/// write per turn and per tool call.
///
/// `UPDATE OR IGNORE` throughout, matching `rekey_assertions_by_tenant`: these
/// write primary keys, and a plain `UPDATE` that hits a collision returns `Err`
/// out of `init_schema` — which fails not just the migration but every
/// subsequent `open()` of that database, permanently, until someone edits it by
/// hand. Skipping one row and saying so is recoverable; bricking the store is
/// not.
///
/// "and saying so" is load-bearing and is why each statement re-counts its own
/// predicate afterwards. `execute` returns rows CHANGED, and a row dropped by
/// `OR IGNORE` is not counted — so without the recount a collision is
/// indistinguishable from having had nothing to do. The row that loses a
/// collision keeps its unqualified key, and every read path builds
/// `{tenant}|{key}`, so it becomes unreachable: silent, tenant-scoped data
/// loss. A skip must be loud enough that someone can go and find the row.
async fn migrate_keys_to_tenant_qualified(conn: &turso::Connection) -> Result<()> {
    // `instr(key, '|') = 0` ⇒ not yet qualified. Entities and vectors
    // first, then the edge endpoints that reference them.
    const EDGE_ID_EXPR: &str =
        "tenant || '|' || source_key || '|' || rel_type || '|' || target_key";
    // `emmo_edge.id` is derived from (tenant, source_key, rel_type,
    // target_key), so rewriting the endpoints above invalidates it — the next
    // `upsert_edge` would compute a different id and insert a duplicate.
    // Recompute it from its components, which is what `upsert_edge` does.
    //
    // The already-migrated predicate is what makes a second pass free rather
    // than merely harmless: without it this matches every tenanted row and
    // rewrites each one to the value it already holds.
    //
    // `IS NOT` rather than `<>`, and the three component NULL guards: all three
    // of `source_key`, `rel_type` and `target_key` are nullable `TEXT`, and
    // SQLite's `||` yields NULL if ANY operand is NULL. Under `<>` a row with a
    // NULL component compares NULL — neither true nor false — so it is
    // excluded, the version is stamped, and it is never retried. The guards
    // keep the concatenation non-NULL; `IS NOT` additionally catches a row
    // whose own `id` is NULL.
    let edge_id_where = format!(
        "WHERE tenant IS NOT NULL AND tenant <> '' \
         AND source_key IS NOT NULL AND rel_type IS NOT NULL AND target_key IS NOT NULL \
         AND id IS NOT ({EDGE_ID_EXPR})"
    );
    let edge_id_sql = format!("UPDATE OR IGNORE emmo_edge SET id = {EDGE_ID_EXPR} {edge_id_where}");

    const UNQUALIFIED: &str = "WHERE instr(key, '|') = 0 AND tenant IS NOT NULL AND tenant <> ''";
    let entity_sql =
        format!("UPDATE OR IGNORE emmo_entity SET key = tenant || '|' || key {UNQUALIFIED}");
    let embedding_sql =
        format!("UPDATE OR IGNORE emmo_embedding SET key = tenant || '|' || key {UNQUALIFIED}");
    const EDGE_ENDPOINT: &str = "AND tenant IS NOT NULL AND tenant <> ''";
    let source_sql = format!(
        "UPDATE OR IGNORE emmo_edge SET source_key = tenant || '|' || source_key \
         WHERE instr(source_key, '|') = 0 {EDGE_ENDPOINT}"
    );
    let target_sql = format!(
        "UPDATE OR IGNORE emmo_edge SET target_key = tenant || '|' || target_key \
         WHERE instr(target_key, '|') = 0 {EDGE_ENDPOINT}"
    );

    for (what, sql, remaining_sql) in [
        (
            "emmo_entity.key",
            entity_sql.as_str(),
            format!("SELECT COUNT(*) FROM emmo_entity {UNQUALIFIED}"),
        ),
        (
            "emmo_embedding.key",
            embedding_sql.as_str(),
            format!("SELECT COUNT(*) FROM emmo_embedding {UNQUALIFIED}"),
        ),
        (
            "emmo_edge.source_key",
            source_sql.as_str(),
            format!(
                "SELECT COUNT(*) FROM emmo_edge WHERE instr(source_key, '|') = 0 {EDGE_ENDPOINT}"
            ),
        ),
        (
            "emmo_edge.target_key",
            target_sql.as_str(),
            format!(
                "SELECT COUNT(*) FROM emmo_edge WHERE instr(target_key, '|') = 0 {EDGE_ENDPOINT}"
            ),
        ),
        (
            "emmo_edge.id",
            edge_id_sql.as_str(),
            format!("SELECT COUNT(*) FROM emmo_edge {edge_id_where}"),
        ),
    ] {
        let affected = conn
            .execute(sql, ())
            .await
            .map_err(|e| anyhow::anyhow!(e).context("tenant-qualified key migration failed"))?;
        if affected > 0 {
            tracing::info!(
                column = what,
                rows = affected,
                "tenant-qualified legacy keys"
            );
        }
        // Anything still matching the predicate lost a primary-key collision.
        let skipped = count_matching(conn, &remaining_sql).await?;
        if skipped > 0 {
            tracing::warn!(
                column = what,
                rows = skipped,
                "legacy keys could NOT be tenant-qualified: the qualified key already exists. \
                 These rows keep their unqualified key and are invisible to every tenant-scoped \
                 read. Recovering them means reconciling the duplicate pair by hand."
            );
        }
    }
    Ok(())
}

/// Run a `SELECT COUNT(*)` and drain the cursor before returning.
async fn count_matching(conn: &turso::Connection, sql: &str) -> Result<i64> {
    let mut rows = conn.query(sql, ()).await?;
    let n = match rows.next().await? {
        Some(row) => row
            .get_value(0)
            .ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(0),
        None => 0,
    };
    while rows.next().await?.is_some() {}
    Ok(n)
}

/// Distinct subject/object display names of `facts`, first-seen order,
/// deduped on `canonical_key` — the harvesting the fact-based embedding
/// entry points share with the name-based ones.
fn distinct_fact_names<F: FactPayload>(facts: &[F]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut names: Vec<String> = Vec::new();
    for payload in facts {
        let fact = payload.to_local_fact();
        for name in [fact.subject, fact.object] {
            if seen.insert(canonical_key(&name)) {
                names.push(name);
            }
        }
    }
    names
}

#[derive(Debug, Clone, Copy)]
struct EntityWrite<'a> {
    /// `None` means the caller has no declared extraction type. A fresh row
    /// falls back to its storage label; an existing classified row is not
    /// downgraded by that weaker legacy write.
    entity_type: Option<&'a str>,
    storage_label: &'a str,
    class_iri: Option<&'a str>,
}

impl<'a> EntityWrite<'a> {
    fn legacy(storage_label: &'a str) -> Self {
        Self {
            entity_type: None,
            storage_label,
            class_iri: None,
        }
    }

    fn classified(node: ClassifiedNode<'a>) -> Self {
        Self {
            entity_type: Some(node.entity_type),
            storage_label: node.storage_label,
            class_iri: Some(node.class_iri),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FactWriteMetadata<'a> {
    subject: Option<EntityWrite<'a>>,
    object: Option<EntityWrite<'a>>,
    ontology: Option<OntologyClassification<'a>>,
}

fn validate_classified_node(node: ClassifiedNode<'_>) -> Result<()> {
    if node.entity_type.trim().is_empty() {
        bail!("classified entity type cannot be empty");
    }
    if node.storage_label.trim().is_empty() {
        bail!("classified entity storage label cannot be empty");
    }
    if node.class_iri.trim().is_empty() {
        bail!("classified entity class IRI cannot be empty");
    }
    Ok(())
}

fn validate_ontology_classification(ontology: OntologyClassification<'_>) -> Result<()> {
    if ontology.version_iri.trim().is_empty() {
        bail!("ontology version IRI cannot be empty");
    }
    if ontology.artifact_sha256.len() != 64
        || !ontology
            .artifact_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("ontology artifact SHA-256 must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Write API
// ─────────────────────────────────────────────────────────────────────────

impl ProvenanceStore {
    /// UPSERT one typed entity, merging on its label-qualified canonical key
    /// so re-ingest never duplicates (mirrors core's MERGE-per-label).
    /// Returns the key for edge writes. Last write wins on name;
    /// `props_json` is only replaced when provided.
    async fn upsert_entity(
        &self,
        name: &str,
        entity: EntityWrite<'_>,
        tenant: &str,
        props_json: Option<String>,
    ) -> Result<String> {
        let key = entity_key(tenant, entity.storage_label, name);
        // `tenant` is deliberately NOT in the DO UPDATE set: the key now
        // carries it, so a conflict can only ever be the same tenant
        // re-ingesting. Reassigning it here is what let one tenant take
        // ownership of another's row.
        self.conn
            .execute(
                r#"INSERT INTO emmo_entity
                   (key, name, label, entity_type, class_iri, tenant, props_json, created_at)
                   VALUES (?1, ?2, ?3, COALESCE(?4, ?3), ?5, ?6, ?7, ?8)
                   ON CONFLICT(key) DO UPDATE SET
                       name = excluded.name,
                       label = excluded.label,
                       entity_type = CASE
                           WHEN ?4 IS NULL
                               THEN COALESCE(emmo_entity.entity_type, excluded.entity_type)
                           ELSE ?4
                       END,
                       class_iri = COALESCE(?5, emmo_entity.class_iri),
                       props_json = COALESCE(excluded.props_json, emmo_entity.props_json)"#,
                [
                    Value::Text(key.clone()),
                    Value::Text(name.to_string()),
                    Value::Text(entity.storage_label.to_string()),
                    entity
                        .entity_type
                        .map_or(Value::Null, |kind| Value::Text(kind.to_string())),
                    entity
                        .class_iri
                        .map_or(Value::Null, |iri| Value::Text(iri.to_string())),
                    Value::Text(tenant.to_string()),
                    match props_json {
                        Some(p) => Value::Text(p),
                        None => Value::Null,
                    },
                    Value::Text(Utc::now().to_rfc3339()),
                ],
            )
            .await?;
        Ok(key)
    }

    /// UPSERT one typed edge. The id is deterministic over
    /// (tenant, source, rel_type, target) so re-ingest updates in place.
    /// `props_json` carries edge attributes (e.g. a composition fraction or
    /// a processing-step order) and is only replaced when provided.
    #[allow(clippy::too_many_arguments)]
    async fn upsert_edge(
        &self,
        source_key: &str,
        target_key: &str,
        rel_type: &str,
        predicate: &str,
        confidence: f64,
        tenant: &str,
        props_json: Option<&str>,
    ) -> Result<()> {
        let id = format!("{tenant}|{source_key}|{rel_type}|{target_key}");
        self.conn
            .execute(
                r#"INSERT INTO emmo_edge
                   (id, source_key, target_key, rel_type, predicate, confidence, tenant, props_json)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                   ON CONFLICT(id) DO UPDATE SET
                       predicate = excluded.predicate,
                       confidence = excluded.confidence,
                       props_json = COALESCE(excluded.props_json, emmo_edge.props_json)"#,
                [
                    Value::Text(id),
                    Value::Text(source_key.to_string()),
                    Value::Text(target_key.to_string()),
                    Value::Text(rel_type.to_string()),
                    Value::Text(predicate.to_string()),
                    Value::Real(confidence),
                    Value::Text(tenant.to_string()),
                    match props_json {
                        Some(p) => Value::Text(p.to_string()),
                        None => Value::Null,
                    },
                ],
            )
            .await?;
        Ok(())
    }

    /// Write one fact as typed EMMO entities + edges, routing on `fact.kind`
    /// exactly like core's typed `write_*_fact` writers, then reify it as a
    /// PROV-O assertion so graph and audit trail stay consistent.
    pub async fn write_fact<F: FactPayload>(&self, fact: &F, prov: &LocalProvenance) -> Result<()> {
        self.write_fact_as(fact, prov, fact.evidence_class(), None)
            .await
    }

    /// Write a fact using the store's established graph shape and stamp the
    /// assertion with the ontology artifact that classified it.
    ///
    /// This is the classified text/paper path: its synthetic `Matter`,
    /// `Measurement`, and related storage nodes remain byte-compatible while
    /// the assertion gains an auditable ontology version and artifact hash.
    /// The stamp is not an input to [`assertion_id`] or any graph identity.
    ///
    /// @req REQ-OWL-1.5 - Classify legacy-shaped facts transactionally.
    pub async fn write_fact_with_classification<F: FactPayload>(
        &self,
        fact: &F,
        prov: &LocalProvenance,
        ontology: OntologyClassification<'_>,
    ) -> Result<()> {
        validate_ontology_classification(ontology)?;
        self.write_fact_as(
            fact,
            prov,
            fact.evidence_class(),
            Some(FactWriteMetadata {
                subject: None,
                object: None,
                ontology: Some(ontology),
            }),
        )
        .await
    }

    /// Store a source-compatible legacy fact with an explicit class and the
    /// subject/object node labels the caller's ACTIVE ontology declares for
    /// it. This is the tabular LLM ingest path: the old `LocalFact` shape
    /// cannot carry the evidence field but its origin is known to be
    /// literature/data extraction (ORANGE), not an ungrounded model
    /// assertion (RED) — and its node labels come from the ontology
    /// declaration the prompt and validator already read, never from this
    /// store's legacy hardcoded vocabulary (which labeled every subject
    /// `Matter` no matter what the prompt instructed, splitting the stored
    /// vocabulary from the declared one).
    pub async fn write_fact_with_evidence(
        &self,
        fact: &LocalFact,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
        labels: FactNodeLabels<'_>,
    ) -> Result<()> {
        if labels.subject.trim().is_empty() || labels.object.trim().is_empty() {
            bail!(
                "refusing to write fact '{}' -[{}]-> '{}' with an empty node label",
                fact.subject,
                fact.predicate,
                fact.object
            );
        }
        self.write_fact_as(
            fact,
            prov,
            evidence_class,
            Some(FactWriteMetadata {
                subject: Some(EntityWrite::legacy(labels.subject)),
                object: Some(EntityWrite::legacy(labels.object)),
                ontology: None,
            }),
        )
        .await
    }

    /// Store one tabular fact with its declared node types, canonical class
    /// IRIs, and the exact ontology artifact that performed classification.
    ///
    /// The compatibility `storage_label` alone still feeds [`entity_key`];
    /// neither class IRIs nor ontology metadata enter entity, edge, or
    /// assertion identities. The assertion classification row commits in the
    /// same transaction as the assertion and graph writes.
    ///
    /// @req REQ-OWL-1.4 - Persist canonical class identity without re-keying.
    /// @req REQ-OWL-1.5 - Record version IRI and artifact SHA transactionally.
    pub async fn write_classified_fact_with_evidence(
        &self,
        fact: &LocalFact,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
        nodes: ClassifiedFactNodes<'_>,
        ontology: OntologyClassification<'_>,
    ) -> Result<()> {
        validate_classified_node(nodes.subject)?;
        validate_classified_node(nodes.object)?;
        validate_ontology_classification(ontology)?;
        self.write_fact_as(
            fact,
            prov,
            evidence_class,
            Some(FactWriteMetadata {
                subject: Some(EntityWrite::classified(nodes.subject)),
                object: Some(EntityWrite::classified(nodes.object)),
                ontology: Some(ontology),
            }),
        )
        .await
    }

    /// Write one entity relayed from a mesh peer: the entity under its own
    /// EMMO label with the properties the peer served, a `Dataset` node for
    /// the dataset it arrived through, a `SYNCED_FROM` edge between them,
    /// and the PROV-O assertion — one atomic transaction, exactly like
    /// [`Self::write_fact`].
    ///
    /// This exists because `write_fact`'s generic arm hardcodes the subject
    /// label to `Matter`, so a peer's `Phase` or `CrystalStructure` node
    /// arrived stripped to a bare mislabeled name. The label is part of the
    /// entity KEY (`entity_key`), so it must be right on the first write —
    /// it cannot be patched on afterwards without minting a second node.
    ///
    /// `label` and `props_json` are peer-supplied: the caller (mesh sync) is
    /// responsible for capping/validating them before they get here, the
    /// same way it validates dataset names. The evidence class stays
    /// `Indeterminate`, matching what the old `write_fact` path recorded for
    /// relays: a relay conveys, it does not verify.
    pub async fn write_synced_entity(
        &self,
        name: &str,
        label: &str,
        props_json: Option<String>,
        dataset_name: &str,
        prov: &LocalProvenance,
    ) -> Result<()> {
        self.write_synced_entity_as(
            name,
            EntityWrite::legacy(label),
            props_json,
            dataset_name,
            prov,
        )
        .await
    }

    /// Relay a peer entity while keeping storage identity, declared type, and
    /// an optional peer-supplied canonical class IRI distinct.
    ///
    /// `None` is required when the peer did not supply a defensible IRI; this
    /// method never fabricates one from a display label. The `SYNCED_FROM`
    /// assertion itself is relay provenance and receives no local ontology
    /// classification stamp.
    ///
    /// @req REQ-OWL-1.4 - Preserve classified entity identity across mesh sync.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_synced_entity_with_identity(
        &self,
        name: &str,
        entity_type: &str,
        storage_label: &str,
        class_iri: Option<&str>,
        props_json: Option<String>,
        dataset_name: &str,
        prov: &LocalProvenance,
    ) -> Result<()> {
        if entity_type.trim().is_empty() {
            bail!("synced entity type cannot be empty");
        }
        if storage_label.trim().is_empty() {
            bail!("synced entity storage label cannot be empty");
        }
        if class_iri.is_some_and(|iri| iri.trim().is_empty()) {
            bail!("synced entity class IRI cannot be empty when present");
        }
        self.write_synced_entity_as(
            name,
            EntityWrite {
                entity_type: Some(entity_type),
                storage_label,
                class_iri,
            },
            props_json,
            dataset_name,
            prov,
        )
        .await
    }

    async fn write_synced_entity_as(
        &self,
        name: &str,
        entity: EntityWrite<'_>,
        props_json: Option<String>,
        dataset_name: &str,
        prov: &LocalProvenance,
    ) -> Result<()> {
        let _same_handle_guard = self.write_lock.lock().await;
        let txn = begin_immediate(&self.conn).await?;
        let result: Result<()> = async {
            let (confidence, _class) = self
                .record_assertion_in_open_txn(
                    &LocalAssertion {
                        subject: name.to_string(),
                        predicate: "SYNCED_FROM".into(),
                        object: dataset_name.to_string(),
                        confidence: None,
                    },
                    prov,
                    None,
                    None,
                    &[],
                    EvidenceClass::Indeterminate,
                    None,
                )
                .await?;
            let subj_key = self
                .upsert_entity(name, entity, &prov.tenant, props_json.clone())
                .await?;
            let obj_key = self
                .upsert_entity(
                    dataset_name,
                    EntityWrite::legacy("Dataset"),
                    &prov.tenant,
                    None,
                )
                .await?;
            self.upsert_edge(
                &subj_key,
                &obj_key,
                "SYNCED_FROM",
                "SYNCED_FROM",
                confidence,
                &prov.tenant,
                None,
            )
            .await?;
            Ok(())
        }
        .await;
        finish_write_txn(txn, result).await
    }

    /// UPSERT one extracted entity as a typed node with NO edge — the
    /// tabular ingest's referential-containment path: an entity the
    /// extraction declared but no stored fact references still lands (and
    /// is visible to `graph_search`), instead of being erased because every
    /// edge that named it dangled, or none named it at all.
    ///
    /// `label` is the caller's DECLARED entity type — this method never
    /// invents one. No PROV-O assertion is recorded: an entity declaration
    /// asserts no subject–predicate–object fact, so there is nothing to
    /// reify ([`Self::entity_origin`] honestly answers `None` until a fact
    /// mentions the name). The row is tenant-scoped and timestamped like
    /// every other entity write, and idempotent on its label-qualified key.
    pub async fn write_extracted_entity(
        &self,
        name: &str,
        label: &str,
        props_json: Option<String>,
        tenant: &str,
    ) -> Result<()> {
        if name.trim().is_empty() {
            bail!("refusing to write an entity with an empty name");
        }
        if label.trim().is_empty() {
            bail!("refusing to write entity '{name}' with an empty label");
        }
        // Under the shared write lock so this single-statement write cannot
        // join (and be rolled back with) a raw transaction some other task
        // has open on the one shared connection.
        let _same_handle_guard = self.write_lock.lock().await;
        self.upsert_entity(name, EntityWrite::legacy(label), tenant, props_json)
            .await?;
        Ok(())
    }

    /// Store one relationship-less extracted entity with its declared type and
    /// canonical class IRI while retaining the compatibility storage key.
    ///
    /// @req REQ-OWL-1.4 - Persist standalone classified entities additively.
    pub async fn write_classified_entity(
        &self,
        name: &str,
        node: ClassifiedNode<'_>,
        props_json: Option<String>,
        tenant: &str,
    ) -> Result<()> {
        if name.trim().is_empty() {
            bail!("refusing to write an entity with an empty name");
        }
        validate_classified_node(node)?;
        let _same_handle_guard = self.write_lock.lock().await;
        self.upsert_entity(name, EntityWrite::classified(node), tenant, props_json)
            .await?;
        Ok(())
    }

    /// `labels` carries the subject/object node labels the caller's ontology
    /// declares (tabular ingest); `None` keeps the store's legacy EMMO-shaped
    /// labels (text extraction and every older caller, byte-for-byte
    /// unchanged). The `Measurement` node of a `measurement` fact is the
    /// store's own fact shape and is never caller-labeled.
    async fn write_fact_as<F: FactPayload>(
        &self,
        payload: &F,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
        metadata: Option<FactWriteMetadata<'_>>,
    ) -> Result<()> {
        let conditions = payload.conditions().to_vec();
        validate_conditions(&conditions)?;
        let fact = payload.to_local_fact();
        let tenant = prov.tenant.as_str();

        // Mirror core: a measurement without a value fails schema validation
        // and is dropped (not written half-typed, not recorded as an
        // assertion). Checked before the transaction so a dropped fact never
        // takes the write lock. Defence in depth only: every ingest path
        // rejects this shape upstream WITH a reported reason, so a caller
        // whose fact vanishes here has already miscounted.
        if fact.kind.as_deref() == Some("measurement") && fact.value.is_none() {
            return Ok(());
        }

        // Defence in depth for the unit, through the same controlled
        // vocabulary every ingest path uses (`crate::units::resolve_unit` —
        // never a second table): a measurement's number is meaningless
        // without its unit (880 GPa vs 880 MPa), so a missing or
        // unresolvable unit refuses the write LOUDLY — the old
        // `unwrap_or_default()` here stored an empty-string unit instead.
        // Ingest drops and reports this shape before it gets here; reaching
        // this bail means a caller bypassed that contract. Raw spellings
        // that DO resolve (`"MPa"`) are canonicalised so the store holds
        // one unit vocabulary, not one per path.
        let canonical_unit = match fact.kind.as_deref() {
            Some("measurement") => match fact.unit.as_deref() {
                Some(raw) => match crate::units::resolve_unit(raw) {
                    Some(unit) => Some(unit.as_str().to_string()),
                    None => bail!(
                        "refusing to store measurement '{} {} {}': unit {raw:?} is neither \
                         a QUDT identifier nor a recognised unit spelling — a number stored \
                         without its unit is a wrong number, never stored unit-less",
                        fact.subject,
                        fact.predicate,
                        fact.object
                    ),
                },
                None => bail!(
                    "refusing to store measurement '{} {} {}' carrying value {:?} with no \
                     unit — a unit-less number is a wrong number, never stored unit-less",
                    fact.subject,
                    fact.predicate,
                    fact.object,
                    fact.value
                ),
            },
            _ => fact.unit.clone(),
        };

        // One fact commits atomically: EMMO entities/edges, the PROV-O
        // activity, the assertion, its evidence contribution, and the
        // aggregate update all land or none do. A failed fact rolls back
        // cleanly; facts already committed from the same document stay —
        // atomicity is per fact, not per document.
        //
        // The guard serializes writers sharing THIS handle: a raw
        // `BEGIN IMMEDIATE` on the one shared connection cannot nest, and
        // without the mutex a `tokio::join!` on one store surfaces as an
        // opaque "cannot start a transaction within a transaction", not
        // `StoreBusy` (see `ProvenanceStore::write_lock`).
        let _same_handle_guard = self.write_lock.lock().await;
        let txn = begin_immediate(&self.conn).await?;
        let result: Result<()> = async {
            // The assertion runs FIRST, and the graph writes below reuse the
            // aggregates it returns: `confidence` and `evidence_class` are
            // REBOUND here from this one write's own values to the parent
            // row's post-update state. That is what keeps the graph on the
            // same evidence gate as the assertion — a duplicate source
            // cannot move an edge's confidence, a re-record cannot upgrade a
            // Measurement node's class, and a genuine corroboration lifts
            // the edge to the combined (noisy-OR) confidence instead of the
            // last writer's own number.
            let (confidence, evidence_class) = self
                .record_assertion_in_open_txn(
                    &LocalAssertion {
                        subject: fact.subject.clone(),
                        predicate: fact.predicate.clone(),
                        object: fact.object.clone(),
                        confidence: fact.confidence,
                    },
                    prov,
                    fact.value,
                    canonical_unit.as_deref(),
                    &conditions,
                    evidence_class,
                    metadata.and_then(|details| details.ontology),
                )
                .await?;

            // One binding per node role: caller-supplied classified identity
            // when present, the established storage shape otherwise. Every
            // fact arm uses these bindings so identity semantics do not drift
            // between dispatch paths.
            let subject_entity = metadata
                .and_then(|details| details.subject)
                .unwrap_or_else(|| EntityWrite::legacy("Matter"));
            let object_entity = |legacy: &'static str| {
                metadata
                    .and_then(|details| details.object)
                    .unwrap_or_else(|| EntityWrite::legacy(legacy))
            };

            match fact.kind.as_deref() {
                Some("measurement") => {
                    // Guarded above; destructure the value the guard proved.
                    let Some(value) = fact.value else {
                        return Ok(());
                    };
                    let unit = canonical_unit
                        .clone()
                        .expect("guarded above: a measurement's unit is resolved or refused");
                    let meas_name = format!(
                        "meas_{}_{}_{value}",
                        canonical_key(&fact.subject),
                        canonical_key(&fact.object)
                    );
                    let props = serde_json::json!({
                        "value": value,
                        "unit": unit,
                        "conditions": conditions,
                        "evidence_class": evidence_class,
                        "evidence_color": evidence_class.color(),
                        "confidence": confidence,
                    });
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let meas_key = self
                        .upsert_entity(
                            &meas_name,
                            EntityWrite::legacy("Measurement"),
                            tenant,
                            Some(props.to_string()),
                        )
                        .await?;
                    let obj_key = self
                        .upsert_entity(&fact.object, object_entity("Property"), tenant, None)
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &meas_key,
                        "HAS_MEASUREMENT",
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                    self.upsert_edge(
                        &meas_key,
                        &obj_key,
                        "OF_PROPERTY",
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                }
                Some("phase") => {
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(&fact.object, object_entity("Phase"), tenant, None)
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        "HAS_PHASE",
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                }
                Some("composition") => {
                    let props = serde_json::json!({ "canonical_formula": &fact.object });
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(
                            &fact.object,
                            object_entity("Composition"),
                            tenant,
                            Some(props.to_string()),
                        )
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        "HAS_COMPOSITION",
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                }
                // Mirrors core's Element node + CONTAINS_ELEMENT edge; the
                // composition fraction (when `value` carries it) rides on the
                // edge props, not on the nodes.
                Some("contains") => {
                    let props = fact
                        .value
                        .map(|f| serde_json::json!({ "fraction": f }).to_string());
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(&fact.object, object_entity("Element"), tenant, None)
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        "CONTAINS_ELEMENT",
                        &fact.predicate,
                        confidence,
                        tenant,
                        props.as_deref(),
                    )
                    .await?;
                }
                Some("processing") => {
                    // The step order (when `value` carries it) rides on the edge.
                    let props = fact
                        .value
                        .map(|o| serde_json::json!({ "order": o }).to_string());
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(&fact.object, object_entity("Manufacturing"), tenant, None)
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        "PROCESSED_BY",
                        &fact.predicate,
                        confidence,
                        tenant,
                        props.as_deref(),
                    )
                    .await?;
                }
                Some("structure") => {
                    let props = serde_json::json!({ "system": &fact.object });
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(
                            &fact.object,
                            object_entity("CrystalStructure"),
                            tenant,
                            Some(props.to_string()),
                        )
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        "HAS_STRUCTURE",
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                }
                Some("application") => {
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(&fact.object, object_entity("Application"), tenant, None)
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        "USED_IN",
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                }
                // Unknown kind: keep the fact as a generic edge, don't drop it.
                _ => {
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(&fact.object, object_entity("Entity"), tenant, None)
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        &fact.predicate,
                        &fact.predicate,
                        confidence,
                        tenant,
                        None,
                    )
                    .await?;
                }
            }
            Ok(())
        }
        .await;
        finish_write_txn(txn, result).await
    }

    /// UPSERT the PROV-O agent + activity for one run (idempotent).
    pub async fn record_activity(&self, prov: &LocalProvenance) -> Result<()> {
        // Under the shared write lock: on the one shared connection an
        // unlocked write silently joins whatever raw transaction is
        // currently open — and is erased by that transaction's rollback
        // AFTER this call already returned Ok (see
        // `ProvenanceStore::write_lock`).
        let _same_handle_guard = self.write_lock.lock().await;
        self.record_activity_in_open_txn(prov).await
    }

    /// The agent + activity writes. Caller must hold `write_lock` — either
    /// bare (the two statements run autocommit) or with an open write
    /// transaction (they join it, which is then the caller's own).
    async fn record_activity_in_open_txn(&self, prov: &LocalProvenance) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT INTO prov_agent (id, kind) VALUES (?1, ?2)
                   ON CONFLICT(id) DO UPDATE SET kind = excluded.kind"#,
                [
                    Value::Text(prov.agent_id.clone()),
                    Value::Text(prov.agent_kind.clone()),
                ],
            )
            .await?;
        self.conn
            .execute(
                r#"INSERT INTO prov_activity
                   (id, agent_id, source_entity_id, tenant, started_at, ended_at, locality)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                   ON CONFLICT(id) DO UPDATE SET
                       agent_id = excluded.agent_id,
                       source_entity_id = excluded.source_entity_id,
                       tenant = excluded.tenant,
                       started_at = excluded.started_at,
                       ended_at = excluded.ended_at,
                       locality = excluded.locality"#,
                [
                    Value::Text(prov.activity_id.clone()),
                    Value::Text(prov.agent_id.clone()),
                    Value::Text(prov.source_entity_id.clone()),
                    Value::Text(prov.tenant.clone()),
                    Value::Text(prov.started_at.clone()),
                    Value::Text(prov.ended_at.clone()),
                    Value::Text(prov.locality.clone()),
                ],
            )
            .await?;
        Ok(())
    }

    /// Record the decoding/sampling parameters of one recorded activity —
    /// an UPDATE on the SAME `prov_activity` row [`Self::record_activity`]
    /// wrote (no parallel mechanism, no side table). A missing activity id
    /// is a loud error: silently recording parameters against nothing would
    /// fake the reproducibility trail this exists to provide.
    pub async fn record_activity_decoding(
        &self,
        activity_id: &str,
        decoding: &ActivityDecoding<'_>,
    ) -> Result<()> {
        // Same-handle serialization as record_activity — see write_lock.
        let _same_handle_guard = self.write_lock.lock().await;
        let affected = self
            .conn
            .execute(
                r#"UPDATE prov_activity
                   SET seed = ?2, temperature = ?3, decoding = ?4
                   WHERE id = ?1"#,
                [
                    Value::Text(activity_id.to_string()),
                    decoding.seed.map_or(Value::Null, Value::Integer),
                    decoding.temperature.map_or(Value::Null, Value::Real),
                    decoding
                        .mode
                        .map_or(Value::Null, |mode| Value::Text(mode.to_string())),
                ],
            )
            .await?;
        if affected == 0 {
            bail!(
                "no prov_activity row '{activity_id}' to record decoding parameters on — \
                 record_activity must run first"
            );
        }
        Ok(())
    }

    /// Reify one triple as a PROV-O assertion. The first sighting creates
    /// the assertion at the extractor's confidence with `corroborations = 1`.
    /// A later sighting from a DIFFERENT origin source (see
    /// [`origin_source_key`] — DOI/URL/file aliases collapse, mesh peers are
    /// relays) adds one evidence contribution, combines confidence noisy-OR,
    /// and increments `corroborations`. Re-recording from the SAME source
    /// changes neither: twelve ingests of one paper are one observation, not
    /// twelve. The evidence class can only ever get worse.
    pub async fn record_assertion(&self, a: &LocalAssertion, prov: &LocalProvenance) -> Result<()> {
        self.record_assertion_with_context(a, prov, None, None, &[], EvidenceClass::Indeterminate)
            .await
    }

    /// One handle, one write transaction at a time: the mutex serializes
    /// same-handle writers, because the raw `BEGIN IMMEDIATE` below cannot
    /// nest on the shared connection (see `ProvenanceStore::write_lock`).
    /// Callers on SEPARATE handles serialize via the database busy wait.
    #[allow(clippy::too_many_arguments)]
    async fn record_assertion_with_context(
        &self,
        a: &LocalAssertion,
        prov: &LocalProvenance,
        value: Option<f64>,
        unit: Option<&str>,
        conditions: &[MeasurementCondition],
        evidence_class: EvidenceClass,
    ) -> Result<()> {
        let _same_handle_guard = self.write_lock.lock().await;
        let txn = begin_immediate(&self.conn).await?;
        let result = self
            .record_assertion_in_open_txn(a, prov, value, unit, conditions, evidence_class, None)
            .await
            .map(|_aggregates| ());
        finish_write_txn(txn, result).await
    }

    /// The assertion + evidence write sequence. Caller MUST hold an open
    /// `BEGIN IMMEDIATE` transaction ([`begin_immediate`]/
    /// [`finish_write_txn`]) — this issues writes first and exactly one
    /// fully-drained SELECT at the end (the aggregate read-back), so no open
    /// read cursor ever interleaves with a write (Turso pre-release is
    /// sensitive to interleaved statements on one connection).
    ///
    /// Returns the parent row's POST-update aggregates
    /// `(confidence, evidence_class)` so `write_fact_as` can mirror them
    /// into the EMMO graph in the same transaction — the graph must follow
    /// the evidence gate and `WORST_CLASS_CASE`, never the last writer.
    ///
    /// Why not one UPSERT: a single statement on `prov_assertion` cannot
    /// tell a duplicate assertion from the same source apart from the same
    /// assertion out of a genuinely NEW source, and comparing against the
    /// parent's single `source` column breaks at the third source. The
    /// evidence INSERT's affected-row count is that decision, and the
    /// surrounding transaction is what lets a second table react to it.
    #[allow(clippy::too_many_arguments)]
    async fn record_assertion_in_open_txn(
        &self,
        a: &LocalAssertion,
        prov: &LocalProvenance,
        value: Option<f64>,
        unit: Option<&str>,
        conditions: &[MeasurementCondition],
        evidence_class: EvidenceClass,
        ontology: Option<OntologyClassification<'_>>,
    ) -> Result<(f64, EvidenceClass)> {
        // Normalize before any write. `None` keeps the historical "asserted
        // without a stated confidence = full confidence" contract; NaN and
        // infinity are rejected rather than clamped because they are always
        // an upstream bug, and a NaN inside noisy-OR silently poisons every
        // later combination.
        let confidence_evidence = match a.confidence {
            None => 1.0,
            Some(c) if !c.is_finite() => {
                anyhow::bail!("assertion confidence must be finite, got {c}")
            }
            Some(c) => c.clamp(0.0, 1.0),
        };
        let id = conditioned_assertion_id(
            &prov.tenant,
            &a.subject,
            &a.predicate,
            &a.object,
            value,
            unit,
            conditions,
        )?;
        let conditions_json = serde_json::to_string(conditions)?;
        let source_key = origin_source_key_for(prov);

        self.record_activity_in_open_txn(prov).await?;

        // Create the aggregate row if this is the first sighting. DO NOTHING
        // on conflict: `activity_id`/`source`/`agent` are the FIRST-committed
        // attribution and are immutable from here on — the old path let every
        // later writer overwrite them, so a fact first seen in paper A and
        // corroborated by paper B reported its source as B, with A gone.
        // value/unit/conditions need no update either: they are inputs to the
        // assertion id, so an id match implies they already agree.
        self.conn
            .execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, value, unit,
                    conditions_json, evidence_class, confidence, corroborations,
                    confidence_basis, activity_id, source, agent, tenant)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                           0.0, 0, 'native',
                           ?9, ?10, ?11, ?12)
                   ON CONFLICT(id) DO NOTHING"#,
                [
                    Value::Text(id.clone()),
                    Value::Text(a.subject.clone()),
                    Value::Text(a.predicate.clone()),
                    Value::Text(a.object.clone()),
                    value.map_or(Value::Null, Value::Real),
                    unit.map_or(Value::Null, |unit| Value::Text(unit.to_string())),
                    Value::Text(conditions_json),
                    Value::Text(evidence_class.as_str().to_string()),
                    Value::Text(prov.activity_id.clone()),
                    Value::Text(prov.source_entity_id.clone()),
                    Value::Text(prov.agent_id.clone()),
                    Value::Text(prov.tenant.clone()),
                ],
            )
            .await?;

        // The classification event is joined to the stable assertion rather
        // than folded into its digest. This permits the same fact to be
        // classified under several ontology releases and ensures a later
        // graph-write error rolls the stamp back with the assertion.
        if let Some(ontology) = ontology {
            self.conn
                .execute(
                    r#"INSERT INTO prov_assertion_classification
                       (assertion_id, activity_id, ontology_version_iri, artifact_sha256)
                       VALUES (?1, ?2, ?3, ?4)
                       ON CONFLICT (
                           assertion_id,
                           activity_id,
                           ontology_version_iri,
                           artifact_sha256
                       ) DO NOTHING"#,
                    [
                        Value::Text(id.clone()),
                        Value::Text(prov.activity_id.clone()),
                        Value::Text(ontology.version_iri.to_string()),
                        Value::Text(ontology.artifact_sha256.to_string()),
                    ],
                )
                .await?;
        }

        // Attempt the per-source contribution. The affected-row count IS the
        // independence decision: 1 = genuinely new origin source, 0 = this
        // source already contributed and must not corroborate again. No
        // SELECT-first — the count answers it atomically. On a duplicate the
        // stored contribution's confidence and provenance stay immutable: a
        // later extraction from the same source cannot raise or replace its
        // numeric contribution.
        let inserted = self
            .conn
            .execute(
                r#"INSERT INTO prov_assertion_evidence
                   (assertion_id, source_key, source_entity_id, source_revision_id,
                    activity_id, agent_id, confidence, evidence_class,
                    confidence_kind, legacy_corroborations)
                   VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, 'source', NULL)
                   ON CONFLICT(assertion_id, source_key) DO NOTHING"#,
                [
                    Value::Text(id.clone()),
                    Value::Text(source_key.clone()),
                    Value::Text(prov.source_entity_id.clone()),
                    Value::Text(prov.activity_id.clone()),
                    Value::Text(prov.agent_id.clone()),
                    Value::Real(confidence_evidence),
                    Value::Text(evidence_class.as_str().to_string()),
                ],
            )
            .await?;

        // The evidence class may DOWNGRADE — even from a duplicate source —
        // and never upgrades: agreement cannot turn literature into GREEN,
        // and a source re-read at lower rigor taints what it previously
        // claimed. Unknown stored values normalize to 'indeterminate'.
        const WORST_CLASS_CASE: &str = r#"CASE
                WHEN evidence_class NOT IN (
                    'indeterminate',
                    'research',
                    'screening',
                    'reference_validated'
                ) THEN 'indeterminate'
                WHEN evidence_class = 'indeterminate' OR ?CLASS = 'indeterminate'
                    THEN 'indeterminate'
                WHEN evidence_class = 'research' OR ?CLASS = 'research'
                    THEN 'research'
                WHEN evidence_class = 'screening' OR ?CLASS = 'screening'
                    THEN 'screening'
                ELSE 'reference_validated'
            END"#;
        self.conn
            .execute(
                &format!(
                    "UPDATE prov_assertion_evidence SET evidence_class = {} \
                     WHERE assertion_id = ?1 AND source_key = ?2",
                    WORST_CLASS_CASE.replace("?CLASS", "?3"),
                ),
                [
                    Value::Text(id.clone()),
                    Value::Text(source_key),
                    Value::Text(evidence_class.as_str().to_string()),
                ],
            )
            .await?;
        self.conn
            .execute(
                &format!(
                    "UPDATE prov_assertion SET evidence_class = {} WHERE id = ?1",
                    WORST_CLASS_CASE.replace("?CLASS", "?2"),
                ),
                [
                    Value::Text(id.clone()),
                    Value::Text(evidence_class.as_str().to_string()),
                ],
            )
            .await?;

        // The aggregate runs ONLY when the evidence INSERT actually inserted
        // a row. Entirely in SQL under the held write lock — no application-
        // side old-value read, so no increment can be lost to a concurrent
        // writer.
        //
        // ORDER-INDEPENDENT by construction: the stored sufficient
        // statistics are a running PRODUCT of (1 - c_i) (`confidence_doubt`)
        // and a running MAX c_i (`confidence_max`), both commutative, so
        // the aggregate is a function of the evidence SET, not of arrival
        // order. (The old form combined incrementally against the running
        // `confidence` — MAX(old, combined) — so {1.0, 0.8} yielded 1.0 or
        // 0.99 depending on which source committed first. Exact to the last
        // ULP for any two sources; for three or more, product association
        // order can differ by ~1e-16, never by a rank.)
        //
        // Lazy seeding: rows written before the columns existed have NULL
        // statistics; COALESCE treats their stored confidence as ONE
        // pseudo-contribution (doubt = 1 - c, max = c) — the same freeze the
        // v5 evidence migration chose. A fresh aggregate row has confidence
        // 0.0, which seeds doubt 1.0 / max 0.0, so the first contribution
        // needs no special case.
        if inserted == 1 {
            self.conn
                .execute(
                    r#"UPDATE prov_assertion
                       SET confidence_doubt = COALESCE(
                               confidence_doubt,
                               1.0 - MAX(0.0, MIN(1.0, COALESCE(confidence, 0.0)))
                           ) * (1.0 - ?2),
                           confidence_max = MAX(
                               COALESCE(
                                   confidence_max,
                                   MAX(0.0, MIN(1.0, COALESCE(confidence, 0.0)))
                               ),
                               ?2
                           ),
                           corroborations = COALESCE(corroborations, 0) + 1
                       WHERE id = ?1"#,
                    [Value::Text(id.clone()), Value::Real(confidence_evidence)],
                )
                .await?;
            // Derived in a second statement so it reads the statistics just
            // written, with no reliance on old-row visibility inside one
            // UPDATE. MAX(strongest single source, capped noisy-OR): a lone
            // certain source stays 1.0 — whenever it arrives — while
            // agreement alone never reaches certainty (0.99 cap).
            self.conn
                .execute(
                    r#"UPDATE prov_assertion
                       SET confidence = MAX(
                               confidence_max,
                               MIN(0.99, 1.0 - confidence_doubt)
                           )
                       WHERE id = ?1"#,
                    [Value::Text(id.clone())],
                )
                .await?;
        }

        // Read the parent's post-update aggregates back — inside the same
        // transaction, after every write, cursor fully drained — so the
        // caller's graph writes carry the evidence-gated values, not this
        // one write's own confidence and class.
        let mut rows = self
            .conn
            .query(
                "SELECT confidence, evidence_class FROM prov_assertion WHERE id = ?1",
                [Value::Text(id)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            anyhow::bail!("assertion row vanished inside its own transaction");
        };
        let aggregate_confidence = row
            .get_value(0)
            .ok()
            .and_then(|v| v.as_real().copied())
            .unwrap_or(0.0);
        let aggregate_class = EvidenceClass::from_stored(&get_str(&row, 1)?);
        while rows.next().await?.is_some() {}
        Ok((aggregate_confidence, aggregate_class))
    }

    // ─────────────────────────────────────────────────────────────────────
    // Read API — cloud-shaped, tenant-scoped
    //
    // Every read takes either one tenant or a SET of tenants. The set
    // variants (`*_scoped`) return the union of per-tenant results, which
    // is safe because every key (`entity_key`, `emmo_edge.id`,
    // `assertion_id`) is tenant-qualified: the union is a union of
    // DISJOINT subgraphs and cannot blend identities. Each returned row
    // names its owner in `tenant`. An empty tenant set reads NOTHING —
    // it is never shorthand for "all tenants".
    //
    // `limit` is one budget for the whole union, not per tenant — a wider
    // scope behaves exactly like a bigger single store, so under a small
    // limit one tenant's strong matches can crowd out another's. That is
    // the deliberate choice; callers needing a guaranteed floor per
    // tenant issue per-tenant reads.
    // ─────────────────────────────────────────────────────────────────────

    /// Substring search over entity names (shortest names first, like the
    /// cloud's CONTAINS fallback).
    pub async fn graph_search(
        &self,
        term: &str,
        tenant: &str,
        limit: i64,
    ) -> Result<Vec<GraphNode>> {
        self.graph_search_scoped(term, &[tenant], limit).await
    }

    /// [`Self::graph_search`] over a set of tenants.
    pub async fn graph_search_scoped(
        &self,
        term: &str,
        tenants: &[&str],
        limit: i64,
    ) -> Result<Vec<GraphNode>> {
        if tenants.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT name, entity_type, label, class_iri, tenant FROM emmo_entity \
             WHERE tenant IN ({}) AND name LIKE ?{} \
             ORDER BY LENGTH(name) LIMIT ?{}",
            tenant_placeholders(1, tenants.len()),
            tenants.len() + 1,
            tenants.len() + 2,
        );
        let mut params = tenant_params(tenants);
        params.push(Value::Text(format!("%{term}%")));
        params.push(Value::Integer(limit));
        let mut rows = self.conn.query(&sql, params).await?;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next().await? {
            nodes.push(row_to_node(&row, 0)?);
        }
        Ok(nodes)
    }

    /// The origin locator this store can honestly report for one entity:
    /// the lexicographically smallest `source` among stored assertions that
    /// mention the entity (as subject or object) under this tenant, or
    /// `None` when no assertion mentions it.
    ///
    /// An entity extracted from several sources has several true origins;
    /// one row of the peer-sync wire format carries exactly one, so the
    /// smallest is chosen for determinism — it is always a REAL recorded
    /// locator, never synthesized. `prov_assertion.source` is the immutable
    /// FIRST attribution of each assertion, so a later corroborating source
    /// cannot displace an assertion's original attribution here.
    pub async fn entity_origin(&self, name: &str, tenant: &str) -> Result<Option<String>> {
        let mut rows = self
            .conn
            .query(
                r#"SELECT source FROM prov_assertion
                   WHERE tenant = ?1 AND (subject = ?2 OR object = ?3)
                   ORDER BY source LIMIT 1"#,
                [
                    Value::Text(tenant.to_string()),
                    Value::Text(name.to_string()),
                    Value::Text(name.to_string()),
                ],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(get_str(&row, 0)?)),
            None => Ok(None),
        }
    }

    /// Edges incident to the named entity (resolved via its canonical key or
    /// exact display name) plus the adjacent nodes, optionally filtered by
    /// relationship type. Keys are label-qualified, so one name may resolve
    /// to several centers (e.g. the same name as Matter and as Phase) —
    /// edges of all of them are returned.
    pub async fn get_neighbors(
        &self,
        name: &str,
        rel_type: Option<&str>,
        tenant: &str,
        limit: i64,
    ) -> Result<TraversalResult> {
        self.get_neighbors_scoped(name, rel_type, &[tenant], limit)
            .await
    }

    /// [`Self::get_neighbors`] over a set of tenants. Center resolution,
    /// node dedupe, and edge dedupe are all tenant-qualified, so the same
    /// name owned by two tenants stays two attributed rows — neither
    /// shadows the other.
    pub async fn get_neighbors_scoped(
        &self,
        name: &str,
        rel_type: Option<&str>,
        tenants: &[&str],
        limit: i64,
    ) -> Result<TraversalResult> {
        if tenants.is_empty() {
            return Ok(TraversalResult {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }
        // Resolve name → center keys/nodes: exact display name first
        // (indexed), else compare the canonical part of each key in Rust
        // (canonical_key is not expressible in SQL).
        let mut centers: Vec<(String, GraphNode)> = Vec::new();
        {
            let sql = format!(
                "SELECT key, name, entity_type, label, class_iri, tenant FROM emmo_entity \
                 WHERE tenant IN ({}) AND name = ?{}",
                tenant_placeholders(1, tenants.len()),
                tenants.len() + 1,
            );
            let mut params = tenant_params(tenants);
            params.push(Value::Text(name.to_string()));
            let mut rows = self.conn.query(&sql, params).await?;
            while let Some(row) = rows.next().await? {
                centers.push((get_str(&row, 0)?, row_to_node(&row, 1)?));
            }
        }
        if centers.is_empty() {
            let canon = canonical_key(name);
            let sql = format!(
                "SELECT key, name, entity_type, label, class_iri, tenant FROM emmo_entity \
                 WHERE tenant IN ({})",
                tenant_placeholders(1, tenants.len()),
            );
            let mut rows = self.conn.query(&sql, tenant_params(tenants)).await?;
            while let Some(row) = rows.next().await? {
                let key = get_str(&row, 0)?;
                // "{tenant}|{label}:{canonical}". The tenant is stripped on
                // its `|` BEFORE the label is stripped on `:` — a per-peer
                // tenant is `mesh:{node_id}`, so splitting the whole key on
                // the first `:` would cut inside the tenant and silently
                // fail to resolve every peer entity on this fallback path.
                // A legacy pre-qualification key (no `|`; the canonical name
                // itself or "{label}:{canonical}") still resolves: both
                // strips fall through to the remainder.
                let after_tenant = key.split_once('|').map_or(key.as_str(), |(_, rest)| rest);
                let key_canon = after_tenant
                    .split_once(':')
                    .map_or(after_tenant, |(_, c)| c);
                if key_canon == canon {
                    let node = row_to_node(&row, 1)?;
                    centers.push((key, node));
                }
            }
        }
        if centers.is_empty() {
            return Ok(TraversalResult {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        // Dedupe keys include the tenant: under a union read, a peer node
        // sharing (label, name) with a local one is a DIFFERENT node and
        // must not be swallowed by the dedupe.
        let node_key = |node: &GraphNode| format!("{}|{}:{}", node.tenant, node.label, node.name);
        let mut nodes: Vec<GraphNode> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_, node) in &centers {
            if seen.insert(node_key(node)) {
                nodes.push(node.clone());
            }
        }

        const EDGE_COLS: &str = "e.rel_type, \
             s.name, s.entity_type, s.label, s.class_iri, s.tenant, \
             t.name, t.entity_type, t.label, t.class_iri, t.tenant, \
             e.tenant";
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut seen_edges: std::collections::HashSet<(String, String, String, String)> =
            std::collections::HashSet::new();
        // One edge query per center, each cursor fully drained before the
        // next statement (turso pre-release is sensitive to interleaved
        // open statements). Center keys are tenant-qualified, so each
        // query can only see its own tenant's edges; the e.tenant filter
        // stays for legacy pre-qualification rows and the index.
        for (center_key, _) in &centers {
            let n = tenants.len();
            let mut rows = match rel_type {
                Some(rt) => {
                    let sql = format!(
                        "SELECT {EDGE_COLS} FROM emmo_edge e \
                         JOIN emmo_entity s ON s.key = e.source_key \
                         JOIN emmo_entity t ON t.key = e.target_key \
                         WHERE e.tenant IN ({}) \
                           AND (e.source_key = ?{} OR e.target_key = ?{}) \
                           AND e.rel_type = ?{} LIMIT ?{}",
                        tenant_placeholders(1, n),
                        n + 1,
                        n + 2,
                        n + 3,
                        n + 4,
                    );
                    let mut params = tenant_params(tenants);
                    params.push(Value::Text(center_key.clone()));
                    params.push(Value::Text(center_key.clone()));
                    params.push(Value::Text(rt.to_string()));
                    params.push(Value::Integer(limit));
                    self.conn.query(&sql, params).await?
                }
                None => {
                    let sql = format!(
                        "SELECT {EDGE_COLS} FROM emmo_edge e \
                         JOIN emmo_entity s ON s.key = e.source_key \
                         JOIN emmo_entity t ON t.key = e.target_key \
                         WHERE e.tenant IN ({}) \
                           AND (e.source_key = ?{} OR e.target_key = ?{}) \
                         LIMIT ?{}",
                        tenant_placeholders(1, n),
                        n + 1,
                        n + 2,
                        n + 3,
                    );
                    let mut params = tenant_params(tenants);
                    params.push(Value::Text(center_key.clone()));
                    params.push(Value::Text(center_key.clone()));
                    params.push(Value::Integer(limit));
                    self.conn.query(&sql, params).await?
                }
            };
            while let Some(row) = rows.next().await? {
                let source = row_to_node(&row, 1)?;
                let target = row_to_node(&row, 6)?;
                let rel = get_str(&row, 0)?;
                let edge_tenant = get_str(&row, 11)?;
                // An edge between two centers shows up in both queries.
                // The tenant is part of the key: the same (source, rel,
                // target) names under two tenants are two edges.
                if !seen_edges.insert((
                    edge_tenant.clone(),
                    source.name.clone(),
                    target.name.clone(),
                    rel.clone(),
                )) {
                    continue;
                }
                edges.push(GraphEdge {
                    source: source.name.clone(),
                    target: target.name.clone(),
                    rel_type: rel,
                    count: 1,
                    tenant: edge_tenant,
                });
                for node in [source, target] {
                    if seen.insert(node_key(&node)) {
                        nodes.push(node);
                    }
                }
            }
        }
        Ok(TraversalResult { nodes, edges })
    }

    /// Legacy cloud-shaped recall. New scientific consumers should use
    /// [`Self::recall_with_context`], which also returns value, unit,
    /// conditions, and evidence class.
    pub async fn recall(&self, query: &str, tenant: &str, limit: i64) -> Result<Vec<RecalledFact>> {
        Ok(self
            .recall_with_context(query, tenant, limit)
            .await?
            .into_iter()
            .map(|fact| RecalledFact {
                subject: fact.subject,
                predicate: fact.predicate,
                object: fact.object,
                confidence: fact.confidence,
                source: fact.source,
                agent: fact.agent,
                tenant: fact.tenant,
            })
            .collect())
    }

    /// Recall complete assertions, highest-confidence first. Every returned
    /// row includes the additive condition and evidence fields; legacy rows
    /// read as empty conditions with RED/indeterminate evidence.
    pub async fn recall_with_context(
        &self,
        query: &str,
        tenant: &str,
        limit: i64,
    ) -> Result<Vec<RecalledMaterialFact>> {
        self.recall_with_context_scoped(query, &[tenant], limit)
            .await
    }

    /// [`Self::recall_with_context`] over a set of tenants, each fact
    /// attributed to its owner via `tenant`.
    pub async fn recall_with_context_scoped(
        &self,
        query: &str,
        tenants: &[&str],
        limit: i64,
    ) -> Result<Vec<RecalledMaterialFact>> {
        if tenants.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{query}%");
        let sql = format!(
            "SELECT subject, predicate, object, value, unit, conditions_json, \
                    evidence_class, confidence, source, agent, tenant \
             FROM prov_assertion \
             WHERE tenant IN ({}) AND (subject LIKE ?{} OR object LIKE ?{}) \
             ORDER BY confidence DESC LIMIT ?{}",
            tenant_placeholders(1, tenants.len()),
            tenants.len() + 1,
            tenants.len() + 2,
            tenants.len() + 3,
        );
        let mut params = tenant_params(tenants);
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
        params.push(Value::Integer(limit));
        let mut rows = self.conn.query(&sql, params).await?;
        let mut facts = Vec::new();
        while let Some(row) = rows.next().await? {
            let conditions_json = get_str(&row, 5)?;
            let conditions = serde_json::from_str(&conditions_json).map_err(|error| {
                anyhow::anyhow!("stored fact has invalid conditions_json: {error}")
            })?;
            facts.push(RecalledMaterialFact {
                subject: get_str(&row, 0)?,
                predicate: get_str(&row, 1)?,
                object: get_str(&row, 2)?,
                value: row
                    .get_value(3)
                    .ok()
                    .and_then(|value| value.as_real().copied()),
                unit: match row.get_value(4)? {
                    Value::Text(unit) if !unit.is_empty() => Some(unit),
                    _ => None,
                },
                conditions,
                evidence_class: EvidenceClass::from_stored(&get_str(&row, 6)?),
                confidence: row
                    .get_value(7)
                    .ok()
                    .and_then(|value| value.as_real().copied())
                    .unwrap_or(0.0),
                source: get_str(&row, 8)?,
                agent: get_str(&row, 9)?,
                tenant: get_str(&row, 10)?,
            });
        }
        Ok(facts)
    }

    /// Every distinct origin source contributing to one (unconditioned)
    /// assertion, ordered by source key.
    ///
    /// The parent row's `source`/`agent`/`activity_id` freeze the FIRST
    /// attribution; corroborating sources are visible only here.
    pub async fn assertion_evidence(
        &self,
        tenant: &str,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<Vec<EvidenceContribution>> {
        self.assertion_evidence_by_id(&assertion_id(tenant, subject, predicate, object))
            .await
    }

    /// [`Self::assertion_evidence`] by assertion id, for CONDITIONED
    /// assertions (value/unit/conditions are part of their identity —
    /// compute the id with [`conditioned_assertion_id`]). Before this
    /// existed, a valued fact's evidence contributions were unreadable
    /// through the public API.
    pub async fn assertion_evidence_by_id(&self, id: &str) -> Result<Vec<EvidenceContribution>> {
        let id = id.to_string();
        let mut rows = self
            .conn
            .query(
                "SELECT source_key, source_entity_id, activity_id, agent_id, \
                        confidence, evidence_class, confidence_kind, legacy_corroborations \
                 FROM prov_assertion_evidence WHERE assertion_id = ?1 \
                 ORDER BY source_key",
                [Value::Text(id)],
            )
            .await?;
        let mut contributions = Vec::new();
        while let Some(row) = rows.next().await? {
            contributions.push(EvidenceContribution {
                source_key: get_str(&row, 0)?,
                source_entity_id: get_str(&row, 1)?,
                activity_id: get_str(&row, 2)?,
                agent_id: get_str(&row, 3)?,
                confidence: row
                    .get_value(4)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or(0.0),
                evidence_class: EvidenceClass::from_stored(&get_str(&row, 5)?),
                confidence_kind: get_str(&row, 6)?,
                legacy_corroborations: row.get_value(7).ok().and_then(|v| v.as_integer().copied()),
            });
        }
        Ok(contributions)
    }

    /// Return every ontology artifact that classified the stable assertion
    /// identified by `assertion_id`, ordered deterministically.
    ///
    /// The result can contain several versions and activities for one
    /// assertion. An empty result honestly means that the assertion predates
    /// classification provenance or was written through an unclassified API.
    ///
    /// @req REQ-OWL-1.5 - Report ontology identity for a classified fact.
    pub async fn assertion_classifications(
        &self,
        assertion_id: &str,
    ) -> Result<Vec<AssertionClassification>> {
        let mut rows = self
            .conn
            .query(
                "SELECT activity_id, ontology_version_iri, artifact_sha256 \
                 FROM prov_assertion_classification WHERE assertion_id = ?1 \
                 ORDER BY ontology_version_iri, artifact_sha256, activity_id",
                [Value::Text(assertion_id.to_string())],
            )
            .await?;
        let mut classifications = Vec::new();
        while let Some(row) = rows.next().await? {
            classifications.push(AssertionClassification {
                activity_id: get_str(&row, 0)?,
                version_iri: get_str(&row, 1)?,
                artifact_sha256: get_str(&row, 2)?,
            });
        }
        Ok(classifications)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Entity vectors — local semantic search without Qdrant
    // ─────────────────────────────────────────────────────────────────────

    /// UPSERT one vector for a label-qualified entity key (little-endian
    /// f32 blob, same encoding as `provenance_embeddings`).
    pub async fn store_entity_embedding(
        &self,
        key: &str,
        tenant: &str,
        vector: &[f32],
    ) -> Result<()> {
        // Under the shared write lock so this single-statement write cannot
        // join (and be rolled back with) a raw transaction some other task
        // has open on the one shared connection.
        let _same_handle_guard = self.write_lock.lock().await;
        self.store_entity_embedding_locked(key, tenant, vector)
            .await
    }

    /// The vector UPSERT itself. Caller must hold `write_lock`.
    async fn store_entity_embedding_locked(
        &self,
        key: &str,
        tenant: &str,
        vector: &[f32],
    ) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT OR REPLACE INTO emmo_embedding
                   (key, tenant, dim, vector) VALUES (?1, ?2, ?3, ?4)"#,
                [
                    Value::Text(key.to_string()),
                    Value::Text(tenant.to_string()),
                    Value::Integer(vector.len() as i64),
                    Value::Blob(prism_embed::vec_to_le_bytes(vector)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Embed the distinct subject/object names of `facts` with `backend`
    /// and store one vector per matching `emmo_entity` row. Names are
    /// resolved to their label-qualified keys via the entity table itself
    /// (no duplicate of `write_fact`'s kind→label routing), so names that
    /// never landed there (e.g. value-less measurements dropped by
    /// `write_fact`) are skipped. Returns the number of vectors stored.
    ///
    /// Like `embed_and_store`, deliberately NOT part of `write_fact`:
    /// graph writes must never wait on (or fail because of) an embedding
    /// model. Callers run this after the fact writes succeed.
    pub async fn embed_and_store_entities<F: FactPayload>(
        &self,
        facts: &[F],
        tenant: &str,
        backend: &dyn prism_embed::EmbedBackend,
    ) -> Result<usize> {
        self.embed_and_store_names(&distinct_fact_names(facts), tenant, backend)
            .await
    }

    /// [`Self::embed_and_store_entities`] by entity display NAME rather than
    /// by fact — for nodes that were written without any fact (standalone
    /// extracted entities, referential containment). Names are deduped on
    /// `canonical_key` (first-seen order) and resolved to their
    /// label-qualified keys via the entity table itself, so names that never
    /// landed there are skipped. Returns the number of vectors stored.
    pub async fn embed_and_store_names(
        &self,
        names: &[String],
        tenant: &str,
        backend: &dyn prism_embed::EmbedBackend,
    ) -> Result<usize> {
        let mut seen = std::collections::HashSet::new();
        let names: Vec<String> = names
            .iter()
            .filter(|n| seen.insert(canonical_key(n)))
            .cloned()
            .collect();
        if names.is_empty() {
            return Ok(0);
        }
        let vectors = backend.embed(&names).await?;

        // Locked AFTER the embedding call (a model pass must never hold the
        // store's write lock) and across the whole key-resolve/UPSERT loop,
        // so no vector write can join another task's open raw transaction on
        // the shared connection.
        let _same_handle_guard = self.write_lock.lock().await;
        let mut stored = 0usize;
        for (name, vector) in names.iter().zip(&vectors) {
            // The read cursor is fully drained BEFORE the writes below
            // (turso pre-release mishandles interleaved open statements —
            // see `record_assertion`).
            let keys = {
                let mut rows = self
                    .conn
                    .query(
                        "SELECT key FROM emmo_entity WHERE tenant = ?1 AND name = ?2",
                        [Value::Text(tenant.to_string()), Value::Text(name.clone())],
                    )
                    .await?;
                let mut keys = Vec::new();
                while let Some(row) = rows.next().await? {
                    keys.push(get_str(&row, 0)?);
                }
                keys
            };
            for key in keys {
                self.store_entity_embedding_locked(&key, tenant, vector)
                    .await?;
                stored += 1;
            }
        }
        Ok(stored)
    }

    /// Best-effort entity embedding for freshly written facts: builds the
    /// configured `prism-embed` backend (on the blocking pool — the first
    /// ever native init downloads the model) and stores one vector per
    /// entity. Failures are logged and swallowed — an ingest must never
    /// fail because of the embedding model.
    pub async fn embed_entities_best_effort<F: FactPayload>(&self, facts: &[F], tenant: &str) {
        self.embed_names_best_effort(&distinct_fact_names(facts), tenant)
            .await
    }

    /// [`Self::embed_entities_best_effort`] by entity display NAME — the
    /// variant for writes that include fact-less standalone entities.
    pub async fn embed_names_best_effort(&self, names: &[String], tenant: &str) {
        if names.is_empty() {
            return;
        }
        let backend = match tokio::task::spawn_blocking(prism_embed::from_config).await {
            Ok(Some(backend)) => backend,
            Ok(None) => {
                tracing::debug!("embedding backend unavailable — entity vectors skipped");
                return;
            }
            Err(e) => {
                tracing::warn!("embedding backend init failed: {e} — entity vectors skipped");
                return;
            }
        };
        match self
            .embed_and_store_names(names, tenant, backend.as_ref())
            .await
        {
            Ok(stored) => tracing::debug!(stored, tenant, "entity vectors stored in Turso"),
            Err(e) => tracing::warn!("entity embedding failed: {e:#} — graph write unaffected"),
        }
    }

    /// Number of stored entity vectors for `tenant` — cheap existence
    /// check so query paths can skip embedding-model init (and fall back
    /// to other stores) when there is nothing to search.
    pub async fn entity_embedding_count(&self, tenant: &str) -> Result<i64> {
        self.entity_embedding_count_scoped(&[tenant]).await
    }

    /// [`Self::entity_embedding_count`] over a set of tenants.
    pub async fn entity_embedding_count_scoped(&self, tenants: &[&str]) -> Result<i64> {
        if tenants.is_empty() {
            return Ok(0);
        }
        let sql = format!(
            "SELECT COUNT(*) FROM emmo_embedding WHERE tenant IN ({})",
            tenant_placeholders(1, tenants.len()),
        );
        let mut rows = self.conn.query(&sql, tenant_params(tenants)).await?;
        Ok(match rows.next().await? {
            Some(row) => row
                .get_value(0)
                .ok()
                .and_then(|v| v.as_integer().copied())
                .unwrap_or(0),
            None => 0,
        })
    }

    /// Distinct stored `(tenant, vector width in bytes)` pairs across
    /// `tenants`, read from the blobs themselves rather than the `dim`
    /// column, so a NULL or stale `dim` cannot misreport what the index
    /// actually holds. Per-tenant so a mismatch error can name WHICH
    /// tenant holds the offending vectors.
    async fn entity_vector_widths(&self, tenants: &[&str]) -> Result<Vec<(String, usize)>> {
        if tenants.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT DISTINCT tenant, LENGTH(vector) FROM emmo_embedding WHERE tenant IN ({})",
            tenant_placeholders(1, tenants.len()),
        );
        let mut rows = self.conn.query(&sql, tenant_params(tenants)).await?;
        let mut widths = Vec::new();
        while let Some(row) = rows.next().await? {
            let tenant = get_str(&row, 0)?;
            if let Some(bytes) = row.get_value(1)?.as_integer().copied() {
                widths.push((tenant, bytes.max(0) as usize));
            }
        }
        Ok(widths)
    }

    /// Semantic entity search ranked by Turso's **native** vector support:
    /// `vector_distance_cos()` scores the stored f32 blobs inside the
    /// database, and `GROUP BY` collapses the same display name under two
    /// labels to its best-scoring row. Returns up to `limit` distinct
    /// `(display name, similarity)` pairs, best first, similarities in
    /// `[-1, 1]`.
    ///
    /// # Honesty contract
    ///
    /// `Ok(vec![])` means exactly one thing: **nothing is embedded for this
    /// tenant**. It never means "the index is broken". Every unusable-index
    /// condition is an `Err` naming the problem — above all a dimension
    /// mismatch, which used to be skipped row-by-row and so was
    /// indistinguishable from "no matches".
    pub async fn semantic_search_entities(
        &self,
        query_vec: &[f32],
        tenant: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        Ok(self
            .semantic_search_entities_scoped(query_vec, &[tenant], limit)
            .await?
            .into_iter()
            .map(|hit| (hit.name, hit.similarity))
            .collect())
    }

    /// [`Self::semantic_search_entities`] over a set of tenants.
    ///
    /// Grouped by `(tenant, name)`, NOT by name alone: under a union read
    /// a peer's entity carrying the same display name as a local one is a
    /// different entity, and a name-only GROUP BY would let one silently
    /// shadow the other. Same-name-same-tenant under two labels still
    /// collapses to its best-scoring row, exactly as before.
    ///
    /// The honesty contract of the single-tenant form holds: `Ok(vec![])`
    /// means nothing is embedded for ANY of these tenants; a dimension
    /// mismatch anywhere in the scope is a loud `Err` naming the tenants,
    /// never a silently shrunken result.
    pub async fn semantic_search_entities_scoped(
        &self,
        query_vec: &[f32],
        tenants: &[&str],
        limit: usize,
    ) -> Result<Vec<SemanticEntityHit>> {
        let stored = self.entity_vector_widths(tenants).await?;
        if stored.is_empty() {
            return Ok(Vec::new()); // genuinely empty index — not a failure
        }
        // A mismatch silently matches nothing, so refuse loudly instead.
        // Checked up front so the message can name each tenant's
        // dimensionality (Turso's own error — "Vectors must have the same
        // dimensions" — names neither), and the whole UNION refuses:
        // quietly dropping the mismatched tenant would make its knowledge
        // invisible again, which is the exact failure this read scope
        // exists to end. The offender is named so it can be re-ingested
        // or scoped out.
        let want = query_vec.len() * 4;
        if stored.iter().any(|(_, w)| *w != want) {
            let mut per_tenant: Vec<String> = stored
                .iter()
                .map(|(tenant, w)| format!("'{tenant}' holds {}-dimension vectors", w / 4))
                .collect();
            per_tenant.sort_unstable();
            anyhow::bail!(
                "local semantic index is unusable: {} but the query embedding is \
                 {}-dimension. The embedding backend changed since the mismatched \
                 tenant's vectors were written — re-ingest that tenant with the \
                 current backend, point PRISM_EMBED_BACKEND back at the one that \
                 wrote them, or scope the query to the matching tenants.",
                per_tenant.join(", "),
                query_vec.len(),
            );
        }

        let n = tenants.len();
        let sql = format!(
            "SELECT n.name, n.tenant, MIN(vector_distance_cos(e.vector, ?{})) AS distance \
             FROM emmo_embedding e JOIN emmo_entity n ON n.key = e.key \
             WHERE e.tenant IN ({}) \
             GROUP BY n.tenant, n.name ORDER BY distance ASC LIMIT ?{}",
            n + 1,
            tenant_placeholders(1, n),
            n + 2,
        );
        let mut params = tenant_params(tenants);
        params.push(Value::Blob(prism_embed::vec_to_le_bytes(query_vec)));
        params.push(Value::Integer(limit.max(1) as i64));
        let mut rows = self.conn.query(&sql, params).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let name = get_str(&row, 0)?;
            let tenant = get_str(&row, 1)?;
            // `vector_distance_cos` is `1 - cosine_similarity`, in [0, 2].
            let distance = match row.get_value(2)? {
                Value::Real(d) => d,
                Value::Integer(d) => d as f64,
                other => anyhow::bail!("vector_distance_cos returned {other:?}, expected a number"),
            };
            out.push(SemanticEntityHit {
                name,
                tenant,
                similarity: 1.0 - distance as f32,
            });
        }
        Ok(out)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Tenant discovery + peer-echo detection
    // ─────────────────────────────────────────────────────────────────────

    /// The tenants a DEFAULT read spans: [`LOCAL_TENANT`], plus every
    /// ontology-composed local tenant (`local@{ontology id}`, e.g. the
    /// MatKG reference graph under `local@matkg` — see the ingest crate's
    /// `storage_tenant`), plus every mesh tenant actually present in the
    /// store. All non-local tenants are DISCOVERED, not hardcoded, so both
    /// the legacy shared `"mesh"` tenant and per-peer `"mesh:{node_id}"`
    /// tenants are found regardless of which shape the sync side currently
    /// writes, and a reference ontology loaded yesterday is visible today
    /// without a flag. Deterministic order: local first, then the
    /// discovered tenants sorted (`local@…` sorts before `mesh…`).
    ///
    /// Every returned row of every scoped read names its owning tenant, so
    /// widening the default scope never BLENDS subgraphs — tenant-qualified
    /// keys keep them disjoint; this only makes them visible, labelled.
    pub async fn default_read_tenants(&self) -> Result<Vec<String>> {
        let mut discovered: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        // Entities and assertions can each exist without the other
        // (`record_assertion` alone writes no entity), so both tables are
        // consulted. Each cursor is fully drained before the next query
        // (turso pre-release is sensitive to interleaved open statements).
        for table in ["emmo_entity", "prov_assertion"] {
            let mut rows = self
                .conn
                .query(
                    &format!(
                        "SELECT DISTINCT tenant FROM {table} \
                         WHERE tenant = 'mesh' OR tenant LIKE 'mesh:%' \
                            OR tenant LIKE 'local@%'"
                    ),
                    (),
                )
                .await?;
            while let Some(row) = rows.next().await? {
                discovered.insert(get_str(&row, 0)?);
            }
        }
        let mut tenants = Vec::with_capacity(1 + discovered.len());
        tenants.push(LOCAL_TENANT.to_string());
        tenants.extend(discovered);
        Ok(tenants)
    }

    /// Which MESH tenants already assert this exact triple (canonical
    /// identity, via [`assertion_id`]).
    ///
    /// This is the read-side tripwire for the laundering loop: an agent
    /// that READS a peer fact and WRITES it back under `"local"` creates
    /// a fresh local-tenant assertion that the tenant-qualified keys
    /// cannot stop, because at the store boundary that write is
    /// indistinguishable from honest independent corroboration (the same
    /// fact extracted from a genuinely independent source). Writers of
    /// agent- or user-supplied facts call this and surface a LOUD warning
    /// when the triple already arrived over the mesh, instead of letting
    /// peer knowledge be silently absorbed as local.
    pub async fn peer_tenants_asserting(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<Vec<String>> {
        let tenants = self.default_read_tenants().await?;
        self.peer_tenants_asserting_among(&tenants, subject, predicate, object)
            .await
    }

    /// [`Self::peer_tenants_asserting`] against an already-discovered
    /// tenant list, so a caller checking MANY triples (an ingest run)
    /// discovers the mesh tenants once instead of twice per fact.
    /// [`LOCAL_TENANT`] entries are skipped — holding the triple locally
    /// is not an echo — and so is every non-mesh tenant: this tripwire is
    /// about PEER laundering, and a reference ontology tenant such as
    /// `local@matkg` (now in the default read scope) holding the same
    /// triple is reference data the user chose to load, not a peer echo.
    pub async fn peer_tenants_asserting_among(
        &self,
        tenants: &[String],
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<Vec<String>> {
        let mut holders = Vec::new();
        for tenant in tenants {
            if !is_mesh_tenant(tenant) {
                continue;
            }
            let id = assertion_id(tenant, subject, predicate, object);
            let mut rows = self
                .conn
                .query(
                    "SELECT 1 FROM prov_assertion WHERE id = ?1",
                    [Value::Text(id)],
                )
                .await?;
            let held = rows.next().await?.is_some();
            while rows.next().await?.is_some() {}
            if held {
                holders.push(tenant.clone());
            }
        }
        Ok(holders)
    }
}

/// Tenant every local single-user write uses, and the tenant a default
/// read scope always includes (see
/// [`ProvenanceStore::default_read_tenants`]).
pub const LOCAL_TENANT: &str = "local";

/// `?start, ?start+1, …` — one numbered placeholder per tenant, for
/// `tenant IN (…)` filters over a caller-chosen tenant set.
fn tenant_placeholders(start: usize, count: usize) -> String {
    (0..count)
        .map(|i| format!("?{}", start + i))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The tenant set as leading positional SQL parameters.
fn tenant_params(tenants: &[&str]) -> Vec<Value> {
    tenants
        .iter()
        .map(|tenant| Value::Text((*tenant).to_string()))
        .collect()
}

/// Read a `GraphNode` from five consecutive columns starting at `offset`
/// (name, entity_type, label, class_iri, tenant).
fn row_to_node(row: &turso::Row, offset: usize) -> Result<GraphNode> {
    Ok(GraphNode {
        name: get_str(row, offset)?,
        entity_type: get_str(row, offset + 1)?,
        label: get_str(row, offset + 2)?,
        class_iri: get_opt_str(row, offset + 3)?,
        tenant: get_str(row, offset + 4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Tempfile-backed Turso DB, removed (with SQLite journal sidecars) on drop.
    struct TempDb {
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("prism_emmo_test_{}.db", uuid::Uuid::new_v4()));
            Self { path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut p = self.path.clone().into_os_string();
                p.push(suffix);
                let _ = std::fs::remove_file(p);
            }
        }
    }

    fn test_prov() -> LocalProvenance {
        LocalProvenance {
            activity_id: "act_test_1".into(),
            agent_id: "gemma-4-12b".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "doc:test_paper".into(),
            source_kind: "Document".into(),
            tenant: "t1".into(),
            started_at: "2026-07-13T00:00:00Z".into(),
            ended_at: "2026-07-13T00:00:01Z".into(),
            locality: "local".into(),
            origin_source_id: None,
        }
    }

    /// [`test_prov`] with its own source and activity — corroboration is
    /// keyed on the ORIGIN SOURCE, so tests vary source and run separately.
    fn prov_from(source: &str, activity: &str) -> LocalProvenance {
        LocalProvenance {
            activity_id: activity.into(),
            source_entity_id: source.into(),
            ..test_prov()
        }
    }

    fn fact(kind: &str, subject: &str, predicate: &str, object: &str) -> LocalFact {
        LocalFact {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
            value: None,
            unit: None,
            confidence: Some(0.8),
            kind: Some(kind.into()),
        }
    }

    async fn count(store: &ProvenanceStore, sql: &str) -> i64 {
        let mut rows = store.conn.query(sql, ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get_value(0)
            .ok()
            .and_then(|v| v.as_integer().copied())
            .unwrap_or(-1)
    }

    async fn query_str(store: &ProvenanceStore, sql: &str) -> String {
        let mut rows = store.conn.query(sql, ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        get_str(&row, 0).unwrap()
    }

    async fn query_f64(store: &ProvenanceStore, sql: &str) -> f64 {
        let mut rows = store.conn.query(sql, ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        row.get_value(0)
            .ok()
            .and_then(|v| v.as_real().copied())
            .unwrap_or(f64::NAN)
    }

    /// The reproducibility record lands on the SAME activity row
    /// `record_activity` wrote — seed, temperature and decoding mode read
    /// back exactly, NULLs stay NULL (a backend with no knobs must never
    /// gain invented defaults), and recording against an activity that was
    /// never recorded is a loud error, not a silent no-op.
    #[tokio::test]
    async fn activity_decoding_round_trips_on_the_activity_row() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        store.record_activity(&prov).await.unwrap();

        store
            .record_activity_decoding(
                &prov.activity_id,
                &ActivityDecoding {
                    seed: Some(42),
                    temperature: Some(0.0),
                    mode: Some("json_schema"),
                },
            )
            .await
            .unwrap();

        let seed = count(
            &store,
            "SELECT seed FROM prov_activity WHERE id = 'act_test_1'",
        )
        .await;
        assert_eq!(seed, 42);
        let temperature = query_f64(
            &store,
            "SELECT temperature FROM prov_activity WHERE id = 'act_test_1'",
        )
        .await;
        assert!(temperature.abs() < f64::EPSILON, "{temperature}");
        let mode = query_str(
            &store,
            "SELECT decoding FROM prov_activity WHERE id = 'act_test_1'",
        )
        .await;
        assert_eq!(mode, "json_schema");
        // The model id it is attributable alongside was already there.
        let agent = query_str(
            &store,
            "SELECT agent_id FROM prov_activity WHERE id = 'act_test_1'",
        )
        .await;
        assert_eq!(agent, "gemma-4-12b");

        // Honest NULLs for a knob-less backend.
        let bare = prov_from("doc:other", "act_test_2");
        store.record_activity(&bare).await.unwrap();
        store
            .record_activity_decoding(
                &bare.activity_id,
                &ActivityDecoding {
                    seed: None,
                    temperature: None,
                    mode: Some("prompt_only"),
                },
            )
            .await
            .unwrap();
        let nulls = count(
            &store,
            "SELECT COUNT(*) FROM prov_activity \
             WHERE id = 'act_test_2' AND seed IS NULL AND temperature IS NULL \
             AND decoding = 'prompt_only'",
        )
        .await;
        assert_eq!(nulls, 1);

        // A phantom activity id is refused loudly.
        let err = store
            .record_activity_decoding(
                "act_never_recorded",
                &ActivityDecoding {
                    seed: Some(1),
                    temperature: Some(0.0),
                    mode: Some("json_schema"),
                },
            )
            .await
            .expect_err("recording decoding on a never-recorded activity must fail");
        assert!(
            format!("{err:#}").contains("record_activity must run first"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn write_fact_each_kind_is_searchable_and_traversable() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let cases = [
            (
                "measurement",
                "Ti-6Al-4V",
                "has_measurement",
                "UTS",
                "HAS_MEASUREMENT",
            ),
            ("phase", "Ti-6Al-4V", "has_phase", "alpha-beta", "HAS_PHASE"),
            (
                "composition",
                "Inconel 718",
                "has_composition",
                "NiCr19Fe18",
                "HAS_COMPOSITION",
            ),
            (
                "contains",
                "Inconel 718",
                "contains",
                "Ni",
                "CONTAINS_ELEMENT",
            ),
            (
                "processing",
                "Inconel 718",
                "processed_by",
                "LPBF",
                "PROCESSED_BY",
            ),
            (
                "structure",
                "Ti-6Al-4V",
                "has_structure",
                "hexagonal",
                "HAS_STRUCTURE",
            ),
            (
                "application",
                "Ti-6Al-4V",
                "used_in",
                "turbine blades",
                "USED_IN",
            ),
        ];
        for (kind, s, p, o, rel) in cases {
            let mut f = fact(kind, s, p, o);
            if kind == "measurement" {
                f.value = Some(1140.0);
                f.unit = Some("MPa".into());
            }
            store.write_fact(&f, &prov).await.unwrap();

            let hits = store.graph_search(s, "t1", 10).await.unwrap();
            assert!(
                hits.iter().any(|n| n.name == s),
                "graph_search missed subject for {kind}"
            );

            let tr = store.get_neighbors(s, Some(rel), "t1", 10).await.unwrap();
            assert!(
                tr.edges.iter().any(|e| e.rel_type == rel),
                "get_neighbors missed {rel} edge for {kind}"
            );
            assert!(tr.nodes.len() >= 2, "expected center + neighbor for {kind}");
        }

        // Unknown kind is kept as a generic predicate edge, not dropped.
        let f = LocalFact {
            subject: "X material".into(),
            predicate: "related_to".into(),
            object: "Y material".into(),
            value: None,
            unit: None,
            confidence: None,
            kind: None,
        };
        store.write_fact(&f, &prov).await.unwrap();
        let tr = store
            .get_neighbors("X material", None, "t1", 10)
            .await
            .unwrap();
        assert!(tr.edges.iter().any(|e| e.rel_type == "related_to"));

        // Tenant scoping: nothing leaks into another tenant.
        assert!(
            store
                .graph_search("Ti", "other", 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_neighbors("Ti-6Al-4V", None, "other", 10)
                .await
                .unwrap()
                .edges
                .is_empty()
        );
    }

    /// Defence in depth at the store boundary (F10): a measurement's number
    /// is meaningless without its unit, so the writer REFUSES — loudly,
    /// never with a silent `Ok` — a measurement whose unit is missing or
    /// resolves to no QUDT identifier, and canonicalises raw spellings that
    /// do resolve so the store holds ONE unit vocabulary. Before this,
    /// `unwrap_or_default()` stored an empty-string unit: a CSV `880` was
    /// indistinguishable from 880 MPa or 880 GPa. Every ingest path drops
    /// and reports these shapes upstream; this guard is for callers that
    /// bypass that contract.
    #[tokio::test]
    async fn measurement_units_are_resolved_or_refused_never_stored_unitless() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // No unit at all → refused loudly.
        let mut unitless = fact("measurement", "Ti-6Al-4V", "has_measurement", "UTS");
        unitless.value = Some(880.0);
        let err = store
            .write_fact(&unitless, &prov)
            .await
            .expect_err("a unit-less measurement must be refused, never stored");
        assert!(
            format!("{err:#}").contains("no unit"),
            "the refusal must name the cause: {err:#}"
        );

        // A unit that resolves to nothing → refused loudly, naming it.
        let mut gibberish = fact("measurement", "Ti-6Al-4V", "has_measurement", "hardness");
        gibberish.value = Some(349.0);
        gibberish.unit = Some("banana".into());
        let err = store
            .write_fact(&gibberish, &prov)
            .await
            .expect_err("an unresolvable unit must be refused, never stored raw");
        assert!(
            format!("{err:#}").contains("banana"),
            "the refusal must name the spelling: {err:#}"
        );

        // Neither refused fact left a trace in the graph.
        assert!(
            store
                .graph_search("Ti-6Al-4V", "t1", 10)
                .await
                .unwrap()
                .is_empty(),
            "a refused measurement must write nothing"
        );

        // A raw spelling that DOES resolve is canonicalised on the way in:
        // the stored unit is the QUDT identifier, not the paper spelling.
        let mut raw_spelling = fact("measurement", "Ti-6Al-4V", "has_measurement", "UTS");
        raw_spelling.value = Some(880.0);
        raw_spelling.unit = Some("MPa".into());
        store.write_fact(&raw_spelling, &prov).await.unwrap();
        let recalled = store.recall_with_context("UTS", "t1", 10).await.unwrap();
        assert_eq!(recalled.len(), 1, "{recalled:?}");
        assert_eq!(recalled[0].value, Some(880.0));
        assert_eq!(
            recalled[0].unit.as_deref(),
            Some("QUDT:MegaPA"),
            "the store must hold the canonical identifier, not the raw spelling"
        );
        // BOTH stored shapes agree: `recall_with_context` reads the
        // assertion row; the synthetic Measurement NODE keeps its own copy
        // in `props_json`, written separately — a regression could decouple
        // them (canonical assertion, raw node) and the recall check alone
        // would never see it.
        let props: serde_json::Value = serde_json::from_str(
            &query_str(
                &store,
                "SELECT props_json FROM emmo_entity WHERE label = 'Measurement'",
            )
            .await,
        )
        .unwrap();
        assert_eq!(
            props["unit"], "QUDT:MegaPA",
            "the Measurement node's props must carry the canonical unit too"
        );
    }

    /// Caller-supplied node labels govern EVERY fact arm, subject and
    /// object alike — the tabular ingest passes the ACTIVE ontology's
    /// declared storage labels here, so what lands in `emmo_entity.label`
    /// (and therefore in the entity KEY) is the ontology's declaration,
    /// never this store's legacy hardcoded vocabulary. The no-label path
    /// (`write_fact` — text extraction and older callers) keeps that legacy
    /// shape byte-for-byte, pinned by the second half.
    #[tokio::test]
    async fn caller_labels_govern_every_arm_and_the_legacy_path_is_unchanged() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // (kind, legacy object label) — every arm write_fact_as has,
        // including the generic fallback arm (kind: None).
        let arms: [(Option<&str>, &str); 8] = [
            (Some("measurement"), "Property"),
            (Some("phase"), "Phase"),
            (Some("composition"), "Composition"),
            (Some("contains"), "Element"),
            (Some("processing"), "Manufacturing"),
            (Some("structure"), "CrystalStructure"),
            (Some("application"), "Application"),
            (None, "Entity"),
        ];

        let label_of = |name: &str| {
            let sql =
                format!("SELECT label FROM emmo_entity WHERE name = '{name}' AND tenant = 't1'");
            let store = &store;
            async move { query_str(store, &sql).await }
        };

        for (i, (kind, legacy_object_label)) in arms.iter().enumerate() {
            // Labeled write: the caller's labels must land verbatim.
            let subj = format!("lab-subj-{i}");
            let obj = format!("lab-obj-{i}");
            let mut f = LocalFact {
                subject: subj.clone(),
                predicate: format!("pred-{i}"),
                object: obj.clone(),
                value: None,
                unit: None,
                confidence: Some(0.8),
                kind: kind.map(str::to_string),
            };
            if *kind == Some("measurement") {
                f.value = Some(42.0);
                // The store refuses unit-less measurements (defence in
                // depth for F10); the labeling under test is orthogonal.
                f.unit = Some("QUDT:MegaPA".into());
            }
            store
                .write_fact_with_evidence(
                    &f,
                    &prov,
                    EvidenceClass::Research,
                    FactNodeLabels {
                        subject: "Molecule",
                        object: "Reaction",
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                label_of(&subj).await,
                "Molecule",
                "arm {kind:?}: subject label must be the caller's, not hardcoded"
            );
            assert_eq!(
                label_of(&obj).await,
                "Reaction",
                "arm {kind:?}: object label must be the caller's, not hardcoded"
            );

            // Legacy write of the same shape: the store's EMMO labels, unchanged.
            let subj = format!("leg-subj-{i}");
            let obj = format!("leg-obj-{i}");
            let mut f = LocalFact {
                subject: subj.clone(),
                predicate: format!("legacy-pred-{i}"),
                object: obj.clone(),
                value: None,
                unit: None,
                confidence: Some(0.8),
                kind: kind.map(str::to_string),
            };
            if *kind == Some("measurement") {
                f.value = Some(42.0);
                // The store refuses unit-less measurements (defence in
                // depth for F10); the labeling under test is orthogonal.
                f.unit = Some("QUDT:MegaPA".into());
            }
            store.write_fact(&f, &prov).await.unwrap();
            assert_eq!(label_of(&subj).await, "Matter", "legacy arm {kind:?}");
            assert_eq!(
                label_of(&obj).await,
                *legacy_object_label,
                "legacy arm {kind:?}"
            );
        }

        // The synthetic Measurement node is the store's own fact shape: it
        // stays `Measurement` on the labeled path too (it is not an
        // extracted entity, so no ontology declares a type for it).
        let meas = count(
            &store,
            "SELECT COUNT(*) FROM emmo_entity WHERE label = 'Measurement' \
             AND tenant = 't1' AND name LIKE 'meas_lab-subj-%'",
        )
        .await;
        assert_eq!(
            meas, 1,
            "the labeled measurement arm must mint its Measurement node"
        );

        // An empty caller label is refused loudly — never written blank.
        let err = store
            .write_fact_with_evidence(
                &fact("phase", "S", "has_phase", "O"),
                &prov,
                EvidenceClass::Research,
                FactNodeLabels {
                    subject: "",
                    object: "Phase",
                },
            )
            .await
            .expect_err("an empty node label must be refused");
        assert!(format!("{err:#}").contains("empty node label"), "{err:#}");
    }

    /// Classified writes keep all three type concepts distinct and stamp the
    /// stable assertion without changing any existing identity formula.
    #[tokio::test]
    async fn classified_fact_persists_identity_and_multi_version_provenance_additively() {
        const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let classified = fact("phase", "classified-alloy", "has_phase", "alpha-phase");
        let nodes = ClassifiedFactNodes {
            subject: ClassifiedNode {
                entity_type: "Alloy",
                storage_label: "Matter",
                class_iri: "urn:test:class:alloy",
            },
            object: ClassifiedNode {
                entity_type: "Phase",
                storage_label: "Phase",
                class_iri: "urn:test:class:phase",
            },
        };

        store
            .write_classified_fact_with_evidence(
                &classified,
                &prov_from("doc:classified-a", "act-classified-a"),
                EvidenceClass::Research,
                nodes,
                OntologyClassification {
                    version_iri: "urn:test:ontology:v1",
                    artifact_sha256: SHA_A,
                },
            )
            .await
            .unwrap();

        let subject = store
            .graph_search("classified-alloy", "t1", 10)
            .await
            .unwrap()
            .into_iter()
            .find(|node| node.name == "classified-alloy")
            .expect("classified subject must be queryable");
        assert_eq!(subject.label, "Matter", "storage label is compatibility");
        assert_eq!(subject.entity_type, "Alloy", "declared type was destroyed");
        assert_eq!(subject.class_iri.as_deref(), Some("urn:test:class:alloy"));

        let subject_key = entity_key("t1", "Matter", "classified-alloy");
        let object_key = entity_key("t1", "Phase", "alpha-phase");
        assert_eq!(
            query_str(
                &store,
                "SELECT key FROM emmo_entity WHERE name = 'classified-alloy'"
            )
            .await,
            subject_key
        );
        assert_eq!(
            query_str(
                &store,
                "SELECT id FROM emmo_edge WHERE rel_type = 'HAS_PHASE'"
            )
            .await,
            format!("t1|{subject_key}|HAS_PHASE|{object_key}"),
            "class metadata must not enter the edge id"
        );

        let stable_assertion_id =
            assertion_id("t1", "classified-alloy", "has_phase", "alpha-phase");
        let first = store
            .assertion_classifications(&stable_assertion_id)
            .await
            .unwrap();
        assert_eq!(
            first,
            vec![AssertionClassification {
                activity_id: "act-classified-a".into(),
                version_iri: "urn:test:ontology:v1".into(),
                artifact_sha256: SHA_A.into(),
            }]
        );

        // Reclassifying the same assertion under another artifact adds an
        // audit row but does not mint another assertion, edge, or entity.
        store
            .write_classified_fact_with_evidence(
                &classified,
                &prov_from("doc:classified-b", "act-classified-b"),
                EvidenceClass::Research,
                nodes,
                OntologyClassification {
                    version_iri: "urn:test:ontology:v2",
                    artifact_sha256: SHA_B,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(count(&store, "SELECT COUNT(*) FROM emmo_edge").await, 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM emmo_entity").await, 2);
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_classification").await,
            2
        );
        let versions = store
            .assertion_classifications(&stable_assertion_id)
            .await
            .unwrap()
            .into_iter()
            .map(|classification| classification.version_iri)
            .collect::<Vec<_>>();
        assert_eq!(versions, ["urn:test:ontology:v1", "urn:test:ontology:v2"]);
    }

    /// Text and paper extraction can stamp classification provenance while
    /// keeping the store's established synthetic node shape unchanged.
    #[tokio::test]
    async fn legacy_shaped_fact_can_be_classification_stamped_without_fake_node_iris() {
        const SHA: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let legacy = fact("phase", "legacy-shaped", "has_phase", "beta-phase");
        store
            .write_fact_with_classification(
                &legacy,
                &test_prov(),
                OntologyClassification {
                    version_iri: "urn:test:ontology:text",
                    artifact_sha256: SHA,
                },
            )
            .await
            .unwrap();

        let subject = store
            .graph_search("legacy-shaped", "t1", 10)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("legacy-shaped subject must be queryable");
        assert_eq!(subject.label, "Matter");
        assert_eq!(subject.entity_type, "Matter");
        assert_eq!(subject.class_iri, None, "the store must not invent an IRI");
        let id = assertion_id("t1", "legacy-shaped", "has_phase", "beta-phase");
        assert_eq!(store.assertion_classifications(&id).await.unwrap().len(), 1);

        let err = store
            .write_fact_with_classification(
                &legacy,
                &test_prov(),
                OntologyClassification {
                    version_iri: "urn:test:ontology:text",
                    artifact_sha256: "NOT-A-SHA",
                },
            )
            .await
            .expect_err("an invalid artifact identity must be refused before writing");
        assert!(err.to_string().contains("64 lowercase hexadecimal"));
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
    }

    /// Opening a pre-OWL database adds only the nullable class identity;
    /// existing rows and keys remain unchanged and honestly unclassified.
    #[tokio::test]
    async fn legacy_emmo_entity_schema_gains_nullable_class_iri_without_rekeying() {
        let db = TempDb::new();
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"CREATE TABLE emmo_entity (
                    key TEXT PRIMARY KEY,
                    name TEXT,
                    label TEXT,
                    entity_type TEXT,
                    tenant TEXT,
                    props_json TEXT,
                    created_at TEXT
                )"#,
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO emmo_entity \
                 (key, name, label, entity_type, tenant, created_at) VALUES \
                 ('t1|Matter:legacy-alloy', 'Legacy Alloy', 'Matter', 'Matter', 't1', 'old')",
                (),
            )
            .await
            .unwrap();
            // The key is already v4-qualified; avoid asking unrelated old-key
            // migrations to reinterpret this focused schema fixture.
            conn.execute("PRAGMA user_version = 5", ()).await.unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let node = store
            .graph_search("Legacy Alloy", "t1", 10)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("legacy row must survive the additive migration");
        assert_eq!(node.label, "Matter");
        assert_eq!(node.entity_type, "Matter");
        assert_eq!(node.class_iri, None);
        assert_eq!(
            query_str(
                &store,
                "SELECT key FROM emmo_entity WHERE name = 'Legacy Alloy'"
            )
            .await,
            "t1|Matter:legacy-alloy"
        );
    }

    #[tokio::test]
    async fn same_name_under_two_labels_keeps_two_entities() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // "alpha" as a Phase (object) and as Matter (subject) — with
        // unqualified keys these collapsed into one label-churning row.
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
            .await
            .unwrap();
        store
            .write_fact(&fact("phase", "alpha", "has_phase", "beta"), &prov)
            .await
            .unwrap();

        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE name = 'alpha'"
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE key = 't1|Matter:alpha'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE key = 't1|Phase:alpha'"
            )
            .await,
            1
        );

        // Traversal from the shared name sees edges of BOTH labels.
        let tr = store.get_neighbors("alpha", None, "t1", 10).await.unwrap();
        assert_eq!(
            tr.edges.len(),
            2,
            "expected one edge per label: {:?}",
            tr.edges
        );
        assert!(
            tr.edges
                .iter()
                .any(|e| e.source == "Ti-6Al-4V" && e.target == "alpha")
        );
        assert!(
            tr.edges
                .iter()
                .any(|e| e.source == "alpha" && e.target == "beta")
        );
    }

    #[tokio::test]
    async fn contains_kind_writes_element_and_fraction_edge_props() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut f = fact("contains", "Nb25Mo25Ta25W25", "contains", "Nb");
        f.value = Some(0.25);
        store.write_fact(&f, &prov).await.unwrap();

        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE key = 't1|Element:nb'"
            )
            .await,
            1
        );
        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'CONTAINS_ELEMENT'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["fraction"].as_f64(), Some(0.25));

        // Re-upsert WITHOUT a fraction must keep the stored props (COALESCE).
        f.value = None;
        store.write_fact(&f, &prov).await.unwrap();
        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'CONTAINS_ELEMENT'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["fraction"].as_f64(), Some(0.25));
    }

    #[tokio::test]
    async fn processing_order_lands_in_edge_props() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut f = fact("processing", "Inconel 718", "processed_by", "annealing");
        f.value = Some(2.0);
        store.write_fact(&f, &prov).await.unwrap();

        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'PROCESSED_BY'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["order"].as_f64(), Some(2.0));
    }

    #[tokio::test]
    async fn write_fact_upserts_are_idempotent() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        let f = fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta");

        store.write_fact(&f, &prov).await.unwrap();
        store.write_fact(&f, &prov).await.unwrap();

        // Re-ingest never duplicates: 2 entities (Matter + Phase), 1 edge,
        // 1 assertion (same source — merged, not corroborated), 1 agent,
        // 1 activity.
        assert_eq!(count(&store, "SELECT COUNT(*) FROM emmo_entity").await, 2);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM emmo_edge").await, 1);
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(count(&store, "SELECT COUNT(*) FROM prov_agent").await, 1);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM prov_activity").await, 1);
    }

    /// Re-recording the same triple from the SAME origin source is one
    /// observation, not two.
    ///
    /// This test replaces `same_triple_twice_corroborates_one_assertion`,
    /// which pinned the DEFECT as intended behavior: it asserted that two
    /// records of one document yield `corroborations = 2` and noisy-OR
    /// confidence 0.96 — i.e. that re-ingesting a paper (or watch mode
    /// re-scanning it) manufactures agreement. The product's thesis is
    /// provenance-weighted convergence; a store that cannot tell twelve
    /// papers agreeing from one ingest repeated twelve times has no
    /// convergence signal at all. Confidence must stay at the single
    /// source's 0.8, and the first attribution must survive the re-record.
    #[tokio::test]
    async fn same_source_twice_does_not_corroborate() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        // Same `source_entity_id` (and thus the same origin key), two
        // different ingest runs by a different extractor build.
        let first = prov_from("doc:test_paper", "act_test_1");
        let mut second = prov_from("doc:test_paper", "act_test_2");
        second.agent_id = "second-extractor".into();
        store.record_assertion(&a, &first).await.unwrap();
        store.record_assertion(&a, &second).await.unwrap();

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            1,
            "a duplicate source must not add an evidence contribution"
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            1,
            "the same source re-recorded must not count as corroboration"
        );

        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            (facts[0].confidence - 0.8).abs() < 1e-9,
            "same-source repetition inflated confidence to {}",
            facts[0].confidence
        );

        // First attribution is immutable: source, activity, and agent all
        // stay with the FIRST committed writer.
        assert_eq!(facts[0].source, "doc:test_paper");
        assert_eq!(facts[0].agent, "gemma-4-12b");
        assert_eq!(
            query_str(&store, "SELECT activity_id FROM prov_assertion").await,
            "act_test_1",
            "the re-record overwrote the first activity attribution"
        );

        // recall is ordered by confidence DESC.
        let weak = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "beta".into(),
            confidence: Some(0.3),
        };
        store.record_assertion(&weak, &first).await.unwrap();
        let facts = store.recall("Ti-6Al-4V", "t1", 10).await.unwrap();
        assert_eq!(facts.len(), 2);
        assert!(facts[0].confidence >= facts[1].confidence);

        // Tenant scoping on recall.
        assert!(
            store
                .recall("alpha-beta", "other", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A genuinely different origin source IS new evidence: one assertion,
    /// two evidence contributions, noisy-OR 1-(1-0.8)² = 0.96.
    #[tokio::test]
    async fn a_second_source_corroborates_with_noisy_or() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        store
            .record_assertion(&a, &prov_from("doc:paper_a", "act_a"))
            .await
            .unwrap();
        store
            .record_assertion(&a, &prov_from("doc:paper_b", "act_b"))
            .await
            .unwrap();

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );

        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            (facts[0].confidence - 0.96).abs() < 1e-9,
            "two independent 0.8 sources must noisy-OR to 0.96, got {}",
            facts[0].confidence
        );
        // Parent attribution remains the FIRST source; B is not lost — it
        // lives in the evidence table (see the first-attribution test).
        assert_eq!(facts[0].source, "doc:paper_a");
        let sources: Vec<String> = store
            .assertion_evidence("t1", "Ti-6Al-4V", "has_phase", "alpha-beta")
            .await
            .unwrap()
            .into_iter()
            .map(|contribution| contribution.source_entity_id)
            .collect();
        assert_eq!(sources, vec!["doc:paper_a", "doc:paper_b"]);
    }

    /// A, B, A: the third record is a repeat of A and must not become a
    /// third contribution.
    #[tokio::test]
    async fn a_b_a_does_not_count_a_twice() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        for (source, activity) in [
            ("doc:paper_a", "act_1"),
            ("doc:paper_b", "act_2"),
            ("doc:paper_a", "act_3"),
        ] {
            store
                .record_assertion(&a, &prov_from(source, activity))
                .await
                .unwrap();
        }

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2,
            "the repeat of source A became a third contribution"
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );
        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert!(
            (facts[0].confidence - 0.96).abs() < 1e-9,
            "A,B,A must score exactly like A,B: {}",
            facts[0].confidence
        );
    }

    /// First attribution survives corroboration, and the corroborating
    /// source is queryable from the evidence table with its own
    /// activity/agent — nothing about B is lost by keeping A on the parent.
    #[tokio::test]
    async fn first_attribution_is_immutable_and_every_source_stays_queryable() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        let prov_a = prov_from("doc:paper_a", "act_a");
        let mut prov_b = prov_from("doc:paper_b", "act_b");
        prov_b.agent_id = "agent-b".into();
        store.record_assertion(&a, &prov_a).await.unwrap();
        store.record_assertion(&a, &prov_b).await.unwrap();

        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert_eq!(facts[0].source, "doc:paper_a", "parent source must stay A");
        assert_eq!(facts[0].agent, "gemma-4-12b", "parent agent must stay A's");
        assert_eq!(
            query_str(&store, "SELECT activity_id FROM prov_assertion").await,
            "act_a",
            "parent activity must stay A's"
        );

        let evidence = store
            .assertion_evidence("t1", "Ti-6Al-4V", "has_phase", "alpha-beta")
            .await
            .unwrap();
        assert_eq!(evidence.len(), 2);
        let b = evidence
            .iter()
            .find(|contribution| contribution.source_entity_id == "doc:paper_b")
            .expect("source B must be queryable from the evidence table");
        assert_eq!(b.activity_id, "act_b");
        assert_eq!(b.agent_id, "agent-b");
        assert!((b.confidence - 0.8).abs() < 1e-9);
        assert_eq!(b.confidence_kind, "source");
        assert_eq!(b.legacy_corroborations, None);
    }

    /// The evidence class may only ever get WORSE — agreement cannot turn
    /// literature into GREEN, and a source re-read at lower rigor taints
    /// what it previously claimed without touching confidence or counts.
    #[tokio::test]
    async fn evidence_class_downgrades_and_never_upgrades() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };
        // screening (A) then reference_validated (B): the parent keeps the
        // WORST class even though B is better and B genuinely corroborates.
        store
            .record_assertion_with_context(
                &a,
                &prov_from("doc:paper_a", "act_1"),
                None,
                None,
                &[],
                EvidenceClass::Screening,
            )
            .await
            .unwrap();
        store
            .record_assertion_with_context(
                &a,
                &prov_from("doc:paper_b", "act_2"),
                None,
                None,
                &[],
                EvidenceClass::ReferenceValidated,
            )
            .await
            .unwrap();
        let facts = store
            .recall_with_context("alpha-beta", "t1", 10)
            .await
            .unwrap();
        assert_eq!(facts[0].evidence_class, EvidenceClass::Screening);
        assert!((facts[0].confidence - 0.96).abs() < 1e-9);

        // The SAME source re-recorded as indeterminate: class downgrades on
        // both the contribution and the parent, confidence and count do not
        // move.
        store
            .record_assertion_with_context(
                &a,
                &prov_from("doc:paper_a", "act_3"),
                None,
                None,
                &[],
                EvidenceClass::Indeterminate,
            )
            .await
            .unwrap();
        let facts = store
            .recall_with_context("alpha-beta", "t1", 10)
            .await
            .unwrap();
        assert_eq!(facts[0].evidence_class, EvidenceClass::Indeterminate);
        assert!(
            (facts[0].confidence - 0.96).abs() < 1e-9,
            "a duplicate-source downgrade must not change confidence: {}",
            facts[0].confidence
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );

        // A later better class never upgrades either row back.
        store
            .record_assertion_with_context(
                &a,
                &prov_from("doc:paper_a", "act_4"),
                None,
                None,
                &[],
                EvidenceClass::Screening,
            )
            .await
            .unwrap();
        let facts = store
            .recall_with_context("alpha-beta", "t1", 10)
            .await
            .unwrap();
        assert_eq!(
            facts[0].evidence_class,
            EvidenceClass::Indeterminate,
            "a later better class must not upgrade the parent"
        );
        let evidence = store
            .assertion_evidence("t1", "Ti-6Al-4V", "has_phase", "alpha-beta")
            .await
            .unwrap();
        let class_for = |source: &str| {
            evidence
                .iter()
                .find(|contribution| contribution.source_entity_id == source)
                .map(|contribution| contribution.evidence_class)
        };
        assert_eq!(
            class_for("doc:paper_a"),
            Some(EvidenceClass::Indeterminate),
            "A's contribution must keep its worst class"
        );
        assert_eq!(
            class_for("doc:paper_b"),
            Some(EvidenceClass::ReferenceValidated),
            "B's contribution must be untouched by A's downgrade"
        );
    }

    /// Research vs screening on a DUPLICATE source: research is the WORSE
    /// class (`rank()`: Indeterminate 0 < Research 1 < Screening 2 <
    /// ReferenceValidated 3) and must win, while the duplicate source
    /// changes neither confidence nor corroborations. Pins the branch order
    /// in `WORST_CLASS_CASE`, which reads counter-intuitively (research
    /// checked before screening) but is CORRECT: it selects the
    /// lowest-ranked class present. Swapping those arms would wrongly keep
    /// 'screening'.
    #[tokio::test]
    async fn research_downgrades_screening_on_duplicate_source() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };
        for (activity, class) in [
            ("act_1", EvidenceClass::Screening),
            ("act_2", EvidenceClass::Research),
        ] {
            store
                .record_assertion_with_context(
                    &a,
                    &prov_from("doc:paper_a", activity),
                    None,
                    None,
                    &[],
                    class,
                )
                .await
                .unwrap();
        }
        let facts = store
            .recall_with_context("alpha-beta", "t1", 10)
            .await
            .unwrap();
        assert_eq!(
            facts[0].evidence_class,
            EvidenceClass::Research,
            "research (rank 1) is worse than screening (rank 2) and must win"
        );
        assert_eq!(
            query_str(&store, "SELECT evidence_class FROM prov_assertion_evidence").await,
            "research",
            "the stored contribution itself must carry the downgrade"
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            1,
            "a duplicate source must not corroborate"
        );
        let plain = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert!(
            (plain[0].confidence - 0.8).abs() < 1e-9,
            "a duplicate source must not move confidence, got {}",
            plain[0].confidence
        );
    }

    /// The EMMO graph must AGREE with the assertion. `upsert_edge` is
    /// last-writer-wins on `confidence`, and the Measurement node's
    /// `props_json` is replaced wholesale — so before the graph writes were
    /// fed the assertion's post-update aggregates, a DUPLICATE source could
    /// re-record Research/0.8 as ReferenceValidated/0.2 and, while the
    /// evidence row and assertion correctly stayed Research/0.8, the edge
    /// dropped to 0.2 and the node UPGRADED to reference_validated: same
    /// evidence set, different graph, evidence class upgraded by a
    /// duplicate. The graph must follow the evidence gate and
    /// `WORST_CLASS_CASE`, never the last writer.
    #[tokio::test]
    async fn the_graph_edge_follows_the_evidence_gate_not_the_last_writer() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let measurement = |confidence, evidence_class| MaterialFact {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_measurement".into(),
            object: "UTS".into(),
            value: Some(1140.0),
            unit: Some(QudtUnit::new("QUDT:MegaPA").unwrap()),
            conditions: vec![],
            confidence: Some(confidence),
            kind: Some("measurement".into()),
            evidence_class,
        };

        store
            .write_fact(
                &measurement(0.8, EvidenceClass::Research),
                &prov_from("doc:paper_a", "act_1"),
            )
            .await
            .unwrap();
        // The SAME source re-recorded "better" but weaker: the assertion
        // keeps Research/0.8, so the graph must too.
        store
            .write_fact(
                &measurement(0.2, EvidenceClass::ReferenceValidated),
                &prov_from("doc:paper_a", "act_2"),
            )
            .await
            .unwrap();

        for rel in ["HAS_MEASUREMENT", "OF_PROPERTY"] {
            let confidence = query_f64(
                &store,
                &format!("SELECT confidence FROM emmo_edge WHERE rel_type = '{rel}'"),
            )
            .await;
            assert!(
                (confidence - 0.8).abs() < 1e-9,
                "a duplicate source moved the {rel} edge to {confidence}"
            );
        }
        let props: serde_json::Value = serde_json::from_str(
            &query_str(
                &store,
                "SELECT props_json FROM emmo_entity WHERE label = 'Measurement'",
            )
            .await,
        )
        .unwrap();
        assert_eq!(
            props["evidence_class"], "research",
            "a duplicate source upgraded the Measurement node's evidence class"
        );
        assert_eq!(
            props["confidence"].as_f64(),
            Some(0.8),
            "a duplicate source moved the Measurement node's confidence"
        );

        // A genuinely NEW source corroborates: the graph follows the
        // assertion's combined aggregate (noisy-OR 0.96), not the last
        // writer's own 0.8.
        store
            .write_fact(
                &measurement(0.8, EvidenceClass::Research),
                &prov_from("doc:paper_b", "act_3"),
            )
            .await
            .unwrap();
        for rel in ["HAS_MEASUREMENT", "OF_PROPERTY"] {
            let confidence = query_f64(
                &store,
                &format!("SELECT confidence FROM emmo_edge WHERE rel_type = '{rel}'"),
            )
            .await;
            assert!(
                (confidence - 0.96).abs() < 1e-9,
                "a corroborated edge must carry the combined confidence, got {confidence} for {rel}"
            );
        }
    }

    /// The independence key collapses the obvious aliases of one source and
    /// keeps genuinely different sources apart.
    #[test]
    fn origin_source_key_normalizes() {
        // A relay carries someone else's knowledge: one conservative key,
        // regardless of which peer relayed it.
        assert_eq!(
            origin_source_key("http://peer-one.example/api#ds", true),
            "mesh:unattributed"
        );

        // DOI forms collapse: raw, resolver URL, percent-encoded, any case.
        assert_eq!(
            origin_source_key("doi:10.1234/AbC", false),
            "doi:10.1234/abc"
        );
        assert_eq!(
            origin_source_key("  https://doi.org/10.1234/AbC  ", false),
            "doi:10.1234/abc"
        );
        assert_eq!(
            origin_source_key("http://dx.doi.org/10.1234%2Fabc", false),
            "doi:10.1234/abc"
        );
        assert_eq!(
            origin_source_key("doi:10.1234%2FAbC", false),
            "doi:10.1234/abc",
            "the doi: form travels percent-encoded too and must decode like the resolver forms"
        );
        assert_eq!(
            origin_source_key("doi:10.1234%252Fabc", false),
            "doi:10.1234%2fabc",
            "decoding applies exactly once: a doubly-encoded slash names a DOI containing a literal %2F, not the plain-slash DOI"
        );

        // URLs: scheme/host case, default port, fragment, and dot segments
        // normalize away; the query string AND ITS ORDER are preserved.
        assert_eq!(
            origin_source_key("HTTPS://Example.com:443/a/./b/../c?q=1&r=2#frag", false),
            "url:https://example.com/a/c?q=1&r=2"
        );
        assert_eq!(
            origin_source_key("http://example.com", false),
            origin_source_key("http://example.com/", false),
        );
        assert_ne!(
            origin_source_key("https://e.com/p?a=1&b=2", false),
            origin_source_key("https://e.com/p?b=2&a=1", false),
            "query order is preserved — aggressive merging is the wrong direction"
        );

        // File paths: file:// URI and plain absolute path, `//` and dot
        // segments, all one key. No filesystem access is involved.
        assert_eq!(
            origin_source_key("file:///data//papers/./x.pdf", false),
            "file:/data/papers/x.pdf"
        );
        assert_eq!(
            origin_source_key("/data/papers/other/../x.pdf", false),
            "file:/data/papers/x.pdf"
        );
        // RFC 8089: an empty authority and `localhost` both mean this
        // machine; the scheme is case-insensitive like every other scheme.
        assert_eq!(
            origin_source_key("file://localhost/data//papers/./x.pdf", false),
            "file:/data/papers/x.pdf"
        );
        assert_eq!(
            origin_source_key("FILE:///data/papers/x.pdf", false),
            "file:/data/papers/x.pdf"
        );
        // A genuine remote authority is a DIFFERENT source from the local
        // path of the same spelling — merging them would drop evidence.
        assert_eq!(
            origin_source_key("file://FileServer/share/x.pdf", false),
            "file://fileserver/share/x.pdf"
        );
        assert_ne!(
            origin_source_key("file://fileserver/share/x.pdf", false),
            origin_source_key("/fileserver/share/x.pdf", false),
            "a remote file authority must not collide with a local path"
        );

        // Opaque relative paths get the file branch's lexical cleanup
        // (defensive: the live ingest path canonicalizes to an absolute
        // path and reaches the `/` branch instead).
        for alias in ["data/x.pdf", "./data/x.pdf", "data/./x.pdf", "data//x.pdf"] {
            assert_eq!(
                origin_source_key(alias, false),
                "opaque:data/x.pdf",
                "{alias:?} must collapse to the plain relative path"
            );
        }

        // Importer-assigned document ids are case-insensitive.
        assert_eq!(
            origin_source_key("document:0A3F", false),
            origin_source_key("DOCUMENT:0a3f", false),
        );

        // Anything else is opaque: stable per exact trimmed string.
        assert_eq!(
            origin_source_key(" doc:test_paper ", false),
            "opaque:doc:test_paper"
        );
        assert_ne!(
            origin_source_key("doc:a", false),
            origin_source_key("doc:b", false)
        );
    }

    /// End to end: the same paper reached by DOI and by resolver URL is ONE
    /// source; a file re-reached through a lexical path alias is ONE source.
    #[tokio::test]
    async fn doi_and_file_aliases_corroborate_once() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        store
            .record_assertion(&a, &prov_from("doi:10.1234/AbC", "act_1"))
            .await
            .unwrap();
        store
            .record_assertion(&a, &prov_from("https://doi.org/10.1234/abc", "act_2"))
            .await
            .unwrap();
        store
            .record_assertion(&a, &prov_from("doi:10.1234%2FAbC", "act_2b"))
            .await
            .unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            1,
            "the DOI resolver URL and the percent-encoded doi: form are the same paper, not extra sources"
        );

        store
            .record_assertion(&a, &prov_from("/data/papers/a.pdf", "act_3"))
            .await
            .unwrap();
        store
            .record_assertion(
                &a,
                &prov_from("file:///data/papers/../papers/a.pdf", "act_4"),
            )
            .await
            .unwrap();
        store
            .record_assertion(
                &a,
                &prov_from("file://localhost/data/papers/a.pdf", "act_5"),
            )
            .await
            .unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2,
            "the file:// and file://localhost/ aliases must merge; the file itself is a genuine second source"
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );
        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert!((facts[0].confidence - 0.96).abs() < 1e-9);
    }

    /// Writers sharing ONE store handle must serialize on the internal
    /// write mutex, not race the raw `BEGIN IMMEDIATE` into "cannot start a
    /// transaction within a transaction" and silently lose facts
    /// (`ProvenanceStore::write_lock`). Needs a multi-thread runtime: on a
    /// current-thread runtime turso's statements complete without yielding,
    /// so single-threaded interleaving cannot reproduce the race. Separate
    /// handles are covered by the concurrent-writer tests; this pins the
    /// same-handle case.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_writers_on_one_handle_serialize() {
        let db = TempDb::new();
        let store = std::sync::Arc::new(ProvenanceStore::open(&db.path).await.unwrap());
        let mut writers = tokio::task::JoinSet::new();
        for writer in 0..4 {
            let store = store.clone();
            writers.spawn(async move {
                for i in 0..8 {
                    let a = LocalAssertion {
                        subject: "Ti-6Al-4V".into(),
                        predicate: "has_phase".into(),
                        object: format!("phase_{writer}_{i}"),
                        confidence: Some(0.8),
                    };
                    let prov = prov_from(
                        &format!("doc:paper_{writer}_{i}"),
                        &format!("act_{writer}_{i}"),
                    );
                    store.record_assertion(&a, &prov).await?;
                }
                anyhow::Ok(())
            });
        }
        while let Some(joined) = writers.join_next().await {
            joined
                .unwrap()
                .expect("a same-handle writer must serialize, not error");
        }
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            32,
            "every same-handle writer's facts must land"
        );
    }

    /// The parent confidence is a function of the evidence SET, not of
    /// arrival order: {1.0, 0.8} must combine identically whichever source
    /// commits first, and the certain source keeps certainty even when it
    /// arrives second. Asymmetric values on purpose — a symmetric pair like
    /// {0.8, 0.8} cannot expose order dependence, which is why the
    /// concurrency tests never saw it.
    #[tokio::test]
    async fn confidence_is_order_independent_across_arrival_orders() {
        async fn combined(first: (&str, f64), second: (&str, f64)) -> f64 {
            let db = TempDb::new();
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            for (i, (source, confidence)) in [first, second].into_iter().enumerate() {
                let a = LocalAssertion {
                    subject: "Ti-6Al-4V".into(),
                    predicate: "has_phase".into(),
                    object: "alpha-beta".into(),
                    confidence: Some(confidence),
                };
                store
                    .record_assertion(&a, &prov_from(source, &format!("act_{i}")))
                    .await
                    .unwrap();
            }
            let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
            facts[0].confidence
        }

        let certain_first = combined(("doc:certain", 1.0), ("doc:strong", 0.8)).await;
        let certain_second = combined(("doc:strong", 0.8), ("doc:certain", 1.0)).await;
        assert!(
            (certain_first - certain_second).abs() < 1e-12,
            "identical evidence must not report different confidence by \
             arrival order: {certain_first} vs {certain_second}"
        );
        assert!(
            (certain_second - 1.0).abs() < 1e-9,
            "a certain source must keep certainty regardless of arrival \
             position, got {certain_second}"
        );
    }

    /// Two mesh peers relaying the same assertion are relays, not two
    /// independent sources: everything unattributed collapses onto ONE
    /// conservative contribution.
    #[tokio::test]
    async fn mesh_relays_collapse_to_one_unattributed_source() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        for (peer, activity) in [
            ("http://peer-one.example/api#dataset", "act_m1"),
            ("http://peer-two.example/api#dataset", "act_m2"),
        ] {
            let mut prov = prov_from(peer, activity);
            prov.tenant = "mesh".into();
            prov.locality = "mesh".into();
            store.record_assertion(&a, &prov).await.unwrap();
        }

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            1,
            "two peers echoing one fact must not count as two sources"
        );
        assert_eq!(
            query_str(&store, "SELECT source_key FROM prov_assertion_evidence").await,
            "mesh:unattributed"
        );
        let facts = store.recall("alpha-beta", "mesh", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            (facts[0].confidence - 0.8).abs() < 1e-9,
            "relayed repetition inflated confidence to {}",
            facts[0].confidence
        );
    }

    /// The capability [`LocalProvenance::origin_source_id`] exists for: two
    /// peers relaying two genuinely DIFFERENT origin sources are two pieces
    /// of evidence, so the mesh tenant can accumulate convergence instead
    /// of silently dropping the second peer's contribution.
    #[tokio::test]
    async fn mesh_relays_of_two_different_origins_corroborate() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        for (peer, activity, origin) in [
            ("http://peer-one.example/api#ds", "act_m1", "doi:10.1234/x"),
            ("http://peer-two.example/api#ds", "act_m2", "doi:10.1234/y"),
        ] {
            let mut prov = prov_from(peer, activity);
            prov.tenant = "mesh".into();
            prov.locality = "mesh".into();
            prov.origin_source_id = Some(origin.into());
            store.record_assertion(&a, &prov).await.unwrap();
        }

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2,
            "two different relayed origins must be two evidence rows"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion_evidence WHERE \
                 source_key IN ('mesh:doi:10.1234/x', 'mesh:doi:10.1234/y')",
            )
            .await,
            2,
            "relayed origins must be keyed inside the mesh: namespace"
        );
        let facts = store.recall("alpha-beta", "mesh", 10).await.unwrap();
        assert_eq!(facts.len(), 1, "one assertion row carries both origins");
        assert!(
            (facts[0].confidence - 0.96).abs() < 1e-9,
            "two independent 0.8 origins must noisy-OR to 0.96, got {}",
            facts[0].confidence
        );
    }

    /// Two peers relaying THE SAME origin — spelled two alias ways — are
    /// one observation: the relayed origin goes through the same
    /// alias-collapsing normalization as a local locator, so echoing a
    /// source cannot mint phantom corroboration.
    #[tokio::test]
    async fn mesh_relays_of_one_origin_count_once() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        for (peer, activity, origin) in [
            (
                "http://peer-one.example/api#ds",
                "act_m1",
                "doi:10.1234/AbC",
            ),
            (
                "http://peer-two.example/api#ds",
                "act_m2",
                " https://doi.org/10.1234/abc ",
            ),
        ] {
            let mut prov = prov_from(peer, activity);
            prov.tenant = "mesh".into();
            prov.locality = "mesh".into();
            prov.origin_source_id = Some(origin.into());
            store.record_assertion(&a, &prov).await.unwrap();
        }

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            1,
            "two spellings of one origin must stay one evidence row"
        );
        assert_eq!(
            query_str(&store, "SELECT source_key FROM prov_assertion_evidence").await,
            "mesh:doi:10.1234/abc"
        );
        let facts = store.recall("alpha-beta", "mesh", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            (facts[0].confidence - 0.8).abs() < 1e-9,
            "one origin echoed by two peers inflated confidence to {}",
            facts[0].confidence
        );
    }

    /// A peer-supplied origin is attacker-chosen input. Claiming the exact
    /// locator of a locally-ingested source must not corroborate — or even
    /// touch — the local tenant's fact: the tenant-keyed assertion id lands
    /// the relay on the mesh tenant's own row, and the relayed key lives in
    /// the disjoint `mesh:` namespace, so it cannot equal (or dedupe
    /// against) the local contribution's key either.
    #[tokio::test]
    async fn a_peer_supplied_origin_cannot_corroborate_a_local_fact() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        // The user ingests a paper locally.
        let mut local = prov_from("/data/papers/x.pdf", "act_local");
        local.tenant = "local".into();
        store.record_assertion(&a, &local).await.unwrap();

        // A peer claims ITS copy of the fact came from that very file.
        let mut mesh = prov_from("http://peer-one.example/api#ds", "act_mesh");
        mesh.tenant = "mesh".into();
        mesh.locality = "mesh".into();
        mesh.origin_source_id = Some("/data/papers/x.pdf".into());
        store.record_assertion(&a, &mesh).await.unwrap();

        // The local fact is untouched: its own confidence, its own single
        // evidence contribution under the un-namespaced local key.
        let facts = store.recall("alpha-beta", "local", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            (facts[0].confidence - 0.8).abs() < 1e-9,
            "a relayed claim of the local locator inflated the LOCAL fact to {}",
            facts[0].confidence
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion_evidence \
                 WHERE source_key = 'file:/data/papers/x.pdf'",
            )
            .await,
            1,
            "only the local ingest may own the un-namespaced key"
        );
        assert_eq!(
            query_str(
                &store,
                "SELECT source_key FROM prov_assertion_evidence \
                 WHERE activity_id = 'act_mesh'",
            )
            .await,
            "mesh:file:/data/papers/x.pdf",
            "the relay's claim must stay inside the mesh: namespace"
        );
    }

    /// Key derivation honours an explicit origin, and the relayed
    /// namespace is disjoint from every locally-derivable one.
    #[test]
    fn origin_source_key_for_honours_and_namespaces_the_origin() {
        let mut prov = test_prov(); // tenant "t1", locality "local"
        prov.source_entity_id = "/data/x.pdf".into();

        // No origin: exactly the historical locator derivation.
        assert_eq!(origin_source_key_for(&prov), "file:/data/x.pdf");

        // A local writer stating the origin it read is trusted directly.
        prov.origin_source_id = Some("doi:10.1234/AbC".into());
        assert_eq!(origin_source_key_for(&prov), "doi:10.1234/abc");

        // A relay's origin is normalized, then namespaced under mesh:.
        prov.locality = "mesh".into();
        assert_eq!(origin_source_key_for(&prov), "mesh:doi:10.1234/abc");

        // A blank or absent origin is NO origin — conservative collapse.
        prov.origin_source_id = Some("   ".into());
        assert_eq!(origin_source_key_for(&prov), "mesh:unattributed");
        prov.origin_source_id = None;
        assert_eq!(origin_source_key_for(&prov), "mesh:unattributed");

        // No local derivation can mint a mesh: key — a literal mesh:…
        // locator lands in the opaque namespace — so a relay cannot claim
        // local origin and a local write cannot pose as a relay's.
        assert_eq!(
            origin_source_key("mesh:unattributed", false),
            "opaque:mesh:unattributed"
        );
        assert_eq!(
            origin_source_key("mesh:doi:10.1234/x", false),
            "opaque:mesh:doi:10.1234/x"
        );
    }

    /// Mesh sync now writes each peer under its own tenant
    /// `mesh:{publisher node id}`. Relay detection must keep holding for
    /// those tenants ON THEIR OWN — a future writer that fills the tenant
    /// but forgets `locality = "mesh"` must still be classified a relay,
    /// or its peer-supplied origin would be trusted as a local one.
    #[test]
    fn a_per_peer_mesh_tenant_is_still_a_relay() {
        let mut prov = test_prov(); // locality "local"
        prov.tenant = "mesh:0f2c7e1a-aaaa-bbbb-cccc-000000000001".into();
        prov.origin_source_id = Some("doi:10.1234/abc".into());
        assert_eq!(
            origin_source_key_for(&prov),
            "mesh:doi:10.1234/abc",
            "a mesh:{{node id}} tenant alone must classify as a relay"
        );

        // And with no conveyed origin it collapses conservatively.
        prov.origin_source_id = None;
        assert_eq!(origin_source_key_for(&prov), "mesh:unattributed");

        // A non-mesh tenant with local locality stays a local write.
        prov.tenant = "meshless".into();
        prov.source_entity_id = "doc:x".into();
        assert_eq!(origin_source_key_for(&prov), "opaque:doc:x");
    }

    /// `write_synced_entity` must store the peer's OWN label and properties
    /// (write_fact's generic arm hardcoded `Matter`, which mislabeled every
    /// non-Matter peer node), link it to a `Dataset` node, and record the
    /// relay's evidence under the mesh-namespaced origin key.
    #[tokio::test]
    async fn write_synced_entity_keeps_label_props_and_origin() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let mut prov = test_prov();
        prov.tenant = "mesh:node-a".into();
        prov.locality = "mesh".into();
        prov.origin_source_id = Some("doi:10.1234/abc".into());

        store
            .write_synced_entity(
                "alpha phase",
                "Phase",
                Some(r#"{"origin_source":"doi:10.1234/abc"}"#.into()),
                "ti-alloys",
                &prov,
            )
            .await
            .unwrap();

        // The entity keeps the peer's label/type and properties.
        let label = query_str(
            &store,
            "SELECT label FROM emmo_entity WHERE name = 'alpha phase' AND tenant = 'mesh:node-a'",
        )
        .await;
        assert_eq!(label, "Phase", "peer label must survive the sync write");
        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_entity WHERE name = 'alpha phase' AND tenant = 'mesh:node-a'",
        )
        .await;
        assert!(
            props.contains("doi:10.1234/abc"),
            "peer properties must survive the sync write: {props}"
        );

        // The dataset node + SYNCED_FROM edge exist under the same tenant.
        let dataset_label = query_str(
            &store,
            "SELECT label FROM emmo_entity WHERE name = 'ti-alloys' AND tenant = 'mesh:node-a'",
        )
        .await;
        assert_eq!(dataset_label, "Dataset");
        let edges = count(
            &store,
            "SELECT COUNT(*) FROM emmo_edge WHERE rel_type = 'SYNCED_FROM' AND tenant = 'mesh:node-a'",
        )
        .await;
        assert_eq!(edges, 1);

        // The assertion's evidence is keyed on the mesh-namespaced origin.
        let evidence = store
            .assertion_evidence("mesh:node-a", "alpha phase", "SYNCED_FROM", "ti-alloys")
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].source_key, "mesh:doi:10.1234/abc");
    }

    /// v5 migration: a legacy RELAY row stored under a real tenant (tenant
    /// `t1`, activity locality `mesh`) must migrate to the SAME
    /// `mesh:unattributed` key the live path assigns. The locality was never
    /// stored on the assertion row, but the assertion's activity row was,
    /// and carries it — a migration that classified relays by tenant alone
    /// assigned `url:…` here, so replaying the same relay live created a
    /// SECOND evidence row and inflated 0.8 to 0.96: phantom corroboration.
    #[tokio::test]
    async fn a_legacy_relay_under_a_real_tenant_migrates_like_the_live_path() {
        let db = TempDb::new();
        let id = assertion_id("t1", "Ti-6Al-4V", "has_phase", "alpha-beta");
        {
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            drop(store); // schema now exists
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO prov_activity
                   (id, agent_id, source_entity_id, tenant, started_at, ended_at, locality)
                   VALUES ('act_relay', 'peer-agent', 'https://peer.example/data', 't1',
                           '2026-01-01T00:00:00Z', '2026-01-01T00:00:01Z', 'mesh')"#,
                (),
            )
            .await
            .unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant)
                   VALUES (?1, 'Ti-6Al-4V', 'has_phase', 'alpha-beta', '[]', 'research',
                           0.8, 1, 'act_relay', 'https://peer.example/data', 'peer-agent', 't1')"#,
                [Value::Text(id.clone())],
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        // Reopening runs the v5 backfill.
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            query_str(&store, "SELECT source_key FROM prov_assertion_evidence").await,
            "mesh:unattributed",
            "the migration must classify tenant-t1/locality-mesh as a relay, \
             exactly like the live path"
        );

        // Replaying the SAME relay live must be the same source, not a
        // second one.
        let mut prov = prov_from("https://peer.example/data", "act_relay_live");
        prov.locality = "mesh".into();
        store
            .record_assertion(
                &LocalAssertion {
                    subject: "Ti-6Al-4V".into(),
                    predicate: "has_phase".into(),
                    object: "alpha-beta".into(),
                    confidence: Some(0.8),
                },
                &prov,
            )
            .await
            .unwrap();

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            1,
            "the migrated relay and the live relay split into two evidence \
             rows — one relayed source counted twice"
        );
        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert!(
            (facts[0].confidence - 0.8).abs() < 1e-9,
            "replaying one relayed source inflated confidence to {}",
            facts[0].confidence
        );
    }

    /// Two concurrent writers, same assertion, same source: both succeed,
    /// one evidence row, confidence unchanged — no key conflict escapes and
    /// no phantom corroboration happens.
    ///
    /// This is the read-modify-write race the transaction exists for: the
    /// old path had both writers SELECT nothing, both INSERT, and the loser
    /// abort its whole document on the primary-key error.
    #[tokio::test]
    async fn concurrent_same_source_writers_both_succeed_with_one_contribution() {
        let db = TempDb::new();
        // Stamp schema + migrations once so the racers race on the write
        // path itself, not on `open()`.
        drop(ProvenanceStore::open(&db.path).await.unwrap());

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut writers = Vec::new();
        for activity in ["act_r1", "act_r2"] {
            let path = db.path.clone();
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let store = ProvenanceStore::open(&path).await?;
                    let a = LocalAssertion {
                        subject: "Ti-6Al-4V".into(),
                        predicate: "has_phase".into(),
                        object: "alpha-beta".into(),
                        confidence: Some(0.8),
                    };
                    let prov = prov_from("doc:test_paper", activity);
                    barrier.wait();
                    store.record_assertion(&a, &prov).await
                })
            }));
        }
        for writer in writers {
            writer
                .join()
                .expect("writer thread panicked")
                .expect("both concurrent same-source writers must succeed");
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            1,
            "a concurrent duplicate of the same source became a second contribution"
        );
        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert!(
            (facts[0].confidence - 0.8).abs() < 1e-9,
            "the concurrent duplicate corroborated: {}",
            facts[0].confidence
        );
    }

    /// Two concurrent writers with genuinely different sources: both count,
    /// atomically — two evidence rows, corroborations 2, noisy-OR 0.96, and
    /// whichever transaction committed first owns the parent attribution.
    #[tokio::test]
    async fn concurrent_different_source_writers_both_count() {
        let db = TempDb::new();
        drop(ProvenanceStore::open(&db.path).await.unwrap());

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut writers = Vec::new();
        for (source, activity) in [("doc:paper_a", "act_a"), ("doc:paper_b", "act_b")] {
            let path = db.path.clone();
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || -> Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let store = ProvenanceStore::open(&path).await?;
                    let a = LocalAssertion {
                        subject: "Ti-6Al-4V".into(),
                        predicate: "has_phase".into(),
                        object: "alpha-beta".into(),
                        confidence: Some(0.8),
                    };
                    let prov = prov_from(source, activity);
                    barrier.wait();
                    store.record_assertion(&a, &prov).await
                })
            }));
        }
        for writer in writers {
            writer
                .join()
                .expect("writer thread panicked")
                .expect("both concurrent different-source writers must succeed");
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2,
            "an increment was lost — both sources must contribute"
        );
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );
        let facts = store.recall("alpha-beta", "t1", 10).await.unwrap();
        assert!(
            (facts[0].confidence - 0.96).abs() < 1e-9,
            "both contributions must noisy-OR: {}",
            facts[0].confidence
        );
        // The parent's first attribution is whichever COMMITTED first —
        // deliberately not asserting which.
        assert!(
            ["doc:paper_a", "doc:paper_b"].contains(&facts[0].source.as_str()),
            "parent attribution must be one of the racers: {}",
            facts[0].source
        );
    }

    /// A writer that meets a short-lived competing transaction WAITS (busy
    /// timeout) and then succeeds — it does not fail fast and it does not
    /// skip the write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_competing_writer_waits_then_succeeds() {
        let db = TempDb::new();
        let holder = ProvenanceStore::open(&db.path).await.unwrap();
        let writer = ProvenanceStore::open(&db.path).await.unwrap();

        holder.conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            holder.conn.execute("COMMIT", ()).await.unwrap();
        });

        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };
        let started = std::time::Instant::now();
        writer
            .record_assertion(&a, &prov_from("doc:test_paper", "act_1"))
            .await
            .expect("a short competing transaction must mean WAIT, not failure");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(300),
            "the writer did not actually contend for the lock — the test is a no-op"
        );
        release.await.unwrap();

        assert_eq!(
            count(&writer, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
    }

    /// A lock held PAST the busy timeout is a typed, retriable error and
    /// leaves no partial fact — never silent success, never a fake
    /// duplicate. (Takes ~5s: the full busy timeout must actually elapse.)
    #[tokio::test]
    async fn a_lock_held_past_the_timeout_is_a_typed_busy_error() {
        let db = TempDb::new();
        let holder = ProvenanceStore::open(&db.path).await.unwrap();
        let writer = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        holder.conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
        let err = writer
            .record_assertion(&a, &prov_from("doc:test_paper", "act_1"))
            .await
            .expect_err("the lock outlives the busy timeout — success would be a lie");
        assert!(
            err.downcast_ref::<StoreBusy>().is_some(),
            "busy must surface as the typed retriable StoreBusy, got: {err:#}"
        );
        holder.conn.execute("ROLLBACK", ()).await.unwrap();

        // No partial fact: no assertion, no evidence, no activity.
        for table in ["prov_assertion", "prov_assertion_evidence", "prov_activity"] {
            assert_eq!(
                count(&writer, &format!("SELECT COUNT(*) FROM {table}")).await,
                0,
                "a failed write left partial rows in {table}"
            );
        }

        // Retriable means exactly that: the same call now goes through.
        writer
            .record_assertion(&a, &prov_from("doc:test_paper", "act_1"))
            .await
            .expect("the busy error must be retriable once the lock is released");
        assert_eq!(
            count(&writer, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
    }

    /// A write cancelled mid-transaction must leave the connection USABLE:
    /// the next operation rolls the abandoned transaction back, so reads
    /// never see the partial rows and the next write does not die on
    /// "cannot start a transaction within a transaction".
    ///
    /// Cancellation is simulated by dropping the transaction guard exactly
    /// where a dropped future drops it — after a write, before COMMIT. The
    /// public path cannot be cancelled deterministically: turso's local
    /// statements complete without yielding, so there is no await point for
    /// a test to park a real cancellation on. Rollback is async and `Drop`
    /// is not, so the guard defers the rollback to the connection's next
    /// operation (see [`begin_immediate`]) — which is why the READ below is
    /// itself part of the pin.
    #[tokio::test]
    async fn a_cancelled_write_rolls_back_and_the_store_stays_usable() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        {
            let _guard = store.write_lock.lock().await;
            let txn = begin_immediate(&store.conn).await.unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO prov_agent (id, kind) VALUES ('half-written', 'SoftwareAgent')",
                    (),
                )
                .await
                .unwrap();
            drop(txn); // the future is dropped here — COMMIT never runs
        }

        // The next READ must not see the abandoned transaction's rows.
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_agent").await,
            0,
            "the cancelled write's partial rows are visible — the abandoned \
             transaction was never rolled back"
        );

        // The next WRITE must succeed instead of surfacing the opaque
        // nested-transaction error, and land its full fact.
        store
            .record_assertion(
                &LocalAssertion {
                    subject: "Ti-6Al-4V".into(),
                    predicate: "has_phase".into(),
                    object: "alpha-beta".into(),
                    confidence: Some(0.8),
                },
                &prov_from("doc:after_cancel", "act_after"),
            )
            .await
            .expect("the store stayed wedged inside the cancelled transaction");
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            1
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_agent").await,
            1,
            "only the post-cancel writer's agent row may exist"
        );
    }

    /// A `record()` racing an OPEN `write_fact` transaction on the same
    /// handle must not silently join it. The unlocked path let B's INSERT
    /// land inside A's transaction, so A's rollback erased a record whose
    /// caller had already been told `Ok(())` — a silent lost write. Every
    /// writer on the shared connection must hold the same write lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_record_racing_an_open_write_transaction_is_not_lost() {
        let db = TempDb::new();
        let store = std::sync::Arc::new(ProvenanceStore::open(&db.path).await.unwrap());

        // A: open a write transaction exactly as `write_fact` does — lock
        // first, then BEGIN IMMEDIATE — and keep it open while B runs.
        let guard = store.write_lock.lock().await;
        let txn = begin_immediate(&store.conn).await.unwrap();

        // B: an unrelated ledger write through the public API.
        let record = crate::new_record(
            "race-session",
            crate::ActionType::ToolCall,
            crate::Actor::Agent,
            Some("t"),
            None,
            serde_json::json!({}),
        );
        let writer = {
            let store = store.clone();
            let record = record.clone();
            tokio::spawn(async move { store.record(&record).await })
        };

        // Give B every chance to (wrongly) run its INSERT while A's
        // transaction is open.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // A's fact is rejected (the NaN-confidence path) — A rolls back.
        let rejected: Result<()> = Err(anyhow::anyhow!("assertion confidence must be finite"));
        finish_write_txn(txn, rejected)
            .await
            .expect_err("the rejected write must surface its error");
        drop(guard);

        writer
            .await
            .unwrap()
            .expect("record() must succeed, after the transaction, not inside it");
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM provenance_records").await,
            1,
            "record() returned Ok but the row is gone — it joined A's \
             transaction and was destroyed by A's rollback"
        );
    }

    /// v5 migration: a pre-evidence row with an inflated `corroborations`
    /// collapses to its one identifiable source, KEEPS its stored (possibly
    /// phantom) confidence, and is permanently marked `legacy_aggregate` on
    /// both the contribution and the parent. Idempotent across reopens and
    /// even across a forced re-run.
    #[tokio::test]
    async fn legacy_corroborations_collapse_to_one_marked_contribution() {
        let db = TempDb::new();
        let id = assertion_id("t1", "steel", "has_phase", "bcc");
        {
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            drop(store); // schema now exists
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant)
                   VALUES (?1, 'steel', 'has_phase', 'bcc', '[]', 'research',
                           0.96, 2, 'act_legacy', 'legacy.csv', 'legacy-agent', 't1')"#,
                [Value::Text(id.clone())],
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        async fn assert_migrated(store: &ProvenanceStore) {
            assert_eq!(
                count(store, "SELECT corroborations FROM prov_assertion").await,
                1,
                "the phantom per-ingest count must collapse to the one known source"
            );
            let facts = store.recall("steel", "t1", 10).await.unwrap();
            assert!(
                (facts[0].confidence - 0.96).abs() < 1e-9,
                "migration must keep the stored confidence, not invent one: {}",
                facts[0].confidence
            );
            assert_eq!(
                query_str(store, "SELECT confidence_basis FROM prov_assertion").await,
                "legacy_aggregate"
            );
            let evidence = store
                .assertion_evidence("t1", "steel", "has_phase", "bcc")
                .await
                .unwrap();
            assert_eq!(evidence.len(), 1);
            assert_eq!(evidence[0].source_key, "opaque:legacy.csv");
            assert_eq!(evidence[0].confidence_kind, "legacy_aggregate");
            assert_eq!(
                evidence[0].legacy_corroborations,
                Some(2),
                "the old count must stay visible on the marked contribution"
            );
            assert_eq!(evidence[0].evidence_class, EvidenceClass::Research);
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_migrated(&store).await;

        // Reopen: stamped, nothing re-runs, nothing changes.
        drop(store);
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_migrated(&store).await;

        // A real second source still corroborates the migrated row…
        store
            .record_assertion(
                &LocalAssertion {
                    subject: "steel".into(),
                    predicate: "has_phase".into(),
                    object: "bcc".into(),
                    confidence: Some(0.5),
                },
                &prov_from("doc:new_paper", "act_new"),
            )
            .await
            .unwrap();
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2
        );

        // …and a FORCED re-run over post-v5 data must not renormalize it:
        // the evidence rows already exist, so the backfill must leave the
        // real aggregates alone.
        store
            .conn
            .execute("PRAGMA user_version = 0", ())
            .await
            .unwrap();
        drop(store);
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(&store, "SELECT corroborations FROM prov_assertion").await,
            2,
            "a re-run backfill destroyed real post-migration corroborations"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2
        );
    }

    /// The same source key under two tenants stays two assertions with two
    /// independent evidence rows — the origin key does not weaken the
    /// tenant-in-key design.
    #[tokio::test]
    async fn same_source_key_under_two_tenants_stays_two_assertions() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let a = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        for tenant in ["t1", "t2"] {
            let mut prov = prov_from("doc:shared_paper", &format!("act_{tenant}"));
            prov.tenant = tenant.into();
            store.record_assertion(&a, &prov).await.unwrap();
        }

        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            2
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion_evidence").await,
            2
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(DISTINCT source_key) FROM prov_assertion_evidence"
            )
            .await,
            1,
            "one shared source key, two per-tenant contributions"
        );
        for tenant in ["t1", "t2"] {
            let facts = store.recall("alpha-beta", tenant, 10).await.unwrap();
            assert_eq!(facts.len(), 1);
            assert!(
                (facts[0].confidence - 0.8).abs() < 1e-9,
                "{tenant} was corroborated by the other tenant's identical source"
            );
        }
    }

    #[test]
    fn canonical_key_normalizes() {
        assert_eq!(canonical_key("  Ti-6Al-4V  "), "ti-6al-4v");
        assert_eq!(canonical_key("Yield   Strength"), "yield strength");
        assert_eq!(
            canonical_key("YIELD\tstrength"),
            canonical_key("yield STRENGTH ")
        );
        assert_ne!(
            canonical_key("yield strength"),
            canonical_key("tensile strength")
        );
    }

    #[test]
    fn assertion_id_is_stable_and_canonical() {
        let a = assertion_id("t1", "Ti-6Al-4V", "has_phase", "alpha-beta");
        let b = assertion_id("t1", "  ti-6al-4v ", "has_phase", "ALPHA-BETA");
        let c = assertion_id("t1", "alpha-beta", "has_phase", "Ti-6Al-4V");
        assert_eq!(a, b, "spelling variants must corroborate one assertion");
        assert_ne!(a, c, "direction matters");
        assert_eq!(a.len(), 64);
    }

    /// A measured 0.0 is real data and must not share an id with "no value".
    ///
    /// `value.map(f64::to_bits).unwrap_or(0)` collapsed them: absent and
    /// Some(0.0) both hashed as eight zero bytes, so a fact recording a
    /// measurement of exactly zero would corroborate — and be overwritten by —
    /// a fact carrying no value at all.
    #[test]
    fn an_absent_value_does_not_hash_like_a_measured_zero() {
        let absent =
            conditioned_assertion_id("t", "s", "p", "o", None, Some("QUDT:K"), &[]).unwrap();
        let zero =
            conditioned_assertion_id("t", "s", "p", "o", Some(0.0), Some("QUDT:K"), &[]).unwrap();
        assert_ne!(absent, zero, "no-value and a measured 0.0 share an id");

        // Same hazard on the unit: absent vs present-but-empty.
        let no_unit = conditioned_assertion_id("t", "s", "p", "o", Some(1.0), None, &[]).unwrap();
        let empty_unit =
            conditioned_assertion_id("t", "s", "p", "o", Some(1.0), Some(""), &[]).unwrap();
        assert_ne!(
            no_unit, empty_unit,
            "absent unit and empty unit share an id"
        );
    }

    /// Golden digest. The assertion id is a persisted key: changing how it is
    /// computed silently re-keys every stored row, so any change to the hash
    /// scheme must be a deliberate act that bumps
    /// `ASSERTION_TENANT_KEY_VERSION`. This test exists to make an accidental
    /// change loud.
    ///
    /// It also pins the width of the length prefix — a `usize` prefix would
    /// produce a different digest on a 32-bit target, making ids
    /// architecture-dependent.
    ///
    /// The expected value is not copied back out of a failing run: it was
    /// computed by an independent implementation of the documented scheme and
    /// matched byte for byte, so this pins the SPEC rather than the code.
    #[test]
    fn assertion_id_digest_is_pinned() {
        assert_eq!(
            assertion_id("local", "Ti-6Al-4V", "has_phase", "alpha-beta"),
            "7d980938cac1e3aa1084e04bf61a466a51d7e3f4f8b5d19d6c84cf10ff30e0b1",
            "the assertion hash scheme changed; bump ASSERTION_TENANT_KEY_VERSION deliberately",
        );
    }

    /// A separator inside a tenant name must not be able to imitate a field
    /// boundary.
    ///
    /// `canonical_key` collapses whitespace and lowercases; it does NOT strip
    /// or escape `|`. With a bare `|` separator these two hashed identically,
    /// which is one tenant silently corroborating another's assertion:
    ///   tenant "acme|steel" + subject "UTS"
    ///   tenant "acme"       + subject "steel|UTS"
    #[test]
    fn a_separator_in_the_tenant_cannot_forge_a_field_boundary() {
        assert_ne!(
            assertion_id("acme|steel", "UTS", "has_measurement", "x"),
            assertion_id("acme", "steel|UTS", "has_measurement", "x"),
            "tenant/subject boundary is forgeable — one tenant can reach another's row",
        );
        // Same hazard on the conditioned path.
        let left =
            conditioned_assertion_id("acme|steel", "UTS", "p", "x", Some(1.0), None, &[]).unwrap();
        let right =
            conditioned_assertion_id("acme", "steel|UTS", "p", "x", Some(1.0), None, &[]).unwrap();
        assert_ne!(
            left, right,
            "conditioned id has the same forgeable boundary"
        );
    }

    /// A row predating the tenant column must not be orphaned by the re-key.
    ///
    /// `add_column_if_absent` gives such rows NULL, which reads as `""`.
    /// Re-keying them under `""` would move them to a tenant no read path ever
    /// queries, silently losing the history the migration exists to preserve.
    #[tokio::test]
    async fn a_row_with_no_tenant_is_recovered_as_local_not_orphaned() {
        let db = TempDb::new();
        {
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            drop(store);
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant)
                   VALUES ('pre-tenant-id', 'steel', 'has_phase', 'bcc', '[]', 'research',
                           0.7, 4, 'act', 'legacy.csv', 'legacy-agent', NULL)"#,
                (),
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let recalled = store.recall("steel", "local", 10).await.unwrap();
        assert_eq!(
            recalled.len(),
            1,
            "the pre-tenant row was orphaned under a tenant nothing reads",
        );
        assert!((recalled[0].confidence - 0.7).abs() < 1e-9);
    }

    /// The same triple under two tenants must be two different assertions.
    ///
    /// `prov_assertion` is keyed on this id alone and the corroboration
    /// lookup does not filter tenant, so an id that omitted the tenant let one
    /// tenant raise another's `confidence` and `corroborations`.
    #[test]
    fn assertion_id_separates_tenants() {
        let local = assertion_id("local", "Ti-6Al-4V", "has_phase", "alpha-beta");
        let mesh = assertion_id("mesh", "Ti-6Al-4V", "has_phase", "alpha-beta");
        assert_ne!(local, mesh, "two tenants collided on one assertion id");

        // Exact, not case-folded: distinct tenants stay distinct.
        assert_ne!(
            assertion_id("Local", "a", "p", "b"),
            assertion_id("local", "a", "p", "b"),
        );
    }

    /// The end-to-end property the id change exists for: a second tenant
    /// asserting the same triple must not corroborate the first.
    #[tokio::test]
    async fn a_second_tenant_cannot_corroborate_the_first() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        let assertion = LocalAssertion {
            subject: "Ti-6Al-4V".into(),
            predicate: "has_phase".into(),
            object: "alpha-beta".into(),
            confidence: Some(0.8),
        };

        for tenant in ["local", "mesh"] {
            let prov = LocalProvenance {
                activity_id: format!("activity-{tenant}"),
                agent_id: format!("agent-{tenant}"),
                agent_kind: "SoftwareAgent".into(),
                source_entity_id: format!("source-{tenant}"),
                source_kind: "Dataset".into(),
                tenant: tenant.into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                ended_at: "2026-01-01T00:00:00Z".into(),
                locality: tenant.into(),
                origin_source_id: None,
            };
            store.record_assertion(&assertion, &prov).await.unwrap();
        }

        // Each tenant sees its own assertion, at its own unmodified confidence.
        for tenant in ["local", "mesh"] {
            let recalled = store.recall("Ti-6Al-4V", tenant, 10).await.unwrap();
            assert_eq!(recalled.len(), 1, "{tenant} lost or duplicated its row");
            assert!(
                (recalled[0].confidence - 0.8).abs() < 1e-9,
                "{tenant} confidence was inflated to {} by the other tenant",
                recalled[0].confidence,
            );
        }
    }

    /// A row written before the tenant was in the id must be re-keyed in
    /// place, not orphaned.
    ///
    /// Without the migration the next write of the same triple computes a
    /// different id, misses this row, and inserts a duplicate whose
    /// `corroborations` restarts at 1 — the user's history silently forks.
    #[tokio::test]
    async fn legacy_tenantless_assertion_ids_are_rekeyed_and_still_corroborate() {
        let db = TempDb::new();

        // The pre-fix id: hashed without the tenant.
        let legacy_id = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(canonical_key("steel").as_bytes());
            h.update(b"|");
            h.update(b"has_phase");
            h.update(b"|");
            h.update(canonical_key("bcc").as_bytes());
            h.finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };

        {
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            drop(store); // schema now exists
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant)
                   VALUES (?1, 'steel', 'has_phase', 'bcc', '[]', 'research',
                           0.7, 3, 'act', 'legacy.csv', 'legacy-agent', 't1')"#,
                [Value::Text(legacy_id.clone())],
            )
            .await
            .unwrap();
            // Opening the store above stamped the schema version. A database
            // that genuinely predates the migration carries version 0, so put
            // it back — otherwise this fixture tests the skip path, not the
            // migration.
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        // Re-opening runs the migration.
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        // Still exactly one row, still readable, history intact.
        let recalled = store.recall("steel", "t1", 10).await.unwrap();
        assert_eq!(recalled.len(), 1, "the legacy row was lost or duplicated");
        assert!((recalled[0].confidence - 0.7).abs() < 1e-9);

        // And it is now reachable by the tenant-scoped id, so a further
        // assertion corroborates it rather than forking a second row.
        let prov = LocalProvenance {
            activity_id: "act2".into(),
            agent_id: "agent2".into(),
            agent_kind: "SoftwareAgent".into(),
            source_entity_id: "second.csv".into(),
            source_kind: "Dataset".into(),
            tenant: "t1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended_at: "2026-01-01T00:00:00Z".into(),
            locality: "local".into(),
            origin_source_id: None,
        };
        store
            .record_assertion(
                &LocalAssertion {
                    subject: "steel".into(),
                    predicate: "has_phase".into(),
                    object: "bcc".into(),
                    confidence: Some(0.5),
                },
                &prov,
            )
            .await
            .unwrap();

        let recalled = store.recall("steel", "t1", 10).await.unwrap();
        assert_eq!(
            recalled.len(),
            1,
            "the re-assertion forked a second row instead of corroborating",
        );
        assert!(
            recalled[0].confidence > 0.7,
            "corroboration did not raise confidence: {}",
            recalled[0].confidence,
        );
    }

    /// A fresh store must be stamped as migrated even though its assertion
    /// table is empty, or every later open repeats the scan for nothing.
    #[tokio::test]
    async fn a_fresh_store_is_stamped_migrated() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        assert_eq!(
            read_user_version(&conn).await.unwrap(),
            PROV_EVIDENCE_VERSION,
            "the stamp must be the LATEST generation, not the assertion one — \
             stamping the assertion version would leave the key migration \
             re-running on every open",
        );
    }

    /// A v3 database must finish the job without re-hashing its assertions.
    ///
    /// v3 -> v4 changes no assertion digest, and the re-key SHA-256s every
    /// assertion in the store, so gating both migrations on one threshold would
    /// pay that scan for nothing. Pinned by observation: an assertion row
    /// carrying a deliberately wrong id is left alone (the re-key did not run)
    /// while the unqualified EMMO key beside it IS qualified (the key migration
    /// did), and the database ends stamped at v4.
    #[tokio::test]
    async fn a_v3_database_migrates_keys_without_rekeying_assertions() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant)
                   VALUES ('deliberately-not-a-digest', 'steel', 'has_phase', 'bcc', '[]',
                           'research', 0.7, 1, 'act', 'x.csv', 'agent', 't1')"#,
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO emmo_entity (key, name, label, entity_type, tenant, props_json, created_at)
                 VALUES ('Matter:steel', 'steel', 'Matter', 'Matter', 't1', '{}', '2026-01-01T00:00:00Z')",
                (),
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 3", ()).await.unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion WHERE id = 'deliberately-not-a-digest'",
            )
            .await,
            1,
            "the assertion re-key ran on a v3 database — it rehashes every \
             assertion for a generation that changes no digest",
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE instr(key, '|') = 0",
            )
            .await,
            0,
            "the key migration did not run on a v3 database",
        );
        assert_eq!(
            read_user_version(&store.conn).await.unwrap(),
            PROV_EVIDENCE_VERSION,
        );
    }

    /// The re-key is a one-shot migration, not per-open work.
    ///
    /// `init_schema` runs on every `ProvenanceStore::open`, and `open` is
    /// called on hot paths (the agent loop and hooks re-open rather than hold
    /// a store). Without the version guard this would scan and SHA-256 every
    /// assertion on every open. Pinning it by observation: a row inserted with
    /// a pre-tenant id AFTER the database is stamped is left alone, which can
    /// only be true if the migration did not run again.
    #[tokio::test]
    async fn the_rekey_does_not_run_again_once_the_database_is_stamped() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant)
                   VALUES ('not-a-tenant-scoped-id', 'steel', 'has_phase', 'bcc', '[]',
                           'research', 0.7, 1, 'act', 'x.csv', 'agent', 't1')"#,
                (),
            )
            .await
            .unwrap();
        }

        ProvenanceStore::open(&db.path).await.unwrap();

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM prov_assertion WHERE id = 'not-a-tenant-scoped-id'",
                (),
            )
            .await
            .unwrap();
        let still_there = rows
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_integer().copied()))
            .unwrap_or(0);
        assert_eq!(
            still_there, 1,
            "the migration ran a second time — it is meant to be one-shot",
        );
    }

    /// The edge-key migration is one-shot too, and that is the expensive half.
    ///
    /// `migrate_keys_to_tenant_qualified` used to run outside the version
    /// guard, and its `emmo_edge.id` recompute had no already-migrated
    /// predicate, so it matched every tenanted edge on every `open()` and
    /// rewrote each row's PRIMARY KEY to the value it already held. `open()` is
    /// called once per agent turn and once per tool call, so that was a
    /// full-table write on the hot path, under `journal_mode=DELETE`.
    ///
    /// Pinned by observation, like the sibling test above: an edge whose id
    /// does NOT match the value the migration would compute is left alone after
    /// a reopen, which can only be true if the migration did not run again.
    #[tokio::test]
    async fn the_edge_key_migration_does_not_run_again_once_stamped() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        // `id` deliberately disagrees with tenant|source|rel|target, which is
        // exactly what the migration rewrites.
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO emmo_edge
                   (id, source_key, target_key, rel_type, predicate, confidence, tenant, props_json)
                   VALUES ('stale-edge-id', 't1|Matter:steel', 't1|Phase:bcc',
                           'has_phase', 'has_phase', 0.9, 't1', '{}')"#,
                (),
            )
            .await
            .unwrap();
        }

        ProvenanceStore::open(&db.path).await.unwrap();

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM emmo_edge WHERE id = 'stale-edge-id'",
                (),
            )
            .await
            .unwrap();
        let untouched = rows
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_integer().copied()))
            .unwrap_or(0);
        assert_eq!(
            untouched, 1,
            "the edge-key migration ran again after the database was stamped — \
             it rewrites every tenanted edge id, on every open",
        );
    }

    /// The positive direction: an UNSTAMPED database must actually migrate.
    ///
    /// Its sibling above only proves the migration is skipped once stamped —
    /// which a `run_key_migrations` short-circuited to `Ok(())` would also
    /// satisfy, and so would an inverted `IS NOT` predicate. This pins that the
    /// stale edge id is genuinely rewritten to the value `upsert_edge` would
    /// compute, so "does not run twice" cannot be achieved by never running.
    #[tokio::test]
    async fn an_unstamped_database_rewrites_a_stale_edge_id() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO emmo_edge
                   (id, source_key, target_key, rel_type, predicate, confidence, tenant, props_json)
                   VALUES ('stale-edge-id', 't1|Matter:steel', 't1|Phase:bcc',
                           'has_phase', 'has_phase', 0.9, 't1', '{}')"#,
                (),
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        ProvenanceStore::open(&db.path).await.unwrap();

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        let mut rows = conn
            .query("SELECT id FROM emmo_edge WHERE tenant = 't1'", ())
            .await
            .unwrap();
        let id = rows
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get_value(0).ok())
            .and_then(|v| v.as_text().map(|s| s.to_string()))
            .unwrap_or_default();
        while rows.next().await.unwrap().is_some() {}
        assert_eq!(
            id, "t1|t1|Matter:steel|has_phase|t1|Phase:bcc",
            "an unstamped database did not rewrite the stale edge id — the \
             migration never ran, or its predicate excludes rows it must match",
        );
    }

    /// An edge row with a NULL component must not be silently skipped forever.
    ///
    /// `source_key`, `rel_type` and `target_key` are nullable `TEXT`, and
    /// SQLite's `||` yields NULL if any operand is NULL. Under a plain `<>` the
    /// comparison is NULL — neither true nor false — so the row is excluded,
    /// the version is stamped, and it is never retried. It must be left intact
    /// rather than rewritten to NULL.
    #[tokio::test]
    async fn a_null_component_edge_is_left_intact_not_nulled() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"INSERT INTO emmo_edge
                   (id, source_key, target_key, rel_type, predicate, confidence, tenant, props_json)
                   VALUES ('keep-me', 't1|Matter:steel', 't1|Phase:bcc',
                           NULL, 'has_phase', 0.9, 't1', '{}')"#,
                (),
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        ProvenanceStore::open(&db.path).await.unwrap();

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        let mut rows = conn
            .query("SELECT COUNT(*) FROM emmo_edge WHERE id = 'keep-me'", ())
            .await
            .unwrap();
        let kept = rows
            .next()
            .await
            .unwrap()
            .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_integer().copied()))
            .unwrap_or(0);
        while rows.next().await.unwrap().is_some() {}
        assert_eq!(
            kept, 1,
            "the NULL-component edge was rewritten to a NULL id instead of \
             being left alone",
        );
    }

    /// Both migrations must survive being re-run, because the stamp is written
    /// only after BOTH have finished.
    ///
    /// A crash between them — or between the second and the stamp — leaves the
    /// version unstamped, so the next open re-enters and runs both again. The
    /// old code guarded and stamped the assertion re-key on its own; folding it
    /// under a shared guard replaced that protection with an assumption, so pin
    /// the assumption.
    #[tokio::test]
    async fn running_both_migrations_twice_is_stable() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let mut prov = test_prov();
        prov.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &prov,
            )
            .await
            .unwrap();
        // Compare the actual KEYS, not row counts. A non-idempotent migration
        // re-prefixes `local|` onto keys that already carry it, which corrupts
        // every key while leaving the counts identical — a count-only snapshot
        // passes against exactly the bug this test exists to catch. (Found by
        // mutation: stripping the `instr(key,'|') = 0` guard survived a
        // count-based version of this assertion.)
        async fn snapshot(s: &ProvenanceStore) -> Vec<String> {
            let mut out = Vec::new();
            for sql in [
                "SELECT key FROM emmo_entity ORDER BY key",
                "SELECT id FROM emmo_edge ORDER BY id",
                "SELECT id FROM prov_assertion ORDER BY id",
            ] {
                let mut rows = s.conn.query(sql, ()).await.unwrap();
                while let Some(row) = rows.next().await.unwrap() {
                    out.push(
                        row.get_value(0)
                            .ok()
                            .and_then(|v| v.as_text().map(|t| t.to_string()))
                            .unwrap_or_default(),
                    );
                }
            }
            out
        }
        let before = snapshot(&store).await;
        assert!(
            !before.is_empty(),
            "precondition: the fixture must have written rows to compare",
        );
        store
            .conn
            .execute("PRAGMA user_version = 0", ())
            .await
            .unwrap();
        drop(store);

        // Re-enter the migrations twice more on an already-migrated database.
        for _ in 0..2 {
            let s = ProvenanceStore::open(&db.path).await.unwrap();
            s.conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
            drop(s);
        }
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let after = snapshot(&store).await;

        assert_eq!(
            before, after,
            "re-running the migrations changed the stored keys — they are not \
             idempotent, so a crash before the stamp corrupts the store",
        );
    }

    /// A collision must skip one row, not brick the database.
    ///
    /// These statements write PRIMARY KEYs. As plain `UPDATE`s a collision
    /// propagated `Err` out of `init_schema`, which fails `open()` itself — so
    /// one unlucky row made every future open of that store fail, permanently.
    /// `UPDATE OR IGNORE` keeps the store openable.
    #[tokio::test]
    async fn a_key_collision_does_not_fail_the_open() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);

        // An unqualified row whose migrated key already exists: qualifying
        // 'Matter:steel' under tenant 't1' collides with the second row.
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            for (key, name) in [("Matter:steel", "steel"), ("t1|Matter:steel", "steel")] {
                conn.execute(
                    "INSERT INTO emmo_entity (key, name, label, entity_type, tenant, props_json, created_at)
                     VALUES (?1, ?2, 'Matter', 'Matter', 't1', '{}', '2026-01-01T00:00:00Z')",
                    [Value::Text(key.to_string()), Value::Text(name.to_string())],
                )
                .await
                .unwrap();
            }
            conn.execute("PRAGMA user_version = 0", ()).await.unwrap();
        }

        // The open must succeed despite the collision.
        ProvenanceStore::open(&db.path)
            .await
            .expect("a colliding legacy key must not fail open() — that bricks the store");

        // And a second open must still work.
        ProvenanceStore::open(&db.path).await.unwrap();
    }

    #[test]
    fn computed_evidence_inherits_the_worst_input() {
        assert_eq!(
            evidence_for_result(
                EvidenceSource::Execution,
                [EvidenceClass::ReferenceValidated, EvidenceClass::Research],
            ),
            EvidenceClass::Research,
            "executing a solver must not launder an orange boundary condition",
        );
        assert_eq!(
            evidence_for_result(
                EvidenceSource::CitedComputation,
                [EvidenceClass::ReferenceValidated],
            ),
            EvidenceClass::Screening,
            "a cited computation is yellow even with green inputs",
        );
        assert_eq!(
            evidence_for_result(EvidenceSource::LiteratureExtraction, []),
            EvidenceClass::Research,
            "literature extraction is orange regardless of confidence",
        );
    }

    #[tokio::test]
    async fn legacy_assertion_rows_migrate_to_empty_conditions_and_red() {
        let db = TempDb::new();
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"CREATE TABLE prov_assertion (
                    id TEXT PRIMARY KEY,
                    subject TEXT,
                    predicate TEXT,
                    object TEXT,
                    confidence REAL,
                    corroborations INTEGER,
                    activity_id TEXT,
                    source TEXT,
                    agent TEXT,
                    tenant TEXT
                )"#,
                (),
            )
            .await
            .unwrap();
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, confidence, corroborations,
                    activity_id, source, agent, tenant)
                   VALUES ('legacy', 'steel', 'has_phase', 'bcc', 0.7, 1,
                           'activity', 'legacy.csv', 'legacy-agent', 't1')"#,
                (),
            )
            .await
            .unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let recalled = store.recall_with_context("steel", "t1", 10).await.unwrap();
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].value, None);
        assert_eq!(recalled[0].unit, None);
        assert!(recalled[0].conditions.is_empty());
        assert_eq!(recalled[0].evidence_class, EvidenceClass::Indeterminate);
    }

    #[tokio::test]
    async fn conditions_distinguish_measurements_and_corroboration_cannot_upgrade_them() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        let measurement = |temperature, evidence_class| MaterialFact {
            subject: "test ceramic".into(),
            predicate: "has_measurement".into(),
            object: "thermal conductivity".into(),
            value: Some(22.0),
            unit: Some(QudtUnit::new("QUDT:W-PER-M-K").unwrap()),
            conditions: vec![MeasurementCondition {
                name: "temperature".into(),
                value: ConditionValue::Number(temperature),
                unit: Some(QudtUnit::new("QUDT:K").unwrap()),
            }],
            confidence: Some(0.9),
            kind: Some("measurement".into()),
            evidence_class,
        };

        let at_1200 = measurement(1200.0, EvidenceClass::Research);
        let at_1300 = measurement(1300.0, EvidenceClass::Research);
        store.write_fact(&at_1200, &prov).await.unwrap();
        store.write_fact(&at_1300, &prov).await.unwrap();
        // A later execution from a DIFFERENT origin source that agrees with
        // the 1200 K value raises confidence but must not upgrade the
        // literature-derived class. (Agreement from the SAME source would
        // change nothing at all — see `same_source_twice_does_not_corroborate`.)
        let mut replication = prov.clone();
        replication.source_entity_id = "doc:replication_run".into();
        replication.activity_id = "act_test_2".into();
        store
            .write_fact(
                &measurement(1200.0, EvidenceClass::ReferenceValidated),
                &replication,
            )
            .await
            .unwrap();

        let recalled = store
            .recall_with_context("thermal conductivity", "t1", 10)
            .await
            .unwrap();
        assert_eq!(
            recalled.len(),
            2,
            "different conditions are different facts"
        );
        assert!(
            recalled
                .iter()
                .all(|fact| fact.evidence_class == EvidenceClass::Research)
        );
        assert!(recalled.iter().any(|fact| {
            fact.conditions[0].value == ConditionValue::Number(1200.0) && fact.confidence > 0.9
        }));
    }

    // ── Entity vectors ───────────────────────────────────────────────────

    /// Deterministic 3-dim stand-in for the real ONNX backend.
    struct MockEmbed;

    fn mock_vec(text: &str) -> Vec<f32> {
        match text {
            "Ti-6Al-4V" => vec![1.0, 0.0, 0.0],
            "alpha" => vec![0.0, 1.0, 0.0],
            "Inconel 718" => vec![0.0, 0.0, 1.0],
            _ => vec![0.6, 0.6, 0.6],
        }
    }

    #[async_trait::async_trait]
    impl prism_embed::EmbedBackend for MockEmbed {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| mock_vec(t)).collect())
        }
        fn dimensions(&self) -> usize {
            3
        }
        fn id(&self) -> &str {
            "test:mock"
        }
    }

    /// An empty index is a legitimate empty ANSWER, not a failure — and it
    /// is the only condition allowed to produce `Ok(vec![])`.
    #[tokio::test]
    async fn semantic_search_entities_empty_store_is_empty() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(store.entity_embedding_count("t1").await.unwrap(), 0);
        let hits = store
            .semantic_search_entities(&[1.0, 0.0, 0.0], "t1", 5)
            .await
            .expect("an empty index must not be reported as a broken one");
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn entity_vectors_store_and_search_ranked() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let facts = vec![
            fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            fact("processing", "Inconel 718", "processed_by", "LPBF"),
        ];
        for f in &facts {
            store.write_fact(f, &prov).await.unwrap();
        }

        // 4 distinct names, each resolving to exactly one entity key.
        let stored = store
            .embed_and_store_entities(&facts, "t1", &MockEmbed)
            .await
            .unwrap();
        assert_eq!(stored, 4);
        assert_eq!(store.entity_embedding_count("t1").await.unwrap(), 4);

        // Query near the Ti-6Al-4V axis → ranked best-first.
        let hits = store
            .semantic_search_entities(&[1.0, 0.2, 0.0], "t1", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].0, "Ti-6Al-4V");
        assert!(hits[0].1 > 0.9);
        assert!(
            hits.windows(2).all(|w| w[0].1 >= w[1].1),
            "scores must be descending: {hits:?}"
        );

        // Limit is respected.
        let hits = store
            .semantic_search_entities(&[1.0, 0.2, 0.0], "t1", 1)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);

        // Tenant scoping: nothing leaks into another tenant.
        assert!(
            store
                .semantic_search_entities(&[1.0, 0.2, 0.0], "other", 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn same_name_under_two_labels_searches_once() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // "alpha" exists as both Phase (object) and Matter (subject).
        let facts = vec![
            fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            fact("phase", "alpha", "has_phase", "beta"),
        ];
        for f in &facts {
            store.write_fact(f, &prov).await.unwrap();
        }

        // One vector per entity ROW (both labels of "alpha" get one) …
        let stored = store
            .embed_and_store_entities(&facts, "t1", &MockEmbed)
            .await
            .unwrap();
        assert_eq!(
            stored, 4,
            "Matter:ti-6al-4v, Phase:alpha, Matter:alpha, Phase:beta"
        );

        // … but search reports the display name once.
        let hits = store
            .semantic_search_entities(&[0.0, 1.0, 0.0], "t1", 10)
            .await
            .unwrap();
        assert_eq!(hits[0].0, "alpha");
        assert_eq!(
            hits.iter().filter(|(name, _)| name == "alpha").count(),
            1,
            "same name under two labels must be deduped: {hits:?}"
        );
    }

    /// A dimension mismatch matches nothing, so it must be an error that
    /// names both dimensionalities — never an empty list, which the caller
    /// cannot tell apart from "the index is empty".
    #[tokio::test]
    async fn semantic_search_entities_errors_on_mismatched_dims() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
            .await
            .unwrap();

        store
            .store_entity_embedding(
                &entity_key("t1", "Matter", "Ti-6Al-4V"),
                "t1",
                &[1.0, 0.0, 0.0],
            )
            .await
            .unwrap();

        // 4-dim query cannot compare against the 3-dim vector.
        let err = store
            .semantic_search_entities(&[1.0, 0.0, 0.0, 0.0], "t1", 10)
            .await
            .expect_err("a dimension mismatch must be loud, not an empty list");
        let msg = format!("{err:#}");
        assert!(
            msg.contains('3') && msg.contains('4'),
            "error must name the stored and query dimensionality: {msg}"
        );

        // Matching dimensionality finds it.
        let hits = store
            .semantic_search_entities(&[1.0, 0.0, 0.0], "t1", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "Ti-6Al-4V");
    }

    /// Similarity must come back on the documented `[-1, 1]` scale after
    /// the conversion from Turso's `[0, 2]` cosine *distance*.
    #[tokio::test]
    async fn semantic_search_entities_similarity_scale() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
            .await
            .unwrap();
        store
            .store_entity_embedding(
                &entity_key("t1", "Matter", "Ti-6Al-4V"),
                "t1",
                &[1.0, 0.0, 0.0],
            )
            .await
            .unwrap();

        let same = store
            .semantic_search_entities(&[1.0, 0.0, 0.0], "t1", 1)
            .await
            .unwrap();
        assert!((same[0].1 - 1.0).abs() < 1e-5, "identical → +1: {same:?}");

        let orthogonal = store
            .semantic_search_entities(&[0.0, 1.0, 0.0], "t1", 1)
            .await
            .unwrap();
        assert!(
            orthogonal[0].1.abs() < 1e-5,
            "orthogonal → 0: {orthogonal:?}"
        );

        let opposite = store
            .semantic_search_entities(&[-1.0, 0.0, 0.0], "t1", 1)
            .await
            .unwrap();
        assert!(
            (opposite[0].1 + 1.0).abs() < 1e-5,
            "opposite → -1: {opposite:?}"
        );
    }

    /// Retrieval by MEANING with the real on-device model: a paraphrase
    /// that shares **no word at all** with any stored entity must still
    /// rank the metal-joining entities above the bread-making ones. A
    /// keyword index scores this query 0 against everything.
    ///
    /// `#[ignore]`d: needs the ~90 MB ONNX model in `~/.prism/models/embed/`.
    /// Run with `cargo test -p prism-provenance -- --ignored`.
    /// Not compiled on Intel macOS, which has no ONNX Runtime build and so
    /// no `NativeOnnx` (see `prism_embed`).
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[tokio::test]
    #[ignore = "downloads/uses the local ONNX embedding model"]
    async fn native_embeddings_retrieve_by_meaning_not_keywords() {
        use prism_embed::EmbedBackend as _;

        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let facts = vec![
            fact(
                "processing",
                "aluminium bicycle frame welding",
                "processed_by",
                "friction stir welding",
            ),
            fact(
                "phase",
                "sourdough bread fermentation",
                "has_phase",
                "wild yeast starter",
            ),
        ];
        for f in &facts {
            store.write_fact(f, &prov).await.unwrap();
        }
        let backend = prism_embed::NativeOnnx::new().expect("local ONNX model");
        store
            .embed_and_store_entities(&facts, "t1", &backend)
            .await
            .unwrap();

        let query = "joining two pieces of metal together without melting them";
        let query_vec = backend
            .embed(std::slice::from_ref(&query.to_string()))
            .await
            .unwrap()
            .remove(0);
        assert_eq!(query_vec.len(), 384, "BGE-small-en-v1.5 is 384-dimension");

        let hits = store
            .semantic_search_entities(&query_vec, "t1", 4)
            .await
            .unwrap();
        assert_eq!(hits.len(), 4, "all four entities are scored: {hits:?}");

        // The premise: this is retrieval by meaning, not by keyword. Assert
        // it rather than trusting the wording — no query word occurs in any
        // entity name, so lexical search has nothing to match on.
        let query_words: std::collections::HashSet<&str> = query.split_whitespace().collect();
        for (name, _) in &hits {
            for word in name.split_whitespace() {
                assert!(
                    !query_words.contains(word),
                    "'{word}' is shared with the query — the test would no longer \
                     distinguish semantic retrieval from keyword matching"
                );
            }
        }

        let metal_joining = ["aluminium bicycle frame welding", "friction stir welding"];
        assert!(
            metal_joining.contains(&hits[0].0.as_str())
                && metal_joining.contains(&hits[1].0.as_str()),
            "both metal-joining entities must outrank both bread-making ones: {hits:?}"
        );
        assert!(
            hits[1].1 > hits[2].1,
            "the two domains must be separated, not tied: {hits:?}"
        );
        assert!(
            hits.iter().all(|(_, s)| (-1.0..=1.0).contains(s)),
            "similarities must stay in [-1, 1]: {hits:?}"
        );
    }

    // ── Tenant isolation ───────────────────────────────────────────────

    /// `crates/mesh/src/sync.rs` writes every peer-supplied entity under
    /// the `"mesh"` tenant precisely "so peer-synced data never blends
    /// with locally [ingested data]". A mesh peer chooses the entity
    /// `name` verbatim, so if the entity primary key is not
    /// tenant-qualified, naming an entity the user already has hands the
    /// peer that row: the ON CONFLICT branch reassigns `tenant`, and
    /// every local read filters `WHERE tenant = 'local'`, so the user's
    /// own knowledge silently disappears.
    #[tokio::test]
    async fn peer_tenant_cannot_capture_a_local_entity() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        // The user ingests a paper locally.
        let mut local = test_prov();
        local.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &local,
            )
            .await
            .unwrap();

        assert!(
            store
                .graph_search("Ti-6Al-4V", "local", 10)
                .await
                .unwrap()
                .iter()
                .any(|n| n.name == "Ti-6Al-4V"),
            "precondition: the local entity must exist before the peer syncs"
        );

        // A subscribed mesh peer returns a dataset row whose `name` is
        // the same material, spelled its way. This is peer-controlled
        // input: sync.rs takes `row["name"]` straight off the wire.
        let mut mesh = test_prov();
        mesh.tenant = "mesh".into();
        store
            .write_fact(
                &LocalFact {
                    subject: "TI-6AL-4V".into(),
                    predicate: "SYNCED_FROM".into(),
                    object: "peer-dataset".into(),
                    value: None,
                    unit: None,
                    confidence: None,
                    kind: None,
                },
                &mesh,
            )
            .await
            .unwrap();

        // The user's own entity must still be theirs.
        let local_hits = store.graph_search("Ti-6Al-4V", "local", 10).await.unwrap();
        assert!(
            local_hits.iter().any(|n| n.name == "Ti-6Al-4V"),
            "a mesh peer captured the local tenant's entity — the user's own \
             ingested knowledge vanished from every `tenant = 'local'` read"
        );
    }

    /// A database written before entity keys carried the tenant must be
    /// rewritten on open, or the next re-ingest writes a SECOND row for
    /// the same entity and edges split across two key spaces.
    #[tokio::test]
    async fn legacy_unqualified_keys_migrate_on_open() {
        let db = TempDb::new();
        {
            let store = ProvenanceStore::open(&db.path).await.unwrap();
            let mut prov = test_prov();
            prov.tenant = "local".into();
            store
                .write_fact(
                    &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                    &prov,
                )
                .await
                .unwrap();
            // Rewind to the pre-fix on-disk shape.
            for sql in [
                "UPDATE emmo_entity SET key = replace(key, 'local|', '')",
                "UPDATE emmo_edge SET source_key = replace(source_key, 'local|', ''),
                     target_key = replace(target_key, 'local|', ''),
                     id = tenant || '|' || replace(source_key, 'local|', '') || '|'
                          || rel_type || '|' || replace(target_key, 'local|', '')",
            ] {
                store.conn.execute(sql, ()).await.unwrap();
            }
            // Rewind the schema stamp to 3, not 0 — this is the real upgrade
            // state, and it is reachable in the wild. The old code let
            // `rekey_assertions_by_tenant` stamp v3 and then ran
            // `migrate_keys_to_tenant_qualified` unguarded LATER in
            // `init_schema`, so any process that died in between left a v3
            // database carrying unqualified EMMO keys on disk.
            //
            // Rewinding to 0 would still enter the migration and pass, but it
            // would stop covering the v3 -> v4 path entirely: an
            // implementation that left the stamp constant at 3, or skipped the
            // key migration for a v3 database, would go undetected. At 3 this
            // test fails for both.
            store
                .conn
                .execute("PRAGMA user_version = 3", ())
                .await
                .unwrap();
            assert_eq!(
                count(
                    &store,
                    "SELECT COUNT(*) FROM emmo_entity WHERE instr(key, '|') = 0",
                )
                .await,
                2,
                "precondition: the legacy shape must have unqualified keys"
            );
        }

        // Reopening runs init_schema, which must migrate.
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE instr(key, '|') = 0",
            )
            .await,
            0,
            "legacy entity keys were not tenant-qualified on open"
        );

        // Re-ingesting the same fact must merge, not duplicate.
        let mut prov = test_prov();
        prov.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &prov,
            )
            .await
            .unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity").await,
            2,
            "re-ingest duplicated entities across the key-format change"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_edge").await,
            1,
            "re-ingest duplicated the edge across the key-format change"
        );
    }

    /// The same name under two tenants must be two rows, each keeping its
    /// own owner. `emmo_embedding` is keyed by the entity key, so once the
    /// entity key separates, entity vectors separate with it.
    #[tokio::test]
    async fn same_name_under_two_tenants_stays_two_owned_rows() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        let mut local = test_prov();
        local.tenant = "local".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "alpha-beta"),
                &local,
            )
            .await
            .unwrap();
        let mut mesh = test_prov();
        mesh.tenant = "mesh".into();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "beta"), &mesh)
            .await
            .unwrap();

        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE label = 'Matter' \
                 AND tenant = 'local'",
            )
            .await,
            1,
            "the local tenant lost its Matter row to the peer"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_entity WHERE label = 'Matter' \
                 AND tenant = 'mesh'",
            )
            .await,
            1,
            "the peer tenant has no Matter row of its own"
        );
    }

    // ── Union reads (mesh-aware read scope) ────────────────────────────

    /// Writes one local fact and one peer fact sharing the subject name
    /// "Ti-6Al-4V": local says has_phase → alpha, mesh:node-a says
    /// has_phase → beta.
    async fn store_with_local_and_peer_fact(store: &ProvenanceStore) {
        let mut local = test_prov();
        local.tenant = "local".into();
        local.source_entity_id = "doc:local".into();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &local)
            .await
            .unwrap();
        let mut peer = test_prov();
        peer.tenant = "mesh:node-a".into();
        peer.activity_id = "act_peer".into();
        peer.source_entity_id = "doc:peer".into();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "beta"), &peer)
            .await
            .unwrap();
    }

    /// THE SHADOWING TRAP, graph side: under a union read, the same
    /// display name owned by two tenants is TWO rows, each naming its
    /// owner — neither may silently disappear, in search, in recall, or
    /// in the neighbor traversal's dedupe.
    #[tokio::test]
    async fn union_read_shows_local_and_peer_rows_of_the_same_name_attributed() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        store_with_local_and_peer_fact(&store).await;
        let scope = ["local", "mesh:node-a"];

        // graph_search: one row per owner.
        let hits = store
            .graph_search_scoped("Ti-6Al-4V", &scope, 10)
            .await
            .unwrap();
        let mut owners: Vec<&str> = hits
            .iter()
            .filter(|n| n.name == "Ti-6Al-4V")
            .map(|n| n.tenant.as_str())
            .collect();
        owners.sort_unstable();
        assert_eq!(
            owners,
            ["local", "mesh:node-a"],
            "one tenant's entity shadowed the other's: {hits:?}"
        );

        // recall: both facts arrive, each attributed to its owner.
        let facts = store
            .recall_with_context_scoped("Ti-6Al-4V", &scope, 10)
            .await
            .unwrap();
        assert!(
            facts
                .iter()
                .any(|f| f.object == "alpha" && f.tenant == "local"),
            "local fact lost or misattributed: {facts:?}"
        );
        assert!(
            facts
                .iter()
                .any(|f| f.object == "beta" && f.tenant == "mesh:node-a"),
            "peer fact lost or misattributed: {facts:?}"
        );

        // get_neighbors: the tenant-qualified dedupe keeps both same-named
        // centers, and each edge names the tenant it belongs to.
        let traversal = store
            .get_neighbors_scoped("Ti-6Al-4V", None, &scope, 10)
            .await
            .unwrap();
        assert!(
            traversal
                .nodes
                .iter()
                .any(|n| n.name == "Ti-6Al-4V" && n.tenant == "local"),
            "local center lost in the union traversal: {:?}",
            traversal.nodes
        );
        assert!(
            traversal
                .nodes
                .iter()
                .any(|n| n.name == "Ti-6Al-4V" && n.tenant == "mesh:node-a"),
            "peer center swallowed by the node dedupe: {:?}",
            traversal.nodes
        );
        assert!(
            traversal
                .edges
                .iter()
                .any(|e| e.target == "alpha" && e.tenant == "local"),
            "local edge lost or unattributed: {:?}",
            traversal.edges
        );
        assert!(
            traversal
                .edges
                .iter()
                .any(|e| e.target == "beta" && e.tenant == "mesh:node-a"),
            "peer edge lost or unattributed: {:?}",
            traversal.edges
        );

        // And the single-tenant reads still see ONLY their own tenant —
        // the union is opt-in per call, isolation is untouched.
        let local_only = store.graph_search("Ti-6Al-4V", "local", 10).await.unwrap();
        assert!(local_only.iter().all(|n| n.tenant == "local"));
    }

    /// THE SHADOWING TRAP, semantic side: `GROUP BY name` alone would let
    /// a peer entity named like a local one collapse into a single row —
    /// one of them disappearing with no signal. Grouped by (tenant, name),
    /// both hits return, attributed.
    #[tokio::test]
    async fn semantic_union_returns_same_name_from_both_tenants() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        store_with_local_and_peer_fact(&store).await;
        store
            .store_entity_embedding(
                &entity_key("local", "Matter", "Ti-6Al-4V"),
                "local",
                &[1.0, 0.0, 0.0],
            )
            .await
            .unwrap();
        store
            .store_entity_embedding(
                &entity_key("mesh:node-a", "Matter", "Ti-6Al-4V"),
                "mesh:node-a",
                &[0.9, 0.1, 0.0],
            )
            .await
            .unwrap();

        let hits = store
            .semantic_search_entities_scoped(&[1.0, 0.0, 0.0], &["local", "mesh:node-a"], 10)
            .await
            .unwrap();
        let mut owners: Vec<&str> = hits
            .iter()
            .filter(|hit| hit.name == "Ti-6Al-4V")
            .map(|hit| hit.tenant.as_str())
            .collect();
        owners.sort_unstable();
        assert_eq!(
            owners,
            ["local", "mesh:node-a"],
            "a name-only GROUP BY shadowed one tenant's entity: {hits:?}"
        );
    }

    /// The default read scope is DISCOVERED from the store: local plus
    /// whatever mesh tenants exist — the legacy shared `"mesh"` and the
    /// per-peer `"mesh:{node_id}"` shape both — and never a foreign
    /// non-mesh tenant.
    #[tokio::test]
    async fn default_read_tenants_is_local_plus_discovered_mesh_tenants() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        // Empty store: just local.
        assert_eq!(store.default_read_tenants().await.unwrap(), ["local"]);

        for tenant in ["local", "mesh", "mesh:node-a", "t1"] {
            let mut prov = test_prov();
            prov.tenant = tenant.into();
            prov.activity_id = format!("act_{tenant}");
            store
                .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "alpha"), &prov)
                .await
                .unwrap();
        }
        assert_eq!(
            store.default_read_tenants().await.unwrap(),
            ["local", "mesh", "mesh:node-a"],
            "default scope must include every mesh tenant and no foreign tenant"
        );
    }

    /// Ontology-composed local tenants (`local@{ontology id}` — the MatKG
    /// reference graph is `local@matkg`) are part of the DEFAULT read
    /// scope, so loaded reference knowledge is visible to `prism query`
    /// and the agent's query tools without a flag — but they are NOT mesh
    /// peers: the peer-echo laundering tripwire must never report
    /// reference data the user chose to load as a peer echo.
    #[tokio::test]
    async fn ontology_tenants_join_default_scope_but_are_not_peer_echoes() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        for tenant in ["local@matkg", "mesh:node-a"] {
            let mut prov = test_prov();
            prov.tenant = tenant.into();
            prov.activity_id = format!("act_{tenant}");
            store
                .write_fact(&fact("phase", "LiFePO4", "COOCCURS_WITH", "Olivine"), &prov)
                .await
                .unwrap();
        }

        assert_eq!(
            store.default_read_tenants().await.unwrap(),
            ["local", "local@matkg", "mesh:node-a"],
            "the ontology tenant must be discovered into the default scope"
        );
        assert_eq!(
            store
                .peer_tenants_asserting("LiFePO4", "COOCCURS_WITH", "Olivine")
                .await
                .unwrap(),
            ["mesh:node-a"],
            "only MESH tenants are peers; local@matkg holding the triple is \
             reference data, not an echo"
        );
    }

    /// THE LAUNDERING LOOP this store cannot prevent: read a peer fact,
    /// write it back under "local". At the store boundary that write is
    /// indistinguishable from honest independent corroboration (a real
    /// second source stating the same fact), so the write LANDS — and the
    /// read-side tripwire must therefore detect the echo so callers can
    /// be loud about it. This test pins both halves honestly.
    #[tokio::test]
    async fn peer_fact_written_back_as_local_is_detected_as_an_echo() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        // The peer's knowledge arrives under its mesh tenant.
        let mut peer = test_prov();
        peer.tenant = "mesh:node-a".into();
        peer.source_entity_id = "doc:peer".into();
        store
            .write_fact(&fact("phase", "Ti-6Al-4V", "has_phase", "beta"), &peer)
            .await
            .unwrap();

        // The tripwire sees the echo — including under canonicalization
        // (case/whitespace variants are the SAME triple identity).
        assert_eq!(
            store
                .peer_tenants_asserting("Ti-6Al-4V", "has_phase", "beta")
                .await
                .unwrap(),
            ["mesh:node-a"]
        );
        assert_eq!(
            store
                .peer_tenants_asserting("  TI-6AL-4V ", "has_phase", "BETA")
                .await
                .unwrap(),
            ["mesh:node-a"],
            "canonical variants of the triple must still trip the wire"
        );
        // A triple nobody synced stays clean.
        assert!(
            store
                .peer_tenants_asserting("Inconel 718", "has_phase", "gamma")
                .await
                .unwrap()
                .is_empty()
        );

        // The write-back itself DOES land as a local assertion — that is
        // the open hazard, pinned here so it cannot be silently forgotten:
        // if a future pass makes the store reject or divert such writes,
        // this assertion should be UPDATED to pin the new behaviour.
        let mut relabelled = test_prov();
        relabelled.tenant = "local".into();
        relabelled.source_entity_id = "doc:peer".into();
        store
            .write_fact(
                &fact("phase", "Ti-6Al-4V", "has_phase", "beta"),
                &relabelled,
            )
            .await
            .unwrap();
        let landed = store
            .recall_with_context("beta", "local", 10)
            .await
            .unwrap();
        assert_eq!(
            landed.len(),
            1,
            "write-back landed differently than documented — update the hazard notes"
        );
        // Detection still stands after the laundering write.
        assert_eq!(
            store
                .peer_tenants_asserting("Ti-6Al-4V", "has_phase", "beta")
                .await
                .unwrap(),
            ["mesh:node-a"]
        );
    }

    /// `get_neighbors`' canonical fallback parses the entity key
    /// ("{tenant}|{label}:{canonical}"). A per-peer tenant is
    /// `mesh:{node_id}` — it CONTAINS a colon — so splitting the whole
    /// key on its first `:` cuts inside the tenant and silently fails to
    /// resolve every peer entity exactly when the query casing differs
    /// from the stored name (the one case the fallback exists for).
    /// Found by adversarial review; this pins the tenant-aware parse.
    #[tokio::test]
    async fn canonical_fallback_resolves_entities_under_colon_bearing_tenants() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        store_with_local_and_peer_fact(&store).await;

        // "TI-6AL-4V" matches no stored display name exactly, forcing the
        // canonical fallback for BOTH tenants.
        let traversal = store
            .get_neighbors_scoped("TI-6AL-4V", None, &["local", "mesh:node-a"], 10)
            .await
            .unwrap();
        assert!(
            traversal
                .nodes
                .iter()
                .any(|n| n.name == "Ti-6Al-4V" && n.tenant == "local"),
            "local center must resolve via the canonical fallback: {:?}",
            traversal.nodes
        );
        assert!(
            traversal
                .nodes
                .iter()
                .any(|n| n.name == "Ti-6Al-4V" && n.tenant == "mesh:node-a"),
            "a mesh:{{node_id}} tenant's entity silently failed canonical \
             resolution — the tenant's colon was parsed as the label separator: {:?}",
            traversal.nodes
        );
        assert!(
            traversal
                .edges
                .iter()
                .any(|e| e.target == "beta" && e.tenant == "mesh:node-a"),
            "peer edges must come with the fallback-resolved center: {:?}",
            traversal.edges
        );
    }

    /// The scoped honesty contract under a MIXED index: when one tenant
    /// in scope holds vectors of a different width, the whole union must
    /// refuse loudly AND name which tenant holds what — never shrink to
    /// the matching tenants, and never blame the wrong index.
    #[tokio::test]
    async fn semantic_union_dimension_mismatch_names_each_tenant() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        store_with_local_and_peer_fact(&store).await;
        store
            .store_entity_embedding(
                &entity_key("local", "Matter", "Ti-6Al-4V"),
                "local",
                &[1.0, 0.0, 0.0],
            )
            .await
            .unwrap();
        // The peer synced vectors from a different embedding backend.
        store
            .store_entity_embedding(
                &entity_key("mesh:node-a", "Matter", "Ti-6Al-4V"),
                "mesh:node-a",
                &[1.0, 0.0, 0.0, 0.0],
            )
            .await
            .unwrap();

        let err = store
            .semantic_search_entities_scoped(&[1.0, 0.0, 0.0], &["local", "mesh:node-a"], 10)
            .await
            .expect_err("a mismatched tenant must fail the union loudly, not shrink it");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("'local' holds 3-dimension"),
            "error must name the local tenant's width: {msg}"
        );
        assert!(
            msg.contains("'mesh:node-a' holds 4-dimension"),
            "error must name the offending tenant's width: {msg}"
        );

        // Scoped down to the matching tenant, search works again — the
        // remedy the error message names.
        let hits = store
            .semantic_search_entities_scoped(&[1.0, 0.0, 0.0], &["local"], 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tenant, "local");
    }
}
