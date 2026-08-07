//! Extracted claims typed for EMMO ingestion.
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
//! * subject / predicate / object with value + QUDT unit
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
//! RECORDED, NOT FIXED (round 12) — NUMERIC DECORATIONS ARE INVISIBLE:
//! "950 ± 30 MPa" stamps the TOLERANCE as the value. The plus-minus
//! sign appears nowhere in this file (grep-verified round 11,
//! re-verified round 12), so the uncertainty figure passes every guard
//! and becomes the property record under both spellings ("+/-" and
//! U+00B1), while the value it decorates stamps as a second claim.
//! Pinned as KNOWN corpus rows in tests/claim_corpus.rs (the
//! numeric-decorations family); the same family carries the
//! digit-dash-LETTER locants ("3-point", "2-step", "2-propanol",
//! "N-methyl-2-pyrrolidone"), which are equally unguarded. Record
//! only — fixing it needs a decoration-aware number scan, not a glyph
//! list.
//!
//! Known provenance caveat: claims can differ by fetch route.
//! (a)+(b) CLOSED round 10 + round 11; (c) opened round 11, recorded
//! closed round 12 on the fabrication half — but round 13 CORRECTED
//! that record: the closure is SPELLING-SCOPED, not predicate-scoped
//! (see item (c) below). The recall half is carried as corpus KNOWN
//! rows.
//!
//! (a) RANGES: JATS preserves U+2013,
//! `pdf-extract` normalises ranges to '-'. Until round 10 the engine
//! refused the en-dash endpoints but stamped the ASCII ones
//! (compound-friendly, by decision), so the same paper yielded different
//! claims depending on how it was fetched. The range guard now covers
//! every glyph of the dash class; both routes refuse both endpoints.
//! (b) SIGNED VALUES at |value| >= 1000: JATS typesets the minus as
//! U+2212, `pdf-extract` emits '-'. Until round 11 the U+2212 sign
//! attached only to the comma-grouped form, so the un-grouped
//! "\u{2212}1350" dropped NoSpan -> MissingQuote (misfiled as the
//! model's fault) while "-1350" stamped. The sign now attaches to the
//! plain form too; both routes stamp.
//! (c) SEPARATOR SHAPES, opened round 11, closed round 12 where it
//! fabricates: JATS typesets the label/value separator as U+2013,
//! `pdf-extract` emits '-'. Round 11's revert made the JATS spelling
//! of a separator shape ("UTS \u{2013}950 MPa") drop while the SAME
//! sentence fetched through the PDF route ("UTS -950 MPa") stamped a
//! negative from a positive source — the divergence class (a)+(b)
//! closed, reopened by the same commit that recorded closing it.
//! Round 12 NARROWED (round 13 record correction: NOT fully closed)
//! the fabrication half with the predicate's SIGN DOMAIN
//! (`NONNEGATIVE_QUANTITIES` in `claims.rs`): for non-negative
//! quantities BOTH routes now drop the separator shape (JATS U+2013:
//! no signed needle -> NoSpan; pdf-extract '-': the SignDomain guard).
//! ROUND 13 RECORD CORRECTION — this is SPELLING-SCOPED, not
//! predicate-scoped: `SignDomain` matches the object against the FIVE
//! EXACT strings of `NONNEGATIVE_QUANTITIES` (slice `contains` =
//! equality on the normalized text), and `normalize_for_containment`
//! only lowercases / maps `_` -> space / collapses whitespace — no
//! stemming, no head-noun, no unit strip. The extractor
//! (`text_extract.rs`) imposes NO property vocabulary, and the
//! codebase itself emits a missed spelling
//! (`object: "tensile strength".into()` at cli/main.rs:13405).
//! Measured guard-isolated round 13: of 22 common spellings only the
//! five canonical (uts, yield strength, hardness, density, grain size)
//! drop a negative; the other 17 — `ultimate tensile strength`,
//! `tensile strength`, `UTS (MPa)`, `0.2% yield strength`, `yield
//! stress`, `proof stress`, `Vickers hardness`, `microhardness`,
//! `relative density`, `average grain size`, `grain diameter`,
//! `compressive strength`, `fracture strength`, ... — still STAMP a
//! negative from a positive source. More entries is NOT the fix (the
//! const doc below disclaims "another word list"); round 13 item 5
//! ADOPTED a head-noun SUFFIX rule — every `*strength`, `*hardness` and
//! `*grain size` is non-negative and has NO signed homograph, so 16 of
//! the 22 spellings now drop a negative (was 5). The round-13 record
//! ALSO claimed `0 over-refusal` here — that was FALSE: at the
//! round-13 HEAD 27 signed spellings (differential phrasing like
//! `change in yield strength`, `difference in hardness`, plus the
//! genuinely-signed homographs `signal strength` / `field strength`)
//! returned Err(Guarded{SignDomain}). Round 14 item 1 corrected that
//! record and round 14 item 3 REPAIRED it: a whole-word differential
//! marker, or a signal/field-strength homograph, now exempts the
//! phrase from the suffix rule so those 27 stamp again (see
//! `is_nonnegative_quantity`). `density` stays EXACT: charge/current
//! density can be negative, so `relative density` / `bulk density`
//! still fabricate (6 spellings open). See `is_nonnegative_quantity`.
//!
//! ROUND 13 ITEM 2 — IS THE REVERT STILL BUYING ANYTHING? Settled by
//! measurement; the winning reading is (a) the revert IS load-bearing.
//! On the round-12 corpus M-A (re-adding U+2013/U+2014 to
//! `number_needles`) measured as a PURE WIN — 0 MUST_STAMP dropped, 0
//! MUST_DROP stamped, 5 KNOWN recall rows recovered — but ONLY because
//! every separator row sat on UTS, where SignDomain masked the
//! needle-set decision (item 1's cannot-fail rows). Item 1 moved those
//! six rows onto residual_stress (a genuinely SIGNED predicate), where
//! SignDomain cannot touch them; re-running M-A there stamps all six
//! (MUST_DROP stamped = 6) — the separator fabrication IS expressible
//! for signed predicates, and the revert is what holds it. The crux
//! the round-11/12 record left open — whether for a signed quantity the
//! separator fabrication is INEXPRESSIBLE (because -950 is a
//! "legitimate reading" of "\u{2013}950") — resolves cleanly: round
//! 11's convention reads U+2013/U+2014 as SEPARATORS (not minuses), so
//! under that convention the source value is +950 and a -950 claim is a
//! fabrication whatever the predicate's sign domain. The two readings
//! share one local shape (\u{2013} before a number); the engine cannot
//! have BOTH the recall (the legitimate -350 stamp, KNOWN MustStamp)
//! AND the safety (the separator -950 drop), so it keeps the safety.
//! The 5 recall rows are the price, NOT free; recovering them would
//! reopen the separator fabrication on every signed quantity.
//! For SIGNED quantities the routes still diverge on recall: the JATS
//! U+2013 spelling drops while the PDF '-' spelling stamps the genuine
//! negative — the '-' reading is the defensible one (the glyph IS the
//! minus sign), and the U+2013 drop is round 11's price (justified by
//! item 2 above), carried as corpus KNOWN rows. See `number_needles`.
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

use serde::{Deserialize, Serialize};

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
#[must_use]
pub fn cap_at_literature(claimed: &str) -> &'static str {
    if rank(claimed) <= rank(EVIDENCE_RESEARCH) && claimed == EVIDENCE_RESEARCH {
        EVIDENCE_RESEARCH
    } else if claimed == EVIDENCE_INDETERMINATE {
        EVIDENCE_INDETERMINATE
    } else if rank(claimed) >= rank(EVIDENCE_RESEARCH) {
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
    pub provenance: ClaimProvenance,
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
/// the only observable signal of how this gate behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefusalGuard {
    /// A negative claim against a quantity that is non-negative by
    /// physical definition (`NONNEGATIVE_QUANTITIES`): nonsense under
    /// EVERY dash glyph, so it refuses what the minus-vs-separator
    /// ambiguity cannot separate (round 12).
    SignDomain,
    /// Endpoint of an en-dash digit range ("950\u{2013}1100"): the sentence
    /// asserts bounds, not a point value.
    Range,
    /// Token-boundary rule: continuation by digits/decimals/grouping, a
    /// leading minus that signs it, or a non-unit letter glued to it.
    Boundary,
    /// Inside a citation marker ("[1140]", "(1140)", "{1140}").
    Citation,
    /// Immediately after a label word (Table/Figure/Ref/...) or a
    /// continuation of such a list.
    Label,
    /// Inside an occurrence of the subject's or object's own name
    /// (the "718" of "Inconel 718").
    InsideName,
}

/// Why no supporting span was found. `NoSpan` reads as the model's fault
/// (the block does not mention the fact at all); `Guarded` is the matcher's
/// refusal and carries the guard that refused the first candidate occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupportRefusal {
    /// No span held the subject or the object (both, for a non-numeric
    /// fact): there was nothing to scan.
    ///
    /// RECORDED, NOT FIXED (round 7): "nothing to scan" is also what a
    /// numeric fact reads as when NO NEEDLE FORM of the value matched
    /// anywhere (a glyph this engine lacks, e.g. a pre-round-7
    /// \u{3bc}m or 2.95\u{c5}). Such drops are matcher over-refusals,
    /// but they surface as `NoSpan` -> `MissingQuote`, which this
    /// module defines as the MODEL's fault — 3 of 12 remaining
    /// over-refusals were misfiled as hallucinations this way. A real
    /// fix must distinguish "no needle form matched" from "no span had
    /// evidence".
    NoSpan,
    /// A span held the subject or object and at least one occurrence of the
    /// value, but every occurrence was refused by a guard. Names the guard
    /// of the FIRST refused occurrence scanned: in a block where the value
    /// appears more than once (most real blocks) the first is the one a
    /// reader meets, and last-wins reporting was positional, not causal —
    /// it over-reported `Boundary` and under-reported `Label`.
    Guarded { guard: RefusalGuard, span: String },
}

impl From<SupportRefusal> for ClaimRejection {
    fn from(refusal: SupportRefusal) -> Self {
        match refusal {
            SupportRefusal::NoSpan => Self::MissingQuote,
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
    if claim.value.is_some() && claim.unit.is_none() {
        // RECORDED, NOT FIXED (round 7): this returns BEFORE the quote
        // check, so a unitless numeric fact never reaches the
        // supporting-span scan — its drop carries no guard and no span,
        // and whatever the matcher would have done with the value is
        // invisible in the drop record.
        return Err(ClaimRejection::NumericValueWithoutUnit);
    }
    for condition in &claim.conditions {
        if matches!(condition.value, ConditionValue::Number(_)) && condition.unit.is_none() {
            return Err(ClaimRejection::NumericConditionWithoutUnit {
                condition: condition.name.clone(),
            });
        }
    }
    let Some(quote) = claim.provenance.quote.as_deref() else {
        return Err(ClaimRejection::MissingQuote);
    };
    if !quote_in_block(quote, block_text) {
        return Err(ClaimRejection::QuoteNotInCitedBlock);
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
/// Support criteria (all case-insensitive, within one sentence/row span):
/// * numeric fact: the value's number appears together with the subject or
///   the object (a bare number could be a citation, so the number alone is
///   not enough). The number must occur as its own token — not as a
///   substring of a longer number and not a digit of an alloy
///   designation, glued (Ti-6Al-4V) or spaced (the claim's own Inconel 718)
///   — and the occurrence must be evidential: a number inside
///   a citation marker `[...]` or immediately after Table/Figure/Ref is a
///   label, not a measurement;
/// * non-numeric fact: both subject and object appear.
pub fn supporting_quote_or_refusal(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
) -> Result<String, SupportRefusal> {
    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);
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
                    match scan_number_evidence(&hay, v, &subject_n, &object_n) {
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
        None => Err(SupportRefusal::NoSpan),
    }
}

/// `supporting_quote_or_refusal` for callers that only need the quote.
#[must_use]
pub fn supporting_quote(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
) -> Option<String> {
    supporting_quote_or_refusal(subject, object, value, block_text).ok()
}

/// Split `block_text` into candidate supporting spans: sentences and table
/// rows. Spans are verbatim substrings (only trimmed), so anything found
/// here can be stored as a quote and later verified by containment.
fn supporting_spans(text: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut start = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            // Do not break sentences on the decimal point of a number.
            let is_decimal_point = b == &b'.'
                && i > 0
                && i + 1 < bytes.len()
                && bytes[i - 1].is_ascii_digit()
                && bytes[i + 1].is_ascii_digit();
            let is_sentence_break = matches!(b, b'.' | b'!' | b'?' | b';')
                && !is_decimal_point
                && !(*b == b'.' && period_ends_abbreviation(line, i));
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

/// Words whose trailing period is an abbreviation, not a sentence end
/// ("Fig.", "Ref.", "Eq."). Deliberately NOT `LABEL_WORDS`: words like
/// "sample." or "run." legitimately end sentences in methods prose, and
/// refusing to split there would fuse two sentences into one span and
/// create fresh false co-occurrences.
const ABBREV_LABEL_WORDS: &[&str] = &["fig", "figs", "ref", "refs", "eq", "eqs"];

/// Does the word right before the period at byte index `dot` end in a
/// label abbreviation? Such a period does not end a span: splitting on it
/// strands the label's number in a fresh span where the label word is
/// invisible, so "Fig. 2" stamps 2 as a measurement (H7).
fn period_ends_abbreviation(line: &str, dot: usize) -> bool {
    let word: String = line[..dot]
        .chars()
        .rev()
        .take_while(|c: &char| c.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>()
        .to_lowercase();
    ABBREV_LABEL_WORDS.contains(&word.as_str())
}

/// What a scan of the value's occurrences in one span found.
enum NumberScan {
    /// One occurrence is evidential.
    Evidential,
    /// No needle form of the value occurs in the span at all.
    Absent,
    /// Every occurrence was refused; holds the guard that refused the FIRST
    /// candidate occurrence scanned.
    Refused(RefusalGuard),
}

/// Does `hay` contain the value as an evidential measurement? At least one
/// string form of the value must occur with clean token boundaries, must
/// not be a citation marker or a Table/Figure/Ref label number, must not
/// sit inside an occurrence of the subject's or object's own name (the
/// "718" of "Inconel 718"), and must not be a dash range endpoint.
/// When every occurrence is refused, the scan names the guard that refused
/// the FIRST one — that name is what makes an over-refusal actionable, and
/// first-wins keeps it causal instead of positional: a value glued inside
/// another token ("AlSi10Mg") usually refuses Boundary late in the span,
/// while the standalone occurrence the reader actually sees was refused by
/// the real guard ("cross-section 10" -> Label).
///
/// RECORDED, NOT FIXED (round 8): "first" is first in NEEDLE-FORM
/// order, then position — the needle forms loop outside, the positions
/// inside. For any value with two needle forms (>= 1000 or negative),
/// the reported guard is therefore not necessarily the occurrence a
/// reader meets first. There is no ground truth for "the causal
/// guard"; the honest shape is reporting ALL refusing guards as a
/// set. Not implemented this round.
fn scan_number_evidence(hay: &str, value: f64, subject_n: &str, object_n: &str) -> NumberScan {
    let mut first: Option<RefusalGuard> = None;
    for needle in number_needles(value) {
        let mut search_from = 0usize;
        while let Some(rel) = hay[search_from..].find(&needle) {
            let start = search_from + rel;
            let end = start + needle.len();
            match refusing_guard(hay, &needle, start, end, subject_n, object_n, value) {
                None => return NumberScan::Evidential,
                Some(guard) => {
                    first.get_or_insert(guard);
                }
            }
            // Advance by the needle's first CHARACTER, not one byte: U+2212
            // needles lead with a 3-byte char, and a rejected occurrence that
            // advanced one byte landed the next hay[search_from..] slice
            // inside the minus (char-boundary panic, whole ingest aborted).
            // The match guarantees the needle sits at `start`, so its first
            // char is the char to skip; map_or(1, ..) keeps the loop
            // terminating even for a hypothetical empty needle.
            search_from = start + needle.chars().next().map_or(1, char::len_utf8);
        }
    }
    match first {
        Some(guard) => NumberScan::Refused(guard),
        None => NumberScan::Absent,
    }
}

/// The guard that refuses the occurrence of `needle` at [start, end), or
/// `None` when the occurrence is evidential. Checked most-specific first so
/// the NAMED refusal is the most informative one; the refuse/accept decision
/// itself does not depend on the order.
///
/// `SignDomain` is checked first though it is claim-level, not
/// occurrence-level: a negative value against a non-negative quantity is
/// nonsense whatever the occurrence looks like, and naming it beats
/// every positional guard's explanation. It is checked INSIDE the
/// occurrence loop, not hoisted claim-level before it, for one reason:
/// attribution. A negative non-negative-quantity claim whose value does
/// NOT occur in the block is NoSpan (the model's fault — it cited a
/// value the block never contained), not Guarded{SignDomain} (the
/// matcher's fault); the round-5 advance-walk rationale that used to
/// stand here was a non-reason (the advance walks whatever the guard
/// placement). Hoisting the check before the loop reddens
/// `sign_domain_does_not_mask_a_non_occurring_value_as_no_span`.
fn refusing_guard(
    hay: &str,
    needle: &str,
    start: usize,
    end: usize,
    subject_n: &str,
    object_n: &str,
    value: f64,
) -> Option<RefusalGuard> {
    if value < 0.0 && is_nonnegative_quantity(object_n) {
        return Some(RefusalGuard::SignDomain);
    }
    if dash_range_endpoint(hay, start, end) {
        return Some(RefusalGuard::Range);
    }
    if !clean_number_boundary(hay, needle, start, end) {
        return Some(RefusalGuard::Boundary);
    }
    if inside_citation_marker(hay, start) {
        return Some(RefusalGuard::Citation);
    }
    if preceding_word_is_label(hay, start, end) {
        return Some(RefusalGuard::Label);
    }
    if occurrence_inside_name(hay, start, end, subject_n)
        || occurrence_inside_name(hay, start, end, object_n)
    {
        return Some(RefusalGuard::InsideName);
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
        if name_start <= start && end <= name_end {
            return true;
        }
        // Char-not-byte advance, as in evidential_number_occurrence:
        // names may lead with a multi-byte char ("\u{3b1}-phase").
        search_from = name_start + hay[name_start..].chars().next().map_or(1, char::len_utf8);
    }
    false
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

/// Letters that may begin a unit token glued directly to a number in
/// table and PDF-extracted text where the space was lost: "950MPa",
/// "1073K", "50um" / "50\u{b5}m", "5wt%". Deliberately an allow-list,
/// not every letter: digit-then-letter gluing like "950x"
/// (magnification) or "2e5" (scientific notation) is not number+unit
/// and stays rejected. "2e5" is denied by `DENIED_UNIT_INITIALS`
/// below, so no future token starting with 'e' can reopen it. "950x"
/// is refused by the allow-list itself — no token starts with 'x';
/// round 9 removed the redundant 'x' denial as a cannot-fail entry
/// (it refused nothing the allow-list refused already). A future
/// token starting with 'x' would reopen the magnification form; the
/// corpus "950x" case is the tripwire for that.
///
/// DERIVED, not hand-listed: the first letter of every `UNIT_TOKENS`
/// entry, plus `EXTRA_UNIT_INITIALS`, minus `DENIED_UNIT_INITIALS`.
///
/// EXTRA: the glyphs PDF extractors actually emit where the token
/// list spells the unit differently — lowercased
/// '\u{e5}' ("2.95\u{c5} lattice parameter") — plus 'f' and 'l', the
/// round-6 hand list's recall letters for "72F" (Fahrenheit) and
/// "50l" (litres), which round 7's switch to pure derivation silently
/// lost. '\u{b0}' needs no entry: the degree sign is not
/// alphanumeric, so a glued "980\u{b0}C" passes the boundary check
/// regardless. Round 10: U+03BC GREEK SMALL LETTER MU moved OUT of
/// this list and into UNIT_TOKENS as a real "\u{3bc}m" token. The
/// EXTRA entry opened only the GLUED boundary path (`unit_initial` is
/// derived from both sources), while `unit_follows` reads the token
/// list alone — so "step 30 \u{3bc}m" dropped while its U+00B5 twin
/// stamped. Kept beside the token, the EXTRA entry would have been an
/// entry no mutation could kill — the same defect 'o' was removed for.
///
/// DENIED: 'e' rides on "ev", but a digit-glued 'e' in prose is
/// scientific notation ("2e5 per second", "1e6 cycles"), not
/// number+unit; denying it costs nothing — no other token starts with
/// 'e', and "In Fig. 3, 5 ev was measured" still stamps through the
/// spaced `unit_follows` path (the lib test and the corpus case both
/// sit under a label locator, so deleting "ev" from UNIT_TOKENS
/// reddens them — the original control sentence had no label word,
/// never consulted `unit_follows`, and 47511ce1's stated proof was
/// void).
/// 'x' was denied in round 8 and REMOVED in round 9: no UNIT_TOKENS
/// entry starts with 'x' and 'x' is not in `EXTRA_UNIT_INITIALS`, so
/// the denial refused nothing the allow-list refused already — a
/// cannot-fail entry, the same defect 'o' was deleted for in the same
/// commit. The old hand list also carried 'd' (days, "30d"); it stays
/// absent: its removal let "2D"/"3D projection" drop correctly, a
/// measured win that outweighs the days form.
const EXTRA_UNIT_INITIALS: &[char] = &['\u{e5}', 'f', 'l'];
const DENIED_UNIT_INITIALS: &[char] = &['e'];

fn unit_initial(c: char) -> bool {
    !DENIED_UNIT_INITIALS.contains(&c)
        && (EXTRA_UNIT_INITIALS.contains(&c) || UNIT_TOKENS.iter().any(|t| t.starts_with(c)))
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

/// Quantities that are non-negative by physical definition: ultimate
/// tensile strength, hardness, density, grain size and yield strength
/// cannot be negative under any convention. A negative claim against
/// one is therefore nonsense under EVERY dash glyph — which is what
/// separates the separator fabrication ("UTS -950" with a +950 source)
/// from the true negatives (residual stress, Seebeck coefficient,
/// temperature...), all of which ride genuinely SIGNED quantities.
/// Normalized names (lowercase, underscores -> spaces), matched against
/// the normalized object by EXACT equality (slice `contains`). ROUND 13
/// RECORD CORRECTION: that match is SPELLING-SCOPED — these five
/// strings only; `normalize_for_containment` does not stem or take a
/// head-noun, so 17 of 22 common spellings measured (`tensile
/// strength`, `relative density`, `average grain size`, ...) are NOT
/// recognized and still stamp a negative (see module header item (c)).
/// Round 12 item 1(c): measured, not guessed — this dropped the six
/// separator rows and the negative UTS range KNOWNs while every
/// signed-quantity negative still stamped. The set is deliberately
/// small and physical, not another word list: unknown
/// quantities default to SIGNED, the permissive direction — a negative
/// claim against an unrecognized quantity is never dropped by this
/// guard. Every entry is pinned by a corpus row that stamps without it.
/// Disabling the guard reddens 15 corpus rows at HEAD (measured round
/// 13: 6 UTS separators, 4 quantity pins incl. 'yield strength', 5
/// negative ranges). Round-12 commit 7b1f71ae recorded this M3 as 14 —
/// it omitted the 'yield strength' Inconel pin from the quantity-pin
/// count while crediting that same row to M5; the correct count is 15.
const NONNEGATIVE_QUANTITIES: &[&str] =
    &["uts", "hardness", "density", "grain size", "yield strength"];

/// Round 14 item 3: a differential marker as a WHOLE WORD turns an
/// otherwise-non-negative magnitude into a SIGNED delta. `change in
/// yield strength`, `difference in hardness`, `delta grain size`,
/// `reduction in strength`, `gradient in hardness` can all be negative
/// though the bare noun is a magnitude — so a suffix match on the noun
/// must NOT refuse them. Matched as whole tokens (`split_whitespace`)
/// so `change` does not fire inside `exchange`.
///
/// Deliberately NOT in this list: `relative` and `anisotropy`, which
/// denote RATIOS (non-negative), not deltas — adding them would license
/// fabrications like `relative density = -0.5`. `relative strength` /
/// `anisotropy in strength` therefore stay refused (measured, reported
/// in round 14): a negative under them is nonsense. Each marker below
/// IS pinned by its own control row in
/// `sign_domain_matches_head_noun_suffix_without_over_refusal` —
/// removing it from this slice reddens that row (no cannot-fail entry).
const SIGNED_DIFFERENTIAL_MARKERS: &[&str] = &[
    "change",
    "difference",
    "delta",
    "reduction",
    "loss",
    "increase",
    "deviation",
    "variation",
    "gradient",
];

/// Round 14 item 3: genuinely-SIGNED homographs of the `*strength`
/// suffix. `signal strength` (dBm is routinely negative) and
/// `*field strength` (signed vector components) are magnitudes that CAN
/// be negative, so the suffix rule must not refuse them. Matched by
/// `ends_with` so `magnetic field strength` / `electric field strength`
/// are caught by the `field strength` entry. NOT here: `ionic strength`,
/// `dielectric strength` — genuinely non-negative, kept refused (pinned
/// by forward control rows in the same test).
const SIGNED_STRENGTH_HOMOGRAPHS: &[&str] = &["signal strength", "field strength"];

/// Round 13 item 5: the exact-match closure was SPELLING-SCOPED — of 22
/// common spellings only the five canonical dropped a negative; 17
/// stamped one. Closing it with "more entries" would be another word
/// list (the const doc above disclaims that). The fix is a HEAD-NOUN
/// SUFFIX rule for the three quantities whose EVERY spelling is a
/// magnitude AND has NO signed homograph: every `*strength`, every
/// `*hardness`, every `*grain size` is non-negative — so `tensile
/// strength`, `microhardness`, `average grain size`, `compressive
/// strength`, `0.2% yield strength` now drop a negative too. NOT
/// `density`: `charge density` / `current density` can be negative, so
/// `density` stays EXACT and `relative density` / `bulk density` still
/// fabricate (the measured price of not over-refusing the signed
/// densities). Round 14 item 1 corrected the round-13 record that had
/// wrongly certified this rule's over-refusal as handled; round 14
/// items 2/3 REPAIR the over-refusal direction. Two exceptions now gate
/// the suffix rule, BOTH checked before it:
///   (a) A whole-word DIFFERENTIAL marker (`change`, `difference`,
///       `delta`, `reduction`, `loss`, `increase`, `deviation`,
///       `variation`, `gradient`; see `SIGNED_DIFFERENTIAL_MARKERS`)
///       anywhere makes the quantity a signed delta — `change in yield
///       strength`, `difference in hardness`, `delta grain size` can be
///       negative though the bare noun is a magnitude — so the suffix
///       rule must not refuse them. `relative` / `anisotropy` are NOT
///       markers (ratios, non-negative) — see the const doc.
///   (b) `signal strength` and `*field strength` are genuinely-signed
///       homographs of the suffix (see `SIGNED_STRENGTH_HOMOGRAPHS`).
///       `ionic strength` / `dielectric strength` stay refused.
/// BOTH directions are pinned by the lib test
/// `sign_domain_matches_head_noun_suffix_without_over_refusal`: the
/// forward rows drop a negative (reverting the suffix rule reddens
/// them), and the over-refusal rows stamp — removing any one marker, or
/// the homograph slice, reddens its own row. No arm or marker is
/// cannot-fail. Round 14 item 4: a trailing unit is stripped FIRST, so a
/// table-header object (`yield strength (MPa)`, `tensile strength, MPa`,
/// `hardness (HV)`, `grain size (um)`, `density (g/cm3)`) is recognized
/// by the exact/suffix rules. Scoped here (not in
/// `normalize_for_containment`), so general containment matching is
/// unaffected. The forward direction is pinned by unit-suffixed rows in
/// the same lib test (removing the strip reddens them). Stripping has no
/// pinnable over-refusal direction of its own: removing it only REMOVES
/// refusal power, and signed unit-suffixed quantities are protected by
/// the (a)/(b) exceptions above (measured — see the round-14 report).
/// Symbol spellings the extractor emits verbatim (`Rm`, `flow stress`,
/// `ultimate tensile stress`, ...) are NOT fixed here; they are a
/// vocabulary problem (round 14 item 5), not a trailing-unit problem.
fn is_nonnegative_quantity(object_n: &str) -> bool {
    let s = strip_trailing_unit(object_n);
    // A whole-word differential marker makes the quantity a signed delta:
    // checked before the suffix rule so `change in yield strength` stamps.
    if s.split_whitespace()
        .any(|t| SIGNED_DIFFERENTIAL_MARKERS.contains(&t))
    {
        return false;
    }
    // signal/field strength are genuinely-signed homographs of *strength.
    if SIGNED_STRENGTH_HOMOGRAPHS.iter().any(|h| s.ends_with(h)) {
        return false;
    }
    NONNEGATIVE_QUANTITIES.contains(&s)
        || s.ends_with("strength")
        || s.ends_with("hardness")
        || s.ends_with("grain size")
}

/// Strip ONE trailing unit so a table-header object is recognized by the
/// exact/suffix rules in `is_nonnegative_quantity`: a trailing
/// parenthetical (`yield strength (MPa)`) or a trailing comma-unit
/// (`tensile strength, MPa`). Returns the input unchanged otherwise.
/// Scoped to the SignDomain check only — `normalize_for_containment` is
/// untouched, so general containment matching is unaffected.
fn strip_trailing_unit(s: &str) -> &str {
    // trailing parenthetical: "yield strength (mpa)" -> "yield strength"
    if let Some(open) = s.rfind(')').and_then(|close| s[..close].rfind('(')) {
        return s[..open].trim_end();
    }
    // trailing comma-unit: "tensile strength, mpa" -> "tensile strength"
    if let Some(comma) = s.rfind(',') {
        return s[..comma].trim_end();
    }
    s
}

/// Token-boundary check: the occurrence must not be adjacent to a digit, to
/// a decimal point that continues it, to a digit-adjacent comma that
/// continues a grouped number ("1,140" is one number, in both directions),
/// to a leading minus that signs it ("-950" / "\u{2212}950" are one number),
/// or to an alphanumeric. Otherwise "95" matches inside "950", "1.5" inside
/// "11.5", "140" inside "1,140", and the "6" of "Ti-6Al-4V". After the
/// number, a letter from `UNIT_INITIALS` is allowed so glued units
/// ("950MPa") still stamp. En-dash range endpoints are refused by
/// `dash_range_endpoint`, not here.
fn clean_number_boundary(hay: &str, needle: &str, start: usize, end: usize) -> bool {
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
            if !unit_initial(after) {
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
    if let Some(after) = hay[end..].chars().next()
        && MINUS_CAPABLE_DASHES.contains(&after)
        && hay[end + after.len_utf8()..].starts_with(|c: char| c.is_ascii_digit())
    {
        return true;
    }
    if let Some(before) = hay[..start].chars().next_back()
        && MINUS_CAPABLE_DASHES.contains(&before)
        && hay[..start - before.len_utf8()].ends_with(|c: char| c.is_ascii_digit())
    {
        return true;
    }
    false
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
fn inside_citation_marker(hay: &str, start: usize) -> bool {
    let prefix = hay[..start].trim_end_matches(|c: char| {
        c.is_ascii_digit() || matches!(c, ',' | ' ') || MINUS_CAPABLE_DASHES.contains(&c)
    });
    let Some(open) = prefix.chars().next_back() else {
        return false;
    };
    match open {
        '[' => true,
        '(' | '{' => {
            let close = if open == '(' { ')' } else { '}' };
            let after = hay[start..].trim_start_matches(|c: char| {
                c.is_ascii_digit() || matches!(c, ',' | ' ') || MINUS_CAPABLE_DASHES.contains(&c)
            });
            after.starts_with(close)
        }
        _ => false,
    }
}

/// Words after which a number is a label, never a measurement — unless
/// the number carries a unit, which the exemption at the top of
/// `preceding_word_is_label` grants (a label number never has a unit
/// after it; a measurement always does).
const LABEL_WORDS: &[&str] = &[
    "table",
    "tables",
    "figure",
    "figures",
    "fig",
    "figs",
    "ref",
    "refs",
    "reference",
    "references",
    "section",
    "sections",
    "eq",
    "eqs",
    "equation",
    "equations",
    "chapter",
    "chapters",
    "entry",
    "entries",
    "scheme",
    "schemes",
    // Sample/run are methods-prose nouns too, but they double as
    // specimen/batch labels: "Sample 5 of Ti-6Al-4V" numbers the
    // specimen, it does not measure it. Kept here; the unit exemption
    // keeps "sample 3 mm thick" and "run 30 min" stamping.
    "sample",
    "samples",
    "run",
    "runs",
    // Round 9: the rest of the specimen-label family, all measured
    // stamping at HEAD — Specimen 5, Batch 12, Coupon 7, Test 3,
    // Trial 4, Experiment 2, Condition 3, Step 2, Panel 4, Column 3,
    // Row 2, Plot 2, Image 4, Micrograph 3, Curve 3, Inset 2,
    // page 12, Appendix 2, and Grade 5 (a designator doubly wrong:
    // Ti-6Al-4V IS grade 5). Every word is pinned by a corpus case;
    // the unit exemption keeps methods prose stamping. Singular forms
    // only — the WORD list stays singular because an unpinned list
    // entry is a cannot-fail item; the plural leak itself ("Specimens
    // 3 and 4" stamps) is pinned on the scoreboard as a KNOWN gap
    // since round 10, so the record of it can no longer go stale.
    "specimen",
    "batch",
    "coupon",
    "test",
    "trial",
    "experiment",
    "condition",
    "step",
    "panel",
    "column",
    "row",
    "plot",
    "image",
    "micrograph",
    "curve",
    "inset",
    "page",
    "appendix",
    "grade",
];

/// Words that continue a label list: the number after one of these is a
/// label when the number one step back sits a label word ("Tables 1 and 2").
const LIST_CONTINUATIONS: &[&str] = &["and", "or", "to", "through"];

/// Units a list's last item can carry. The curated set is deliberately
/// the vocabulary of measurements: reference lists never carry units, so
/// a chain that ends in one of these is a VALUE list and the walk-back
/// to a label word must stop. A token matches only at a token boundary,
/// so prose words that merely start with a unit ("for", "uts") never
/// match.
const UNIT_TOKENS: &[&str] = &[
    // pressure / stress / hardness
    "pa", "kpa", "mpa", "gpa", "tpa", "bar", "mbar", "kbar", "atm", "torr", "psi", "ksi", "hv",
    "hrc", "hrb", // force, length, mass
    "n", "kn", "mn", "gn", "m", "mm", "cm", "nm", "um", "\u{b5}m", "\u{3bc}m", "pm", "km", "g",
    "mg", "kg", // time, temperature
    "s", "ms", "ns", "ps", "min", "h", "k", "\u{b0}c", "\u{b0}f",
    // energy, power, frequency
    "j", "kj", "mj", "gj", "ev", "kev", "mev", "gev", "tev", "w", "mw", "kw", "hz", "khz", "mhz",
    "ghz", "thz", "rpm", // electrical, magnetic
    "v", "mv", "kv", "a", "ma", "ohm", "t", // fractions
    "%", "wt%", "at%", "vol%", "mol", "ppm", "ppb",
];

/// Does the text after `end` carry a unit for the number — one optional
/// space, then a unit token at a token boundary ("970 mpa", "1140mpa")?
/// `hay` is normalized: lowercase, single spaces.
fn unit_follows(hay: &str, end: usize) -> bool {
    let rest = hay[end..].strip_prefix(' ').unwrap_or(&hay[end..]);
    UNIT_TOKENS.iter().any(|u| {
        rest.strip_prefix(u)
            .is_some_and(|tail| tail.chars().next().is_none_or(|c| !c.is_alphanumeric()))
    })
}

/// Byte length of the leading number in `s`, reading through
/// thousands-grouping commas ("1,140"). A comma followed by a space is a
/// list separator, not grouping, and ends the number.
fn leading_number_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len()
        && (bytes[i].is_ascii_digit()
            || (bytes[i] == b',' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)))
    {
        i += 1;
    }
    i
}

/// Does the list chain continuing after `end` — ", <number>" items and
/// "<and|or|to|through> <number>" steps — close with a unit? A chain that
/// ends in a unit is a value list ("950, 960 and 970 MPa"); reference
/// lists never carry one ("Refs. 25, 26 and 27"). A unit directly after
/// the occurrence is a chain of length zero and counts too.
fn chain_ends_in_unit(hay: &str, end: usize) -> bool {
    let mut pos = end;
    loop {
        if unit_follows(hay, pos) {
            return true;
        }
        let rest = &hay[pos..];
        if let Some(item) = rest.strip_prefix(", ") {
            let n = leading_number_len(item);
            if n == 0 {
                return false;
            }
            pos += ", ".len() + n;
        } else if let Some(item) = rest.strip_prefix(' ')
            && let Some(conj) = LIST_CONTINUATIONS
                .iter()
                .find(|c| item.starts_with(*c) && item.as_bytes().get(c.len()) == Some(&b' '))
        {
            let skip = 1 + conj.len() + 1;
            let n = leading_number_len(&rest[skip..]);
            if n == 0 {
                return false;
            }
            pos += skip + n;
        } else {
            return false;
        }
    }
}

/// Step back over ", <number>" items one number at a time; the word at
/// the head of the list decides.
fn walk_comma_items(prefix: &mut String, word: &mut String) {
    while word.is_empty() && prefix.ends_with(',') {
        let before_comma = prefix[..prefix.len() - 1].trim_end_matches(' ');
        let number = trailing_word(before_comma);
        if number.is_empty() || !number.chars().all(|c: char| c.is_ascii_digit()) {
            break;
        }
        *prefix = before_comma[..before_comma.len() - number.len()]
            .trim_end_matches([' ', '.', ':'])
            .to_string();
        *word = trailing_word(prefix);
    }
}

/// Does the occurrence sit right after Table/Figure/Ref ("Table 1",
/// "Figure 2", "Ref. 25")? Such a number labels a document object; it is
/// not evidence for a property value. Continuations of a label list are
/// caught by stepping back over them to the head word: ", <number>"
/// items repeatedly ("Refs. 25, 26"), then one conjunction and the
/// number-run before it ("Tables 1 and 2", "Refs. 25\u{2013}27 and 28",
/// "Sections 3.1 and 4").
///
/// The walk runs only for REFERENCE lists: the chain continuing after
/// the occurrence decides. It ends in a unit -> value list -> the label
/// word before it is just the sentence's locator ("In Table 5, 950, 960
/// and 970 MPa"), and the walk must not reach it; no unit -> reference
/// list -> walk to the head word ("Refs. 25, 26 and 27"). The unit is
/// the discriminator the walk never looked at; without it the walk
/// stepped from a value back over the locator label and dropped every
/// value in the list.
///
/// The conjunction step trims the number-run before the conjunction
/// GREEDILY — digits, dots, commas, spaces and dashes — because dotted
/// labels are ubiquitous ("Sections 3.1 and 4", "Eqs. 2.1 and 3"): a
/// trim that stops at the '.' strands "3" as the head word and never
/// reaches "sections". Reference lists carry no units, so the walk
/// cannot be rescued by a unit check; a value list with a unit never
/// walks at all (`chain_ends_in_unit`), and one without a unit lands on
/// its real head word ("measured"), not a label. Round 6 bounded this
/// trim to one number-run; the bound opened dotted-label fabrications
/// and reddened nothing on revert, so it is gone.
fn preceding_word_is_label(hay: &str, start: usize, end: usize) -> bool {
    // A SPACED unit after the number makes it a measurement, whatever
    // word precedes it: "sample 3 mm thick" and "run 30 min" are
    // methods prose, while "Sample 5 of Ti-6Al-4V" (no unit) stays a
    // label. Checked first so it exempts the head word itself, not
    // just the list walk.
    //
    // The exemption REQUIRES the space (round 8). UNIT_TOKENS holds
    // eleven single letters (n m g s h k j w v a t) — exactly the
    // symbol letters of materials prose — so round 7's spaceless
    // exemption read a glued sub-panel letter or symbol column as a
    // unit and dropped the label guard for every label word: "Figure
    // 2a shows..." stamped 2, "Table 4a" stamped 4. Every measured
    // win that needs THIS exemption is spaced.
    //
    // HONEST COST (round 9 — the round-8 claim that
    // `clean_number_boundary` redeems glued-unit recall was FALSE):
    // passing the boundary check only avoids the Boundary guard; THIS
    // guard fires afterwards and nothing redeems it. The space
    // requirement silently costs thirteen glued recall forms, carried
    // as KNOWN failures in tests/claim_corpus.rs: sample 3mm,
    // run 30min, sample 980°C, samples 5mm, sample 30um, sample 5wt%,
    // and — round 10, the bill for the nineteen round-9 words —
    // coupon 3mm, specimen 5mm, panel 2mm, test 950MPa, scan step
    // 50um, condition 980C, trial 30min. Round 10 removed
    // cross-section 10mm from this list: that claim is a position, not
    // a property of the alloy — its drop is correct, not a cost. Round
    // 11 removed batch 25kg: the mass of one powder lot is extensive,
    // not a property — its drop is correct, not a cost.
    // Recorded residue: a SPACED single-letter unit
    // still exempts ("Table 4 K values" stamps 4) — see the corpus
    // KNOWN cases in tests/claim_corpus.rs.
    if hay[end..].starts_with(' ') && unit_follows(hay, end) {
        return false;
    }
    let mut prefix = hay[..start].trim_end_matches([' ', '.', ':']).to_string();
    let mut word = trailing_word(&prefix);
    if !chain_ends_in_unit(hay, end) {
        walk_comma_items(&mut prefix, &mut word);
        if LIST_CONTINUATIONS.contains(&word.as_str()) {
            // Round 10: the dash set is the whole dash class here too —
            // "Refs. 25\u{2010}27 and 28" used to strand its trim on the
            // dash and stamp 28, the same partial-trio leak the
            // citation marker had.
            prefix = prefix[..prefix.len() - word.len()]
                .trim_end_matches(|c: char| {
                    c.is_ascii_digit()
                        || matches!(c, ',' | ' ' | '.' | ':')
                        || MINUS_CAPABLE_DASHES.contains(&c)
                })
                .to_string();
            word = trailing_word(&prefix);
        }
    }
    LABEL_WORDS.contains(&word.as_str())
}

/// The trailing alphanumeric word of `prefix`, empty when `prefix` ends
/// in a non-word character.
fn trailing_word(prefix: &str) -> String {
    prefix
        .chars()
        .rev()
        .take_while(|c: &char| c.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
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
/// COVERAGE, corrected round 12 (the round-11 record overclaimed; the
/// round-12 closure is measured, not guessed): the revert holds for
/// U+2013/U+2014, and the predicate's SIGN DOMAIN covers what the
/// revert could not. U+2212 stays a sign glyph and ASCII '-' always
/// was one; round 12 item 1(b) measured the SAME separator shapes
/// still stamping a negative under both glyphs, and item 1(c) adopted
/// the one discriminator that separates the classes without a glyph
/// list: UTS, hardness, density, grain size and yield strength are
/// non-negative by physical definition, so a negative claim against
/// one is nonsense under EVERY glyph — `RefusalGuard::SignDomain`
/// (`NONNEGATIVE_QUANTITIES`) refuses it, the separator shapes drop
/// for the right reason, and the true negatives survive because they
/// ride genuinely signed quantities (residual stress, Seebeck,
/// temperature). The round-11 sentence "unambiguously a minus, never
/// a separator" described the glyph's typography, not the code, and
/// is struck: for SIGNED quantities the minus-vs-separator reading of
/// '-'/'\u{2212}' stays locally indistinguishable, and the engine keeps the
/// MINUS reading there — the glyph IS the minus sign, so a negative
/// that stamps is the defensible reading (its recall twin, the
/// correct positive, still drops Boundary; corpus KNOWN row).
/// Unknown quantities default to signed — the permissive direction.
/// Under U+2013/U+2014 the true negative drops (recall loss, corpus
/// KNOWN rows) and the separator shapes drop (corpus MustDrop pins).
///
/// FETCH ROUTE, the module header's class: header item (c) carries
/// the route view. Round 11 reopened it — the paper whose JATS
/// spelling ("UTS \u{2013}950 MPa") round 11 pinned MustDrop fabricated
/// -950 through the PDF route. Round 12 closed the fabrication half
/// for non-negative quantities (both routes now drop); for signed
/// quantities a recall divergence remains (JATS U+2013 drops, PDF '-'
/// stamps the genuine negative), recorded as the header describes.
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
            provenance: ClaimProvenance {
                document_id: "10.1234/hea".to_string(),
                document_url: "https://doi.org/10.1234/hea".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote: quote.map(str::to_string),
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
            provenance: ClaimProvenance {
                document_id: "10.1234/unrelated".to_string(),
                document_url: "https://doi.org/10.1234/unrelated".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote: None,
            },
        }
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
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), unrelated_block).is_none());
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
        let found = supporting_quote("CoCrFeNi", "thermal_conductivity", Some(11.5), BLOCK);
        assert_eq!(
            found.as_deref(),
            Some("Its thermal conductivity is 11.5 W/(m K) at room temperature.")
        );
    }

    #[test]
    fn supporting_quote_does_not_split_decimal_numbers() {
        let block = "See results. Conductivity of CoCrFeNi was 11.5 W/(m K). Done.";
        let found = supporting_quote("CoCrFeNi", "conductivity", Some(11.5), block);
        assert!(found.unwrap().contains("11.5"));
    }

    #[test]
    fn supporting_quote_needs_a_salient_token_beside_the_number() {
        // The number alone could be a citation number; it must appear with
        // the subject or the object to count as support.
        let block = "Discussion of prior work [1140] follows. No alloy data here.";
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), block).is_none());
    }

    #[test]
    fn supporting_quote_matches_comma_grouped_numbers() {
        let block = "The Ti-6Al-4V billet showed a UTS of 1,140 MPa.";
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), block).is_some());
    }

    #[test]
    fn non_numeric_fact_needs_subject_and_object_in_one_span() {
        let block = "The Ti-6Al-4V microstructure contained an alpha-beta phase.";
        assert!(supporting_quote("Ti-6Al-4V", "alpha-beta", None, block).is_some());
        // Object absent from the block: no support.
        assert!(supporting_quote("Ti-6Al-4V", "omega phase", None, block).is_none());
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
    fn assert_dropped_end_to_end(subject: &str, object: &str, value: f64, block: &str) {
        let quote = supporting_quote(subject, object, Some(value), block);
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
            provenance: ClaimProvenance {
                document_id: "10.1234/doc".to_string(),
                document_url: "https://doi.org/10.1234/doc".to_string(),
                source: "openalex".to_string(),
                locator: locator(),
                quote,
            },
        };
        assert_eq!(
            validate_and_stamp(claim, block).unwrap_err(),
            ClaimRejection::MissingQuote
        );
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
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), uts_block).is_some());
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "thermal_conductivity",
                Some(11.5),
                conductivity_block
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
        assert!(supporting_quote("CoCrFeNi", "conductivity", Some(5.0), block).is_none());
        // The integer part: only the after-'.' guard refuses it.
        assert!(supporting_quote("CoCrFeNi", "conductivity", Some(11.0), block).is_none());
        // Positive control: 11.5 itself still stamps.
        assert!(supporting_quote("CoCrFeNi", "conductivity", Some(11.5), block).is_some());
    }

    /// The sign is the finding: for residual stress, -950 vs +950 is the
    /// difference between compressive and tensile. Both halves of the
    /// round-4 repro are pinned. (a) The true negative claim stamps under
    /// U+2212 MINUS SIGN prose — killed by removing either U+2212 producer
    /// in `number_needles` (the decimal push owns the decimal assert; the
    /// signs loop owns the integer asserts). (b) The sign-flipped positive
    /// claim is dropped — killed by removing the before-minus guard in
    /// `clean_number_boundary`. The last assert pins the symmetry: a
    /// negative claim never stamps against positive prose either.
    #[test]
    fn negative_value_claims_match_negative_prose_and_refuse_the_flip() {
        let unicode_minus = "The residual stress in Ti-6Al-4V was \u{2212}950 MPa.";
        let ascii_minus = "The residual stress in Ti-6Al-4V was -950 MPa.";

        // The true negative claim stamps under both minus glyphs.
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "residual_stress", Some(-950.0), unicode_minus)
                .as_deref(),
            Some(unicode_minus)
        );
        assert!(
            supporting_quote("Ti-6Al-4V", "residual_stress", Some(-950.0), ascii_minus).is_some()
        );
        // Grouped negative integer under U+2212 (the signs loop).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-1140.0),
                "The residual stress in Ti-6Al-4V was \u{2212}1,140 MPa."
            )
            .is_some()
        );
        // Negative decimal under U+2212 (the decimal push).
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "seebeck_coefficient",
                Some(-11.5),
                "The CoCrFeNi Seebeck coefficient was \u{2212}11.5 uV/K."
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
                "The Ti-6Al-4V samples were tested at \u{2212}196 \u{b0}C."
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

        // Round 10: the old assert here pinned 1100 of "batches 950-1100"
        // as the joins_compound killer — but it stamped a batch
        // identifier as a measurement, and `dash_range_endpoint` now
        // refuses the whole digit/dash/digit class. joins_compound's
        // surviving effect is LETTER-dash-digit compounds ("U-235"),
        // itself a fabrication channel carried as a KNOWN row in the
        // corpus rather than a green pin. The signed-needle half of an
        // ASCII range still drops here: the "-1100" needle starts on
        // the hyphen, and the digit before it is alphanumeric.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            -1100.0,
            "The Ti-6Al-4V UTS ranged from 950-1100 MPa.",
        );
    }

    /// Round 5: after a REJECTED U+2212-prefixed needle, the scan must
    /// advance by the needle's first character, not one byte. "950x" is
    /// rejected because 'x' is deliberately not a unit initial; the old
    /// `start + 1` advance then landed `hay[search_from..]` inside the
    /// 3-byte U+2212 and panicked the whole ingest run on a non-char
    /// boundary. The correct outcome is a drop: the zoom factor is not
    /// evidence for a stress of -950. Round 12: the object is the SIGNED
    /// quantity 'stress', not UTS — under UTS the new SignDomain guard
    /// would refuse first and mask the boundary/'x' rejection this test
    /// exists to exercise.
    #[test]
    fn rejected_unicode_minus_needle_advances_by_char_not_byte() {
        assert_eq!(
            supporting_quote(
                "Ti-6Al-4V",
                "stress",
                Some(-950.0),
                "Ti-6Al-4V at \u{2212}950x zoom had stress."
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
                "Ti-6Al-4V stress \u{2212}950\u{2013}1100 MPa."
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
                "\u{3b1}-phase strength was 950 MPa."
            )
            .is_some()
        );
    }

    /// Round 13 item 4: the same char-not-byte advance, second bite of
    /// this bug class. The existing test above covers a MULTI-BYTE name
    /// glyph matching a multi-byte hay glyph (2-byte U+03B1 <-> 2-byte).
    /// This one covers a 1-BYTE name glyph that matches a MULTI-BYTE
    /// hay glyph: subject "-" folding onto a U+2010 HYPHEN (3 bytes) in
    /// the hay. The OLD advance `name.chars().next()` stepped 1 byte
    /// (the needle's first char) and landed mid-glyph inside the U+2010,
    /// panicking "byte index 3 is not a char boundary; it is inside
    /// '‐'". The fix advances by the HAY glyph at name_start. subject/
    /// object are model-supplied, so this is reachable from untrusted
    /// LLM output — a panic aborts the ingest run, not one claim. At
    /// HEAD this was invisible: 84 lib + 197 corpus stay green with the
    /// fix reverted, so the fix was UNPINNED. Reverting the
    /// `occurrence_inside_name` advance to `name.chars()` reddens this
    /// assert (panic).
    #[test]
    fn single_byte_dash_subject_matching_multibyte_hay_glyph_does_not_panic() {
        let r = supporting_quote_or_refusal("-", "UTS", Some(950.0), "ti\u{2010}6al had 950 MPa");
        assert!(r.is_ok(), "must not panic and must find the support: {r:?}");
    }

    /// Round 13 item 5 + round 14 items 2/3: the SignDomain guard matches
    /// by head-noun SUFFIX (every *strength/*hardness/*grain size is
    /// non-negative), with round-14 exceptions for differential phrasing
    /// and signal/field-strength homographs. Pin BOTH directions: the
    /// forward rows drop a negative under a non-negative spelling, and the
    /// over-refusal rows stamp under a legitimately-signed one. No arm or
    /// marker is cannot-fail — each reddens under its own revert.
    #[test]
    fn sign_domain_matches_head_noun_suffix_without_over_refusal() {
        // Forward direction: a negative under a genuinely-non-negative
        // spelling MUST be refused (is_err). Reverting the suffix rule to
        // exact-match reddens every row. `ionic strength` / `dielectric
        // strength` pin that the round-14 exceptions do NOT over-allow a
        // genuinely-non-negative homograph.
        for nonneg_spelling in [
            "tensile strength",
            "ultimate tensile strength",
            "microhardness",
            "Vickers hardness",
            "average grain size",
            "compressive strength",
            "0.2% yield strength",
            "ionic strength",
            "dielectric strength",
            // Round 14 item 4: a trailing unit (the table-header form) is
            // stripped before the suffix/exact rules. Removing
            // strip_trailing_unit reddens every row below.
            "yield strength (MPa)",
            "hardness (HV)",
            "grain size (um)",
            "tensile strength, MPa",
            "microhardness, HV0.5",
            "density (g/cm3)",
        ] {
            let prose = format!("The Ti-6Al-4V {nonneg_spelling} was -950 MPa.");
            let r = supporting_quote_or_refusal("Ti-6Al-4V", nonneg_spelling, Some(-950.0), &prose);
            assert!(
                r.is_err(),
                "suffix rule must drop a negative under {nonneg_spelling:?}: {r:?}"
            );
        }
        // Over-refusal direction: a negative under a legitimately-SIGNED
        // spelling MUST stamp (is_ok). The first six are the round-13
        // controls (kept). The next nine pin the differential markers:
        // removing any one marker from SIGNED_DIFFERENTIAL_MARKERS
        // reddens its row. The last four pin the signal/field
        // homographs: removing SIGNED_STRENGTH_HOMOGRAPHS (or the
        // field-strength entry) reddens them.
        for signed_spelling in [
            "residual stress",
            "Seebeck coefficient",
            "charge density",
            "current density",
            "density change",
            "grain size difference",
            "change in yield strength",
            "difference in hardness",
            "delta grain size",
            "reduction in strength",
            "loss of strength",
            "increase in tensile strength",
            "deviation in strength",
            "variation in grain size",
            "gradient in hardness",
            "signal strength",
            "field strength",
            "magnetic field strength",
            "electric field strength",
        ] {
            let prose = format!("The Ti-6Al-4V {signed_spelling} was -950 MPa.");
            let r = supporting_quote_or_refusal("Ti-6Al-4V", signed_spelling, Some(-950.0), &prose);
            assert!(
                r.is_ok(),
                "over-refusal: a negative under {signed_spelling:?} must still stamp: {r:?}"
            );
        }
    }

    /// Round 13 item 6.2: SignDomain is checked per-occurrence (inside the
    /// loop), not hoisted claim-level, so a non-occurring negative against
    /// a non-negative quantity stays NoSpan (the model cited a value the
    /// block never contained) rather than Guarded{SignDomain} (the
    /// matcher's fault). Hoisting the check before the loop reddens this.
    #[test]
    fn sign_domain_does_not_mask_a_non_occurring_value_as_no_span() {
        // The block states +950; the claim -950 never occurs in any needle
        // form. NoSpan (model's fault), not SignDomain (matcher's fault).
        let r = supporting_quote_or_refusal(
            "Ti-6Al-4V",
            "UTS",
            Some(-950.0),
            "The Ti-6Al-4V UTS was 950 MPa.",
        );
        assert!(
            matches!(r, Err(SupportRefusal::NoSpan)),
            "non-occurring negative must be NoSpan, not SignDomain: {r:?}"
        );
    }

    /// F-1: a thousands comma adjacent to a digit is part of the number,
    /// in both directions. "1,140" must behave byte-for-byte like "1140":
    /// searching for 140 or 1 inside it finds nothing, exactly the
    /// guarantee `substring_number_match_is_dropped` asserts for 95/950.
    #[test]
    fn comma_grouped_number_digits_are_not_token_boundaries() {
        let block = "The Ti-6Al-4V UTS is 1,140 MPa.";
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(140.0), block).is_none());
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1.0), block).is_none());
        // Control: the same fact written without grouping already drops 140.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(140.0),
                "The Ti-6Al-4V UTS is 1140 MPa."
            )
            .is_none()
        );
        // The real value still stamps in the grouped form.
        assert!(supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), block).is_some());

        assert!(supporting_quote("A", "UTS", Some(12.0), "A UTS is 12,345 MPa.").is_none());
        assert!(supporting_quote("A", "UTS", Some(345.0), "A UTS is 12,345 MPa.").is_none());
        assert!(supporting_quote("A", "UTS", Some(12345.0), "A UTS is 12,345 MPa.").is_some());

        // Row form: the leading "1" is not a standalone value either.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1.0),
                "Alloy UTS\nTi-6Al-4V 1,140\n..."
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
                "The Ti-6Al-4V samples measured 12, 15 and 950 MPa."
            )
            .is_some()
        );
    }

    /// F-2: in materials tables and PDF-extracted text the space between
    /// a number and its unit is often lost. A number glued to a
    /// unit-initial letter must still stamp; a number glued to any other
    /// letter must not.
    #[test]
    fn numbers_glued_to_their_unit_still_stamp() {
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V UTS is 950MPa."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(1073.0),
                "Ti-6Al-4V was annealed at 1073K."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "grain_size",
                Some(50.0),
                "CoCrFeNi grains of 50um were observed."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "grain_size",
                Some(50.0),
                "CoCrFeNi grains of 50\u{b5}m were observed."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "CoCrFeNi",
                "content",
                Some(5.0),
                "The CoCrFeNi alloy contains 5wt% Cr."
            )
            .is_some()
        );

        // Degree/percent glue was never broken (non-alphanumeric) and
        // must keep stamping.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(500.0),
                "Ti-6Al-4V was held at 500 \u{b0}C."
            )
            .is_some()
        );

        // Glued NON-unit letter: still dropped ("950x" is magnification,
        // not 950 + a unit).
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti-6Al-4V image at 950x magnification."
            )
            .is_none()
        );
        // Digit glue is still the substring reject: 95 inside 950MPa.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(95.0),
                "The Ti-6Al-4V UTS is 950MPa."
            )
            .is_none()
        );

        // A glued unit letter never redeems a digit inside a hyphen-joined
        // designation, ASCII or en dash (the "6" of "Ti-6Al-4V").
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(6.0),
                "The Ti-6Al-4V billets were 950MPa rated."
            )
            .is_none()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(6.0),
                "The Ti\u{2013}6Al\u{2013}4V billets were 950MPa rated."
            )
            .is_none()
        );

        // En-dash positive control: since round 12 the dash class folds
        // in NAME matching (find_name), so the en-dash typesetting of the
        // designation matches the ASCII subject through the SUBJECT arm
        // itself — before round 12 it survived ONLY through the object
        // arm of the subject-OR-object rule, the object arm carrying a
        // subject-matching failure. Either way the fact must stamp; do
        // not flip that OR to AND.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "The Ti\u{2013}6Al\u{2013}4V UTS is 950 MPa."
            )
            .is_some()
        );
    }

    /// Round 7: `UNIT_INITIALS` is DERIVED from `UNIT_TOKENS`, so it
    /// cannot drift from the unit vocabulary the chain/`unit_follows`
    /// checks already trust — plus the glyphs PDF extractors actually
    /// emit. The round-6 hand-list was missing four: U+03BC GREEK MU
    /// (extractors emit the Greek letter, not U+00B5 MICRO SIGN), 'o'
    /// (the mangled degree sign of "980oC"), 'r' (though `rpm` IS a
    /// unit token), and \u{e5} (angstrom). Round 8: 'o' left
    /// `EXTRA_UNIT_INITIALS` — it rides on the "ohm" token's initial,
    /// and listing it twice made an entry no mutation could kill — so
    /// each stamp assert below reddens when the SOURCE of its initial
    /// is removed: for 'o' that is the "ohm" token, for 'r' the
    /// "rpm" token, for \u{e5} `EXTRA_UNIT_INITIALS`. Round 10: the
    /// U+03BC source is the "\u{3bc}m" TOKEN in `UNIT_TOKENS` — the
    /// EXTRA entry beside it would have been the same unkillable
    /// duplicate 'o' was removed for, and only the token opens the
    /// SPACED path (`unit_follows` reads the token list alone). The
    /// trailing asserts pin the allow-list half — a glued NON-unit
    /// letter must still drop.
    #[test]
    fn glued_units_with_pdf_glyphs_still_stamp() {
        // U+03BC GREEK SMALL LETTER MU, the form PDF extractors emit.
        // Deleting the "\u{3bc}m" token reddens BOTH asserts: the glued
        // form loses its derived initial, the spaced form its unit.
        assert!(
            supporting_quote(
                "AlSi10Mg",
                "layer_thickness",
                Some(30.0),
                "AlSi10Mg was built with a 30\u{3bc}m layer thickness."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "AlSi10Mg",
                "scan_step_size",
                Some(30.0),
                "The AlSi10Mg scan step 30 \u{3bc}m was imaged."
            )
            .is_some()
        );
        // 'o' — the degree-sign mangle of "980 \u{b0}C".
        assert!(
            supporting_quote(
                "Inconel 718",
                "temperature",
                Some(980.0),
                "Inconel 718 was solution treated at 980oC."
            )
            .is_some()
        );
        // 'r' — rpm is a UNIT_TOKEN, so its initial must open a glued unit.
        assert!(
            supporting_quote(
                "Inconel 718",
                "rotation_speed",
                Some(1000.0),
                "The Inconel 718 powder was blended at 1000rpm for 30 min."
            )
            .is_some()
        );
        // \u{e5} ANGSTROM, lowercased by containment normalization.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "lattice_parameter",
                Some(2.95),
                "The Ti-6Al-4V beta lattice parameter was 2.95\u{c5}."
            )
            .is_some()
        );

        // Drop half: the allow-list is still an allow-list. A glued
        // letter no unit token starts with stays rejected (mutation-
        // proven red if unit_initial is broadened to every letter).
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            950.0,
            "The Ti-6Al-4V coupon was imaged at 950x magnification.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            950.0,
            "The Ti-6Al-4V coupon was indexed 950z in the log.",
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
                 after annealing."
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
                 sample reached 950 MPa."
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
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(1100.0), hyphen_range),
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
        // NoSpan. GROUND TRUTH for this exact tuple — the prose DOES
        // assert -350 MPa, an engineer calls that supported, so a stamp
        // is what SHOULD happen — belongs to the corpus KNOWN recall row
        // (tests/claim_corpus.rs, the U+2013 \u{2013}350 entry); it is not
        // restated here. Round 11's end-to-end drop assert contradicted
        // that row: one owner per fact. When a separator-safe sign
        // mechanism lands, THIS assert reddens and that KNOWN marker must
        // be stripped in the same change — two coherent signals that the
        // fix landed, not the old deadlock where the lib defended the
        // drop the corpus called a loss.
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "residual_stress", Some(-350.0), minus),
            Err(SupportRefusal::NoSpan)
        );
        // Grouped form: the unsigned grouped needle is still refused...
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "residual_stress",
            1140.0,
            "The residual stress in Ti-6Al-4V was \u{2013}1,140 MPa.",
        );
        // ...and its signed twin drops the same way (round 12 item 3:
        // MECHANISM, not ground truth). No '-'/'\u{2212}' needle matches the
        // U+2013 spelling of the grouped form either, so the grouped true
        // negative also surfaces as NoSpan. Ground truth (the prose
        // asserts \u{2013}1,140 MPa, a stamp is what SHOULD happen) belongs
        // to the corpus KNOWN grouped-recall row this round restores; the
        // assert pins only the current mechanism.
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "residual_stress",
                Some(-1140.0),
                "The residual stress in Ti-6Al-4V was \u{2013}1,140 MPa."
            ),
            Err(SupportRefusal::NoSpan)
        );

        // Stamp direction: a genuine point value in the same sentence
        // still stamps — the refusal is dash-adjacent, not sentence-wide.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "stress",
                Some(400.0),
                "The residual stress in Ti-6Al-4V was \u{2013}350 MPa as built and \
                 400 MPa after annealing."
            )
            .is_some()
        );

        // The range rule keeps its name: the high endpoint of
        // "950\u{2013}1100" has a digit before the dash, so it is
        // Range (checked first), not the new sign refusal.
        assert_eq!(
            supporting_quote_or_refusal(
                "Ti-6Al-4V",
                "UTS",
                Some(1100.0),
                "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa."
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
                supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(14.0), &list),
                Err(SupportRefusal::Guarded {
                    guard: RefusalGuard::Citation,
                    span: list.clone(),
                }),
                "dash U+{:04X} leaked the citation walk",
                dash as u32
            );
        }
    }

    /// Fabrication path 3 (label form): a number immediately after
    /// Table/Figure/Ref is a label, not a measurement.
    #[test]
    fn table_figure_ref_label_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            3.0,
            "Ti-6Al-4V properties are listed in Table 3.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "Ti-6Al-4V data appear in Figure 2.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            25.0,
            // No period after "Ref": with "Ref." the span split strands
            // the 25 in a span holding neither subject nor object, so the
            // label rule never fires. This form exercises it for real:
            // mutation-proven red when ref/refs are removed from
            // LABEL_WORDS.
            "UTS data for Ti-6Al-4V appears in Ref 25.",
        );
    }

    /// F-3: the label vocabulary also covers Section/Eq/Chapter/
    /// Entry/Scheme labels. Mutation-proven: removing "section" from
    /// LABEL_WORDS turns the first assert red. (Sample/Run are label
    /// words again as of round 7 — the unit exemption in
    /// `preceding_word_is_label` keeps methods prose stamping; their
    /// fabrication direction is pinned in
    /// `sample_and_run_label_numbers_are_not_support`.)
    #[test]
    fn section_and_kindred_label_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            4.0,
            "The Ti-6Al-4V results are in Section 4.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            7.0,
            "The Ti-6Al-4V model is given in Eq 7.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "The Ti-6Al-4V route is shown in Scheme 2.",
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
                "Inconel 718 showed a UTS of 718 MPa."
            )
            .is_some()
        );
        // Positive control: the genuine row still stamps verbatim.
        assert_eq!(
            supporting_quote("Inconel 718", "UTS", Some(1375.0), table).as_deref(),
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
                "The Ti-6Al-4V UTS (950 MPa) was reproducible."
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
                supporting_quote("Ti-6Al-4V", "UTS", Some(1140.0), &block.text).is_none(),
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
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), &body_block.text).is_some(),
            "the real value after the citation must still stamp: {:?}",
            body_block.text
        );
    }

    /// The list form of label references: in "Tables 1 and 2" the
    /// number after the conjunction is a label too, but only the
    /// immediately preceding word was checked, so 2 stamped. Walk back
    /// over the conjunction and the number before it, exactly once.
    /// (Mid-list comma items like "Refs. 25, 26" stay an open gap.)
    #[test]
    fn label_list_numbers_after_a_conjunction_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "Ti-6Al-4V data are listed in Tables 1 and 2.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            26.0,
            "Ti-6Al-4V is discussed in Refs. 25 and 26.",
        );
        // Positive control: a real measurement after "and <number>"
        // still stamps when the word one number back is not a label.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(960.0),
                "The Ti-6Al-4V samples measured 950 and 960 MPa."
            )
            .is_some()
        );
    }

    /// Comma-separated reference lists: "Refs. 25, 26" stamped 26 in
    /// round 3 because the walk-back covered one conjunction but not
    /// comma items. The walk now steps back over ", <number>" repeatedly
    /// before the conjunction step, and the label word at the head of
    /// the list decides. Positive controls pin the distinguishing
    /// feature: values in a comma list walk back to a non-label head
    /// word and stamp — they are values, not references. Mutation-proven
    /// red by deleting the comma walk-back loop (the two Refs asserts)
    /// and by a one-token dash on its `ends_with(',')` condition.
    #[test]
    fn comma_separated_reference_list_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            26.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26 for UTS data.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            27.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26, 27 for UTS data.",
        );
        // Comma list ending in a conjunction: the walk-backs compose.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            27.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26 and 27 for UTS data.",
        );
        // The first number after the label word stays refused too.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            25.0,
            "Ti-6Al-4V is discussed in Refs. 25, 26 for UTS data.",
        );

        // Positive controls: value lists are NOT reference lists; the
        // head word, not the commas, is the distinguishing feature.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(970.0),
                "The Ti-6Al-4V samples measured 950, 960 and 970 MPa."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(970.0),
                "The Ti-6Al-4V samples measured 950, 960, 970 MPa."
            )
            .is_some()
        );
        // A mid-list value also stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(960.0),
                "The Ti-6Al-4V samples measured 950, 960, 970 MPa."
            )
            .is_some()
        );
    }

    /// H7: an abbreviated label ("Fig. 2", "Ref. 25") used to defeat the
    /// label rule: the abbreviating period ended the span, stranding the
    /// number in a fresh span where the label word was invisible, so
    /// `UTS = 2` stamped. The period of such an abbreviation must not end
    /// a span. ("Table 3" / "Figure 2" without the period were already
    /// blocked; the round-2 `Ref 25` fixture only worked because the
    /// period was removed from it.)
    #[test]
    fn abbreviated_label_numbers_are_not_support() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            2.0,
            "As shown in Fig. 2 Ti-6Al-4V was tested to failure.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            25.0,
            "As reported in Ref. 25 Ti-6Al-4V is widely used.",
        );
        // "Eqs." joins the label family: the first number after it is a
        // label, and the conjunction step takes the "and 8" tail with it
        // (the walk trims the number-run before the conjunction, and a
        // reference list carries no units, so it still walks).
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            7.0,
            "The fits are given in Eqs. 7 and 8 for Ti-6Al-4V UTS.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            8.0,
            "The fits are given in Eqs. 7 and 8 for Ti-6Al-4V UTS.",
        );
        // Positive control: a real value in the same sentence as an
        // abbreviated label still stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "As shown in Fig. 2 the Ti-6Al-4V UTS is 950 MPa."
            )
            .is_some()
        );
    }

    /// Round 7: dotted section/equation numbering is ubiquitous, and
    /// reference lists can be space-separated. The round-6 bound (one
    /// number-run per conjunction) stopped at the '.' of "3.1", stranded
    /// "3" as the head word and never reached "sections" — the trailing
    /// number stamped. Reverting the walk-back to the greedy trim fixes
    /// these and reddens nothing: reference lists carry no units, value
    /// lists with a unit never walk at all (`chain_ends_in_unit`), and
    /// value lists without one land on their real head word. The first
    /// three asserts each go red when the greedy trim is narrowed back
    /// to one number-run; the fourth pins the dotted list's head number
    /// (label-word rule) and its dotted sibling (boundary rule).
    #[test]
    fn dotted_and_space_separated_label_lists_are_not_support() {
        assert_dropped_end_to_end(
            "Inconel 718",
            "UTS",
            4.0,
            "Inconel 718 data are in Sections 3.1 and 4.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            3.0,
            "The Ti-6Al-4V fit is given in Eqs. 2.1 and 3.",
        );
        assert_dropped_end_to_end(
            "Inconel 718",
            "creep_rate",
            27.0,
            "Inconel 718 creep is discussed in Refs. 25 26 and 27.",
        );
        // The head number of a dotted list stays refused too: the "3" of
        // "Sections 3 and 3.1" is refused by the label word itself, its
        // dotted sibling by the boundary rule.
        assert_dropped_end_to_end(
            "Inconel 718",
            "UTS",
            3.0,
            "Inconel 718 data are in Sections 3 and 3.1.",
        );

        // Round 10: a dash-joined reference range walks back through
        // every glyph of the dash class, not just '-', U+2013 and
        // U+2014; the conjunction tail is refused by Label, guard-named.
        let dash_range = "The Ti-6Al-4V data are listed in Refs. 25\u{2010}27 and 28.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(28.0), dash_range),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: dash_range.to_string(),
            })
        );

        // Stamp direction: the greedy trim must not over-walk a VALUE
        // list. Dotted values with no trailing unit walk back to their
        // real head word, not a label, and stamp. Round 10: the old
        // prose here was "batches were 3.1 and 4" — batch identifiers,
        // which certified the plural leak as required behaviour (the
        // corpus carries it as a KNOWN fabrication now). A genuine
        // unitless value list pins the same mechanism honestly.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "strain",
                Some(4.0),
                "The Ti-6Al-4V strains were 3.1 and 4."
            )
            .is_some()
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
    /// by fusing all rows into one span). The caption line also exercises
    /// the label rule: the "1" of "Table 1" sits in a span that holds the
    /// subject AND the object, so only the label rule keeps it from
    /// stamping (the 1.0 assert, mutation-proven by disabling
    /// `preceding_word_is_label`).
    #[test]
    fn properties_table_rows_are_separate_spans() {
        let table = "Table 1 UTS of Ti-6Al-4V and Inconel 718\n\
                     Alloy UTS (MPa)\n\
                     Ti-6Al-4V 950\n\
                     Inconel 718 1375";

        // Inconel's number cannot support a claim about Ti-6Al-4V: the
        // subject and 1375 never share a row.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1375.0, table);
        // The "1" of "Table 1" is a label, not a UTS value, even though
        // the caption span holds both the subject and the object.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1.0, table);

        // Genuine rows still stamp: the alloy and its own number share a row.
        assert_eq!(
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), table).as_deref(),
            Some("Ti-6Al-4V 950")
        );
        assert_eq!(
            supporting_quote("Inconel 718", "UTS", Some(1375.0), table).as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// Fabrication path 1, end to end through the real JATS sink: from one
    /// properties table, neither `Ti-6Al-4V UTS = 1375` (Inconel's number)
    /// nor `Ti-6Al-4V UTS = 1` (the "1" of "Table 1", or the digit inside
    /// "718") may find support in ANY block of the document.
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
                supporting_quote("Ti-6Al-4V", "UTS", Some(1375.0), &block.text).is_none(),
                "block {:?} fabricated support for Inconel's number: {:?}",
                block.locator.kind,
                block.text
            );
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(1.0), &block.text).is_none(),
                "block {:?} fabricated support from a label digit: {:?}",
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
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), &table.text).as_deref(),
            Some("Ti-6Al-4V 950")
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
                supporting_quote("Ti-6Al-4V", "UTS", Some(1375.0), &block.text).is_none(),
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
            supporting_quote("Ti-6Al-4V", "UTS", Some(950.0), &table.text).as_deref(),
            Some("Ti-6Al-4V 950")
        );
        assert_eq!(
            supporting_quote("Inconel 718", "UTS", Some(1375.0), &table.text).as_deref(),
            Some("Inconel 718 1375")
        );
    }

    /// Round 7: `sample`/`samples`/`run`/`runs` are back in
    /// `LABEL_WORDS` — round 6 cut them and opened specimen-label
    /// fabrications ("Sample 5 of Ti-6Al-4V was tested" stamped UTS =
    /// 5). The idioms separate cleanly: a label number never has a unit
    /// after it, a measurement always does, so the exemption at the top
    /// of `preceding_word_is_label` pins the stamp direction and the
    /// label words pin the drop direction. This test is the stamp half:
    /// each assert reddens when the `unit_follows` exemption is removed
    /// from `preceding_word_is_label` (NOT when the words leave
    /// LABEL_WORDS — that mutation reddens the drop test instead).
    #[test]
    fn methods_prose_nouns_sample_and_run_do_not_label_numbers() {
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "thickness",
                Some(3.0),
                "Each Ti-6Al-4V sample 3 mm thick was ground and polished."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "thickness",
                Some(3.0),
                "The Ti-6Al-4V samples 3 mm thick were ground and polished."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "duration",
                Some(2.0),
                "The Ti-6Al-4V run 2 h at 1073 K produced full densification."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "duration",
                Some(2.0),
                "The Ti-6Al-4V runs 2 h at 1073 K produced full densification."
            )
            .is_some()
        );
        // The reviewer corpus's literal recall cases, same mechanism.
        assert!(
            supporting_quote(
                "Inconel 718",
                "duration",
                Some(30.0),
                "Each Inconel 718 run 30 min at 980 \u{b0}C was quenched."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "AlSi10Mg",
                "thickness",
                Some(5.0),
                "The AlSi10Mg samples 5 mm thick were sectioned."
            )
            .is_some()
        );
        // The real temperature in the same sentence still stamps.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "temperature",
                Some(1073.0),
                "The Ti-6Al-4V run 2 h at 1073 K produced full densification."
            )
            .is_some()
        );
    }

    /// Round 8 (H1): the unit exemption at the top of
    /// `preceding_word_is_label` requires a SPACE before the unit.
    /// UNIT_TOKENS holds eleven single letters (n m g s h k j w v a
    /// t) — exactly the symbol letters of materials prose — so round
    /// 7's spaceless exemption read a glued sub-panel letter or
    /// symbol column as a unit and disabled the label guard for EVERY
    /// label word: 242 label-letter combinations flipped DROP ->
    /// STAMP, every one a fabrication ("Figure 2a shows...", "Table
    /// 4a"). Both directions pinned: the spaced recall form stamps,
    /// the glued panel letter drops. Mutations: deleting the
    /// `starts_with(' ')` condition reddens the drop asserts;
    /// deleting the exemption reddens the stamp asserts.
    #[test]
    fn unit_exemption_requires_a_space_before_the_unit() {
        // Stamp direction: every win that needs the exemption is spaced.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "thickness",
                Some(3.0),
                "Each Ti-6Al-4V sample 3 mm thick was ground and polished."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Inconel 718",
                "duration",
                Some(30.0),
                "Each Inconel 718 run 30 min at 980 \u{b0}C was quenched."
            )
            .is_some()
        );
        // Drop direction: a glued single letter is a sub-panel letter
        // or symbol column as often as it is a unit; the Label guard
        // must survive it.
        assert_dropped_end_to_end(
            "AlSi10Mg",
            "porosity",
            2.0,
            "Figure 2a shows the AlSi10Mg porosity.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "microstructure",
            3.0,
            "Fig. 3a shows the Ti-6Al-4V microstructure.",
        );
        // And the refusal keeps its name: the guard is Label, not
        // Boundary (the glued 'a' passes the boundary check as a unit
        // initial — only the label word refuses it).
        assert_eq!(
            supporting_quote_or_refusal(
                "AlSi10Mg",
                "porosity",
                Some(2.0),
                "Figure 2a shows the AlSi10Mg porosity."
            ),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: "Figure 2a shows the AlSi10Mg porosity.".to_string(),
            })
        );
    }

    /// Round 8 (H2): the derivation of glued unit initials from
    /// `UNIT_TOKENS` admitted 'e' via the "ev" token, reopening
    /// scientific notation — "2e5 per second" stamped 2, "1e6 cycles"
    /// stamped 1 — while the doc comment kept promising "2e5 stays
    /// rejected". The derivation now subtracts `DENIED_UNIT_INITIALS`,
    /// so the promise is enforced by the code, not by the token list
    /// happening to lack the letter. Both directions pinned: the
    /// glued-denial drops and the spaced form of the SAME unit still
    /// stamps. Mutations: removing 'e' from `DENIED_UNIT_INITIALS`
    /// reddens the 2e5/1e6 asserts; removing "ev" from `UNIT_TOKENS`
    /// reddens the spaced control.
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
        // One of the five designation-suffix fabrications round 7
        // opened: denying 'e' closes it.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "elongation",
            16.0,
            "The Ti-6Al-4V tensile tests followed ASTM E8-16e1.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            950.0,
            "The Ti-6Al-4V coupon was imaged at 950x magnification.",
        );
        // Spaced control: denying the glued 'e' costs nothing — spaced
        // "5 ev" still stamps, under a label locator so the exemption's
        // `unit_follows` actually decides the outcome: deleting "ev"
        // from UNIT_TOKENS reddens this. Round 9: the original sentence
        // had no label word and proved nothing.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "band_gap",
                Some(5.0),
                "In Fig. 3, 5 ev was measured for the Ti-6Al-4V band gap."
            )
            .is_some()
        );
    }

    /// Round 8 (H2): round 7's switch to pure derivation silently
    /// lost 'f' and 'l' — the round-6 hand list's recall letters for
    /// "72F" (Fahrenheit) and "50l" (litres) — and nothing tested
    /// them. Restored in `EXTRA_UNIT_INITIALS`, pinned here. 'd'
    /// (days, "30d") stays absent by decision: its removal let
    /// "2D"/"3D projection" drop correctly, and the trade is pinned
    /// by the last assert.
    #[test]
    fn f_and_l_recall_initials_stamp_glued_units_again() {
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "storage_temperature",
                Some(72.0),
                "The Ti-6Al-4V coupons were stored at 72F."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "tank_volume",
                Some(50.0),
                "The Ti-6Al-4V powder tank holds 50l."
            )
            .is_some()
        );
        // The decision half: keeping 'd' absent lets 2D/3D drop.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "projection",
            2.0,
            "Two 2D projections of the Ti-6Al-4V microstructure were aligned.",
        );
    }

    /// Round 7: the drop half of the sample/run restoration. With the
    /// words back in `LABEL_WORDS`, a specimen/batch number that carries
    /// NO unit is a label and drops — this is what round 6's cut opened:
    /// six fabrications measured across two corpora. Each assert reddens
    /// when its word leaves LABEL_WORDS; the stamp half lives in
    /// `methods_prose_nouns_sample_and_run_do_not_label_numbers`. The
    /// last two asserts are the free wins: a unit-bearing number after
    /// "cross-section" (word "section") stamps again too.
    #[test]
    fn sample_and_run_label_numbers_are_not_support() {
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 5.0, "Sample 5 of Ti-6Al-4V was tested.");
        assert_dropped_end_to_end(
            "Inconel 718",
            "build_failure",
            12.0,
            "Run 12 of the Inconel 718 build failed.",
        );
        // Both ends of a sample range are labels.
        assert_dropped_end_to_end(
            "AlSi10Mg",
            "print_count",
            1.0,
            "Samples 1 to 6 of AlSi10Mg were printed.",
        );
        assert_dropped_end_to_end(
            "AlSi10Mg",
            "print_count",
            6.0,
            "Samples 1 to 6 of AlSi10Mg were printed.",
        );
        // "runs" keeps its own kill: both ends of a run range.
        assert_dropped_end_to_end(
            "Inconel 718",
            "campaign",
            7.0,
            "Runs 7 to 12 of the Inconel 718 campaign failed.",
        );
        assert_dropped_end_to_end(
            "Inconel 718",
            "campaign",
            12.0,
            "Runs 7 to 12 of the Inconel 718 campaign failed.",
        );

        // Free wins from the unit exemption: measurement after
        // "cross-section" carries a unit, so the label word "section"
        // no longer refuses it.
        assert!(
            supporting_quote(
                "AlSi10Mg",
                "height",
                Some(10.0),
                "A cross-section 10 mm above the build plate was examined for AlSi10Mg."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "AlSi10Mg",
                "height",
                Some(10.0),
                "Cross-sections 10 mm above the build plate were examined for AlSi10Mg."
            )
            .is_some()
        );
    }

    /// Round 6: every label word KEPT in `LABEL_WORDS` gets a
    /// falsifiable assert — untested denylist words are the dangerous
    /// ones, and this list carried sixteen of them — the twelve added
    /// words plus figures/figs/reference/references. Each assert reddens
    /// when its word is removed from LABEL_WORDS. Pre-existing tests
    /// cover table/tables/figure/fig/ref/refs/section/eq/eqs/scheme.
    #[test]
    fn every_kept_label_word_refuses_its_number() {
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            3.0,
            "The Ti-6Al-4V data appear in Figures 2 and 3.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            5.0,
            "Ti-6Al-4V results are plotted in Figs. 4 and 5.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            12.0,
            "Ti-6Al-4V data are taken from Reference 12.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            13.0,
            "Ti-6Al-4V data are taken from References 12 and 13.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            5.0,
            "The Ti-6Al-4V data appear in Sections 4 and 5.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            7.0,
            "The Ti-6Al-4V fit is given in Equation 7.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            8.0,
            "The Ti-6Al-4V fits are given in Equations 7 and 8.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            3.0,
            "The Ti-6Al-4V model is given in Chapter 3.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            4.0,
            "The Ti-6Al-4V models are given in Chapters 3 and 4.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            5.0,
            "The Ti-6Al-4V data come from Entry 5.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            6.0,
            "The Ti-6Al-4V data come from Entries 5 and 6.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            3.0,
            "The Ti-6Al-4V routes are shown in Schemes 2 and 3.",
        );
    }

    /// Round 6 read "30-50um" as a measurement because the glued unit
    /// redeemed the high endpoint; round 9 picked the ground truth (a
    /// range endpoint is not a point value) and round 10 enforces it —
    /// `dash_range_endpoint` on the whole dash class owns digit/dash/
    /// digit runs FIRST, whatever unit follows the second number. The
    /// `clean_number_boundary` clause this test used to pin keeps its
    /// OTHER job: LETTER-dash designation digits stay refused. The
    /// en-dash object-arm assert is the deletion killer: with the clause
    /// gone, the "6" of Ti\u{2013}6Al\u{2013}4V stamps via the object
    /// arm (the subject's ASCII hyphens mismatch the en-dash text, and
    /// occurrence_inside_name sees nothing). Mutations: deleting the
    /// clause reddens the en-dash assert. Round 10 note: the clause's
    /// digit-before-dash condition is defense-in-depth now — Range
    /// refuses every digit/dash/digit occurrence before boundary runs.
    #[test]
    fn dash_range_endpoints_with_glued_units_are_not_point_values() {
        // Round 10 ground truth: the endpoints of a digit/dash/digit run
        // are range bounds, not measurements, even when the second
        // number carries a glued unit.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "layer_thickness",
            50.0,
            "Ti-6Al-4V powder layers of 30-50um were deposited.",
        );
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "layer_thickness",
            30.0,
            "Ti-6Al-4V powder layers of 30-50um were deposited.",
        );
        assert_dropped_end_to_end(
            "CoCrFeNi",
            "grain_size",
            10.0,
            "CoCrFeNi grains of 5-10mm were observed.",
        );
        // Letter-dash designation digits stay refused. In the en-dash
        // form ONLY this clause refuses the "6" — deletion turns it red.
        assert_dropped_end_to_end(
            "Ti-6Al-4V",
            "UTS",
            6.0,
            "The Ti\u{2013}6Al\u{2013}4V UTS is 950 MPa.",
        );
        // ASCII form: double-covered by occurrence_inside_name; kept as
        // documentation of the class.
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 6.0, "The Ti-6Al-4V UTS is 950 MPa.");
    }

    /// Round 6: the walk-back used to compose comma steps with an
    /// unbounded conjunction trim, so a VALUE after a sentence's locator
    /// label ("In Table 5,") walked back over the label number and
    /// dropped the whole value list — measured drops, all stamped at
    /// base. The chain continuing after the occurrence decides: it ends
    /// in a unit -> value list, no walk; no unit -> reference list,
    /// walk. Mutation-proven red by removing the gate, by removing the
    /// comma step or the conjunction step of the forward chain scan, or
    /// by removing the head-of-chain unit check. The reference-list
    /// asserts above (Refs. 25, 26 / and 27 / Eqs. 7 and 8) kill
    /// mutations that over-broaden `UNIT_TOKENS` ("for" as a unit would
    /// stamp them).
    #[test]
    fn value_lists_after_a_label_locator_still_stamp() {
        // Comma + conjunction list after "Table 5,": all three values.
        let list = "In Table 5, 950, 960 and 970 MPa were measured for Ti-6Al-4V.";
        for v in [950.0, 960.0, 970.0] {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(v), list).is_some(),
                "value {v} dropped from: {list}"
            );
        }
        // Longer list: the tail value that carries the unit AND the four
        // mid values the unbounded trim used to eat.
        let long = "Per Table 2, 950, 960, 970, 980 and 990 MPa were recorded for Ti-6Al-4V.";
        for v in [950.0, 960.0, 970.0, 980.0, 990.0] {
            assert!(
                supporting_quote("Ti-6Al-4V", "UTS", Some(v), long).is_some(),
                "value {v} dropped from: {long}"
            );
        }
        // "Fig. N," clause opener — guard interaction: the H7 period fix
        // keeps the value in one span with "fig", and the comma walk then
        // traversed it. Neither guard alone did this.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "As shown in Fig. 5, 950 MPa was the peak Ti-6Al-4V UTS."
            )
            .is_some()
        );
        // Comma-grouped value right after the label comma.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(1140.0),
                "Per Table 3, 1,140 MPa was the Ti-6Al-4V peak."
            )
            .is_some()
        );
        // Signed value after the label comma.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "surface_stress",
                Some(-950.0),
                "From Fig. 6, \u{2212}950 MPa was the Ti-6Al-4V surface stress."
            )
            .is_some()
        );

        // Positive controls that must keep stamping exactly as before.
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "Figure 3 shows a UTS of 950 MPa."
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "alloy",
                "strength",
                Some(1100.0),
                "in Table 4 the alloy reached 1100 MPa"
            )
            .is_some()
        );
        assert!(
            supporting_quote(
                "Ti-6Al-4V",
                "UTS",
                Some(950.0),
                "In Fig. 4, the Ti-6Al-4V UTS of 950 MPa is marked."
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
        // Label: span holds subject + object + value; the label walk
        // refuses the only occurrence.
        let label = "Ti-6Al-4V properties are listed in Table 3.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "properties", Some(3.0), label),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: label.to_string(),
            })
        );

        // Boundary: "95" inside "950" — digit continuation.
        let boundary = "The Ti-6Al-4V UTS is 950 MPa.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(95.0), boundary),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Boundary,
                span: boundary.to_string(),
            })
        );

        // Citation: the number sits in a bracketed marker.
        let citation = "Ti-6Al-4V has been studied extensively in prior work [1140].";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(1140.0), citation),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Citation,
                span: citation.to_string(),
            })
        );

        // InsideName: the "718" of the claim's own subject name.
        let table = "Alloy UTS (MPa)\nTi-6Al-4V 950\nInconel 718 1375";
        assert_eq!(
            supporting_quote_or_refusal("Inconel 718", "UTS", Some(718.0), table),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::InsideName,
                span: "Inconel 718 1375".to_string(),
            })
        );

        // Range: en-dash digit range endpoint.
        let range = "The Ti-6Al-4V UTS ranged from 950\u{2013}1100 MPa.";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(950.0), range),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Range,
                span: range.to_string(),
            })
        );

        // SignDomain: a negative claim against a quantity that is
        // non-negative by physical definition — refused whatever the
        // occurrence looks like (round 12 item 1c). Deleting the
        // NONNEGATIVE_QUANTITIES check reddens this assert and lets the
        // separator shape stamp a compressive value from a tensile source.
        let separator = "Ti-6Al-4V UTS -950 MPa (longitudinal)";
        assert_eq!(
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(-950.0), separator),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::SignDomain,
                span: separator.to_string(),
            })
        );
    }

    /// Round 7: the reported guard is the FIRST refusal, not the last.
    /// Last-wins reporting was positional, not causal: any block where
    /// the value appears more than once (most real blocks) could flip
    /// the name when two sentences swapped, and the natural
    /// single-sentence case reported Boundary when the real refusal was
    /// Label. An engineer chasing "Boundary" from last-wins would go
    /// read `clean_number_boundary` and find nothing wrong. Mutations:
    /// switching `supporting_quote_or_refusal` back to last-wins reddens
    /// the first assert; switching `scan_number_evidence` reddens the
    /// in-span assert.
    #[test]
    fn guarded_refusals_report_the_first_refusal_not_the_last() {
        // Cross-span: the value occurs once per span, each span refused
        // by a different guard. First-wins makes the report follow the
        // text, so swapping the sentences swaps the reported guard.
        let label_first =
            "Table 2 lists the Inconel 718 data. The Inconel 718 modulus was 2.5 GPa.";
        assert_eq!(
            supporting_quote_or_refusal("Inconel 718", "modulus", Some(2.0), label_first),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: "Table 2 lists the Inconel 718 data.".to_string(),
            })
        );
        let boundary_first =
            "The Inconel 718 modulus was 2.5 GPa. Table 2 lists the Inconel 718 data.";
        assert_eq!(
            supporting_quote_or_refusal("Inconel 718", "modulus", Some(2.0), boundary_first),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Boundary,
                span: "The Inconel 718 modulus was 2.5 GPa.".to_string(),
            })
        );

        // Within ONE span: the standalone occurrence a reader meets is
        // refused by Label ("cross-section 10"); the occurrence glued
        // inside "alsi10mg" refuses Boundary later in the scan.
        // Last-wins reported Boundary here; the real guard is Label.
        let one_span =
            "A cross-section 10 layers above the build plate showed 3% AlSi10Mg porosity.";
        assert_eq!(
            supporting_quote_or_refusal("AlSi10Mg", "porosity", Some(10.0), one_span),
            Err(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: one_span.to_string(),
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
            supporting_quote_or_refusal("Ti-6Al-4V", "UTS", Some(1140.0), block),
            Err(SupportRefusal::NoSpan)
        );
        assert_eq!(
            ClaimRejection::from(SupportRefusal::NoSpan),
            ClaimRejection::MissingQuote
        );
        assert_eq!(
            ClaimRejection::from(SupportRefusal::Guarded {
                guard: RefusalGuard::Label,
                span: "s".to_string(),
            }),
            ClaimRejection::NoEvidentialOccurrence {
                guard: RefusalGuard::Label,
                span: "s".to_string(),
            }
        );
    }
}
