//! Offline integration tests: the federated engine against mock HTTP
//! servers. Proves concurrency across sources, dedup by exact identifiers,
//! disk-cache reuse, and honest per-source error reporting.

use std::collections::HashMap;
use std::time::Duration;

use prism_retrieval::{EngineConfig, FailureKind, RetrievalEngine};

const ARXIV_BODY: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2401.11111v1</id>
    <published>2024-01-01T00:00:00Z</published>
    <title>Alpha paper</title>
    <summary>First.</summary>
    <author><name>A. One</name></author>
    <arxiv:doi>10.7777/SHARED</arxiv:doi>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2401.22222v1</id>
    <published>2024-02-01T00:00:00Z</published>
    <title>Beta paper</title>
    <summary>Second.</summary>
    <author><name>B. Two</name></author>
  </entry>
</feed>"#;

const CROSSREF_BODY: &str = r#"{
  "message": {
    "items": [
      {
        "DOI": "10.7777/shared",
        "title": ["Alpha paper (publisher version)"],
        "author": [{"given": "A.", "family": "One"}],
        "published": {"date-parts": [[2024, 1, 5]]},
        "abstract": "Publisher abstract, richer than arXiv's.",
        "URL": "https://publisher.example/alpha",
        "link": [{"URL": "https://publisher.example/alpha.pdf", "content-type": "application/pdf"}]
      }
    ]
  }
}"#;

fn engine_for(server_url: &str, cache_dir: Option<std::path::PathBuf>) -> RetrievalEngine {
    let mut overrides = HashMap::new();
    overrides.insert("arxiv".to_string(), server_url.to_string());
    overrides.insert("crossref".to_string(), server_url.to_string());
    let cfg = EngineConfig {
        sources: vec!["arxiv".to_string(), "crossref".to_string()],
        base_overrides: overrides,
        cache_dir,
        per_source_timeout_secs: 10,
        max_attempts: 2,
        ..EngineConfig::default()
    };
    RetrievalEngine::new(cfg)
}

#[tokio::test]
async fn search_dedups_by_doi_and_reports_every_source() {
    let mut server = mockito::Server::new_async().await;
    let arxiv_mock = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(ARXIV_BODY)
        .expect(1)
        .create_async()
        .await;
    let crossref_mock = server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(CROSSREF_BODY)
        .expect(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_for(&server.url(), Some(dir.path().to_path_buf()));
    let outcome = engine.search("test query", 10).await;

    // arXiv returned 2, Crossref 1; the shared DOI merges into one record.
    assert_eq!(outcome.papers.len(), 2);
    assert_eq!(outcome.duplicates_merged, 1);

    let merged = outcome
        .papers
        .iter()
        .find(|p| p.doi.as_deref() == Some("10.7777/shared"))
        .expect("merged record must exist");
    // Merge semantics are gap-fill, never overwrite: the arXiv record
    // arrived first with its own abstract, so that one is kept...
    assert_eq!(merged.abstract_text.as_deref(), Some("First."));
    // ...while the PDF link and publisher landing data arXiv lacked are
    // absorbed from Crossref.
    assert_eq!(
        merged.fulltext_url.as_deref(),
        Some("https://publisher.example/alpha.pdf")
    );
    // Both identifiers survive the merge.
    assert_eq!(
        merged.external_ids.get("arxiv").map(String::as_str),
        Some("2401.11111v1")
    );

    // Every source reported honestly.
    assert_eq!(outcome.source_status.len(), 2);
    assert!(outcome.source_status.iter().all(|s| s.status == "ok"));

    arxiv_mock.assert_async().await;
    crossref_mock.assert_async().await;

    // Second identical search is served from cache: no new requests.
    let again = engine.search("test query", 10).await;
    assert_eq!(again.papers.len(), 2);
    assert!(again.source_status.iter().all(|s| s.cache_hit));
    arxiv_mock.assert_async().await; // still exactly 1 hit each
    crossref_mock.assert_async().await;
}

#[tokio::test]
async fn one_source_failing_never_silences_the_others() {
    let mut server = mockito::Server::new_async().await;
    let _arxiv = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(ARXIV_BODY)
        .create_async()
        .await;
    let crossref_fail = server
        .mock("GET", "/works")
        .match_query(mockito::Matcher::Any)
        .with_status(500)
        .with_body("upstream melt")
        .expect_at_least(1)
        .create_async()
        .await;

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_for(&server.url(), Some(dir.path().to_path_buf()));
    let outcome = engine.search("test query", 10).await;

    // The healthy source still delivers; the broken one says it broke.
    assert_eq!(outcome.papers.len(), 2);
    let failed = outcome
        .source_status
        .iter()
        .find(|s| s.source == "crossref")
        .unwrap();
    assert_eq!(failed.status, "error");
    assert!(failed.error.as_deref().unwrap().contains("500"));
    assert_eq!(
        failed.failure_kind,
        Some(FailureKind::Transport),
        "a 5xx surviving retries is typed as transport"
    );
    let ok = outcome
        .source_status
        .iter()
        .find(|s| s.source == "arxiv")
        .unwrap();
    assert_eq!(ok.status, "ok");
    assert_eq!(ok.count, 2);
    crossref_fail.assert_async().await;
}

#[tokio::test]
async fn a_source_timeout_is_reported_as_timeout() {
    let mut server = mockito::Server::new_async().await;
    let _arxiv = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_body(ARXIV_BODY)
        .create_async()
        .await;

    // A black-hole endpoint: accepts connections, never answers. This forces
    // the per-source timeout to fire without depending on any real network.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let blackhole_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let _keep_alive = socket; // hold open, send nothing
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });

    let mut overrides = HashMap::new();
    overrides.insert("arxiv".to_string(), server.url());
    overrides.insert("crossref".to_string(), format!("http://{blackhole_addr}"));
    let cfg = EngineConfig {
        sources: vec!["arxiv".to_string(), "crossref".to_string()],
        base_overrides: overrides,
        cache_dir: None,
        per_source_timeout_secs: 1,
        max_attempts: 1,
        ..EngineConfig::default()
    };
    let engine = RetrievalEngine::new(cfg);
    let outcome = engine.search("test query", 10).await;

    let timed_out = outcome
        .source_status
        .iter()
        .find(|s| s.source == "crossref")
        .unwrap();
    assert_eq!(timed_out.status, "timeout");
    assert_eq!(
        timed_out.failure_kind,
        Some(FailureKind::Cancelled),
        "our per-source deadline ended the fetch — typed as cancelled"
    );
    // arXiv is unaffected.
    assert_eq!(outcome.papers.len(), 2);
}
