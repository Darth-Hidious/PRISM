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

use prism_provenance::{MaterialFact, RepairDisposition, RepairItem};
use serde_json::Value;

use crate::ontologies::Ontology;
use crate::text_extract::{
    DEFAULT_GROUNDING_NUMERIC_TOLERANCE, GroundingPolicy, GroundingRefusal, RejectedFact,
    RejectedSubject, RejectionClass, fact_identity, numeric_fact_grounding, subject_appears,
};

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
    ontology: &dyn Ontology,
) -> Option<RepairDisposition> {
    use RejectionClass::*;
    let disposition = match rejection.class {
        // Unit interpretation requires the active ontology and a reader.
        // Code has no vocabulary-neutral correction to render.
        UnresolvedUnit => None,
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
            rejection, document, text, policy, decided_at, ontology,
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
/// B4 WIRING: the normalization ALSO applies
/// [`unwrap_soft_line_breaks`](crate::text_extract::unwrap_soft_line_breaks)
/// BEFORE typography folding — a subject hyphen-wrapped across PDF lines
/// ("Ti-6Al-\n4V") is a typesetting artifact, and without the unambiguous
/// hyphen-wrap join such a fact could never ground through this tier even
/// though the document plainly names the subject. The join's signature is
/// deliberately closed (alphanumeric-hyphen-newline-alphanumeric); every
/// other line boundary stays a record boundary.
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
    ontology: &dyn Ontology,
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
    // B4: unwrap FIRST (it reads raw line structure; typography folding
    // would destroy the hyphen-wrap signature), then fold.
    let normalized_text = normalize_typography(&crate::text_extract::unwrap_soft_line_breaks(text));
    let normalized_subject = normalize_typography(&fact.subject);
    if !subject_appears(&normalized_subject, &normalized_text) {
        return withdraw(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            "the document does not name the subject even after deterministic typographic \
             normalization (hyphen-wrap joins, Unicode dashes, whitespace, case); the \
             judgement stands"
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
            "the subject appears once typography is normalized (hyphen-wrap joins included), \
             but a value-less \
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
    match numeric_fact_grounding(&probe, &normalized_text, grounding, ontology) {
        Ok(evidence_span) => accept(
            rejection,
            document,
            RULE_SUBJECT_NORMALIZATION,
            fact,
            evidence_span,
            "the subject appears once soft-wrap joins, Unicode dashes and whitespace \
             are normalized, and \
             the fact passes the full grounding gate on the normalized text"
                .to_string(),
            decided_at,
        ),
        Err(refusal) => {
            // CONTRACT CHANGE (de-hardcoding): a guarded refusal persists the
            // guard name and the exact examined span as the ledger's
            // evidence — a note the re-checking model can act on, not prose.
            let evidence = match &refusal {
                GroundingRefusal::Guarded { span, .. } => Some(span.clone()),
                GroundingRefusal::Unsupported(_) => None,
            };
            withdraw_with_evidence(
                rejection,
                document,
                RULE_SUBJECT_NORMALIZATION,
                format!(
                    "the subject appears once typography is normalized (hyphen-wrap joins \
                     included), but the fact still \
                     fails the grounding gate: {refusal}"
                ),
                evidence,
                decided_at,
            )
        }
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
    withdraw_with_evidence(rejection, document, rule, reason, None, decided_at)
}

/// A withdraw that also persists the matcher's evidence — the NAMED guard
/// and the EXACT span it examined. CONTRACT CHANGE (de-hardcoding): this is
/// the evidence a re-checking model needs; it rides the ledger's existing
/// `evidence` column instead of being paraphrased into prose and lost.
fn withdraw_with_evidence(
    rejection: &RejectedFact,
    document: &str,
    rule: &str,
    reason: String,
    evidence: Option<String>,
    decided_at: f64,
) -> RepairDisposition {
    RepairDisposition {
        item_id: repair_item_id(document, rejection),
        attempt: 0,
        document: document.to_string(),
        class: rejection.class.as_str().to_string(),
        outcome: "withdraw".to_string(),
        corrected_json: None,
        evidence,
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

/// Deterministic typographic normalization for the subject re-check:
/// Unicode hyphens/dashes/minus to ASCII `-`, and horizontal whitespace
/// runs to one space — WITHIN each line. Line boundaries are provenance
/// boundaries in the grounding gate and are preserved, so normalization
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
            verification: None,
            verification_reason: None,
        }
    }

    #[test]
    fn unresolved_units_are_deferred_without_a_rust_vocabulary() {
        // CONTRACT CHANGE: all deterministic spelling/kind repair tests were
        // removed with their tables. Code records no unit verdict; the queued
        // reader receives source spans and preserves an exact term.
        let rejection = raw_speed_rejection("QUDT:MM-PER-S");
        assert!(
            dispose(
                &rejection,
                DOC,
                "source",
                &RepairPolicy::default(),
                NOW,
                &crate::ontologies::EmmoOntology
            )
            .is_none()
        );
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
            let disposition = dispose(
                &rejection,
                DOC,
                text,
                &RepairPolicy::default(),
                NOW,
                &crate::ontologies::EmmoOntology,
            )
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
        let disposition = dispose(
            &rejection,
            DOC,
            text,
            &RepairPolicy::default(),
            NOW,
            &crate::ontologies::EmmoOntology,
        )
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
                &crate::ontologies::EmmoOntology,
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
        // CONTRACT CHANGE: the grounding tier matches a supplied exact term;
        // it no longer translates a paper spelling through a Rust table.
        let text = "The Ti\u{2013}6Al\u{2013}4V specimens exhibited an ultimate tensile \
                    strength of 1140 MPa at room temperature.";
        let fact = converted_fact(
            "Ti-6Al-4V",
            "ultimate tensile strength",
            Some(1140.0),
            Some("MPa"),
        );
        // The original gate really does refuse this subject — the repair is
        // re-checking a genuine refusal, not a fabricated one.
        assert!(!subject_appears("Ti-6Al-4V", text));
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::SubjectNotNamed,
            detail: "the document never names that subject".into(),
        };
        let disposition = dispose(
            &rejection,
            DOC,
            text,
            &RepairPolicy::default(),
            NOW,
            &crate::ontologies::EmmoOntology,
        )
        .expect("subject normalization is a code decision");
        assert_eq!(disposition.outcome, "accept", "{}", disposition.reason);
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

    /// B4 WIRING: a subject hyphen-wrapped across PDF lines ("Ti-6Al-\n4V")
    /// could never ground through the repair tier before — the join the
    /// extraction-side helper was built for was never applied here. The
    /// unambiguous hyphen-wrap signature now joins in this tier's
    /// normalization, and the fact is accepted with fields frozen.
    #[test]
    fn subject_normalization_joins_a_hyphen_wrapped_subject() {
        let text = "The Ti-6Al-\n4V specimens exhibited an ultimate tensile \
                    strength of 1140 MPa at room temperature.";
        let fact = converted_fact(
            "Ti-6Al-4V",
            "ultimate tensile strength",
            Some(1140.0),
            Some("MPa"),
        );
        // The original gate really does refuse this subject on the raw
        // text — the repair is re-checking a genuine refusal.
        assert!(!subject_appears("Ti-6Al-4V", text));
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::SubjectNotNamed,
            detail: "the document never names that subject".into(),
        };
        let disposition = dispose(
            &rejection,
            DOC,
            text,
            &RepairPolicy::default(),
            NOW,
            &crate::ontologies::EmmoOntology,
        )
        .expect("subject normalization is a code decision");
        assert_eq!(disposition.outcome, "accept", "{}", disposition.reason);
        let corrected: MaterialFact =
            serde_json::from_str(disposition.corrected_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            corrected.subject, "Ti-6Al-4V",
            "the stored subject is frozen"
        );
        assert!(
            disposition
                .evidence
                .as_deref()
                .is_some_and(|span| span.contains("1140 MPa")),
            "{:?}",
            disposition.evidence
        );
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
        let disposition = dispose(
            &absent,
            DOC,
            text,
            &policy,
            NOW,
            &crate::ontologies::EmmoOntology,
        )
        .expect("always a code decision");
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
        let disposition = dispose(
            &assertion,
            DOC,
            text,
            &policy,
            NOW,
            &crate::ontologies::EmmoOntology,
        )
        .expect("always a code decision");
        assert_eq!(disposition.outcome, "withdraw");
        assert!(
            disposition.reason.contains("never returns to a model"),
            "{}",
            disposition.reason
        );
    }

    /// CONTRACT CHANGE (de-hardcoding): when the re-check's grounding gate
    /// refuses every candidate occurrence, the withdraw persists the NAMED
    /// guard and the EXACT span examined on the ledger row — the evidence a
    /// re-checking reader needs, instead of a paraphrase that loses both.
    #[test]
    fn a_guarded_grounding_refusal_persists_the_guard_and_span_as_evidence() {
        // The en-dash subject normalizes, but the value's only occurrence
        // sits inside a citation marker — the Citation guard refuses it.
        let text = "The Ti\u{2013}6Al\u{2013}4V strength was reported [1140] for a \
                    related alloy.";
        let fact = converted_fact("Ti-6Al-4V", "strength", Some(1140.0), Some("MPa"));
        assert!(!subject_appears("Ti-6Al-4V", text));
        let rejection = RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::SubjectNotNamed,
            detail: "the document never names that subject".into(),
        };
        let disposition = dispose(
            &rejection,
            DOC,
            text,
            &RepairPolicy::default(),
            NOW,
            &crate::ontologies::EmmoOntology,
        )
        .expect("subject normalization is a code decision");
        assert_eq!(disposition.outcome, "withdraw", "{}", disposition.reason);
        // The guard's name survives into the reason...
        assert!(
            disposition.reason.contains("Citation"),
            "{}",
            disposition.reason
        );
        // ...and the exact examined span rides the ledger's evidence column.
        let evidence = disposition
            .evidence
            .expect("a guarded refusal keeps its span");
        assert!(evidence.contains("[1140]"), "{evidence}");
        assert!(evidence.contains("strength"), "{evidence}");
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
            &crate::ontologies::EmmoOntology,
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
                    NOW,
                    &crate::ontologies::EmmoOntology,
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
