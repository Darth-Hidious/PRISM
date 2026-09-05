//! Declaration 2: capabilities VERIFIED against the translator, not asserted.
//!
//! Each built-in adapter declares what it can serve (`Source::capabilities`).
//! These tests hold every declaration against the WIRE REQUEST the adapter's
//! translator actually emits, via a mock server that only answers when the
//! request carries the declared value. A declared `max_page_size` the
//! translator does not honour — the "advertised capability the translator
//! silently drops" class of lie — misses the mock, fails the source, and
//! fails the test. Same for a declared `max_offset` the successor-cursor
//! gate does not enforce, in both directions (declared deeper than the gate,
//! declared shallower than the gate).
//!
//! Adapters come from `SourceRegistry::builtin()` and page-size checks run
//! through `RetrievalEngine::new` + `search` — the production registry and
//! the production dispatch path, not doubles.
//!
//! Scope, stated honestly: literature sources have NO caller-selectable
//! filter surface beyond the free-text query (see `SourceCaps` docs), so
//! there is deliberately no filter-vocabulary declaration to verify. And a
//! `max_offset` of `None` means "the translator imposes no ceiling", which
//! is the absence of behaviour — it cannot be proven by a finite test and
//! is left unverified rather than fake-verified.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use prism_retrieval::ratelimit::RateLimiter;
use prism_retrieval::{EngineConfig, FetchCtx, RetrievalEngine, SourceRegistry};

/// Drive `id` through a real engine search asking for MORE rows than the
/// adapter declares it can serve per page. The mock answers only a request
/// whose page-size parameter equals the DECLARED `max_page_size`, so a
/// declaration the translator does not honour turns into a failed source.
async fn assert_wire_page_size(id: &str, path: &str, param: &str, empty_body: &str) {
    let mut server = mockito::Server::new_async().await;
    let caps = SourceRegistry::builtin()
        .get(id)
        .unwrap_or_else(|| panic!("built-in source '{id}' must be registered"))
        .capabilities();
    let mock = server
        .mock("GET", path)
        .match_query(mockito::Matcher::UrlEncoded(
            param.into(),
            caps.max_page_size.to_string(),
        ))
        .with_status(200)
        .with_body(empty_body)
        .expect(1)
        .create_async()
        .await;

    let engine = RetrievalEngine::new(EngineConfig {
        sources: vec![id.to_string()],
        base_overrides: HashMap::from([(id.to_string(), server.url())]),
        cache_dir: None,
        per_source_timeout_secs: 10,
        max_attempts: 1,
        ..EngineConfig::default()
    });
    // Ask for more than the declaration allows; the translator must clamp
    // the wire request to exactly the declared maximum.
    let outcome = engine.search("q", caps.max_page_size + 57).await;

    assert_eq!(outcome.source_status.len(), 1);
    assert_eq!(
        outcome.source_status[0].status, "ok",
        "'{id}' declared max_page_size={} but its translator put something \
         else on the wire ({param}=... missed the mock): {:?}",
        caps.max_page_size, outcome.source_status[0].error
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn arxiv_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "arxiv",
        "/",
        "max_results",
        r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"></feed>"#,
    )
    .await;
}

#[tokio::test]
async fn openalex_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "openalex",
        "/works",
        "per-page",
        r#"{"results": [], "meta": {"count": 0}}"#,
    )
    .await;
}

#[tokio::test]
async fn crossref_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "crossref",
        "/works",
        "rows",
        r#"{"message": {"items": [], "total-results": 0}}"#,
    )
    .await;
}

#[tokio::test]
async fn pubmed_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "pubmed",
        "/esearch.fcgi",
        "retmax",
        r#"{"esearchresult": {"idlist": [], "count": "0"}}"#,
    )
    .await;
}

#[tokio::test]
async fn semantic_scholar_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "semantic_scholar",
        "/paper/search",
        "limit",
        r#"{"data": [], "total": 0}"#,
    )
    .await;
}

#[tokio::test]
async fn europepmc_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "preprints_europepmc",
        "/search",
        "pageSize",
        r#"{"resultList": {"result": []}, "hitCount": 0}"#,
    )
    .await;
}

#[tokio::test]
async fn chemrxiv_wire_page_size_matches_its_declaration() {
    assert_wire_page_size(
        "chemrxiv",
        "/items",
        "limit",
        r#"{"itemHits": [], "totalCount": 0}"#,
    )
    .await;
}

#[tokio::test]
async fn doaj_wire_page_size_matches_its_declaration() {
    // DOAJ carries the query in the PATH: `{base}/{query}?pageSize=...`.
    assert_wire_page_size("doaj", "/q", "pageSize", r#"{"results": [], "total": 0}"#).await;
}

#[tokio::test]
async fn osti_wire_page_size_matches_its_declaration() {
    // OSTI pages with `rows`/`page`; the body is a bare array.
    assert_wire_page_size("osti", "/records", "rows", "[]").await;
}

#[tokio::test]
async fn ntrs_wire_page_size_matches_its_declaration() {
    // NTRS pages with `page[size]`/`page[from]` — the bare `size` parameter
    // is ignored by the live endpoint, so the declaration is only honoured
    // if the translator emits the bracketed form.
    assert_wire_page_size(
        "ntrs",
        "/citations/search",
        "page[size]",
        r#"{"stats": {"total": 0}, "results": []}"#,
    )
    .await;
}

// ── max_offset: the declared paging ceiling versus the successor gate ─────

/// Bare fetch context for driving one adapter directly — the documented
/// hand-built-context case for adapter-level tests.
fn bare_ctx(id: &str, base: String, limit: usize) -> FetchCtx {
    FetchCtx {
        client: reqwest::Client::new(),
        headers: reqwest::header::HeaderMap::new(),
        mailto: None,
        limit,
        base_overrides: HashMap::from([(id.to_string(), base)]),
        limiters: HashMap::new(),
        cache: None,
        max_attempts: 1,
        cache_hits: std::sync::Mutex::new(HashMap::new()),
        network_fetches: std::sync::atomic::AtomicUsize::new(0),
        cache_fetches: std::sync::atomic::AtomicUsize::new(0),
        fulltext_limiter: Arc::new(RateLimiter::new(Duration::ZERO)),
        extra_headers: HashMap::new(),
    }
}

/// Serve FULL pages (2 raw records against a page size of 2, with a huge
/// server-side total) at any offset, so only the translator's own ceiling
/// can end the chain — then hold that ceiling against the declaration:
///  * a full page just below the declared ceiling must continue EXACTLY to
///    it (dies if the gate is lower than declared, i.e. the declaration
///    overclaims reach);
///  * a full page at the declared ceiling must end the chain (dies if the
///    gate is higher than declared, i.e. the translator pages past what it
///    declared).
async fn assert_offset_ceiling(id: &str, path: &str, full_page_of_two: &str) {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", path)
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(full_page_of_two)
        .expect_at_least(2)
        .create_async()
        .await;
    let src = SourceRegistry::builtin()
        .get(id)
        .unwrap_or_else(|| panic!("built-in source '{id}' must be registered"));
    let ceiling = src
        .capabilities()
        .max_offset
        .unwrap_or_else(|| panic!("'{id}' declares a paging ceiling")) as usize;
    let ctx = bare_ctx(id, server.url(), 2);

    let (page, next) = src
        .fetch_page(&ctx, "q", &(ceiling - 2).to_string())
        .await
        .expect("page below the ceiling must fetch");
    assert_eq!(
        page.raw_count, 2,
        "fixture must be a FULL page, or the raw-count gate ends the chain \
         and the ceiling is never exercised"
    );
    assert_eq!(
        next,
        Some(ceiling.to_string()),
        "'{id}': a full page just below the declared ceiling must continue \
         exactly to it"
    );

    let (_, next) = src
        .fetch_page(&ctx, "q", &ceiling.to_string())
        .await
        .expect("page at the ceiling must fetch");
    assert_eq!(
        next, None,
        "'{id}': the translator must never request past its declared \
         max_offset"
    );
}

#[tokio::test]
async fn crossref_offset_ceiling_matches_its_declaration() {
    assert_offset_ceiling(
        "crossref",
        "/works",
        r#"{"message": {"total-results": 999999, "items": [
            {"DOI": "10.1/a", "title": ["Alpha"]},
            {"DOI": "10.1/b", "title": ["Beta"]}
        ]}}"#,
    )
    .await;
}

#[tokio::test]
async fn semantic_scholar_offset_ceiling_matches_its_declaration() {
    assert_offset_ceiling(
        "semantic_scholar",
        "/paper/search",
        r#"{"total": 999999, "data": [
            {"title": "Alpha", "paperId": "p1"},
            {"title": "Beta", "paperId": "p2"}
        ]}"#,
    )
    .await;
}

#[tokio::test]
async fn ntrs_offset_ceiling_matches_its_declaration() {
    // The live window 400s past `from + size > 10_000`; the declared ceiling
    // (10_000 − MAX_PAGE_SIZE) keeps every declared-size page inside it.
    assert_offset_ceiling(
        "ntrs",
        "/citations/search",
        r#"{"stats": {"total": 999999}, "results": [
            {"id": 1, "title": "Alpha"},
            {"id": 2, "title": "Beta"}
        ]}"#,
    )
    .await;
}
