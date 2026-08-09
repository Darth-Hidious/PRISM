//! Completeness accounting for literature pagination.
//!
//! Every source's continuation gate must derive from the RAW record count
//! the server returned, never from how many records survived parsing. The
//! old gates compared PARSED papers against the page size, so ONE skipped
//! record (title-less entry, missing DOI, missing `bibjson`) on a full page
//! ended the chain, the source was reported `ok`, and a sweep reported
//! `finished: true` having stopped early.
//!
//! Each test serves page one FULL of raw records where at least one is
//! skippable, and pins that the source still requests page two. The mock for
//! page two carries `expect_at_least(1)` and is asserted — with the old
//! parsed-count gate the second request never happens and the test fails on
//! that assertion.
//!
//! All tests run against mock HTTP servers; no real network is touched.

use std::collections::HashMap;

use prism_retrieval::{EngineConfig, RetrievalEngine, SourceId, SweepPlan, sweep};

fn engine_for(
    source: SourceId,
    server_url: &str,
    cache_dir: Option<std::path::PathBuf>,
) -> RetrievalEngine {
    let mut overrides = HashMap::new();
    overrides.insert(source, server_url.to_string());
    RetrievalEngine::new(EngineConfig {
        sources: vec![source],
        base_overrides: overrides,
        cache_dir,
        per_source_timeout_secs: 10,
        max_attempts: 1,
        ..EngineConfig::default()
    })
}

fn plan_for(source: SourceId, max_pages: usize, per_page: usize) -> SweepPlan {
    SweepPlan {
        query: "test".to_string(),
        sources: vec![source],
        max_pages_per_source: max_pages,
        per_page_limit: per_page,
    }
}

/// Sweep `source` against `server`, with page-two mocks already registered,
/// and assert the chain CONTINUED past the skip-carrying first page — and
/// that the server-reported total reached `source_status.available`.
async fn assert_chain_continues(
    source: SourceId,
    server: &mockito::Server,
    page2: &mockito::Mock,
    expected_papers: usize,
    expected_available: Option<u64>,
) {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine_for(source, &server.url(), None);
    let plan = plan_for(source, 3, 2);
    let state_path = sweep::default_state_path(dir.path(), &plan);
    let outcome = engine.run_sweep(&plan, &state_path).await.unwrap();

    // The second page was actually requested: the skip did not end the chain.
    page2.assert_async().await;
    assert_eq!(
        outcome.papers.len(),
        expected_papers,
        "parsed papers: {:?}",
        outcome.source_status
    );
    assert_eq!(outcome.source_status.len(), 1);
    assert_eq!(
        outcome.source_status[0].status, "ok",
        "{:?}",
        outcome.source_status[0].error
    );
    assert_eq!(
        outcome.source_status[0].available, expected_available,
        "the server's own total must survive parsing into source_status"
    );
    assert!(
        outcome.finished,
        "the chain exhausted honestly at the empty second page"
    );
}

// ── arXiv ──────────────────────────────────────────────────────────────────

const ARXIV_PAGE1_ONE_SKIP: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2401.00001v1</id>
    <published>2024-01-01T00:00:00Z</published>
    <title>Kept paper</title>
    <summary>Kept.</summary>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2401.00002v1</id>
    <published>2024-01-02T00:00:00Z</published>
    <title>   </title>
    <summary>Title-less: the parser skips this one.</summary>
  </entry>
</feed>"#;

const ARXIV_EMPTY: &str =
    r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"></feed>"#;

#[tokio::test]
async fn arxiv_one_skipped_record_does_not_end_the_chain() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded("start".into(), "0".into()))
        .with_status(200)
        .with_body(ARXIV_PAGE1_ONE_SKIP)
        .expect_at_least(1)
        .create_async()
        .await;
    // The cursor advances by the RAW count (2), not the parsed count (1):
    // advancing by parsed would re-read the skipped tail forever.
    let page2 = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded("start".into(), "2".into()))
        .with_status(200)
        .with_body(ARXIV_EMPTY)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Arxiv, &server, &page2, 1, None).await;
}

/// THE headline honesty test: a sweep whose page cap fires mid-chain must
/// NOT report `finished`. With the old parsed-count gate this exact fixture
/// LIED: the skip made the page look short, the chain "exhausted", and the
/// sweep claimed completeness having read one page of many.
#[tokio::test]
async fn a_capped_sweep_over_a_skip_carrying_page_does_not_claim_finished() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded("start".into(), "0".into()))
        .with_status(200)
        .with_body(ARXIV_PAGE1_ONE_SKIP)
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_for(SourceId::Arxiv, &server.url(), None);
    let plan = plan_for(SourceId::Arxiv, 1, 2); // cap fires after page one
    let state_path = sweep::default_state_path(dir.path(), &plan);
    let outcome = engine.run_sweep(&plan, &state_path).await.unwrap();

    assert_eq!(outcome.papers.len(), 1);
    assert_eq!(outcome.source_status[0].status, "ok");
    assert!(
        !outcome.finished,
        "one full raw page under a 1-page cap means the chain was NOT \
         exhausted; reporting finished claims completeness the sweep lacks"
    );
}

/// The other half of derived completeness: when the server SAYS how many
/// records exist and the sweep consumed exactly that many raw records, the
/// page cap landing on the last page is completeness, not truncation.
#[tokio::test]
async fn a_capped_sweep_that_consumed_the_reported_total_is_finished() {
    let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/">
  <opensearch:totalResults>2</opensearch:totalResults>
  <entry>
    <id>http://arxiv.org/abs/2401.00003v1</id>
    <title>First of exactly two</title>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2401.00004v1</id>
    <title>Second of exactly two</title>
  </entry>
</feed>"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded("start".into(), "0".into()))
        .with_status(200)
        .with_body(feed)
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_for(SourceId::Arxiv, &server.url(), None);
    let plan = plan_for(SourceId::Arxiv, 1, 2);
    let state_path = sweep::default_state_path(dir.path(), &plan);
    let outcome = engine.run_sweep(&plan, &state_path).await.unwrap();

    assert_eq!(outcome.papers.len(), 2);
    assert_eq!(outcome.source_status[0].available, Some(2));
    assert!(
        outcome.finished,
        "raw records consumed == server-reported total: the cap landed on \
         the last page and completeness is DERIVED from that accounting"
    );
}

// ── OpenAlex ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn openalex_one_skipped_record_does_not_end_the_chain() {
    let page1 = r#"{
      "meta": {"count": 2},
      "results": [
        {"id": "https://openalex.org/W1", "display_name": "Kept work"},
        {"id": "https://openalex.org/W2", "display_name": "   "}
      ]
    }"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::UrlEncoded("page".into(), "1".into()))
        .with_status(200)
        .with_body(page1)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
        .with_status(200)
        .with_body(r#"{"meta": {"count": 2}, "results": []}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Openalex, &server, &page2, 1, Some(2)).await;
}

// ── Crossref ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn crossref_one_skipped_record_does_not_end_the_chain() {
    let page1 = r#"{
      "message": {
        "total-results": 2,
        "items": [
          {"DOI": "10.1/kept", "title": ["Kept item"]},
          {"title": ["No DOI: the parser skips this one"]}
        ]
      }
    }"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::UrlEncoded("offset".into(), "0".into()))
        .with_status(200)
        .with_body(page1)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::UrlEncoded("offset".into(), "2".into()))
        .with_status(200)
        .with_body(r#"{"message": {"total-results": 2, "items": []}}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Crossref, &server, &page2, 1, Some(2)).await;
}

// ── PubMed ─────────────────────────────────────────────────────────────────

/// PubMed's gate was already raw-based (the esearch id list, before esummary
/// parsing can skip a record). This pins that it STAYS raw-based: page one
/// serves two PMIDs of which esummary can only parse one.
#[tokio::test]
async fn pubmed_gate_stays_on_the_raw_esearch_ids() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/esearch.fcgi")
        .match_query(mockito::Matcher::UrlEncoded("retstart".into(), "0".into()))
        .with_status(200)
        .with_body(r#"{"esearchresult": {"count": "2", "idlist": ["111", "222"]}}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    // esummary knows only 111; 222 is skipped by the parser.
    server
        .mock("GET", "/esummary.fcgi")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(r#"{"result": {"uids": ["111"], "111": {"title": "Kept paper"}}}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/esearch.fcgi")
        .match_query(mockito::Matcher::UrlEncoded("retstart".into(), "2".into()))
        .with_status(200)
        .with_body(r#"{"esearchresult": {"count": "2", "idlist": []}}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Pubmed, &server, &page2, 1, Some(2)).await;
}

// ── Semantic Scholar ───────────────────────────────────────────────────────

#[tokio::test]
async fn semantic_scholar_one_skipped_record_does_not_end_the_chain() {
    let page1 = r#"{
      "total": 2,
      "data": [
        {"paperId": "kept1", "title": "Kept paper"},
        {"paperId": "skip1", "title": ""}
      ]
    }"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/paper/search")
        .match_query(mockito::Matcher::UrlEncoded("offset".into(), "0".into()))
        .with_status(200)
        .with_body(page1)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/paper/search")
        .match_query(mockito::Matcher::UrlEncoded("offset".into(), "2".into()))
        .with_status(200)
        .with_body(r#"{"total": 2, "data": []}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::SemanticScholar, &server, &page2, 1, Some(2)).await;
}

// ── Europe PMC preprints ───────────────────────────────────────────────────

/// Europe PMC pages by server cursor, so a single skip never ended its chain
/// — but a page whose EVERY record the parser skipped did, while the server
/// still advanced the mark. The gate must use the raw result count.
#[tokio::test]
async fn europepmc_an_all_skipped_page_does_not_end_the_chain() {
    let page1 = r#"{
      "hitCount": 2,
      "nextCursorMark": "MARK2",
      "resultList": {"result": [
        {"id": "PPR1", "title": "   "},
        {"id": "PPR2"}
      ]}
    }"#;
    // Terminal page: same mark back, no results.
    let page2_body = r#"{
      "hitCount": 2,
      "nextCursorMark": "MARK2",
      "resultList": {"result": []}
    }"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/search")
        .match_query(mockito::Matcher::UrlEncoded(
            "cursorMark".into(),
            "*".into(),
        ))
        .with_status(200)
        .with_body(page1)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/search")
        .match_query(mockito::Matcher::UrlEncoded(
            "cursorMark".into(),
            "MARK2".into(),
        ))
        .with_status(200)
        .with_body(page2_body)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Preprints, &server, &page2, 0, Some(2)).await;
}

// ── ChemRxiv ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn chemrxiv_one_skipped_record_does_not_end_the_chain() {
    let page1 = r#"{
      "totalCount": 2,
      "itemHits": [
        {"item": {"id": "a1", "title": "Kept preprint"}},
        {"item": {"id": "a2", "title": "   "}}
      ]
    }"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/items")
        .match_query(mockito::Matcher::UrlEncoded("skip".into(), "0".into()))
        .with_status(200)
        .with_body(page1)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/items")
        .match_query(mockito::Matcher::UrlEncoded("skip".into(), "2".into()))
        .with_status(200)
        .with_body(r#"{"totalCount": 2, "itemHits": []}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Chemrxiv, &server, &page2, 1, Some(2)).await;
}

// ── DOAJ ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn doaj_one_skipped_record_does_not_end_the_chain() {
    let page1 = r#"{
      "total": 2,
      "results": [
        {"id": "d1", "bibjson": {"title": "Kept article"}},
        {"id": "d2"}
      ]
    }"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/test")
        .match_query(mockito::Matcher::UrlEncoded("page".into(), "1".into()))
        .with_status(200)
        .with_body(page1)
        .expect_at_least(1)
        .create_async()
        .await;
    let page2 = server
        .mock("GET", "/test")
        .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
        .with_status(200)
        .with_body(r#"{"total": 2, "results": []}"#)
        .expect_at_least(1)
        .create_async()
        .await;
    assert_chain_continues(SourceId::Doaj, &server, &page2, 1, Some(2)).await;
}

// ── What exists vs what was returned (B) ───────────────────────────────────

/// The server's total must reach `source_status.available` so `count <
/// available` is visible to the caller instead of being discarded by the
/// parser.
#[tokio::test]
async fn search_surfaces_the_server_reported_total_as_available() {
    let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/">
  <opensearch:totalResults>250</opensearch:totalResults>
  <entry>
    <id>http://arxiv.org/abs/2401.00005v1</id>
    <title>One of 250</title>
  </entry>
</feed>"#;
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(feed)
        .create_async()
        .await;

    let engine = engine_for(SourceId::Arxiv, &server.url(), None);
    let outcome = engine.search("test", 10).await;

    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(status.status, "ok", "{:?}", status.error);
    assert_eq!(status.count, 1);
    assert_eq!(
        status.available,
        Some(250),
        "the ok is a SLICE of 250 available records and must say so"
    );
}

// ── Replay drift visibility (D) ────────────────────────────────────────────

/// A resume whose cache entries lapsed cannot be replayed drift-free: the
/// refetched pages must be COUNTED in `replay_refetches`, not silently
/// absorbed into an outcome that looks like a faithful replay.
#[tokio::test]
async fn a_replay_that_had_to_refetch_reports_the_drift() {
    let feed = |title: &str, id: &str| {
        format!(
            r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry><id>http://arxiv.org/abs/{id}v1</id><title>{title}</title></entry>
</feed>"#
        )
    };
    let mut server = mockito::Server::new_async().await;
    for (start, title, id) in [
        (0usize, "Page zero", "2401.11110"),
        (1, "Page one", "2401.11111"),
    ] {
        server
            .mock("GET", "/")
            .match_query(mockito::Matcher::UrlEncoded(
                "start".into(),
                start.to_string(),
            ))
            .with_status(200)
            .with_body(feed(title, id))
            .expect_at_least(1)
            .create_async()
            .await;
    }
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded("start".into(), "2".into()))
        .with_status(200)
        .with_body(ARXIV_EMPTY)
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    let engine = engine_for(SourceId::Arxiv, &server.url(), Some(cache_dir.clone()));
    let plan = plan_for(SourceId::Arxiv, 3, 1);
    let state_path = sweep::default_state_path(dir.path(), &plan);

    let run1 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run1.papers.len(), 2);
    assert_eq!(
        run1.replay_refetches, 0,
        "a fresh sweep replays nothing, so nothing can drift"
    );

    // Simulate TTL expiry of page one's entry.
    let needle = "start=1";
    for entry in std::fs::read_dir(&cache_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json")
            && std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains(needle)
        {
            std::fs::remove_file(path).unwrap();
        }
    }

    let run2 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run2.papers.len(), 2);
    assert_eq!(run2.pages_from_cache, 2);
    assert_eq!(run2.pages_fetched, 1);
    assert_eq!(
        run2.replay_refetches, 1,
        "the lapsed page was refetched live during replay; the outcome must \
         say the replay was not drift-free"
    );
}
