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
