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
/// Support criteria (all case-insensitive, within one sentence/row span):
/// * numeric fact: the value's number appears together with the subject or
///   the object (a bare number could be a citation, so the number alone is
///   not enough). The number must occur as its own token — not as a
///   substring of a longer number and not as a digit inside an alloy
///   designation — and the occurrence must be evidential: a number inside
///   a citation marker `[...]` or immediately after Table/Figure/Ref is a
///   label, not a measurement;
/// * non-numeric fact: both subject and object appear.
#[must_use]
pub fn supporting_quote(
    subject: &str,
    object: &str,
    value: Option<f64>,
    block_text: &str,
) -> Option<String> {
    let subject_n = normalize_for_containment(subject);
    let object_n = normalize_for_containment(object);
    for span in supporting_spans(block_text) {
        let hay = normalize_for_containment(span);
        let supported = match value {
            Some(v) => {
                number_is_evidential(&hay, v)
                    && ((!subject_n.is_empty() && hay.contains(&subject_n))
                        || (!object_n.is_empty() && hay.contains(&object_n)))
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
            if matches!(b, b'.' | b'!' | b'?' | b';') && !is_decimal_point {
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

/// Does `hay` contain the value as an evidential measurement? At least one
/// string form of the value must occur with clean token boundaries and must
/// not be a citation marker or a Table/Figure/Ref label number.
fn number_is_evidential(hay: &str, value: f64) -> bool {
    number_needles(value)
        .iter()
        .any(|needle| evidential_number_occurrence(hay, needle))
}

/// Scan every occurrence of `needle` in `hay` for one that is real evidence.
fn evidential_number_occurrence(hay: &str, needle: &str) -> bool {
    let mut search_from = 0usize;
    while let Some(rel) = hay[search_from..].find(needle) {
        let start = search_from + rel;
        let end = start + needle.len();
        if clean_number_boundary(hay, needle, start, end)
            && !inside_citation_marker(hay, start)
            && !preceding_word_is_label(hay, start)
        {
            return true;
        }
        search_from = start + 1;
    }
    false
}

/// Letters that may begin a unit token glued directly to a number in
/// table and PDF-extracted text where the space was lost: "950MPa",
/// "1073K", "50um" / "50\u{b5}m", "5wt%". Deliberately an allow-list,
/// not every letter: digit-then-letter gluing like "950x" (magnification)
/// or "2e5" (scientific notation) is not number+unit and stays rejected,
/// so 'e' and 'x' are absent on purpose. '\u{b5}' is present because
/// U+00B5 MICRO SIGN is alphabetic.
const UNIT_INITIALS: &[char] = &[
    'a', 'c', 'd', 'f', 'g', 'h', 'j', 'k', 'l', 'm', 'n', 'p', 's', 't', 'u', 'v', 'w', '\u{b5}',
];

/// Token-boundary check: the occurrence must not be adjacent to a digit, to
/// a decimal point that continues it, to a digit-adjacent comma that
/// continues a grouped number ("1,140" is one number, in both directions),
/// or to an alphanumeric. Otherwise "95" matches inside "950", "1.5" inside
/// "11.5", "140" inside "1,140", and the "6" of "Ti-6Al-4V". After the
/// number, a letter from `UNIT_INITIALS` is allowed so glued units
/// ("950MPa") still stamp.
fn clean_number_boundary(hay: &str, needle: &str, start: usize, end: usize) -> bool {
    if let Some(before) = hay[..start].chars().next_back() {
        if before.is_alphanumeric() {
            return false;
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
            if !UNIT_INITIALS.contains(&after) {
                return false;
            }
            // A glued unit letter redeems a number, but never a digit
            // inside a hyphen-joined designation (the "6" of "Ti-6Al-4V",
            // ASCII or en/em dash).
            if let Some(before) = hay[..start].chars().next_back()
                && matches!(before, '-' | '\u{2013}' | '\u{2014}')
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

/// Is the occurrence inside a bracketed citation marker such as `[1140]`,
/// `[11, 12]` or `[11–13]`? Walk back over the digits and separators a
/// citation range may contain; if the first other character is `[`, the
/// number is a citation, not a measurement.
fn inside_citation_marker(hay: &str, start: usize) -> bool {
    let prefix = hay[..start].trim_end_matches(|c: char| {
        c.is_ascii_digit() || matches!(c, ',' | ' ' | '-' | '\u{2013}' | '\u{2014}')
    });
    prefix.ends_with('[')
}

/// Words after which a number is a label, never a measurement.
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
];

/// Does the occurrence sit right after Table/Figure/Ref ("Table 1",
/// "Figure 2", "Ref. 25")? Such a number labels a document object; it is
/// not evidence for a property value.
fn preceding_word_is_label(hay: &str, start: usize) -> bool {
    let prefix = hay[..start].trim_end_matches([' ', '.', ':']);
    let word: String = prefix
        .chars()
        .rev()
        .take_while(|c: &char| c.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    LABEL_WORDS.contains(&word.as_str())
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

        // En-dash positive control: the subject's hyphens do not match en
        // dashes, so the fact survives ONLY through the object arm of the
        // subject-OR-object rule. Do not flip that OR to AND.
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

    /// Fabrication path 3: a citation marker is not evidence. The exact
    /// sentence the extractor prompt uses as its example must never stamp
    /// the example's number.
    #[test]
    fn citation_marker_is_not_support() {
        let block = "Ti-6Al-4V has been studied extensively in prior work [1140].";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 1140.0, block);
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

    /// Fabrication path 4: digits inside alloy designations are not
    /// numeric prose. Alloy names are numeric by convention, so a block
    /// with no measurement must not stamp the digits of the name.
    #[test]
    fn alloy_designation_digits_are_not_support() {
        let block = "The Ti-6Al-4V samples were annealed and examined.";
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 6.0, block);
        assert_dropped_end_to_end("Ti-6Al-4V", "UTS", 4.0, block);
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
}
