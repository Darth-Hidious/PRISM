use std::collections::HashMap;
use std::sync::Arc;

use prism_embed::OpenAiCompat;
use prism_retrieval::{EngineConfig, RelevancePolicy, RelevanceStatus, RetrievalEngine};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const QUERY: &str = "PEEK dielectric breakdown strength";
const COLLISION_TITLE: &str = "PEEK: Picking Essential frames via Efficient Knowledge distillation";
const MATERIALS_TITLE: &str = "Dielectric breakdown strength of PEEK composites";
const ASTRONOMY_TITLE: &str = "Ultraviolet Extinction at High Galactic Latitudes II";

fn source_response() -> serde_json::Value {
    json!({
        "message": {
            "total-results": 3,
            "items": [
                {
                    "DOI": "10.1000/peek-cv",
                    "title": [COLLISION_TITLE],
                    "abstract": "A computer-vision method for efficient video frame selection."
                },
                {
                    "DOI": "10.1000/peek-dielectric",
                    "title": [MATERIALS_TITLE],
                    "abstract": "Electrical insulation tests measure breakdown fields in the polymer."
                },
                {
                    "DOI": "10.1000/galactic-extinction",
                    "title": [ASTRONOMY_TITLE],
                    "abstract": "A survey of interstellar dust and ultraviolet starlight."
                }
            ]
        }
    })
}

async fn mount_source(server: &MockServer, expected_calls: u64) {
    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(source_response()))
        .expect(expected_calls)
        .mount(server)
        .await;
}

fn engine(server: &MockServer, policy: Option<RelevancePolicy>) -> RetrievalEngine {
    RetrievalEngine::new(EngineConfig {
        sources: vec!["crossref".to_string()],
        base_overrides: HashMap::from([("crossref".to_string(), server.uri())]),
        cache_dir: None,
        per_source_timeout_secs: 10,
        max_attempts: 1,
        relevance: policy,
        ..EngineConfig::default()
    })
}

fn fixture_backend(server: &MockServer) -> Arc<OpenAiCompat> {
    Arc::new(OpenAiCompat::new(&server.uri(), "relevance-fixture", None))
}

#[tokio::test]
async fn real_search_batches_ranks_filters_and_reports_the_peek_collision() {
    let server = MockServer::start().await;
    mount_source(&server, 2).await;

    // Query, collision (~0.53), materials (1.0), astronomy (~0.57). The
    // unrelated scores approximate the shipped BGE model's measured fixture
    // scores. The response deliberately carries indices out of order so this
    // also exercises the shipped OpenAI-compatible backend's ordering.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"index": 2, "embedding": [1.0, 0.0]},
                {"index": 0, "embedding": [1.0, 0.0]},
                {"index": 3, "embedding": [0.57, 0.82]},
                {"index": 1, "embedding": [0.53, 0.85]}
            ]
        })))
        .expect(2)
        .mount(&server)
        .await;

    // A permissive caller override keeps every paper, making ranking itself
    // observable: the source returns the acronym collision first.
    let ranked = engine(
        &server,
        Some(RelevancePolicy {
            minimum_similarity: -1.0,
            ..RelevancePolicy::default()
        }),
    )
    .with_relevance_backend(Some(fixture_backend(&server)))
    .search(QUERY, 10)
    .await;
    assert_eq!(ranked.relevance.status, RelevanceStatus::Applied);
    assert_eq!(ranked.relevance.evaluated, 3);
    assert_eq!(ranked.relevance.dropped, 0);
    assert_eq!(ranked.papers[0].title, MATERIALS_TITLE);
    let collision_rank = ranked
        .papers
        .iter()
        .position(|paper| paper.title == COLLISION_TITLE)
        .expect("collision paper is retained by the permissive override");
    assert!(
        collision_rank > 0,
        "the materials paper must rank above the PEEK acronym collision"
    );

    // The production default excludes both unrelated fields. This assertion
    // fails if the relevance stage is disconnected from the real
    // RetrievalEngine::search path or if the shipped policy regresses below
    // the measured acronym-collision boundary.
    let filtered = engine(&server, Some(RelevancePolicy::default()))
        .with_relevance_backend(Some(fixture_backend(&server)))
        .search(QUERY, 10)
        .await;
    assert_eq!(filtered.papers.len(), 1);
    assert_eq!(filtered.papers[0].title, MATERIALS_TITLE);
    assert_eq!(filtered.source_status[0].count, 3);
    assert_eq!(filtered.relevance.status, RelevanceStatus::Applied);
    assert_eq!(filtered.relevance.candidates, 3);
    assert_eq!(filtered.relevance.evaluated, 3);
    assert_eq!(filtered.relevance.dropped, 2);
    assert_eq!(
        filtered.relevance.threshold,
        Some(RelevancePolicy::default().minimum_similarity)
    );
    let dropped_titles: Vec<&str> = filtered
        .relevance
        .off_topic_examples
        .iter()
        .map(|example| example.title.as_str())
        .collect();
    assert_eq!(dropped_titles, [ASTRONOMY_TITLE, COLLISION_TITLE]);

    server.verify().await;
    let requests = server.received_requests().await.expect("request recording");
    let embedding_requests: Vec<_> = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/embeddings")
        .collect();
    assert_eq!(
        embedding_requests.len(),
        2,
        "each search must make one embedding round trip, not one per paper"
    );
    let expected_inputs = json!([
        QUERY,
        format!(
            "{COLLISION_TITLE}\n\nA computer-vision method for efficient video frame selection."
        ),
        format!(
            "{MATERIALS_TITLE}\n\nElectrical insulation tests measure breakdown fields in the polymer."
        ),
        format!("{ASTRONOMY_TITLE}\n\nA survey of interstellar dust and ultraviolet starlight.")
    ]);
    for request in embedding_requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["input"], expected_inputs);
    }
}

#[tokio::test]
async fn real_search_without_a_backend_returns_everything_and_says_unfiltered() {
    let server = MockServer::start().await;
    mount_source(&server, 2).await;

    let unavailable = engine(&server, Some(RelevancePolicy::default()))
        .with_relevance_backend(None)
        .search(QUERY, 10)
        .await;
    let original_order: Vec<&str> = unavailable
        .papers
        .iter()
        .map(|paper| paper.title.as_str())
        .collect();
    assert_eq!(
        original_order,
        [COLLISION_TITLE, MATERIALS_TITLE, ASTRONOMY_TITLE]
    );
    assert_eq!(unavailable.relevance.status, RelevanceStatus::Unavailable);
    assert!(unavailable.relevance.returned_unfiltered);
    assert_eq!(unavailable.relevance.candidates, 3);
    assert_eq!(unavailable.relevance.evaluated, 0);
    assert_eq!(unavailable.relevance.dropped, 0);
    assert!(
        unavailable
            .relevance
            .message
            .as_deref()
            .is_some_and(|message| message.contains("no embedding backend"))
    );

    // Filtering off is a different, deliberate state and preserves the same
    // result set and source order without consulting any embedding backend.
    let disabled = engine(&server, None).search(QUERY, 10).await;
    let disabled_order: Vec<&str> = disabled
        .papers
        .iter()
        .map(|paper| paper.title.as_str())
        .collect();
    assert_eq!(disabled_order, original_order);
    assert_eq!(disabled.relevance.status, RelevanceStatus::Disabled);
    assert!(disabled.relevance.returned_unfiltered);

    server.verify().await;
    let requests = server.received_requests().await.expect("request recording");
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() == "/works"),
        "unavailable and disabled paths must not call an embedding endpoint"
    );
}

// ── Selector stage: precision after recall ───────────────────────────────────
//
// The measured defect: bge-small matches shared VOCABULARY, not meaning. For
// the live query "PFAS-free elastomer seals gaskets O-ring chemical
// resistance service temperature" it kept "Completely Symmetric Resistance
// Forms on the Stretched Sierpinski Gasket" — pure mathematics — because a
// Sierpinski gasket shares words with a sealing gasket, and no cosine
// threshold can separate them. These tests drive the optional LLM selector
// stage through the production `RetrievalEngine::search` path with scripted
// judges; no live model is ever contacted.

use async_trait::async_trait;
use prism_retrieval::{
    Selector, SelectorCandidate, SelectorPolicy, SelectorStatus, SelectorVerdict,
};

const SEALS_QUERY: &str =
    "PFAS-free elastomer seals gaskets O-ring chemical resistance service temperature";
const SEALS_TITLE: &str = "PFAS-Free Nonmetallic Seals for Chemical Service";
const SIERPINSKI_TITLE: &str =
    "Completely Symmetric Resistance Forms on the Stretched Sierpinski Gasket";
const APOLLONIAN_TITLE: &str =
    "Single line Apollonian gaskets: is the limit a space filling fractal curve?";
const FRACTAL_REASON: &str = "fractal geometry, not sealing hardware";

fn seals_source_response() -> serde_json::Value {
    json!({
        "message": {
            "total-results": 3,
            "items": [
                {
                    "DOI": "10.1000/sierpinski-gasket",
                    "title": [SIERPINSKI_TITLE],
                    "abstract": "Resistance forms and Dirichlet structures on fractal sets."
                },
                {
                    "DOI": "10.1000/pfas-free-seals",
                    "title": [SEALS_TITLE],
                    "abstract": "Elastomer O-ring seals for chemical service without PFAS."
                },
                {
                    "DOI": "10.1000/apollonian-gasket",
                    "title": [APOLLONIAN_TITLE],
                    "abstract": "Circle packings and space filling fractal curves."
                }
            ]
        }
    })
}

async fn mount_seals_source(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(seals_source_response()))
        .expect(1)
        .mount(server)
        .await;
}

fn selector_engine(server: &MockServer, relevance: Option<RelevancePolicy>) -> RetrievalEngine {
    RetrievalEngine::new(EngineConfig {
        sources: vec!["crossref".to_string()],
        base_overrides: HashMap::from([("crossref".to_string(), server.uri())]),
        cache_dir: None,
        per_source_timeout_secs: 10,
        max_attempts: 1,
        relevance,
        selector: Some(SelectorPolicy::default()),
        ..EngineConfig::default()
    })
}

fn titles(outcome: &prism_retrieval::SearchOutcome) -> Vec<&str> {
    outcome
        .papers
        .iter()
        .map(|paper| paper.title.as_str())
        .collect()
}

/// Rules on every candidate: fractal papers are off-subject, seals are not.
struct SubjectJudge;

#[async_trait]
impl Selector for SubjectJudge {
    fn id(&self) -> String {
        "model:test-judge".to_string()
    }
    async fn judge(
        &self,
        _query: &str,
        candidates: &[SelectorCandidate],
    ) -> Result<Vec<Option<SelectorVerdict>>, String> {
        Ok(candidates
            .iter()
            .map(|candidate| {
                let fractal = candidate.title.contains("Sierpinski")
                    || candidate.title.contains("Apollonian");
                Some(SelectorVerdict {
                    relevant: !fractal,
                    reason: if fractal {
                        FRACTAL_REASON.to_string()
                    } else {
                        "elastomer sealing materials".to_string()
                    },
                })
            })
            .collect())
    }
}

/// Fails the whole batch, the way a dead endpoint or a timeout would.
struct FailingJudge;

#[async_trait]
impl Selector for FailingJudge {
    fn id(&self) -> String {
        "model:failing-judge".to_string()
    }
    async fn judge(
        &self,
        _query: &str,
        _candidates: &[SelectorCandidate],
    ) -> Result<Vec<Option<SelectorVerdict>>, String> {
        Err("judge exploded mid-flight".to_string())
    }
}

/// Never rules on the Sierpinski paper and drops only the Apollonian one.
struct OmittingJudge;

#[async_trait]
impl Selector for OmittingJudge {
    fn id(&self) -> String {
        "model:omitting-judge".to_string()
    }
    async fn judge(
        &self,
        _query: &str,
        candidates: &[SelectorCandidate],
    ) -> Result<Vec<Option<SelectorVerdict>>, String> {
        Ok(candidates
            .iter()
            .map(|candidate| {
                if candidate.title.contains("Sierpinski") {
                    None
                } else if candidate.title.contains("Apollonian") {
                    Some(SelectorVerdict {
                        relevant: false,
                        reason: FRACTAL_REASON.to_string(),
                    })
                } else {
                    Some(SelectorVerdict {
                        relevant: true,
                        reason: "elastomer sealing materials".to_string(),
                    })
                }
            })
            .collect())
    }
}

/// THE defect, fixed: every paper scores above the cosine threshold (the
/// embedding stage cannot separate a Sierpinski gasket from a sealing
/// gasket), and the selector removes exactly the papers about a different
/// subject — leaving exactly the seals paper.
#[tokio::test]
async fn selector_separates_what_cosine_cannot_the_sierpinski_gasket_case() {
    let server = MockServer::start().await;
    mount_seals_source(&server).await;
    // Query [1, 0]; seals 1.0, Sierpinski 0.8, Apollonian ~0.7 — ALL at or
    // above the 0.6 default, reproducing the measured razor-thin band where
    // no threshold discriminates.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"index": 0, "embedding": [1.0, 0.0]},
                {"index": 1, "embedding": [0.8, 0.6]},
                {"index": 2, "embedding": [1.0, 0.0]},
                {"index": 3, "embedding": [0.7, 0.7141]}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = selector_engine(&server, Some(RelevancePolicy::default()))
        .with_relevance_backend(Some(fixture_backend(&server)))
        .with_selector(Some(Arc::new(SubjectJudge)))
        .search(SEALS_QUERY, 10)
        .await;

    // The embedding stage kept all three — that IS the defect under test.
    assert_eq!(outcome.relevance.status, RelevanceStatus::Applied);
    assert_eq!(outcome.relevance.evaluated, 3);
    assert_eq!(outcome.relevance.dropped, 0);

    // The selector left exactly the seals paper.
    assert_eq!(titles(&outcome), [SEALS_TITLE]);
    assert!(!outcome.relevance.returned_unfiltered);

    // The report's counts equal what actually happened.
    let selector = outcome
        .relevance
        .selector
        .as_ref()
        .expect("stage configured");
    assert_eq!(selector.status, SelectorStatus::Applied);
    assert_eq!(selector.candidates, 3);
    assert_eq!(selector.judged, 3);
    assert_eq!(selector.dropped, 2);
    assert_eq!(selector.model.as_deref(), Some("model:test-judge"));
    assert!(!selector.returned_unfiltered);
    let dropped: Vec<(&str, &str)> = selector
        .dropped_examples
        .iter()
        .map(|example| (example.title.as_str(), example.reason.as_str()))
        .collect();
    assert_eq!(
        dropped,
        [
            (SIERPINSKI_TITLE, FRACTAL_REASON),
            (APOLLONIAN_TITLE, FRACTAL_REASON),
        ]
    );

    server.verify().await;
}

/// FAIL OPEN: the stage is enabled but no LLM is configured — every paper
/// survives and the report says exactly that.
#[tokio::test]
async fn selector_without_a_judge_keeps_every_paper_and_says_so() {
    let server = MockServer::start().await;
    mount_seals_source(&server).await;

    let outcome = selector_engine(&server, None).search(SEALS_QUERY, 10).await;

    assert_eq!(
        titles(&outcome),
        [SIERPINSKI_TITLE, SEALS_TITLE, APOLLONIAN_TITLE]
    );
    assert_eq!(outcome.relevance.status, RelevanceStatus::Disabled);
    assert!(outcome.relevance.returned_unfiltered);
    let selector = outcome
        .relevance
        .selector
        .as_ref()
        .expect("stage configured");
    assert_eq!(selector.status, SelectorStatus::Unavailable);
    assert_eq!(selector.candidates, 3);
    assert_eq!(selector.judged, 0);
    assert_eq!(selector.dropped, 0);
    assert_eq!(selector.model, None);
    assert!(selector.returned_unfiltered);
    assert!(
        selector
            .message
            .as_deref()
            .is_some_and(|message| message.contains("no selector LLM")),
        "{:?}",
        selector.message
    );
}

/// FAIL OPEN: the judge errors mid-flight — every paper survives and the
/// report carries the reason.
#[tokio::test]
async fn selector_failure_keeps_every_paper_and_carries_the_reason() {
    let server = MockServer::start().await;
    mount_seals_source(&server).await;

    let outcome = selector_engine(&server, None)
        .with_selector(Some(Arc::new(FailingJudge)))
        .search(SEALS_QUERY, 10)
        .await;

    assert_eq!(
        titles(&outcome),
        [SIERPINSKI_TITLE, SEALS_TITLE, APOLLONIAN_TITLE]
    );
    assert!(outcome.relevance.returned_unfiltered);
    let selector = outcome
        .relevance
        .selector
        .as_ref()
        .expect("stage configured");
    assert_eq!(selector.status, SelectorStatus::Failed);
    assert_eq!(selector.candidates, 3);
    assert_eq!(selector.judged, 0);
    assert_eq!(selector.dropped, 0);
    assert_eq!(selector.model.as_deref(), Some("model:failing-judge"));
    assert!(selector.returned_unfiltered);
    assert!(
        selector
            .message
            .as_deref()
            .is_some_and(|message| message.contains("judge exploded mid-flight")),
        "{:?}",
        selector.message
    );
}

/// FAIL OPEN, per candidate: a paper the judge did not rule on survives. The
/// counts distinguish "judged" from "presented", and dropping papers clears
/// the top-level `returned_unfiltered` even when the embedding stage was
/// disabled.
#[tokio::test]
async fn a_candidate_the_judge_did_not_rule_on_survives() {
    let server = MockServer::start().await;
    mount_seals_source(&server).await;

    let outcome = selector_engine(&server, None)
        .with_selector(Some(Arc::new(OmittingJudge)))
        .search(SEALS_QUERY, 10)
        .await;

    assert_eq!(titles(&outcome), [SIERPINSKI_TITLE, SEALS_TITLE]);
    assert!(
        !outcome.relevance.returned_unfiltered,
        "a selector drop must clear the top-level unfiltered flag"
    );
    let selector = outcome
        .relevance
        .selector
        .as_ref()
        .expect("stage configured");
    assert_eq!(selector.status, SelectorStatus::Applied);
    assert_eq!(selector.candidates, 3);
    assert_eq!(selector.judged, 2);
    assert_eq!(selector.dropped, 1);
    assert_eq!(selector.dropped_examples.len(), 1);
    assert_eq!(selector.dropped_examples[0].title, APOLLONIAN_TITLE);
    assert_eq!(selector.dropped_examples[0].reason, FRACTAL_REASON);
}

/// FAIL OPEN, end to end through the shipped `LlmSelector`: a judge that
/// answers prose instead of the JSON contract drops nothing.
#[tokio::test]
async fn malformed_judge_output_keeps_every_paper() {
    let server = MockServer::start().await;
    mount_seals_source(&server).await;

    let mut llm_server = mockito::Server::new_async().await;
    let chat = llm_server
        .mock("POST", "/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "choices": [{
                    "message": {"content": "The papers all look great to me!"}
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
            })
            .to_string(),
        )
        .create_async()
        .await;
    let llm = prism_llm::LlmClient::new(prism_llm::LlmConfig {
        base_url: llm_server.url(),
        model: "test-judge".into(),
        ..prism_llm::LlmConfig::default()
    });

    let outcome = selector_engine(&server, None)
        .with_selector(Some(Arc::new(prism_retrieval::LlmSelector::new(llm))))
        .search(SEALS_QUERY, 10)
        .await;
    chat.assert_async().await;

    assert_eq!(
        titles(&outcome),
        [SIERPINSKI_TITLE, SEALS_TITLE, APOLLONIAN_TITLE]
    );
    assert!(outcome.relevance.returned_unfiltered);
    let selector = outcome
        .relevance
        .selector
        .as_ref()
        .expect("stage configured");
    assert_eq!(selector.status, SelectorStatus::Failed);
    assert_eq!(selector.dropped, 0);
    assert_eq!(selector.model.as_deref(), Some("model:test-judge"));
    assert!(
        selector
            .message
            .as_deref()
            .is_some_and(|message| message.contains("selector judgement failed")),
        "{:?}",
        selector.message
    );
}
