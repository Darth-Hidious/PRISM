//! Declaration 4 at PRODUCTION dispatch: an engine built by
//! `RetrievalEngine::new`, the builtin adapters, real HTTP responses from a
//! mock server — and the typed failure kind lands in
//! `source_status.failure_kind`, while the `status` string and the `error`
//! text stay byte-for-byte what they were before the taxonomy existed.
//!
//! Each kind gets its own test so collapsing any two kinds into one kills
//! the test for the kind that vanished, not a shared umbrella.

use std::collections::HashMap;
use std::time::Duration;

use prism_retrieval::{EngineConfig, FailureKind, RetrievalEngine, SourceStatus};

fn engine_for(id: &str, base: String, timeout_secs: u64) -> RetrievalEngine {
    RetrievalEngine::new(EngineConfig {
        sources: vec![id.to_string()],
        base_overrides: HashMap::from([(id.to_string(), base)]),
        cache_dir: None,
        per_source_timeout_secs: timeout_secs,
        max_attempts: 1,
        ..EngineConfig::default()
    })
}

/// Run one arXiv search against a mock that answers with `http_status`.
async fn arxiv_status_for(http_status: usize) -> SourceStatus {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(http_status)
        .with_body("nope")
        .expect_at_least(1)
        .create_async()
        .await;
    let engine = engine_for("arxiv", server.url(), 10);
    let outcome = engine.search("q", 5).await;
    assert_eq!(outcome.source_status.len(), 1);
    outcome.source_status.into_iter().next().unwrap()
}

#[tokio::test]
async fn a_401_is_typed_auth_with_the_same_error_string() {
    let status = arxiv_status_for(401).await;
    assert_eq!(status.status, "error", "the status vocabulary is unchanged");
    assert_eq!(status.failure_kind, Some(FailureKind::Auth));
    let msg = status.error.as_deref().unwrap();
    assert!(
        msg.contains("HTTP 401 Unauthorized") && msg.contains("after 1 attempt(s)"),
        "the pre-taxonomy error text must survive verbatim: {msg}"
    );
}

#[tokio::test]
async fn a_403_is_typed_auth() {
    let status = arxiv_status_for(403).await;
    assert_eq!(status.status, "error");
    assert_eq!(status.failure_kind, Some(FailureKind::Auth));
}

#[tokio::test]
async fn a_429_is_typed_rate_limited_not_a_generic_error() {
    let status = arxiv_status_for(429).await;
    assert_eq!(status.status, "error");
    assert_eq!(
        status.failure_kind,
        Some(FailureKind::RateLimited),
        "throttling must be distinguishable from a dead network — backoff \
         depends on it"
    );
    assert!(
        status.error.as_deref().unwrap().contains("HTTP 429"),
        "{:?}",
        status.error
    );
}

#[tokio::test]
async fn a_400_is_typed_unsupported_query() {
    let status = arxiv_status_for(400).await;
    assert_eq!(status.status, "error");
    assert_eq!(
        status.failure_kind,
        Some(FailureKind::UnsupportedQuery),
        "a refused query must not look retryable"
    );
}

#[tokio::test]
async fn an_unparseable_body_is_typed_malformed() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body("<html>this is not the JSON you are looking for</html>")
        .expect_at_least(1)
        .create_async()
        .await;
    let engine = engine_for("crossref", server.url(), 10);
    let outcome = engine.search("q", 5).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "error");
    assert_eq!(
        status.failure_kind,
        Some(FailureKind::Malformed),
        "the server ANSWERED; this is not a transport failure: {:?}",
        status.error
    );
}

#[tokio::test]
async fn a_refused_connection_is_typed_transport() {
    // Nothing listens on port 1.
    let engine = engine_for("arxiv", "http://127.0.0.1:1".to_string(), 10);
    let outcome = engine.search("q", 5).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "error");
    assert_eq!(status.failure_kind, Some(FailureKind::Transport));
    assert!(
        status
            .error
            .as_deref()
            .unwrap()
            .contains("failed to complete"),
        "the pre-taxonomy transport wording must survive: {:?}",
        status.error
    );
}

#[tokio::test]
async fn a_deadline_expiry_is_typed_cancelled_and_still_reports_timeout() {
    // A black-hole endpoint: accepts connections, never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let blackhole = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let _keep_alive = socket;
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });

    let engine = engine_for("arxiv", format!("http://{blackhole}"), 1);
    let outcome = engine.search("q", 5).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(
        status.status, "timeout",
        "the status vocabulary is unchanged"
    );
    assert_eq!(status.failure_kind, Some(FailureKind::Cancelled));
    assert_eq!(
        status.error.as_deref(),
        Some("exceeded per-source timeout of 1s"),
        "the pre-taxonomy timeout wording must survive verbatim"
    );
}

/// The wire format: the kind serializes snake_case, and ONLY failures carry
/// the key — a success status serializes exactly as it did before the field
/// existed.
#[tokio::test]
async fn the_kind_serializes_snake_case_and_only_on_failures() {
    let failed = arxiv_status_for(429).await;
    let json = serde_json::to_value(&failed).unwrap();
    assert_eq!(json["failure_kind"], "rate_limited");

    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"></feed>"#)
        .create_async()
        .await;
    let engine = engine_for("arxiv", server.url(), 10);
    let outcome = engine.search("q", 5).await;
    let json = serde_json::to_value(&outcome.source_status[0]).unwrap();
    assert_eq!(outcome.source_status[0].status, "ok");
    assert!(
        json.get("failure_kind").is_none(),
        "an ok status must serialize exactly as before the field existed: {json}"
    );
}

// ── Live-measured source failures (2026-08-31) and their remedies ─────────

/// Serialize env mutation with the workspace lock, restoring on drop so a
/// failed assertion cannot leak state into other tests.
struct KeyGuard {
    saved: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl KeyGuard {
    fn with(value: Option<&str>) -> Self {
        let lock = prism_runtime::offline::test_support::env_lock();
        let saved = std::env::var("SEMANTIC_SCHOLAR_API_KEY").ok();
        unsafe {
            match value {
                Some(v) => std::env::set_var("SEMANTIC_SCHOLAR_API_KEY", v),
                None => std::env::remove_var("SEMANTIC_SCHOLAR_API_KEY"),
            }
        }
        Self { saved, _lock: lock }
    }
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        unsafe {
            match self.saved.take() {
                Some(v) => std::env::set_var("SEMANTIC_SCHOLAR_API_KEY", v),
                None => std::env::remove_var("SEMANTIC_SCHOLAR_API_KEY"),
            }
        }
    }
}

/// The unauthenticated Semantic Scholar pool 429s on effectively every call.
/// The status must not stop at "HTTP 429" — it names the remedy, so the
/// operator learns about the key path from the failure itself.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn semantic_scholar_429_names_the_api_key_remedy() {
    let _env = KeyGuard::with(None);
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/paper/search")
        .match_query(mockito::Matcher::Any)
        .with_status(429)
        .with_body(r#"{"message": "Too Many Requests"}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    let engine = engine_for("semantic_scholar", server.url(), 10);
    let outcome = engine.search("q", 5).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "error");
    assert_eq!(status.failure_kind, Some(FailureKind::RateLimited));
    let msg = status.error.as_deref().unwrap();
    assert!(
        msg.contains("SEMANTIC_SCHOLAR_API_KEY"),
        "the failure must name the remedy: {msg}"
    );
    // The remedy must be reachable from where the reader is: the TUI's
    // command palette has a Settings entry for exactly this key.
    assert!(
        msg.contains("Search sources & keys"),
        "the message names the palette entry that sets the key: {msg}"
    );
    assert!(msg.contains("HTTP 429"), "the raw status survives: {msg}");
}

/// With a key configured the request carries `x-api-key` — and ONLY the
/// Semantic Scholar request does; the mock refuses unkeyed requests, so a
/// pass proves the header went on the wire.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn semantic_scholar_sends_the_configured_api_key() {
    let _env = KeyGuard::with(Some("test-key-123"));
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/paper/search")
        .match_query(mockito::Matcher::Any)
        .match_header("x-api-key", "test-key-123")
        .with_status(200)
        .with_body(r#"{"total": 0, "data": []}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    let engine = engine_for("semantic_scholar", server.url(), 10);
    let outcome = engine.search("q", 5).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(
        status.status, "ok",
        "the keyed request must match the mock's x-api-key expectation: {:?}",
        status.error
    );
}

/// chemrxiv.org now fronts its public API with a browser-only Cloudflare
/// challenge (HTTP 403, `cf-mitigated: challenge`). The status must state
/// that reason — and where ChemRxiv preprints still arrive from — instead
/// of a bare 403 that reads like a transient fault.
#[tokio::test]
async fn chemrxiv_403_names_the_cloudflare_challenge_and_the_alternative() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/items")
        .match_query(mockito::Matcher::Any)
        .with_status(403)
        .with_body("<!DOCTYPE html><title>Just a moment...</title>")
        .expect_at_least(1)
        .create_async()
        .await;
    let engine = engine_for("chemrxiv", server.url(), 10);
    let outcome = engine.search("q", 5).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "error");
    assert_eq!(status.failure_kind, Some(FailureKind::Auth));
    let msg = status.error.as_deref().unwrap();
    assert!(
        msg.contains("Cloudflare") && msg.contains("preprints_europepmc"),
        "the failure must state the real reason and the alternative: {msg}"
    );
    assert!(msg.contains("HTTP 403"), "the raw status survives: {msg}");
}

/// Unauthenticated, the shared pool's 429 is deterministic within the retry
/// horizon: the in-band retries burned ~3.7 s per search and never once
/// succeeded. One attempt gets the honest answer fast — the mock counts.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn semantic_scholar_unauthenticated_429_is_not_retried_in_band() {
    let _env = KeyGuard::with(None);
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/paper/search")
        .match_query(mockito::Matcher::Any)
        .with_status(429)
        .with_body(r#"{"message": "Too Many Requests"}"#)
        .expect(1)
        .create_async()
        .await;
    let engine = RetrievalEngine::new(EngineConfig {
        sources: vec!["semantic_scholar".to_string()],
        base_overrides: HashMap::from([("semantic_scholar".to_string(), server.url())]),
        cache_dir: None,
        per_source_timeout_secs: 10,
        max_attempts: 3,
        ..EngineConfig::default()
    });
    let outcome = engine.search("q", 5).await;

    mock.assert_async().await;
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "error");
    assert_eq!(status.failure_kind, Some(FailureKind::RateLimited));
}

/// With a key configured the engine's FULL retry budget applies — a keyed
/// 429 is a genuine, transient rate limit, not the exhausted shared pool.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn semantic_scholar_keyed_429_keeps_the_engine_retry_budget() {
    let _env = KeyGuard::with(Some("test-key-123"));
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/paper/search")
        .match_query(mockito::Matcher::Any)
        .match_header("x-api-key", "test-key-123")
        .with_status(429)
        .with_body(r#"{"message": "Too Many Requests"}"#)
        .expect(3)
        .create_async()
        .await;
    let engine = RetrievalEngine::new(EngineConfig {
        sources: vec!["semantic_scholar".to_string()],
        base_overrides: HashMap::from([("semantic_scholar".to_string(), server.url())]),
        cache_dir: None,
        per_source_timeout_secs: 30,
        max_attempts: 3,
        ..EngineConfig::default()
    });
    let outcome = engine.search("q", 5).await;

    mock.assert_async().await;
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "error");
    assert_eq!(status.failure_kind, Some(FailureKind::RateLimited));
}
