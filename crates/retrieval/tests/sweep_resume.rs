//! Resume honesty for checkpointed sweeps.
//!
//! F1: when a completed page's cache entry is gone AND the source
//! transiently fails on it, the "restart source from beginning" branch must
//! reset the page budget — otherwise the re-fetched chain hits the cap early
//! and tail papers are silently lost while the sweep claims completeness.
//!
//! F4: duplicates_merged must not be seeded from the checkpoint, or every
//! duplicate is counted once per resume.
//!
//! F9: pages_from_cache must measure "no network happened", not "was marked
//! done" — a replayed page whose cache entry vanished is a refetch.
//!
//! All tests enter the degraded branches against a mock arXiv endpoint; no
//! real network is touched.

use std::collections::HashMap;

use prism_retrieval::{EngineConfig, RetrievalEngine, SourceId, SweepPlan, sweep};

fn feed(arxiv_id: &str, title: &str, doi: Option<&str>) -> String {
    let doi = doi
        .map(|d| format!("<arxiv:doi>{d}</arxiv:doi>"))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/{arxiv_id}v1</id>
    <published>2024-01-01T00:00:00Z</published>
    <title>{title}</title>
    <summary>Abstract of {title}.</summary>
    <author><name>A. One</name></author>
    {doi}
  </entry>
</feed>"#
    )
}

const EMPTY_FEED: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
</feed>"#;

fn engine_for(server_url: &str, cache_dir: std::path::PathBuf) -> RetrievalEngine {
    let mut overrides = HashMap::new();
    overrides.insert(SourceId::Arxiv, server_url.to_string());
    let cfg = EngineConfig {
        sources: vec![SourceId::Arxiv],
        base_overrides: overrides,
        cache_dir: Some(cache_dir),
        per_source_timeout_secs: 10,
        // One attempt: a 500 must surface as a page failure, not be hidden
        // by a retry that consumes the transient-failure mock.
        max_attempts: 1,
        ..EngineConfig::default()
    };
    RetrievalEngine::new(cfg)
}

fn plan(max_pages: usize) -> SweepPlan {
    SweepPlan {
        query: "test".to_string(),
        sources: vec![SourceId::Arxiv],
        max_pages_per_source: max_pages,
        per_page_limit: 1,
    }
}

/// Delete the cached entry for the page fetched with `start=N` (simulates
/// TTL expiry / a swallowed cache.put failure).
fn drop_cached_page(cache_dir: &std::path::Path, start: usize) {
    let needle = format!("start={start}");
    for entry in std::fs::read_dir(cache_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json")
            && std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains(&needle)
        {
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test]
async fn degraded_resume_does_not_lose_papers_or_claim_completeness() {
    let mut server = mockito::Server::new_async().await;

    // Pages 0..=3 each serve one distinct paper; every page is full, so the
    // cursor chain never exhausts within the plan's cap.
    for i in 0..=3 {
        server
            .mock("GET", "/")
            .match_query(mockito::Matcher::UrlEncoded(
                "start".to_string(),
                i.to_string(),
            ))
            .with_status(200)
            .with_body(feed(&format!("2401.1111{i}"), &format!("Paper {i}"), None))
            .expect_at_least(1)
            .create_async()
            .await;
    }
    // Page 1's transient failure is registered AFTER run 1's healthy mock
    // (mockito serves the first mock whose expectations are unmet), with a
    // further healthy mock registered after it for the restart's refetch.
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded(
            "start".to_string(),
            "1".to_string(),
        ))
        .with_status(500)
        .with_body("upstream melt")
        .expect(1)
        .create_async()
        .await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded(
            "start".to_string(),
            "1".to_string(),
        ))
        .with_status(200)
        .with_body(feed("2401.11111", "Paper 1", None))
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    let engine = engine_for(&server.url(), cache_dir.clone());
    let plan = plan(4);
    let state_path = sweep::default_state_path(dir.path(), &plan);

    // Run 1: fresh sweep fetches all four papers. Capped before the chain
    // exhausts, so it must not claim completeness.
    let run1 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run1.papers.len(), 4);
    assert_eq!(run1.pages_fetched, 4);
    assert_eq!(run1.pages_from_cache, 0);
    assert!(!run1.finished, "a capped sweep must not claim completeness");

    // Ordinary trigger: page 1's cache entry is gone (TTL expiry)...
    drop_cached_page(&cache_dir, 1);

    // Run 2: the replay of page 1 misses the cache AND the source fails on
    // it (the transient 500), entering the restart branch. The refetched
    // chain must deliver every paper again.
    let run2 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(
        run2.papers.len(),
        4,
        "resume lost papers: the restart branch ate the page budget"
    );
    // The replayed-then-refetched page 0 merges once, no more.
    assert_eq!(run2.duplicates_merged, 1);
    // Page 1's replay failed on the network; pages 0..3 were then refetched,
    // pages 0/2/3 served from the still-cached entries, page 1 over the
    // network. Counts follow real traffic, not bookkeeping.
    assert_eq!(run2.pages_from_cache, 4);
    assert_eq!(run2.pages_fetched, 1);
    assert!(
        !run2.finished,
        "the chain was never exhausted; finished must stay false"
    );
}

#[tokio::test]
async fn replayed_page_refetched_over_network_is_counted_as_fetched() {
    let mut server = mockito::Server::new_async().await;
    for i in [0usize, 1] {
        server
            .mock("GET", "/")
            .match_query(mockito::Matcher::UrlEncoded(
                "start".to_string(),
                i.to_string(),
            ))
            .with_status(200)
            .with_body(feed(&format!("2401.2222{i}"), &format!("Paper {i}"), None))
            .expect_at_least(1)
            .create_async()
            .await;
    }
    // Page 2 is empty: it exhausts the chain honestly.
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded(
            "start".to_string(),
            "2".to_string(),
        ))
        .with_status(200)
        .with_body(EMPTY_FEED)
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    let engine = engine_for(&server.url(), cache_dir.clone());
    let plan = plan(3);
    let state_path = sweep::default_state_path(dir.path(), &plan);

    let run1 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run1.papers.len(), 2);
    assert!(run1.finished, "chain exhausted within the cap");

    // Page 1's cache entry disappears; the source is healthy on the resume.
    drop_cached_page(&cache_dir, 1);

    let run2 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run2.papers.len(), 2);
    assert!(run2.finished);
    // Pages 0 and 2 replayed from cache; page 1 went over the network and
    // must count as FETCHED — "0 refetched" may only mean zero refetches.
    assert_eq!(run2.pages_from_cache, 2);
    assert_eq!(run2.pages_fetched, 1);
}

#[tokio::test]
async fn resume_does_not_double_count_merged_duplicates() {
    let mut server = mockito::Server::new_async().await;
    // Two pages carrying the SAME work under different arXiv ids — a
    // cross-page duplicate merged by DOI.
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded(
            "start".to_string(),
            "0".to_string(),
        ))
        .with_status(200)
        .with_body(feed("2401.33330", "Shared work", Some("10.7777/shared")))
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/")
        .match_query(mockito::Matcher::UrlEncoded(
            "start".to_string(),
            "1".to_string(),
        ))
        .with_status(200)
        .with_body(feed(
            "2401.33331",
            "Shared work (v2)",
            Some("10.7777/shared"),
        ))
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    let engine = engine_for(&server.url(), cache_dir.clone());
    let plan = plan(2);
    let state_path = sweep::default_state_path(dir.path(), &plan);

    let run1 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run1.papers.len(), 1);
    assert_eq!(run1.duplicates_merged, 1);

    // Fully cached, fully healthy resume: the replay re-observes the merge,
    // so it must count ONCE — not once per run.
    let run2 = engine.run_sweep(&plan, &state_path).await.unwrap();
    assert_eq!(run2.papers.len(), 1);
    assert_eq!(
        run2.duplicates_merged, 1,
        "duplicates_merged double-counted across the resume"
    );
    assert_eq!(run2.pages_from_cache, 2);
    assert_eq!(run2.pages_fetched, 0);
}
