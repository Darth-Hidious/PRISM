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

/// The exact non-empty unit term selected by an extraction source.
///
/// A term may be an ontology IRI, a prefixed name, or the spelling printed in
/// a paper. Its vocabulary and interpretation belong to the active ontology
/// and the reading model; this storage type deliberately does not embed a
/// QUDT-only gate or a Rust-maintained spelling table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UnitTerm(String);

impl UnitTerm {
    pub fn new(term: impl Into<String>) -> Result<Self> {
        let term = term.into();
        if term.trim().is_empty() {
            anyhow::bail!("unit term must not be empty");
        }
        Ok(Self(term))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for UnitTerm {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for UnitTerm {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let term = String::deserialize(deserializer)?;
        Self::new(term).map_err(serde::de::Error::custom)
    }
}

/// Compatibility name for callers that still use the former type name.
/// Construction follows [`UnitTerm`]'s vocabulary-neutral contract.
pub type QudtUnit = UnitTerm;

/// The sign domain an ONTOLOGY declares for a quantity kind.
///
/// Whether a quantity can be negative is a property of the quantity kind
/// itself (a boolean/annotation on the quantity class or its dimensional
/// parent), so it belongs to the active ontology, never to Rust: PRISM is a
/// harness with pluggable ontologies, and a sign table compiled into the
/// matcher would be one domain's physics forced onto every customer. The
/// matcher reads this value at grounding time; the default is silence, and a
/// silent ontology leaves the sign check INERT — it never applies by
/// inference from the quantity's name, in any language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum QuantitySignDomain {
    /// The ontology declares nothing about this quantity's sign. This is the
    /// only honest answer when the ontology has no such annotation: the sign
    /// check does not apply, and no guess replaces it.
    #[default]
    Unspecified,
    /// The quantity is non-negative by definition; a negative claim against
    /// it is nonsense under every notation.
    NonNegative,
    /// The quantity is legitimately signed; negative claims are in-domain.
    Signed,
}

/// A numerical or categorical boundary-condition value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionValue {
    Number(f64),
    Text(String),
}

/// One solver-consumable measurement condition. A supplied unit term is
/// preserved exactly; absence is represented as `unit: null` without the
/// store inferring what the condition should require.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementCondition {
    pub name: String,
    pub value: ConditionValue,
    #[serde(default)]
    pub unit: Option<UnitTerm>,
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

/// How thoroughly the deterministic ingest checks verified one stored fact
/// against its source document.
///
/// ANNOTATE, DON'T REFUSE. These used to be refusal reasons: a fact that
/// failed a grounding check was dropped, and a model that fabricated nothing
/// stored nothing (measured: glm-5.2 extracted 74 facts from one LPBF paper,
/// fabricated 0, stored 0 — 63 refused as "subject not named" because it
/// wrote a MORE precise subject than the paper's spelling). The checks were
/// good signal calibrated into a bad gate. Now the fact is STORED and the
/// check's verdict rides with it as this status; reads default to the
/// trusted subset ([`Self::is_trusted`]) so nothing unverified is promoted,
/// and everything unverified stays findable for later review.
///
/// This is a SEPARATE axis from [`EvidenceClass`]: the class says how the
/// knowledge was produced (literature vs execution — monotone ceiling,
/// worst-wins), the status says how far deterministic checks verified this
/// particular extraction against its source. A weak status never upgrades
/// the class, and the class ceiling still holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Every deterministic check passed: the subject is named, the value,
    /// unit and conditions are carried by one supporting span (numeric
    /// facts), and semantic review affirmed the polarity (value-less facts).
    Grounded,
    /// The model's unit spelling resolved to nothing, so the unit was taken
    /// from the DOCUMENT (printed adjacent to the value), and every check
    /// then passed with that unit. Trusted: the unit is page-attested.
    UnitFromPage,
    /// The reading agent proposed this fact together with an exact,
    /// bounds-checked citation it had just read, and NO deterministic check
    /// compared the fact's value, unit, subject or conditions to that span.
    /// Trusted-but-unverified: the fresh paper path mints exactly this —
    /// re-running the retired lexical gates there would re-install the
    /// muzzle that was measured and removed (~44% of quarantines came from
    /// checks that could not pass). Because the span was never checked, no
    /// judgement about span-support was rendered, so a later re-read
    /// ([`crate::reverify`]-style affirmation from the exact cited lines)
    /// is a FIRST ask, not a re-roll — this is the population re-verification
    /// targets.
    CitedByReader,
    /// The document never names the fact's subject verbatim. Not proof of
    /// fabrication — the measured failure mode is a subject MORE precise
    /// than the paper's spelling ("LPBF Ti-6Al-4V fatigue bar (as-built)"
    /// for a paper that says "Ti-6Al-4V").
    SubjectNotVerbatim,
    /// No single span carries the value together with its subject, unit and
    /// conditions. The strongest fabrication signal a deterministic check
    /// renders.
    ValueNotInSource,
    /// A producer supplied an explicitly unusable unit term, such as a blank
    /// string. Mere absence is neutral and does not imply this status.
    /// The fact is stored so the check remains an annotation, not a drop.
    UnitUnresolved,
    /// Too few independent extraction passes produced this fact (see the
    /// ingest sampling policy). A statement about the MODEL's consistency;
    /// the document was never consulted.
    ///
    /// LEGACY — NO LONGER PRODUCED, AND MUST NOT BE DELETED. Ingest moved to
    /// fail-to-promote: a fact short of the agreement bar keeps its
    /// reader-cited status and records the shortfall in
    /// `verification_reason`, because demoting it ranked an uncorroborated
    /// fact BELOW where `--samples 1` would have left it and hid 21,109 of
    /// one corpus's 21,218 facts from the default read.
    ///
    /// The variant stays because stored data still carries it: those rows are
    /// readable, and `reverify list --status sample_disagreement` reaches them
    /// through [`Self::ALL`]. Removing it would orphan every such row.
    SampleDisagreement,
    /// Nothing beyond the model's own assertion vouches for this fact: the
    /// review was skipped by policy, the reviewer rendered no verdict, or
    /// the shape was self-contradictory (a value-less fact carrying a unit).
    ModelAsserted,
    /// Semantic review examined the fact and abstained.
    ReviewUncertain,
    /// Semantic review examined the fact and found the source DENIES it.
    /// The least trusted status: stored so the denial is auditable, never
    /// shown by default.
    ReviewDenied,
}

impl VerificationStatus {
    /// Every status, once, for schema generation and parsing.
    pub const ALL: [Self; 10] = [
        Self::Grounded,
        Self::UnitFromPage,
        Self::CitedByReader,
        Self::SubjectNotVerbatim,
        Self::ValueNotInSource,
        Self::UnitUnresolved,
        Self::SampleDisagreement,
        Self::ModelAsserted,
        Self::ReviewUncertain,
        Self::ReviewDenied,
    ];

    /// Stable identifier — the stored column value and the serde form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grounded => "grounded",
            Self::UnitFromPage => "unit_from_page",
            Self::CitedByReader => "cited_by_reader",
            Self::SubjectNotVerbatim => "subject_not_verbatim",
            Self::ValueNotInSource => "value_not_in_source",
            Self::UnitUnresolved => "unit_unresolved",
            Self::SampleDisagreement => "sample_disagreement",
            Self::ModelAsserted => "model_asserted",
            Self::ReviewUncertain => "review_uncertain",
            Self::ReviewDenied => "review_denied",
        }
    }

    /// Inverse of [`Self::as_str`]. `None` for anything this enum does not
    /// declare — including the stored NULL, which means "written before
    /// verification statuses existed, or by a path that runs its own guards".
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == text)
    }

    /// Trust rank, higher = more verified. Total order, used two ways:
    /// WORST-wins when one sighting trips several checks (the most
    /// disqualifying defect names the fact), BEST-wins when several
    /// sightings of one assertion disagree (a grounding witness in any
    /// source is a real witness — a later sloppy extraction must not
    /// un-ground a fact a document supports, and the upgrade can only come
    /// from a deterministic pass over a real document, so it launders
    /// nothing).
    #[must_use]
    pub fn rank(self) -> i64 {
        match self {
            Self::ReviewDenied => 0,
            Self::ValueNotInSource => 1,
            Self::UnitUnresolved => 2,
            Self::SubjectNotVerbatim => 3,
            Self::ReviewUncertain => 4,
            Self::SampleDisagreement => 5,
            Self::ModelAsserted => 6,
            Self::CitedByReader => 7,
            Self::UnitFromPage => 8,
            Self::Grounded => 9,
        }
    }

    /// Whether default (user-facing) reads include this status: the statuses
    /// whose every deterministic check passed against the source, plus
    /// [`Self::CitedByReader`] — trusted-but-unverified, the reading agent's
    /// cited proposal with no span check run. Everything else is present and
    /// findable, never promoted.
    #[must_use]
    pub fn is_trusted(self) -> bool {
        matches!(
            self,
            Self::Grounded | Self::UnitFromPage | Self::CitedByReader
        )
    }

    /// Whether a judgement about the fact was actually RENDERED — the
    /// anti-ratchet rule, carried over from the refusal-era
    /// `RejectionClass::judgement_was_rendered`: a reviewer that re-asks
    /// where an answer already exists keeps every "yes" and re-rolls every
    /// "no", converting sampling noise into acceptances. A follow-on model
    /// pass over weak-status facts may re-ask ONLY where this is `false`.
    #[must_use]
    pub fn judgement_was_rendered(self) -> bool {
        match self {
            Self::Grounded
            | Self::UnitFromPage
            | Self::SubjectNotVerbatim
            | Self::ValueNotInSource
            | Self::ReviewUncertain
            | Self::ReviewDenied => true,
            Self::CitedByReader
            | Self::UnitUnresolved
            | Self::SampleDisagreement
            | Self::ModelAsserted => false,
        }
    }
}

/// Which verification statuses a fact read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerificationFilter {
    /// The DEFAULT for anything user-facing: statuses that passed every
    /// check ([`VerificationStatus::is_trusted`]) plus rows with no recorded
    /// status (written before statuses existed, or by a path with its own
    /// guard regime — hiding those would silently vanish existing data).
    #[default]
    Trusted,
    /// Everything, whatever its status. The review surface.
    Any,
    /// Exactly one status — how a reviewer pulls, say, every
    /// `subject_not_verbatim` fact.
    Status(VerificationStatus),
}

impl VerificationFilter {
    /// The SQL predicate this filter puts on a `prov_assertion` read.
    /// Status spellings come from [`VerificationStatus::as_str`] — fixed
    /// identifiers, never caller input.
    fn sql_clause(self, column: &str) -> String {
        match self {
            Self::Trusted => {
                let trusted: Vec<String> = VerificationStatus::ALL
                    .into_iter()
                    .filter(|status| status.is_trusted())
                    .map(|status| format!("'{}'", status.as_str()))
                    .collect();
                format!("({column} IS NULL OR {column} IN ({}))", trusted.join(", "))
            }
            Self::Any => "1=1".to_string(),
            Self::Status(status) => format!("{column} = '{}'", status.as_str()),
        }
    }
}

/// SQL CASE expression mapping a stored status column to its trust rank
/// ([`VerificationStatus::rank`]); NULL and unknown strings rank below
/// everything, so any recorded status replaces them. Generated from the one
/// enum so the SQL can never drift from the Rust ordering.
fn verification_rank_case(column: &str) -> String {
    let arms: Vec<String> = VerificationStatus::ALL
        .into_iter()
        .map(|status| format!("WHEN '{}' THEN {}", status.as_str(), status.rank()))
        .collect();
    format!("CASE {column} {} ELSE -1 END", arms.join(" "))
}

/// New extraction/storage contract. The legacy [`LocalFact`] remains source
/// compatible for CLI/server/mesh callers, while new extraction uses this
/// type to preserve conditions and any unit terms it supplied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<UnitTerm>,
    #[serde(default)]
    pub conditions: Vec<MeasurementCondition>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub evidence_class: EvidenceClass,
    /// How far the deterministic ingest checks verified this fact against
    /// its source. `None` means no verification was recorded — a write
    /// from before statuses existed, or from a path with its own guard
    /// regime (claims, tabular, mesh relay) — and reads treat that as
    /// visible-by-default, not as trusted-by-verification.
    #[serde(default)]
    pub verification: Option<VerificationStatus>,
    /// Why the status is what it is, in the check's own words — the reason
    /// that used to die in a drop report. `None` for facts whose status
    /// needs no explanation (a clean `grounded`).
    #[serde(default)]
    pub verification_reason: Option<String>,
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

/// Optional endpoint identities selected by an ontology-reading agent.
///
/// Paper facts may classify either endpoint independently. Missing bindings
/// remain generic; present bindings retain their canonical class IRI. This
/// path always writes a generic graph edge so a model-supplied legacy `kind`
/// hint cannot silently select a built-in domain shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct OntologyBoundFactNodes<'a> {
    pub subject: Option<ClassifiedNode<'a>>,
    pub object: Option<ClassifiedNode<'a>>,
}

/// Immutable ontology artifact identity used to classify one assertion.
///
/// @req REQ-OWL-1.5 - Record ontology version IRI and artifact SHA-256.
#[derive(Debug, Clone, Copy)]
pub struct OntologyClassification<'a> {
    pub version_iri: &'a str,
    pub artifact_sha256: &'a str,
}

/// A validated, source-revision-specific witness for one extracted fact.
///
/// Line numbers are one-based and inclusive. The evidence span is kept
/// byte-for-byte as supplied so retrieval can re-open the source, read those
/// exact lines, and compare them with the witness that was originally stored.
/// `source_revision_id` is the lowercase hexadecimal SHA-256 of the complete
/// source text; it identifies which revision the line coordinates address.
///
/// Fields are private so every value persisted through the public API has
/// passed [`Self::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceCitation {
    line_start: i64,
    line_end: i64,
    evidence_span: String,
    source_revision_id: String,
    locator_json: Option<String>,
}

impl SourceCitation {
    /// Validate and construct an exact source witness.
    ///
    /// `locator_json`, when present, must be valid JSON. Its original text is
    /// retained rather than normalized so source-specific locator details are
    /// not rewritten by the provenance layer.
    pub fn new(
        line_start: i64,
        line_end: i64,
        evidence_span: impl Into<String>,
        source_revision_id: impl Into<String>,
        locator_json: Option<String>,
    ) -> Result<Self> {
        if line_start < 1 {
            bail!("citation line_start must be one-based, got {line_start}");
        }
        if line_end < line_start {
            bail!(
                "citation line_end must be inclusive and no earlier than line_start \
                 ({line_start}), got {line_end}"
            );
        }

        let evidence_span = evidence_span.into();
        if evidence_span.trim().is_empty() {
            bail!("citation evidence_span cannot be empty");
        }

        let source_revision_id = source_revision_id.into();
        if source_revision_id.len() != 64
            || source_revision_id
                .bytes()
                .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            bail!("citation source_revision_id must be a lowercase hexadecimal SHA-256");
        }

        if let Some(locator) = locator_json.as_deref() {
            serde_json::from_str::<serde_json::Value>(locator)
                .map_err(|error| anyhow::anyhow!("citation locator_json is invalid: {error}"))?;
        }

        Ok(Self {
            line_start,
            line_end,
            evidence_span,
            source_revision_id,
            locator_json,
        })
    }

    #[must_use]
    pub fn line_start(&self) -> i64 {
        self.line_start
    }

    #[must_use]
    pub fn line_end(&self) -> i64 {
        self.line_end
    }

    #[must_use]
    pub fn evidence_span(&self) -> &str {
        &self.evidence_span
    }

    #[must_use]
    pub fn source_revision_id(&self) -> &str {
        &self.source_revision_id
    }

    #[must_use]
    pub fn locator_json(&self) -> Option<&str> {
        self.locator_json.as_deref()
    }
}

/// Common storage view implemented by both the additive conditioned contract
/// and the source-compatible legacy fact.
pub trait FactPayload {
    fn to_local_fact(&self) -> LocalFact;
    fn conditions(&self) -> &[MeasurementCondition];
    fn evidence_class(&self) -> EvidenceClass;
    /// The verification status and reason this write carries, if the
    /// producing path recorded one. Defaults to `None` — "no status
    /// recorded", the honest answer for every payload that predates
    /// verification statuses.
    fn verification(&self) -> Option<(VerificationStatus, Option<&str>)> {
        None
    }
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

    fn verification(&self) -> Option<(VerificationStatus, Option<&str>)> {
        self.verification
            .map(|status| (status, self.verification_reason.as_deref()))
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
    /// Edge attributes as stored (`emmo_edge.props_json`) — a composition
    /// fraction, a measurement's value, whatever the writer attached.
    ///
    /// These were written on every ingest and read back by NOTHING: the
    /// traversal that is the only production read path did not select the
    /// column, so a `CONTAINS_ELEMENT` edge came back without its fraction
    /// and the number was unreachable from the moment it was stored.
    /// `#[serde(default)]` for the same reason `tenant` carries it — older
    /// payloads stay deserializable and read as `None`.
    #[serde(default)]
    pub props_json: Option<String>,
    /// Writer-assigned confidence for this edge, likewise stored and never
    /// read back. `None` means the row carried SQL NULL, which is NOT the
    /// same as zero confidence and must not be rendered as such.
    #[serde(default)]
    pub confidence: Option<f64>,
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
    /// How far the deterministic ingest checks verified this fact against
    /// its source (best sighting so far). `None` = no status recorded —
    /// legacy rows and non-text write paths. `#[serde(default)]` keeps
    /// payloads serialized before this field deserializable.
    #[serde(default)]
    pub verification_status: Option<VerificationStatus>,
    /// The check's own words for why the status is what it is.
    #[serde(default)]
    pub verification_reason: Option<String>,
}

/// One stored assertion addressed by its stable id.
///
/// Unlike text search recall, this shape preserves every conditioned fact
/// field and returns weak-status assertions as stored. Retrieval uses it with
/// a selected [`EvidenceContribution`] to re-open the exact source witness;
/// this method is an identity lookup, not a trust filter.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StoredAssertion {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub conditions: Vec<MeasurementCondition>,
    pub evidence_class: EvidenceClass,
    pub confidence: f64,
    pub corroborations: i64,
    pub activity_id: String,
    pub source: String,
    pub agent: String,
    pub tenant: String,
    pub verification_status: Option<VerificationStatus>,
    pub verification_reason: Option<String>,
}

/// One semantic entity hit, attributed to the tenant whose entity row it
/// scored. Similarity is cosine, in `[-1, 1]`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SemanticEntityHit {
    pub name: String,
    pub tenant: String,
    pub similarity: f32,
}

/// One physically comparable partition of the local entity-vector store.
///
/// `model = None` is a legacy vector written before model identity was
/// recorded. It remains readable and visible here, but callers must not
/// silently treat it as belonging to their current embedding model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingPartition {
    pub model: Option<String>,
    pub dimensions: usize,
    pub count: usize,
}

/// One caller-computed entity vector to compare with the stored geometry.
/// `probe_id` is returned unchanged so one batched query can be joined back
/// to the caller's proposed graph writes without relying on result order.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EntityGeometryProbe {
    pub probe_id: usize,
    /// Proposed display name, used to prioritize trivial lexical variants
    /// before the per-probe result cap is applied.
    pub name: String,
    /// Persisted label/key partition, when the caller knows it. Typing uses
    /// this with `name` for leave-one-out class-region measurements.
    pub storage_label: Option<String>,
    pub vector: Vec<f32>,
}

/// A raw cosine-distance observation for one stored entity.
///
/// These rows are measurements only: this API never merges, rewrites, or
/// deletes graph data. Nullable type identity preserves legacy rows honestly.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EntityGeometryNeighbor {
    pub probe_id: usize,
    pub name: String,
    pub storage_label: String,
    pub entity_type: Option<String>,
    pub class_iri: Option<String>,
    pub distance: f64,
}

/// The mean cosine distance from a probe to the nearest stored exemplars of
/// one ontology class. `exemplars` is the class's full compatible population;
/// the mean itself includes at most the caller's `neighbors_per_class` rows.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ClassRegionDistance {
    pub probe_id: usize,
    pub entity_type: Option<String>,
    pub class_iri: String,
    pub exemplars: usize,
    pub mean_distance: f64,
}

/// One caller-computed triple geometry probe. Subject and object embeddings
/// must come from the model named in the corresponding read call.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TripleGeometryProbe {
    pub probe_id: usize,
    pub predicate: String,
    pub subject_vector: Vec<f32>,
    pub object_vector: Vec<f32>,
}

/// A same-predicate assertion neighboring a proposed triple in joint
/// subject/object embedding space. `distance` is the larger endpoint cosine
/// distance, so one exact endpoint cannot hide a distant other endpoint. It
/// is a raw prior, never an instruction to mutate.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TripleGeometryNeighbor {
    pub probe_id: usize,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub confidence: Option<f64>,
    pub subject_distance: f64,
    pub object_distance: f64,
    /// Maximum endpoint distance. A pair is close only when both endpoints
    /// satisfy the caller's cutoff.
    pub distance: f64,
}

/// Coverage of graph entities by one physically compatible vector partition.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EntityGeometryCoverage {
    pub entities: usize,
    pub compatible_embeddings: usize,
}

/// One stored per-source evidence contribution for an assertion.
///
/// `recall` reports only the immutable FIRST attribution on the parent row;
/// every corroborating source lives here (see
/// [`ProvenanceStore::assertion_evidence`]).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EvidenceContribution {
    /// Canonical independence key (`doi:…` / `url:…` / `file:…` /
    /// `document:…` / `opaque:…`; relays are `mesh:<origin key>` when the
    /// peer conveyed the origin, `mesh:unattributed` when it did not).
    pub source_key: String,
    /// The locator/display string exactly as this contribution supplied it.
    pub source_entity_id: String,
    /// SHA-256 of the source text whose coordinates the citation addresses.
    /// `None` is explicit legacy/uncited evidence.
    #[serde(default)]
    pub source_revision_id: Option<String>,
    /// Exact source text retained when this contribution was written.
    #[serde(default)]
    pub evidence_span: Option<String>,
    /// One-based inclusive source line range. Both values are `None` for an
    /// uncited or legacy contribution.
    #[serde(default)]
    pub line_start: Option<i64>,
    #[serde(default)]
    pub line_end: Option<i64>,
    /// Optional source-specific locator metadata, stored as JSON text.
    #[serde(default)]
    pub locator_json: Option<String>,
    pub activity_id: String,
    pub agent_id: String,
    pub confidence: f64,
    pub evidence_class: EvidenceClass,
    /// Verification result for THIS source contribution, separate from the
    /// best-wins aggregate cached on `prov_assertion`.
    #[serde(default)]
    pub verification_status: Option<VerificationStatus>,
    #[serde(default)]
    pub verification_reason: Option<String>,
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

/// The concept a predicate names, stripped of how it was written.
///
/// `assertion_id` hashes the predicate RAW, which is correct for identity —
/// two facts are the same fact only if they say the same thing the same way.
/// It is wrong for AGREEMENT. Measured on a real corpus 2026-08-26, the same
/// measurement arrives under three spellings and is stored as three separate,
/// mutually uncorroborating facts:
///
/// | subject | value | stored under |
/// |---|---|---|
/// | ss 316l | 1658 K | `hasSolidusTemperature` AND `solidus temperature` |
/// | ss 316l | 1723 K | `hasLiquidusTemperature` AND `liquidus temperature` |
/// | ti-6al-4v | 3315 K | `https://w3id.org/emmo#EMMO_e1097637…` AND `boiling temperature` |
///
/// PRISM's own ontology mapping manufactures the duplicate that then fails to
/// corroborate. Result on that corpus: 1,607 of 1,609 facts sat at
/// `corroborations = 1`, which means seen once — never corroborated.
///
/// So: an IRI collapses to its final segment, `has`/`is` prefixes go,
/// camelCase splits into words, and everything non-alphanumeric becomes a
/// single space.
#[must_use]
pub fn predicate_concept(predicate: &str) -> String {
    // An IRI names its concept in the last segment: take it, then treat it
    // like any other label.
    let tail = predicate
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(predicate)
        .trim();

    // camelCase / PascalCase -> spaced words, so `hasSolidusTemperature`
    // and `solidus temperature` meet.
    let mut spaced = String::with_capacity(tail.len() + 8);
    let mut prev_lower = false;
    for ch in tail.chars() {
        if ch.is_uppercase() && prev_lower {
            spaced.push(' ');
        }
        prev_lower = ch.is_lowercase() || ch.is_numeric();
        spaced.extend(ch.to_lowercase());
    }

    let words: Vec<&str> = spaced
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    // Drop a leading `has`/`is`/`had` — ontology naming convention, not
    // meaning. Never drop it if it is the ONLY word.
    let start = usize::from(words.len() > 1 && matches!(words[0], "has" | "is" | "had"));
    words[start..].join(" ")
}

/// Key on which two facts count as SAYING THE SAME THING, for corroboration.
///
/// Deliberately looser than [`assertion_id`]: identity must be exact, but
/// agreement must not be, or two papers reporting one measurement in different
/// words never corroborate each other — which is the state the corpus was
/// measured in.
///
/// Returns `None` when there is no parsed numeric value. Agreement between two
/// free-text objects is a judgement, not a hash, and pretending otherwise
/// would manufacture false corroboration — the failure mode that matters most
/// here, because a wrongly-green fact is worse than an honestly-red one.
#[must_use]
pub fn corroboration_key(
    tenant: &str,
    subject: &str,
    predicate: &str,
    value: Option<f64>,
    unit: Option<&str>,
) -> Option<String> {
    let value = value.filter(|v| v.is_finite())?;
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    hash_field(&mut h, tenant.as_bytes());
    hash_field(&mut h, canonical_key(subject).as_bytes());
    hash_field(&mut h, predicate_concept(predicate).as_bytes());
    // Normalise the number itself so 1658 and 1658.0 meet, without pretending
    // 1658 and 1659 are the same measurement.
    hash_field(&mut h, format!("{value:.6e}").as_bytes());
    hash_field(&mut h, canonical_key(unit.unwrap_or("")).as_bytes());
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Lowercase alphanumeric-only surface used solely to prioritize trivial
/// punctuation/spacing variants in bounded geometry result sets.
fn lexical_key(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
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
    if read_user_version(conn).await? >= MATKG_NAMESPACE_VERSION {
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
        if version >= MATKG_NAMESPACE_VERSION {
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
        if version < SAMPLE_DISAGREEMENT_RETIRED_VERSION {
            migrate_retire_sample_disagreement(conn).await?;
        }
        if version < MATKG_NAMESPACE_VERSION {
            migrate_matkg_namespace(conn).await?;
        }

        // Stamp even when nothing needed changing — a fresh store has empty
        // tables, and returning without stamping would make every subsequent
        // open repeat the scans this guard exists to avoid. One stamp, after
        // all generations, so a crash between them re-runs from the last
        // committed generation rather than skipping one.
        conn.execute(
            &format!("PRAGMA user_version = {MATKG_NAMESPACE_VERSION}"),
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
/// Generation 6 — move stored `sample_disagreement` rows to the status
/// fail-to-promote ingest would have given them.
///
/// See [`SAMPLE_DISAGREEMENT_RETIRED_VERSION`] for why this is safe to move
/// UP and why value-less-with-unit rows land on `model_asserted` instead.
///
/// THE STATUS IS DENORMALIZED ACROSS THREE SURFACES and all three move, or
/// the graph and the assertion table would disagree about the same fact:
/// `prov_assertion` (the authority), `prov_assertion_evidence` (per witness),
/// and the `emmo_edge.props_json` copy the graph reads.
///
/// One bounded imprecision, stated rather than hidden: `emmo_edge` carries no
/// assertion id and no value or unit, so the value-less-with-unit exception
/// cannot be reproduced there and every edge copy moves to
/// `cited_by_reader`. On one measured corpus that is ~196 of 20,892 edges
/// whose denormalized copy reads one rank more trusting than their assertion
/// row, which still says `model_asserted`. `prov_assertion` remains the
/// authority for trust decisions; a graph read that filters on the edge copy
/// alone was already reading a cache, not the record.
///
/// The original reason is KEPT and the migration appends its own marker. That
/// text is the only surviving evidence of how much agreement a fact had, and
/// an audit must be able to tell a migrated row from a freshly ingested one.
///
/// Caller must hold the one-shot guard and the open transaction — see
/// [`run_key_migrations`].
async fn migrate_retire_sample_disagreement(conn: &turso::Connection) -> Result<()> {
    const MIGRATED: &str =
        "[migrated: cross-sample disagreement no longer lowers a reader-cited fact]";

    // The authority. `NULL || ' '` is NULL, so COALESCE supplies the empty
    // prefix when a row never carried a reason.
    conn.execute(
        &format!(
            "UPDATE prov_assertion \
                SET verification_status = CASE \
                        WHEN value IS NULL AND unit IS NOT NULL THEN 'model_asserted' \
                        ELSE 'cited_by_reader' END, \
                    verification_reason = COALESCE(verification_reason || ' ', '') || '{MIGRATED}' \
              WHERE verification_status = 'sample_disagreement'"
        ),
        (),
    )
    .await?;

    // Per-witness rows take the SAME decision as their parent, read from the
    // parent's value and unit. `EXISTS` guards the correlated subquery: a
    // witness whose assertion is gone must keep its own status rather than
    // have NULL written over it.
    conn.execute(
        "UPDATE prov_assertion_evidence \
            SET verification_status = ( \
                    SELECT CASE \
                             WHEN a.value IS NULL AND a.unit IS NOT NULL THEN 'model_asserted' \
                             ELSE 'cited_by_reader' END \
                      FROM prov_assertion a WHERE a.id = prov_assertion_evidence.assertion_id) \
          WHERE verification_status = 'sample_disagreement' \
            AND EXISTS (SELECT 1 FROM prov_assertion a \
                         WHERE a.id = prov_assertion_evidence.assertion_id)",
        (),
    )
    .await?;

    // The graph's denormalized copy. Replaces the exact key/value pair rather
    // than the whole column, so any other property on the edge survives.
    conn.execute(
        "UPDATE emmo_edge \
            SET props_json = replace(props_json, \
                    '\"verification_status\":\"sample_disagreement\"', \
                    '\"verification_status\":\"cited_by_reader\"') \
          WHERE props_json LIKE '%sample_disagreement%'",
        (),
    )
    .await?;
    Ok(())
}

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

/// Generation 6 — retire `sample_disagreement` from STORED rows.
///
/// Ingest moved to fail-to-promote: a fact short of the cross-sample agreement
/// bar keeps the reader-cited status it earned and records the shortfall in
/// `verification_reason`. Changing that code does not rewrite databases that
/// were already written under the old rule, and those rows are the whole
/// problem — `sample_disagreement` is not in [`VerificationStatus::is_trusted`],
/// so the default read hides them. One measured corpus of 86 papers has 21,109
/// of 21,218 facts in exactly that state: present, correct, and invisible.
///
/// Safe to move UP because `mark_at_most` only ever recorded the LOWEST-ranked
/// finding. A row reading `sample_disagreement` (rank 5) therefore had nothing
/// worse to say about itself — any real defect would have outranked it
/// downward and be stored instead.
///
/// ONE EXCEPTION, and it is recoverable from the data rather than guessed: a
/// value-less row carrying a unit is what `annotate_cited_fact` calls
/// `model_asserted` (rank 6). Under the old ordering the disagreement stamp
/// landed first and blocked that, so those rows must land on `model_asserted`,
/// not `cited_by_reader`, or the migration would promote them past a finding
/// that genuinely applies.
const SAMPLE_DISAGREEMENT_RETIRED_VERSION: i64 = 6;

/// Generation 7 — move the PRISM-minted MatKG namespace to `mirdyne.com`.
///
/// Upstream MatKG identifies its entities only under the placeholder
/// `http://example.com/`, so PRISM mints the class IRIs itself; the host in
/// them is ours to choose and it is now `mirdyne.com`. Stored rows written
/// under the old host still name classes the shipped ontology no longer
/// declares, which strands them: a bound term whose `class_iri` resolves to
/// nothing is indistinguishable from a term that was never bound.
///
/// The rewrite is a pure prefix swap. Every local name is unchanged, so the
/// old-to-new mapping is total and injective, and `class_iri` feeds no
/// assertion digest — no identity is recomputed and no fact changes meaning.
const MATKG_NAMESPACE_VERSION: i64 = 7;

const MATKG_NAMESPACE_BEFORE: &str = "https://marc27.com/ontology/matkg";
const MATKG_NAMESPACE_AFTER: &str = "https://mirdyne.com/ontology/matkg";

/// Every stored site of that namespace, as `(table, column)`.
///
/// The parent IRI is embedded in `item_id`, which is the proposal queue's
/// PRIMARY KEY and half of the sighting table's composite key, so those two
/// have to move together or a queued proposal loses its citations;
/// `ontology_term_binding.proposal_item_id` is the foreign key to that same
/// string and moves with them.
///
/// `provenance_records.output_json` is deliberately ABSENT. Those rows are
/// captured stdout of commands that really did run under the old namespace.
/// Rewriting them would make the audit trail assert a history that did not
/// happen, which is the one thing a provenance store must never do.
const MATKG_NAMESPACE_SITES: &[(&str, &str)] = &[
    ("emmo_entity", "class_iri"),
    ("ontology_class_embedding", "class_iri"),
    ("ontology_term_binding", "class_iri"),
    ("ontology_term_binding", "nearest_class_iri"),
    ("ontology_term_binding", "proposal_item_id"),
    ("ontology_proposal_queue", "item_id"),
    ("ontology_proposal_queue", "proposal_json"),
    ("ontology_proposal_sighting", "item_id"),
];

/// Rewrite the MatKG namespace prefix everywhere it is stored.
///
/// Runs inside the caller's migration transaction, so a failure at any site
/// rolls the whole generation back rather than leaving half the store on each
/// host. A collision on one of the keyed columns would surface here as a
/// constraint error and abort — which is the wanted behaviour, since silently
/// replacing the colliding row would discard a real proposal.
///
/// The ontology tables are created lazily by the term-binding layer and the
/// `nearest_*` columns were added later still, so a store opened before either
/// existed is normal, not corrupt. Those two cases are skipped; anything else
/// propagates.
async fn migrate_matkg_namespace(conn: &turso::Connection) -> Result<()> {
    for (table, column) in MATKG_NAMESPACE_SITES {
        let sql = format!(
            "UPDATE {table} SET {column} = \
             REPLACE({column}, '{MATKG_NAMESPACE_BEFORE}', '{MATKG_NAMESPACE_AFTER}') \
             WHERE {column} LIKE '%{MATKG_NAMESPACE_BEFORE}%'"
        );
        if let Err(error) = conn.execute(sql, ()).await {
            let message = error.to_string().to_lowercase();
            if message.contains("no such table") || message.contains("no such column") {
                continue;
            }
            return Err(anyhow::anyhow!(error).context(format!(
                "failed to rewrite MatKG namespace in {table}.{column}"
            )));
        }
    }
    Ok(())
}

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
            canonical_name TEXT,
            lexical_name TEXT,
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
    // The stable key already carries `canonical_key(name)`. Materialize that
    // identity for indexed batch resolution so harmless spelling drift (for
    // example `Ti` -> `Ti `) cannot orphan an otherwise valid embedding.
    crate::add_column_if_absent(conn, "emmo_entity", "canonical_name", "TEXT").await?;
    crate::add_column_if_absent(conn, "emmo_entity", "lexical_name", "TEXT").await?;
    conn.execute(
        r#"UPDATE emmo_entity
           SET canonical_name = SUBSTR(
               SUBSTR(key, INSTR(key, '|') + 1),
               INSTR(SUBSTR(key, INSTR(key, '|') + 1), ':') + 1
           )
           WHERE canonical_name IS NULL"#,
        (),
    )
    .await?;
    // One-statement legacy approximation for the common trivial variants;
    // all new writes use Rust's Unicode alphanumeric normalization below.
    conn.execute(
        r#"UPDATE emmo_entity
           SET lexical_name = LOWER(
               REPLACE(REPLACE(REPLACE(REPLACE(REPLACE(
                   COALESCE(name, ''), ' ', ''), '"', ''), '''', ''), '-', ''), '_', '')
           )
           WHERE lexical_name IS NULL"#,
        (),
    )
    .await?;
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
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_canonical_name \
         ON emmo_entity(tenant, canonical_name)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_entity_lexical_name \
         ON emmo_entity(tenant, lexical_name)",
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
            subject_canonical TEXT,
            predicate TEXT,
            object TEXT,
            object_canonical TEXT,
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
    crate::add_column_if_absent(conn, "prov_assertion", "subject_canonical", "TEXT").await?;
    crate::add_column_if_absent(conn, "prov_assertion", "object_canonical", "TEXT").await?;
    conn.execute(
        "UPDATE prov_assertion SET subject_canonical = LOWER(TRIM(subject)) \
         WHERE subject_canonical IS NULL",
        (),
    )
    .await?;
    conn.execute(
        "UPDATE prov_assertion SET object_canonical = LOWER(TRIM(object)) \
         WHERE object_canonical IS NULL",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_prov_assertion_geometry \
         ON prov_assertion(tenant, predicate, subject_canonical, object_canonical)",
        (),
    )
    .await?;
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
    // Verification status: how far the deterministic ingest checks verified
    // the fact against its source (annotate-not-refuse — see
    // [`VerificationStatus`]). NULL on legacy rows and on every path that
    // records no status; the default read treats NULL as visible, so no
    // pre-existing row vanishes when this column appears.
    crate::add_column_if_absent(conn, "prov_assertion", "verification_status", "TEXT").await?;
    crate::add_column_if_absent(conn, "prov_assertion", "verification_reason", "TEXT").await?;

    // One row per (assertion, distinct origin source) — the AUTHORITATIVE
    // record corroboration is computed from. Keyed by `source_key`
    // (see `origin_source_key`), NOT by ingestion activity: activity UUIDs
    // identify runs, and counting runs is exactly the self-corroboration
    // defect the v5 migration exists to fix. `source_revision_id` (e.g. a
    // content SHA-256) is attribution metadata, deliberately OUTSIDE the
    // primary key: a file edited in place stays the same source.
    // Contribution confidence is never updated or deleted through the API —
    // subtracting a contribution from noisy-OR needs a full recompute, so that
    // mutation is prohibited rather than half supported. Duplicate-source
    // writes may downgrade the evidence class. They may atomically replace an
    // entirely uncited legacy locator/attribution with the first complete
    // citation, or improve verification for the same exact witness; a partial
    // or complete witness is never combined with or overwritten by another.
    // The FK keeps
    // evidence attached to its assertion across the id re-key migrations (ON
    // UPDATE CASCADE); it is enforced because `open()` sets
    // `PRAGMA foreign_keys=ON` on every connection.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS prov_assertion_evidence (
            assertion_id TEXT NOT NULL,
            source_key TEXT NOT NULL,

            source_entity_id TEXT NOT NULL,
            source_revision_id TEXT,
            evidence_span TEXT,
            line_start INTEGER,
            line_end INTEGER,
            locator_json TEXT,
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
            verification_status TEXT,
            verification_reason TEXT,

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
    // Citation and per-source verification columns are nullable on purpose:
    // old contributions remain honest uncited evidence instead of gaining an
    // invented span, revision, locator, or check result during migration.
    crate::add_column_if_absent(conn, "prov_assertion_evidence", "evidence_span", "TEXT").await?;
    crate::add_column_if_absent(conn, "prov_assertion_evidence", "line_start", "INTEGER").await?;
    crate::add_column_if_absent(conn, "prov_assertion_evidence", "line_end", "INTEGER").await?;
    crate::add_column_if_absent(conn, "prov_assertion_evidence", "locator_json", "TEXT").await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion_evidence",
        "verification_status",
        "TEXT",
    )
    .await?;
    crate::add_column_if_absent(
        conn,
        "prov_assertion_evidence",
        "verification_reason",
        "TEXT",
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
    // sparse-only one, so ranking is a full scan. The validation APIs batch
    // all document probes into constant SQL round trips, but compute remains
    // O(probes × stored vectors); this is the explicit million-paper scaling
    // limit until Turso exposes a compatible dense-vector index.
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS emmo_embedding (
            key TEXT PRIMARY KEY,
            tenant TEXT,
            model TEXT,
            dim INTEGER,
            vector BLOB
        )"#,
        (),
    )
    .await?;
    // Additive model identity for databases created before geometry-backed
    // validation. Existing rows deliberately remain NULL: attributing an old
    // vector to whichever backend happens to be configured today would make
    // an unvalidated model partition pose as a compatible one.
    crate::add_column_if_absent(conn, "emmo_embedding", "model", "TEXT").await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_embedding_tenant ON emmo_embedding(tenant)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emmo_embedding_partition \
         ON emmo_embedding(tenant, model, dim)",
        (),
    )
    .await?;

    // The resolution ladder's durable surfaces: per-term binding records
    // (rung + score) and per-ontology class-label vectors.
    crate::term_binding::init_schema(conn).await?;

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

/// The graph shape ONE typed fact kind writes — the ACTIVE ontology's
/// declaration, never the store's. The writer used to hold a closed
/// seven-entry kind→shape table in Rust, so an ontology whose fact kinds
/// differ from EMMO's (a legal `"obligation"`, a pharma `"assay"`) had every
/// typed fact degraded to untyped `Entity` endpoints by the fallback arm:
/// zero Rust edits bought a degraded graph, not a working one. The table
/// now belongs to the ontology adapters (`prism_ingest`'s
/// `Ontology::fact_graph_shape`; EMMO declares its seven legacy shapes
/// there), and this writer executes exactly the shape it is handed — one
/// generic path, no domain vocabulary of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactGraphShape {
    /// Storage label for the OBJECT node when the caller supplied no
    /// ontology-classified identity for it. Classified metadata still wins:
    /// the ontology's declared endpoint class is identity, this label is the
    /// established fallback for unclassified writes.
    pub object_storage_label: String,
    /// The typed edge's rel_type (EMMO: `HAS_PHASE`, `CONTAINS_ELEMENT`, …;
    /// an induced ontology: its own declared relation token).
    pub edge_rel_type: String,
    /// Whether this shape reifies a measurement node between the endpoints:
    /// subject —[`edge_rel_type`]→ Measurement(value/unit/conditions/evidence)
    /// —`OF_PROPERTY`→ object. The reified node, its props and the
    /// `OF_PROPERTY` edge are the store's own audit shape (they are what
    /// `graph_search` reads); the object label still comes from this
    /// declaration.
    pub reified_measurement: bool,
    /// Prop key under which the OBJECT NODE carries the fact's object text
    /// (EMMO composition: `canonical_formula`; EMMO structure: `system`).
    /// `None` writes no object-node prop.
    pub object_text_prop: Option<String>,
    /// Prop key under which the typed EDGE carries the fact's numeric value
    /// (EMMO contains: `fraction`; EMMO processing: `order`). `None` writes
    /// no edge-value prop; the value still rides the assertion.
    pub edge_value_prop: Option<String>,
}

impl FactGraphShape {
    /// The EMMO shape for one of the store's seven legacy fact kinds, as the
    /// EMMO adapter declares it. Kept here so every writer test (and the
    /// EMMO adapter in `prism_ingest`) shares the one declaration instead
    /// of restating it — this is EMMO's own table, resident where EMMO's
    /// compatibility shapes live.
    #[must_use]
    pub fn emmo(kind: &str) -> Option<Self> {
        Some(match kind {
            "measurement" => Self {
                object_storage_label: "Property".into(),
                edge_rel_type: "HAS_MEASUREMENT".into(),
                reified_measurement: true,
                object_text_prop: None,
                edge_value_prop: None,
            },
            "phase" => Self {
                object_storage_label: "Phase".into(),
                edge_rel_type: "HAS_PHASE".into(),
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: None,
            },
            "composition" => Self {
                object_storage_label: "Composition".into(),
                edge_rel_type: "HAS_COMPOSITION".into(),
                reified_measurement: false,
                object_text_prop: Some("canonical_formula".into()),
                edge_value_prop: None,
            },
            "contains" => Self {
                object_storage_label: "Element".into(),
                edge_rel_type: "CONTAINS_ELEMENT".into(),
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: Some("fraction".into()),
            },
            "processing" => Self {
                object_storage_label: "Manufacturing".into(),
                edge_rel_type: "PROCESSED_BY".into(),
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: Some("order".into()),
            },
            "structure" => Self {
                object_storage_label: "CrystalStructure".into(),
                edge_rel_type: "HAS_STRUCTURE".into(),
                reified_measurement: false,
                object_text_prop: Some("system".into()),
                edge_value_prop: None,
            },
            "application" => Self {
                object_storage_label: "Application".into(),
                edge_rel_type: "USED_IN".into(),
                reified_measurement: false,
                object_text_prop: None,
                edge_value_prop: None,
            },
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
struct FactWriteMetadata<'a> {
    subject: Option<EntityWrite<'a>>,
    object: Option<EntityWrite<'a>>,
    ontology: Option<OntologyClassification<'a>>,
    /// The ontology-declared graph shape for this fact's kind, when the
    /// caller resolved one (`Ontology::fact_graph_shape`). `None` means no
    /// declared shape: the fact is kept as a generic edge — same honest
    /// default as an undeclared kind, never a frozen built-in table.
    shape: Option<FactGraphShape>,
    force_generic_graph: bool,
}

/// The object write's classified identity when the caller supplied one, else
/// the established fallback label the resolved shape declares for this fact
/// kind. Identity is the ontology's declaration; the fallback is the shape's.
fn object_entity_write<'a>(
    metadata: Option<&FactWriteMetadata<'a>>,
    legacy: &'a str,
) -> EntityWrite<'a> {
    metadata
        .and_then(|details| details.object)
        .unwrap_or_else(|| EntityWrite::legacy(legacy))
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
                   (key, name, canonical_name, lexical_name, label, entity_type, class_iri,
                    tenant, props_json, created_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6, ?5), ?7, ?8, ?9, ?10)
                   ON CONFLICT(key) DO UPDATE SET
                       name = excluded.name,
                       canonical_name = excluded.canonical_name,
                       lexical_name = excluded.lexical_name,
                       label = excluded.label,
                       entity_type = CASE
                           WHEN ?6 IS NULL
                               THEN COALESCE(emmo_entity.entity_type, excluded.entity_type)
                           ELSE ?6
                       END,
                       class_iri = COALESCE(?7, emmo_entity.class_iri),
                       props_json = COALESCE(excluded.props_json, emmo_entity.props_json)"#,
                [
                    Value::Text(key.clone()),
                    Value::Text(name.to_string()),
                    Value::Text(canonical_key(name)),
                    Value::Text(lexical_key(name)),
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
        self.write_fact_as(fact, prov, fact.evidence_class(), None, None)
            .await
    }

    /// Write a fact using the store's established graph shape and stamp the
    /// assertion with the ontology artifact that classified it.
    ///
    /// `shape` is the ACTIVE ontology's declared graph shape for this fact's
    /// kind (`FactGraphShape`, resolved by the caller through the ontology
    /// adapter). `None` keeps the fact as a generic edge — the honest
    /// default for a kind the ontology does not declare.
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
        shape: Option<FactGraphShape>,
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
                shape,
                force_generic_graph: false,
            }),
            None,
        )
        .await
    }

    /// [`Self::write_fact_with_classification`] with an exact, revision-bound
    /// source witness persisted on this contribution.
    pub async fn write_fact_with_classification_and_citation<F: FactPayload>(
        &self,
        fact: &F,
        prov: &LocalProvenance,
        ontology: OntologyClassification<'_>,
        shape: Option<FactGraphShape>,
        citation: &SourceCitation,
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
                shape,
                force_generic_graph: false,
            }),
            Some(citation),
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
        shape: Option<FactGraphShape>,
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
                shape,
                force_generic_graph: false,
            }),
            None,
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
    /// Generic over [`FactPayload`] like its sibling
    /// [`Self::write_fact_with_classification`]: the underlying
    /// `write_fact_as` always was, and pinning this one to `LocalFact` only
    /// meant the DOCUMENT path (which carries `MaterialFact`) could not reach
    /// the classified write at all, and silently fell back to the default
    /// `Matter` label for every subject it stored.
    pub async fn write_classified_fact_with_evidence<F: FactPayload>(
        &self,
        fact: &F,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
        nodes: ClassifiedFactNodes<'_>,
        ontology: OntologyClassification<'_>,
        shape: Option<FactGraphShape>,
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
                shape,
                force_generic_graph: false,
            }),
            None,
        )
        .await
    }

    /// [`Self::write_classified_fact_with_evidence`] with an exact,
    /// revision-bound source witness persisted on this contribution.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_classified_fact_with_evidence_and_citation<F: FactPayload>(
        &self,
        fact: &F,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
        nodes: ClassifiedFactNodes<'_>,
        ontology: OntologyClassification<'_>,
        shape: Option<FactGraphShape>,
        citation: &SourceCitation,
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
                shape,
                force_generic_graph: false,
            }),
            Some(citation),
        )
        .await
    }

    /// Persist a paper-agent fact with whatever canonical endpoint classes
    /// the active ontology supplied and its exact source witness.
    ///
    /// Unlike the compatibility writers, this path deliberately uses the
    /// generic edge shape. Ontology identity comes from the proposed
    /// predicate and endpoint bindings; a legacy closed `kind` hint is not a
    /// second vocabulary and cannot steer storage here.
    pub async fn write_ontology_bound_fact_with_citation<F: FactPayload>(
        &self,
        fact: &F,
        prov: &LocalProvenance,
        evidence_class: EvidenceClass,
        nodes: OntologyBoundFactNodes<'_>,
        ontology: OntologyClassification<'_>,
        citation: &SourceCitation,
    ) -> Result<()> {
        if let Some(subject) = nodes.subject {
            validate_classified_node(subject)?;
        }
        if let Some(object) = nodes.object {
            validate_classified_node(object)?;
        }
        validate_ontology_classification(ontology)?;
        self.write_fact_as(
            fact,
            prov,
            evidence_class,
            Some(FactWriteMetadata {
                subject: nodes.subject.map(EntityWrite::classified),
                object: nodes.object.map(EntityWrite::classified),
                ontology: Some(ontology),
                shape: None,
                force_generic_graph: true,
            }),
            Some(citation),
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
            let (confidence, _class, _status) = self
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
                    None,
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
        citation: Option<&SourceCitation>,
    ) -> Result<()> {
        let conditions = payload.conditions().to_vec();
        let fact = payload.to_local_fact();
        let tenant = prov.tenant.as_str();

        // Defence in depth checks only the non-empty term carried by the
        // typed payload. The paper agent navigates the active ontology; the
        // store must not second-guess that result with a closed Rust table.
        //
        let verification = payload.verification();
        validate_conditions(&conditions)?;
        // A present term must be non-empty, but absence has no universal
        // semantic meaning the store can decide. The active ontology and
        // reader own that judgement; persistence records the supplied shape.
        let stored_unit = fact
            .unit
            .as_deref()
            .map(|raw| UnitTerm::new(raw.to_string()))
            .transpose()?
            .map(|unit| unit.as_str().to_string());
        let force_generic_graph = metadata
            .as_ref()
            .is_some_and(|details| details.force_generic_graph);

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
            // aggregates it returns: `confidence`, `evidence_class`, and
            // `verification_status` are REBOUND here from this one write's
            // own values to the parent row's post-update state. That is what
            // keeps the graph on the same evidence gate as the assertion — a
            // duplicate source cannot move an edge's confidence, a re-record
            // cannot upgrade a Measurement node's class, and a genuine
            // corroboration lifts the edge to the combined (noisy-OR)
            // confidence instead of the last writer's own number. The
            // verification status mirrored onto the graph is likewise the
            // best-wins aggregate, so the edge and the assertion can never
            // disagree about how verified the fact is.
            let (confidence, evidence_class, verification_status) = self
                .record_assertion_in_open_txn(
                    &LocalAssertion {
                        subject: fact.subject.clone(),
                        predicate: fact.predicate.clone(),
                        object: fact.object.clone(),
                        confidence: fact.confidence,
                    },
                    prov,
                    fact.value,
                    stored_unit.as_deref(),
                    &conditions,
                    evidence_class,
                    verification,
                    metadata.as_ref().and_then(|details| details.ontology),
                    citation,
                )
                .await?;

            // The status half of every edge/node props write below: merge
            // the aggregate verification status into a props object, or
            // leave the props exactly as they were (including None) when no
            // status is recorded — a pure-tabular or mesh write keeps its
            // historical byte shape.
            let props_with_status = |base: Option<serde_json::Value>| -> Option<String> {
                match (base, verification_status) {
                    (Some(serde_json::Value::Object(mut map)), Some(status)) => {
                        map.insert(
                            "verification_status".to_string(),
                            serde_json::Value::String(status.as_str().to_string()),
                        );
                        Some(serde_json::Value::Object(map).to_string())
                    }
                    (Some(base), None) => Some(base.to_string()),
                    (None, Some(status)) => Some(
                        serde_json::json!({ "verification_status": status.as_str() }).to_string(),
                    ),
                    (Some(base), Some(_)) => Some(base.to_string()),
                    (None, None) => None,
                }
            };

            // One binding per node role: caller-supplied classified identity
            // when present, the established storage shape otherwise. Every
            // fact arm uses these bindings so identity semantics do not drift
            // between dispatch paths.
            let metadata = metadata.as_ref();
            let subject_entity =
                metadata
                    .and_then(|details| details.subject)
                    .unwrap_or_else(|| {
                        EntityWrite::legacy(if force_generic_graph {
                            "Entity"
                        } else {
                            "Matter"
                        })
                    });

            // ONE generic path. The graph shape for a typed fact kind is the
            // ACTIVE ONTOLOGY's declaration, resolved by the caller and
            // carried here as metadata — the store no longer owns a closed
            // kind→(class, edge) table, so a non-materials ontology's kinds
            // are first-class typed shapes, not degraded `Entity` endpoints.
            // A fact with no declared shape (or a measurement-shaped proposal
            // without a value) is kept as a generic edge — never dropped.
            let shape = if force_generic_graph {
                None
            } else {
                metadata.and_then(|details| details.shape.clone())
            };
            match shape {
                Some(shape) if shape.reified_measurement && fact.value.is_some() => {
                    // The guard selects the reified shape only when a numeric
                    // value exists. A value-less proposal falls through to
                    // the generic edge arm, retaining its assertion instead
                    // of disappearing because of a model-supplied hint.
                    let value = fact.value.expect("measurement guard checked value");
                    let unit = stored_unit.clone();
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
                            props_with_status(Some(props)),
                        )
                        .await?;
                    let obj_key = self
                        .upsert_entity(
                            &fact.object,
                            object_entity_write(metadata, &shape.object_storage_label),
                            tenant,
                            None,
                        )
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &meas_key,
                        &shape.edge_rel_type,
                        &fact.predicate,
                        confidence,
                        tenant,
                        props_with_status(None).as_deref(),
                    )
                    .await?;
                    self.upsert_edge(
                        &meas_key,
                        &obj_key,
                        "OF_PROPERTY",
                        &fact.predicate,
                        confidence,
                        tenant,
                        props_with_status(None).as_deref(),
                    )
                    .await?;
                }
                Some(shape) if !shape.reified_measurement => {
                    let props = shape
                        .object_text_prop
                        .as_ref()
                        .map(|key| serde_json::json!({ key.clone(): &fact.object }));
                    let edge_props = props_with_status(
                        shape
                            .edge_value_prop
                            .as_deref()
                            .zip(fact.value)
                            .map(|(key, value)| serde_json::json!({ key: value })),
                    );
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(
                            &fact.object,
                            object_entity_write(metadata, &shape.object_storage_label),
                            tenant,
                            props.map(|props| props.to_string()),
                        )
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        &shape.edge_rel_type,
                        &fact.predicate,
                        confidence,
                        tenant,
                        edge_props.as_deref(),
                    )
                    .await?;
                }
                // No declared shape (or a value-less measurement proposal):
                // keep the fact as a generic edge, don't drop it.
                _ => {
                    let subj_key = self
                        .upsert_entity(&fact.subject, subject_entity, tenant, None)
                        .await?;
                    let obj_key = self
                        .upsert_entity(
                            &fact.object,
                            object_entity_write(metadata, "Entity"),
                            tenant,
                            None,
                        )
                        .await?;
                    self.upsert_edge(
                        &subj_key,
                        &obj_key,
                        &fact.predicate,
                        &fact.predicate,
                        confidence,
                        tenant,
                        props_with_status(None).as_deref(),
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
            .record_assertion_in_open_txn(
                a,
                prov,
                value,
                unit,
                conditions,
                evidence_class,
                None,
                None,
                None,
            )
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
    /// `(confidence, evidence_class, verification_status)` so
    /// `write_fact_as` can mirror them into the EMMO graph in the same
    /// transaction — the graph must follow the evidence gate,
    /// `WORST_CLASS_CASE`, and the best-wins verification aggregate, never
    /// the last writer.
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
        verification: Option<(VerificationStatus, Option<&str>)>,
        ontology: Option<OntologyClassification<'_>>,
        citation: Option<&SourceCitation>,
    ) -> Result<(f64, EvidenceClass, Option<VerificationStatus>)> {
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
                   (id, subject, subject_canonical, predicate, object, object_canonical, value, unit,
                    conditions_json, evidence_class, confidence, corroborations,
                    confidence_basis, activity_id, source, agent, tenant)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                           0.0, 0, 'native',
                           ?11, ?12, ?13, ?14)
                   ON CONFLICT(id) DO NOTHING"#,
                [
                    Value::Text(id.clone()),
                    Value::Text(a.subject.clone()),
                    Value::Text(canonical_key(&a.subject)),
                    Value::Text(a.predicate.clone()),
                    Value::Text(a.object.clone()),
                    Value::Text(canonical_key(&a.object)),
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
        // stored contribution's confidence stays immutable: a later
        // extraction from the same source cannot raise or replace its numeric
        // contribution. Attribution to an existing witness is immutable too;
        // the sole exception below is a wholly uncited legacy row receiving
        // its first complete witness and that witness's locator/activity.
        let inserted = self
            .conn
            .execute(
                r#"INSERT INTO prov_assertion_evidence
                   (assertion_id, source_key, source_entity_id, source_revision_id,
                    evidence_span, line_start, line_end, locator_json,
                    activity_id, agent_id, confidence, evidence_class,
                    verification_status, verification_reason,
                    confidence_kind, legacy_corroborations)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                           ?9, ?10, ?11, ?12, ?13, ?14, 'source', NULL)
                   ON CONFLICT(assertion_id, source_key) DO NOTHING"#,
                [
                    Value::Text(id.clone()),
                    Value::Text(source_key.clone()),
                    Value::Text(prov.source_entity_id.clone()),
                    citation.map_or(Value::Null, |citation| {
                        Value::Text(citation.source_revision_id().to_string())
                    }),
                    citation.map_or(Value::Null, |citation| {
                        Value::Text(citation.evidence_span().to_string())
                    }),
                    citation.map_or(Value::Null, |citation| {
                        Value::Integer(citation.line_start())
                    }),
                    citation.map_or(Value::Null, |citation| Value::Integer(citation.line_end())),
                    citation
                        .and_then(SourceCitation::locator_json)
                        .map_or(Value::Null, |locator| Value::Text(locator.to_string())),
                    Value::Text(prov.activity_id.clone()),
                    Value::Text(prov.agent_id.clone()),
                    Value::Real(confidence_evidence),
                    Value::Text(evidence_class.as_str().to_string()),
                    verification.map_or(Value::Null, |(status, _)| {
                        Value::Text(status.as_str().to_string())
                    }),
                    verification
                        .and_then(|(_, reason)| reason)
                        .map_or(Value::Null, |reason| Value::Text(reason.to_string())),
                ],
            )
            .await?;

        // A source may have been stored before citation support existed, or
        // first observed by an uncited path and later re-read exactly. A
        // citation is one indivisible witness: replace a legacy row only when
        // ALL core citation cells are NULL. Updating fields independently
        // could combine revision/span/coordinates from different reads into
        // a witness that never existed. The source locator and attribution
        // move with the citation so retrieval opens the exact snapshot and
        // audits the activity/agent that produced it.
        if let Some(citation) = citation {
            self.conn
                .execute(
                    r#"UPDATE prov_assertion_evidence
                       SET source_entity_id = ?3,
                           source_revision_id = ?4,
                           evidence_span = ?5,
                           line_start = ?6,
                           line_end = ?7,
                           locator_json = ?8,
                           activity_id = ?9,
                           agent_id = ?10
                       WHERE assertion_id = ?1 AND source_key = ?2
                         AND source_revision_id IS NULL
                         AND evidence_span IS NULL
                         AND line_start IS NULL
                         AND line_end IS NULL"#,
                    [
                        Value::Text(id.clone()),
                        Value::Text(source_key.clone()),
                        Value::Text(prov.source_entity_id.clone()),
                        Value::Text(citation.source_revision_id().to_string()),
                        Value::Text(citation.evidence_span().to_string()),
                        Value::Integer(citation.line_start()),
                        Value::Integer(citation.line_end()),
                        citation
                            .locator_json()
                            .map_or(Value::Null, |locator| Value::Text(locator.to_string())),
                        Value::Text(prov.activity_id.clone()),
                        Value::Text(prov.agent_id.clone()),
                    ],
                )
                .await?;
        }

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
                    Value::Text(source_key.clone()),
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

        // Verification status: BEST-wins, the opposite direction from the
        // evidence class, deliberately. The class records how knowledge was
        // produced and can only get worse; the status records what the check
        // established about one exact source witness. A duplicate may improve
        // that status only when it names the SAME stored citation (including
        // its reopenable source locator and optional locator metadata), or
        // when both the stored and incoming writes are uncited. Otherwise a
        // conclusion from a different revision/span could launder the stored
        // witness. Newly inserted source evidence qualifies by construction
        // and still participates in the parent BEST-wins aggregate. A write
        // carrying NO status leaves both rows untouched; status and reason
        // always move together.
        if let Some((status, reason)) = verification {
            let stored_rank = verification_rank_case("verification_status");
            let cited = i64::from(citation.is_some());
            let citation_params = || {
                [
                    Value::Text(id.clone()),
                    Value::Text(source_key.clone()),
                    Value::Integer(status.rank()),
                    Value::Text(status.as_str().to_string()),
                    reason.map_or(Value::Null, |reason| Value::Text(reason.to_string())),
                    Value::Integer(cited),
                    citation.map_or(Value::Null, |_| Value::Text(prov.source_entity_id.clone())),
                    citation.map_or(Value::Null, |citation| {
                        Value::Text(citation.source_revision_id().to_string())
                    }),
                    citation.map_or(Value::Null, |citation| {
                        Value::Text(citation.evidence_span().to_string())
                    }),
                    citation.map_or(Value::Null, |citation| {
                        Value::Integer(citation.line_start())
                    }),
                    citation.map_or(Value::Null, |citation| Value::Integer(citation.line_end())),
                    citation
                        .and_then(SourceCitation::locator_json)
                        .map_or(Value::Null, |locator| Value::Text(locator.to_string())),
                ]
            };
            let same_witness = r#"(
                    (?6 = 0
                     AND source_revision_id IS NULL
                     AND evidence_span IS NULL
                     AND line_start IS NULL
                     AND line_end IS NULL)
                    OR
                    (?6 = 1
                     AND source_entity_id = ?7
                     AND source_revision_id = ?8
                     AND evidence_span = ?9
                     AND line_start = ?10
                     AND line_end = ?11
                     AND locator_json IS ?12)
                )"#;
            self.conn
                .execute(
                    &format!(
                        "UPDATE prov_assertion_evidence \
                         SET verification_reason = ?5, verification_status = ?4 \
                         WHERE assertion_id = ?1 AND source_key = ?2 \
                           AND ?3 > {stored_rank} AND {same_witness}"
                    ),
                    citation_params(),
                )
                .await?;
            self.conn
                .execute(
                    &format!(
                        "UPDATE prov_assertion \
                         SET verification_reason = ?5, verification_status = ?4 \
                         WHERE id = ?1 AND ?3 > {stored_rank} \
                           AND EXISTS ( \
                               SELECT 1 FROM prov_assertion_evidence \
                               WHERE assertion_id = ?1 AND source_key = ?2 \
                                 AND {same_witness} \
                           )"
                    ),
                    citation_params(),
                )
                .await?;
        }

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
                "SELECT confidence, evidence_class, verification_status \
                 FROM prov_assertion WHERE id = ?1",
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
        let aggregate_status = get_opt_str(&row, 2)?
            .as_deref()
            .and_then(VerificationStatus::parse);
        while rows.next().await?.is_some() {}
        Ok((aggregate_confidence, aggregate_class, aggregate_status))
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

    /// The stored attributes of one entity (`emmo_entity.props_json`).
    ///
    /// Ingest has always written these; nothing could read them back, so a
    /// crystal structure or any other extracted attribute was unreachable the
    /// moment it landed. Exact name within one tenant — the same pairing every
    /// other per-tenant read uses, so the same name owned by two tenants stays
    /// two rows.
    ///
    /// `Ok(None)` distinguishes "no such entity" and "entity with no stored
    /// attributes" from an error; neither is exceptional.
    pub async fn entity_props_json(&self, name: &str, tenant: &str) -> Result<Option<String>> {
        let mut rows = self
            .conn
            .query(
                "SELECT props_json FROM emmo_entity WHERE tenant = ?1 AND name = ?2",
                vec![
                    Value::Text(tenant.to_string()),
                    Value::Text(name.to_string()),
                ],
            )
            .await?;
        match rows.next().await? {
            Some(row) => crate::get_opt_str(&row, 0),
            None => Ok(None),
        }
    }

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

        // `e.props_json` and `e.confidence` are last so the existing column
        // indices below keep their meaning. Both were written on every
        // ingest and selected by nothing.
        const EDGE_COLS: &str = "e.rel_type, \
             s.name, s.entity_type, s.label, s.class_iri, s.tenant, \
             t.name, t.entity_type, t.label, t.class_iri, t.tenant, \
             e.tenant, e.props_json, e.confidence";
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
                    props_json: crate::get_opt_str(&row, 12)?,
                    // NULL confidence stays None. Defaulting it to 0.0 would
                    // render an unscored edge as a maximally doubted one.
                    confidence: row.get_value(13).ok().and_then(|v| v.as_real().copied()),
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
    ///
    /// Returns the TRUSTED subset ([`VerificationFilter::Trusted`]): facts
    /// whose deterministic checks all passed, plus rows with no recorded
    /// status. Unverified facts are present and findable through
    /// [`Self::recall_with_context_filtered`], never promoted here.
    pub async fn recall_with_context_scoped(
        &self,
        query: &str,
        tenants: &[&str],
        limit: i64,
    ) -> Result<Vec<RecalledMaterialFact>> {
        self.recall_with_context_filtered(query, tenants, limit, VerificationFilter::Trusted)
            .await
    }

    /// [`Self::recall_with_context_scoped`] with an explicit verification
    /// filter — the review surface: `Any` reads everything,
    /// `Status(s)` pulls exactly one status (say, every
    /// `subject_not_verbatim` fact awaiting a reviewer).
    pub async fn recall_with_context_filtered(
        &self,
        query: &str,
        tenants: &[&str],
        limit: i64,
        filter: VerificationFilter,
    ) -> Result<Vec<RecalledMaterialFact>> {
        if tenants.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!("%{query}%");
        let sql = format!(
            "SELECT subject, predicate, object, value, unit, conditions_json, \
                    evidence_class, confidence, source, agent, tenant, \
                    verification_status, verification_reason \
             FROM prov_assertion \
             WHERE tenant IN ({}) AND (subject LIKE ?{} OR object LIKE ?{}) \
               AND {} \
             ORDER BY confidence DESC LIMIT ?{}",
            tenant_placeholders(1, tenants.len()),
            tenants.len() + 1,
            tenants.len() + 2,
            filter.sql_clause("verification_status"),
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
                verification_status: get_opt_str(&row, 11)?
                    .as_deref()
                    .and_then(VerificationStatus::parse),
                verification_reason: get_opt_str(&row, 12)?,
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

    /// Load one assertion by its stable id without applying a verification
    /// filter.
    ///
    /// The full value/unit/condition identity is returned so a retrieval
    /// caller can pair the fact with one of its per-source citations and
    /// re-check exactly the assertion that citation originally supported.
    pub async fn assertion_by_id(&self, id: &str) -> Result<Option<StoredAssertion>> {
        let mut rows = self
            .conn
            .query(
                "SELECT id, subject, predicate, object, value, unit, conditions_json, \
                        evidence_class, confidence, corroborations, activity_id, source, \
                        agent, tenant, verification_status, verification_reason \
                 FROM prov_assertion WHERE id = ?1",
                [Value::Text(id.to_string())],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let conditions_json = get_str(&row, 6)?;
        let conditions = serde_json::from_str(&conditions_json).map_err(|error| {
            anyhow::anyhow!("stored assertion has invalid conditions_json: {error}")
        })?;
        let assertion = StoredAssertion {
            id: get_str(&row, 0)?,
            subject: get_str(&row, 1)?,
            predicate: get_str(&row, 2)?,
            object: get_str(&row, 3)?,
            value: row
                .get_value(4)
                .ok()
                .and_then(|value| value.as_real().copied()),
            unit: get_opt_str(&row, 5)?.filter(|unit| !unit.is_empty()),
            conditions,
            evidence_class: EvidenceClass::from_stored(&get_str(&row, 7)?),
            confidence: row
                .get_value(8)
                .ok()
                .and_then(|value| value.as_real().copied())
                .unwrap_or(0.0),
            corroborations: row
                .get_value(9)
                .ok()
                .and_then(|value| value.as_integer().copied())
                .unwrap_or(0),
            activity_id: get_str(&row, 10)?,
            source: get_str(&row, 11)?,
            agent: get_str(&row, 12)?,
            tenant: get_str(&row, 13)?,
            verification_status: get_opt_str(&row, 14)?
                .as_deref()
                .and_then(VerificationStatus::parse),
            verification_reason: get_opt_str(&row, 15)?,
        };
        while rows.next().await?.is_some() {}
        Ok(Some(assertion))
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
                "SELECT source_key, source_entity_id, source_revision_id, evidence_span, \
                        line_start, line_end, locator_json, activity_id, agent_id, \
                        confidence, evidence_class, verification_status, verification_reason, \
                        confidence_kind, legacy_corroborations \
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
                source_revision_id: get_opt_str(&row, 2)?,
                evidence_span: get_opt_str(&row, 3)?,
                line_start: row
                    .get_value(4)
                    .ok()
                    .and_then(|value| value.as_integer().copied()),
                line_end: row
                    .get_value(5)
                    .ok()
                    .and_then(|value| value.as_integer().copied()),
                locator_json: get_opt_str(&row, 6)?,
                activity_id: get_str(&row, 7)?,
                agent_id: get_str(&row, 8)?,
                confidence: row
                    .get_value(9)
                    .ok()
                    .and_then(|v| v.as_real().copied())
                    .unwrap_or(0.0),
                evidence_class: EvidenceClass::from_stored(&get_str(&row, 10)?),
                verification_status: get_opt_str(&row, 11)?
                    .as_deref()
                    .and_then(VerificationStatus::parse),
                verification_reason: get_opt_str(&row, 12)?,
                confidence_kind: get_str(&row, 13)?,
                legacy_corroborations: row.get_value(14).ok().and_then(|v| v.as_integer().copied()),
            });
        }
        Ok(contributions)
    }

    /// Assertions whose recorded verification status is exactly `status`,
    /// scoped to the read tenants — the CANDIDATE list for re-verification
    /// review (`cited_by_reader` is the span-unchecked population the
    /// fresh paper path produces). Ordered by id for deterministic paging.
    /// Rows with no recorded status are not returned; they predate the
    /// status axis and are reached by `assertion_by_id`.
    pub async fn assertions_by_verification(
        &self,
        status: VerificationStatus,
        tenants: &[&str],
        limit: i64,
    ) -> Result<Vec<StoredAssertion>> {
        if tenants.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT id, subject, predicate, object, value, unit, conditions_json, \
                    evidence_class, confidence, corroborations, activity_id, source, \
                    agent, tenant, verification_status, verification_reason \
             FROM prov_assertion \
             WHERE tenant IN ({}) AND verification_status = ?{} \
             ORDER BY id LIMIT ?{}",
            tenant_placeholders(1, tenants.len()),
            tenants.len() + 1,
            tenants.len() + 2,
        );
        let mut params = tenant_params(tenants);
        params.push(Value::Text(status.as_str().to_string()));
        params.push(Value::Integer(limit.max(0)));
        let mut rows = self.conn.query(&sql, params).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let conditions_json = get_str(&row, 6)?;
            let conditions = serde_json::from_str(&conditions_json).map_err(|error| {
                anyhow::anyhow!("stored assertion has invalid conditions_json: {error}")
            })?;
            out.push(StoredAssertion {
                id: get_str(&row, 0)?,
                subject: get_str(&row, 1)?,
                predicate: get_str(&row, 2)?,
                object: get_str(&row, 3)?,
                value: row
                    .get_value(4)
                    .ok()
                    .and_then(|value| value.as_real().copied()),
                unit: get_opt_str(&row, 5)?.filter(|unit| !unit.is_empty()),
                conditions,
                evidence_class: EvidenceClass::from_stored(&get_str(&row, 7)?),
                confidence: row
                    .get_value(8)
                    .ok()
                    .and_then(|value| value.as_real().copied())
                    .unwrap_or(0.0),
                corroborations: row
                    .get_value(9)
                    .ok()
                    .and_then(|value| value.as_integer().copied())
                    .unwrap_or(0),
                activity_id: get_str(&row, 10)?,
                source: get_str(&row, 11)?,
                agent: get_str(&row, 12)?,
                tenant: get_str(&row, 13)?,
                verification_status: get_opt_str(&row, 14)?
                    .as_deref()
                    .and_then(VerificationStatus::parse),
                verification_reason: get_opt_str(&row, 15)?,
            });
        }
        Ok(out)
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
        self.store_entity_embedding_locked(key, tenant, None, vector)
            .await
    }

    /// UPSERT one entity vector with the stable embedding backend/model id
    /// that produced it. New semantic-validation code should use this form;
    /// [`Self::store_entity_embedding`] remains the legacy, unattributed API
    /// and deliberately stores `model = NULL`.
    pub async fn store_entity_embedding_with_model(
        &self,
        key: &str,
        tenant: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<()> {
        if model.trim().is_empty() {
            bail!("embedding model id must not be empty");
        }
        validate_model_vector(vector, model, key)?;
        let _same_handle_guard = self.write_lock.lock().await;
        self.store_entity_embedding_locked(key, tenant, Some(model), vector)
            .await
    }

    /// The vector UPSERT itself. Caller must hold `write_lock`.
    async fn store_entity_embedding_locked(
        &self,
        key: &str,
        tenant: &str,
        model: Option<&str>,
        vector: &[f32],
    ) -> Result<()> {
        self.conn
            .execute(
                r#"INSERT OR REPLACE INTO emmo_embedding
                   (key, tenant, model, dim, vector)
                   VALUES (?1, ?2, ?3, ?4, ?5)"#,
                [
                    Value::Text(key.to_string()),
                    Value::Text(tenant.to_string()),
                    model.map_or(Value::Null, |model| Value::Text(model.to_string())),
                    Value::Integer(vector.len() as i64),
                    Value::Blob(prism_embed::vec_to_le_bytes(vector)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Store a caller-precomputed model batch, resolving each display name
    /// to every matching typed entity row in `tenant`.
    ///
    /// The vectors and names are positional peers and must have equal length.
    /// Canonical duplicate names keep their first vector, matching
    /// [`Self::embed_and_store_names`]. The whole document costs one SQL
    /// `INSERT .. SELECT` round trip after the caller's one model batch; it
    /// never performs one embedding or one database query per name.
    pub async fn store_precomputed_name_embeddings(
        &self,
        names: &[String],
        vectors: &[Vec<f32>],
        tenant: &str,
        model: &str,
    ) -> Result<usize> {
        if names.len() != vectors.len() {
            bail!(
                "embedding backend returned {} vectors for {} names",
                vectors.len(),
                names.len()
            );
        }
        if model.trim().is_empty() {
            bail!("embedding model id must not be empty");
        }
        if names.is_empty() {
            return Ok(0);
        }

        let mut seen = std::collections::HashSet::new();
        let inputs: Vec<(&String, &Vec<f32>)> = names
            .iter()
            .zip(vectors)
            .filter(|(name, _)| seen.insert(canonical_key(name)))
            .collect();
        let dimensions = inputs[0].1.len();
        for (name, vector) in &inputs {
            validate_model_vector(vector, model, name)?;
        }
        if let Some((name, vector)) = inputs.iter().find(|(_, vector)| vector.len() != dimensions) {
            bail!(
                "embedding model `{model}` returned mixed dimensions in one batch: \
                 expected {dimensions}, got {} for `{name}`",
                vector.len()
            );
        }

        let _same_handle_guard = self.write_lock.lock().await;
        let mut params = Vec::with_capacity(inputs.len() * 4 + 2);
        let mut value_rows = Vec::with_capacity(inputs.len());
        for (name, vector) in inputs {
            let start = params.len() + 1;
            value_rows.push(format!(
                "(?{start}, ?{}, ?{}, ?{})",
                start + 1,
                start + 2,
                start + 3
            ));
            params.push(Value::Text(name.clone()));
            params.push(Value::Text(canonical_key(name)));
            params.push(Value::Integer(vector.len() as i64));
            params.push(Value::Blob(prism_embed::vec_to_le_bytes(vector)));
        }
        let tenant_param = params.len() + 1;
        params.push(Value::Text(tenant.to_string()));
        let model_param = params.len() + 1;
        params.push(Value::Text(model.to_string()));
        let sql = format!(
            "WITH inputs(name, canonical_name, dim, vector) AS (VALUES {}) \
             INSERT INTO emmo_embedding(key, tenant, model, dim, vector) \
             SELECT entity.key, ?{tenant_param}, ?{model_param}, inputs.dim, inputs.vector \
             FROM inputs \
             JOIN emmo_entity entity \
               ON entity.tenant = ?{tenant_param} \
              AND entity.canonical_name = inputs.canonical_name \
             ON CONFLICT(key) DO UPDATE SET \
               tenant = excluded.tenant, model = excluded.model, \
               dim = excluded.dim, vector = excluded.vector",
            value_rows.join(", ")
        );
        let stored = self.conn.execute(&sql, params).await?;
        Ok(stored as usize)
    }

    /// Embed the distinct subject/object names of `facts` with `backend`
    /// and store one vector per matching `emmo_entity` row. Names are
    /// resolved to their label-qualified keys via the entity table itself
    /// (no duplicate of `write_fact`'s kind→label routing), so names that
    /// never landed there are skipped. Returns the number of vectors stored.
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
        // The write lock is acquired inside this call, AFTER the model pass;
        // model inference must never hold the store's shared write lock.
        self.store_precomputed_name_embeddings(&names, &vectors, tenant, backend.id())
            .await
    }

    /// Best-effort entity embedding for freshly written facts: builds the
    /// configured `prism-embed` backend (on the blocking pool for snapshot
    /// verification and ONNX initialization) and stores one vector per
    /// entity. Runtime never acquires model files. Failures are logged and
    /// swallowed — an ingest must never fail because of the embedding model.
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

    /// Inventory the physically comparable embedding partitions stored for
    /// `tenant`. Dimensions are read from the vector blobs themselves, not
    /// trusted from the advisory `dim` column. A `None` model is an explicit
    /// legacy/unattributed partition and must not be used as the current
    /// model's validation geometry.
    pub async fn entity_embedding_partitions(
        &self,
        tenant: &str,
    ) -> Result<Vec<EmbeddingPartition>> {
        let mut rows = self
            .conn
            .query(
                "SELECT model, LENGTH(vector), COUNT(*) \
                 FROM emmo_embedding WHERE tenant = ?1 \
                 GROUP BY model, LENGTH(vector) \
                 ORDER BY model IS NOT NULL, model, LENGTH(vector)",
                [Value::Text(tenant.to_string())],
            )
            .await?;
        let mut partitions = Vec::new();
        while let Some(row) = rows.next().await? {
            let model = get_opt_str(&row, 0)?;
            let bytes = row
                .get_value(1)?
                .as_integer()
                .copied()
                .ok_or_else(|| anyhow::anyhow!("stored embedding vector has no byte length"))?;
            if bytes < 0 || bytes % 4 != 0 {
                bail!("stored embedding vector has invalid byte length {bytes}");
            }
            let count =
                row.get_value(2)?.as_integer().copied().ok_or_else(|| {
                    anyhow::anyhow!("embedding partition count is not an integer")
                })?;
            if count < 0 {
                bail!("embedding partition count is negative: {count}");
            }
            partitions.push(EmbeddingPartition {
                model,
                dimensions: (bytes / 4) as usize,
                count: count as usize,
            });
        }
        Ok(partitions)
    }

    /// Count how much of a tenant's semantically embeddable entity graph is
    /// represented in one model/dimension partition. Store-owned synthetic
    /// `Measurement` reification nodes are excluded from both sides only when
    /// they have no class IRI and participate in the generated
    /// `HAS_MEASUREMENT`/`OF_PROPERTY` edge shape. A real ontology may
    /// legitimately declare a class stored under label `Measurement`; label
    /// alone must never hide that instance from coverage.
    pub async fn entity_geometry_coverage(
        &self,
        tenant: &str,
        model: &str,
        dimensions: usize,
    ) -> Result<EntityGeometryCoverage> {
        if model.trim().is_empty() {
            bail!("embedding model id must not be empty");
        }
        let mut rows = self
            .conn
            .query(
                "SELECT \
                    (SELECT COUNT(*) FROM emmo_entity candidate \
                     WHERE candidate.tenant = ?1 AND NOT ( \
                       candidate.label = 'Measurement' \
                       AND candidate.class_iri IS NULL \
                       AND EXISTS (SELECT 1 FROM emmo_edge incoming \
                         WHERE incoming.tenant = ?1 \
                           AND incoming.target_key = candidate.key \
                           AND incoming.rel_type = 'HAS_MEASUREMENT') \
                       AND EXISTS (SELECT 1 FROM emmo_edge outgoing \
                         WHERE outgoing.tenant = ?1 \
                           AND outgoing.source_key = candidate.key \
                           AND outgoing.rel_type = 'OF_PROPERTY') \
                     )), \
                    (SELECT COUNT(DISTINCT embedding.key) \
                     FROM emmo_embedding embedding \
                     JOIN emmo_entity entity ON entity.key = embedding.key \
                     WHERE embedding.tenant = ?1 AND entity.tenant = ?1 \
                       AND NOT ( \
                         entity.label = 'Measurement' \
                         AND entity.class_iri IS NULL \
                         AND EXISTS (SELECT 1 FROM emmo_edge incoming \
                           WHERE incoming.tenant = ?1 \
                             AND incoming.target_key = entity.key \
                             AND incoming.rel_type = 'HAS_MEASUREMENT') \
                         AND EXISTS (SELECT 1 FROM emmo_edge outgoing \
                           WHERE outgoing.tenant = ?1 \
                             AND outgoing.source_key = entity.key \
                             AND outgoing.rel_type = 'OF_PROPERTY') \
                       ) \
                       AND embedding.model = ?2 \
                       AND LENGTH(embedding.vector) = ?3 * 4)",
                [
                    Value::Text(tenant.to_string()),
                    Value::Text(model.to_string()),
                    Value::Integer(dimensions as i64),
                ],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("entity geometry coverage query returned no row"))?;
        let count_at = |index, field: &str| -> Result<usize> {
            let count = row
                .get_value(index)?
                .as_integer()
                .copied()
                .ok_or_else(|| anyhow::anyhow!("{field} is not an integer"))?;
            usize::try_from(count).map_err(|_| anyhow::anyhow!("{field} is negative: {count}"))
        };
        Ok(EntityGeometryCoverage {
            entities: count_at(0, "entity count")?,
            compatible_embeddings: count_at(1, "compatible embedding count")?,
        })
    }

    /// Compare a batch of proposed entity vectors with the matching stored
    /// model partition, returning raw cosine distances only.
    ///
    /// One `WITH probes .. VALUES` statement handles the whole document.
    /// `max_distance` and the per-probe result cap are caller policy; this
    /// storage layer has no semantic threshold and performs no graph writes.
    pub async fn entity_geometry_neighbors(
        &self,
        probes: &[EntityGeometryProbe],
        tenant: &str,
        model: &str,
        max_distance: f64,
        max_neighbors_per_probe: usize,
    ) -> Result<Vec<EntityGeometryNeighbor>> {
        validate_geometry_request(
            probes.iter().map(|probe| probe.probe_id),
            model,
            max_distance,
        )?;
        if probes.is_empty() || max_neighbors_per_probe == 0 {
            return Ok(Vec::new());
        }

        let (probe_values, mut params) = entity_geometry_probe_values(probes);
        let tenant_param = params.len() + 1;
        params.push(Value::Text(tenant.to_string()));
        let model_param = params.len() + 1;
        params.push(Value::Text(model.to_string()));
        let distance_param = params.len() + 1;
        params.push(Value::Real(max_distance));
        let limit_param = params.len() + 1;
        params.push(Value::Integer(max_neighbors_per_probe as i64));
        let sql = format!(
            "WITH probes( \
                probe_id, canonical_name, lexical_name, storage_label, dim, vector \
             ) AS (VALUES {probe_values}), \
             candidates AS ( \
               SELECT probes.probe_id, entity.key, entity.name, entity.label, \
                      entity.entity_type, entity.class_iri, \
                      entity.lexical_name = probes.lexical_name AS exact_lexical, \
                      vector_distance_cos(embedding.vector, probes.vector) AS distance \
               FROM probes \
               JOIN emmo_embedding embedding \
                ON embedding.tenant = ?{tenant_param} \
                AND embedding.model = ?{model_param} \
                AND LENGTH(embedding.vector) = probes.dim * 4 \
               JOIN emmo_entity entity \
                 ON entity.key = embedding.key \
                AND entity.tenant = ?{tenant_param} \
             ), ranked AS ( \
               SELECT probe_id, key, name, label, entity_type, class_iri, distance, \
                      ROW_NUMBER() OVER ( \
                        PARTITION BY probe_id ORDER BY exact_lexical DESC, distance, key \
                      ) AS neighbor_rank \
               FROM candidates WHERE distance <= ?{distance_param} \
             ) \
             SELECT probe_id, name, label, entity_type, class_iri, distance \
             FROM ranked WHERE neighbor_rank <= ?{limit_param} \
             ORDER BY probe_id, distance, key"
        );
        let mut rows = self.conn.query(&sql, params).await?;
        let mut neighbors = Vec::new();
        while let Some(row) = rows.next().await? {
            neighbors.push(EntityGeometryNeighbor {
                probe_id: geometry_probe_id(&row, 0)?,
                name: get_str(&row, 1)?,
                storage_label: get_str(&row, 2)?,
                entity_type: get_opt_str(&row, 3)?,
                class_iri: get_opt_str(&row, 4)?,
                distance: geometry_number(&row, 5, "entity cosine distance")?,
            });
        }
        Ok(neighbors)
    }

    /// Measure each probe against every non-NULL ontology class region.
    /// For each class the result averages its nearest
    /// `neighbors_per_class` stored exemplars, a caller-selected robustness
    /// parameter. This is one batched SQL statement and never changes type
    /// assignments or any other graph state.
    pub async fn class_region_distances(
        &self,
        probes: &[EntityGeometryProbe],
        tenant: &str,
        model: &str,
        neighbors_per_class: usize,
    ) -> Result<Vec<ClassRegionDistance>> {
        validate_geometry_request(
            probes.iter().map(|probe| probe.probe_id),
            model,
            f64::INFINITY,
        )?;
        if probes.is_empty() || neighbors_per_class == 0 {
            return Ok(Vec::new());
        }

        let (probe_values, mut params) = entity_geometry_probe_values(probes);
        let tenant_param = params.len() + 1;
        params.push(Value::Text(tenant.to_string()));
        let model_param = params.len() + 1;
        params.push(Value::Text(model.to_string()));
        let neighbors_param = params.len() + 1;
        params.push(Value::Integer(neighbors_per_class as i64));
        let sql = format!(
            "WITH probes( \
                probe_id, canonical_name, lexical_name, storage_label, dim, vector \
             ) AS (VALUES {probe_values}), \
             raw_distances AS ( \
               SELECT probes.probe_id, entity.key, entity.entity_type, \
                      entity.class_iri, \
                      vector_distance_cos(embedding.vector, probes.vector) AS distance \
               FROM probes \
               JOIN emmo_embedding embedding \
                ON embedding.tenant = ?{tenant_param} \
                AND embedding.model = ?{model_param} \
                AND LENGTH(embedding.vector) = probes.dim * 4 \
               JOIN emmo_entity entity \
                 ON entity.key = embedding.key \
                AND entity.tenant = ?{tenant_param} \
                AND entity.class_iri IS NOT NULL \
                AND NOT ( \
                    probes.storage_label IS NOT NULL \
                    AND entity.canonical_name = probes.canonical_name \
                    AND entity.label = probes.storage_label \
                ) \
             ), ranked AS ( \
               SELECT probe_id, key, entity_type, class_iri, distance, \
                      COUNT(*) OVER ( \
                        PARTITION BY probe_id, class_iri \
                      ) AS exemplar_count, \
                      ROW_NUMBER() OVER ( \
                        PARTITION BY probe_id, class_iri ORDER BY distance, key \
                      ) AS exemplar_rank \
               FROM raw_distances \
             ) \
             SELECT probe_id, MIN(entity_type), class_iri, \
                    MAX(exemplar_count), AVG(distance) \
             FROM ranked WHERE exemplar_rank <= ?{neighbors_param} \
             GROUP BY probe_id, class_iri \
             ORDER BY probe_id, AVG(distance), class_iri"
        );
        let mut rows = self.conn.query(&sql, params).await?;
        let mut distances = Vec::new();
        while let Some(row) = rows.next().await? {
            let exemplars = row
                .get_value(3)?
                .as_integer()
                .copied()
                .ok_or_else(|| anyhow::anyhow!("class exemplar count is not an integer"))?;
            if exemplars < 0 {
                bail!("class exemplar count is negative: {exemplars}");
            }
            distances.push(ClassRegionDistance {
                probe_id: geometry_probe_id(&row, 0)?,
                entity_type: get_opt_str(&row, 1)?,
                class_iri: get_str(&row, 2)?,
                exemplars: exemplars as usize,
                mean_distance: geometry_number(&row, 4, "class-region mean distance")?,
            });
        }
        Ok(distances)
    }

    /// Find same-predicate assertions near a batch of proposed triples in
    /// joint subject/object space. Both endpoint distances must satisfy the
    /// cutoff independently, and results are bounded per probe before they
    /// leave Turso. The method reports raw prior evidence and never writes.
    pub async fn triple_geometry_neighbors(
        &self,
        probes: &[TripleGeometryProbe],
        tenant: &str,
        model: &str,
        max_distance: f64,
        max_neighbors_per_probe: usize,
    ) -> Result<Vec<TripleGeometryNeighbor>> {
        validate_geometry_request(
            probes.iter().map(|probe| probe.probe_id),
            model,
            max_distance,
        )?;
        if probes.is_empty() || max_neighbors_per_probe == 0 {
            return Ok(Vec::new());
        }

        let (probe_values, mut params) = triple_geometry_probe_values(probes);
        let tenant_param = params.len() + 1;
        params.push(Value::Text(tenant.to_string()));
        let model_param = params.len() + 1;
        params.push(Value::Text(model.to_string()));
        let distance_param = params.len() + 1;
        params.push(Value::Real(max_distance));
        let limit_param = params.len() + 1;
        params.push(Value::Integer(max_neighbors_per_probe as i64));
        let sql = format!(
            "WITH probes( \
                probe_id, predicate, subject_dim, subject_vector, \
                object_dim, object_vector \
             ) AS (VALUES {probe_values}), endpoint_distances AS ( \
               SELECT probes.probe_id, assertion.id, assertion.subject, \
                      assertion.predicate, assertion.object, assertion.value, \
                      assertion.unit, assertion.confidence, \
                      vector_distance_cos( \
                          subject_embedding.vector, probes.subject_vector \
                      ) AS subject_distance, \
                      vector_distance_cos( \
                          object_embedding.vector, probes.object_vector \
                      ) AS object_distance \
               FROM probes \
               JOIN prov_assertion assertion \
                 ON assertion.tenant = ?{tenant_param} \
                AND assertion.predicate = probes.predicate \
                AND assertion.conditions_json = '[]' \
               JOIN emmo_entity subject_entity \
                 ON subject_entity.tenant = ?{tenant_param} \
                AND subject_entity.canonical_name = assertion.subject_canonical \
               JOIN emmo_embedding subject_embedding \
                 ON subject_embedding.key = subject_entity.key \
                AND subject_embedding.tenant = ?{tenant_param} \
                AND subject_embedding.model = ?{model_param} \
                AND LENGTH(subject_embedding.vector) = probes.subject_dim * 4 \
               JOIN emmo_entity object_entity \
                 ON object_entity.tenant = ?{tenant_param} \
                AND object_entity.canonical_name = assertion.object_canonical \
               JOIN emmo_embedding object_embedding \
                 ON object_embedding.key = object_entity.key \
                AND object_embedding.tenant = ?{tenant_param} \
                AND object_embedding.model = ?{model_param} \
                AND LENGTH(object_embedding.vector) = probes.object_dim * 4 \
             ), deduplicated AS ( \
               SELECT probe_id, id, subject, predicate, object, value, unit, \
                      confidence, MIN(subject_distance) AS subject_distance, \
                      MIN(object_distance) AS object_distance \
               FROM endpoint_distances \
               GROUP BY probe_id, id, subject, predicate, object, value, unit, confidence \
             ), scored AS ( \
               SELECT *, CASE \
                   WHEN subject_distance >= object_distance THEN subject_distance \
                   ELSE object_distance \
               END AS distance \
               FROM deduplicated \
               WHERE subject_distance <= ?{distance_param} \
                 AND object_distance <= ?{distance_param} \
             ), ranked AS ( \
               SELECT *, ROW_NUMBER() OVER ( \
                   PARTITION BY probe_id ORDER BY distance, id \
               ) AS neighbor_rank \
               FROM scored \
             ) \
             SELECT probe_id, subject, predicate, object, value, unit, confidence, \
                    subject_distance, object_distance, distance \
             FROM ranked WHERE neighbor_rank <= ?{limit_param} \
             ORDER BY probe_id, distance, id"
        );
        let mut rows = self.conn.query(&sql, params).await?;
        let mut neighbors = Vec::new();
        while let Some(row) = rows.next().await? {
            neighbors.push(TripleGeometryNeighbor {
                probe_id: geometry_probe_id(&row, 0)?,
                subject: get_str(&row, 1)?,
                predicate: get_str(&row, 2)?,
                object: get_str(&row, 3)?,
                value: geometry_optional_number(&row, 4, "assertion value")?,
                unit: get_opt_str(&row, 5)?,
                confidence: geometry_optional_number(&row, 6, "assertion confidence")?,
                subject_distance: geometry_number(&row, 7, "triple subject distance")?,
                object_distance: geometry_number(&row, 8, "triple object distance")?,
                distance: geometry_number(&row, 9, "triple pair distance")?,
            });
        }
        Ok(neighbors)
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
                // SQLite's vector extension may return a distance a few
                // ulps outside its documented [0, 2] interval. Keep the
                // public cosine-similarity contract bounded despite that
                // numeric approximation.
                similarity: (1.0 - distance as f32).clamp(-1.0, 1.0),
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

fn validate_model_vector(vector: &[f32], model: &str, identity: &str) -> Result<()> {
    if vector.is_empty() {
        bail!("embedding model `{model}` returned a zero-dimensional vector for `{identity}`");
    }
    if vector.iter().any(|component| !component.is_finite()) {
        bail!("embedding model `{model}` returned a non-finite vector for `{identity}`");
    }
    if vector.iter().all(|component| *component == 0.0) {
        bail!("embedding model `{model}` returned the zero vector for `{identity}`");
    }
    Ok(())
}

/// Tenant every local single-user write uses, and the tenant a default
/// read scope always includes (see
/// [`ProvenanceStore::default_read_tenants`]).
pub const LOCAL_TENANT: &str = "local";

fn validate_geometry_request(
    probe_ids: impl IntoIterator<Item = usize>,
    model: &str,
    max_distance: f64,
) -> Result<()> {
    if model.trim().is_empty() {
        bail!("embedding model id must not be empty");
    }
    if max_distance.is_nan() || max_distance < 0.0 {
        bail!("geometry distance cutoff must be non-negative, got {max_distance}");
    }
    let mut seen = std::collections::HashSet::new();
    for probe_id in probe_ids {
        if !seen.insert(probe_id) {
            bail!("duplicate geometry probe id {probe_id}");
        }
    }
    Ok(())
}

/// Build the VALUES clause and its parameters once for a whole entity-probe
/// batch. Each vector is exactly one BLOB parameter; dimensions are carried
/// separately so incompatible stored partitions are filtered before Turso's
/// vector function is evaluated.
fn entity_geometry_probe_values(probes: &[EntityGeometryProbe]) -> (String, Vec<Value>) {
    let mut params = Vec::with_capacity(probes.len() * 6);
    let mut rows = Vec::with_capacity(probes.len());
    for probe in probes {
        let start = params.len() + 1;
        rows.push(format!(
            "(?{start}, ?{}, ?{}, ?{}, ?{}, ?{})",
            start + 1,
            start + 2,
            start + 3,
            start + 4,
            start + 5
        ));
        params.push(Value::Integer(probe.probe_id as i64));
        params.push(Value::Text(canonical_key(&probe.name)));
        params.push(Value::Text(lexical_key(&probe.name)));
        params.push(
            probe
                .storage_label
                .as_ref()
                .map_or(Value::Null, |label| Value::Text(label.clone())),
        );
        params.push(Value::Integer(probe.vector.len() as i64));
        params.push(Value::Blob(prism_embed::vec_to_le_bytes(&probe.vector)));
    }
    (rows.join(", "), params)
}

/// Triple-probe counterpart of [`entity_geometry_probe_values`]. Subject and
/// object are independent BLOB parameters because they are compared with
/// different stored entity roles in the same SQL statement.
fn triple_geometry_probe_values(probes: &[TripleGeometryProbe]) -> (String, Vec<Value>) {
    let mut params = Vec::with_capacity(probes.len() * 6);
    let mut rows = Vec::with_capacity(probes.len());
    for probe in probes {
        let start = params.len() + 1;
        rows.push(format!(
            "(?{start}, ?{}, ?{}, ?{}, ?{}, ?{})",
            start + 1,
            start + 2,
            start + 3,
            start + 4,
            start + 5
        ));
        params.push(Value::Integer(probe.probe_id as i64));
        params.push(Value::Text(probe.predicate.clone()));
        params.push(Value::Integer(probe.subject_vector.len() as i64));
        params.push(Value::Blob(prism_embed::vec_to_le_bytes(
            &probe.subject_vector,
        )));
        params.push(Value::Integer(probe.object_vector.len() as i64));
        params.push(Value::Blob(prism_embed::vec_to_le_bytes(
            &probe.object_vector,
        )));
    }
    (rows.join(", "), params)
}

fn geometry_probe_id(row: &turso::Row, index: usize) -> Result<usize> {
    let id = row
        .get_value(index)?
        .as_integer()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("geometry probe id is not an integer"))?;
    usize::try_from(id).map_err(|_| anyhow::anyhow!("geometry probe id is negative: {id}"))
}

fn geometry_number(row: &turso::Row, index: usize, field: &str) -> Result<f64> {
    let value = match row.get_value(index)? {
        Value::Real(value) => value,
        Value::Integer(value) => value as f64,
        value => bail!("{field} is not numeric: {value:?}"),
    };
    if !value.is_finite() {
        bail!("{field} is not finite: {value}");
    }
    Ok(value)
}

fn geometry_optional_number(row: &turso::Row, index: usize, field: &str) -> Result<Option<f64>> {
    match row.get_value(index)? {
        Value::Null => Ok(None),
        Value::Real(value) if value.is_finite() => Ok(Some(value)),
        Value::Integer(value) => Ok(Some(value as f64)),
        value => bail!("{field} is not a finite number or NULL: {value:?}"),
    }
}

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

    /// CONTRACT CHANGE: bare `write_fact` no longer resolves a typed graph
    /// shape from the kind STRING — the store's closed kind→(class, edge)
    /// table is gone, and the shape is the ontology's declaration resolved
    /// by the caller. Tests that pin typed shapes write through the same
    /// shape-resolving call every production caller uses, with the EMMO
    /// declaration standing in for the EMMO adapter.
    async fn write_emmo_shaped<F: FactPayload>(
        store: &ProvenanceStore,
        fact: &F,
        prov: &LocalProvenance,
    ) {
        store
            .write_fact_with_classification(
                fact,
                prov,
                test_ontology_classification(),
                fact.to_local_fact()
                    .kind
                    .as_deref()
                    .and_then(FactGraphShape::emmo),
            )
            .await
            .unwrap();
    }

    const TEST_ONTOLOGY_SHA: &str =
        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn test_ontology_classification() -> OntologyClassification<'static> {
        OntologyClassification {
            version_iri: "urn:test:ontology:citation",
            artifact_sha256: TEST_ONTOLOGY_SHA,
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
        // CONTRACT CHANGE: this test used to pin the store's CLOSED
        // kind→(class, edge) table by writing bare `write_fact` calls and
        // letting the kind STRING select the shape. The store no longer owns
        // that table — the graph shape is the ACTIVE ontology's declaration
        // (`FactGraphShape::emmo` here stands in for the EMMO adapter every
        // production caller resolves through). Each kind is now written WITH
        // its declared shape, exactly as pipeline/repair/papers do, and the
        // same edges must result.
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        let classification = OntologyClassification {
            version_iri: "urn:test:ontology:shapes",
            artifact_sha256: TEST_ONTOLOGY_SHA,
        };

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
            store
                .write_fact_with_classification(
                    &f,
                    &prov,
                    classification,
                    FactGraphShape::emmo(kind),
                )
                .await
                .unwrap();

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

        // CONTRACT CHANGE: a fact whose kind the ontology does NOT declare
        // (a legal ontology's "obligation") used to hit the closed table's
        // `_` arm and degrade to `Entity` endpoints while EMMO kinds got
        // first-class shapes. The table is the ontology's now: with no
        // declared shape the fact is still kept — as a generic edge — and
        // the ONTOLOGY declares its own shape to make the kind typed.
        let foreign = fact("obligation", "Contract 7", "imposes", "Duty to pay");
        store
            .write_fact_with_classification(&foreign, &prov, classification, None)
            .await
            .unwrap();
        let obligation_tr = store
            .get_neighbors("Contract 7", None, "t1", 10)
            .await
            .unwrap();
        assert!(
            obligation_tr.edges.iter().any(|e| e.rel_type == "imposes"),
            "an undeclared kind is kept as a generic edge, never dropped"
        );

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

    /// CONTRACT CHANGE (annotate, don't refuse): `kind` is only a graph
    /// shape hint. A model may label a value-less assertion "measurement";
    /// without a number there is no Measurement node to construct, but the
    /// assertion and its generic predicate edge must still be retained.
    #[tokio::test]
    async fn a_valueless_measurement_hint_is_stored_as_a_generic_edge() {
        // CONTRACT CHANGE: this legacy `kind` hint used to make the writer
        // return success without storing anything. It now falls back to the
        // representable generic relation and keeps the verification note.
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let fact = MaterialFact {
            subject: "ValueLessSubject".into(),
            predicate: "has_reported_property".into(),
            object: "ductility".into(),
            value: None,
            unit: None,
            conditions: vec![],
            confidence: Some(0.7),
            kind: Some("measurement".into()),
            evidence_class: EvidenceClass::Research,
            verification: Some(VerificationStatus::ModelAsserted),
            verification_reason: Some("the proposal supplied no numeric value".into()),
        };
        store.write_fact(&fact, &test_prov()).await.unwrap();

        let id = conditioned_assertion_id(
            "t1",
            &fact.subject,
            &fact.predicate,
            &fact.object,
            None,
            None,
            &[],
        )
        .unwrap();
        let stored = store
            .assertion_by_id(&id)
            .await
            .unwrap()
            .expect("the value-less assertion must not disappear");
        assert_eq!(stored.verification_status, fact.verification);
        assert_eq!(stored.verification_reason, fact.verification_reason);

        let traversal = store
            .get_neighbors(&fact.subject, None, "t1", 10)
            .await
            .unwrap();
        assert!(
            traversal
                .edges
                .iter()
                .any(|edge| edge.rel_type == fact.predicate),
            "the value-less proposal must use the generic predicate edge"
        );
        assert!(
            traversal
                .nodes
                .iter()
                .all(|node| node.label != "Measurement"),
            "only a numeric measurement may mint a Measurement node"
        );
    }

    #[tokio::test]
    async fn measurement_unit_terms_are_nonempty_and_preserved() {
        // CONTRACT CHANGE: persistence no longer decides whether a numeric
        // value semantically requires a unit. It preserves absence, rejects
        // only a present-but-empty term, and keeps every non-empty term exact.
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        // No unit at all is a representable source shape, not a Rust verdict.
        let mut unitless = fact("measurement", "Ti-6Al-4V", "has_measurement", "UTS");
        unitless.value = Some(880.0);
        write_emmo_shaped(&store, &unitless, &prov).await;
        let recalled = store.recall_with_context("UTS", "t1", 10).await.unwrap();
        assert_eq!(recalled.len(), 1, "{recalled:?}");
        assert_eq!(recalled[0].unit, None);

        // Whitespace carries no source or ontology identity and is refused.
        let mut empty = fact("measurement", "Ti-6Al-4V", "has_measurement", "hardness");
        empty.value = Some(349.0);
        empty.unit = Some("  \t".into());
        let err = store
            .write_fact(&empty, &prov)
            .await
            .expect_err("an empty unit term must be refused, never stored");
        assert!(
            format!("{err:#}").contains("must not be empty"),
            "the refusal must name the cause: {err:#}"
        );

        // The present-but-empty fact left no trace; the absent-term fact did.
        assert!(
            store
                .graph_search("hardness", "t1", 10)
                .await
                .unwrap()
                .is_empty(),
            "a refused empty term must write nothing"
        );

        let unit_iri = "https://pharma.example/ontology/unit/mg-per-kg";
        let mut ontology_term = fact("measurement", "compound", "ex:hasDose", "dose");
        ontology_term.value = Some(5.0);
        ontology_term.unit = Some(unit_iri.into());
        write_emmo_shaped(&store, &ontology_term, &prov).await;
        let recalled = store.recall_with_context("dose", "t1", 10).await.unwrap();
        assert_eq!(recalled.len(), 1, "{recalled:?}");
        assert_eq!(recalled[0].value, Some(5.0));
        assert_eq!(
            recalled[0].unit.as_deref(),
            Some(unit_iri),
            "the assertion must retain the exact customer ontology term"
        );
        // BOTH stored shapes agree: `recall_with_context` reads the
        // assertion row; the synthetic Measurement NODE keeps its own copy
        // in `props_json`, written separately — a regression could decouple
        // them (canonical assertion, raw node) and the recall check alone
        // would never see it.
        let props: serde_json::Value = serde_json::from_str(
            &query_str(
                &store,
                "SELECT props_json FROM emmo_entity \
                 WHERE label = 'Measurement' AND name LIKE 'meas_compound_dose_%'",
            )
            .await,
        )
        .unwrap();
        assert_eq!(
            props["unit"], unit_iri,
            "the Measurement node must carry the same exact term"
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
                    kind.and_then(FactGraphShape::emmo),
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

            // CONTRACT CHANGE: the second half used to prove the kind STRING
            // alone selected EMMO labels through bare `write_fact`. The
            // store no longer holds that table; the shape is declared by the
            // ontology and resolved by the caller. Writing with the shape
            // through the unclassified path must produce the SAME established
            // EMMO labels, byte-for-byte — that is what this half pins now.
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
            }
            store
                .write_fact_with_classification(
                    &f,
                    &prov,
                    OntologyClassification {
                        version_iri: "urn:test:ontology:legacy",
                        artifact_sha256: TEST_ONTOLOGY_SHA,
                    },
                    kind.and_then(FactGraphShape::emmo),
                )
                .await
                .unwrap();
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
                FactGraphShape::emmo("phase"),
            )
            .await
            .expect_err("an empty node label must be refused");
        assert!(format!("{err:#}").contains("empty node label"), "{err:#}");
    }

    #[tokio::test]
    async fn ontology_bound_paper_fact_keeps_partial_class_identity_and_generic_edge() {
        const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let mut paper_fact = verified_fact("Wirkstoff A", Some(VerificationStatus::Grounded), None);
        paper_fact.predicate = "urn:pharma:behandelt".into();
        paper_fact.object = "Krankheit B".into();
        paper_fact.value = None;
        paper_fact.unit = None;
        // CONTRACT CHANGE: a paper-agent write ignores this legacy domain
        // hint; the active ontology's canonical predicate is the graph edge.
        paper_fact.kind = Some("phase".into());
        let citation =
            SourceCitation::new(1, 1, "Wirkstoff A behandelt Krankheit B.", SHA, None).unwrap();

        store
            .write_ontology_bound_fact_with_citation(
                &paper_fact,
                &prov_from("doc:pharma", "act-pharma"),
                EvidenceClass::Research,
                OntologyBoundFactNodes {
                    subject: Some(ClassifiedNode {
                        entity_type: "Wirkstoff",
                        storage_label: "Wirkstoff",
                        class_iri: "urn:pharma:Wirkstoff",
                    }),
                    object: None,
                },
                OntologyClassification {
                    version_iri: "urn:pharma:ontology:v1",
                    artifact_sha256: SHA,
                },
                &citation,
            )
            .await
            .unwrap();

        let subject = store
            .graph_search("Wirkstoff A", "t1", 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let object = store
            .graph_search("Krankheit B", "t1", 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(subject.class_iri.as_deref(), Some("urn:pharma:Wirkstoff"));
        assert_eq!(object.label, "Entity");
        assert_eq!(
            query_str(
                &store,
                "SELECT rel_type FROM emmo_edge WHERE predicate = 'urn:pharma:behandelt'"
            )
            .await,
            "urn:pharma:behandelt"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_edge WHERE rel_type = 'HAS_PHASE'"
            )
            .await,
            0
        );
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
                FactGraphShape::emmo("phase"),
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
                FactGraphShape::emmo("phase"),
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
                FactGraphShape::emmo("phase"),
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
                FactGraphShape::emmo("phase"),
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
        write_emmo_shaped(
            &store,
            &fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            &prov,
        )
        .await;
        write_emmo_shaped(&store, &fact("phase", "alpha", "has_phase", "beta"), &prov).await;

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
        write_emmo_shaped(&store, &f, &prov).await;

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
        write_emmo_shaped(&store, &f, &prov).await;
        let props = query_str(
            &store,
            "SELECT props_json FROM emmo_edge WHERE rel_type = 'CONTAINS_ELEMENT'",
        )
        .await;
        let props: serde_json::Value = serde_json::from_str(&props).unwrap();
        assert_eq!(props["fraction"].as_f64(), Some(0.25));
    }

    /// The test above proves the fraction is STORED, by reading SQL directly.
    /// It proves nothing about whether anyone can get it back: the traversal
    /// that is the only production read path did not select the column, so
    /// every fraction was unreachable from the moment it was written.
    ///
    /// This drives `get_neighbors` — the real dispatch — so removing
    /// `e.props_json` from `EDGE_COLS` fails here instead of passing quietly.
    #[tokio::test]
    async fn traversal_returns_the_edge_attributes_it_stored() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut f = fact("contains", "Nb25Mo25Ta25W25", "contains", "Nb");
        f.value = Some(0.25);
        write_emmo_shaped(&store, &f, &prov).await;

        let tr = store
            .get_neighbors("Nb25Mo25Ta25W25", Some("CONTAINS_ELEMENT"), "t1", 10)
            .await
            .unwrap();

        let edge = tr
            .edges
            .iter()
            .find(|e| e.rel_type == "CONTAINS_ELEMENT")
            .expect("the composition edge must come back from the traversal");
        let props: serde_json::Value = serde_json::from_str(
            edge.props_json
                .as_deref()
                .expect("a stored fraction must reach the caller, not stay in the table"),
        )
        .unwrap();
        assert_eq!(
            props["fraction"].as_f64(),
            Some(0.25),
            "the fraction that was stored must be the fraction that is read"
        );
    }

    #[tokio::test]
    async fn processing_order_lands_in_edge_props() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut f = fact("processing", "Inconel 718", "processed_by", "annealing");
        f.value = Some(2.0);
        write_emmo_shaped(&store, &f, &prov).await;

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

        write_emmo_shaped(&store, &f, &prov).await;
        write_emmo_shaped(&store, &f, &prov).await;

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
            verification: None,
            verification_reason: None,
        };

        write_emmo_shaped(
            &store,
            &measurement(0.8, EvidenceClass::Research),
            &prov_from("doc:paper_a", "act_1"),
        )
        .await;
        // The SAME source re-recorded "better" but weaker: the assertion
        // keeps Research/0.8, so the graph must too.
        write_emmo_shaped(
            &store,
            &measurement(0.2, EvidenceClass::ReferenceValidated),
            &prov_from("doc:paper_a", "act_2"),
        )
        .await;

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
        write_emmo_shaped(
            &store,
            &measurement(0.8, EvidenceClass::Research),
            &prov_from("doc:paper_b", "act_3"),
        )
        .await;
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

    // ── corroboration: agreement is looser than identity ──
    //
    // Every case below is a REAL pair from the corpus measured 2026-08-26,
    // where the same measurement was stored as two mutually uncorroborating
    // facts because `assertion_id` hashes the predicate raw.

    #[test]
    fn the_ontology_iri_and_its_plain_label_name_one_concept() {
        assert_eq!(
            predicate_concept("hasSolidusTemperature"),
            "solidus temperature"
        );
        assert_eq!(
            predicate_concept("solidus temperature"),
            "solidus temperature"
        );
        assert_eq!(
            predicate_concept("hasLiquidusTemperature"),
            "liquidus temperature"
        );
        assert_eq!(
            predicate_concept("https://w3id.org/emmo#boilingTemperature"),
            "boiling temperature"
        );
    }

    #[test]
    fn a_lone_has_is_not_stripped_into_nothing() {
        // `has` as the ONLY word is the predicate, not a prefix.
        assert_eq!(predicate_concept("has"), "has");
        assert_eq!(predicate_concept("is"), "is");
    }

    #[test]
    fn the_same_measurement_written_two_ways_corroborates() {
        // ss 316l = 1658 K, stored under both spellings in the real corpus.
        let a = corroboration_key(
            "t",
            "SS 316L",
            "hasSolidusTemperature",
            Some(1658.0),
            Some("K"),
        );
        let b = corroboration_key(
            "t",
            "ss 316l",
            "solidus temperature",
            Some(1658.0),
            Some("K"),
        );
        assert!(a.is_some());
        assert_eq!(a, b, "these are the same measurement and must corroborate");

        // And the whole point: identity still tells them apart.
        assert_ne!(
            assertion_id("t", "SS 316L", "hasSolidusTemperature", "1658 K"),
            assertion_id("t", "ss 316l", "solidus temperature", "1658 K"),
            "assertion_id must stay EXACT — agreement is a separate question"
        );
    }

    #[test]
    fn a_different_number_never_corroborates() {
        let a = corroboration_key(
            "t",
            "SS 316L",
            "solidus temperature",
            Some(1658.0),
            Some("K"),
        );
        let b = corroboration_key(
            "t",
            "SS 316L",
            "solidus temperature",
            Some(1659.0),
            Some("K"),
        );
        assert_ne!(a, b, "1658 and 1659 are different measurements");
    }

    #[test]
    fn a_different_unit_never_corroborates() {
        let k = corroboration_key("t", "x", "melting point", Some(1658.0), Some("K"));
        let c = corroboration_key("t", "x", "melting point", Some(1658.0), Some("degC"));
        assert_ne!(k, c, "1658 K and 1658 degC are not the same measurement");
    }

    #[test]
    fn a_different_tenant_never_corroborates() {
        let a = corroboration_key("tenant-a", "x", "melting point", Some(9.0), Some("K"));
        let b = corroboration_key("tenant-b", "x", "melting point", Some(9.0), Some("K"));
        assert_ne!(a, b, "tenants must never corroborate each other");
    }

    #[test]
    fn a_fact_with_no_number_yields_no_key() {
        // Agreement between two free-text objects is a judgement, not a hash.
        // Returning a key here would manufacture false corroboration, and a
        // wrongly-green fact is worse than an honestly-red one.
        assert!(corroboration_key("t", "x", "described as", None, None).is_none());
        assert!(corroboration_key("t", "x", "y", Some(f64::NAN), Some("K")).is_none());
        assert!(corroboration_key("t", "x", "y", Some(f64::INFINITY), None).is_none());
    }

    #[test]
    fn trivial_numeric_spelling_does_not_split_a_measurement() {
        let a = corroboration_key("t", "x", "melting point", Some(1658.0), Some("K"));
        let b = corroboration_key("t", "x", "melting point", Some(1658.000000), Some("K"));
        assert_eq!(a, b);
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
            MATKG_NAMESPACE_VERSION,
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
    /// A STORE WRITTEN UNDER THE OLD DEMOTION RULE BECOMES READABLE ON OPEN.
    ///
    /// The measured damage: 21,109 of one corpus's 21,218 facts sat at
    /// `sample_disagreement`, which is not in the trusted set, so the default
    /// read hid them. Ingest now fails-to-promote instead of demoting, but that
    /// only helps FUTURE writes — a database already on disk stays hidden until
    /// something moves it, and no user should have to know that.
    ///
    /// Also pins the exception: a value-less row carrying a unit is
    /// `model_asserted`, not `cited_by_reader`, because that finding genuinely
    /// applies and was only masked by the disagreement stamp landing first.
    #[tokio::test]
    async fn a_legacy_sample_disagreement_store_becomes_readable_on_open() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        drop(store);
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            // Two legacy rows: an ordinary one, and the value-less-with-unit
            // shape that must NOT be promoted as far.
            conn.execute(
                r#"INSERT INTO prov_assertion
                   (id, subject, predicate, object, conditions_json, evidence_class,
                    confidence, corroborations, activity_id, source, agent, tenant,
                    value, unit, verification_status, verification_reason)
                   VALUES ('ordinary', 'steel', 'has_phase', 'bcc', '[]', 'research',
                           0.7, 1, 'act', 'x.pdf', 'agent', 't1', 5.0, NULL,
                           'sample_disagreement', 'proposed by 1 of 3 paper-reading samples'),
                          ('valueless', 'steel', 'has_speed', 'fast', '[]', 'research',
                           0.7, 1, 'act', 'x.pdf', 'agent', 't1', NULL, 'QUDT:MilliM-PER-SEC',
                           'sample_disagreement', 'proposed by 1 of 3 paper-reading samples')"#,
                (),
            )
            .await
            .unwrap();
            // The graph's denormalized copy, alongside another property that
            // must survive the rewrite.
            conn.execute(
                r#"INSERT INTO emmo_edge (id, source_key, target_key, rel_type, predicate,
                                          confidence, tenant, props_json)
                   VALUES ('e1', 'Matter:steel', 'Matter:bcc', 'has_phase', 'has_phase',
                           0.7, 't1',
                           '{"verification_status":"sample_disagreement","keep":"me"}')"#,
                (),
            )
            .await
            .unwrap();
            conn.execute("PRAGMA user_version = 5", ()).await.unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion WHERE verification_status = 'sample_disagreement'",
            )
            .await,
            0,
            "no stored row may still carry the retired status"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion \
                 WHERE id = 'ordinary' AND verification_status = 'cited_by_reader'",
            )
            .await,
            1,
            "an ordinary uncorroborated fact becomes trusted-but-unverified"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion \
                 WHERE id = 'valueless' AND verification_status = 'model_asserted'",
            )
            .await,
            1,
            "a value-less row carrying a unit must not be promoted past `model_asserted`"
        );
        // The original reason survives — it is the only record of how much
        // agreement the fact had — and the migration is auditable.
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM prov_assertion \
                 WHERE verification_reason LIKE '%1 of 3 paper-reading samples%' \
                   AND verification_reason LIKE '%[migrated:%'",
            )
            .await,
            2,
            "the original reason must be kept AND the migration marked"
        );
        // The graph copy moves too, and unrelated properties survive.
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM emmo_edge \
                 WHERE props_json LIKE '%\"verification_status\":\"cited_by_reader\"%' \
                   AND props_json LIKE '%\"keep\":\"me\"%'",
            )
            .await,
            1,
            "the denormalized edge copy must move without losing other properties"
        );
    }

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
            MATKG_NAMESPACE_VERSION,
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
            verification: None,
            verification_reason: None,
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

    #[tokio::test]
    async fn embedding_partition_inventory_distinguishes_legacy_and_model_vectors() {
        let db = TempDb::new();
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"CREATE TABLE emmo_embedding (
                    key TEXT PRIMARY KEY,
                    tenant TEXT,
                    dim INTEGER,
                    vector BLOB
                )"#,
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO emmo_embedding(key, tenant, dim, vector) \
                 VALUES (?1, 't1', 3, ?2)",
                [
                    Value::Text("legacy-key".into()),
                    Value::Blob(prism_embed::vec_to_le_bytes(&[1.0, 0.0, 0.0])),
                ],
            )
            .await
            .unwrap();
            conn.execute(
                &format!("PRAGMA user_version = {SAMPLE_DISAGREEMENT_RETIRED_VERSION}"),
                (),
            )
            .await
            .unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        store
            .store_entity_embedding_with_model("model-key", "t1", "test:model-v2", &[0.0, 1.0, 0.0])
            .await
            .unwrap();

        assert_eq!(
            store.entity_embedding_partitions("t1").await.unwrap(),
            [
                EmbeddingPartition {
                    model: None,
                    dimensions: 3,
                    count: 1,
                },
                EmbeddingPartition {
                    model: Some("test:model-v2".into()),
                    dimensions: 3,
                    count: 1,
                },
            ],
            "the migrated NULL partition must stay distinct from attributed vectors"
        );
    }

    #[tokio::test]
    async fn geometry_coverage_excludes_store_owned_measurement_nodes() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let mut measurement = fact("measurement", "Ti-6Al-4V", "has_measurement", "UTS");
        measurement.value = Some(880.0);
        measurement.unit = Some("QUDT:MegaPA".into());
        write_emmo_shaped(&store, &measurement, &test_prov()).await;
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity").await,
            3,
            "the storage shape must include its synthetic Measurement node"
        );

        store
            .store_precomputed_name_embeddings(
                &["Ti-6Al-4V".into(), "UTS".into()],
                &[vec![1.0, 0.0], vec![0.0, 1.0]],
                "t1",
                "test:coverage",
            )
            .await
            .unwrap();

        // A user ontology can legitimately persist an extracted class under
        // this same display label. Its class IRI and absence from the
        // generated two-edge reification shape keep it in the denominator.
        store
            .write_classified_entity(
                "reported measurement",
                ClassifiedNode {
                    entity_type: "Measurement",
                    storage_label: "Measurement",
                    class_iri: "https://example.test/Measurement",
                },
                None,
                "t1",
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .entity_geometry_coverage("t1", "test:coverage", 2)
                .await
                .unwrap(),
            EntityGeometryCoverage {
                entities: 3,
                compatible_embeddings: 2,
            },
            "only the generated node is excluded; a real Measurement class stays visible"
        );

        store
            .store_precomputed_name_embeddings(
                &["reported measurement".into()],
                &[vec![0.7, 0.3]],
                "t1",
                "test:coverage",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .entity_geometry_coverage("t1", "test:coverage", 2)
                .await
                .unwrap(),
            EntityGeometryCoverage {
                entities: 3,
                compatible_embeddings: 3,
            },
            "complete endpoint/class geometry must not be poisoned by reification"
        );
    }

    #[tokio::test]
    async fn batched_entity_and_class_geometry_are_model_scoped_and_canonical() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        for (name, entity_type, class_iri) in [
            ("Ti ", "Element", "https://example.test/Element"),
            ("Al", "Element", "https://example.test/Element"),
            ("LPBF", "Process", "https://example.test/Process"),
        ] {
            store
                .write_classified_entity(
                    name,
                    ClassifiedNode {
                        entity_type,
                        storage_label: entity_type,
                        class_iri,
                    },
                    None,
                    "t1",
                )
                .await
                .unwrap();
        }
        let names = vec!["Ti".into(), "Al".into(), "LPBF".into()];
        let vectors = vec![vec![1.0, 0.0], vec![0.8, 0.2], vec![0.0, 1.0]];
        assert_eq!(
            store
                .store_precomputed_name_embeddings(&names, &vectors, "t1", "test:geometry")
                .await
                .unwrap(),
            3,
            "canonical name resolution must find the stored `Ti ` row from input `Ti`"
        );

        let probes = vec![
            EntityGeometryProbe {
                probe_id: 7,
                name: "Ti".into(),
                storage_label: None,
                vector: vec![1.0, 0.0],
            },
            EntityGeometryProbe {
                probe_id: 9,
                name: "LPBF".into(),
                storage_label: None,
                vector: vec![0.0, 1.0],
            },
        ];
        let neighbors = store
            .entity_geometry_neighbors(&probes, "t1", "test:geometry", 2.0, 1)
            .await
            .unwrap();
        assert_eq!(neighbors.len(), 2, "one ranked neighbor per batched probe");
        assert_eq!(
            (neighbors[0].probe_id, neighbors[0].name.as_str()),
            (7, "Ti ")
        );
        assert_eq!(
            (neighbors[1].probe_id, neighbors[1].name.as_str()),
            (9, "LPBF")
        );
        assert!(
            neighbors
                .iter()
                .all(|neighbor| neighbor.distance.abs() < 1e-6)
        );

        let regions = store
            .class_region_distances(&probes, "t1", "test:geometry", 1)
            .await
            .unwrap();
        for probe_id in [7, 9] {
            assert!(
                regions.iter().any(|region| {
                    region.probe_id == probe_id
                        && region.class_iri == "https://example.test/Element"
                        && region.exemplars == 2
                }),
                "the class population must remain two when the mean uses one neighbor: {regions:?}"
            );
            assert!(
                regions.iter().any(|region| {
                    region.probe_id == probe_id
                        && region.class_iri == "https://example.test/Process"
                        && region.exemplars == 1
                }),
                "the batched class query omitted the Process region: {regions:?}"
            );
        }

        assert!(
            store
                .entity_geometry_neighbors(&probes, "t1", "other:model", 2.0, 4)
                .await
                .unwrap()
                .is_empty(),
            "geometry from another model partition must never be mixed in"
        );
    }

    #[tokio::test]
    async fn batched_triple_geometry_reports_prior_without_mutating_graph() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();
        for assertion in [
            fact("contains", "Alloy A", "contains", "Ti"),
            fact("contains", "Alloy B", "contains", "Al"),
        ] {
            store.write_fact(&assertion, &prov).await.unwrap();
        }
        // Re-ingest updates the entity display spelling while the immutable
        // assertion keeps its first spelling. Geometry must join canonical
        // identity, not orphan the assertion on exact display text.
        store
            .write_classified_entity(
                " alloy a ",
                ClassifiedNode {
                    entity_type: "Alloy",
                    storage_label: "Matter",
                    class_iri: "https://example.test/Alloy",
                },
                None,
                "t1",
            )
            .await
            .unwrap();
        let names = vec!["Alloy A".into(), "Ti".into(), "Alloy B".into(), "Al".into()];
        let vectors = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 1.0],
            vec![1.0, 1.0, 0.0],
        ];
        store
            .store_precomputed_name_embeddings(&names, &vectors, "t1", "test:triples")
            .await
            .unwrap();
        let assertions_before = count(&store, "SELECT COUNT(*) FROM prov_assertion").await;
        let entities_before = count(&store, "SELECT COUNT(*) FROM emmo_entity").await;

        let probes = vec![
            TripleGeometryProbe {
                probe_id: 2,
                predicate: "contains".into(),
                subject_vector: vectors[0].clone(),
                object_vector: vectors[1].clone(),
            },
            TripleGeometryProbe {
                probe_id: 4,
                predicate: "contains".into(),
                subject_vector: vectors[2].clone(),
                object_vector: vectors[3].clone(),
            },
        ];
        let neighbors = store
            .triple_geometry_neighbors(&probes, "t1", "test:triples", 1e-6, 4)
            .await
            .unwrap();
        assert_eq!(
            neighbors.len(),
            2,
            "each exact batched probe needs one prior"
        );
        assert!(neighbors.iter().any(|neighbor| {
            neighbor.probe_id == 2
                && neighbor.subject == "Alloy A"
                && neighbor.object == "Ti"
                && neighbor.confidence == Some(0.8)
        }));
        assert!(neighbors.iter().any(|neighbor| {
            neighbor.probe_id == 4
                && neighbor.subject == "Alloy B"
                && neighbor.object == "Al"
                && neighbor.confidence == Some(0.8)
        }));
        assert!(
            neighbors
                .iter()
                .all(|neighbor| neighbor.distance.abs() < 1e-6)
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM prov_assertion").await,
            assertions_before,
            "a geometric signal must never drop or rewrite an assertion"
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM emmo_entity").await,
            entities_before,
            "a geometric signal must never merge or rewrite an entity"
        );
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
        assert_eq!(
            store.entity_embedding_partitions("t1").await.unwrap(),
            [EmbeddingPartition {
                model: Some(prism_embed::EmbedBackend::id(&MockEmbed).into()),
                dimensions: 3,
                count: 4,
            }],
            "embed_and_store_entities must persist EmbedBackend::id()"
        );

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
        write_emmo_shaped(
            &store,
            &fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            &prov,
        )
        .await;

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
        write_emmo_shaped(
            &store,
            &fact("phase", "Ti-6Al-4V", "has_phase", "alpha"),
            &prov,
        )
        .await;
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
    /// `#[ignore]`d: needs the pinned ONNX snapshot to have been explicitly
    /// installed in `~/.prism/models/embed/`.
    /// Run with `cargo test -p prism-provenance -- --ignored`.
    /// Not compiled on Intel macOS, which has no ONNX Runtime build and so
    /// no `NativeOnnx` (see `prism_embed`).
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[tokio::test]
    #[ignore = "requires the explicitly installed pinned BGE snapshot"]
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

    // ─────────────────────────────────────────────────────────────────────
    // Exact source citations and assertion identity reads
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn source_citation_rejects_coordinates_hashes_and_locators_it_cannot_address() {
        const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        let citation = SourceCitation::new(
            3,
            4,
            "  exact supporting sentence  ",
            SHA,
            Some(r#"{"page":2,"section":"results"}"#.into()),
        )
        .unwrap();
        assert_eq!(citation.line_start(), 3);
        assert_eq!(citation.line_end(), 4);
        assert_eq!(
            citation.evidence_span(),
            "  exact supporting sentence  ",
            "validation must not normalize the exact witness"
        );
        assert_eq!(citation.source_revision_id(), SHA);
        assert_eq!(
            citation.locator_json(),
            Some(r#"{"page":2,"section":"results"}"#)
        );

        assert!(SourceCitation::new(0, 1, "span", SHA, None).is_err());
        assert!(SourceCitation::new(2, 1, "span", SHA, None).is_err());
        assert!(SourceCitation::new(1, 1, "  ", SHA, None).is_err());
        assert!(SourceCitation::new(1, 1, "span", "short", None).is_err());
        assert!(
            SourceCitation::new(
                1,
                1,
                "span",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                None,
            )
            .is_err(),
            "the source revision contract is lowercase hexadecimal SHA-256"
        );
        assert!(
            SourceCitation::new(1, 1, "span", SHA, Some("not-json".into())).is_err(),
            "locator_json must be queryable JSON rather than mislabeled text"
        );
    }

    /// Citation belongs to a SOURCE CONTRIBUTION, not the aggregate
    /// assertion. Two papers supporting one conditioned fact therefore keep
    /// two distinct witnesses and two source-specific check results. A repeat
    /// read of paper A cannot silently replace A's first stored witness.
    #[tokio::test]
    async fn distinct_sources_keep_distinct_citations_and_duplicates_do_not_overwrite() {
        const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const SHA_REPLACEMENT: &str =
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let first_fact = verified_fact("CitedAlloy", Some(VerificationStatus::Grounded), None);
        let second_fact = verified_fact(
            "CitedAlloy",
            Some(VerificationStatus::SubjectNotVerbatim),
            Some("paper B uses a shorter subject name"),
        );
        let citation_a = SourceCitation::new(
            7,
            7,
            "CitedAlloy has an UTS of 1140 MPa.",
            SHA_A,
            Some(r#"{"page":1}"#.into()),
        )
        .unwrap();
        let citation_b = SourceCitation::new(
            42,
            43,
            "The reported value was 1140 MPa under the stated conditions.",
            SHA_B,
            None,
        )
        .unwrap();

        store
            .write_fact_with_classification_and_citation(
                &first_fact,
                &prov_from("doc:paper-a", "act-citation-a"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_a,
            )
            .await
            .unwrap();
        store
            .write_fact_with_classification_and_citation(
                &second_fact,
                &prov_from("doc:paper-b", "act-citation-b"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_b,
            )
            .await
            .unwrap();

        // Same origin source, later run, different proposed witness. This is
        // not a third observation and must not rewrite the first witness.
        let replacement = SourceCitation::new(
            99,
            100,
            "a later, incompatible proposed witness",
            SHA_REPLACEMENT,
            Some(r#"{"page":99}"#.into()),
        )
        .unwrap();
        store
            .write_fact_with_classification_and_citation(
                &first_fact,
                &prov_from("doc:paper-a", "act-citation-a-repeat"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &replacement,
            )
            .await
            .unwrap();

        let id = conditioned_assertion_id(
            "t1",
            "CitedAlloy",
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &[],
        )
        .unwrap();
        let evidence = store.assertion_evidence_by_id(&id).await.unwrap();
        assert_eq!(evidence.len(), 2, "one row per source, not per read");
        let paper_a = evidence
            .iter()
            .find(|item| item.source_entity_id == "doc:paper-a")
            .unwrap();
        assert_eq!(paper_a.source_revision_id.as_deref(), Some(SHA_A));
        assert_eq!(paper_a.line_start, Some(7));
        assert_eq!(paper_a.line_end, Some(7));
        assert_eq!(
            paper_a.evidence_span.as_deref(),
            Some("CitedAlloy has an UTS of 1140 MPa.")
        );
        assert_eq!(paper_a.locator_json.as_deref(), Some(r#"{"page":1}"#));
        assert_eq!(
            paper_a.verification_status,
            Some(VerificationStatus::Grounded)
        );

        let paper_b = evidence
            .iter()
            .find(|item| item.source_entity_id == "doc:paper-b")
            .unwrap();
        assert_eq!(paper_b.source_revision_id.as_deref(), Some(SHA_B));
        assert_eq!(paper_b.line_start, Some(42));
        assert_eq!(paper_b.line_end, Some(43));
        assert_eq!(
            paper_b.verification_status,
            Some(VerificationStatus::SubjectNotVerbatim)
        );
        assert_eq!(
            paper_b.verification_reason.as_deref(),
            Some("paper B uses a shorter subject name")
        );
    }

    /// The old write method now delegates with NO citation. Missing citation
    /// fields are returned as explicit `None`, preserving the meaning of old
    /// rows. If that same source is later re-opened exactly, the duplicate
    /// write fills only those previously missing cells without corroborating.
    #[tokio::test]
    async fn uncited_evidence_is_explicit_none_and_a_reread_fills_missing_citation() {
        const SHA: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let fact = verified_fact("LegacyCitationAlloy", None, None);
        store
            .write_fact_with_classification(
                &fact,
                &prov_from("doc:legacy-paper", "act-legacy"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
            )
            .await
            .unwrap();
        let id = conditioned_assertion_id(
            "t1",
            "LegacyCitationAlloy",
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &[],
        )
        .unwrap();
        let before = store.assertion_evidence_by_id(&id).await.unwrap();
        assert_eq!(before.len(), 1);
        let before = &before[0];
        assert_eq!(before.source_revision_id, None);
        assert_eq!(before.evidence_span, None);
        assert_eq!(before.line_start, None);
        assert_eq!(before.line_end, None);
        assert_eq!(before.locator_json, None);
        assert_eq!(before.verification_status, None);
        assert_eq!(before.verification_reason, None);

        let citation = SourceCitation::new(12, 12, "1140 MPa", SHA, None).unwrap();
        store
            .write_fact_with_classification_and_citation(
                &fact,
                &prov_from("doc:legacy-paper", "act-reread"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation,
            )
            .await
            .unwrap();
        let after = store.assertion_evidence_by_id(&id).await.unwrap();
        assert_eq!(after.len(), 1, "a re-read is not corroboration");
        assert_eq!(after[0].source_revision_id.as_deref(), Some(SHA));
        assert_eq!(after[0].evidence_span.as_deref(), Some("1140 MPa"));
        assert_eq!(after[0].line_start, Some(12));
        assert_eq!(after[0].line_end, Some(12));
    }

    /// A legacy contribution can be upgraded when a later re-read supplies
    /// its first complete citation. The evidence row must then point at the
    /// exact reopenable snapshot and at the activity/agent that created that
    /// witness, while retaining one source contribution under the stable
    /// original-source key.
    #[tokio::test]
    async fn a_complete_citation_atomically_upgrades_a_legacy_source_locator() {
        const SHA: &str = "abababababababababababababababababababababababababababababababab";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let subject = "LegacyLocatorAlloy";
        let fact = verified_fact(subject, None, None);
        let original_source = "/papers/legacy-locator.pdf";
        store
            .write_fact_with_classification(
                &fact,
                &prov_from(original_source, "act-before-reread"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
            )
            .await
            .unwrap();

        let mut reread_prov = prov_from(
            "/snapshots/abababababababababababababababababababababababababababababababab.txt",
            "act-exact-reread",
        );
        reread_prov.origin_source_id = Some(original_source.into());
        reread_prov.agent_id = "citation-rereader".into();
        let grounded = verified_fact(subject, Some(VerificationStatus::Grounded), None);
        let citation = SourceCitation::new(
            17,
            18,
            "The exact source witness spans two lines.",
            SHA,
            Some(r#"{"source_kind":"text_snapshot"}"#.into()),
        )
        .unwrap();
        store
            .write_fact_with_classification_and_citation(
                &grounded,
                &reread_prov,
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation,
            )
            .await
            .unwrap();

        let id = conditioned_assertion_id(
            "t1",
            subject,
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &[],
        )
        .unwrap();
        let evidence = store.assertion_evidence_by_id(&id).await.unwrap();
        assert_eq!(evidence.len(), 1, "a re-read is not corroboration");
        let contribution = &evidence[0];
        assert_eq!(contribution.source_key, "file:/papers/legacy-locator.pdf");
        assert_eq!(contribution.source_entity_id, reread_prov.source_entity_id);
        assert_eq!(contribution.activity_id, "act-exact-reread");
        assert_eq!(contribution.agent_id, "citation-rereader");
        assert_eq!(contribution.source_revision_id.as_deref(), Some(SHA));
        assert_eq!(
            contribution.evidence_span.as_deref(),
            Some(citation.evidence_span())
        );
        assert_eq!(contribution.line_start, Some(17));
        assert_eq!(contribution.line_end, Some(18));
        assert_eq!(
            contribution.locator_json,
            citation.locator_json().map(str::to_string)
        );
        assert_eq!(
            contribution.verification_status,
            Some(VerificationStatus::Grounded)
        );
        assert_eq!(
            store
                .assertion_by_id(&id)
                .await
                .unwrap()
                .unwrap()
                .verification_status,
            Some(VerificationStatus::Grounded),
            "the eligible source-specific upgrade must also update the aggregate"
        );
    }

    /// Citation coordinates are one indivisible witness. A partially
    /// populated legacy/corrupt row cannot borrow only its missing endpoint
    /// from a later, different citation, because that would manufacture a
    /// span that neither extraction actually observed.
    #[tokio::test]
    async fn a_partial_citation_is_never_hybridized_with_a_later_citation() {
        const SHA_A: &str = "acacacacacacacacacacacacacacacacacacacacacacacacacacacacacacacac";
        const SHA_B: &str = "bdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbdbd";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let subject = "PartialCitationAlloy";
        let weak = verified_fact(
            subject,
            Some(VerificationStatus::ValueNotInSource),
            Some("the first witness was weak"),
        );
        let first_prov = prov_from("/snapshots/partial-a.txt", "act-partial-a");
        let citation_a =
            SourceCitation::new(4, 5, "first witness", SHA_A, Some(r#"{"page":1}"#.into()))
                .unwrap();
        store
            .write_fact_with_classification_and_citation(
                &weak,
                &first_prov,
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_a,
            )
            .await
            .unwrap();
        store
            .conn
            .execute("UPDATE prov_assertion_evidence SET line_end = NULL", ())
            .await
            .unwrap();

        let grounded = verified_fact(subject, Some(VerificationStatus::Grounded), None);
        let citation_b = SourceCitation::new(
            40,
            41,
            "different later witness",
            SHA_B,
            Some(r#"{"page":9}"#.into()),
        )
        .unwrap();
        let mut later_prov = prov_from("/snapshots/partial-b.txt", "act-partial-b");
        later_prov.origin_source_id = Some("/snapshots/partial-a.txt".into());
        later_prov.agent_id = "later-agent".into();
        store
            .write_fact_with_classification_and_citation(
                &grounded,
                &later_prov,
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_b,
            )
            .await
            .unwrap();

        let id = conditioned_assertion_id(
            "t1",
            subject,
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &[],
        )
        .unwrap();
        let evidence = store.assertion_evidence_by_id(&id).await.unwrap();
        assert_eq!(evidence.len(), 1);
        let contribution = &evidence[0];
        assert_eq!(contribution.source_entity_id, first_prov.source_entity_id);
        assert_eq!(contribution.activity_id, "act-partial-a");
        assert_eq!(contribution.agent_id, first_prov.agent_id);
        assert_eq!(contribution.source_revision_id.as_deref(), Some(SHA_A));
        assert_eq!(contribution.evidence_span.as_deref(), Some("first witness"));
        assert_eq!(contribution.line_start, Some(4));
        assert_eq!(
            contribution.line_end, None,
            "the missing cell stays missing"
        );
        assert_eq!(contribution.locator_json.as_deref(), Some(r#"{"page":1}"#));
        assert_eq!(
            contribution.verification_status,
            Some(VerificationStatus::ValueNotInSource),
            "a different citation cannot improve this evidence row"
        );
    }

    /// A stronger conclusion from the same source key is not evidence about
    /// the stored witness when its revision/span/coordinates differ. Neither
    /// the per-source row nor the parent assertion may be upgraded by that
    /// mismatched re-read.
    #[tokio::test]
    async fn a_mismatched_later_citation_cannot_upgrade_verification() {
        const SHA_A: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        const SHA_B: &str = "dededededededededededededededededededededededededededededededede";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let subject = "MismatchedCitationAlloy";
        let source = "/snapshots/mismatch.txt";
        let weak = verified_fact(
            subject,
            Some(VerificationStatus::ValueNotInSource),
            Some("the stored witness does not ground the value"),
        );
        let citation_a = SourceCitation::new(8, 8, "stored witness", SHA_A, None).unwrap();
        store
            .write_fact_with_classification_and_citation(
                &weak,
                &prov_from(source, "act-mismatch-a"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_a,
            )
            .await
            .unwrap();

        let grounded = verified_fact(subject, Some(VerificationStatus::Grounded), None);
        let citation_b = SourceCitation::new(88, 88, "different witness", SHA_B, None).unwrap();
        store
            .write_fact_with_classification_and_citation(
                &grounded,
                &prov_from(source, "act-mismatch-b"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_b,
            )
            .await
            .unwrap();

        let id = conditioned_assertion_id(
            "t1",
            subject,
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &[],
        )
        .unwrap();
        let contribution = store.assertion_evidence_by_id(&id).await.unwrap().remove(0);
        assert_eq!(contribution.source_revision_id.as_deref(), Some(SHA_A));
        assert_eq!(
            contribution.evidence_span.as_deref(),
            Some("stored witness")
        );
        assert_eq!(contribution.line_start, Some(8));
        assert_eq!(contribution.line_end, Some(8));
        assert_eq!(
            contribution.verification_status,
            Some(VerificationStatus::ValueNotInSource)
        );
        assert_eq!(
            contribution.verification_reason.as_deref(),
            Some("the stored witness does not ground the value")
        );
        let assertion = store.assertion_by_id(&id).await.unwrap().unwrap();
        assert_eq!(
            assertion.verification_status,
            Some(VerificationStatus::ValueNotInSource)
        );
        assert_eq!(
            assertion.verification_reason.as_deref(),
            Some("the stored witness does not ground the value")
        );

        // Control: the stronger result is allowed when it is explicitly tied
        // to the exact witness already stored.
        store
            .write_fact_with_classification_and_citation(
                &grounded,
                &prov_from(source, "act-mismatch-exact"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation_a,
            )
            .await
            .unwrap();
        let contribution = store.assertion_evidence_by_id(&id).await.unwrap().remove(0);
        assert_eq!(
            contribution.verification_status,
            Some(VerificationStatus::Grounded)
        );
        assert_eq!(contribution.verification_reason, None);
        let assertion = store.assertion_by_id(&id).await.unwrap().unwrap();
        assert_eq!(
            assertion.verification_status,
            Some(VerificationStatus::Grounded)
        );
        assert_eq!(assertion.verification_reason, None);
    }

    /// A database that already has the pre-citation evidence table receives
    /// nullable columns in place. Its existing contribution survives and
    /// reports no witness or source-specific verification rather than an
    /// invented default.
    #[tokio::test]
    async fn pre_citation_evidence_schema_migrates_existing_rows_to_explicit_none() {
        let db = TempDb::new();
        {
            let database = turso::Builder::new_local(db.path.to_str().unwrap())
                .build()
                .await
                .unwrap();
            let conn = database.connect().unwrap();
            conn.execute(
                r#"CREATE TABLE prov_assertion_evidence (
                    assertion_id TEXT NOT NULL,
                    source_key TEXT NOT NULL,
                    source_entity_id TEXT NOT NULL,
                    source_revision_id TEXT,
                    activity_id TEXT NOT NULL,
                    agent_id TEXT NOT NULL,
                    confidence REAL NOT NULL,
                    evidence_class TEXT NOT NULL,
                    confidence_kind TEXT NOT NULL DEFAULT 'source',
                    legacy_corroborations INTEGER,
                    PRIMARY KEY (assertion_id, source_key)
                )"#,
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO prov_assertion_evidence \
                 (assertion_id, source_key, source_entity_id, source_revision_id, \
                  activity_id, agent_id, confidence, evidence_class, confidence_kind) \
                 VALUES ('legacy-assertion', 'document:legacy', 'doc:legacy', NULL, \
                         'act-legacy', 'agent-legacy', 0.4, 'research', 'source')",
                (),
            )
            .await
            .unwrap();
            // This fixture is already on the evidence-table generation; only
            // the new nullable columns should be added on open.
            conn.execute("PRAGMA user_version = 5", ()).await.unwrap();
        }

        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let evidence = store
            .assertion_evidence_by_id("legacy-assertion")
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        let contribution = &evidence[0];
        assert_eq!(contribution.source_entity_id, "doc:legacy");
        assert_eq!(contribution.source_revision_id, None);
        assert_eq!(contribution.evidence_span, None);
        assert_eq!(contribution.line_start, None);
        assert_eq!(contribution.line_end, None);
        assert_eq!(contribution.locator_json, None);
        assert_eq!(contribution.verification_status, None);
        assert_eq!(contribution.verification_reason, None);
    }

    /// Generation 7 moves the PRISM-minted MatKG namespace to `mirdyne.com`.
    ///
    /// The keyed columns are the ones that can fail quietly. `item_id` embeds
    /// the parent IRI and is the proposal queue's PRIMARY KEY as well as half
    /// of the sighting table's composite key, so a rewrite that moves one and
    /// not the other detaches a queued proposal from its citations while
    /// leaving both rows individually well-formed. The join is asserted, not
    /// just the two counts.
    ///
    /// The captured stdout in `provenance_records` is asserted UNCHANGED. That
    /// row records a command that really did run under the old namespace; a
    /// provenance store that edits its own history is worse than one that
    /// stores none. `ontology_term_binding` is created here WITHOUT the later
    /// `nearest_class_iri` column, so the missing-column skip is exercised too.
    #[tokio::test]
    async fn generation_seven_moves_the_matkg_namespace_and_leaves_history_alone() {
        const OLD: &str = "https://marc27.com/ontology/matkg#Property";
        const NEW: &str = "https://mirdyne.com/ontology/matkg#Property";
        let db = TempDb::new();

        // Let production build its own schema first, then plant legacy rows
        // into it and wind the stamp back so the generation has work to do.
        ProvenanceStore::open(&db.path).await.unwrap();

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();

        for ddl in [
            "CREATE TABLE IF NOT EXISTS ontology_proposal_queue (item_id TEXT PRIMARY KEY, \
             kind TEXT NOT NULL CHECK (kind IN ('class', 'relation')), label TEXT NOT NULL, \
             document TEXT NOT NULL, tenant TEXT NOT NULL, proposal_json TEXT NOT NULL, \
             enqueued_at REAL NOT NULL)",
            "CREATE TABLE IF NOT EXISTS ontology_proposal_sighting (item_id TEXT NOT NULL, \
             document TEXT NOT NULL, citation_json TEXT NOT NULL, sighted_at REAL NOT NULL, \
             PRIMARY KEY (item_id, document, citation_json))",
            "CREATE TABLE IF NOT EXISTS ontology_class_embedding (ontology_id TEXT NOT NULL, \
             class_iri TEXT NOT NULL, label TEXT NOT NULL, model TEXT NOT NULL, \
             dim INTEGER NOT NULL, vector BLOB NOT NULL, \
             PRIMARY KEY (ontology_id, class_iri, label, model))",
            "CREATE TABLE IF NOT EXISTS ontology_term_binding (tenant TEXT NOT NULL, \
             term TEXT NOT NULL, verbatim TEXT NOT NULL, class_iri TEXT, ontology_id TEXT, \
             rung INTEGER NOT NULL, score REAL, threshold REAL, model TEXT, \
             proposal_item_id TEXT, resolved_at TEXT NOT NULL, PRIMARY KEY (tenant, term))",
        ] {
            conn.execute(ddl, ()).await.unwrap();
        }

        // Real queue keys embed a quoted label, so the fixture carries one and
        // the literal doubles it rather than dodging the shape under test.
        let item_id = format!("class|'ThermalExpansion'|parents=[{OLD}]");
        let item_id = item_id.replace('\'', "''");
        conn.execute(
            format!(
                "INSERT INTO ontology_proposal_queue VALUES \
                 ('{item_id}', 'class', 'ThermalExpansion', 'doc:1', 'local', \
                  '{{\"parents\":[\"{OLD}\"]}}', 1.0)"
            ),
            (),
        )
        .await
        .unwrap();
        conn.execute(
            format!(
                "INSERT INTO ontology_proposal_sighting VALUES ('{item_id}', 'doc:1', '{{}}', 1.0)"
            ),
            (),
        )
        .await
        .unwrap();
        conn.execute(
            format!(
                "INSERT INTO emmo_entity (\"key\", name, entity_type, tenant, class_iri) \
                 VALUES ('local:thermalexpansion', 'ThermalExpansion', 'Property', 'local', '{OLD}')"
            ),
            (),
        )
        .await
        .unwrap();
        conn.execute(
            format!(
                "INSERT INTO ontology_class_embedding VALUES \
                 ('matkg', '{OLD}', 'Property', 'bge-small', 2, X'0000')"
            ),
            (),
        )
        .await
        .unwrap();
        conn.execute(
            format!(
                "INSERT INTO ontology_term_binding \
                 (tenant, term, verbatim, class_iri, ontology_id, rung, proposal_item_id, resolved_at) \
                 VALUES ('local', 'thermal expansion', 'thermal expansion', '{OLD}', 'matkg', 1, \
                         '{item_id}', '2026-01-01')"
            ),
            (),
        )
        .await
        .unwrap();
        // Audit history: captured stdout of a run that really used the old host.
        conn.execute(
            format!(
                "INSERT INTO provenance_records \
                 (id, timestamp, session_id, action_type, actor, input_json, output_json) \
                 VALUES ('rec-1', '2026-01-01', 's1', 'tool_call', 'agent', '{{}}', \
                         '{{\"iri\":\"{OLD}\"}}')"
            ),
            (),
        )
        .await
        .unwrap();
        conn.execute(
            &format!("PRAGMA user_version = {SAMPLE_DISAGREEMENT_RETIRED_VERSION}"),
            (),
        )
        .await
        .unwrap();
        drop(conn);

        ProvenanceStore::open(&db.path).await.unwrap();

        let database = turso::Builder::new_local(db.path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = database.connect().unwrap();
        let count = |sql: String| {
            let conn = conn.clone();
            async move {
                let mut rows = conn.query(sql, ()).await.unwrap();
                rows.next()
                    .await
                    .unwrap()
                    .and_then(|r| r.get_value(0).ok().and_then(|v| v.as_integer().copied()))
                    .unwrap_or(-1)
            }
        };

        for (table, column) in [
            ("ontology_proposal_queue", "item_id"),
            ("ontology_proposal_queue", "proposal_json"),
            ("ontology_proposal_sighting", "item_id"),
            ("emmo_entity", "class_iri"),
            ("ontology_class_embedding", "class_iri"),
            ("ontology_term_binding", "class_iri"),
            ("ontology_term_binding", "proposal_item_id"),
        ] {
            assert_eq!(
                count(format!(
                    "SELECT COUNT(*) FROM {table} WHERE {column} LIKE '%{NEW}%'"
                ))
                .await,
                1,
                "{table}.{column} was not moved to the new namespace",
            );
            assert_eq!(
                count(format!(
                    "SELECT COUNT(*) FROM {table} WHERE {column} LIKE '%marc27.com/ontology%'"
                ))
                .await,
                0,
                "{table}.{column} still holds the old namespace",
            );
        }

        assert_eq!(
            count(
                "SELECT COUNT(*) FROM ontology_proposal_queue q \
                 JOIN ontology_proposal_sighting s ON s.item_id = q.item_id"
                    .to_string()
            )
            .await,
            1,
            "the proposal lost its citations — the two halves of item_id moved apart",
        );

        assert_eq!(
            count(
                "SELECT COUNT(*) FROM provenance_records \
                 WHERE output_json LIKE '%marc27.com/ontology%'"
                    .to_string()
            )
            .await,
            1,
            "the migration rewrote captured stdout — provenance must not edit its own history",
        );

        assert_eq!(
            count("PRAGMA user_version".to_string()).await,
            MATKG_NAMESPACE_VERSION,
            "the store was not stamped at the new generation",
        );
    }

    /// Identity lookup did not exist before retrieval re-verification: a
    /// conditioned assertion could be found only by fuzzy subject/object
    /// search. The id read now returns the exact value, unit, conditions,
    /// source ownership, and annotation even when that status is untrusted.
    #[tokio::test]
    async fn assertion_by_id_loads_the_complete_conditioned_fact() {
        const SHA: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let conditions = vec![
            MeasurementCondition {
                name: "temperature".into(),
                value: ConditionValue::Number(700.0),
                unit: Some(QudtUnit::new("QUDT:K").unwrap()),
            },
            MeasurementCondition {
                name: "atmosphere".into(),
                value: ConditionValue::Text("argon".into()),
                unit: None,
            },
        ];
        let mut fact = verified_fact(
            "ConditionedAlloy",
            Some(VerificationStatus::ValueNotInSource),
            Some("no single span carried every condition"),
        );
        fact.conditions = conditions.clone();
        let citation =
            SourceCitation::new(21, 23, "1140 MPa at 700 K in argon", SHA, None).unwrap();
        store
            .write_fact_with_classification_and_citation(
                &fact,
                &prov_from("file:/papers/conditioned.txt", "act-conditioned"),
                test_ontology_classification(),
                FactGraphShape::emmo("measurement"),
                &citation,
            )
            .await
            .unwrap();
        let id = conditioned_assertion_id(
            "t1",
            "ConditionedAlloy",
            "has_measurement",
            "UTS",
            Some(1140.0),
            Some("QUDT:MegaPA"),
            &conditions,
        )
        .unwrap();

        let stored = store
            .assertion_by_id(&id)
            .await
            .unwrap()
            .expect("conditioned assertion must be addressable by id");
        assert_eq!(stored.id, id);
        assert_eq!(stored.subject, "ConditionedAlloy");
        assert_eq!(stored.predicate, "has_measurement");
        assert_eq!(stored.object, "UTS");
        assert_eq!(stored.value, Some(1140.0));
        assert_eq!(stored.unit.as_deref(), Some("QUDT:MegaPA"));
        assert_eq!(stored.conditions, conditions);
        assert_eq!(stored.evidence_class, EvidenceClass::Research);
        assert_eq!(stored.corroborations, 1);
        assert_eq!(stored.activity_id, "act-conditioned");
        assert_eq!(stored.source, "file:/papers/conditioned.txt");
        assert_eq!(stored.tenant, "t1");
        assert_eq!(
            stored.verification_status,
            Some(VerificationStatus::ValueNotInSource),
            "identity lookup must not hide a weak stored annotation"
        );
        assert_eq!(
            stored.verification_reason.as_deref(),
            Some("no single span carried every condition")
        );
        assert!(store.assertion_by_id("missing-id").await.unwrap().is_none());
    }

    // ─────────────────────────────────────────────────────────────────────
    // Verification status (annotate-not-refuse)
    // ─────────────────────────────────────────────────────────────────────

    fn verified_fact(
        subject: &str,
        verification: Option<VerificationStatus>,
        reason: Option<&str>,
    ) -> MaterialFact {
        MaterialFact {
            subject: subject.into(),
            predicate: "has_measurement".into(),
            object: "UTS".into(),
            value: Some(1140.0),
            unit: Some(QudtUnit::new("QUDT:MegaPA").unwrap()),
            conditions: vec![],
            confidence: Some(0.9),
            kind: Some("measurement".into()),
            evidence_class: EvidenceClass::Research,
            verification,
            verification_reason: reason.map(str::to_string),
        }
    }

    /// The core read contract of annotate-not-refuse: a weak fact is
    /// STORED, excluded from the default (trusted) read, and findable by
    /// explicit filter — with the check's reason intact. Rows with no
    /// recorded status stay visible by default, or every fact written
    /// before this column existed would silently vanish.
    #[tokio::test]
    async fn weak_facts_are_stored_findable_and_excluded_from_the_default_read() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        store
            .write_fact(
                &verified_fact("GroundedAlloy", Some(VerificationStatus::Grounded), None),
                &prov,
            )
            .await
            .unwrap();
        store
            .write_fact(
                &verified_fact(
                    "PreciseAlloy",
                    Some(VerificationStatus::SubjectNotVerbatim),
                    Some("the document never names that subject"),
                ),
                &prov,
            )
            .await
            .unwrap();
        // No status at all — a legacy/tabular-shaped write.
        store
            .write_fact(&verified_fact("LegacyAlloy", None, None), &prov)
            .await
            .unwrap();

        let trusted = store.recall_with_context("Alloy", "t1", 10).await.unwrap();
        let names: Vec<&str> = trusted.iter().map(|f| f.subject.as_str()).collect();
        assert!(names.contains(&"GroundedAlloy"), "{names:?}");
        assert!(
            names.contains(&"LegacyAlloy"),
            "a row with no recorded status must stay visible: {names:?}"
        );
        assert!(
            !names.contains(&"PreciseAlloy"),
            "a weak-status fact must not be promoted into the default read: {names:?}"
        );

        let all = store
            .recall_with_context_filtered("Alloy", &["t1"], 10, VerificationFilter::Any)
            .await
            .unwrap();
        assert_eq!(all.len(), 3, "everything is present: {all:?}");
        let weak = store
            .recall_with_context_filtered(
                "Alloy",
                &["t1"],
                10,
                VerificationFilter::Status(VerificationStatus::SubjectNotVerbatim),
            )
            .await
            .unwrap();
        assert_eq!(weak.len(), 1, "{weak:?}");
        assert_eq!(weak[0].subject, "PreciseAlloy");
        assert_eq!(
            weak[0].verification_reason.as_deref(),
            Some("the document never names that subject"),
            "the refusal-era reason must survive as a queryable field"
        );
    }

    /// Status aggregation is BEST-wins across sightings (a grounding
    /// witness in any source is a real witness), while a later weaker
    /// sighting or a status-less write can never downgrade or clear it.
    /// This is the opposite direction from the evidence class, on purpose —
    /// the class ceiling is asserted untouched at the end.
    #[tokio::test]
    async fn verification_upgrades_on_a_grounding_witness_and_never_downgrades() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();

        store
            .write_fact(
                &verified_fact(
                    "Ti-6Al-4V",
                    Some(VerificationStatus::ValueNotInSource),
                    Some("no span carries the value"),
                ),
                &prov_from("doc:paper_a", "act_a"),
            )
            .await
            .unwrap();
        // A different source grounds the SAME assertion.
        store
            .write_fact(
                &verified_fact("Ti-6Al-4V", Some(VerificationStatus::Grounded), None),
                &prov_from("doc:paper_b", "act_b"),
            )
            .await
            .unwrap();
        // A third source's sloppy extraction must not un-ground it, and a
        // status-less write must not clear it.
        store
            .write_fact(
                &verified_fact(
                    "Ti-6Al-4V",
                    Some(VerificationStatus::SubjectNotVerbatim),
                    Some("later sloppy sighting"),
                ),
                &prov_from("doc:paper_c", "act_c"),
            )
            .await
            .unwrap();
        store
            .write_fact(
                &verified_fact("Ti-6Al-4V", None, None),
                &prov_from("doc:paper_d", "act_d"),
            )
            .await
            .unwrap();

        let facts = store
            .recall_with_context_filtered("Ti-6Al-4V", &["t1"], 10, VerificationFilter::Any)
            .await
            .unwrap();
        assert_eq!(facts.len(), 1, "one assertion, four sightings: {facts:?}");
        assert_eq!(
            facts[0].verification_status,
            Some(VerificationStatus::Grounded),
            "best sighting wins and sticks"
        );
        assert_eq!(
            facts[0].verification_reason, None,
            "the reason moves with the status it explains"
        );
        // The monotone evidence ceiling is a separate axis and still holds:
        // four literature sightings stay Research, however verified.
        assert_eq!(facts[0].evidence_class, EvidenceClass::Research);
    }

    #[tokio::test]
    async fn an_absent_unit_is_not_a_store_level_semantic_verdict() {
        // CONTRACT CHANGE: absence is representable independently of status.
        // The store preserves a caller's verification annotation when one is
        // present, but no longer invents or requires that semantic judgement.
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut silent = verified_fact("QuietAlloy", None, None);
        silent.unit = None;
        write_emmo_shaped(&store, &silent, &prov).await;
        let silent_facts = store
            .recall_with_context_filtered("QuietAlloy", &["t1"], 10, VerificationFilter::Any)
            .await
            .unwrap();
        assert_eq!(silent_facts.len(), 1);
        assert_eq!(silent_facts[0].unit, None);
        assert_eq!(silent_facts[0].verification_status, None);

        let mut annotated = verified_fact(
            "HonestAlloy",
            Some(VerificationStatus::UnitUnresolved),
            Some("unit \"MPa·strangeness\" resolved to nothing"),
        );
        annotated.unit = None;
        write_emmo_shaped(&store, &annotated, &prov).await;

        let facts = store
            .recall_with_context_filtered(
                "HonestAlloy",
                &["t1"],
                10,
                VerificationFilter::Status(VerificationStatus::UnitUnresolved),
            )
            .await
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].value, Some(1140.0), "the number is not lost");
        assert_eq!(facts[0].unit, None, "and it never wears a fake unit");
        // Excluded from the trusted default, of course.
        assert!(
            store
                .recall_with_context("HonestAlloy", "t1", 10)
                .await
                .unwrap()
                .is_empty()
        );
        // The Measurement node's props carry the status and the null unit.
        // The node name derives from canonical keys, so find the Measurement
        // neighbor instead of assuming the exact spelling.
        let traversal = store
            .get_neighbors("HonestAlloy", None, "t1", 10)
            .await
            .unwrap();
        let meas = traversal
            .nodes
            .iter()
            .find(|node| node.label == "Measurement")
            .expect("the measurement node exists");
        let props: serde_json::Value = serde_json::from_str(
            &store
                .entity_props_json(&meas.name, "t1")
                .await
                .unwrap()
                .expect("measurement props exist"),
        )
        .unwrap();
        assert_eq!(props["verification_status"], "unit_unresolved");
        assert!(props["unit"].is_null(), "honest null, never \"\": {props}");
    }

    /// The status lands on the graph EDGE, not only in a report: a weak
    /// phase fact's HAS_PHASE edge carries `verification_status` in its
    /// props, and a status-less write keeps its historical byte shape
    /// (props absent).
    #[tokio::test]
    async fn the_verification_status_rides_the_graph_edge_props() {
        let db = TempDb::new();
        let store = ProvenanceStore::open(&db.path).await.unwrap();
        let prov = test_prov();

        let mut weak_phase = verified_fact(
            "WeakAlloy",
            Some(VerificationStatus::ReviewDenied),
            Some("the source denies this phase"),
        );
        weak_phase.kind = Some("phase".into());
        weak_phase.value = None;
        weak_phase.unit = None;
        weak_phase.object = "omega".into();
        write_emmo_shaped(&store, &weak_phase, &prov).await;

        let traversal = store
            .get_neighbors("WeakAlloy", None, "t1", 10)
            .await
            .unwrap();
        let edge = traversal
            .edges
            .iter()
            .find(|edge| edge.rel_type == "HAS_PHASE")
            .expect("the phase edge exists — annotated, not refused");
        let props: serde_json::Value =
            serde_json::from_str(edge.props_json.as_deref().expect("edge carries props")).unwrap();
        assert_eq!(props["verification_status"], "review_denied");

        // Control: a status-less write of another fact leaves props absent.
        let mut plain = verified_fact("PlainAlloy", None, None);
        plain.kind = Some("phase".into());
        plain.value = None;
        plain.unit = None;
        plain.object = "alpha".into();
        write_emmo_shaped(&store, &plain, &prov).await;
        let traversal = store
            .get_neighbors("PlainAlloy", None, "t1", 10)
            .await
            .unwrap();
        let edge = traversal
            .edges
            .iter()
            .find(|edge| edge.rel_type == "HAS_PHASE")
            .unwrap();
        assert_eq!(edge.props_json, None, "no status, no invented props");
    }

    /// The anti-ratchet rule survives the vocabulary change: exactly the
    /// statuses that came from a RENDERED judgement refuse per-item model
    /// re-asking, and the trusted set is exactly the top of the rank order.
    #[test]
    fn verification_rank_trust_and_ratchet_are_consistent() {
        use VerificationStatus::*;
        // Ranks are a total order (used by SQL best-wins): all distinct.
        let mut ranks: Vec<i64> = VerificationStatus::ALL.iter().map(|s| s.rank()).collect();
        ranks.sort_unstable();
        ranks.dedup();
        assert_eq!(ranks.len(), VerificationStatus::ALL.len());
        // Trusted = the top of the order, nothing else.
        // CONTRACT CHANGE: `CitedByReader` joined the trusted set — the
        // fresh paper path stamps it for every cited proposal, and keeping
        // those facts out of default reads would re-install the measured
        // muzzle (~44% of quarantines came from checks that could not
        // pass). It ranks BELOW `UnitFromPage`/`Grounded` (no deterministic
        // check ran), so the trusted set is still exactly the top of the
        // rank order — now three statuses, not two.
        for status in VerificationStatus::ALL {
            assert_eq!(
                status.is_trusted(),
                status.rank() >= CitedByReader.rank(),
                "{status:?}"
            );
        }
        // Rendered judgements (deterministic doc checks and review verdicts)
        // may not be re-rolled; lookup failures, sampling noise, and
        // never-reviewed assertions may.
        for status in [
            Grounded,
            UnitFromPage,
            SubjectNotVerbatim,
            ValueNotInSource,
            ReviewUncertain,
            ReviewDenied,
        ] {
            assert!(status.judgement_was_rendered(), "{status:?}");
        }
        // CONTRACT CHANGE: `CitedByReader` joins the re-askable set — no
        // deterministic check or review judged span-support for it, so an
        // affirmation pass over its exact citation is a first ask.
        for status in [
            CitedByReader,
            UnitUnresolved,
            SampleDisagreement,
            ModelAsserted,
        ] {
            assert!(!status.judgement_was_rendered(), "{status:?}");
        }
        // The stored spelling round-trips.
        for status in VerificationStatus::ALL {
            assert_eq!(VerificationStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(VerificationStatus::parse("anything_else"), None);
    }
}
