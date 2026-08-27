//! Falsifiable tests for the model tier. Each test names the property it
//! defends; the wiremock `.expect(n)` counts prove the one-call-per-item
//! contract (a retry-loop regression either panics on an unexpected
//! request or fails the expectation on server drop).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use prism_llm::{LlmClient, LlmConfig};
use prism_provenance::{
    LocalProvenance, MaterialFact, OntologyClassification, ProvenanceStore, QudtUnit, RepairItem,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::repair::{RepairPolicy, dispose, queue_item};
use crate::text_extract::{RejectedFact, RejectedSubject, RejectionClass};

const DOC: &str = "doc:test-paper.pdf";
const NOW: f64 = 1_754_000_000.0;
const ELONGATION_DOC: &str = "The steel samples reached an elongation of 4.5 % before fracture.";

/// Scripted OpenAI-shape responder. Replies are handed out in order; the
/// last reply repeats once they are exhausted. Every request body is
/// captured so tests can inspect WHAT the model was given (access, not
/// stuffing).
struct ScriptedRepairModel {
    responses: Vec<String>,
    calls: AtomicUsize,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Respond for ScriptedRepairModel {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let body = std::str::from_utf8(&request.body)
            .unwrap_or_default()
            .to_string();
        if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
            // The chat-completions shape: collect every message's content.
            let content: Vec<String> = parsed
                .get("messages")
                .and_then(Value::as_array)
                .map(|messages| {
                    messages
                        .iter()
                        .filter_map(|m| m.get("content").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if let Ok(mut bodies) = self.bodies.lock() {
                bodies.push(content.join("\n"));
            }
        }
        let content = self
            .responses
            .get(index.min(self.responses.len().saturating_sub(1)))
            .cloned()
            .unwrap_or_default();
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop"
            }]
        }))
    }
}

struct RepairHarness {
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
}

/// A mock model endpoint expecting EXACTLY `expected_requests` calls —
/// wiremock fails the test on drop if the worker made more or fewer.
async fn scripted_server(
    responses: Vec<String>,
    expected_requests: u64,
) -> (MockServer, RepairHarness) {
    let server = MockServer::start().await;
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ScriptedRepairModel {
            responses,
            calls: AtomicUsize::new(0),
            bodies: Arc::clone(&bodies),
        })
        .expect(expected_requests)
        .mount(&server)
        .await;
    (server, RepairHarness { bodies })
}

fn client_for(server: &MockServer) -> LlmClient {
    LlmClient::new(LlmConfig {
        base_url: format!("{}/v1", server.uri()),
        model: "test-repairer".into(),
        timeout_secs: 30,
        ..Default::default()
    })
}

async fn open_store() -> (TempDir, ProvenanceStore) {
    let dir = tempfile::tempdir().expect("temp dir for the store");
    let store = ProvenanceStore::open(&dir.path().join("provenance.db"))
        .await
        .expect("a fresh store opens");
    (dir, store)
}

fn repair_provenance(document: &str) -> LocalProvenance {
    LocalProvenance {
        activity_id: "test-repair-activity".to_string(),
        agent_id: "test-repairer".to_string(),
        agent_kind: "SoftwareAgent".to_string(),
        source_entity_id: document.to_string(),
        source_kind: "Document".to_string(),
        tenant: "local".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        ended_at: "2026-01-01T00:00:00Z".to_string(),
        locality: "local".to_string(),
        origin_source_id: None,
        origin_action_id: None,
    }
}

fn test_classification() -> OntologyClassification<'static> {
    OntologyClassification {
        version_iri: "http://example.org/ontology/test",
        artifact_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    }
}

/// An `UnresolvedUnit` item the structural code tier cannot decide.
fn elongation_rejection(claimed_unit: &str) -> RejectedFact {
    let raw = json!({
        "subject": "steel", "predicate": "has_measurement", "object": "elongation",
        "value": 4.5, "unit": claimed_unit, "kind": "measurement",
        "confidence": 0.9, "evidence_class": "research", "conditions": []
    });
    RejectedFact {
        subject: RejectedSubject::Raw(Box::new(raw)),
        class: RejectionClass::UnresolvedUnit,
        detail: format!("test: unit {claimed_unit:?} did not resolve"),
    }
}

fn elongation_correction(unit: &str) -> Value {
    json!({
        "subject": "steel", "predicate": "has_measurement", "object": "elongation",
        "value": 4.5, "unit": unit, "kind": "measurement",
        "confidence": 0.9, "evidence_class": "research", "conditions": []
    })
}

fn accept_reply(corrected: Value, reason: &str) -> String {
    json!({"decision": "accept", "corrected": corrected, "reason": reason}).to_string()
}

fn withdraw_reply(reason: &str) -> String {
    json!({"decision": "withdraw", "reason": reason}).to_string()
}

async fn enqueue(
    store: &ProvenanceStore,
    rejection: &RejectedFact,
    enqueued_at: f64,
) -> RepairItem {
    let item = queue_item(rejection, DOC, "local", enqueued_at);
    store.enqueue_repair(&item).await.expect("enqueue");
    item
}

/// A queued `UnresolvedUnit` whose document prints a term ends ACCEPT with
/// that exact term, verified by the same grounding gate and ledgered with its
/// evidence. Rust supplies no term menu.
#[tokio::test]
async fn a_queued_unresolved_unit_with_a_printed_term_is_accepted() {
    // CONTRACT CHANGE: the repair model copies the paper term rather than
    // choosing a canonical identifier from a Rust vocabulary.
    assert!(
        dispose(
            &elongation_rejection("QUDT:INVENTED"),
            DOC,
            ELONGATION_DOC,
            &RepairPolicy::default(),
            NOW,
            &crate::ontologies::EmmoOntology,
        )
        .is_none(),
        "test premise: the code tiers must queue this refusal for the model tier"
    );

    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;

    let (server, _harness) = scripted_server(
        vec![accept_reply(
            elongation_correction("%"),
            "the document prints the value with a percent sign",
        )],
        1, // EXACTLY one model call for the one item
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");

    assert_eq!(report.items_seen, 1, "{report:?}");
    assert_eq!(report.accepted, 1, "{report:?}");
    assert_eq!(report.withdrawn, 0, "{report:?}");
    assert_eq!(report.model_calls, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");

    // Exactly one ledger row, an accept by the model, carrying the span
    // that verified the correction.
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "accept");
    assert_eq!(ledger[0].dispositioner, "model:test-repairer");
    assert_eq!(ledger[0].attempt, 1);
    let evidence = ledger[0]
        .evidence
        .as_deref()
        .expect("an accept carries evidence");
    assert!(evidence.contains("elongation of 4.5 %"), "{evidence}");

    // The repaired fact entered through the normal write path with the exact
    // term supported by the evidence.
    let recalled = store
        .recall_with_context("elongation", "local", 10)
        .await
        .unwrap();
    assert_eq!(recalled.len(), 1, "{recalled:?}");
    assert_eq!(recalled[0].value, Some(4.5));
    assert_eq!(recalled[0].unit.as_deref(), Some("%"));

    // The queue is empty — the item was dequeued by its disposition.
    assert!(store.pending_repairs(DOC, 10).await.unwrap().is_empty());
}

/// FIELD FREEZE: a reply that changes the subject is auto-WITHDRAWN with
/// "repair exceeded its mandate" — never written, never accepted.
#[tokio::test]
async fn a_reply_that_changes_the_subject_is_auto_withdrawn() {
    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;

    let mut corrected = elongation_correction("%");
    corrected["subject"] = json!("some other alloy");
    let (server, _harness) = scripted_server(vec![accept_reply(corrected, "trust me")], 1).await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");

    assert_eq!(report.accepted, 0, "{report:?}");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "withdraw");
    assert!(
        ledger[0].reason.contains("exceeded its mandate") && ledger[0].reason.contains("subject"),
        "the reason must name the violation: {}",
        ledger[0].reason
    );
    // Nothing was written.
    assert!(
        store
            .recall_with_context("elongation", "local", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// BOUNDED: exactly ONE attempt per item per run — a garbage reply costs
/// one call and requeues the item (no retry loop), and a SECOND failure is
/// a final WITHDRAW recorded in the ledger. The wiremock expectation (2
/// calls across two runs, never more) kills any retry-loop regression.
#[tokio::test]
async fn exactly_one_attempt_per_item_and_a_second_failure_is_final() {
    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;

    let (server, _harness) = scripted_server(
        vec!["I cannot decide this, the document is unclear (no JSON here)".to_string()],
        2, // one call per run, two runs — never a retry within a run
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let policy = RepairWorkerPolicy::default();

    // Run 1: the attempt fails; the item stays queued with its attempt
    // counted, and NO ledger row exists (no decision was rendered).
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &policy,
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.requeued, 1, "{report:?}");
    assert_eq!(report.accepted + report.withdrawn, 0, "{report:?}");
    assert!(store.repair_dispositions(DOC).await.unwrap().is_empty());
    let pending = store.pending_repairs(DOC, 10).await.unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].attempts, 1, "the failed attempt must be counted");

    // Run 2: the second failure is FINAL — a ledgered withdraw naming the
    // real constraint, and the item leaves the queue.
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &policy,
        NOW + 20.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    assert_eq!(report.requeued, 0, "{report:?}");
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "withdraw");
    assert!(
        ledger[0]
            .reason
            .starts_with("final withdrawal after 2 failed attempt(s)"),
        "{}",
        ledger[0].reason
    );
    assert!(
        ledger[0].reason.contains("not valid repair JSON"),
        "the final reason must carry the real constraint of the last failure: {}",
        ledger[0].reason
    );
    // The model rendered nothing; the attempt limit decided.
    assert_eq!(ledger[0].dispositioner, "code:repair-worker");
    assert!(store.pending_repairs(DOC, 10).await.unwrap().is_empty());
}

/// SAME GATES: a correction that would not pass the grounding gate that
/// refused the fact is WITHDRAWN — the document prints the value with NO
/// unit beside it, so a model-chosen unit cannot ground.
#[tokio::test]
async fn a_repair_that_fails_the_grounding_gate_is_withdrawn() {
    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;

    // The value appears, but the proposed exact term is absent after it.
    let bare = "The steel samples reached an elongation of 4.5 before fracture.";
    let (server, _harness) = scripted_server(
        vec![accept_reply(
            elongation_correction("%"),
            "a percent is plausible for elongation",
        )],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        bare,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");

    assert_eq!(report.accepted, 0, "{report:?}");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "withdraw");
    assert!(
        ledger[0].reason.contains("grounding gate"),
        "the reason must name the gate that refused the repair: {}",
        ledger[0].reason
    );
    assert!(
        store
            .recall_with_context("elongation", "local", 10)
            .await
            .unwrap()
            .is_empty(),
        "a repair that failed the gates must not be stored"
    );
}

/// Every item the run DECIDES leaves exactly one disposition row — a mixed
/// batch of an accept, an explicit model withdraw, and one garbage reply:
/// two decided items, two ledger rows, one each; the undecided item stays
/// queued with no row.
#[tokio::test]
async fn every_decided_item_leaves_exactly_one_disposition_row() {
    let (_dir, store) = open_store().await;
    // Three distinct refusals (distinct identities → distinct item ids).
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;
    let hardness = RejectedFact {
        subject: RejectedSubject::Raw(Box::new(json!({
            "subject": "steel", "predicate": "has_measurement", "object": "hardness",
            "value": 349.0, "unit": "QUDT:INVENTED", "kind": "measurement",
            "confidence": 0.9, "evidence_class": "research", "conditions": []
        }))),
        class: RejectionClass::UnresolvedUnit,
        detail: "test: unit did not resolve".into(),
    };
    enqueue(&store, &hardness, NOW + 1.0).await;
    let uts = RejectedFact {
        subject: RejectedSubject::Raw(Box::new(json!({
            "subject": "steel", "predicate": "has_measurement", "object": "UTS",
            "value": 880.0, "unit": "QUDT:INVENTED", "kind": "measurement",
            "confidence": 0.9, "evidence_class": "research", "conditions": []
        }))),
        class: RejectionClass::UnresolvedUnit,
        detail: "test: unit did not resolve".into(),
    };
    enqueue(&store, &uts, NOW + 2.0).await;

    let text = format!(
        "{ELONGATION_DOC}\nThe steel hardness was measured qualitatively.\n\
         The steel UTS was discussed without numbers."
    );
    let (server, _harness) = scripted_server(
        vec![
            accept_reply(
                elongation_correction("%"),
                "the document prints a percent sign",
            ),
            withdraw_reply("the document prints no unit beside the hardness value"),
            "garbage, not a disposition".to_string(),
        ],
        3, // one call per item — the batch is worked one by one
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        &text,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");

    assert_eq!(report.items_seen, 3, "{report:?}");
    assert_eq!(report.accepted, 1, "{report:?}");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    assert_eq!(report.requeued, 1, "{report:?}");
    assert_eq!(report.model_calls, 3, "{report:?}");

    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 2, "one row per decided item: {ledger:?}");
    let mut by_item: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for row in &ledger {
        *by_item.entry(row.item_id.as_str()).or_default() += 1;
    }
    assert!(
        by_item.values().all(|count| *count == 1),
        "no item may carry two rows: {by_item:?}"
    );
    assert_eq!(by_item.len(), 2, "{by_item:?}");

    // Only the garbage item remains queued.
    let pending = store.pending_repairs(DOC, 10).await.unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert!(pending[0].item_id.contains("UTS"), "{}", pending[0].item_id);
}

/// REVIEW MISSING gets the SAME reviewer question Phase 1 would have asked
/// (the shared builder, one fact, its evidence spans), and only an
/// explicit `Asserted` survives — written, with the evidence spans as the
/// ledger's evidence. A `Denied` verdict is a final withdraw.
#[tokio::test]
async fn review_missing_accepts_only_an_explicit_asserted_verdict() {
    let fact = MaterialFact {
        subject: "steel".into(),
        predicate: "has_phase".into(),
        object: "ferrite".into(),
        value: None,
        unit: None,
        conditions: Vec::new(),
        confidence: Some(0.8),
        kind: Some("phase".into()),
        evidence_class: Default::default(),
        verification: None,
        verification_reason: None,
    };
    let text = "The steel showed a ferrite phase throughout the sample.";

    // ACCEPT arm.
    let (_dir, store) = open_store().await;
    enqueue(
        &store,
        &RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact.clone())),
            class: RejectionClass::ReviewMissing,
            detail: "semantic model review returned no verdict".into(),
        },
        NOW,
    )
    .await;
    let (server, harness) = scripted_server(
        vec![
            json!({
                "decisions": [{"fact_index": 0, "verdict": "asserted",
                               "reason": "the document states the ferrite phase"}]
            })
            .to_string(),
        ],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        text,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.accepted, 1, "{report:?}");

    // The reviewer question is the Phase-1 question itself.
    let prompt = harness.bodies.lock().unwrap()[0].clone();
    assert!(
        prompt.contains("semantic grounding reviewer"),
        "the repair tier must ask the SAME reviewer question: {prompt}"
    );
    assert!(prompt.contains("ferrite"), "{prompt}");

    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "accept");
    assert_eq!(ledger[0].dispositioner, "model:test-repairer");
    let evidence = ledger[0].evidence.as_deref().expect("evidence spans");
    assert!(evidence.contains("ferrite phase"), "{evidence}");
    // Field freeze is trivially total: the accepted fact IS the queued one.
    let corrected: MaterialFact =
        serde_json::from_str(ledger[0].corrected_json.as_deref().unwrap()).unwrap();
    assert_eq!(corrected.subject, "steel");
    assert_eq!(corrected.predicate, "has_phase");
    assert_eq!(corrected.object, "ferrite");
    assert_eq!(
        store
            .recall_with_context("ferrite", "local", 10)
            .await
            .unwrap()
            .len(),
        1,
        "the asserted fact must be written"
    );

    // DENIED arm: a rendered denial is a final withdraw.
    let (_dir, store) = open_store().await;
    enqueue(
        &store,
        &RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact.clone())),
            class: RejectionClass::ReviewMissing,
            detail: "semantic model review returned no verdict".into(),
        },
        NOW,
    )
    .await;
    let (server, _harness) = scripted_server(
        vec![
            json!({
                "decisions": [{"fact_index": 0, "verdict": "denied",
                               "reason": "the document rules ferrite out"}]
            })
            .to_string(),
        ],
        1,
    )
    .await;
    let llm = client_for(&server);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        text,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 20.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "withdraw");
    assert!(ledger[0].reason.contains("denied"), "{}", ledger[0].reason);
    assert!(
        store
            .recall_with_context("ferrite", "local", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// ANTI-RATCHET, defended at the door: a rendered judgement that somehow
/// sits in the queue is withdrawn WITHOUT a model call — wiremock expects
/// zero requests, so any call fails the test.
#[tokio::test]
async fn a_rendered_judgement_never_reaches_the_model() {
    let (_dir, store) = open_store().await;
    let fact = MaterialFact {
        subject: "Alloy X".into(),
        predicate: "has_phase".into(),
        object: "omega".into(),
        value: None,
        unit: None,
        conditions: Vec::new(),
        confidence: Some(0.7),
        kind: Some("phase".into()),
        evidence_class: Default::default(),
        verification: None,
        verification_reason: None,
    };
    // `queue_item` would panic for a rendered class — construct the row
    // directly to simulate one that bypassed the check.
    let item = RepairItem {
        item_id: format!("{DOC}|alloy-omega|review_denied"),
        document: DOC.to_string(),
        tenant: "local".to_string(),
        class: RejectionClass::ReviewDenied.as_str().to_string(),
        subject_json: serde_json::to_string(&fact).unwrap(),
        detail: "semantic model review returned Denied".to_string(),
        enqueued_at: NOW,
        attempts: 0,
    };
    store.enqueue_repair(&item).await.unwrap();

    let (server, _harness) = scripted_server(vec![], 0).await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        "Alloy X showed no omega phase.",
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    assert_eq!(report.model_calls, 0, "{report:?}");
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].dispositioner, "code:repair-worker");
    assert!(
        ledger[0].reason.contains("rendered judgement"),
        "{}",
        ledger[0].reason
    );
}

/// ACCESS, NOT STUFFING: the UnresolvedUnit prompt carries the frozen
/// fact, the refusal reason, the value-bearing span and the closed
/// vocabulary — and NOTHING else from the document (an unrelated line
/// must not leak in). The vocabulary offered for an unknown-kind property
#[tokio::test]
async fn the_unresolved_unit_prompt_gives_access_without_stuffing_context() {
    // CONTRACT CHANGE: the prompt supplies source access but no Rust-owned
    // unit menu; the reader copies the exact non-empty source term.
    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;
    let text = format!("{ELONGATION_DOC}\nThe furnace schedule remained proprietary.");

    let (server, harness) = scripted_server(
        vec![accept_reply(
            elongation_correction("%"),
            "the source prints this exact term",
        )],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    run_repair_pass(
        &store,
        &llm,
        DOC,
        &text,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");

    let prompt = harness.bodies.lock().unwrap()[0].clone();
    // The frozen fact and the refusal reason are there.
    assert!(prompt.contains("\"QUDT:INVENTED\""), "{prompt}");
    assert!(prompt.contains("did not resolve"), "{prompt}");
    // The value-bearing span is there…
    assert!(prompt.contains("elongation of 4.5 %"), "{prompt}");
    // …and the unrelated line is NOT.
    assert!(
        !prompt.contains("furnace schedule"),
        "the prompt must carry access, not the whole document: {prompt}"
    );
    assert!(!prompt.contains("CLOSED UNIT VOCABULARY"), "{prompt}");
    assert!(!prompt.contains("QUDT:MegaPA"), "{prompt}");
}

#[tokio::test]
async fn unresolved_unit_prompt_has_no_rust_vocabulary() {
    // CONTRACT CHANGE: property words no longer select a hardcoded unit
    // subset. A reader may still withdraw when the cited text is insufficient.
    let (_dir, store) = open_store().await;
    let rejection = RejectedFact {
        subject: RejectedSubject::Raw(Box::new(json!({
            "subject": "AlSi10Mg", "predicate": "has_measurement", "object": "scan speed",
            "value": 1250.0, "unit": "QUDT:INVENTED", "kind": "measurement",
            "confidence": 0.9, "evidence_class": "research", "conditions": []
        }))),
        class: RejectionClass::UnresolvedUnit,
        detail: "test: unit did not resolve".into(),
    };
    enqueue(&store, &rejection, NOW).await;
    // The value appears with NO unit beside it, so the model withdraws.
    let text = "The AlSi10Mg parts were built at a scan speed of 1250 throughout.";

    let (server, harness) = scripted_server(
        vec![withdraw_reply(
            "the document prints the value but no unit beside it",
        )],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        text,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.withdrawn, 1, "{report:?}");

    let prompt = harness.bodies.lock().unwrap()[0].clone();
    for removed_term in ["QUDT:MilliM-PER-SEC", "QUDT:M-PER-SEC", "QUDT:MegaPA"] {
        assert!(!prompt.contains(removed_term), "{removed_term}: {prompt}");
    }
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "withdraw");
    assert_eq!(ledger[0].dispositioner, "model:test-repairer");
    assert!(
        ledger[0].reason.contains("no unit beside it"),
        "{}",
        ledger[0].reason
    );
}

/// MalformedShape (a raw fact that never converted): the model supplies
/// the missing unit from the spans, the correction clears the same gates,
/// and the fact is stored.
#[tokio::test]
async fn a_malformed_shape_is_repaired_from_the_subject_spans() {
    let (_dir, store) = open_store().await;
    let rejection = RejectedFact {
        subject: RejectedSubject::Raw(Box::new(json!({
            "subject": "steel", "predicate": "has_measurement", "object": "elongation",
            "value": 4.5, "unit": null, "kind": "measurement",
            "confidence": 0.9, "evidence_class": "research", "conditions": []
        }))),
        class: RejectionClass::MalformedShape,
        detail: "numeric value 4.5 arrived with no unit at all".into(),
    };
    enqueue(&store, &rejection, NOW).await;

    let (server, harness) = scripted_server(
        vec![accept_reply(
            elongation_correction("%"),
            "the span prints a percent sign after the value",
        )],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.accepted, 1, "{report:?}");

    // The prompt's access window is the spans NAMING THE SUBJECT.
    let prompt = harness.bodies.lock().unwrap()[0].clone();
    assert!(prompt.contains("elongation of 4.5 %"), "{prompt}");
    assert!(prompt.contains("no unit at all"), "{prompt}");

    let recalled = store
        .recall_with_context("elongation", "local", 10)
        .await
        .unwrap();
    assert_eq!(recalled.len(), 1, "{recalled:?}");
    assert_eq!(recalled[0].unit.as_deref(), Some("%"));
}

/// ValuelessWithUnit (a converted fact): an explicit model withdraw is
/// ledgered as-is — the typical honest outcome for a contradictory shape.
#[tokio::test]
async fn a_valueless_with_unit_item_withdraws_on_the_models_explicit_word() {
    let (_dir, store) = open_store().await;
    let fact = MaterialFact {
        subject: "steel".into(),
        predicate: "has_measurement".into(),
        object: "ferrite fraction".into(),
        value: None,
        unit: Some(QudtUnit::new("QUDT:PERCENT").unwrap()),
        conditions: Vec::new(),
        confidence: Some(0.8),
        kind: None,
        evidence_class: Default::default(),
        verification: None,
        verification_reason: None,
    };
    enqueue(
        &store,
        &RejectedFact {
            subject: RejectedSubject::Converted(Box::new(fact)),
            class: RejectionClass::ValuelessWithUnit,
            detail: "a value-less assertion carried a unit".into(),
        },
        NOW,
    )
    .await;

    let (server, harness) = scripted_server(
        vec![withdraw_reply(
            "the document states no value for the ferrite fraction; the unit was spurious",
        )],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        "The steel contained a ferrite fraction, reported without numbers.",
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.withdrawn, 1, "{report:?}");

    // The prompt carried the contradiction and the subject spans.
    let prompt = harness.bodies.lock().unwrap()[0].clone();
    assert!(
        prompt.contains("a value-less assertion carried a unit"),
        "{prompt}"
    );
    assert!(
        prompt.contains("ferrite fraction, reported without numbers"),
        "{prompt}"
    );

    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].outcome, "withdraw");
    assert!(
        ledger[0].reason.contains("spurious"),
        "{}",
        ledger[0].reason
    );
}

/// The run is BOUNDED by `max_items_per_run`: with two items queued and a
/// limit of one, exactly one item is seen and exactly one call is made —
/// the second item waits for the next run.
#[tokio::test]
async fn the_run_is_bounded_by_max_items() {
    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;
    let hardness = RejectedFact {
        subject: RejectedSubject::Raw(Box::new(json!({
            "subject": "steel", "predicate": "has_measurement", "object": "hardness",
            "value": 349.0, "unit": "QUDT:INVENTED", "kind": "measurement",
            "confidence": 0.9, "evidence_class": "research", "conditions": []
        }))),
        class: RejectionClass::UnresolvedUnit,
        detail: "test: unit did not resolve".into(),
    };
    enqueue(&store, &hardness, NOW + 1.0).await;

    let (server, _harness) = scripted_server(
        vec![accept_reply(elongation_correction("%"), "percent")],
        1, // the second item must NOT be prompted in this run
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let policy = RepairWorkerPolicy {
        max_items_per_run: 1,
        ..Default::default()
    };
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &policy,
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.items_seen, 1, "{report:?}");
    assert_eq!(report.model_calls, 1, "{report:?}");
    assert_eq!(store.pending_repairs(DOC, 10).await.unwrap().len(), 1);
}

/// A non-empty proposed term is not judged by a Rust vocabulary, but still
/// must be present in the source span to pass the grounding check.
#[tokio::test]
async fn an_unsupported_term_is_withdrawn_by_source_grounding() {
    // CONTRACT CHANGE: vocabulary membership is gone; this is rejected only
    // because the exact proposed term is absent from the evidence.
    let (_dir, store) = open_store().await;
    enqueue(&store, &elongation_rejection("QUDT:INVENTED"), NOW).await;

    let (server, _harness) = scripted_server(
        vec![accept_reply(
            elongation_correction("QUDT:TOTALLY-NEW-UNIT"),
            "invented a unit",
        )],
        1,
    )
    .await;
    let llm = client_for(&server);
    let prov = repair_provenance(DOC);
    let report = run_repair_pass(
        &store,
        &llm,
        DOC,
        ELONGATION_DOC,
        &prov,
        test_classification(),
        &RepairWorkerPolicy::default(),
        NOW + 10.0,
        &crate::ontologies::EmmoOntology,
    )
    .await
    .expect("the run completes");
    assert_eq!(report.withdrawn, 1, "{report:?}");
    let ledger = store.repair_dispositions(DOC).await.unwrap();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert!(
        ledger[0].reason.contains("grounding gate"),
        "{}",
        ledger[0].reason
    );
    assert!(
        store
            .recall_with_context("elongation", "local", 10)
            .await
            .unwrap()
            .is_empty()
    );
}
