//! Code-tier dispositions for the repair queue: every refused fact a
//! DETERMINISTIC rule can decide, decided with ZERO model calls.
//!
//! Phase 1 refuses facts and records why ([`RejectedFact`]); this module is
//! the first thing that looks at those refusals. Each tier takes one
//! rejection and returns `Some(RepairDisposition)` when code alone can
//! render the decision, or `None` when the item must be queued for the
//! model tier. Every disposition here carries `dispositioner:
//! "code:<rule>"`, and every ACCEPT carries the verbatim span that
//! justified it — code never accepts on its own authority, only on the
//! document's.
//!
//! The anti-ratchet rule ([`RejectionClass::judgement_was_rendered`])
//! is enforced HERE, in code, not by caller discipline: a rejection whose
//! judgement was rendered can never come out of [`dispose`] as `None`
//! (queue-for-model), and [`queue_item`] refuses to construct a queue item
//! for one. Re-asking a rendered judgement keeps every "yes" and re-rolls
//! every "no" — sampling noise ratcheting into acceptances.

use std::collections::BTreeSet;

use prism_provenance::{
    EvidenceSource, MaterialFact, RepairDisposition, RepairItem, evidence_for_result,
};
use serde_json::Value;

use crate::qudt_units::{property_quantity_kind, unit_quantity_kind};
use crate::text_extract::{
    DEFAULT_GROUNDING_NUMERIC_TOLERANCE, GroundingPolicy, RejectedFact, RejectedSubject,
    RejectionClass, convert_fact, fact_identity, numeric_fact_grounding, sentence_spans,
    subject_appears,
};

/// Tier A: the corrected unit was read off the document, adjacent to the
/// fact's value, and the corrected fact passed the full grounding gate.
pub const RULE_UNIT_RE_RESOLUTION: &str = "unit-re-resolution";
/// The document's own printed unit spelling resolves to nothing — the
/// maintainer feedback loop. Code never bridges a vocabulary gap by fiat.
pub const RULE_VOCABULARY_GAP: &str = "vocabulary-gap";
/// The value appears nowhere in the document in any rendered form: the
/// number is the extractor's invention, withdrawn with zero calls.
pub const RULE_NO_NEAR_MISS: &str = "no-near-miss";
/// A deterministic grounding search already examined every span and its
/// judgement stands; re-asking would be persuasion, not repair.
pub const RULE_GROUNDING_STANDS: &str = "grounding-stands";
/// Subject re-checked under deterministic typographic normalization only
/// (Unicode dashes, whitespace, case). Never a model call, ever.
pub const RULE_SUBJECT_NORMALIZATION: &str = "subject-normalization";
/// A semantic review verdict was rendered; it is final on this path.
pub const RULE_REVIEW_VERDICT_FINAL: &str = "review-verdict-final";
/// The operator's policy deferred the question; not an error, and not a
/// model's problem.
pub const RULE_POLICY_DEFERRED: &str = "policy-deferred";

/// Declared tunables for the code tiers.
///
/// A limit that lives in a policy struct is visible, documented and
/// overridable; one that lives in a literal is a surprise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepairPolicy {
    /// Relative numeric tolerance used when locating a fact's value in the
    /// document — same meaning and same default as
    /// [`GroundingPolicy::numeric_tolerance`], so a value the grounding
    /// gate would have matched is the value the repair tiers look for.
    pub numeric_tolerance: f64,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            numeric_tolerance: DEFAULT_GROUNDING_NUMERIC_TOLERANCE,
        }
    }
}

/// Stable identity of one REFUSAL: the document, the fact's identity
/// (subject/predicate/object plus value and claimed unit), and the
/// rejection class. Two windows of one document re-reporting the same
/// refusal produce the same id; the same fact refused for a different
/// reason is a different item.
#[must_use]
pub fn repair_item_id(document: &str, rejection: &RejectedFact) -> String {
    let identity = match &rejection.subject {
        RejectedSubject::Converted(fact) => format!(
            "'{} {} {}' value={:?} unit={:?}",
            fact.subject,
            fact.predicate,
            fact.object,
            fact.value,
            fact.unit.as_ref().map(|unit| unit.as_str()),
        ),
        RejectedSubject::Raw(raw) => format!(
            "{} value={:?} unit={:?}",
            fact_identity(raw),
            raw.get("value").and_then(Value::as_f64),
            raw.get("unit").and_then(Value::as_str),
        ),
    };
    format!("{document}|{identity}|{}", rejection.class.as_str())
}

/// Build the queue item for a rejection the code tiers could not decide.
///
/// PANICS if the rejection's judgement was rendered: such an item must
/// never reach a model, and refusing to construct it makes the rule
/// structural rather than conventional.
#[must_use]
pub fn queue_item(
    rejection: &RejectedFact,
    document: &str,
    tenant: &str,
    enqueued_at: f64,
) -> RepairItem {
    assert!(
        !rejection.class.judgement_was_rendered(),
        "BUG: attempted to queue a rendered judgement ({}) for a model — \
         re-asking a rendered judgement is persuasion, not repair",
        rejection.class.as_str()
    );
    let subject_json = match &rejection.subject {
        RejectedSubject::Converted(fact) => {
            serde_json::to_string(fact).expect("a MaterialFact that was constructed serializes")
        }
        RejectedSubject::Raw(raw) => raw.to_string(),
    };
    RepairItem {
        item_id: repair_item_id(document, rejection),
        document: document.to_string(),
        tenant: tenant.to_string(),
        class: rejection.class.as_str().to_string(),
        subject_json,
        detail: rejection.detail.clone(),
        enqueued_at,
        attempts: 0,
    }
}

/// Run the code tiers over one rejection.
///
/// `Some` is a decision code rendered alone (record it in the ledger);
/// `None` means the item must be queued for the model tier — and the
/// anti-ratchet invariant is asserted here: `None` is only ever returned
/// for classes whose judgement was NOT rendered.
#[must_use]
pub fn dispose(
    rejection: &RejectedFact,
    document: &str,
    text: &str,
    policy: &RepairPolicy,
    decided_at: f64,
) -> Option<RepairDisposition> {
    use RejectionClass::*;
    let disposition = match rejection.class {
        UnresolvedUnit => tier_unit_re_resolution(rejection, document, text, policy, decided_at),
        // Disagreement between extraction passes is a statement about the
        // MODEL's consistency, not about the document. No code tier can
        // settle it — deciding would mean re-running extraction, which is a
        // model call, so it queues for the model tier rather than being
        // withdrawn on code's authority.
        SampleDisagreement => None,
        NumericUnsupported => Some(tier_numeric_near_miss(
            rejection, document, text, policy, decided_at,
        )),
        SubjectNotNamed => Some(tier_subject_normalization(
            rejection, document, text, policy, decided_at,
        )),
        ReviewDenied | ReviewUncertain => {
            Some(tier_review_verdict_final(rejection, document, decided_at))
        }
        PolicyDeferred => Some(withdraw(
            rejection,
            document,
            RULE_POLICY_DEFERRED,
            "policy-deferred: the operator's grounding policy forbade storing value-less \
             assertions without model review; this is a recorded deferral, not an error — \
             re-ingesting under a review-enabled policy re-judges it"
                .to_string(),
            decided_at,
        )),
        MalformedShape | ValuelessWithUnit | ReviewMissing => None,
    };
    // The anti-ratchet invariant, enforced where the decision is made.
    assert!(
        disposition.is_some() || !rejection.class.judgement_was_rendered(),
        "BUG: a rendered judgement ({}) was left for a model",
        rejection.class.as_str()
    );
    disposition
}

/// Tier A — unit re-resolution, the deterministic repair for
/// [`RejectionClass::UnresolvedUnit`].
///
/// Find the fact's value in the document (through the same evidential
/// numeric matching the grounding gate uses — a citation or range endpoint
/// cannot donate its neighbourhood), read the unit printed immediately
/// after it, and resolve THAT. The model's claimed spelling never picks
/// the identifier: the document does, which is what makes two different
/// invented spellings converge on one stored identifier.
///
/// The quantity-kind guard is STRICT: a candidate is accepted only when
/// the property's stated kind and the printed unit's kind are BOTH known
/// and equal. A resolvable-but-wrong adjacent token ("30 s" next to a scan
/// speed) would otherwise store a falsehood with a span attached — the
/// worst outcome this design exists to prevent. Properties and units
/// outside the kind vocabulary therefore queue for the model tier instead
/// of being guessed at.
///
/// An accept additionally re-runs the FULL grounding gate on the corrected
/// fact (`subject_appears` + [`numeric_fact_grounding`], the same
/// functions Phase 1 runs), so a repaired fact is held to exactly the bar
/// a normally-admitted fact met, and the recorded evidence is the span
/// that gate returned.
fn tier_unit_re_resolution(
    rejection: &RejectedFact,
    document: &str,
    text: &str,
    policy: &RepairPolicy,
    decided_at: f64,
) -> Option<RepairDisposition> {
    // UnresolvedUnit is minted at conversion time, so the subject is the
    // raw extraction. Anything else is unexpected — leave it for a model.
    let RejectedSubject::Raw(raw) = &rejection.subject else {
        return None;
    };
    let subject = raw.get("subject").and_then(Value::as_str)?;
    let object = raw.get("object").and_then(Value::as_str)?;
    // A unit on a value-less fact is a contradictory shape; there is no
    // value to find a printed unit next to.
    let value = raw.get("value").and_then(Value::as_f64)?;
    let claimed = raw.get("unit").and_then(Value::as_str).unwrap_or("?");

    let expected_kind = property_quantity_kind(object);
    let mut consistent: Vec<String> = Vec::new();
    let mut gap_spellings: Vec<String> = Vec::new();

    for span in text.lines().flat_map(sentence_spans) {
        // The span must name the PROPERTY, not merely the subject.
        //
        // `evidential_numeric_lexeme_satisfies` binds on subject OR object
        // (`claims.rs`) — a deliberate Phase-1 tradeoff, made with a model in
        // the loop that read the passage. Repair has no model: code accepts
        // on its own authority, so it must demand more, not the same.
        //
        // Without this, a fact about "scan speed" bound to a sentence about
        // the RECOATER — same subject, same number, kind-consistent unit —
        // and was stored as evidenced. The paper stated no scan speed at all.
        // A falsehood carrying a genuine verbatim quote is worse than a
        // dropped fact, because it looks verified. Found by adversarial
        // review; `a_span_naming_only_the_subject_cannot_repair_a_different_property`
        // fails if this guard is removed.
        if !object.trim().is_empty() && !span_names_the_property(span, object) {
            continue;
        }
        // The callback always returns false: this is a COLLECTING pass over
        // every evidential occurrence, not an accept-first search — the
        // boolean result is therefore always false and carries nothing.
        let _ = prism_retrieval::claims::evidential_numeric_lexeme_satisfies(
            subject,
            object,
            value,
            span,
            policy.numeric_tolerance,
            |hay, range| {
                match prism_provenance::units::span_value_resolved_adjacent_unit(hay, range.end) {
                    Some(unit) => {
                        let kind_established = matches!(
                            (expected_kind, unit_quantity_kind(unit.as_str())),
                            (Some(expected), Some(printed)) if expected == printed
                        );
                        if kind_established {
                            consistent.push(unit.as_str().to_string());
                        }
                    }
                    None => {
                        if let Some(spelling) = adjacent_spelling(hay, range.end, span) {
                            gap_spellings.push(spelling);
                        }
                    }
                }
                false
            },
        );
    }

    let identifiers: BTreeSet<&str> = consistent.iter().map(String::as_str).collect();
    match identifiers.len() {
        1 => {
            let identifier = identifiers
                .first()
                .expect("len() == 1 guarantees a first element");
            let mut corrected_raw = raw.as_ref().clone();
            corrected_raw["unit"] = Value::String((*identifier).to_string());
            // The corrected fact must clear the SAME gates Phase 1 applies —
            // conversion, the literature evidence cap, subject presence and
            // full numeric grounding — or code has no business accepting it.
            let Ok(mut corrected) = convert_fact(corrected_raw) else {
                return None;
            };
            corrected.evidence_class = evidence_for_result(
                EvidenceSource::LiteratureExtraction,
                [corrected.evidence_class],
            );
            if !subject_appears(&corrected.subject, text) {
                return None;
            }
            let grounding = GroundingPolicy {
                numeric_tolerance: policy.numeric_tolerance,
                ..Default::default()
            };
            let Ok(evidence_span) = numeric_fact_grounding(&corrected, text, grounding) else {
                return None;
            };
            Some(accept(
                rejection,
                document,
                RULE_UNIT_RE_RESOLUTION,
                &corrected,
                evidence_span,
                format!(
                    "the extractor's unit {claimed:?} resolves to nothing; the document \
                     prints the value with a unit resolving to {identifier}, and the \
                     corrected fact passes the full grounding gate"
                ),
                decided_at,
            ))
        }
        0 if !gap_spellings.is_empty() => Some(withdraw(
            rejection,
            document,
            RULE_VOCABULARY_GAP,
            format!("vocabulary-gap:{}", gap_spellings[0]),
            decided_at,
        )),
        0 if !prism_retrieval::claims::numeric_value_appears(
            value,
            text,
            policy.numeric_tolerance,
        ) =>
        {
            Some(withdraw(
                rejection,
                document,
                RULE_NO_NEAR_MISS,
                format!(
                    "no-near-miss: value {value} appears nowhere in the document in any \
                     rendered form — the number is the extractor's invention, and no \
                     model call can change what the document does not say"
                ),
                decided_at,
            ))
        }
        // Ambiguous (two different printed units beside equal values), a
        // kind the guard could not establish, or a bare value with nothing
        // printed after it: code cannot decide safely. UnresolvedUnit is an
        // unrendered class, so the model tier may look.
        _ => None,
    }
}

/// Near-miss filter for [`RejectionClass::NumericUnsupported`] — a RENDERED
/// judgement, so the outcome is always a final code decision, never a
/// model call.
fn tier_numeric_near_miss(
    rejection: &RejectedFact,
    document: &str,
    text: &str,
    policy: &RepairPolicy,
    decided_at: f64,
) -> RepairDisposition {
    let value = match &rejection.subject {
        RejectedSubject::Converted(fact) => fact.value,
        RejectedSubject::Raw(raw) => raw.get("value").and_then(Value::as_f64),
    };
    match value {
        Some(value)
            if !prism_retrieval::claims::numeric_value_appears(
                value,
                text,
                policy.numeric_tolerance,
            ) =>
        {
            withdraw(
                rejection,
                document,
                RULE_NO_NEAR_MISS,
                format!(
                    "no-near-miss: value {value} appears nowhere in the document in any \
                     rendered form — the number is the extractor's invention, and no \
                     model call can change what the document does not say"
                ),
                decided_at,
            )
        }
        Some(value) => withdraw(
            rejection,
            document,
            RULE_GROUNDING_STANDS,
            format!(
                "a rendered form of value {value} occurs in the document, but the \
                 deterministic grounding search already examined every span and found \
                 none carrying it with its subject, unit and conditions; the judgement \
                 stands — {}",
                rejection.detail
            ),
            decided_at,
        ),
        // A value-less assertion refused for unsupported CONDITIONS: the
        // deterministic search over every span already failed.
        None => withdraw(
            rejection,
            document,
            RULE_GROUNDING_STANDS,
            format!(
                "deterministic condition grounding searched every span and failed; the \
                 judgement stands — {}",
                rejection.detail
            ),
            decided_at,
        ),
    }
}

/// [`RejectionClass::SubjectNotNamed`]: deterministic typographic
/// normalization only (Unicode dashes, whitespace; case was already folded
/// by the original check), then the re-check — never a model call, ever.
///
/// A re-check that passes does NOT re-admit the fact on subject presence
/// alone: the original pipeline stopped at the subject gate, so the value,
/// unit and conditions were never grounded. The fact must pass the full
/// grounding gate on the normalized text before code accepts it. The
/// matching PROBE uses normalized subject/object spellings; the stored
/// fact's fields are frozen — normalization is for matching, never for
/// rewriting.
fn tier_subject_normalization(
    rejection: &RejectedFact,
    document: &str,
    text: &str,
    policy: &RepairPolicy,
    decided_at: f64,
) -> RepairDisposition {
    let RejectedSubject::Converted(fact) = &rejection.subject else {
        return withdraw(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            format!(
                "the refusal carries no converted fact to re-check; the subject-not-named \
                 judgement stands — {}",
                rejection.detail
            ),
            decided_at,
        );
    };
    let normalized_text = normalize_typography(text);
    let normalized_subject = normalize_typography(&fact.subject);
    if !subject_appears(&normalized_subject, &normalized_text) {
        return withdraw(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            "the document does not name the subject even after deterministic typographic \
             normalization (Unicode dashes, whitespace, case); the judgement stands"
                .to_string(),
            decided_at,
        );
    }
    if fact.value.is_none() {
        // The subject was a typographic false positive, but a value-less
        // assertion still needs a semantic polarity review — a model call,
        // barred for this rendered class, ever. A versioned gate change
        // plus re-ingest re-judges the whole corpus symmetrically.
        return withdraw(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            "the subject appears once typography is normalized, but a value-less \
             assertion needs a semantic polarity review and this rendered class never \
             returns to a model; a versioned gate change plus re-ingest re-judges it"
                .to_string(),
            decided_at,
        );
    }
    let mut probe = (**fact).clone();
    probe.subject = normalized_subject;
    probe.object = normalize_typography(&fact.object);
    let grounding = GroundingPolicy {
        numeric_tolerance: policy.numeric_tolerance,
        ..Default::default()
    };
    match numeric_fact_grounding(&probe, &normalized_text, grounding) {
        Ok(evidence_span) => accept(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            fact,
            evidence_span,
            "the subject appears once Unicode dashes and whitespace are normalized, and \
             the fact passes the full grounding gate on the normalized text"
                .to_string(),
            decided_at,
        ),
        Err(reason) => withdraw(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            format!(
                "the subject appears once typography is normalized, but the fact still \
                 fails the grounding gate: {reason}"
            ),
            decided_at,
        ),
    }
}

/// [`RejectionClass::ReviewDenied`] / [`RejectionClass::ReviewUncertain`]:
/// the verdict was rendered and is final on this path — recorded with its
/// sub-class, never queued. The finality is asserted against
/// [`RejectionClass::judgement_was_rendered`] rather than duplicated here.
fn tier_review_verdict_final(
    rejection: &RejectedFact,
    document: &str,
    decided_at: f64,
) -> RepairDisposition {
    assert!(
        rejection.class.judgement_was_rendered(),
        "BUG: {} reached the final-verdict tier without a rendered judgement",
        rejection.class.as_str()
    );
    withdraw(
        rejection,
        document,
        RULE_REVIEW_VERDICT_FINAL,
        format!(
            "{}: a semantic review verdict was rendered and is final — re-asking would \
             keep every yes and re-roll every no; {}",
            rejection.class.as_str(),
            rejection.detail
        ),
        decided_at,
    )
}

fn withdraw(
    rejection: &RejectedFact,
    document: &str,
    rule: &str,
    reason: String,
    decided_at: f64,
) -> RepairDisposition {
    RepairDisposition {
        item_id: repair_item_id(document, rejection),
        attempt: 0,
        document: document.to_string(),
        class: rejection.class.as_str().to_string(),
        outcome: "withdraw".to_string(),
        corrected_json: None,
        evidence: None,
        reason,
        dispositioner: format!("code:{rule}"),
        decided_at,
    }
}

/// Build an ACCEPT disposition — and enforce the field freeze in code: a
/// repair NEVER changes subject, predicate or object. A tier that tries is
/// a bug, and it dies here rather than reaching the ledger.
fn accept(
    rejection: &RejectedFact,
    document: &str,
    rule: &str,
    corrected: &MaterialFact,
    evidence_span: String,
    reason: String,
    decided_at: f64,
) -> RepairDisposition {
    let (subject, predicate, object) = match &rejection.subject {
        RejectedSubject::Converted(fact) => (
            Some(fact.subject.as_str()),
            Some(fact.predicate.as_str()),
            Some(fact.object.as_str()),
        ),
        RejectedSubject::Raw(raw) => (
            raw.get("subject").and_then(Value::as_str),
            raw.get("predicate").and_then(Value::as_str),
            raw.get("object").and_then(Value::as_str),
        ),
    };
    assert_eq!(
        subject,
        Some(corrected.subject.as_str()),
        "FIELD FREEZE: a repair changed the subject"
    );
    assert_eq!(
        predicate,
        Some(corrected.predicate.as_str()),
        "FIELD FREEZE: a repair changed the predicate"
    );
    assert_eq!(
        object,
        Some(corrected.object.as_str()),
        "FIELD FREEZE: a repair changed the object"
    );
    RepairDisposition {
        item_id: repair_item_id(document, rejection),
        attempt: 0,
        document: document.to_string(),
        class: rejection.class.as_str().to_string(),
        outcome: "accept".to_string(),
        corrected_json: Some(
            serde_json::to_string(corrected)
                .expect("a MaterialFact that was constructed serializes"),
        ),
        evidence: Some(evidence_span),
        reason,
        dispositioner: format!("code:{rule}"),
        decided_at,
    }
}

/// The token printed immediately after a value that did NOT resolve as a
/// unit — the vocabulary-gap report. Read from the normalized haystack
/// (only whitespace may separate value and token), then mapped back to the
/// document's own casing where the verbatim span allows it.
fn adjacent_spelling(hay: &str, value_end: usize, verbatim_span: &str) -> Option<String> {
    let after = hay.get(value_end..)?;
    let offset = after.find(|c: char| !c.is_whitespace())?;
    let token = after[offset..].split_whitespace().next()?;
    let token = token
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .trim_start_matches('(')
        .trim_end_matches(')');
    // A following number or bare punctuation is not a unit spelling.
    if token.is_empty() || !token.chars().any(char::is_alphabetic) {
        return None;
    }
    let lower = verbatim_span.to_lowercase();
    if lower.len() == verbatim_span.len()
        && let Some(position) = lower.find(token)
        && verbatim_span.is_char_boundary(position)
        && verbatim_span.is_char_boundary(position + token.len())
    {
        return Some(verbatim_span[position..position + token.len()].to_string());
    }
    Some(token.to_string())
}

/// Deterministic typographic normalization for the subject re-check:
/// Unicode hyphens/dashes/minus to ASCII `-`, and horizontal whitespace
/// runs to one space — WITHIN each line. Line boundaries are provenance
/// boundaries in the grounding gate and are preserved, so normalization
/// Whether a span actually names the PROPERTY the fact is about.
///
/// Repair accepts on code's own authority, with no model reading the passage,
/// so it demands more than Phase-1 grounding does: the span must contain the
/// property name, not merely the subject. Deliberately conservative — a
/// paper writing "scanning speed" where the fact says "scan speed" fails
/// this, and the item is QUEUED for the model tier rather than accepted.
/// Failing safe here costs recall; failing open stores a falsehood carrying a
/// verbatim quote, which is worse.
///
/// Matching mirrors the grounding scanner's own normalisation: lowercase and
/// whitespace-collapsed, so line wrapping and double spaces in extracted PDF
/// text do not hide a property that is genuinely present.
fn span_names_the_property(span: &str, object: &str) -> bool {
    fn folded(text: &str) -> String {
        text.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
    let needle = folded(object);
    if needle.is_empty() {
        return false;
    }
    folded(span).contains(&needle)
}

/// can never merge one record's value with another record's condition.
/// Case is not touched here; the term matcher already folds it.
fn normalize_typography(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mapped: String = line
                .chars()
                .map(|c| match c {
                    '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
                    c if c.is_whitespace() => ' ',
                    c => c,
                })
                .collect();
            mapped.split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "doc:test-paper.pdf";
    const NOW: f64 = 1_754_000_000.0;

    fn raw_speed_rejection(claimed_unit: &str) -> RejectedFact {
        let raw = serde_json::json!({
            "subject": "AlSi10Mg", "predicate": "has_measurement",
            "object": "scan speed", "value": 1250.0, "unit": claimed_unit,
            "kind": "measurement", "confidence": 0.9,
            "evidence_class": "research", "conditions": []
        });
        RejectedFact {
            subject: RejectedSubject::Raw(Box::new(raw)),
            class: RejectionClass::UnresolvedUnit,
            detail: format!("test: unit {claimed_unit:?} did not resolve"),
        }
    }

    fn converted_fact(
        subject: &str,
        object: &str,
        value: Option<f64>,
        unit: Option<&str>,
    ) -> MaterialFact {
        MaterialFact {
            subject: subject.into(),
            predicate: "has_measurement".into(),
            object: object.into(),
            value,
            unit: unit.map(|identifier| {
                prism_provenance::QudtUnit::new(identifier).expect("valid test unit")
            }),
            conditions: Vec::new(),
            confidence: Some(0.9),
            kind: value.is_some().then(|| "measurement".to_string()),
            evidence_class: Default::default(),
        }
    }

    /// THE HEADLINE PROPERTY. Two rejections carrying DIFFERENT invented
    /// identifiers for the same underlying unit converge on the SAME stored
    /// identifier, because the document — not the model's claim — picks it.
    ///
    /// This is falsifiable at the obvious shortcut: `resolve_unit`
    /// canonicalises `QUDT:Meter-Per-Second` to `QUDT:M-PER-SEC`, so an
    /// implementation that consults the CLAIMED spelling stores M-PER-SEC
    /// for one input and MilliM-PER-SEC for the other — and this fails.
    #[test]
    fn different_invented_identifiers_converge_on_the_documents_identifier() {
        let text = "The AlSi10Mg parts were built at a scan speed of 1250 mm/s.";
        let policy = RepairPolicy::default();

        let mut stored = Vec::new();
        for claimed in ["QUDT:Meter-Per-Second", "QUDT:MM-PER-S"] {
            let rejection = raw_speed_rejection(claimed);
            let disposition = dispose(&rejection, DOC, text, &policy, NOW)
                .unwrap_or_else(|| panic!("{claimed} must be decided by code"));
            assert_eq!(disposition.outcome, "accept", "{claimed}");
            assert_eq!(disposition.dispositioner, "code:unit-re-resolution");
            let corrected: MaterialFact =
                serde_json::from_str(disposition.corrected_json.as_deref().unwrap()).unwrap();
            // Field freeze held.
            assert_eq!(corrected.subject, "AlSi10Mg");
            assert_eq!(corrected.predicate, "has_measurement");
            assert_eq!(corrected.object, "scan speed");
            // Evidence is the verbatim span that justified the unit.
            let evidence = disposition.evidence.as_deref().unwrap();
            assert!(evidence.contains("1250 mm/s"), "{evidence}");
            stored.push(corrected.unit.unwrap().as_str().to_string());
        }
        assert_eq!(stored[0], stored[1], "one unit, one identity");
        assert_eq!(stored[0], "QUDT:MilliM-PER-SEC");
    }

    /// A resolvable-but-WRONG adjacent token must never be accepted: the
    /// strict quantity-kind guard refuses a printed dwell time ("30 s")
    /// sitting where a scan speed's unit would be. And an adjacent token
    /// the vocabulary cannot resolve at all ("30 day") is the maintainer
    /// feedback loop: withdrawn as a vocabulary gap naming the spelling,
    /// never bridged by fiat.
    /// The kind guard must fail CLOSED when the property's quantity kind is
    /// unknown, not open.
    ///
    /// `a_resolvable_but_wrong_adjacent_token_is_never_accepted` covers the
    /// case where both kinds are known and disagree. It does NOT cover an
    /// unrecognised property, where `property_quantity_kind` returns None —
    /// and that is the wider hole, because a paper may report any property
    /// the table has never seen. Found by mutation: weakening the guard to
    /// "reject only when both kinds are known AND differ" left every repair
    /// test passing.
    /// REPRODUCTION of an adversarial-review finding: Tier A could bind a
    /// fact to the WRONG occurrence of its number and accept it with a real,
    /// verbatim quote as evidence.
    ///
    /// The grounding predicate binds on subject OR object appearing in the
    /// span (`claims.rs`), so a sentence naming only the SUBJECT satisfies it
    /// — even when that sentence is about a different property entirely. For
    /// Phase 1 that is a deliberate tradeoff with a model in the loop. For
    /// repair it is not: code accepts here on its own authority, so it must
    /// demand that the span names the PROPERTY too.
    ///
    /// Without that, this document yields "AlSi10Mg scan speed = 1250 mm/s"
    /// sourced from a sentence about the RECOATER — a falsehood carrying
    /// genuine evidence, which is worse than dropping the fact.
    #[test]
    fn a_span_naming_only_the_subject_cannot_repair_a_different_property() {
        let policy = RepairPolicy::default();
        let raw = serde_json::json!({
            "subject": "AlSi10Mg", "predicate": "has_measurement",
            "object": "scan speed", "value": 1250.0, "unit": "QUDT:MM-PER-S",
            "kind": "measurement", "evidence_class": "research", "conditions": []
        });
        let rejection = RejectedFact {
            subject: RejectedSubject::Raw(Box::new(raw)),
            class: RejectionClass::UnresolvedUnit,
            detail: "test: unresolved unit".into(),
        };

        // The number and a resolvable, kind-consistent unit are present — but
        // the sentence is about the recoater, and never mentions scan speed.
        // The paper states no scan speed value at all.
        let doc = "For the AlSi10Mg builds the recoater cross-feed speed was \
                   fixed at 1250 mm/s throughout the campaign.";
        let decided = dispose(&rejection, DOC, doc, &policy, NOW);
        if let Some(d) = &decided {
            assert_ne!(
                d.outcome, "accept",
                "the span never names the property; accepting binds the fact to \
                 another quantity and calls it evidenced: {d:?}"
            );
        }
    }

    #[test]
    fn an_unknown_property_kind_is_never_silently_accepted() {
        let policy = RepairPolicy::default();
        // "acoustic damping ratio" is not in the quantity-kind table, so the
        // expected kind is None — nothing can establish consistency.
        assert!(
            crate::qudt_units::property_quantity_kind("acoustic damping ratio").is_none(),
            "test premise: this property must be unknown to the table"
        );
        let raw = serde_json::json!({
            "subject": "AlSi10Mg", "predicate": "has_measurement",
            "object": "acoustic damping ratio", "value": 30.0,
            "unit": "QUDT:INVENTED", "kind": "measurement",
            "evidence_class": "research", "conditions": []
        });
        let rejection = RejectedFact {
            subject: RejectedSubject::Raw(Box::new(raw)),
            class: RejectionClass::UnresolvedUnit,
            detail: "test: unresolved unit".into(),
        };

        // "30 s" resolves to QUDT:SEC. With the property's kind unknown, code
        // CANNOT establish that seconds is right for a damping ratio — so it
        // must not accept. Storing it would attach a verbatim span to a unit
        // nobody verified, which is worse than dropping the fact.
        let doc = "The AlSi10Mg damping measurement settled after 30 s of ring-down.";
        let decided = dispose(&rejection, DOC, doc, &policy, NOW);
        if let Some(d) = &decided {
            assert_ne!(
                d.outcome, "accept",
                "an unknown property kind must never yield a code ACCEPT: {d:?}"
            );
        }
    }

    #[test]
    fn a_resolvable_but_wrong_adjacent_token_is_never_accepted() {
        let policy = RepairPolicy::default();
        let raw = serde_json::json!({
            "subject": "AlSi10Mg", "predicate": "has_measurement",
            "object": "scan speed", "value": 30.0, "unit": "QUDT:MM-PER-S",
            "kind": "measurement", "evidence_class": "research", "conditions": []
        });
        let rejection = RejectedFact {
            subject: RejectedSubject::Raw(Box::new(raw)),
            class: RejectionClass::UnresolvedUnit,
            detail: "test: unresolved unit".into(),
        };

        // "30 s" resolves (QUDT:SEC) but its kind (time) contradicts the
        // property (speed): NOT accepted — queued for the model tier, which
        // is legal for this unrendered class.
        let wrong_kind = "The AlSi10Mg scan speed run paused for 30 s before the next layer.";
        assert!(
            dispose(&rejection, DOC, wrong_kind, &policy, NOW).is_none(),
            "a kind-inconsistent printed unit must not be accepted by code"
        );

        // "30 day" resolves to nothing: vocabulary gap, withdrawn with the
        // document's verbatim spelling — zero model calls.
        let unresolvable = "The AlSi10Mg scan speed run paused for 30 day intervals.";
        let disposition = dispose(&rejection, DOC, unresolvable, &policy, NOW)
            .expect("a vocabulary gap is a code decision");
        assert_eq!(disposition.outcome, "withdraw");
        assert_eq!(disposition.reason, "vocabulary-gap:day");
        assert_eq!(disposition.dispositioner, "code:vocabulary-gap");

        // Two DIFFERENT consistent units beside equal values is ambiguous:
        // code must not pick one, so the item queues.
        let ambiguous = "The AlSi10Mg scan speed was 30 mm/s. \
                         Another AlSi10Mg scan speed was 30 m/s.";
        assert!(
            dispose(&rejection, DOC, ambiguous, &policy, NOW).is_none(),
            "two candidate identifiers must not be resolved by coin flip"
        );
    }

    /// An UnresolvedUnit whose value appears nowhere in the document is the
    /// same invention the near-miss filter withdraws — no model call can
    /// change what the document does not say.
    #[test]
    fn an_unresolved_unit_on_an_invented_value_is_withdrawn_not_queued() {
        let rejection = raw_speed_rejection("QUDT:MM-PER-S");
        let text = "The AlSi10Mg parts were examined for scan speed effects, \
                    described qualitatively.";
        let disposition = dispose(&rejection, DOC, text, &RepairPolicy::default(), NOW)
            .expect("an absent value is a code decision");
        assert_eq!(disposition.outcome, "withdraw");
        assert!(
            disposition.reason.starts_with("no-near-miss"),
            "{}",
            disposition.reason
        );
        assert_eq!(disposition.dispositioner, "code:no-near-miss");
    }

    /// The three invented accuracies (0.935/0.944/0.946): NumericUnsupported
    /// values that appear nowhere in any rendered form are withdrawn
    /// `no-near-miss` — a final code decision, zero model calls, and never
    /// enqueued (the class is a rendered judgement).
    #[test]
    fn invented_accuracies_are_withdrawn_no_near_miss() {
        let text = "The CNN model was evaluated on a held-out split and its accuracy \
                    was described qualitatively.";
        for value in [0.935, 0.944, 0.946] {
            let fact = converted_fact("CNN model", "accuracy", Some(value), Some("QUDT:UNITLESS"));
            let rejection = RejectedFact {
                subject: RejectedSubject::Converted(Box::new(fact)),
                class: RejectionClass::NumericUnsupported,
                detail: format!(
                    "CNN model has_measurement accuracy ({value}): not supported by the \
                     document — no sentence or table row carries value {value} with the \
                     fact's subject or property"
                ),
            };
            let disposition = dispose(&rejection, DOC, text, &RepairPolicy::default(), NOW)
                .expect("a rendered judgement is always a code decision");
            assert_eq!(disposition.outcome, "withdraw", "{value}");
            assert!(
                disposition.reason.starts_with("no-near-miss"),
                "{value}: {}",
                disposition.reason
            );
            assert_eq!(disposition.dispositioner, "code:no-near-miss");
        }
    }

    /// When a rendered form of the value DOES occur, the grounding judgement
    /// still stands (the deterministic search already ran) — withdrawn with
    /// the distinct `grounding-stands` rule so an audit can tell an invented
    /// number from a real one that failed attribution.
    #[test]
    fn a_near_miss_still_withdraws_but_names_the_standing_judgement() {
        let text = "An accuracy of 0.935 was reported by prior work for a different model. \
                    The CNN model was described qualitatively.";
        let fact = converted_fact("CNN model", "accuracy", Some(0.935), Some("QUDT:UNITLESS"));
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::NumericUnsupported,
            detail: "test: numeric grounding failed".into(),
        };
        let disposition = dispose(&rejection, DOC, text, &RepairPolicy::default(), NOW)
            .expect("a rendered judgement is always a code decision");
        assert_eq!(disposition.outcome, "withdraw");
        assert_eq!(disposition.dispositioner, "code:grounding-stands");
        assert!(
            disposition.reason.contains("judgement"),
            "{}",
            disposition.reason
        );
    }

    /// A ReviewDenied item is NEVER enqueued. `dispose` must decide it
    /// (this assertion fails if it ever returns `None`, which is the queue
    /// path), and `queue_item` refuses to construct the item at all.
    #[test]
    fn a_review_denied_item_is_never_enqueued() {
        let fact = converted_fact("Alloy X", "omega", None, None);
        for class in [
            RejectionClass::ReviewDenied,
            RejectionClass::ReviewUncertain,
        ] {
            let rejection = RejectedFact {
                subject: RejectedSubject::Converted(Box::new(fact.clone())),
                class,
                detail: "semantic model review returned Denied".into(),
            };
            let disposition = dispose(
                &rejection,
                DOC,
                "Alloy X showed no omega phase.",
                &RepairPolicy::default(),
                NOW,
            )
            .expect("a rendered review verdict must be decided by code, never queued");
            assert_eq!(disposition.outcome, "withdraw");
            assert_eq!(disposition.dispositioner, "code:review-verdict-final");
            assert!(
                disposition.reason.starts_with(class.as_str()),
                "the sub-class must be recorded: {}",
                disposition.reason
            );
        }
    }

    #[test]
    #[should_panic(expected = "rendered judgement")]
    fn queueing_a_rendered_judgement_panics() {
        let fact = converted_fact("Alloy X", "omega", None, None);
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::ReviewDenied,
            detail: "denied".into(),
        };
        let _ = queue_item(&rejection, DOC, "local", NOW);
    }

    /// SubjectNotNamed: a document that prints `Ti–6Al–4V` with en-dashes
    /// names the subject `Ti-6Al-4V` — deterministic normalization finds
    /// it, the fact passes the full grounding gate, and the ACCEPT keeps
    /// every field frozen (the stored subject is the extractor's, not a
    /// rewritten one).
    #[test]
    fn subject_normalization_readmits_a_dash_variant_with_fields_frozen() {
        let text = "The Ti\u{2013}6Al\u{2013}4V specimens exhibited an ultimate tensile \
                    strength of 1140 MPa at room temperature.";
        let fact = converted_fact(
            "Ti-6Al-4V",
            "ultimate tensile strength",
            Some(1140.0),
            Some("QUDT:MegaPA"),
        );
        // The original gate really does refuse this subject — the repair is
        // re-checking a genuine refusal, not a fabricated one.
        assert!(!subject_appears("Ti-6Al-4V", text));
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::SubjectNotNamed,
            detail: "the document never names that subject".into(),
        };
        let disposition = dispose(&rejection, DOC, text, &RepairPolicy::default(), NOW)
            .expect("subject normalization is a code decision");
        assert_eq!(disposition.outcome, "accept");
        assert_eq!(disposition.dispositioner, "code:subject-normalization");
        let corrected: MaterialFact =
            serde_json::from_str(disposition.corrected_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            corrected.subject, "Ti-6Al-4V",
            "the stored subject is frozen"
        );
        let evidence = disposition.evidence.as_deref().unwrap();
        assert!(evidence.contains("1140 MPa"), "{evidence}");
    }

    /// …and the two ways it stays withdrawn: a subject that is genuinely
    /// absent, and a value-less assertion (whose re-admission would need a
    /// model review — barred for this class, ever).
    #[test]
    fn subject_normalization_withdraws_absence_and_never_calls_a_model() {
        let policy = RepairPolicy::default();
        let absent = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(converted_fact(
                "Inconel 718",
                "UTS",
                Some(950.0),
                Some("QUDT:MegaPA"),
            ))),
            class: RejectionClass::SubjectNotNamed,
            detail: "the document never names that subject".into(),
        };
        let text = "The Ti\u{2013}6Al\u{2013}4V specimens reached a UTS of 950 MPa.";
        let disposition =
            dispose(&absent, DOC, text, &policy, NOW).expect("always a code decision");
        assert_eq!(disposition.outcome, "withdraw");
        assert!(
            disposition.reason.contains("stands"),
            "{}",
            disposition.reason
        );

        // Value-less: even though normalization finds the subject, polarity
        // review is a model call and this class never gets one.
        let assertion = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(converted_fact(
                "Ti-6Al-4V",
                "alpha-beta",
                None,
                None,
            ))),
            class: RejectionClass::SubjectNotNamed,
            detail: "the document never names that subject".into(),
        };
        let text = "The Ti\u{2013}6Al\u{2013}4V specimens showed an alpha-beta structure.";
        let disposition =
            dispose(&assertion, DOC, text, &policy, NOW).expect("always a code decision");
        assert_eq!(disposition.outcome, "withdraw");
        assert!(
            disposition.reason.contains("never returns to a model"),
            "{}",
            disposition.reason
        );
    }

    /// PolicyDeferred is recorded as a deferral — an operator's configured
    /// choice, not an error and not a model's problem.
    #[test]
    fn policy_deferred_is_recorded_without_a_model() {
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(converted_fact(
                "Alloy X", "omega", None, None,
            ))),
            class: RejectionClass::PolicyDeferred,
            detail: "the policy forbids storing value-less assertions without review".into(),
        };
        let disposition = dispose(
            &rejection,
            DOC,
            "Alloy X contained an omega phase.",
            &RepairPolicy::default(),
            NOW,
        )
        .expect("a deferral is recorded, never queued");
        assert_eq!(disposition.outcome, "withdraw");
        assert_eq!(disposition.dispositioner, "code:policy-deferred");
        assert!(
            disposition.reason.contains("not an error"),
            "{}",
            disposition.reason
        );
    }

    /// The classes code cannot decide queue for the model tier — and every
    /// one of them is an UNRENDERED judgement, so queueing is legal and
    /// `queue_item` constructs the item without complaint.
    #[test]
    fn undecidable_classes_queue_without_violating_the_anti_ratchet() {
        let raw = serde_json::json!({
            "subject": "steel", "predicate": "has_measurement", "object": "elongation",
            "value": 4.5, "unit": null, "kind": "measurement",
            "evidence_class": "research", "conditions": []
        });
        let cases = [
            RejectedFact {
                subject: RejectedSubject::Raw(Box::new(raw)),
                class: RejectionClass::MalformedShape,
                detail: "numeric value 4.5 arrived with no unit at all".into(),
            },
            RejectedFact {
                subject: RejectedSubject::Converted(Box::new(converted_fact(
                    "steel",
                    "ferrite",
                    None,
                    Some("QUDT:PERCENT"),
                ))),
                class: RejectionClass::ValuelessWithUnit,
                detail: "a value-less assertion carried a unit".into(),
            },
            RejectedFact {
                subject: RejectedSubject::Converted(Box::new(converted_fact(
                    "steel", "ferrite", None, None,
                ))),
                class: RejectionClass::ReviewMissing,
                detail: "semantic model review returned no verdict".into(),
            },
        ];
        for rejection in &cases {
            assert!(
                dispose(
                    rejection,
                    DOC,
                    "The steel showed ferrite.",
                    &RepairPolicy::default(),
                    NOW
                )
                .is_none(),
                "{} has no code tier",
                rejection.class.as_str()
            );
            let item = queue_item(rejection, DOC, "local", NOW);
            assert_eq!(item.class, rejection.class.as_str());
            assert_eq!(item.document, DOC);
            assert_eq!(item.attempts, 0);
        }
    }

    /// One refusal, one id: the same rejection seen from two overlapping
    /// windows converges, and a different class is a different item.
    #[test]
    fn repair_item_ids_identify_the_refusal_not_the_run() {
        let first = raw_speed_rejection("QUDT:MM-PER-S");
        let second = raw_speed_rejection("QUDT:MM-PER-S");
        assert_eq!(repair_item_id(DOC, &first), repair_item_id(DOC, &second));

        let other_class = RejectedFact {
            class: RejectionClass::MalformedShape,
            ..first.clone()
        };
        assert_ne!(
            repair_item_id(DOC, &first),
            repair_item_id(DOC, &other_class)
        );
        assert_ne!(
            repair_item_id(DOC, &first),
            repair_item_id("doc:other.pdf", &first)
        );
    }
}
