//! Extracted claims typed for ontology-bound ingestion.
//!
//! This engine never invents claims: a claim exists only because an
//! extractor produced it from a located block of a real document, and every
//! claim must be verifiable against that exact block: a claim carries a
//! verbatim `quote`, and a claim whose quote does not occur in the block it
//! cites is dropped — a claim citing a document that does not support it is
//! a false provenance record, which is worse than no claim.
//!
//! The engine's job here is the contract and the stamping:
//!
//! * subject / predicate / object with value + unit term
//! * measurement conditions (a number without conditions is not a property)
//! * provenance back to the exact document and locator
//! * an evidence class capped at `research` — literature extraction can
//!   never be `reference_validated` (mirrors `app/tools/evidence.py` and
//!   `prism_provenance::evidence_for_result(LiteratureExtraction)`)
//!
//! The schema is field-compatible with `prism_provenance::MaterialFact` so a
//! serialized claim deserializes into it at the ingest boundary. The
//! vocabulary strings are the stable machine contract ("indeterminate",
//! "research", "screening", "reference_validated").
//!
//! CONTRACT CHANGE (de-hardcoding) — NO DOMAIN VOCABULARY IN THIS FILE.
//! PRISM is a HARNESS with PLUGGABLE ontologies: a customer brings a domain
//! (pharma, semiconductors, biology — in any language), the ontology is
//! authored for it, and this Rust never changes. Earlier revisions of this
//! module carried English/materials vocabularies that decided what a fact
//! means — a sign table (`NONNEGATIVE_QUANTITIES` + differential markers +
//! strength homographs), a unit lexicon (`UNIT_TOKENS` and its derived
//! initials), and label vocabularies (`LABEL_WORDS`, `ABBREV_LABEL_WORDS`,
//! `LIST_CONTINUATIONS`). All of it is deleted. Every guard left is either
//! domain-independent structure (token boundaries, bracketed citation
//! markers, digit/dash/digit ranges, containment inside the claim's own
//! name) or knowledge the CALLER supplies per claim through [`GuardPolicy`],
//! which the ingest side reads from the active ontology and the fact itself
//! at grounding time. Where the ontology is silent, the check does not
//! apply — it is never replaced by a guess. The guards are notes for a
//! later re-checking model, not verdicts: a fact that fails one is stored
//! carrying a `VerificationStatus`, not dropped. A guard that could not be
//! made domain-independent (the label vocabulary) was deleted rather than
//! kept wrong: a wrong note is worse than no note.
//!
//! RECORDED, NOT FIXED (round 12) — NUMERIC DECORATIONS ARE INVISIBLE
//! (PARTIALLY CLOSED, bugfix pass B8): "950 ± 30 MPa" used to stamp the
//! TOLERANCE as the value. The PLUS-MINUS half is now FIXED — the
//! `Uncertainty` guard refuses a number whose left neighbour is the
//! plus-minus notation (U+00B1 or ASCII `+/-`), mathematical notation,
//! not vocabulary; the value it decorates still stamps. Pinned as KNOWN corpus rows in tests/claim_corpus.rs (the
//! numeric-decorations family); the same family carries the
//! digit-dash-LETTER locants ("3-point", "2-step", "2-propanol",
//! "N-methyl-2-pyrrolidone"), which are STILL unguarded (refusing
//! digit-dash-letter outright would also refuse hyphenated unit
//! spellings such as "2-mm", and telling them apart needs a unit
//! lexicon only the ontology can supply — exactly the hardcoding this
//! contract deleted).
//!
//! BUGFIX-PASS NOTES (B9–B12), for the ledger:
//! * B9: `SupportRefusal::ValueNotRendered` now splits "the value never
//!   rendered anywhere" from `NoSpan` — over-refusals are no longer filed
//!   as the model's hallucinations by default.
//! * B10: `validate_and_stamp` runs the quote-containment checks BEFORE
//!   the unit refusal, so a unitless numeric claim with a false citation
//!   is recorded as the false citation it is.
//! * B11: `scan_number_evidence` reports the first refusal in POSITION
//!   order (was: needle-form order).
//! * B12 (spaced-dash half): `dash_range_endpoint` tolerates whitespace
//!   around the dash glyph on both sides, so "950 \u{2013} 1100" and its
//!   ASCII twin drop both endpoints. The WORD-form range ("950 to 1100")
//!   and the double-dash negative form stay open and recorded: a range
//!   connector word is language vocabulary, and "950 bis 1100" in a
//!   German paper would sail past an English word guard — that input
//!   belongs to the ontology, not to Rust.
//!
//! Known provenance caveat: claims can differ by fetch route.
//! (a) RANGES: JATS preserves U+2013, `pdf-extract` normalises ranges to
//! '-'. The range guard covers every glyph of the dash class; both routes
//! refuse both endpoints of a digit/dash/digit run.
//! (b) SIGNED VALUES at |value| >= 1000: JATS typesets the minus as
//! U+2212, `pdf-extract` emits '-'. The sign attaches to both the plain
//! and the comma-grouped rendering; both routes stamp a genuine negative.
//! (c) SEPARATOR SHAPES — the dash between a label and its value
//! ("UTS –950 MPa") is locally indistinguishable from a minus. The
//! domain-independent defences left are: U+2013/U+2014 are NOT sign
//! glyphs (the shape drops as NoSpan on the JATS route), the
//! `SeparatorDash` guard's label-inline shape (the claim's own object
//! name abuts the dash — no vocabulary needed), and its bracketed shape
//! when the fact's own unit term follows the value. For a claim whose
//! ontology declares the quantity NON-NEGATIVE ([`GuardPolicy`]), the
//! `SignDomain` guard refuses every glyph. Where the ontology is SILENT
//! the ASCII/U+2212 separator fabrication on a signed quantity is OPEN
//! again, honestly: closing it with a quantity-name list is exactly the
//! hardcoding this contract deleted.
//!
//! RECORDED, NOT FIXED — the open set of non-negative quantities was
//! measured as UNBOUNDED (round 14 item 5): density families, magnitude
//! classes, symbol/synonym forms the extractor emits verbatim — no
//! enumeration in Rust closes it, and every entry would be one domain's
//! vocabulary. The closure lives where it always belonged: the ontology
//! annotates its quantity kinds, and [`GuardPolicy::quantity_sign`]
//! carries the answer at grounding time.
//!
//! PRODUCTION MITIGATION (round 14 item 6): a SignDomain over-refusal is
//! NOT invisible. cli/papers.rs pushes every rejected claim into the
//! output `rejected[]` JSON carrying the guard name, subject, object,
//! value and locator, so the drop is observable and attributable
//! downstream. On this branch refusal is annotation, not loss: the fact
//! is stored with its `VerificationStatus`.
//!
//! DASH-GLYPH HISTORY (still load-bearing for the surviving guards):
//! the dash class is every glyph the typeset world uses where a minus
//! can stand (ASCII hyphen, U+2010..U+2015, U+2212, U+FE63). Round 9
//! measured the sign flip stamping through each glyph and made the class
//! whole; round 11 reverted making U+2013/U+2014 sign glyphs because the
//! separator shape fabricated negatives — for SIGNED quantities the '-' /
//! U+2212 minus-vs-separator reading stays locally indistinguishable and
//! the engine keeps the MINUS reading (the glyph IS the minus sign, so a
//! negative that stamps is the defensible reading). Round 16 added
//! `SeparatorDash`: on a signed predicate it refuses (A) the label-inline
//! shape — the object's own name abuts the dash ("residual stress -950
//! MPa") — and (B) the bracketed shape — the fact's own unit term glued
//! right after the value, immediately followed by a dash that is not
//! followed by a digit. Both close without vocabulary: a true minus
//! follows a verb/preposition, never the object name, and never carries
//! a trailing parenthetical dash. The line-start shape ("-950 MPa was
//! recorded") carries neither signal and stays a KNOWN live fabrication
//! in the corpus.
//!
//! RECORDED, NOT FIXED (round 9) — the largest remaining structural
//! gap: THE VALUE IS NEVER TIED TO THE PREDICATE. Measured at HEAD,
//! both shapes pass every guard this branch built:
//!
//! * "The Ti-6Al-4V UTS was 950 MPa and the yield strength 880 MPa."
//!   claimed as yield_strength = 950 STAMPS: the span holds the
//!   subject, the object word and the number, and nothing asks which
//!   property the number belongs to.
//! * `validate_and_stamp` checks only that a unit EXISTS, never that
//!   it matches the prose: a 950 GPa claim against "950 MPa" text
//!   stamps with a verbatim quote.
//!
//! Right number, wrong property, wrong unit, perfect provenance.
//! Closing it needs predicate/value binding in the span scan and a
//! unit-match check in `validate_and_stamp`. Round 10 pinned both
//! shapes as KNOWN corpus rows — the predicate-binding case in the
//! main table, the unit-mismatch case in the validation table, which
//! is the only tuple with a unit field — so the record of the gap
//! lives on the scoreboard, where it cannot go stale, instead of here.

use prism_provenance::VerificationStatus;
use serde::{Deserialize, Serialize};

/// The ontology's declaration of a quantity's sign domain — re-exported so
/// matcher callers name one type, sourced in the vocabulary-neutral crate.
pub use prism_provenance::QuantitySignDomain;

use crate::fulltext::Locator;

pub const EVIDENCE_INDETERMINATE: &str = "indeterminate";
pub const EVIDENCE_RESEARCH: &str = "research";
pub const EVIDENCE_SCREENING: &str = "screening";
pub const EVIDENCE_REFERENCE_VALIDATED: &str = "reference_validated";

fn rank(class: &str) -> u8 {
    match class {
        EVIDENCE_INDETERMINATE => 0,
        EVIDENCE_RESEARCH => 1,
        EVIDENCE_SCREENING => 2,
        EVIDENCE_REFERENCE_VALIDATED => 3,
        _ => 0, // unknown values are indeterminate, never trusted
    }
}

/// Apply the literature-extraction ceiling: the result can never outrank
/// `research`, and it can never outrank its input. Unknown inputs collapse
/// to indeterminate.
///
/// B14: the old first branch (`rank(claimed) <= rank(RESEARCH) && claimed ==
/// RESEARCH`) was redundant — the second conjunct implies the first — and
/// read as if it did more than it did. The function is exactly "rank at
/// least research → research, else indeterminate", and now says so.
#[must_use]
pub fn cap_at_literature(claimed: &str) -> &'static str {
    if rank(claimed) >= rank(EVIDENCE_RESEARCH) {
        EVIDENCE_RESEARCH
    } else {
        EVIDENCE_INDETERMINATE
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionValue {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeasurementCondition {
    pub name: String,
    pub value: ConditionValue,
    /// QUDT unit identifier. REQUIRED for numeric conditions; a numeric
    /// condition without a unit is rejected, not defaulted.
    #[serde(default)]
    pub unit: Option<String>,
}

/// Where a claim came from — document plus locator, so a human can find the
/// sentence a number was read out of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimProvenance {
    /// DOI / arXiv id / PMCID / source id of the document.
    pub document_id: String,
    pub document_url: String,
    /// Retrieval source that served the document.
    pub source: String,
    pub locator: Locator,
    /// Verbatim span the claim was read from, when the extractor supplies it.
    #[serde(default)]
    pub quote: Option<String>,
    /// SHA-256 of the complete source text whose line coordinates were read.
    /// `None` is an explicit legacy/uncited claim.
    #[serde(default)]
    pub source_revision_id: Option<String>,
    /// One-based inclusive source line range for `quote`. Both fields stay
    /// optional so claims serialized before exact citations remain readable.
    #[serde(default)]
    pub line_start: Option<i64>,
    #[serde(default)]
    pub line_end: Option<i64>,
    /// Local UTF-8 source representation whose hash and line coordinates the
    /// citation addresses. Remote-paper ingestion may cache a block here so
    /// exact rereading does not depend on refetching or reparsing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_text_path: Option<String>,
}

/// One extracted material claim, EMMO-shaped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedClaim {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub value: Option<f64>,
    /// QUDT unit identifier for `value`.
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub conditions: Vec<MeasurementCondition>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// EMMO kind hint: measurement | phase | composition | processing | ...
    #[serde(default)]
    pub kind: Option<String>,
    /// Always stamped through `cap_at_literature`; never trusted from input.
    pub evidence_class: String,
    /// Deterministic population checks are annotations, never deletion gates.
    #[serde(default)]
    pub verification: Option<VerificationStatus>,
    #[serde(default)]
    pub verification_reason: Option<String>,
    /// Canonical endpoint/property identities selected from the active
    /// ontology during paper reading.
    #[serde(default)]
    pub ontology: ClaimOntologyBinding,
    pub provenance: ClaimProvenance,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimOntologyBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_class_iri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate_iri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_class_iri: Option<String>,
}

/// Errors a claim can carry instead of being silently accepted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClaimRejection {
    NumericConditionWithoutUnit {
        condition: String,
    },
    NumericValueWithoutUnit,
    EmptySubject,
    /// The claim carried no verbatim quote, so nothing ties it to the block
    /// it cites. Unverifiable claims are dropped, never stamped.
    MissingQuote,
    /// B9: the numeric value occurred in NO rendered form anywhere in the
    /// cited block — distinct from `MissingQuote` (nothing ties the claim
    /// to the block) and from `NoEvidentialOccurrence` (a guard examined
    /// and refused occurrences). The block may name the subject plainly
    /// while the number is missing in every spelling the engine knows, or
    /// the value is invented; the drop record now says which shape it was
    /// instead of filing all of them as "the model's fault".
    ValueNotRendered,
    /// The claim's quote does not occur in the block at its recorded
    /// locator. The citation is false; the claim is dropped, not downgraded.
    QuoteNotInCitedBlock,
    /// A span held the claim's subject or object and the value occurred in
    /// it, but a guard refused every occurrence: the matcher was too
    /// literal, the model did not hallucinate. Distinct from `MissingQuote`
    /// so over-refusal is measurable in the drop set. `guard` refused the
    /// FIRST candidate occurrence scanned; `span` is the verbatim span that
    /// held it.
    NoEvidentialOccurrence {
        guard: RefusalGuard,
        span: String,
    },
}

/// The guard that refused a candidate occurrence of the value. Named so a
/// drop can say WHY a supporting span yielded no evidence — the drop set is
/// the only observable signal of how this gate behaves. Every guard is
/// domain-independent: pure structure, or knowledge the caller supplied per
/// claim through [`GuardPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefusalGuard {
    /// A negative claim against a quantity the ACTIVE ONTOLOGY declares
    /// non-negative by definition ([`GuardPolicy::quantity_sign`]): nonsense
    /// under EVERY dash glyph, so it refuses what the minus-vs-separator
    /// ambiguity cannot separate. When the ontology is silent the check does
    /// not apply — this guard never infers a sign domain from the name.
    SignDomain,
    /// Endpoint of an en-dash digit range ("950\u{2013}1100"): the sentence
    /// asserts bounds, not a point value. B12 FIX: the digits no longer need
    /// to be GLUED to the dash — one space on either side ("950 \u{2013}
    /// 1100", journal typesetting) used to defeat the guard and stamp both
    /// endpoints as point values. The dash class is unchanged; only optional
    /// whitespace around it is tolerated, on both the forward (low
    /// endpoint) and backward (high endpoint) sides.
    Range,
    /// B8 FIX (the ± half): the occurrence is an UNCERTAINTY figure, not a
    /// value — the token immediately before it (skipping whitespace) is the
    /// plus-minus notation `\u{00b1}` or its ASCII spelling `+/-`, as in
    /// "950 ± 30 MPa". The VALUE such a tolerance decorates (950) still
    /// stamps — only the decoration figure (30) is refused. This is
    /// mathematical notation, not domain vocabulary: it carries no unit or
    /// quantity knowledge, exactly like the dash class. The digit-dash-letter
    /// locant family ("3-point", "2-propanol") is STILL unguarded — see the
    /// module ledger for why (it collides with hyphenated unit spellings
    /// without unit knowledge only the ontology can supply).
    Uncertainty,
    /// Token-boundary rule: continuation by digits/decimals/grouping, a
    /// leading minus that signs it, or a non-unit letter glued to it. A
    /// glued letter is redeemed only by the fact's OWN unit term
    /// ([`GuardPolicy::unit_term`]) — Rust holds no unit lexicon.
    Boundary,
    /// Inside a citation marker ("[1140]", "(1140)", "{1140}").
    Citation,
    /// Inside an occurrence of the subject's or object's own name
    /// (the "718" of "Inconel 718").
    InsideName,
    /// Round 16: a dash in SEPARATOR or PARENTHETICAL role on a SIGNED
    /// predicate, where `SignDomain` cannot help (residual stress is
    /// legitimately signed). Two locally-distinguishable shapes, both
    /// fabrications of the SIGN against a tensile source value:
    ///   (A) label-inline — the claimed object's own normalised name
    ///       abuts the dash ("residual stress -950 MPa"): the dash
    ///       separates the label from its value, so the source reads
    ///       +950 and a -950 claim fabricates the sign.
    ///   (B) bracketed — a dash GLUED to the fact's own unit term right
    ///       after the value ("result -950 MPa- matched"), the closing
    ///       parenthetical a minus never carries. Without a supplied unit
    ///       term this shape is inert.
    /// Fires only for value < 0 (a signed needle); for quantities the
    /// ontology declares non-negative `SignDomain` refuses first. The
    /// line-start shape ("-950 MPa was recorded") carries NEITHER signal
    /// and is deliberately NOT refused — it is locally indistinguishable
    /// from a genuine line-start minus ("-350 MPa was the surface
    /// stress") — and is carried as a KNOWN live fabrication in the
    /// corpus.
    SeparatorDash,
}

/// Domain knowledge the refusal guards need for ONE claim, supplied by the
/// caller — which reads it from the active ontology and the fact itself at
/// grounding time. PRISM is a harness with pluggable ontologies: none of
/// this may be compiled in, and a silent ontology leaves the corresponding
/// guard inert rather than guessed at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GuardPolicy {
    /// The sign domain the ontology declares for the claimed quantity kind.
    /// [`QuantitySignDomain::Unspecified`] (the default) means the ontology
    /// said nothing: the `SignDomain` guard does not apply.
    pub quantity_sign: QuantitySignDomain,
    /// The fact's exact unit term as chosen by the reader/ontology. The only
    /// token that can redeem a letter glued to the value, or mark a trailing
    /// dash as a closing parenthetical (`SeparatorDash` shape B). `None`
    /// means no unit knowledge: glued letters refuse, shape B is inert.
    /// Comparison is case-folded, exactly like the normalized text it is
    /// matched against.
    pub unit_term: Option<String>,
}

impl GuardPolicy {
    /// The policy for an ontology that declares nothing and a fact with no
    /// unit knowledge: every vocabulary-dependent guard stays inert. This is
    /// the honest default, never a guess in disguise.
    pub const SILENT: Self = Self {
        quantity_sign: QuantitySignDomain::Unspecified,
        unit_term: None,
    };
}

/// Why no supporting span was found. `NoSpan` reads as the model's fault
/// (the block does not mention the fact at all); `Guarded` is the matcher's
/// refusal and carries the guard that refused the first candidate occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupportRefusal {
    /// No span held the subject or the object (both, for a non-numeric
    /// fact): there was nothing to scan.
    NoSpan,
    /// B9 FIX (was RECORDED, NOT FIXED, round 7): for a numeric fact, the
    /// value occurred in NO rendered form ANYWHERE in the block — no
    /// needle form, no equal-valued lexeme. This is a DISTINCT fault from
    /// `NoSpan`: the block may name the subject plainly while the number is
    /// missing in every spelling the engine knows (a glyph it lacks, a
    /// unit conversion by the reader) — or the value is invented. The old
    /// single `NoSpan` filed all of these as "the block does not mention
    /// the fact at all" (`MissingQuote`), which the module defines as the
    /// MODEL's fault; measured round 7, 3 of 12 remaining over-refusals
    /// were misfiled as hallucinations this way. The split does not pick a
    /// culprit — it names the shape so a re-checker can.
    ValueNotRendered,
    /// A span held the subject or object and at least one occurrence of the
    /// value, but every occurrence was refused by a guard. Names the guard
    /// of the FIRST refused occurrence scanned (B11: first in POSITION); in
    /// a block where the value appears more than once (most real blocks)
    /// the first is the one a reader meets, and last-wins reporting was
    /// positional, not causal — it over-reported `Boundary` and
    /// under-reported `Label`.
    Guarded { guard: RefusalGuard, span: String },
}

impl From<SupportRefusal> for ClaimRejection {
    fn from(refusal: SupportRefusal) -> Self {
        match refusal {
            SupportRefusal::NoSpan => Self::MissingQuote,
            SupportRefusal::ValueNotRendered => Self::ValueNotRendered,
            SupportRefusal::Guarded { guard, span } => Self::NoEvidentialOccurrence { guard, span },
        }
    }
}

/// Validate and stamp one claim against `block_text`, the text of the block
/// the claim cites. Returns the stamped claim or the reason it was refused —
/// refused claims are reported, not dropped silently and not fixed by
/// guessing.
///
/// Containment is part of the contract: the claim must carry a verbatim
/// quote and that quote must occur in `block_text`. An extractor's output
/// (including prompt examples it parrots back) cannot survive as a claim
/// about a document that does not contain it.
pub fn validate_and_stamp(
    mut claim: ExtractedClaim,
    block_text: &str,
) -> Result<ExtractedClaim, ClaimRejection> {
    if claim.subject.trim().is_empty() {
        return Err(ClaimRejection::EmptySubject);
    }
    // B10 FIX: the QUOTE checks now run BEFORE the unit refusal. The old
    // order returned `NumericValueWithoutUnit` first, so a unitless numeric
    // claim with a FALSE citation was recorded as merely "unitless" — the
    // false-citation signal (the more severe fault: a claim citing a block
    // that does not contain it) was invisible in the drop record. The
    // containment contract gates everything else a claim says about its
    // block, so it gates the unit check too.
    let Some(quote) = claim.provenance.quote.as_deref() else {
        return Err(ClaimRejection::MissingQuote);
    };
    if !quote_in_block(quote, block_text) {
        return Err(ClaimRejection::QuoteNotInCitedBlock);
    }
    if claim.value.is_some() && claim.unit.is_none() {
        return Err(ClaimRejection::NumericValueWithoutUnit);
    }
    for condition in &claim.conditions {
        if matches!(condition.value, ConditionValue::Number(_)) && condition.unit.is_none() {
            return Err(ClaimRejection::NumericConditionWithoutUnit {
                condition: condition.name.clone(),
            });
        }
    }
    claim.evidence_class = cap_at_literature(&claim.evidence_class).to_string();
    Ok(claim)
}

/// Whitespace-collapsed lowercase form used for containment comparisons.
/// Underscores become spaces so EMMO-style identifiers (`thermal_conductivity`)
/// match their prose form.
fn normalize_for_containment(s: &str) -> String {
    s.replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Does `quote` occur (modulo whitespace and case) in the cited block?
fn quote_in_block(quote: &str, block_text: &str) -> bool {
    let needle = normalize_for_containment(quote);
    !needle.is_empty() && normalize_for_containment(block_text).contains(&needle)
}

/// Find a verbatim span of `block_text` that supports the fact described by
/// (`subject`, `object`, `value`), or say WHY no span does. The distinction
/// matters: `SupportRefusal::NoSpan` means the block does not mention the
/// fact at all (the drop is the model's), while `SupportRefusal::Guarded`
/// means a span held the fact but a guard refused every occurrence of the
/// value (the drop is the matcher's) — without it, over-refusal is
/// invisible in the output.
///
/// `policy` carries the domain knowledge the guards need — the ontology's
/// sign declaration for the quantity and the fact's own unit term. With
/// [`GuardPolicy::default`] the matcher knows neither and every
/// vocabulary-dependent guard stays inert: that is the honest state for an
/// ontology that declares nothing.
///
/// Support criteria (all case-insensitive, within one sentence/row span):
/// * numeric fact: the value's number appears together with the subject or
///   the object (a bare number could be a citation, so the number alone is
///   not enough). The number must occur as its own token — not as a
///   substring of a longer number and not a digit of the claim's own
///   designation, glued (Ti-6Al-4V) or spaced (the claim's own Inconel 718)
///   — and the occurrence must be evidential: a number inside a citation
///   marker `[...]` is a reference, not a measurement;
/// * non-numeric fact: both subject and object appear.
pub fn supporting_quote_or_refusal(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
    policy: &GuardPolicy,
) -> Result<String, SupportRefusal> {
    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);
    let guards = GuardInputs::from_policy(policy);
    // Keep the FIRST refusal, not the last: when the value occurs in
    // several spans, the last one refused is a position in the scan
    // order, not the cause of the drop — swapping two sentences in a
    // block changed the reported guard under last-wins.
    let mut first_refusal: Option<(RefusalGuard, String)> = None;
    for span in supporting_spans(block_text) {
        let hay = normalize_for_containment(span);
        match value {
            Some(v) => {
                // The subject-OR-object disjunction. ROUND 12 PRICE, MEASURED
                // (decision belongs to the branch owner; do not flip without
                // re-measuring): round 12 item 4(a) folded the dash class
                // into name matching, so the en-dash positive control in the
                // lib tests now rides the SUBJECT arm and the old 1-lib-assert
                // cost of require-subject is gone. Re-measured at round-12
                // HEAD:
                // * require-subject (subject must be present; the object arm
                //   alone insufficient): corpus 0 MUST_STAMP dropped, 0
                //   MUST_DROP stamped, 1 KNOWN FIXED — the subject-blind
                //   cross-subject row, i.e. the corpus's largest live
                //   fabrication channel closes. But 3 LIB asserts redden, all
                //   facts that ride ONLY the object arm: the CoCrFeNi
                //   supporting-sentence test (subject sits in the previous
                //   sentence), the JATS citation test's 'its UTS is 950 MPa'
                //   (same shape), and 'Figure 3 shows a UTS of 950 MPa.'
                //   (caption names the property, not the alloy). Honest
                //   price: 0 corpus rows, 3 lib asserts.
                // * require-both (this OR flipped to AND): corpus 18
                //   MUST_STAMP dropped, 19 KNOWN 'fixed' — among them the
                //   transposed-table row, whose marker would be stripped
                //   through the object arm while the column-binding gap stays
                //   wide open (the ca71cf65 defect, mirrored) — and 21 lib
                //   asserts redden. Not a candidate.
                let name_near = (!subject_n.is_empty() && find_name(&hay, &subject_n, 0).is_some())
                    || (!object_n.is_empty() && find_name(&hay, &object_n, 0).is_some());
                if name_near {
                    match scan_number_evidence(&hay, v, &subject_n, &object_n, &guards) {
                        NumberScan::Evidential => return Ok(span.trim().to_string()),
                        NumberScan::Refused(guard) => {
                            first_refusal.get_or_insert((guard, span.trim().to_string()));
                        }
                        NumberScan::Absent => {}
                    }
                }
            }
            None => {
                if !subject_n.is_empty()
                    && !object_n.is_empty()
                    && hay.contains(&subject_n)
                    && hay.contains(&object_n)
                {
                    return Ok(span.trim().to_string());
                }
            }
        }
    }
    match first_refusal {
        Some((guard, span)) => Err(SupportRefusal::Guarded { guard, span }),
        None => {
            // B9: split "the value never rendered anywhere" (matcher
            // over-refusal or invented value) from "no qualifying span"
            // (the block may render the value, just never beside the
            // subject/object). The needle forms are the same set the scan
            // used, applied to the whole block with no name proximity.
            if let Some(v) = value {
                let whole = normalize_for_containment(block_text);
                if !number_needles(v)
                    .iter()
                    .any(|needle| whole.contains(needle.as_str()))
                {
                    return Err(SupportRefusal::ValueNotRendered);
                }
            }
            Err(SupportRefusal::NoSpan)
        }
    }
}

/// `supporting_quote_or_refusal` for callers that only need the quote.
#[must_use]
pub fn supporting_quote(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
    policy: &GuardPolicy,
) -> Option<String> {
    supporting_quote_or_refusal(subject, object, value, block_text, policy).ok()
}

/// Find a supporting quote by comparing complete numeric lexemes rather than
/// formatted renderings of `value`.
///
/// `numeric_tolerance` is caller policy, not matcher policy. Candidates match
/// when they have the same sign and `abs(candidate - value) /
/// max(1, abs(candidate), abs(value)) <= numeric_tolerance`. A non-finite value
/// or tolerance, a negative tolerance, and ambiguous single-comma forms such as
/// `1,140` fail closed. The scanner recognizes decimal points, unambiguous
/// decimal-comma/grouped forms, ASCII/U+2212 signs, and exponents. It then sends
/// the exact lexeme and byte range through the same refusal guards as
/// [`supporting_quote`].
///
/// Unlike the legacy `Option` contract, the error retains the first named
/// refusal guard and the exact span it examined. Callers must decide how to
/// record that evidence rather than losing it while testing for presence.
///
/// `policy` is the per-claim knowledge the guards read — see
/// [`supporting_quote_or_refusal`].
pub fn supporting_quote_with_numeric_tolerance(
    subject: &str,
    object: &str,
    value: f64,
    block_text: &str,
    numeric_tolerance: f64,
    policy: &GuardPolicy,
) -> Result<String, SupportRefusal> {
    if !value.is_finite() || !numeric_tolerance.is_finite() || numeric_tolerance < 0.0 {
        return Err(SupportRefusal::NoSpan);
    }

    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);
    let guards = GuardInputs::from_policy(policy);
    let mut first_refusal: Option<(RefusalGuard, String)> = None;
    for span in supporting_spans(block_text) {
        let hay = normalize_for_containment(span);
        let name_near = (!subject_n.is_empty() && find_name(&hay, &subject_n, 0).is_some())
            || (!object_n.is_empty() && find_name(&hay, &object_n, 0).is_some());
        if !name_near {
            continue;
        }

        match scan_numeric_lexeme_evidence(
            &hay,
            value,
            numeric_tolerance,
            &subject_n,
            &object_n,
            &guards,
        ) {
            NumberScan::Evidential => return Ok(span.trim().to_string()),
            NumberScan::Refused(guard) => {
                first_refusal.get_or_insert((guard, span.trim().to_string()));
            }
            NumberScan::Absent => {}
        }
    }

    match first_refusal {
        Some((guard, span)) => Err(SupportRefusal::Guarded { guard, span }),
        None => {
            // B9: same split as `supporting_quote_or_refusal`, answered
            // with the lexeme scanner: `Absent` at whole-block scope means
            // no complete numeric lexeme renders the value under
            // tolerance — the matcher never saw the number at all.
            let whole = normalize_for_containment(block_text);
            if scan_numeric_lexeme_evidence(
                &whole,
                value,
                numeric_tolerance,
                &subject_n,
                &object_n,
                &guards,
            ) == NumberScan::Absent
            {
                return Err(SupportRefusal::ValueNotRendered);
            }
            Err(SupportRefusal::NoSpan)
        }
    }
}

/// Return whether an evidential numeric lexeme satisfies a caller's metadata
/// check.
///
/// Every candidate passes the same numeric parser, tolerance comparison,
/// subject/object proximity rule, and refusal guards as
/// [`supporting_quote_with_numeric_tolerance`] before `accepts` can see it.
/// The callback receives the normalized supporting span and the accepted
/// lexeme's byte range within that span. This is the safe integration point
/// for binding an adjacent unit: a refused equal-valued citation or identifier
/// can never donate its unit to another occurrence.
#[must_use]
pub fn evidential_numeric_lexeme_satisfies(
    subject: &str,
    object: &str,
    value: f64,
    block_text: &str,
    numeric_tolerance: f64,
    policy: &GuardPolicy,
    mut accepts: impl FnMut(&str, std::ops::Range<usize>) -> bool,
) -> bool {
    if !value.is_finite() || !numeric_tolerance.is_finite() || numeric_tolerance < 0.0 {
        return false;
    }

    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);
    let guards = GuardInputs::from_policy(policy);
    for span in supporting_spans(block_text) {
        let hay = normalize_for_containment(span);
        let name_near = (!subject_n.is_empty() && find_name(&hay, &subject_n, 0).is_some())
            || (!object_n.is_empty() && find_name(&hay, &object_n, 0).is_some());
        if !name_near {
            continue;
        }

        let mut search_from = 0usize;
        while search_from < hay.len() {
            let Some(first_char) = hay[search_from..].chars().next() else {
                break;
            };
            let Some(lexeme) = numeric_lexeme_at(&hay, search_from) else {
                search_from += first_char.len_utf8();
                continue;
            };
            search_from = lexeme.end;
            let Some(observed) = lexeme.value else {
                continue;
            };
            if numeric_values_match(value, observed, numeric_tolerance)
                && refusing_guard(
                    &hay,
                    lexeme.start,
                    lexeme.end,
                    &subject_n,
                    &object_n,
                    value,
                    &guards,
                )
                .is_none()
                && accepts(&hay, lexeme.start..lexeme.end)
            {
                return true;
            }
        }
    }
    false
}

/// Whether ANY complete numeric lexeme in `block_text` renders `value`
/// under `numeric_tolerance` — no subject/object proximity, no evidential
/// guards.
///
/// This is deliberately the WEAKEST matching this module offers, because it
/// exists to prove ABSENCE: the repair tier withdraws a refused numeric
/// fact with zero model calls only when the value appears nowhere in the
/// document in any rendered form, so the generous direction (a citation or
/// a range endpoint still counts as an appearance) is the conservative one.
/// It must never be used as evidence that a value IS supported — that is
/// [`supporting_quote_with_numeric_tolerance`]'s job, guards included.
#[must_use]
pub fn numeric_value_appears(value: f64, block_text: &str, numeric_tolerance: f64) -> bool {
    if !value.is_finite() || !numeric_tolerance.is_finite() || numeric_tolerance < 0.0 {
        return false;
    }
    let hay = normalize_for_containment(block_text);
    let mut search_from = 0usize;
    while search_from < hay.len() {
        let Some(first_char) = hay[search_from..].chars().next() else {
            break;
        };
        let Some(lexeme) = numeric_lexeme_at(&hay, search_from) else {
            search_from += first_char.len_utf8();
            continue;
        };
        search_from = lexeme.end;
        if lexeme
            .value
            .is_some_and(|observed| numeric_values_match(value, observed, numeric_tolerance))
        {
            return true;
        }
    }
    false
}

/// Split `block_text` into candidate supporting spans: sentences and table
/// rows. Spans are verbatim substrings (only trimmed), so anything found
/// here can be stored as a quote and later verified by containment.
///
/// Splitting is purely structural: sentence-final punctuation ends a span,
/// except a period inside a decimal or a dot-joined scientific token such
/// as `MPa.m^0.5`. CONTRACT CHANGE (de-hardcoding): there is no longer an
/// abbreviation vocabulary (the old `ABBREV_LABEL_WORDS`): deciding which
/// trailing periods abbreviate English label words was exactly the kind of
/// compiled-in language knowledge a promoted German ontology must not
/// depend on. The price — "Fig. 2" now splits, stranding the 2 in its own
/// span — is a weaker note, not a wrong verdict: a stranded label number
/// can only stamp when the span ALSO holds the subject or object, and the
/// fact carries its verification status either way.
fn supporting_spans(text: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut start = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            // Do not break inside a numeric decimal or a dot-joined
            // scientific unit such as `MPa.m^0.5`.
            let is_inline_token_point = b == &b'.'
                && i > 0
                && i + 1 < bytes.len()
                && bytes[i - 1].is_ascii_alphanumeric()
                && bytes[i + 1].is_ascii_alphanumeric();
            let is_sentence_break =
                matches!(b, b'.' | b'!' | b'?' | b';') && !is_inline_token_point;
            if is_sentence_break {
                spans.push(&line[start..=i]);
                start = i + 1;
            }
        }
        if start < line.len() {
            spans.push(&line[start..]);
        }
    }
    spans
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// What a scan of the value's occurrences in one span found.
#[derive(PartialEq)]
enum NumberScan {
    /// One occurrence is evidential.
    Evidential,
    /// No needle form of the value occurs in the span at all.
    Absent,
    /// Every occurrence was refused; holds the guard that refused the FIRST
    /// candidate occurrence scanned.
    Refused(RefusalGuard),
}

/// The resolved guard inputs for one scan, derived from the caller's
/// [`GuardPolicy`]: the ontology's sign declaration, and the fact's unit
/// term folded exactly like the text it is matched against. `unit_folded`
/// is `None` when the caller supplied no unit knowledge.
struct GuardInputs {
    sign: QuantitySignDomain,
    unit_folded: Option<String>,
}

impl GuardInputs {
    fn from_policy(policy: &GuardPolicy) -> Self {
        Self {
            sign: policy.quantity_sign,
            unit_folded: policy.unit_term.as_deref().map(str::to_lowercase),
        }
    }

    fn unit_n(&self) -> Option<&str> {
        self.unit_folded.as_deref()
    }
}

/// Does `hay` contain the value as an evidential measurement? At least one
/// string form of the value must occur with clean token boundaries, must
/// not be a citation marker, must not sit inside an occurrence of the
/// subject's or object's own name (the "718" of "Inconel 718"), and must
/// not be a dash range endpoint. When every occurrence is refused, the
/// scan names the guard that refused the FIRST one — that name is what
/// makes an over-refusal actionable, and first-wins keeps it causal
/// instead of positional.
///
/// B11 FIX: "first" is now first in POSITION. The old loop was
/// needle-form-major — every position of form 1, then every position of
/// form 2 — so for any value with two needle forms (>= 1000 or negative)
/// the reported guard belonged to whichever form the loop enumerated
/// first, not to the occurrence a reader meets first. Occurrences of all
/// forms are now gathered and examined in byte-position order; overlapping
/// spellings of the same lexeme ("1350" inside "-1350") keep their natural
/// left-to-right order, which the signed spelling wins.
fn scan_number_evidence(
    hay: &str,
    value: f64,
    subject_n: &str,
    object_n: &str,
    guards: &GuardInputs,
) -> NumberScan {
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for needle in number_needles(value) {
        let mut search_from = 0usize;
        while let Some(relative_offset) = hay[search_from..].find(&needle) {
            let start = search_from + relative_offset;
            let end = start + needle.len();
            candidates.push((start, end));
            // Advance by the needle's first CHARACTER, not one byte: U+2212
            // needles lead with a 3-byte char, and a rejected occurrence that
            // advanced one byte landed the next hay[search_from..] slice
            // inside the minus (char-boundary panic, whole ingest aborted).
            search_from = start + needle.chars().next().map_or(1, char::len_utf8);
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    let mut first: Option<RefusalGuard> = None;
    for (start, end) in candidates {
        match refusing_guard(hay, start, end, subject_n, object_n, value, guards) {
            None => return NumberScan::Evidential,
            Some(guard) => {
                first.get_or_insert(guard);
            }
        }
    }
    match first {
        Some(guard) => NumberScan::Refused(guard),
        None => NumberScan::Absent,
    }
}

#[derive(Clone, Copy)]
struct NumericLexeme {
    start: usize,
    end: usize,
    /// `None` means the complete source token is ambiguous or malformed. Its
    /// byte range is still returned so the scanner skips the token whole
    /// instead of reconsidering an interior suffix as a different number.
    value: Option<f64>,
}

/// Numeric counterpart to `scan_number_evidence`. Unlike `number_needles`,
/// this scans the document's complete numeric lexemes and compares their
/// parsed values. The exact source spelling and byte range still go through
/// `refusing_guard`; numeric equivalence never bypasses provenance guards.
fn scan_numeric_lexeme_evidence(
    hay: &str,
    value: f64,
    numeric_tolerance: f64,
    subject_n: &str,
    object_n: &str,
    guards: &GuardInputs,
) -> NumberScan {
    let mut first: Option<RefusalGuard> = None;
    let mut search_from = 0usize;
    while search_from < hay.len() {
        let Some(first_char) = hay[search_from..].chars().next() else {
            break;
        };
        let Some(lexeme) = numeric_lexeme_at(hay, search_from) else {
            search_from += first_char.len_utf8();
            continue;
        };
        search_from = lexeme.end;

        let Some(observed) = lexeme.value else {
            continue;
        };
        if !numeric_values_match(value, observed, numeric_tolerance) {
            continue;
        }
        match refusing_guard(
            hay,
            lexeme.start,
            lexeme.end,
            subject_n,
            object_n,
            value,
            guards,
        ) {
            None => return NumberScan::Evidential,
            Some(guard) => {
                first.get_or_insert(guard);
            }
        }
    }

    match first {
        Some(guard) => NumberScan::Refused(guard),
        None => NumberScan::Absent,
    }
}

fn numeric_values_match(expected: f64, observed: f64, numeric_tolerance: f64) -> bool {
    if !expected.is_finite()
        || !observed.is_finite()
        || !numeric_tolerance.is_finite()
        || numeric_tolerance < 0.0
    {
        return false;
    }
    if expected == 0.0 || observed == 0.0 {
        return expected == observed;
    }
    if expected.is_sign_negative() != observed.is_sign_negative() {
        return false;
    }
    let scale = expected.abs().max(observed.abs()).max(1.0);
    (expected - observed).abs() / scale <= numeric_tolerance
}

/// Parse a numeric lexeme beginning exactly at `start`. A leading sign stays
/// attached so the boundary guard sees the complete token; only a dash after
/// a digit is left outside the lexeme so `dash_range_endpoint` can recognize
/// the high endpoint of a digit/dash/digit range. The caller advances to
/// `end`, so digits inside a complete (including malformed or ambiguous)
/// candidate are never reconsidered as shorter substrings.
fn numeric_lexeme_at(hay: &str, start: usize) -> Option<NumericLexeme> {
    let first = hay[start..].chars().next()?;
    let mut cursor = start;
    if is_numeric_sign(first) {
        let sign_is_prefix = hay[..start]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_digit());
        if !sign_is_prefix {
            return None;
        }
        cursor += first.len_utf8();
        if !hay[cursor..].starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
    } else if !first.is_ascii_digit() {
        return None;
    }

    cursor += hay[cursor..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();

    while let Some(separator) = hay[cursor..].chars().next() {
        if !matches!(separator, '.' | ',') {
            break;
        }
        let after_separator = cursor + separator.len_utf8();
        if !hay[after_separator..].starts_with(|c: char| c.is_ascii_digit()) {
            break;
        }
        cursor = after_separator
            + hay[after_separator..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .map(char::len_utf8)
                .sum::<usize>();
    }

    if hay[cursor..].starts_with(['e', 'E']) {
        cursor += 1;
        if let Some(sign) = hay[cursor..].chars().next()
            && is_numeric_sign(sign)
        {
            cursor += sign.len_utf8();
        }
        let exponent_digits = hay[cursor..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .map(char::len_utf8)
            .sum::<usize>();
        if exponent_digits == 0 {
            // Keep the exponent marker, its first sign, and any immediately
            // following sign/digit run inside one malformed token. Resetting
            // to `exponent_mark` made `1e--3` rescan `-3` as independent
            // evidence.
            cursor += hay[cursor..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || is_numeric_sign(*c))
                .map(char::len_utf8)
                .sum::<usize>();
        } else {
            cursor += exponent_digits;
        }
    }

    let raw = &hay[start..cursor];
    let value = parse_numeric_lexeme(raw);
    Some(NumericLexeme {
        start,
        end: cursor,
        value,
    })
}

fn is_numeric_sign(c: char) -> bool {
    matches!(c, '+' | '-' | '\u{2212}')
}

/// Parse one complete source lexeme. A lone comma followed by three digits is
/// ambiguous (`1,140` is either 1140 or 1.140), so it fails closed. Other lone
/// commas are decimal, multiple three-digit comma groups are grouped integers,
/// and mixed punctuation supports the unambiguous `1,140.5` form while
/// rejecting the inverse.
fn parse_numeric_lexeme(raw: &str) -> Option<f64> {
    let normalized: String = raw
        .chars()
        .map(|c| if c == '\u{2212}' { '-' } else { c })
        .collect();
    let exponent_at = normalized.find(['e', 'E']);
    let (mantissa, exponent) = match exponent_at {
        Some(at) => {
            if normalized[at + 1..].contains(['e', 'E']) {
                return None;
            }
            (&normalized[..at], Some(&normalized[at + 1..]))
        }
        None => (normalized.as_str(), None),
    };

    let (sign, unsigned_mantissa) = match mantissa.as_bytes().first() {
        Some(b'+' | b'-') => (&mantissa[..1], &mantissa[1..]),
        _ => ("", mantissa),
    };
    if unsigned_mantissa.is_empty() {
        return None;
    }

    let comma_count = unsigned_mantissa.bytes().filter(|b| *b == b',').count();
    let dot_count = unsigned_mantissa.bytes().filter(|b| *b == b'.').count();
    let canonical_mantissa = match (comma_count, dot_count) {
        (0, 0) if unsigned_mantissa.bytes().all(|b| b.is_ascii_digit()) => {
            unsigned_mantissa.to_string()
        }
        (0, 1) => {
            let (integer, fraction) = unsigned_mantissa.split_once('.')?;
            if integer.is_empty()
                || fraction.is_empty()
                || !integer.bytes().all(|b| b.is_ascii_digit())
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            unsigned_mantissa.to_string()
        }
        (1, 0) => {
            let (integer, trailing) = unsigned_mantissa.split_once(',')?;
            if integer.is_empty()
                || trailing.is_empty()
                || !integer.bytes().all(|b| b.is_ascii_digit())
                || !trailing.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            if integer != "0" && trailing.len() == 3 {
                return None;
            }
            format!("{integer}.{trailing}")
        }
        (_, 0) if comma_grouped_integer(unsigned_mantissa) => unsigned_mantissa.replace(',', ""),
        (_, 1) => {
            let (integer, fraction) = unsigned_mantissa.split_once('.')?;
            if !comma_grouped_integer(integer)
                || fraction.is_empty()
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            format!("{}.{}", integer.replace(',', ""), fraction)
        }
        _ => return None,
    };

    let mut canonical = format!("{sign}{canonical_mantissa}");
    if let Some(exponent) = exponent {
        let exponent_digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        if exponent_digits.is_empty() || !exponent_digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        canonical.push('e');
        canonical.push_str(exponent);
    }
    canonical
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn comma_grouped_integer(value: &str) -> bool {
    let mut groups = value.split(',');
    let Some(first) = groups.next() else {
        return false;
    };
    if first.is_empty()
        || first.len() > 3
        || !first.bytes().all(|b| b.is_ascii_digit())
        || first == "0"
    {
        return false;
    }
    let mut group_count = 0usize;
    for group in groups {
        group_count += 1;
        if group.len() != 3 || !group.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    group_count > 0
}

/// The guard that refuses the occurrence at [start, end) in `hay`, or
/// `None` when the occurrence is evidential. Checked most-specific first so
/// the NAMED refusal is the most informative one; the refuse/accept decision
/// itself does not depend on the order.
///
/// `SignDomain` is checked first though it is claim-level, not
/// occurrence-level: a negative value against a quantity the ontology
/// declares non-negative is nonsense whatever the occurrence looks like,
/// and naming it beats every positional guard's explanation. It is checked
/// INSIDE the occurrence loop, not hoisted claim-level before it, for one
/// reason: attribution. A negative claim whose value does NOT occur in the
/// block is NoSpan (the model's fault — it cited a value the block never
/// contained), not Guarded{SignDomain} (the matcher's fault). Hoisting the
/// check before the loop reddens
/// `sign_domain_does_not_mask_a_non_occurring_value_as_no_span`.
fn refusing_guard(
    hay: &str,
    start: usize,
    end: usize,
    subject_n: &str,
    object_n: &str,
    value: f64,
    guards: &GuardInputs,
) -> Option<RefusalGuard> {
    if value < 0.0 && guards.sign == QuantitySignDomain::NonNegative {
        return Some(RefusalGuard::SignDomain);
    }
    if dash_range_endpoint(hay, start, end) {
        return Some(RefusalGuard::Range);
    }
    if uncertainty_decoration(hay, start) {
        return Some(RefusalGuard::Uncertainty);
    }
    if !clean_number_boundary(hay, start, end, guards.unit_n()) {
        return Some(RefusalGuard::Boundary);
    }
    if inside_citation_marker(hay, start, end) {
        return Some(RefusalGuard::Citation);
    }
    if occurrence_inside_name(hay, start, end, subject_n)
        || occurrence_inside_name(hay, start, end, object_n)
    {
        return Some(RefusalGuard::InsideName);
    }
    if separator_or_paren_dash_on_signed_value(hay, start, end, object_n, value, guards.unit_n()) {
        return Some(RefusalGuard::SeparatorDash);
    }
    None
}

/// Does the occurrence at [start, end) sit inside an occurrence of the
/// claim's own subject/object name? Space-separated designations like
/// "Inconel 718" have clean token boundaries around their trailing digits,
/// so the boundary rule cannot tell the "718" of the name from a measured
/// 718; positional containment inside the name can. An occurrence that
/// repeats OUTSIDE the name is still evidence. Name matching here folds
/// the dash class (`find_name`), exactly as the name-presence check does.
fn occurrence_inside_name(hay: &str, start: usize, end: usize, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut search_from = 0usize;
    while let Some((name_start, name_end)) = find_name(hay, name, search_from) {
        // Numeric canonicalization may make the source lexeme longer than the
        // model's name spelling (`Inconel 718.0` vs `Inconel 718`). Any
        // overlap is designation evidence, not an independent measurement.
        if name_start < end && start < name_end {
            return true;
        }
        // Char-not-byte advance, as in evidential_number_occurrence:
        // names may lead with a multi-byte char ("\u{3b1}-phase").
        search_from = name_start + hay[name_start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Round 16: refuse a SIGNED needle whose dash is in SEPARATOR or
/// PARENTHETICAL role, not sign role, on a quantity `SignDomain` cannot
/// touch (a genuinely-signed predicate such as residual stress). Two
/// shapes, each closed by its own sub-condition; both pin the SAME
/// fabrication class — a compressive value stamped from a tensile
/// source. See `RefusalGuard::SeparatorDash` for the shape catalogue.
///
/// Why the object NAME and not another word list: round 12 measured a
/// generic "preceding word" rule and it failed in both directions
/// (it could not read the line-start shape, and "From Fig. 6, -950"
/// has no word before the dash either). The information round 12
/// lacked is the object's OWN name, already in scope here as
/// `object_n`. In the separator shape the name abuts the dash; in a
/// true minus it follows a verb or preposition ("was", "at",
/// "reached"). The name is precise where a word list was not.
///
/// (A) label-inline — `object_n` is the trailing token(s) of the text
/// before the dash (one optional space between). A word boundary
/// before it stops a suffix of a longer word matching ("distress"
/// must not match object "stress").
///
/// (B) bracketed — the fact's OWN unit term (`unit_n`, supplied by the
/// caller from the reader/ontology — Rust holds no unit lexicon) sits
/// right after the value (one optional space), immediately followed by
/// a `MINUS_CAPABLE_DASH` that is NOT followed by a digit. That glued
/// dash is the closing parenthetical a minus never carries; the
/// not-a-digit guard leaves digit/dash/digit ranges (owned by the
/// `Range` guard, checked first) untouched. Without a supplied unit
/// term this shape is inert: no guess replaces it.
///
/// DELIBERATELY not refused — the line-start shape "-950 MPa was
/// recorded" has neither signal and is locally indistinguishable from
/// a genuine line-start minus ("-350 MPa was the surface stress"). It
/// is carried as a KNOWN live fabrication in the corpus.
fn separator_or_paren_dash_on_signed_value(
    hay: &str,
    start: usize,
    end: usize,
    object_n: &str,
    value: f64,
    unit_n: Option<&str>,
) -> bool {
    if value >= 0.0 {
        return false;
    }
    // (A) the object's own name abuts the dash as complete trailing
    // words. `strip_suffix` is an exact match; the boundary check
    // before it stops a longer word's suffix matching.
    if !object_n.is_empty()
        && let Some(before_obj) = hay[..start].trim_end().strip_suffix(object_n)
    {
        let boundary = before_obj.is_empty()
            || before_obj
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric());
        if boundary {
            return true;
        }
    }
    // (B) the fact's own unit term right after the value, immediately
    // followed by a dash that is not followed by a digit (so a
    // digit/dash/digit range stays owned by the `Range` guard).
    let Some(unit_n) = unit_n.filter(|u| !u.is_empty()) else {
        return false;
    };
    let rest = hay[end..].strip_prefix(' ').unwrap_or(&hay[end..]);
    match rest.strip_prefix(unit_n) {
        Some(tail) => {
            // The unit term must end on a token boundary, exactly as the
            // exact-match resolver requires.
            let unit_ended_clean = tail.chars().next().is_none_or(|c| !c.is_alphanumeric());
            unit_ended_clean
                && matches!(tail.chars().next(), Some(d) if MINUS_CAPABLE_DASHES.contains(&d)
                    && tail[d.len_utf8()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_ascii_digit()))
        }
        None => false,
    }
}

/// Dash-class-aware name search: the first occurrence of `name` in
/// `hay` at or after `from`, treating every glyph of
/// `MINUS_CAPABLE_DASHES` as the SAME character. Returns the matched
/// byte range — its length may differ from `name.len()`, because a
/// 3-byte U+2013 in `hay` matches a 1-byte '-' in `name`.
///
/// Round 12 item 4: typesetting variants of one designation are the
/// SAME alloy — "Ti\u{2013}6Al\u{2013}4V" and "Ti-6Al-4V" differ only in the
/// dash the typesetter picked. Before this landed, the en-dash
/// spelling matched the ASCII subject only through the object arm of
/// the subject-OR-object rule: the object arm carrying a
/// subject-matching failure. Deliberately applied to NAME matching
/// only — the value scan still reads the raw `hay`, so U+2013 stays a
/// non-sign glyph exactly as round 11 decided; folding the scanned
/// text itself would turn "\u{2013}350" into "-350" and reopen the
/// separator fabrication through the back door.
fn find_name(hay: &str, name: &str, from: usize) -> Option<(usize, usize)> {
    let name_chars: Vec<char> = name.chars().collect();
    if name_chars.is_empty() || from > hay.len() {
        return None;
    }
    let dash_eq = |got: char, want: char| {
        got == want || (MINUS_CAPABLE_DASHES.contains(&got) && MINUS_CAPABLE_DASHES.contains(&want))
    };
    let mut start = from;
    while let Some(first) = hay[start..].chars().next() {
        let mut pos = start;
        let mut matched = true;
        for &want in &name_chars {
            match hay[pos..].chars().next() {
                Some(got) if dash_eq(got, want) => pos += got.len_utf8(),
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if matched {
            return Some((start, pos));
        }
        start += first.len_utf8();
    }
    None
}

/// The dash class: every glyph the typeset world uses where a minus
/// sign can stand. ASCII hyphen; U+2010 HYPHEN and U+2011
/// NON-BREAKING HYPHEN are ordinary PDF-extractor output; U+2012 is
/// literally named FIGURE DASH; U+2013 EN DASH and U+2014 EM DASH
/// are the typeset range/sentence dashes; U+2015 HORIZONTAL BAR and
/// U+FE63 SMALL HYPHEN-MINUS round out the measured set. Round 8
/// listed only '-', U+2212 and U+2013 here and let the other six
/// flip signs; round 9 made it a class. The joins-compound test
/// inside `clean_number_boundary` separates every member: a range
/// dash has a digit before it, a minus sign does not.
const MINUS_CAPABLE_DASHES: &[char] = &[
    '-', '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
    '\u{fe63}',
];

/// Token-boundary check: the occurrence must not be adjacent to a digit, to
/// a decimal point that continues it, to a digit-adjacent comma that
/// continues a grouped number ("1,140" is one number, in both directions),
/// to a leading minus that signs it ("-950" / "\u{2212}950" are one number),
/// or to an alphanumeric. Otherwise "95" matches inside "950", "1.5" inside
/// "11.5", "140" inside "1,140", and the "6" of "Ti-6Al-4V". After the
/// number, the fact's OWN unit term — exactly the term the reader/ontology
/// chose, no Rust unit lexicon — redeems a glued letter ("950MPa" with unit
/// "MPa"); without a supplied unit term every glued alphanumeric refuses.
/// En-dash range endpoints are refused by `dash_range_endpoint`, not here.
fn clean_number_boundary(hay: &str, start: usize, end: usize, unit_n: Option<&str>) -> bool {
    // The occurrence's own first character, derived from its byte range —
    // the caller no longer threads a needle it can always reconstruct.
    let needle = &hay[start..end];
    if let Some(before) = hay[..start].chars().next_back() {
        if before.is_alphanumeric() {
            return false;
        }
        if MINUS_CAPABLE_DASHES.contains(&before) {
            // A leading minus is part of the number: an unsigned needle
            // must not match the digits of a signed token ("950" inside
            // "-950" or "\u{2212}950"), or the sign-flipped claim stamps
            // as fact — for residual stress that turns compressive into
            // tensile, worse than a miss. The minus is a sign only when
            // it does not join a compound: whitespace, line start or
            // opening punctuation before it. A letter or digit before it
            // is the hyphen of a designation ("ti-6al-4v") or a
            // digit-joined compound, which the designation guards own.
            //
            // The glyph set is the whole dash class
            // (`MINUS_CAPABLE_DASHES`), not a hand-picked trio: round 7
            // added U+2013 for the MINUS half only, round 8 recorded
            // U+2014 as the unrecorded twin and fixed nothing, and
            // round 9 measured the sign flip still stamping through
            // U+2010, U+2011, U+2012, U+2015 and U+FE63. The
            // joins-compound test separates every member the same way,
            // and `dash_range_endpoint` (checked first) keeps
            // refusing the range case as Range. Round 11 reverted
            // round 10's making U+2013/U+2014 sign glyphs — the
            // separator shape fabricated negatives (see
            // `number_needles`) — so the true "\u{2013}350" claim
            // drops again; this clause still kills the UNSIGNED
            // needle's sign-flipped match for every glyph of the class.
            let joins_compound = hay[..start - before.len_utf8()]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
            if !joins_compound {
                return false;
            }
        }
        if before == '.' && needle.starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        if before == ',' && hay[..start - before.len_utf8()].ends_with(|c: char| c.is_ascii_digit())
        {
            return false;
        }
    }
    if let Some(after) = hay[end..].chars().next() {
        if after.is_alphanumeric() {
            if !glued_fact_unit(hay, end, unit_n) {
                return false;
            }
            // A glued unit letter redeems a number, but not a digit that
            // a LETTER dash-joins into a designation (the "6" of
            // "Ti-6Al-4V", under any glyph of the dash class — round 10
            // widened the old trio; U+2011 NON-BREAKING HYPHEN is the
            // glyph a typesetter picks so Ti-6Al-4V survives
            // line-breaking, the likeliest one in a real PDF). A dash
            // leading the span (no char before it) keeps the refusal.
            // Deletion of this clause is mutation-proven by the en-dash
            // object-arm case in
            // dash_range_endpoints_with_glued_units_are_not_point_values:
            // with it gone, the "6" of Ti\u{2013}6Al\u{2013}4V stamps.
            //
            // Round 10: the digit-before-dash half of this clause — the
            // "redemption" that let the second number of "30-50um" keep
            // its unit and stamp — is owned FIRST by
            // `dash_range_endpoint`: a digit/dash/digit run is a range
            // whatever the glyph, and round 9's ground truth made both
            // endpoints MustDrop. The digit condition below survives as
            // defense-in-depth. Its old recorded residue, the "ASTM
            // E466-15a" standard designator (round 7), is refused as a
            // range too.
            if let Some(before) = hay[..start].chars().next_back()
                && MINUS_CAPABLE_DASHES.contains(&before)
                && hay[..start - before.len_utf8()]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_ascii_digit())
            {
                return false;
            }
        }
        if after == '.' && hay[end + 1..].starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        if after == ',' && hay[end + after.len_utf8()..].starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

/// Whether the fact's OWN unit term begins exactly at `pos` in `hay`, glued
/// to the number (no space). `hay` and the term are both already folded
/// (lowercased); the term must end on a token boundary, exactly as the
/// exact-match unit resolvers in `prism_provenance::units` require. This is
/// the ONLY redemption for a letter glued to a number — Rust holds no unit
/// lexicon, so an unclaimed glued letter ("950x", "2e5", "950z") refuses.
fn glued_fact_unit(hay: &str, pos: usize, unit_n: Option<&str>) -> bool {
    let Some(unit_n) = unit_n.filter(|u| !u.is_empty()) else {
        return false;
    };
    let Some(tail) = hay[pos..].strip_prefix(unit_n) else {
        return false;
    };
    tail.chars().next().is_none_or(|c| !c.is_alphanumeric())
}

/// Range endpoints on a dash: "950\u{2013}1100 MPa" asserts a range,
/// not two point values, so a number that opens or closes a
/// digit/dash/digit run is refused. Round 10: the dash set is the whole
/// dash class (`MINUS_CAPABLE_DASHES`), not U+2013 only. Ground truth
/// does not depend on which glyph the typesetter or extractor emitted —
/// "30-50um layers", "950-1100 batches" and "E1820-20b" are ranges and
/// designators under the ASCII hyphen exactly as under the en dash, and
/// the round-4 compound-friendly exception stamped batch identifiers as
/// property records with perfect provenance. Round 9's ground-truth pick
/// for the ASCII range form (MustDrop) made the exception untenable.
/// A SIGNED value is not a range endpoint: a range dash always has a
/// digit before it, a minus sign does not ("-950" carries whitespace or
/// line start before the dash), so signed needles survive this guard.
/// Recorded residual gaps, restored to the record round 11 (round 10
/// recorded only the word form in the round that WIDENED the gap):
/// spaced ranges ("950 to 1100"), spaced-dash ranges ("950 \u{2013}
/// 1100" and its ASCII twin), and negative ranges (the low endpoint of
/// "-950--400" stamps; "-950 to -400" stamps both endpoints) still
/// stamp their endpoints — the guard only sees digit/dash/digit
/// adjacency, so one space defeats it, and after the second dash of a
/// double-dash comes a sign, not a digit. Round 10's reversal took the
/// negative-range shape from two sign glyphs to four; round 11's
/// revert restored the two ('-', U+2212) but not the drop. Round 12:
/// a NEGATIVE range against a NONNEGATIVE quantity now drops through
/// the SignDomain guard before the adjacency question arises (its
/// corpus rows graduated from KNOWN the day the guard landed); the
/// adjacency gap itself stays open for signed quantities and positive
/// values. All pinned as KNOWN corpus rows.
fn dash_range_endpoint(hay: &str, start: usize, end: usize) -> bool {
    // B12: the trims tolerate the spaced forms ("950 \u{2013} 1100") that
    // journal typesetting and pdf-extract both emit; a digit still has to
    // sit on the FAR side of the dash, so a spaced unary minus ("was -
    // 950 MPa", where the far side is a word) never becomes a range.
    // The trimmed slice's offsets are RELATIVE to it — all indexing below
    // stays inside the slice, never re-based on `hay` (re-basing was the
    // first draft of this fix and it panicked mid-multibyte).
    let after_ws = hay[end..].trim_start_matches(' ');
    if let Some(after) = after_ws.chars().next()
        && MINUS_CAPABLE_DASHES.contains(&after)
        && after_ws[after.len_utf8()..]
            .trim_start_matches(' ')
            .starts_with(|c: char| c.is_ascii_digit())
    {
        return true;
    }
    let before_ws = hay[..start].trim_end_matches(' ');
    if let Some(before) = before_ws.chars().next_back()
        && MINUS_CAPABLE_DASHES.contains(&before)
        && before_ws[..before_ws.len() - before.len_utf8()]
            .trim_end_matches(' ')
            .ends_with(|c: char| c.is_ascii_digit())
    {
        return true;
    }
    false
}

/// B8 (the ± half): is the occurrence at `start` an UNCERTAINTY figure —
/// the token immediately to its left (skipping spaces) is the plus-minus
/// notation? Both spellings of the notation are mathematical symbols, not
/// domain vocabulary: `\u{00b1}` (PLUS-MINUS SIGN) and the ASCII run
/// `+/-`. The MIRROR shape (the value a tolerance decorates, e.g. 950 in
/// "950 ± 30 MPa") has a word or nothing to its left and stamps normally —
/// pinned by the corpus control row.
fn uncertainty_decoration(hay: &str, start: usize) -> bool {
    let before = hay[..start].trim_end_matches(' ');
    if before.ends_with('\u{00b1}') {
        return true;
    }
    before.ends_with("+/-")
}

/// Is the occurrence inside a citation marker? Bracketed styles
/// (`[1140]`, `[11, 12]`, `[11–13]`) and the paren/brace styles that
/// survive as bare numbers in text (`(1140)`, `{1140, 1141}`). Walk back
/// over the digits and separators a citation range may contain; if the
/// first other character is `[`, the number is a citation, not a
/// measurement. `(` / `{` count only when the marker also CLOSES before
/// any non-number text: "(950 MPa)" is a parenthesized value, not a
/// citation, so a bracket-class rule that only looks backwards would
/// refuse legitimate values.
///
/// Round 10: the range separator is the whole dash class, not the
/// hand-picked trio. `[11\u{2010}13]` and its five other glyph twins
/// walked back only to the dash, never saw the bracket, and stamped
/// the second citation number as a measurement.
fn inside_citation_marker(hay: &str, start: usize, end: usize) -> bool {
    let prefix = hay[..start].trim_end_matches(is_citation_numeric_syntax);
    let Some(open) = prefix.chars().next_back() else {
        return false;
    };
    match open {
        '[' => true,
        '(' | '{' => {
            let close = if open == '(' { ')' } else { '}' };
            let after = hay[end..].trim_start_matches(is_citation_numeric_syntax);
            after.starts_with(close)
        }
        _ => false,
    }
}

fn is_citation_numeric_syntax(c: char) -> bool {
    c.is_ascii_digit()
        || c.is_ascii_whitespace()
        || matches!(c, ',' | ';' | '.' | '+' | 'e' | 'E')
        || MINUS_CAPABLE_DASHES.contains(&c)
}

/// String forms under which a numeric value may legitimately appear in
/// a paper: the plain rendering plus the comma-grouped integer form. A
/// negative value appears under TWO minus glyphs: ASCII hyphen and
/// U+2212 MINUS SIGN (the glyph typeset PDFs and JATS carry); without
/// both, the true negative claim is dropped while its sign-flipped twin
/// stamps. The U+2212 sign attaches to the PLAIN rendering too, not just
/// the comma-grouped one: an un-grouped "\u{2212}1350" (|value| >= 1000)
/// otherwise has no needle and drops NoSpan while pdf-extract's "-1350"
/// stamps — a fetch-route divergence, closed round 11.
///
/// REVERTED, ROUND 11 (was FIXED round 10, DECIDED round 7,
/// DECIDED-NOT-IMPLEMENTED round 6): U+2013 EN DASH and U+2014 EM DASH
/// are NOT needle glyphs. Round 10 made them sign glyphs on the argument
/// "a range dash always has a digit before it, a minus does not". That
/// separates a minus from a RANGE but NOT from a SEPARATOR: a
/// label/value separator also has no digit before the dash. Measured
/// round 11, the separator shapes stamped a negative for a positive
/// source value — "Ti-6Al-4V UTS \u{2013}950 MPa" stamped -950 and
/// dropped the correct +950, recording tensile as compressive, the exact
/// fabrication this branch exists to stop. The minus and separator
/// readings are locally indistinguishable (both word/dash/digit), so the
/// branch's priority decides: a fabrication is worse than a miss.
///
/// COVERAGE, corrected round 12 and re-grounded by the de-hardcoding
/// contract: the revert holds for U+2013/U+2014, and the sign domain the
/// ONTOLOGY declares for the quantity ([`GuardPolicy::quantity_sign`])
/// covers what the revert could not. U+2212 stays a sign glyph and ASCII
/// '-' always was one; for a quantity the ontology declares non-negative,
/// a negative claim is nonsense under EVERY glyph —
/// `RefusalGuard::SignDomain` refuses it and the separator shapes drop
/// for the right reason. Where the ontology is SILENT the round-11
/// sentence "unambiguously a minus, never a separator" stays struck: for
/// such quantities the minus-vs-separator reading of '-'/'\u{2212}' is
/// locally indistinguishable, and the engine keeps the MINUS reading —
/// the glyph IS the minus sign, so a negative that stamps is the
/// defensible reading (its recall twin, the correct positive, still drops
/// Boundary; corpus KNOWN row). Silence is the permissive direction, and
/// it is never papered over with a quantity-name list. Under
/// U+2013/U+2014 the true negative drops (recall loss, corpus KNOWN rows)
/// and the separator shapes drop (corpus MustDrop pins).
///
/// FETCH ROUTE, the module header's class: header item (c) carries
/// the route view. Round 11 reopened it — the paper whose JATS
/// spelling ("UTS \u{2013}950 MPa") round 11 pinned MustDrop fabricated
/// -950 through the PDF route; the ontology-served sign domain is what
/// closes that fabrication half where it is declared. For signed (or
/// undeclared) quantities a recall divergence remains (JATS U+2013
/// drops, PDF '-' stamps the genuine negative), recorded as the header
/// describes.
///
/// FIXED, ROUND 9 (was RECORDED, NOT FIXED, round 8): the sign flip
/// stamped through U+2010, U+2011, U+2012, U+2014, U+2015 and U+FE63 as
/// well. `clean_number_boundary` refuses the unsigned needle after ANY
/// glyph of the dash class (`MINUS_CAPABLE_DASHES`) that does not join a
/// compound. The true negative under
/// U+2010/U+2011/U+2012/U+2013/U+2014/U+2015/U+FE63 drops — none of
/// those is a needle glyph; every glyph's sign-flipped twin is a
/// MustDrop pin.
fn number_needles(value: f64) -> Vec<String> {
    let plain = format!("{value}");
    let mut out = vec![plain.clone()];
    if value < 0.0 {
        // The U+2212 MINUS SIGN rendering of the PLAIN (un-grouped)
        // form, attached for every negative — not only decimals: an
        // integer below -1000 otherwise has only the comma-grouped
        // "\u{2212}1,350" needle, so the un-grouped "\u{2212}1350"
        // JATS preserves would drop NoSpan while pdf-extract's "-1350"
        // stamps — a fetch-route divergence, closed round 11. U+2212
        // only: U+2013/U+2014 are not sign glyphs (see the fn doc).
        out.push(format!("\u{2212}{}", &plain[1..]));
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        let digits = (value as i64).abs().to_string();
        let grouped: String = digits
            .chars()
            .rev()
            .enumerate()
            .flat_map(|(i, c)| {
                let mut v = Vec::with_capacity(2);
                if i > 0 && i % 3 == 0 {
                    v.push(',');
                }
                v.push(c);
                v
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let signs: &[&str] = if value < 0.0 {
            &["-", "\u{2212}"]
        } else {
            &[""]
        };
        for sign in signs {
            let needle = format!("{sign}{grouped}");
            if !out.contains(&needle) {
                out.push(needle);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fulltext::{BlockKind, Locator};

    fn locator() -> Locator {
        Locator {
            kind: BlockKind::Body,
            section_path: vec!["2. Results".to_string()],
            label: None,
            char_offset: 0,
        }
    }

    const BLOCK: &str = "We measured CoCrFeNi. Its thermal conductivity is 11.5 W/(m K) \
                         at room temperature.";
    const QUOTE: &str = "thermal conductivity is 11.5 W/(m K)";

    fn claim(value: Option<f64>, unit: Option<&str>, evidence: &str) -> ExtractedClaim {
        claim_with_quote(value, unit, evidence, Some(QUOTE))
    }

    fn claim_with_quote(
        value: Option<f64>,
        unit: Option<&str>,
        evidence: &str,
        quote: Option<&str>,
    ) -> ExtractedClaim {
        ExtractedClaim {
            subject: "CoCrFeNi".to_string(),
            predicate: "has_measurement".to_string(),
            object: "thermal_conductivity".to_string(),
            value,
            unit: unit.map(str::to_string),
            conditions: vec![],
            confidence: Some(0.9),
            kind: Some("measurement".to_string()),
            evidence_class: evidence.to_string(),
            verification: None,
            verification_reason: None,
            ontology: Default::default(),
            provenance: ClaimProvenance {
                document_id: "10.1234/hea".to_string(),
                document_url: "https://doi.org/10.1234/hea".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote: quote.map(str::to_string),
                source_revision_id: None,
                line_start: None,
                line_end: None,
                source_text_path: None,
            },
        }
    }

    /// The extractor prompt's own Ti-6Al-4V example fact, verbatim
    /// (crates/ingest/src/text_extract.rs). It is the canonical fabrication
    /// vector: a model parroting the example back must never produce a
    /// stamped claim pointing at a document that does not contain it.
    fn prompt_example_claim() -> ExtractedClaim {
        ExtractedClaim {
            subject: "Ti-6Al-4V".to_string(),
            predicate: "has_measurement".to_string(),
            object: "UTS".to_string(),
            value: Some(1140.0),
            unit: Some("QUDT:MegaPA".to_string()),
            conditions: vec![
                MeasurementCondition {
                    name: "temperature".to_string(),
                    value: ConditionValue::Number(298.15),
                    unit: Some("QUDT:K".to_string()),
                },
                MeasurementCondition {
                    name: "atmosphere".to_string(),
                    value: ConditionValue::Text("air".to_string()),
                    unit: None,
                },
            ],
            confidence: Some(0.9),
            kind: Some("measurement".to_string()),
            evidence_class: "research".to_string(),
            verification: None,
            verification_reason: None,
            ontology: Default::default(),
            provenance: ClaimProvenance {
                document_id: "10.1234/unrelated".to_string(),
                document_url: "https://doi.org/10.1234/unrelated".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote: None,
                source_revision_id: None,
                line_start: None,
                line_end: None,
                source_text_path: None,
            },
        }
    }

    /// A policy carrying the ontology's declaration that the claimed
    /// quantity is non-negative — what the ontology serves at grounding
    /// time; the matcher never derives it from the name.
    fn nonnegative_policy() -> GuardPolicy {
        GuardPolicy {
            quantity_sign: QuantitySignDomain::NonNegative,
            unit_term: None,
        }
    }

    /// A policy carrying the fact's own unit term, exactly as the
    /// reader/ontology chose it.
    fn unit_policy(term: &str) -> GuardPolicy {
        GuardPolicy {
            quantity_sign: QuantitySignDomain::Unspecified,
            unit_term: Some(term.to_string()),
        }
    }

    /// The absence check is deliberately WEAKER than evidential matching:
    /// rendered-form equivalence counts, and so does a guarded occurrence
    /// (a citation label is still an appearance). Only a value the document
    /// never prints at all fails it.
    #[test]
    fn numeric_value_appears_is_the_weakest_check() {
        // Rendered forms of 1.2 all count.
        for text in [
            "modulus was 1.2 GPa",
            "modulus was 1.20 GPa",
            "modulus was 1,2 GPa",
        ] {
            assert!(numeric_value_appears(1.2, text, 1e-9), "{text}");
        }
        // A citation occurrence would be REFUSED as evidence, but it is
        // still an appearance — near-miss, not invention.
        assert!(numeric_value_appears(
            1.2,
            "prior work [1.20] reported this",
            1e-9
        ));
        // Absent means absent: no rendering anywhere, and a different
        // number containing the same digits is not a rendering.
        assert!(!numeric_value_appears(
            0.935,
            "the accuracy was described qualitatively",
            1e-9
        ));
        assert!(!numeric_value_appears(
            0.935,
            "sample 935 of the batch",
            1e-9
        ));
    }

    #[test]
    fn literature_can_never_be_promoted() {
        assert_eq!(cap_at_literature("reference_validated"), "research");
        assert_eq!(cap_at_literature("screening"), "research");
        assert_eq!(cap_at_literature("research"), "research");
        assert_eq!(cap_at_literature("indeterminate"), "indeterminate");
        assert_eq!(cap_at_literature("garbage"), "indeterminate");
    }

    #[test]
    fn stamped_claim_keeps_research_ceiling() {
        let promoted = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "reference_validated");
        let stamped = validate_and_stamp(promoted, BLOCK).unwrap();
        assert_eq!(stamped.evidence_class, "research");
    }

    #[test]
    fn numeric_value_without_unit_is_refused() {
        let bare = claim(Some(11.5), None, "research");
        assert_eq!(
            validate_and_stamp(bare, BLOCK).unwrap_err(),
            ClaimRejection::NumericValueWithoutUnit
        );
    }

    #[test]
    fn numeric_condition_without_unit_is_refused() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.conditions.push(MeasurementCondition {
            name: "temperature".to_string(),
            value: ConditionValue::Number(300.0),
            unit: None,
        });
        assert_eq!(
            validate_and_stamp(c, BLOCK).unwrap_err(),
            ClaimRejection::NumericConditionWithoutUnit {
                condition: "temperature".to_string()
            }
        );
    }

    #[test]
    fn textual_condition_without_unit_is_allowed() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.conditions.push(MeasurementCondition {
            name: "atmosphere".to_string(),
            value: ConditionValue::Text("air".to_string()),
            unit: None,
        });
        assert!(validate_and_stamp(c, BLOCK).is_ok());
    }

    /// F2 regression. The reviewer fed the extractor prompt's own Ti-6Al-4V
    /// example through the validator and it was accepted at confidence 0.9
    /// with a locator pointing at a document that never contained it. The
    /// containment contract must refuse it: without a quote it is
    /// unverifiable, and even the example text itself is not in the block.
    #[test]
    fn prompt_example_fact_is_dropped_not_stamped() {
        let unrelated_block = "We report a novel magnesium alloy with 60 HV hardness.";

        // As the reviewer reproduced it: no quote at all.
        let as_reviewed = prompt_example_claim();
        assert_eq!(as_reviewed.confidence, Some(0.9));
        assert_eq!(
            validate_and_stamp(as_reviewed, unrelated_block).unwrap_err(),
            ClaimRejection::MissingQuote
        );

        // Even attaching the example sentence as a quote cannot fake
        // containment: the cited block does not contain it.
        let mut with_quote = prompt_example_claim();
        with_quote.provenance.quote =
            Some("Ti-6Al-4V has a UTS of 1140 MPa at 298.15 K in air".to_string());
        assert_eq!(
            validate_and_stamp(with_quote, unrelated_block).unwrap_err(),
            ClaimRejection::QuoteNotInCitedBlock
        );

        // And the supporting-quote finder finds nothing for it in the block.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                unrelated_block,
                &GuardPolicy::SILENT
            )
            .is_none()
        );
    }

    #[test]
    fn claim_whose_quote_is_not_in_its_block_is_dropped() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.provenance.quote = Some("thermal conductivity is 99 W/(m K)".to_string());
        assert_eq!(
            validate_and_stamp(c, BLOCK).unwrap_err(),
            ClaimRejection::QuoteNotInCitedBlock
        );
    }

    #[test]
    fn claim_without_quote_is_dropped_not_stamped() {
        let bare = claim_with_quote(Some(11.5), Some("QUDT:W-PER-M-K"), "research", None);
        assert_eq!(
            validate_and_stamp(bare, BLOCK).unwrap_err(),
            ClaimRejection::MissingQuote
        );
    }

    #[test]
    fn containment_tolerates_whitespace_and_case_only() {
        let mut c = claim(Some(11.5), Some("QUDT:W-PER-M-K"), "research");
        c.provenance.quote = Some("Thermal  Conductivity\nis 11.5 W/(m K)".to_string());
        assert!(validate_and_stamp(c, BLOCK).is_ok());
    }

    #[test]
    fn supporting_quote_finds_the_sentence_with_the_value() {
        let found = supporting_quote(
            "CoCrFeNi",
            "thermal_conductivity",
            Some(11.5),
            BLOCK,
            &GuardPolicy::SILENT,
        );
        assert_eq!(
            found.as_deref(),
            Some("Its thermal conductivity is 11.5 W/(m K) at room temperature.")
        );
    }

    #[test]
    fn supporting_quote_does_not_split_decimal_numbers() {
        let block = "See results. Conductivity of CoCrFeNi was 11.5 W/(m K). Done.";
        let found = supporting_quote(
            "CoCrFeNi",
            "conductivity",
            Some(11.5),
            block,
            &GuardPolicy::SILENT,
        );
        assert!(found.unwrap().contains("11.5"));
    }

    #[test]
    fn supporting_quote_needs_a_salient_token_beside_the_number() {
        // The number alone could be a citation number; it must appear with
        // the subject or the object to count as support.
        let block = "Discussion of prior work [1140] follows. No alloy data here.";
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                block,
                &GuardPolicy::SILENT
            )
            .is_none()
        );
    }

    #[test]
    fn supporting_quote_matches_comma_grouped_numbers() {
        let block = "The Ti-6Al-4V billet showed a UTS of 1,140 MPa.";
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                block,
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    #[test]
    fn numeric_tolerant_supporting_quote_matches_complete_numeric_lexemes() {
        // CONTRACT CHANGE: the matcher now returns structured refusals, so
        // success/failure is asserted as Result instead of lossy Option.
        let exact_rendering = "The Alloy A modulus was 1.20 GPa.";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1.2,
                exact_rendering,
                0.0,
                &GuardPolicy::SILENT,
            )
            .as_deref(),
            Ok(exact_rendering)
        );

        let decimal_comma = "The Alloy A modulus was 1,2 GPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1.2,
                decimal_comma,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_ok()
        );

        let ambiguous_comma = "The Alloy A UTS was 1,140 MPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "UTS",
                1140.0,
                ambiguous_comma,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_err()
        );
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "UTS",
                1.14,
                ambiguous_comma,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_err()
        );

        let unambiguous_grouping = "The Alloy A UTS was 1,140,000 MPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "UTS",
                1140000.0,
                unambiguous_grouping,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_ok()
        );

        let signed_exponent = "The Alloy A conductivity was -1.20E-3 S/m.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "conductivity",
                -1.20e-3,
                signed_exponent,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_ok()
        );

        let positive_negative_exponent = "The Alloy A conductivity was +1.20E-3 S/m.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "conductivity",
                1.20e-3,
                positive_negative_exponent,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_ok()
        );
    }

    #[test]
    fn numeric_tolerant_supporting_quote_uses_the_supplied_relative_tolerance() {
        // CONTRACT CHANGE: failed tolerance checks remain distinguishable
        // from successful evidence through the lossless Result contract.
        let block = "The Alloy A modulus was 1.2004 GPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1.2,
                block,
                0.001,
                &GuardPolicy::SILENT,
            )
            .is_ok()
        );
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1.2,
                block,
                0.0001,
                &GuardPolicy::SILENT,
            )
            .is_err()
        );

        let opposite_sign = "The Alloy A residual stress was -1.0 MPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "residual_stress",
                1.0,
                opposite_sign,
                2.0,
                &GuardPolicy::SILENT,
            )
            .is_err(),
            "tolerance must never reverse numeric polarity"
        );
    }

    #[test]
    fn numeric_tolerant_supporting_quote_skips_rejected_lexemes_whole() {
        // CONTRACT CHANGE: rejected lexemes now produce a structured error;
        // this test no longer treats every rejection as an absent Option.
        let mixed_locale = "The Alloy A modulus was 1.140,5 GPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1140.5,
                mixed_locale,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_err(),
            "a suffix of a rejected complete token is not independent evidence"
        );

        for malformed_exponent in [
            "The Alloy A rate was 1e--3 per second.",
            "The Alloy A rate was 1e+-3 per second.",
        ] {
            assert!(
                supporting_quote_with_numeric_tolerance(
                    "Alloy A",
                    "rate",
                    1e-3,
                    malformed_exponent,
                    0.0,
                    &GuardPolicy::SILENT,
                )
                .is_err(),
                "a malformed exponent suffix became evidence: {malformed_exponent}"
            );
        }
    }

    #[test]
    fn numeric_tolerant_supporting_quote_keeps_every_refusal_guard() {
        // CONTRACT CHANGE: this is now the primary public API contract, not a
        // secondary diagnostic helper behind a lossy Option wrapper.
        // CONTRACT CHANGE (de-hardcoding): the Label guard no longer exists —
        // label vocabulary was domain knowledge and moved out of Rust. The
        // guards pinned here are the domain-independent ones plus the
        // ontology-served SignDomain.
        let range = "The Alloy A modulus ranged from 1.20\u{2013}1.40 GPa.";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1.2,
                range,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Range,
                span: range.to_string(),
            })
        );

        let citation = "The Alloy A modulus follows prior work [1.20].";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                1.2,
                citation,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: citation.to_string(),
            })
        );

        let later_citation = "The Alloy A modulus follows prior work [1.20, 2.30].";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "modulus",
                2.3,
                later_citation,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: later_citation.to_string(),
            })
        );

        let inside_name = "Inconel 718 was studied for UTS.";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Inconel 718",
                "UTS",
                718.0,
                inside_name,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::InsideName,
                span: inside_name.to_string(),
            })
        );

        let formatted_inside_name = "Inconel 718.0 UTS results were reported in MPa.";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Inconel 718",
                "UTS",
                718.0,
                formatted_inside_name,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::InsideName,
                span: formatted_inside_name.to_string(),
            })
        );

        let boundary = "The Alloy A UTS marker was x950.0x.";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "UTS",
                950.0,
                boundary,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Boundary,
                span: boundary.to_string(),
            })
        );

        // SignDomain now reads the ONTOLOGY's declaration, not a compiled
        // quantity list: silent policy stamps the negative, NonNegative
        // refuses it.
        let sign_domain = "The Alloy A UTS was -950.0 MPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "UTS",
                -950.0,
                sign_domain,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_ok(),
            "a silent ontology leaves the sign check inert"
        );
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "UTS",
                -950.0,
                sign_domain,
                0.0,
                &nonnegative_policy(),
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::SignDomain,
                span: sign_domain.to_string(),
            })
        );

        let separator = "Alloy A residual stress \u{2212}950.0 MPa (longitudinal).";
        assert_eq!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "residual_stress",
                -950.0,
                separator,
                0.0,
                &GuardPolicy::SILENT,
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::SeparatorDash,
                span: separator.to_string(),
            })
        );

        let true_negative = "The residual stress in Alloy A was \u{2212}350.0 MPa.";
        assert!(
            supporting_quote_with_numeric_tolerance(
                "Alloy A",
                "residual_stress",
                -350.0,
                true_negative,
                0.0,
                &GuardPolicy::SILENT,
            )
            .is_ok()
        );
    }

    #[test]
    fn non_numeric_fact_needs_subject_and_object_in_one_span() {
        let block = "The Ti-6Al-4V microstructure contained an alpha-beta phase.";
        assert!(
            supporting_quote("Ti-6Al-4V", "alpha-beta", None, block, &GuardPolicy::SILENT)
                .is_some()
        );
        // Object absent from the block: no support.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "omega phase",
                None,
                block,
                &GuardPolicy::SILENT
            )
            .is_none()
        );
    }

    // ------------------------------------------------------------------
    // Fabrication-path regression tests. Each one reproduces a confirmed
    // false-stamp path and asserts the claim is DROPPED: no supporting
    // quote found, and therefore refused at validate_and_stamp exactly as
    // papers.rs runs it (supporting_quote -> quote None -> MissingQuote).
    // ------------------------------------------------------------------

    /// Mirrors the production flow in crates/cli/src/papers.rs: the quote
    /// is whatever `supporting_quote` finds in the block, and a claim with
    /// no quote must be refused by `validate_and_stamp`.
    fn assert_dropped_end_to_end_with_policy(
        subject: &str,
        object: &str,
        value: f64,
        block: &str,
        policy: &GuardPolicy,
    ) {
        let quote = supporting_quote(subject, object, Some(value), block, policy);
        assert!(
            quote.is_none(),
            "fabricated support found for {subject}/{object}={value}: {quote:?}"
        );
        let claim = ExtractedClaim {
            subject: subject.to_string(),
            predicate: "has_measurement".to_string(),
            object: object.to_string(),
            value: Some(value),
            unit: Some("QUDT:MegaPA".to_string()),
            conditions: vec![],
            confidence: Some(0.9),
            kind: Some("measurement".to_string()),
            evidence_class: "research".to_string(),
            verification: None,
            verification_reason: None,
            ontology: Default::default(),
            provenance: ClaimProvenance {
                document_id: "10.1234/doc".to_string(),
                document_url: "https://doi.org/10.1234/doc".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote,
                source_revision_id: None,
                line_start: None,
                line_end: None,
                source_text_path: None,
            },
        };
        assert_eq!(
            validate_and_stamp(claim, block).unwrap_err(),
            ClaimRejection::MissingQuote
        );
    }

    fn assert_dropped_end_to_end(subject: &str, object: &str, value: f64, block: &str) {
        assert_dropped_end_to_end_with_policy(subject, object, value, block, &GuardPolicy::SILENT);
    }

    /// Fabrication path 2: substring number matching. The matched number
    /// must stand on its own token boundary, or an order-of-magnitude-wrong
    /// number passes the gate.
    #[test]
    fn substring_number_match_is_dropped() {
        let uts_block = "The Ti-6Al-4V sample showed a UTS of 950 MPa.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 95.0, uts_block);

        let conductivity_block = "The thermal conductivity of CoCrFeNi is 11.5 W/(m K).";
        assert_dropped_end_to_end("CoCrFeNi", "thermal_conductivity", 1.5, conductivity_block);

        // The genuine values in the same blocks still stamp.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                uts_block,
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "thermal_conductivity",
                Some(11.5),
                conductivity_block,
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// The two decimal-point guards in `clean_number_boundary` each do
    /// work no other guard covers: the fractional digit of "11.5" matches
    /// value 5 (only the BEFORE-'.' guard refuses it), and the integer
    /// part matches value 11 (only the AFTER-'.' guard refuses it). The
    /// pre-existing decimal case (1.5 vs 11.5) is caught by the
    /// alphanumeric guards instead, so without these two asserts both
    /// guards could be deleted with the whole suite green. Each guard is
    /// mutation-proven red independently.
    #[test]
    fn decimal_point_guards_refuse_partial_number_matches() {
        let block = "The CoCrFeNi conductivity is 11.5 W/(m K).";
        // The fractional digit: only the before-'.' guard refuses it.
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "conductivity",
                Some(5.0),
                block,
                &GuardPolicy::SILENT
            )
            .is_none()
        );
        // The integer part: only the after-'.' guard refuses it.
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "conductivity",
                Some(11.0),
                block,
                &GuardPolicy::SILENT
            )
            .is_none()
        );
        // Positive control: 11.5 itself still stamps.
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "conductivity",
                Some(11.5),
                block,
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// The sign is the finding: for residual stress, -950 vs +950 is the
    /// difference between compressive and tensile. Both halves of the
    /// round-4 repro are pinned. (a) The true negative claim stamps under
    /// U+2212 MINUS SIGN prose — killed by removing either U+2212 producer
    /// in `number_needles` (the decimal push owns the decimal assert; the
    /// signs loop owns the integer asserts). (b) The sign-flipped positive
    /// claim is dropped — killed by the before-minus guard in
    /// `clean_number_boundary`. The last assert pins the symmetry: a
    /// negative claim never stamps against positive prose either.
    /// CONTRACT CHANGE (de-hardcoding): all of this runs under the SILENT
    /// policy — the structural sign rules never needed a quantity list.
    #[test]
    fn negative_value_claims_match_negative_prose_and_refuse_the_flip() {
        let unicode_minus = "The residual stress in Ti-6Al-4V was \u{2212}950 MPa.";
        let ascii_minus = "The residual stress in Ti-6Al-4V was -950 MPa.";

        // The true negative claim stamps under both minus glyphs.
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-950.0),
                unicode_minus,
                &GuardPolicy::SILENT
            )
            .as_deref(),
            Some(unicode_minus)
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-950.0),
                ascii_minus,
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // Grouped negative integer under U+2212 (the signs loop).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-1140.0),
                "The residual stress in Ti-6Al-4V was \u{2212}1,140 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // Negative decimal under U+2212 (the decimal push).
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "seebeck_coefficient",
                Some(-11.5),
                "The CoCrFeNi Seebeck coefficient was \u{2212}11.5 uV/K.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // Cryogenic temperature with a degree unit — the round-6
        // positive-control list names it explicitly.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(-196.0),
                "The Ti-6Al-4V samples were tested at \u{2212}196 \u{b0}C.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        // The sign-flipped claim is dropped, not stamped: the prose says
        // compressive, +950 says tensile. Both glyphs.
        assert_dropped_end_to_end("Ti-6Al-4V", "residual_stress", 950.0, unicode_minus);
        assert_dropped_end_to_end("Ti-6Al-4V", "residual_stress", 950.0, ascii_minus);

        // Symmetry: the negative claim against positive prose is dropped.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "residual_stress",
            -950.0,
            "The residual stress in Ti-6Al-4V was 950 MPa.",
        );

        // The signed-needle half of an ASCII range still drops: the
        // "-1100" needle starts on the hyphen, and the digit before it is
        // alphanumeric.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            -1100.0,
            "The Ti-6Al-4V UTS ranged from 950-1100 MPa.",
        );
    }

    /// Round 5: after a REJECTED U+2212-prefixed needle, the scan must
    /// advance by the needle's first character, not one byte. "950x" is
    /// rejected because no supplied unit term redeems the glued 'x'
    /// (CONTRACT CHANGE: the unit lexicon is gone, so an unclaimed glued
    /// letter refuses under the SILENT policy); the old `start + 1`
    /// advance then landed `hay[search_from..]` inside the 3-byte U+2212
    /// and panicked the whole ingest run on a non-char boundary. The
    /// correct outcome is a drop: the zoom factor is not evidence for a
    /// stress of -950.
    #[test]
    fn rejected_unicode_minus_needle_advances_by_char_not_byte() {
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "stress",
                Some(-950.0),
                "Ti-6Al-4V at \u{2212}950x zoom had stress.",
                &GuardPolicy::SILENT
            ),
            None
        );
    }

    /// Round 5: same advance bug via the range-endpoint guard — a U+2212
    /// value that opens an en-dash range ("\u{2212}950\u{2013}1100 MPa")
    /// is rejected as a range endpoint, and the reject-then-advance must
    /// clear the 3-byte minus without slicing inside it.
    #[test]
    fn rejected_unicode_minus_range_endpoint_advances_by_char_not_byte() {
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "stress",
                Some(-950.0),
                "Ti-6Al-4V stress \u{2212}950\u{2013}1100 MPa.",
                &GuardPolicy::SILENT
            ),
            None
        );
    }

    /// Round 5 audit, same bug class: `occurrence_inside_name` also
    /// advanced one byte after a non-containing name match. A subject name
    /// that leads with a multi-byte char ("\u{3b1}-phase", U+03B1 GREEK
    /// SMALL LETTER ALPHA is 2 bytes) made the next slice panic. The
    /// occurrence is outside the name, so the value stamps.
    #[test]
    fn multibyte_leading_subject_name_does_not_panic_the_name_scan() {
        assert!(
            supporting_quote(
                "\u{3b1}-phase",
                "strength",
                Some(950.0),
                "\u{3b1}-phase strength was 950 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// Round 13 item 4: the same char-not-byte advance, second bite of
    /// this bug class — a 1-BYTE name glyph matching a MULTI-BYTE hay
    /// glyph: subject "-" folding onto a U+2010 HYPHEN (3 bytes) in the
    /// hay. subject/object are model-supplied, so this is reachable from
    /// untrusted LLM output — a panic aborts the ingest run, not one
    /// claim.
    #[test]
    fn single_byte_dash_subject_matching_multibyte_hay_glyph_does_not_panic() {
        let r = supporting_quote_or_refusal(
            "-",
            "UTS",
            Some(950.0),
            "ti\u{2010}6al had 950 MPa",
            &GuardPolicy::SILENT,
        );
        assert!(r.is_ok(), "must not panic and must find the support: {r:?}");
    }

    /// CONTRACT CHANGE (de-hardcoding) — the replacement for the old
    /// `sign_domain_matches_head_noun_suffix_without_over_refusal`
    /// vocabulary test, which pinned a compiled English/materials table
    /// (the hardcoding under review). The REAL property survives,
    /// re-expressed: when the ONTOLOGY declares a quantity non-negative,
    /// a negative claim against it refuses whatever the quantity is
    /// NAMED — in any language, any spelling, because the name is never
    /// consulted. When the ontology is silent, nothing refuses: silence
    /// is inert, never a guess.
    #[test]
    fn sign_domain_reads_the_ontology_not_the_quantity_name() {
        // Forward direction: under an ontology NonNegative declaration a
        // negative refuses for EVERY spelling — including every spelling
        // the old compiled list missed and every spelling in another
        // language. The matcher cannot see the name at all.
        for spelling in [
            "tensile strength",
            "ultimate tensile strength",
            "relative density",
            "average grain size",
            "Zugfestigkeit",
            "duret\u{e9}",
            "Dichte",
            "Rm",
        ] {
            let prose = format!("The sample {spelling} was -950 MPa.");
            let r = supporting_quote_or_refusal(
                "sample",
                spelling,
                Some(-950.0),
                &prose,
                &nonnegative_policy(),
            );
            assert!(
                matches!(
                    r,
                    Err(SupportRefusal::Guarded {
                        guard: RefusalGuard::SignDomain,
                        ..
                    })
                ),
                "an ontology-declared non-negative quantity must refuse a negative \
                 whatever its name: {spelling:?}: {r:?}"
            );
        }
        // Silence direction: the SAME claims under a silent ontology all
        // stamp. The old list's five canonical spellings included — no
        // compiled residue of it survives anywhere in this crate.
        for spelling in [
            "uts",
            "hardness",
            "density",
            "grain size",
            "yield strength",
            "residual stress",
            "dissociation constant",
        ] {
            let prose = format!("The sample {spelling} was -950 MPa.");
            let r = supporting_quote_or_refusal(
                "sample",
                spelling,
                Some(-950.0),
                &prose,
                &GuardPolicy::SILENT,
            );
            assert!(
                r.is_ok(),
                "a silent ontology must leave the sign check inert for {spelling:?}: {r:?}"
            );
        }
        // An explicit Signed declaration stamps too — the guard only ever
        // fires on NonNegative.
        let signed = GuardPolicy {
            quantity_sign: QuantitySignDomain::Signed,
            unit_term: None,
        };
        let prose = "The sample residual stress was -950 MPa.";
        assert!(
            supporting_quote_or_refusal("sample", "residual stress", Some(-950.0), prose, &signed)
                .is_ok()
        );
    }

    /// Round 13 item 6.2: SignDomain is checked per-occurrence (inside the
    /// loop), not hoisted claim-level, so a non-occurring negative against
    /// an ontology-declared non-negative quantity stays in the "the block
    /// never contained the value" class rather than `Guarded{SignDomain}`
    /// (the matcher's fault). Hoisting the check before the loop reddens
    /// this.
    ///
    /// B9 CONTRACT CHANGE: that class used to be `NoSpan`; it is now the
    /// finer `ValueNotRendered` — no needle form of the value occurred
    /// anywhere in the block, which is exactly what this fixture builds.
    /// The property under test (SignDomain does not mask the real cause)
    /// is unchanged.
    #[test]
    fn sign_domain_does_not_mask_a_non_occurring_value_as_no_span() {
        // The block states +950; the claim -950 never occurs in any needle
        // form. ValueNotRendered (the block never contained the value),
        // not SignDomain (matcher's fault).
        let r = supporting_quote_or_refusal(
            "Ti-6Al-4V",
            "UTS",
            Some(-950.0),
            "The Ti-6Al-4V UTS was 950 MPa.",
            &nonnegative_policy(),
        );
        assert!(
            matches!(r, Err(SupportRefusal::ValueNotRendered)),
            "non-occurring negative must be ValueNotRendered, not SignDomain: {r:?}"
        );
    }

    /// F-1: a thousands comma adjacent to a digit is part of the number,
    /// in both directions. "1,140" must behave byte-for-byte like "1140":
    /// searching for 140 or 1 inside it finds nothing, exactly the
    /// guarantee `substring_number_match_is_dropped` asserts for 95/950.
    #[test]
    fn comma_grouped_number_digits_are_not_token_boundaries() {
        let block = "The Ti-6Al-4V UTS is 1,140 MPa.";
        assert!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(140.0), block, &GuardPolicy::SILENT)
                .is_none()
        );
        assert!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(1.0), block, &GuardPolicy::SILENT).is_none()
        );
        // Control: the same fact written without grouping already drops 140.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(140.0),
                "The Ti-6Al-4V UTS is 1140 MPa.",
                &GuardPolicy::SILENT
            )
            .is_none()
        );
        // The real value still stamps in the grouped form.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                block,
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        assert!(
            supporting_quote(
                "A",
                "UTS",
                Some(12.0),
                "A UTS is 12,345 MPa.",
                &GuardPolicy::SILENT
            )
            .is_none()
        );
        assert!(
            supporting_quote(
                "A",
                "UTS",
                Some(345.0),
                "A UTS is 12,345 MPa.",
                &GuardPolicy::SILENT
            )
            .is_none()
        );
        assert!(
            supporting_quote(
                "A",
                "UTS",
                Some(12345.0),
                "A UTS is 12,345 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        // Row form: the leading "1" is not a standalone value either.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1.0),
                "Alloy UTS\nTi-6Al-4V 1,140\n...",
                &GuardPolicy::SILENT
            )
            .is_none()
        );

        // A list comma is NOT number continuation: a value followed by a
        // comma and a space must still stamp (pins the digit-adjacency
        // condition against a blanket comma reject).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(12.0),
                "The Ti-6Al-4V samples measured 12, 15 and 950 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// CONTRACT CHANGE (de-hardcoding) — the replacement for the old
    /// glued-unit-initial tests, which pinned `UNIT_TOKENS` and its
    /// derived initials (the hardcoding under review). The REAL property
    /// survives, re-expressed: a letter GLUED to a number redeems only
    /// when it begins the fact's OWN unit term — the term the
    /// reader/ontology chose — and never otherwise. Rust holds no unit
    /// lexicon; a silent fact refuses every glued letter.
    #[test]
    fn glued_letters_redeem_only_through_the_facts_own_unit() {
        // Mechanism pins, one per direction.
        assert!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS is 950MPa.",
                &unit_policy("MPa")
            )
            .is_ok(),
            "the fact's own unit term redeems its glued form"
        );
        // The term is case-folded exactly like the text it is matched
        // against: the reader's "MPa" redeems the normalized "mpa".
        assert!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS is 950MPa.",
                &unit_policy("mpa")
            )
            .is_ok()
        );
        // Silence refuses the same sentence: no lexicon guesses "MPa".
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 950.0, "The Ti-6Al-4V UTS is 950MPa.");
        // A DIFFERENT supplied unit does not redeem: the claim said kPa,
        // the page says MPa — that disagreement belongs to the reader and
        // the ontology, not to a Rust equivalence table.
        assert_dropped_end_to_end_with_policy(
            "Ti-6Al-4V",
            "UTS",
            950.0,
            "The Ti-6Al-4V UTS is 950MPa.",
            &unit_policy("kPa"),
        );
        // Glued NON-unit letters stay dropped with or without a unit
        // ("950x" is magnification, "2e5" is scientific notation).
        assert_dropped_end_to_end_with_policy(
            "Ti-6Al-4V",
            "UTS",
            950.0,
            "The Ti-6Al-4V image at 950x magnification.",
            &unit_policy("MPa"),
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "strain_rate",
            2.0,
            "The Ti-6Al-4V strain rate was 2e5 per second.",
        );
        // Digit glue is still the substring reject: 95 inside 950MPa.
        assert_dropped_end_to_end_with_policy(
            "Ti-6Al-4V",
            "UTS",
            95.0,
            "The Ti-6Al-4V UTS is 950MPa.",
            &unit_policy("MPa"),
        );
        // A supplied unit never redeems a digit inside a hyphen-joined
        // designation, ASCII or en dash (the "6" of "Ti-6Al-4V").
        assert_dropped_end_to_end_with_policy(
            "Ti-6Al-4V",
            "UTS",
            6.0,
            "The Ti-6Al-4V billets were 950MPa rated.",
            &unit_policy("MPa"),
        );
        assert_dropped_end_to_end_with_policy(
            "Ti-6Al-4V",
            "UTS",
            6.0,
            "The Ti\u{2013}6Al\u{2013}4V billets were 950MPa rated.",
            &unit_policy("MPa"),
        );
        // Degree/percent glue was never broken (non-alphanumeric) and
        // must keep stamping under silence.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(500.0),
                "Ti-6Al-4V was held at 500 \u{b0}C.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // En-dash positive control: the dash class folds in NAME matching
        // (find_name), so the en-dash typesetting of the designation
        // matches the ASCII subject through the SUBJECT arm itself.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti\u{2013}6Al\u{2013}4V UTS is 950 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// Range endpoints are not point values: "950\u{2013}1100 MPa"
    /// asserts a range, and stamping UTS = 950 AND UTS = 1100 fabricates
    /// two facts the sentence never states. Round 10: the dash rule
    /// covers the WHOLE dash class — the round-4 ASCII exception
    /// stamped "950-1100" batch identifiers as measurements, fabricated
    /// property records with perfect provenance. The rule refuses the
    /// adjacent OCCURRENCE, not the number: an endpoint that recurs
    /// elsewhere as a genuine point value still stamps. Mutation-proven
    /// per side: swapping the after-side dash for a non-dash char
    /// reddens the 950 assert; swapping the before-side dash reddens
    /// the 1100 assert.
    #[test]
    fn en_dash_range_endpoints_are_not_point_values() {
        let range = "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 950.0, range);
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1100.0, range);

        // Grouped endpoints are refused too.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "The Ti-6Al-4V UTS ranged from 1,140\u{2013}1,375 MPa.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1375.0,
            "The Ti-6Al-4V UTS ranged from 1,140\u{2013}1,375 MPa.",
        );

        // A different point value in the same sentence still stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1150.0),
                "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa and reached 1150 MPa \
                 after annealing.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        // An endpoint that recurs outside the range as a genuine point
        // value still stamps: the rule refuses the range-adjacent
        // occurrence only.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa and the annealed \
                 sample reached 950 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        // Round 10: the ASCII form is a range too. "950-1100" batch
        // identifiers stamped as property records until the dash rule
        // grew the whole dash class; both endpoints now drop, pinned
        // here and in the sign test.
        let batches = "The Ti-6Al-4V batches 950-1100 were tested.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 950.0, batches);
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1100.0, batches);

        // The guard names the class: a U+2010-joined run is Range, not
        // Boundary.
        let hyphen_range = "The Ti-6Al-4V UTS ranged from 950\u{2010}1100 MPa.";
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(1100.0),
                hyphen_range,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Range,
                span: hyphen_range.to_string(),
            })
        );
    }

    /// Round 7, restored round 11: U+2013 is NOT a sign glyph. Some
    /// PDFs typeset negatives as "\u{2013}350 MPa"; the unsigned needle
    /// matched the digits and stamped the sign-flipped twin —
    /// compressive recorded as tensile. The fix refuses the unsigned
    /// needle when U+2013 precedes it with no digit before the dash.
    /// Round 10 read the dash as a minus and stamped the signed needle,
    /// until round 11 measured the SEPARATOR shape: "UTS \u{2013}950
    /// MPa" has no digit before the dash either and stamped a
    /// compressive value from a tensile source. The two readings are
    /// locally indistinguishable, so BOTH halves now drop under
    /// U+2013/U+2014 — a fabrication is worse than a miss (see
    /// `number_needles`). Mutation: removing U+2013 from the
    /// before-dash match in `clean_number_boundary` reddens the +350
    /// drop assert.
    #[test]
    fn en_dash_is_not_a_sign_glyph() {
        let minus = "The residual stress in Ti-6Al-4V was \u{2013}350 MPa.";
        // The fabrication half: the sign-flipped positive claim drops.
        assert_dropped_end_to_end("Ti-6Al-4V", "residual_stress", 350.0, minus);
        // MECHANISM, not ground truth (round 12 item 3). U+2013 is no
        // sign glyph, so the true negative constructs no signed needle,
        // the scan finds no needle form at all, and the drop surfaces as
        // ValueNotRendered (B9: the value never rendered anywhere — the
        // old conflated name for this class was NoSpan). GROUND TRUTH for
        // this exact tuple — the prose DOES assert -350 MPa, an engineer
        // calls that supported, so a stamp is what SHOULD happen — belongs
        // to the corpus KNOWN recall row (tests/claim_corpus.rs, the
        // U+2013 \u{2013}350 entry); it is not restated here.
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-350.0),
                minus,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::ValueNotRendered)
        );
        // Grouped form: the unsigned grouped needle is still refused...
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "residual_stress",
            1140.0,
            "The residual stress in Ti-6Al-4V was \u{2013}1,140 MPa.",
        );
        // ...and its signed twin drops the same way (MECHANISM, not
        // ground truth; the corpus carries the KNOWN grouped-recall row).
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-1140.0),
                "The residual stress in Ti-6Al-4V was \u{2013}1,140 MPa.",
                &GuardPolicy::SILENT
            ),
            // B9: same class as the ungrouped twin above — no signed
            // needle form of -1140 renders anywhere (U+2013 is not a sign
            // glyph), so the drop is ValueNotRendered, not NoSpan.
            Err(SupportRefusal::ValueNotRendered)
        );

        // Stamp direction: a genuine point value in the same sentence
        // still stamps — the refusal is dash-adjacent, not sentence-wide.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "stress",
                Some(400.0),
                "The residual stress in Ti-6Al-4V was \u{2013}350 MPa as built and \
                 400 MPa after annealing.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        // The range rule keeps its name: the high endpoint of
        // "950\u{2013}1100" has a digit before the dash, so it is
        // Range (checked first), not the sign refusal.
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(1100.0),
                "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.",
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Range,
                span: "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.".to_string(),
            })
        );
    }

    /// Fabrication path 3: a citation marker is not evidence. The exact
    /// sentence the extractor prompt uses as its example must never stamp
    /// the example's number.
    #[test]
    fn citation_marker_is_not_support() {
        let block = "Ti-6Al-4V has been studied extensively in prior work [1140].";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1140.0, block);

        // Round 10: a dash-joined citation list walks back through
        // every glyph of the dash class, not just '-', U+2013 and
        // U+2014. The shape uses a comma-joined tail (14) that is not
        // dash-adjacent: dash-adjacent numbers inside brackets are
        // refused by the Range guard first, so only this shape names
        // the Citation guard the walk produces.
        for dash in [
            '\u{2010}', '\u{2011}', '\u{2012}', '\u{2015}', '\u{2212}', '\u{fe63}',
        ] {
            let list = format!("Ti-6Al-4V has been studied extensively [11{dash}12, 14].");
            assert_eq!(
                supporting_quote_or_refusal(
                    "Ti-6Al-4V",
                    "UTS",
                    Some(14.0),
                    &list,
                    &GuardPolicy::SILENT
                ),
                Err(SupportRefusal::Guarded {
                    guard: RefusalGuard::Citation,
                    span: list.clone(),
                }),
                "dash U+{:04X} leaked the citation walk",
                dash as u32
            );
        }
    }

    /// CONTRACT CHANGE (de-hardcoding) — the replacement for the old
    /// label-word tests (`table_figure_ref_label_numbers_are_not_support`,
    /// `section_and_kindred_label_numbers_are_not_support`, the label-list
    /// walks, `sample_and_run_label_numbers_are_not_support`,
    /// `every_kept_label_word_refuses_its_number`), each of which pinned
    /// an English vocabulary — the hardcoding under review. Which words
    /// introduce citation labels is domain AND language knowledge: a
    /// German paper writes "Tabelle 1", "Abb. 3", "Probe 5". No honest
    /// structural rule replaces the list, so the check is DELETED and the
    /// judgement moves to the ontology and the re-checking model: the
    /// matcher honestly reports support, and the fact carries its
    /// verification status. Bracketed citation markers remain refused —
    /// that guard is punctuation, not vocabulary.
    #[test]
    fn label_words_have_no_special_standing_in_the_matcher() {
        for (block, value) in [
            ("Ti-6Al-4V properties are listed in Table 3.", 3.0),
            ("Ti-6Al-4V data appear in Figure 2.", 2.0),
            ("UTS data for Ti-6Al-4V appears in Ref 25.", 25.0),
            ("The Ti-6Al-4V results are in Section 4.", 4.0),
            ("Sample 5 of Ti-6Al-4V was tested.", 5.0),
            ("Run 12 of the Inconel 718 build failed.", 12.0),
            ("Ti-6Al-4V data are listed in Tables 1 and 2.", 2.0),
            ("Ti-6Al-4V is discussed in Refs. 25, 26 for UTS data.", 26.0),
        ] {
            // The subject is "Inconel 718" where the prose names it, else
            // Ti-6Al-4V; both ride the subject-or-object arm.
            let subject = if block.contains("Inconel 718") {
                "Inconel 718"
            } else {
                "Ti-6Al-4V"
            };
            let object = if block.contains("UTS") {
                "UTS"
            } else {
                "property"
            };
            let r = supporting_quote_or_refusal(
                subject,
                object,
                Some(value),
                block,
                &GuardPolicy::SILENT,
            );
            assert!(
                r.is_ok(),
                "the matcher no longer encodes label vocabulary; {block:?} now reports \
                 support and the fact carries its verification status: {r:?}"
            );
        }
        // The structural guard that SURVIVES: bracketed citation markers
        // are punctuation, not vocabulary, and stay refused.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied (1140).",
        );
    }

    /// F-4: the same protection for SPACE-separated designations, where
    /// the boundary rule cannot help: the trailing digits of the claim's
    /// own subject/object are part of the name, not a measurement.
    /// Mutation-proven red when the inside-designation check is removed.
    /// An occurrence that is genuinely repeated OUTSIDE the name still
    /// stamps (last assert), so this is not a blanket digit-phobia.
    #[test]
    fn spaced_designation_digits_of_the_claims_own_name_are_not_support() {
        let table = "Alloy UTS (MPa)\nTi-6Al-4V 950\nInconel 718 1375";
        assert_dropped_end_to_end("Inconel 718", "UTS", 718.0, table);
        // The digits recur as a real measurement beside the name: stamp.
        assert!(
            supporting_quote(
                "Inconel 718",
                "UTS",
                Some(718.0),
                "Inconel 718 showed a UTS of 718 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // Positive control: the genuine row still stamps verbatim.
        assert_eq!(
            supporting_quote(
                "Inconel 718",
                "UTS",
                Some(1375.0),
                table,
                &GuardPolicy::SILENT
            )
            .as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// H6, prose side: parenthesized and braced citation numbers are
    /// markers, not measurements. The closing-side check is what keeps
    /// the legitimate parenthesized value "UTS (950 MPa)" stamping: a
    /// bracket-class rule that only looks backwards would refuse it, so
    /// '(' / '{' count as citation openers only when the marker closes
    /// before any non-number text.
    #[test]
    fn paren_and_brace_citation_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied (1140).",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied {1140}.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            1140.0,
            "Ti-6Al-4V has been widely studied (1140, 1141).",
        );
        // Positive control: a parenthesized value WITH its unit is not a
        // citation and must stamp.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS (950 MPa) was reproducible.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// H6 end to end through parse_jats: superscript bibr xrefs parse to
    /// a bare number, and `prefix.ends_with('[')` never sees them. The
    /// parser wraps bibr xref text in [...], so the bracketed-citation
    /// guard refuses it: the extractor prompt's own example fact
    /// (Ti-6Al-4V / UTS / 1140) must never stamp against this sentence.
    /// A real value in the same sentence still stamps (the required
    /// "real value after a citation" positive control).
    #[test]
    fn jats_bibr_xref_number_is_not_support() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Citations</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Ti-6Al-4V has been widely studied<sup><xref ref-type="bibr" rid="b1">1140</xref></sup>; its UTS is 950 MPa.</p>
    </sec>
  </body>
</article>"#;
        let ft = crate::fulltext::parse_jats(body.as_bytes()).unwrap();
        for block in &ft.blocks {
            assert!(
                supporting_quote(
                    "Ti-6Al-4V",
                    "UTS",
                    Some(1140.0),
                    &block.text,
                    &GuardPolicy::SILENT
                )
                .is_none(),
                "block {:?} fabricated support from a superscript citation: {:?}",
                block.locator.kind,
                block.text
            );
        }
        let body_block = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Body)
            .unwrap();
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                &body_block.text,
                &GuardPolicy::SILENT
            )
            .is_some(),
            "the real value after the citation must still stamp: {:?}",
            body_block.text
        );
    }

    /// Fabrication path 4: digits inside alloy designations are not
    /// numeric prose. Alloy names are numeric by convention, so a block
    /// with no measurement must not stamp the digits of the name.
    #[test]
    fn alloy_designation_digits_are_not_support() {
        let block = "The Ti-6Al-4V samples were annealed and examined.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 6.0, block);
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 4.0, block);

        // Round 10: the designation guard's glyph set is the whole dash
        // class. U+2011 NON-BREAKING HYPHEN is the spelling a typesetter
        // uses to keep Ti-6Al-4V on one line; under a DIFFERENT subject
        // (so occurrence_inside_name cannot mask the boundary clause) the
        // 6 must still drop.
        assert_dropped_end_to_end(
            "Inconel 718",
            "hardness",
            6.0,
            "The Ti\u{2011}6Al\u{2011}4V and Inconel 718 alloys were compared.",
        );
    }

    /// Fabrication path 1: a table must not act as one giant span. Rows
    /// are separate spans, so a number in one row cannot support a claim
    /// whose subject lives in another row (the 1375 assert, mutation-proven
    /// by fusing all rows into one span).
    ///
    /// CONTRACT CHANGE (de-hardcoding): the caption digit "1" of "Table 1"
    /// USED to be refused by the label vocabulary even though the caption
    /// span holds the subject AND the object. That English list is gone,
    /// so under a silent ontology the matcher honestly reports the
    /// co-occurrence; the caption-vs-measurement judgement moves to the
    /// ontology and the re-checking model, and the fact carries its
    /// verification status.
    #[test]
    fn properties_table_rows_are_separate_spans() {
        let table = "Table 1 UTS of Ti-6Al-4V and Inconel 718\n\
                     Alloy UTS (MPa)\n\
                     Ti-6Al-4V 950\n\
                     Inconel 718 1375";

        // Inconel's number cannot support a claim about Ti-6Al-4V: the
        // subject and 1375 never share a row.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1375.0, table);
        // The caption digit now reports support under a silent ontology —
        // the vocabulary that refused it was the hardcoding under review.
        assert!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(1.0), table, &GuardPolicy::SILENT).is_some()
        );

        // Genuine rows still stamp: the alloy and its own number share a row.
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), table, &GuardPolicy::SILENT)
                .as_deref(),
            Some("Ti-6Al-4V 950")
        );
        assert_eq!(
            supporting_quote(
                "Inconel 718",
                "UTS",
                Some(1375.0),
                table,
                &GuardPolicy::SILENT
            )
            .as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// Fabrication path 1, end to end through the real JATS sink: from one
    /// properties table, `Ti-6Al-4V UTS = 1375` (Inconel's number) must
    /// find no support in ANY block of the document.
    ///
    /// CONTRACT CHANGE (de-hardcoding): the label-digit probe (the "1" of
    /// "Table 1") moved the other way — the caption sentence holds the
    /// subject, so under a silent ontology the body block now reports
    /// support for it; the vocabulary that refused it is gone.
    #[test]
    fn jats_properties_table_supports_no_cross_row_claim() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Properties</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Mechanical properties of Ti-6Al-4V and Inconel 718 are shown in Table 1.</p>
      <table-wrap>
        <label>Table 1</label>
        <caption><p>Mechanical properties.</p></caption>
        <table>
          <tr><th>Alloy</th><th>UTS (MPa)</th></tr>
          <tr><td>Ti-6Al-4V</td><td>950</td></tr>
          <tr><td>Inconel 718</td><td>1375</td></tr>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = crate::fulltext::parse_jats(body.as_bytes()).unwrap();
        for block in &ft.blocks {
            assert!(
                supporting_quote(
                    "Ti-6Al-4V",
                    "UTS",
                    Some(1375.0),
                    &block.text,
                    &GuardPolicy::SILENT
                )
                .is_none(),
                "block {:?} fabricated support for Inconel's number: {:?}",
                block.locator.kind,
                block.text
            );
        }
        // The genuine row still stamps in the table block.
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                &table.text,
                &GuardPolicy::SILENT
            )
            .as_deref(),
            Some("Ti-6Al-4V 950")
        );
        // The caption digit reports support in the body block under a
        // silent ontology — recorded, not judged here.
        let body_block = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Body)
            .unwrap();
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1.0),
                &body_block.text,
                &GuardPolicy::SILENT
            )
            .is_some(),
            "the caption sentence names the subject, so the matcher reports the \
             co-occurrence: {:?}",
            body_block.text
        );
    }

    /// Fabrication path 1, OASIS/CALS table model (H1): JATS permits two
    /// table models; a paper using <tgroup>/<row>/<entry> is no less
    /// protected than one using <tr>/<td>. From ONE OASIS table,
    /// `Ti-6Al-4V UTS = 1375` (Inconel's number) must find no support in
    /// any block; both genuine rows still stamp. Mutation-proven red when
    /// `row` is removed from the row-boundary match in parse_jats.
    #[test]
    fn jats_oasis_table_supports_no_cross_row_claim() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Properties</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Mechanical properties are shown in Table 1.</p>
      <table-wrap>
        <label>Table 1</label>
        <table>
          <tgroup cols="2">
            <tbody>
              <row><entry>Ti-6Al-4V</entry><entry>950</entry></row>
              <row><entry>Inconel 718</entry><entry>1375</entry></row>
            </tbody>
          </tgroup>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = crate::fulltext::parse_jats(body.as_bytes()).unwrap();
        for block in &ft.blocks {
            assert!(
                supporting_quote(
                    "Ti-6Al-4V",
                    "UTS",
                    Some(1375.0),
                    &block.text,
                    &GuardPolicy::SILENT
                )
                .is_none(),
                "block {:?} fabricated support for Inconel's number: {:?}",
                block.locator.kind,
                block.text
            );
        }
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                &table.text,
                &GuardPolicy::SILENT
            )
            .as_deref(),
            Some("Ti-6Al-4V 950")
        );
        assert_eq!(
            supporting_quote(
                "Inconel 718",
                "UTS",
                Some(1375.0),
                &table.text,
                &GuardPolicy::SILENT
            )
            .as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// The methods-prose recall family: spaced units after a number keep
    /// stamping. CONTRACT CHANGE (de-hardcoding): these used to be the
    /// EXEMPTION half of the label guard (a spaced unit redeemed a number
    /// after "sample"/"run"). With the label vocabulary gone there is
    /// nothing to exempt from — the numbers stamp on their own clean
    /// boundaries, and the test documents the recall the removal keeps.
    #[test]
    fn methods_prose_numbers_with_spaced_units_still_stamp() {
        for (subject, object, value, prose) in [
            (
                "Ti-6Al-4V",
                "thickness",
                3.0,
                "Each Ti-6Al-4V sample 3 mm thick was ground and polished.",
            ),
            (
                "Ti-6Al-4V",
                "thickness",
                3.0,
                "The Ti-6Al-4V samples 3 mm thick were ground and polished.",
            ),
            (
                "Ti-6Al-4V",
                "duration",
                2.0,
                "The Ti-6Al-4V run 2 h at 1073 K produced full densification.",
            ),
            (
                "Inconel 718",
                "duration",
                30.0,
                "Each Inconel 718 run 30 min at 980 \u{b0}C was quenched.",
            ),
            (
                "AlSi10Mg",
                "thickness",
                5.0,
                "The AlSi10Mg samples 5 mm thick were sectioned.",
            ),
        ] {
            assert!(
                supporting_quote_or_refusal(
                    subject,
                    object,
                    Some(value),
                    prose,
                    &GuardPolicy::SILENT
                )
                .is_ok(),
                "recall lost for: {prose}"
            );
        }
        // The real temperature in the same sentence still stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(1073.0),
                "The Ti-6Al-4V run 2 h at 1073 K produced full densification.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// Scientific notation and magnification glue stay rejected — and now
    /// for the honest reason: no supplied unit term redeems the glued
    /// letter, so the Boundary guard refuses it. CONTRACT CHANGE
    /// (de-hardcoding): this no longer depends on `DENIED_UNIT_INITIALS`
    /// or on "ev" being a unit token; with the fact's own unit supplied,
    /// the spaced and glued forms of that unit stamp while "2e5" still
    /// refuses (the claim's unit is not "e5").
    #[test]
    fn scientific_notation_and_magnification_glue_stay_rejected() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "strain_rate",
            2.0,
            "The Ti-6Al-4V strain rate was 2e5 per second.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "cycles_to_failure",
            1.0,
            "Ti-6Al-4V ran 1e6 cycles to failure.",
        );
        // A supplied unit does not reopen the glued-e form: the fact's
        // unit is "ev", and "e5" is not "ev".
        assert_dropped_end_to_end_with_policy(
            "Ti-6Al-4V",
            "strain_rate",
            2.0,
            "The Ti-6Al-4V strain rate was 2e5 per second.",
            &unit_policy("ev"),
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            950.0,
            "The Ti-6Al-4V coupon was imaged at 950x magnification.",
        );
        // Spaced "5 ev" stamps under silence: after the number comes a
        // space, so no glued letter needs redeeming.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "band_gap",
                Some(5.0),
                "In Fig. 3, 5 ev was measured for the Ti-6Al-4V band gap.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    /// CONTRACT CHANGE (de-hardcoding): "2D"/"3D projection" stays
    /// dropped — no supplied unit redeems the glued 'd', and nothing in
    /// Rust special-cases the letter.
    #[test]
    fn projection_designators_stay_rejected_without_a_unit() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "projection",
            2.0,
            "Two 2D projections of the Ti-6Al-4V microstructure were aligned.",
        );
    }

    /// Round 6: the walk-back used to compose comma steps with an
    /// unbounded conjunction trim, so a VALUE after a sentence's locator
    /// label ("In Table 5,") walked back over the label number and
    /// dropped the whole value list — measured drops, all stamped at
    /// base. CONTRACT CHANGE (de-hardcoding): the walk-back, its
    /// unit-chain discriminator and the label vocabulary that drove it
    /// are all deleted — Rust holds no label words and no unit lexicon.
    /// The recall these cases pinned SURVIVES for the plain reason that
    /// every value now sits on clean boundaries beside the subject:
    /// there is no guard left to over-walk.
    #[test]
    fn value_lists_after_a_label_locator_still_stamp() {
        // Comma + conjunction list after "Table 5,": all three values.
        let list = "In Table 5, 950, 960 and 970 MPa were measured for Ti-6Al-4V.";
        for v in [950.0, 960.0, 970.0] {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(v), list, &GuardPolicy::SILENT).is_some(),
                "value {v} dropped from: {list}"
            );
        }
        // Longer list: the tail value that carries the unit AND the four
        // mid values the unbounded trim used to eat.
        let long = "Per Table 2, 950, 960, 970, 980 and 990 MPa were recorded for Ti-6Al-4V.";
        for v in [950.0, 960.0, 970.0, 980.0, 990.0] {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(v), long, &GuardPolicy::SILENT).is_some(),
                "value {v} dropped from: {long}"
            );
        }
        // "Fig. N," clause opener.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "As shown in Fig. 5, 950 MPa was the peak Ti-6Al-4V UTS.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // Comma-grouped value right after the label comma.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                "Per Table 3, 1,140 MPa was the Ti-6Al-4V peak.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        // Signed value after the label comma.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "surface_stress",
                Some(-950.0),
                "From Fig. 6, \u{2212}950 MPa was the Ti-6Al-4V surface stress.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );

        // Positive controls that must keep stamping exactly as before.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "Figure 3 shows a UTS of 950 MPa.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "alloy",
                "strength",
                Some(1100.0),
                "in Table 4 the alloy reached 1100 MPa",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "In Fig. 4, the Ti-6Al-4V UTS of 950 MPa is marked.",
                &GuardPolicy::SILENT
            )
            .is_some()
        );
    }

    // ------------------------------------------------------------------
    // Over-refusal instrumentation: a drop caused by a guard must name
    // the guard, not read as the model's hallucination. Each assert
    // below is a mutation target: removing the matching guard check in
    // `refusing_guard` turns its occurrence evidential (Ok instead of
    // Guarded), and removing the refusal recording in
    // `supporting_quote_or_refusal` collapses every Guarded into
    // NoSpan.
    // ------------------------------------------------------------------

    #[test]
    fn guarded_refusals_name_the_guard_that_dropped_the_value() {
        // Boundary: "95" inside "950" — digit continuation.
        let boundary = "The Ti-6Al-4V UTS is 950 MPa.";
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(95.0),
                boundary,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Boundary,
                span: boundary.to_string(),
            })
        );

        // Citation: the number sits in a bracketed marker.
        let citation = "Ti-6Al-4V has been studied extensively in prior work [1140].";
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                citation,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: citation.to_string(),
            })
        );

        // InsideName: the "718" of the claim's own subject name.
        let table = "Alloy UTS (MPa)\nTi-6Al-4V 950\nInconel 718 1375";
        assert_eq!(
            supporting_quote_or_refusal(
                "Inconel 718",
                "UTS",
                Some(718.0),
                table,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::InsideName,
                span: "Inconel 718 1375".to_string(),
            })
        );

        // Range: en-dash digit range endpoint.
        let range = "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.";
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                range,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Range,
                span: range.to_string(),
            })
        );

        // SignDomain: the ontology declares the quantity non-negative, so
        // a negative claim refuses whatever the occurrence looks like.
        // CONTRACT CHANGE (de-hardcoding): the declaration arrives
        // through `GuardPolicy`; under the silent policy this exact
        // sentence stamps (the vocabulary that knew "UTS" is gone).
        let separator = "Ti-6Al-4V UTS -950 MPa (longitudinal)";
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(-950.0),
                separator,
                &nonnegative_policy()
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::SignDomain,
                span: separator.to_string(),
            })
        );
    }

    /// Round 16: a dash in SEPARATOR or PARENTHETICAL role on a SIGNED
    /// predicate (residual stress) is refused — `SignDomain` cannot
    /// help because the quantity is genuinely signed. Both shapes drop
    /// as `SeparatorDash`, and BOTH directions are pinned: the forward
    /// rows are refused, and the true-minus rows (a verb/preposition
    /// before the dash) still stamp. Removing sub-condition (A) reddens
    /// the inline rows; removing (B) reddens the bracketed rows;
    /// widening either (or dropping the value<0 gate) reddens an
    /// over-refusal row. No cannot-fail arm.
    ///
    /// CONTRACT CHANGE (de-hardcoding): shape (B) now reads the fact's
    /// OWN unit term from the policy instead of `UNIT_TOKENS` — the
    /// bracketed rows below supply "MPa" exactly as the reader/ontology
    /// would, and are inert without it (last assert).
    #[test]
    fn separator_dash_refuses_signed_predicate_separator_shapes() {
        // (A) label-inline: the object name abuts the dash. Both glyphs.
        for prose in [
            "Ti-6Al-4V residual stress -950 MPa (longitudinal)",
            "Ti-6Al-4V residual stress \u{2212}950 MPa (longitudinal)",
        ] {
            assert_eq!(
                supporting_quote_or_refusal(
                    "Ti-6Al-4V",
                    "residual_stress",
                    Some(-950.0),
                    prose,
                    &GuardPolicy::SILENT
                ),
                Err(SupportRefusal::Guarded {
                    guard: RefusalGuard::SeparatorDash,
                    span: prose.to_string(),
                })
            );
        }
        // (B) bracketed: a dash glued to the fact's own unit right after
        // the value. The unit arrives through the policy.
        let bracketed = GuardPolicy {
            quantity_sign: QuantitySignDomain::Unspecified,
            unit_term: Some("MPa".to_string()),
        };
        for prose in [
            "The Ti-6Al-4V result -950 MPa- matched the target.",
            "The Ti-6Al-4V result \u{2212}950 MPa\u{2212} matched the target.",
        ] {
            assert_eq!(
                supporting_quote_or_refusal(
                    "Ti-6Al-4V",
                    "residual_stress",
                    Some(-950.0),
                    prose,
                    &bracketed
                ),
                Err(SupportRefusal::Guarded {
                    guard: RefusalGuard::SeparatorDash,
                    span: prose.to_string(),
                })
            );
        }
        // Without a supplied unit term shape (B) is inert — no lexicon
        // guesses "MPa". The sentence then stamps: an honest weaker note.
        assert!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-950.0),
                "The Ti-6Al-4V result -950 MPa- matched the target.",
                &GuardPolicy::SILENT
            )
            .is_ok()
        );
        // Over-refusal direction: a true minus MUST still stamp — the
        // dash follows a verb/preposition (never the object name) and no
        // dash is glued to the unit. These pin that the guard is precise.
        assert!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-350.0),
                "The residual stress in Ti-6Al-4V was \u{2212}350 MPa.",
                &bracketed
            )
            .is_ok()
        );
        assert!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "surface_stress",
                Some(-950.0),
                "From Fig. 6, \u{2212}950 MPa was the Ti-6Al-4V surface stress.",
                &bracketed
            )
            .is_ok()
        );
        // A genuine line-start minus must stamp too — the shape the
        // corpus carries as a KNOWN fabrication: no name abuts, no
        // trailing dash, locally indistinguishable from a separator.
        assert!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-350.0),
                "\u{2212}350 MPa was the Ti-6Al-4V surface stress (Fig. 6).",
                &bracketed
            )
            .is_ok()
        );
    }

    /// The reported guard is the FIRST refusal, not the last. Last-wins
    /// reporting was positional, not causal: any block where the value
    /// appears more than once (most real blocks) could flip the name
    /// when two sentences swapped. CONTRACT CHANGE (de-hardcoding): the
    /// cross-span pair uses two STRUCTURAL guards (Citation and
    /// Boundary) — the old Label/Boundary pair pinned the deleted label
    /// vocabulary.
    #[test]
    fn guarded_refusals_report_the_first_refusal_not_the_last() {
        // Cross-span: the value occurs once per span, each span refused
        // by a different guard. First-wins makes the report follow the
        // text, so swapping the sentences swaps the reported guard.
        let citation_first =
            "Prior work [2] covers the Inconel 718 modulus. The Inconel 718 modulus was 2.5 GPa.";
        assert_eq!(
            supporting_quote_or_refusal(
                "Inconel 718",
                "modulus",
                Some(2.0),
                citation_first,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: "Prior work [2] covers the Inconel 718 modulus.".to_string(),
            })
        );
        let boundary_first =
            "The Inconel 718 modulus was 2.5 GPa. Prior work [2] covers the Inconel 718 modulus.";
        assert_eq!(
            supporting_quote_or_refusal(
                "Inconel 718",
                "modulus",
                Some(2.0),
                boundary_first,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Boundary,
                span: "The Inconel 718 modulus was 2.5 GPa.".to_string(),
            })
        );
    }

    /// A block that never mentions the fact's salient tokens is NoSpan,
    /// which maps to MissingQuote — that drop IS the model's fault and
    /// must stay indistinguishable from a hallucinated quote.
    #[test]
    fn no_span_refusal_stays_missing_quote() {
        let block = "Discussion of prior work [1140] follows. No alloy data here.";
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                block,
                &GuardPolicy::SILENT
            ),
            Err(SupportRefusal::NoSpan)
        );
        assert_eq!(
            ClaimRejection::from(SupportRefusal::NoSpan),
            ClaimRejection::MissingQuote
        );
        assert_eq!(
            ClaimRejection::from(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: "s".to_string(),
            }),
            ClaimRejection::NoEvidentialOccurrence {
                guard: RefusalGuard::Citation,
                span: "s".to_string(),
            }
        );
    }
}
