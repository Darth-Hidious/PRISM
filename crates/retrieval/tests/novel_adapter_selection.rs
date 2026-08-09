//! THE adapter contract, proven end to end: a source whose id is invented
//! HERE — `krx_novel_lit_7` appears nowhere in the source tree and no
//! `SourceId` variant knows it — is registered at runtime and then CHOSEN BY
//! ITS REGISTRY ID STRING through the same selection paths production uses:
//! `EngineConfig::sources` for `search`, `SweepPlan::sources` for sweeps.
//!
//! If selection ever consults the enum again (parse-then-resolve, a match
//! over variants, anything that only knows the eight built-ins), the novel
//! id stops resolving and these tests die on their status assertions.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use prism_retrieval::{
    EngineConfig, FetchCtx, Paper, RetrievalEngine, Source, SourceCaps, SourceError, SourcePage,
    SourceRegistry, SweepPlan,
};

/// Invented in this test. Not a built-in, not an enum variant, not a string
/// that occurs anywhere under `src/`.
const NOVEL_ID: &str = "krx_novel_lit_7";

struct NovelSource;

#[async_trait]
impl Source for NovelSource {
    fn id(&self) -> &'static str {
        NOVEL_ID
    }
    fn min_interval(&self) -> Duration {
        Duration::ZERO
    }
    fn initial_cursor(&self) -> &'static str {
        "0"
    }
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: 5,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        self.fetch_page(ctx, query, "0").await.map(|(p, _)| p)
    }
    async fn fetch_page(
        &self,
        _ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        Ok((
            SourcePage {
                papers: vec![Paper {
                    source: NOVEL_ID.to_string(),
                    source_id: format!("novel-{cursor}"),
                    title: format!("{query} (novel page {cursor})"),
                    authors: Vec::new(),
                    year: None,
                    published: None,
                    doi: None,
                    external_ids: Default::default(),
                    abstract_text: None,
                    url: format!("urn:novel:{cursor}"),
                    fulltext_url: None,
                    fulltext_format: None,
                    journal: None,
                }],
                raw_count: 1,
                available: Some(1),
            },
            None,
        ))
    }
}

fn registry_with_novel() -> SourceRegistry {
    // The FULL builtin registry plus the novel adapter — the realistic
    // third-party situation, not a lab registry containing only the double.
    let mut reg = SourceRegistry::builtin();
    reg.register(Arc::new(NovelSource))
        .expect("a free id must register");
    reg
}

/// `search`: the novel id is written into `EngineConfig::sources` — the
/// production selection input — and the engine must resolve it through the
/// registry, fan out to it, and hand its results back.
#[tokio::test]
async fn a_novel_adapter_is_selected_by_registry_id_string_for_search() {
    let engine = RetrievalEngine::with_registry(
        EngineConfig {
            sources: vec![NOVEL_ID.to_string()],
            cache_dir: None,
            ..EngineConfig::default()
        },
        registry_with_novel(),
    );

    let outcome = engine.search("anything", 5).await;

    assert_eq!(
        outcome.source_status.len(),
        1,
        "exactly the selected source runs — no built-in sneaks in, none is \
         silently substituted: {:?}",
        outcome.source_status
    );
    let status = &outcome.source_status[0];
    assert_eq!(status.source, NOVEL_ID);
    assert_eq!(
        status.status, "ok",
        "the novel id must RESOLVE, not report a missing adapter: {:?}",
        status.error
    );
    assert_eq!(status.count, 1);
    assert_eq!(outcome.papers.len(), 1);
    assert_eq!(
        outcome.papers[0].source, NOVEL_ID,
        "the results must come from the novel adapter itself"
    );
}

/// `run_sweep`: the novel id is written into `SweepPlan::sources` and the
/// sweep resolves it against the registry at sweep time — the second
/// production selection path, independent of the engine's fan-out list.
#[tokio::test]
async fn a_novel_adapter_is_swept_by_registry_id_string() {
    let engine = RetrievalEngine::with_registry(
        EngineConfig {
            // NOT in the engine's own fan-out selection: the PLAN chooses.
            sources: Vec::new(),
            cache_dir: None,
            ..EngineConfig::default()
        },
        registry_with_novel(),
    );
    let plan = SweepPlan {
        query: "anything".to_string(),
        sources: vec![NOVEL_ID.to_string()],
        max_pages_per_source: 3,
        per_page_limit: 5,
    };
    let dir = tempfile::tempdir().unwrap();

    let outcome = engine
        .run_sweep(&plan, &dir.path().join("state.json"))
        .await
        .expect("sweep must run");

    assert_eq!(outcome.source_status.len(), 1);
    assert_eq!(outcome.source_status[0].source, NOVEL_ID);
    assert_eq!(
        outcome.source_status[0].status, "ok",
        "the plan's novel id must resolve through the registry: {:?}",
        outcome.source_status[0].error
    );
    assert_eq!(outcome.papers.len(), 1);
    assert_eq!(outcome.papers[0].source, NOVEL_ID);
    assert!(
        outcome.finished,
        "one exhausted page with available=1 is a complete sweep"
    );
}

/// The negative control: the same novel string WITHOUT the registration must
/// be reported as a missing adapter — proving the string is resolved through
/// the registry rather than accepted on faith.
#[tokio::test]
async fn an_unregistered_id_string_reports_a_missing_adapter() {
    let engine = RetrievalEngine::new(EngineConfig {
        sources: vec![NOVEL_ID.to_string()],
        cache_dir: None,
        ..EngineConfig::default()
    });
    let outcome = engine.search("anything", 5).await;

    assert_eq!(outcome.papers.len(), 0);
    assert_eq!(outcome.source_status.len(), 1);
    let status = &outcome.source_status[0];
    assert_eq!(status.source, NOVEL_ID);
    assert_eq!(status.status, "error");
    assert_eq!(
        status.error.as_deref(),
        Some("no adapter registered for source 'krx_novel_lit_7'")
    );
}
