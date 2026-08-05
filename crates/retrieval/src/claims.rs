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
///
/// Shared containment comparison: the papers pipeline uses it against the
/// cited block, and the local-file ingest pipeline (`prism-ingest`) uses it
/// against the source document text before anything reaches the provenance
/// store. Both callers rely on this one implementation — behaviour must not
/// diverge between them.
pub fn quote_in_block(quote: &str, block_text: &str) -> bool {
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
///   not enough);
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
                number_needles(v).iter().any(|n| hay.contains(&n[..]))
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
}
