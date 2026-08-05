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
/// (`subject`, `object`, `value`). Returns `None` when the block does not
/// contain the fact's salient evidence — such a fact cannot become a claim
/// without fabricating provenance.
///
/// Support criteria (all case-insensitive):
///
/// * **numeric fact** — all four must hold:
///   1. the subject appears somewhere in the **block** (not necessarily the
///      same span: papers say "Ti-6Al-4V samples were prepared. The alloy
///      showed a UTS of 1140 MPa.");
///   2. the value's number appears in the span at **digit boundaries**, so
///      `1140` is not found inside `11140` or `11,140`;
///   3. that number is **not inside a bracketed citation marker** — `[1140]`
///      is a reference, not a measurement;
///   4. the **property is mentioned before the number** in the same span,
///      either literally (`UTS`) or as the acronym of consecutive words
///      (`ultimate tensile strength`). This is what binds the number to
///      *this* claim: in "batch 1140 showed a UTS of 950 MPa" the property
///      follows the number, so 1140 is not its value.
/// * **non-numeric fact** — subject and object both appear in one span.
///
/// Two independent reviewers stamped seven fabricated blocks through the
/// previous version of this function, which required only that the number and
/// (subject *or* object) co-occur anywhere in the span. Each rule above kills
/// at least one of them; the tests name them A1–A7.
#[must_use]
pub fn supporting_quote(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
) -> Option<String> {
    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);

    // A numeric fact whose subject is nowhere in the document cannot be
    // supported by it, however well the number matches.
    if value.is_some()
        && (subject_n.is_empty() || !normalize_for_containment(block_text).contains(&subject_n))
    {
        return None;
    }

    for span in supporting_spans(block_text) {
        let hay = normalize_for_containment(span);
        let supported = match value {
            Some(v) => {
                let masked = mask_bracketed(&hay);
                number_needles(v)
                    .iter()
                    .filter_map(|n| {
                        find_at_digit_boundary(&masked, n).map(|pos| (pos, pos + n.len()))
                    })
                    .any(|(start, end)| {
                        property_binds_number(&object_n, &subject_n, &masked, start, end)
                    })
            }
            None => {
                !subject_n.is_empty()
                    && !object_n.is_empty()
                    && hay.contains(&subject_n)
                    && hay.contains(&object_n)
            }
        };
        if supported {
            return Some(span.trim().to_string());
        }
    }
    None
}

/// Blank out `[...]` spans so digits inside a citation marker cannot be read
/// as a measurement. Characters are replaced one-for-one with spaces, so byte
/// offsets stay meaningful *within the returned string*.
fn mask_bracketed(hay: &str) -> String {
    let mut out = String::with_capacity(hay.len());
    let mut depth = 0usize;
    for c in hay.chars() {
        match c {
            '[' => {
                depth += 1;
                out.push(' ');
            }
            ']' => {
                depth = depth.saturating_sub(1);
                out.push(' ');
            }
            _ if depth > 0 => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// Find `needle` in `hay` where neither side continues a longer number.
///
/// Without this, `1140` matches inside `11140` (a sample id) and `1,140`
/// matches inside `11,140` (a component count) — reviewer bypasses A6 and A7.
fn find_at_digit_boundary(hay: &str, needle: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = hay[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_digit());
        // A trailing `.5` or `,000` means the real number is longer.
        let mut after = hay[end..].chars();
        let after_ok = match after.next() {
            None => true,
            Some(c) if c.is_ascii_digit() => false,
            Some(c) if c == '.' || c == ',' => after.next().is_none_or(|d| !d.is_ascii_digit()),
            Some(_) => true,
        };
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// Words that may sit between a property and its value without breaking the
/// binding: "a UTS **of** 1140 MPa", "elongation **was** 14".
const BINDING_CONNECTIVES: &[&str] = &[
    "of",
    "was",
    "were",
    "is",
    "are",
    "at",
    "to",
    "a",
    "an",
    "the",
    "about",
    "approximately",
    "reached",
    "measured",
    "showed",
    "exhibited",
    "had",
    "with",
    "up",
];

/// Does the claim's property bind to the number at `num`?
///
/// **Binding, not ordering.** The previous rule only asked whether the property
/// appeared *before* the number, which a reviewer defeated three ways:
///
/// * `head.contains("uts")` matched inside `outputs`, `struts`, `nuts` — a raw
///   substring test, so a part count became a tensile strength;
/// * reordering beat it outright — "A UTS of 950 MPa was measured for
///   Ti-6Al-4V batch 1140" stamps 1140 as the UTS because UTS merely came
///   first, while the real value 950 sits in between;
/// * any three words whose initials spell the acronym bound it — "**U**nder
///   **t**hermal **s**tress, ... 1140 C".
///
/// It was also lossy in the other direction: "14% elongation" is the standard
/// way to report elongation, and a property-must-come-first rule can never
/// support it.
///
/// So: the property mention and the number must be **adjacent in either
/// order**, separated only by connectives, the subject, or short unit-like
/// tokens — and never by another number. A digit in the gap means some other
/// quantity sits between them, which is exactly the batch-id/sample-count
/// family.
fn property_binds_number(
    object_n: &str,
    subject_n: &str,
    hay: &str,
    num_start: usize,
    num_end: usize,
) -> bool {
    if object_n.is_empty() {
        return false;
    }
    property_mentions(object_n, hay)
        .into_iter()
        .any(|(start, end, from_acronym)| {
            // Property first ("a UTS of 1140 MPa") tolerates connectives and
            // the subject. Value first ("14% elongation", "1140 MPa UTS") does
            // NOT: a verb between the number and the property means they are
            // separate facts -- "batch 1140 showed a UTS of 950 MPa" names the
            // real value 950, and 1140 is the batch. Only a unit may sit there.
            let (gap, value_first) = if end <= num_start {
                (&hay[end..num_start], false)
            } else if num_end <= start {
                (&hay[num_end..start], true)
            } else {
                return false; // overlapping — not a separate mention
            };
            // An acronym expansion binds only within its own clause: a comma
            // between the words and the number means they are not one phrase.
            if from_acronym && gap.contains(',') {
                return false;
            }
            if value_first {
                gap_is_only_unit(gap)
            } else {
                gap_is_only_connective(gap, subject_n)
            }
        })
}

/// Every place the property is named: literal at word boundaries, plus
/// acronym expansions over consecutive words. Returns `(start, end,
/// from_acronym)` byte ranges into `hay`.
fn property_mentions(object_n: &str, hay: &str) -> Vec<(usize, usize, bool)> {
    let mut out = Vec::new();
    let is_word_char = |c: char| c.is_ascii_alphanumeric();

    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(object_n) {
        let start = from + rel;
        let end = start + object_n.len();
        // Word boundaries: "uts" must not be found inside "outputs".
        let before_ok = hay[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = hay[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            out.push((start, end, false));
        }
        from = start + 1;
    }

    // Acronym expansion only makes sense for a single alphabetic token.
    if object_n.contains(' ') || !object_n.chars().all(|c| c.is_ascii_alphabetic()) {
        return out;
    }
    let n = object_n.chars().count();
    if n < 2 {
        return out;
    }
    let mut words: Vec<(usize, &str)> = Vec::new();
    let mut word_start: Option<usize> = None;
    for (i, ch) in hay.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = word_start.take() {
                words.push((s, &hay[s..i]));
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(s) = word_start {
        words.push((s, &hay[s..]));
    }
    for w in words.windows(n) {
        let initials: String = w
            .iter()
            .filter_map(|(_, word)| word.chars().find(|c| c.is_ascii_alphanumeric()))
            .collect();
        if initials == object_n {
            let (start, _) = w[0];
            let (last_start, last_word) = w[n - 1];
            // End at the last alphanumeric character, so trailing punctuation
            // stays in the gap. Otherwise "stress," swallows its own comma and
            // the clause-boundary check below never sees it.
            let trimmed = last_word.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
            out.push((start, last_start + trimmed.len(), true));
        }
    }
    out
}

/// May this text sit between a property and its value?
///
/// Connectives, the subject, and short unit-like tokens (`mpa`, `%`, `nm`) are
/// fine. **Any digit is not** — another number between them means the property
/// already has a different value, and this one belongs to something else.
/// The value-first gap: only a unit may separate a number from the property it
/// belongs to (`14`**%** `elongation`, `1140` **MPa** `UTS`). A connective verb
/// there means a new clause began and the number belongs to something else.
fn gap_is_only_unit(gap: &str) -> bool {
    if gap.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    gap.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .all(|token| token.len() <= 4 && !BINDING_CONNECTIVES.contains(&token))
}

fn gap_is_only_connective(gap: &str, subject_n: &str) -> bool {
    gap.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter(|t| !t.is_empty())
        .all(|token| {
            // The subject is allowed even though material names carry digits
            // (Ti-6Al-4V). Checking digits over the whole gap instead of per
            // token rejected every sentence that named its own material.
            if !subject_n.is_empty() && subject_n.contains(token) {
                return true;
            }
            // Any OTHER number between the property and this value means the
            // property already has a different value, and this one belongs to
            // something else -- the batch-id / sample-count family.
            if token.chars().any(|c| c.is_ascii_digit()) {
                return false;
            }
            // Connectives, or unit-ish short tokens such as mpa, gpa, hv, nm.
            BINDING_CONNECTIVES.contains(&token) || token.len() <= 4
        })
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
            // `|` splits a table row into cells. A whole row as one span let
            // the elongation column support a UTS claim (reviewer bypass A4).
            if matches!(b, b'.' | b'!' | b'?' | b';' | b'|') && !is_decimal_point {
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

/// String forms under which a numeric value may legitimately appear in a
/// paper: the plain rendering plus the comma-grouped integer form.
fn number_needles(value: f64) -> Vec<String> {
    let mut out = vec![format!("{value}")];
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
        let sign = if value < 0.0 { "-" } else { "" };
        out.push(format!("{sign}{grouped}"));
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

    // ── Salience hardening: reviewer bypasses A1–A7 ─────────────────────
    //
    // Two independent reviewers stamped all seven blocks below as
    // Ti-6Al-4V / UTS / 1140 MPa at confidence 0.9, evidence_class
    // `research`, against the real library. Each must be refused.

    const SALIENCE_SUBJECT: &str = "Ti-6Al-4V";
    const SALIENCE_OBJECT: &str = "UTS";
    const SALIENCE_VALUE: f64 = 1140.0;

    fn no_support(block: &str) {
        assert!(
            supporting_quote(
                SALIENCE_SUBJECT,
                SALIENCE_OBJECT,
                Some(SALIENCE_VALUE),
                block
            )
            .is_none(),
            "block must NOT support Ti-6Al-4V / UTS / 1140 MPa: {block:?}"
        );
    }

    fn support(block: &str) -> String {
        supporting_quote(
            SALIENCE_SUBJECT,
            SALIENCE_OBJECT,
            Some(SALIENCE_VALUE),
            block,
        )
        .unwrap_or_else(|| panic!("block MUST support Ti-6Al-4V / UTS / 1140 MPa: {block:?}"))
    }

    /// A1: `[1140]` is a citation marker, not a measurement.
    #[test]
    fn a1_citation_marker_digits_are_not_evidence() {
        no_support("Ti-6Al-4V is widely used in aerospace applications [1140].");
    }

    /// A2: 1140 is a batch id; the actual UTS in the sentence is 950.
    #[test]
    fn a2_batch_id_equal_to_the_claim_value_is_not_evidence() {
        no_support("For Ti-6Al-4V, batch 1140 showed a UTS of 950 MPa.");
    }

    /// A3: 1140 belongs to the diameter, not the UTS.
    #[test]
    fn a3_number_bound_to_another_property_is_not_evidence() {
        no_support("The Ti-6Al-4V rods (diameter 1140 um) exhibited a UTS of 950 MPa.");
    }

    /// A4: a table row is not one undifferentiated span; the number sits in
    /// the elongation column, not the UTS column.
    #[test]
    fn a4_table_row_columns_do_not_cross_support() {
        no_support("Ti-6Al-4V | annealed | UTS 950 MPa | elongation 1140");
    }

    /// A5: the subject Ti-6Al-4V appears nowhere in the block.
    #[test]
    fn a5_subject_absent_from_the_block_is_refused() {
        no_support("The UTS of the forged billet was 1140 MPa.");
    }

    /// A6: `1140` must not match as a substring of `11140`.
    #[test]
    fn a6_digit_boundary_blocks_substring_match_inside_11140() {
        no_support("The Ti-6Al-4V ingot id was 11140 and its UTS was 950 MPa.");
    }

    /// A7: `1,140` must not match inside `11,140`.
    #[test]
    fn a7_digit_boundary_blocks_substring_match_inside_11_140() {
        no_support("In total, 11,140 Ti-6Al-4V components were inspected.");
    }

    // ── Round 2: bypasses B1-B3, found by review of the A1-A7 fix ───────
    //
    // The A1-A7 fix required the property to appear BEFORE the number. A
    // reviewer defeated that three ways, all stamping at confidence 0.9.
    // Ordering is not binding.

    /// B1: `uts` hid inside `outputs` — the property test was a raw substring.
    #[test]
    fn b1_property_substring_inside_another_word_is_not_a_mention() {
        no_support("The outputs of Ti-6Al-4V machining were 1140 parts.");
        no_support("Ti-6Al-4V statute limits outputs to 1140 units.");
    }

    /// B2: put the property first and the fabricated number later, and the
    /// ordering rule waves it through — while the REAL value sits between.
    #[test]
    fn b2_property_first_does_not_bind_a_later_unrelated_number() {
        no_support("A UTS of 950 MPa was measured for Ti-6Al-4V batch 1140.");
        no_support("Ti-6Al-4V had a UTS of 950 MPa across 1140 samples.");
        no_support("UTS data for Ti-6Al-4V were collected at wavelength 1140 nm.");
        no_support("Ti-6Al-4V UTS reached 950 MPa (lot 1140, certified).");
        no_support("Ti-6Al-4V UTS was 950 MPa over 1,140 test bars.");
    }

    /// B3: any three words whose initials spell the acronym bound it.
    #[test]
    fn b3_acronym_collision_across_a_clause_boundary_is_refused() {
        no_support("Under thermal stress, Ti-6Al-4V reached 1140 C.");
        no_support("Until tested soundly, Ti-6Al-4V lot 1140 was held.");
    }

    // ── Lossiness the same review caught: real claims the rule dropped ──

    /// "N% elongation" is the standard English form. A property-must-come-
    /// first rule can never support it, which made the rule systematically
    /// lossy for the most common way elongation is reported.
    #[test]
    fn legit_value_before_property_stamps() {
        let quote = supporting_quote(
            "Ti-6Al-4V",
            "elongation",
            Some(14.0),
            "Ti-6Al-4V exhibited 14% elongation at fracture.",
        )
        .expect("value-first phrasing is standard and must be supported");
        assert!(quote.contains("14%"));
    }

    /// Value, unit, then property — also common in tables and captions.
    #[test]
    fn legit_value_unit_property_order_stamps() {
        support("Ti-6Al-4V: 1140 MPa UTS.");
    }

    // ── Legitimate cases that must STILL stamp ──────────────────────────

    /// The property appears under its full name, not the `UTS` acronym:
    /// the number must still bind to it.
    #[test]
    fn legit_full_property_name_stamps_via_acronym_binding() {
        let quote = support("The ultimate tensile strength of Ti-6Al-4V was 1140 MPa.");
        assert!(quote.contains("1140"));
    }

    /// Comma-grouped rendering of the same value.
    #[test]
    fn legit_comma_grouped_value_stamps() {
        let quote = support("The Ti-6Al-4V billet showed a UTS of 1,140 MPa.");
        assert!(quote.contains("1,140"));
    }

    /// Anaphora: the subject names the alloy in the previous sentence, and
    /// the measurement sentence says "the alloy". The subject is required in
    /// the BLOCK, not the same span — that is the chosen rule, and this is
    /// the case it exists for.
    #[test]
    fn legit_anaphora_subject_in_block_not_in_span_stamps() {
        let quote = support("Ti-6Al-4V samples were prepared. The alloy showed a UTS of 1140 MPa.");
        assert_eq!(quote, "The alloy showed a UTS of 1140 MPa.");
    }

    /// Non-numeric fact whose subject and object genuinely co-occur.
    #[test]
    fn legit_non_numeric_cooccurrence_stamps() {
        let block = "The Ti-6Al-4V microstructure contained an alpha-beta phase.";
        assert!(supporting_quote("Ti-6Al-4V", "alpha-beta", None, block).is_some());
    }
}
