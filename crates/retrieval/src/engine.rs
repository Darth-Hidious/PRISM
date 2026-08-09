//! The federated search orchestrator: concurrent across sources, polite
//! within a source, cached on disk, honest about every outcome.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::cache::DiskCache;
use crate::model::{Paper, SearchOutcome, SourceStatus};
use crate::ratelimit::RateLimiter;
use crate::sources::{self, FetchCtx, Source, SourceId, SourceRegistry, all_sources};

pub const DEFAULT_USER_AGENT: &str = concat!(
    "prism-retrieval/",
    env!("CARGO_PKG_VERSION"),
    " (research tool)"
);

/// Engine configuration. All fields have sensible builder defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub user_agent: String,
    /// Contact address for polite pools (OpenAlex, Crossref).
    pub mailto: Option<String>,
    /// Sources to fan out to, in deterministic reporting order.
    pub sources: Vec<SourceId>,
    pub per_source_timeout_secs: u64,
    pub cache_dir: Option<std::path::PathBuf>,
    pub cache_ttl_secs: u64,
    pub max_attempts: u32,
    /// Test/mirror overrides, keyed by source name.
    pub base_overrides: HashMap<SourceId, String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            mailto: None,
            sources: all_sources(),
            per_source_timeout_secs: 30,
            cache_dir: default_cache_dir(),
            cache_ttl_secs: 24 * 60 * 60,
            max_attempts: 3,
            base_overrides: HashMap::new(),
        }
    }
}

pub fn default_cache_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".prism").join("retrieval").join("cache"))
}

pub struct RetrievalEngine {
    cfg: EngineConfig,
    client: Client,
    /// Polite limiters keyed by source id string. Populated for every
    /// selected (and registered) source from the adapter's `min_interval`.
    limiters: HashMap<String, Arc<RateLimiter>>,
    /// Every available source. Dispatch goes through this, never a match.
    registry: SourceRegistry,
    /// The sources this engine fans out to, in reporting order.
    selected: Vec<Arc<dyn Source>>,
    /// Configured ids that resolved to no adapter, in configuration order.
    /// Reported by every `search` as an error status — a configured source
    /// must never silently vanish from the outcome.
    missing: Vec<String>,
}

impl RetrievalEngine {
    pub fn new(cfg: EngineConfig) -> Self {
        Self::with_registry(cfg, SourceRegistry::builtin())
    }

    /// Build an engine over an explicit registry — the seam tests use to
    /// substitute adapters; [`RetrievalEngine::new`] passes the built-ins.
    pub(crate) fn with_registry(cfg: EngineConfig, registry: SourceRegistry) -> Self {
        let mut limiters = HashMap::new();
        let mut selected: Vec<Arc<dyn Source>> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for id in &cfg.sources {
            // Resolve the configured SourceId to its registry adapter. A
            // configured id with no adapter (cannot happen for the eight
            // built-ins) is remembered and reported by search(), never
            // silently dropped.
            match registry.get(id.as_str()) {
                Some(source) => {
                    limiters.insert(
                        source.id().to_string(),
                        Arc::new(RateLimiter::new(source.min_interval())),
                    );
                    selected.push(source);
                }
                None => missing.push(id.as_str().to_string()),
            }
        }
        // The client timeout is a backstop comfortably ABOVE the per-source
        // deadline enforced in search(); that way the deadline fires first
        // and the outcome is reported as "timeout", not as a transport error.
        let client = Client::builder()
            .timeout(Duration::from_secs(cfg.per_source_timeout_secs + 10))
            .build()
            .expect("reqwest client must build");
        Self {
            cfg,
            client,
            limiters,
            registry,
            selected,
            missing,
        }
    }

    /// Register an additional source at runtime and add it to the fan-out
    /// selection. This is the plugin seam: a source unknown to [`SourceId`]
    /// can be served without touching the enum or any match arm.
    ///
    /// Registering an id that is already selected REPLACES it — honouring
    /// [`SourceRegistry::register`]'s shadowing contract — so the id keeps
    /// one fan-out slot, one status entry, and adopts the new adapter's
    /// `min_interval`.
    pub fn register_source(&mut self, source: Arc<dyn Source>) {
        let id = source.id().to_string();
        self.limiters.insert(
            id.clone(),
            Arc::new(RateLimiter::new(source.min_interval())),
        );
        self.missing.retain(|m| *m != id);
        match self.selected.iter_mut().find(|s| s.id() == id) {
            Some(slot) => *slot = source.clone(),
            None => self.selected.push(source.clone()),
        }
        self.registry.register(source);
    }

    /// Read-only access to the registry (lookups, iteration).
    pub fn registry(&self) -> &SourceRegistry {
        &self.registry
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    pub(crate) fn fetch_ctx_for(&self, limit: usize) -> FetchCtx {
        let cache =
            self.cfg.cache_dir.clone().map(|dir| {
                DiskCache::new(dir).with_ttl(Duration::from_secs(self.cfg.cache_ttl_secs))
            });
        FetchCtx {
            client: self.client.clone(),
            headers: sources::default_headers(&self.cfg.user_agent)
                .expect("configured user agent must be a valid header"),
            mailto: self.cfg.mailto.clone(),
            limit,
            base_overrides: self
                .cfg
                .base_overrides
                .iter()
                .map(|(id, url)| (id.as_str().to_string(), url.clone()))
                .collect(),
            limiters: self.limiters.clone(),
            cache,
            max_attempts: self.cfg.max_attempts,
            cache_hits: std::sync::Mutex::new(std::collections::HashMap::new()),
            network_fetches: std::sync::atomic::AtomicUsize::new(0),
            cache_fetches: std::sync::atomic::AtomicUsize::new(0),
            fulltext_limiter: Arc::new(RateLimiter::new(Duration::from_millis(1000))),
        }
    }

    /// Fetch the best available full text for a paper (JATS preferred, PDF
    /// fallback). `Ok(None)` means the paper advertises no full text — it is
    /// reported as absent, never fabricated.
    pub async fn fetch_fulltext_for(
        &self,
        paper: &Paper,
    ) -> anyhow::Result<Option<crate::fulltext::Fulltext>> {
        let ctx = self.fetch_ctx_for(1);
        crate::fulltext::fetch_fulltext(&ctx, paper).await
    }

    /// Fan out one query to every configured source concurrently. Returns
    /// deduplicated papers plus an honest per-source status log.
    pub async fn search(&self, query: &str, per_source_limit: usize) -> SearchOutcome {
        let start = Instant::now();
        let ctx = self.fetch_ctx_for(per_source_limit);
        let timeout = Duration::from_secs(self.cfg.per_source_timeout_secs);

        let futures = self.selected.iter().cloned().map(|source| {
            let ctx = &ctx;
            async move {
                let source_start = Instant::now();
                let result = tokio::time::timeout(timeout, source.fetch(ctx, query)).await;
                (source, source_start.elapsed(), result)
            }
        });
        let outcomes = join_all(futures).await;

        let mut papers: Vec<Paper> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut duplicates_merged = 0usize;
        let mut source_status: Vec<SourceStatus> = Vec::new();

        for (source, elapsed, result) in outcomes {
            let latency_ms = elapsed.as_secs_f64() * 1000.0;
            let source_id = source.id();
            match result {
                Ok(Ok(found)) => {
                    let count = found.len();
                    for paper in found {
                        let key = paper.dedup_key();
                        match seen.get(&key) {
                            Some(idx) => {
                                papers[*idx].absorb(&paper);
                                duplicates_merged += 1;
                            }
                            None => {
                                seen.insert(key, papers.len());
                                papers.push(paper);
                            }
                        }
                    }
                    source_status.push(SourceStatus {
                        source: source_id.to_string(),
                        status: "ok".to_string(),
                        count,
                        latency_ms,
                        cache_hit: ctx
                            .cache_hits
                            .lock()
                            .expect("cache_hits poisoned")
                            .get(source_id)
                            .copied()
                            .unwrap_or(false),
                        error: None,
                    });
                }
                Ok(Err(e)) => source_status.push(SourceStatus {
                    source: source_id.to_string(),
                    status: "error".to_string(),
                    count: 0,
                    latency_ms,
                    cache_hit: false,
                    error: Some(format!("{e:#}")),
                }),
                Err(_) => source_status.push(SourceStatus {
                    source: source_id.to_string(),
                    status: "timeout".to_string(),
                    count: 0,
                    latency_ms,
                    cache_hit: false,
                    error: Some(format!(
                        "exceeded per-source timeout of {}s",
                        self.cfg.per_source_timeout_secs
                    )),
                }),
            }
        }

        // Configured sources that resolved to no adapter are reported with
        // the same vocabulary the sweep uses — never silently omitted.
        for id in &self.missing {
            source_status.push(SourceStatus {
                source: id.clone(),
                status: "error".to_string(),
                count: 0,
                latency_ms: 0.0,
                cache_hit: false,
                error: Some(format!("no adapter registered for source '{id}'")),
            });
        }

        SearchOutcome {
            papers,
            duplicates_merged,
            source_status,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::*;
    use crate::model::Paper;
    use crate::sources::{FetchCtx, Source};

    #[test]
    fn default_config_targets_all_sources() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.sources.len(), all_sources().len());
        assert!(cfg.per_source_timeout_secs > 0);
    }

    /// Builds an engine that fans out to NO built-in source (empty selection),
    /// so the only thing queried is whatever we register.
    fn isolated_engine() -> RetrievalEngine {
        RetrievalEngine::new(EngineConfig {
            sources: Vec::new(),
            cache_dir: None,
            ..EngineConfig::default()
        })
    }

    /// The seam under test: a source with no [`SourceId`] variant is registered
    /// through the registry and the engine queries it — no enum edit, no match
    /// arm touched.
    #[tokio::test]
    async fn registered_adapter_is_queried_without_enum_or_match() {
        let mut engine = isolated_engine();
        let registered: Arc<dyn Source> = Arc::new(EchoSource);
        engine.register_source(registered.clone());

        // The write went through the REGISTRY, not just the fan-out list:
        // the id resolves there, to exactly the adapter we registered.
        let via_registry = engine
            .registry()
            .get("echo")
            .expect("registered id must resolve through the registry");
        assert!(
            Arc::ptr_eq(&via_registry, &registered),
            "registry must hold the registered adapter itself"
        );

        let outcome = engine.search("anything", 10).await;

        // The test source was actually consulted.
        assert_eq!(outcome.source_status.len(), 1);
        let status = &outcome.source_status[0];
        assert_eq!(status.source, "echo");
        assert_eq!(status.status, "ok");
        assert_eq!(status.count, 1);
        // Its paper surfaced (not dropped, not fabricated into emptiness).
        assert_eq!(outcome.papers.len(), 1);
        assert_eq!(outcome.papers[0].source, "echo");
        assert_eq!(outcome.papers[0].source_id, "echo-1");
    }

    /// Registering an id twice must SHADOW, not duplicate: one fan-out slot,
    /// one status entry, and the replacement's behaviour and politeness
    /// interval win everywhere (selection, limiter, registry).
    #[tokio::test]
    async fn re_registering_an_id_shadows_selection_limiter_and_registry() {
        let mut engine = isolated_engine();
        engine.register_source(Arc::new(NamedSource {
            id: "dup",
            title: "first",
            interval: Duration::from_millis(700),
        }));
        engine.register_source(Arc::new(NamedSource {
            id: "dup",
            title: "second",
            interval: Duration::from_millis(250),
        }));

        let outcome = engine.search("anything", 10).await;
        // ONE status for the id — the old adapter no longer runs.
        assert_eq!(
            outcome.source_status.len(),
            1,
            "shadowed id must produce exactly one status entry"
        );
        assert_eq!(outcome.source_status[0].source, "dup");
        assert_eq!(outcome.source_status[0].count, 1);
        // The replacement's behaviour wins.
        assert_eq!(outcome.papers.len(), 1);
        assert_eq!(outcome.papers[0].title, "second");
        // The replacement's politeness interval wins.
        assert_eq!(
            engine.limiters["dup"].min_interval(),
            Duration::from_millis(250),
            "limiter must adopt the replacement adapter's min_interval"
        );
        // The registry resolves to the replacement too.
        assert_eq!(
            engine
                .registry()
                .get("dup")
                .expect("dup registered")
                .min_interval(),
            Duration::from_millis(250)
        );
    }

    /// A configured source with no adapter must surface as an error status —
    /// the same vocabulary the sweep uses — never silently vanish. And
    /// registering the adapter afterwards heals the report.
    #[tokio::test]
    async fn configured_source_with_no_adapter_reports_error_status() {
        let mut engine = RetrievalEngine::with_registry(
            EngineConfig {
                sources: vec![SourceId::Arxiv],
                cache_dir: None,
                ..EngineConfig::default()
            },
            SourceRegistry::new(), // empty: nothing resolves
        );

        let outcome = engine.search("anything", 10).await;
        assert_eq!(outcome.papers.len(), 0);
        assert_eq!(
            outcome.source_status.len(),
            1,
            "a configured source must never be silently omitted"
        );
        let status = &outcome.source_status[0];
        assert_eq!(status.source, "arxiv");
        assert_eq!(status.status, "error");
        assert_eq!(
            status.error.as_deref(),
            Some("no adapter registered for source 'arxiv'")
        );

        // Registering an adapter for the missing id clears the error.
        engine.register_source(Arc::new(NamedSource {
            id: "arxiv",
            title: "healed",
            interval: Duration::from_millis(1),
        }));
        let outcome = engine.search("anything", 10).await;
        assert_eq!(outcome.source_status.len(), 1);
        assert_eq!(outcome.source_status[0].source, "arxiv");
        assert_eq!(outcome.source_status[0].status, "ok");
    }

    /// A source that fails must be distinguishable from one that returned
    /// nothing: it reports `status: "error"` carrying its message, never an
    /// empty-but-ok result.
    #[tokio::test]
    async fn adapter_error_surfaces_as_status_error_not_empty() {
        let mut engine = isolated_engine();
        engine.register_source(Arc::new(ErrSource));
        let outcome = engine.search("anything", 10).await;

        assert_eq!(outcome.papers.len(), 0);
        assert_eq!(outcome.source_status.len(), 1);
        let status = &outcome.source_status[0];
        assert_eq!(status.source, "boom");
        assert_eq!(status.status, "error");
        assert_eq!(status.count, 0);
        assert!(!status.cache_hit);
        assert_eq!(status.error.as_deref(), Some("boom-adapter-failed"));
    }

    // ── Test-only adapters ───────────────────────────────────────────────
    // No network, no cache, no SourceId variant: pure trait objects proving
    // the registry is the dispatch path.

    struct EchoSource;
    #[async_trait]
    impl Source for EchoSource {
        fn id(&self) -> &'static str {
            "echo"
        }
        fn min_interval(&self) -> Duration {
            Duration::ZERO
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> anyhow::Result<Vec<Paper>> {
            Ok(vec![Paper {
                source: "echo".to_string(),
                source_id: "echo-1".to_string(),
                title: "Echo".to_string(),
                authors: Vec::new(),
                year: None,
                published: None,
                doi: None,
                external_ids: Default::default(),
                abstract_text: None,
                url: "urn:echo:1".to_string(),
                fulltext_url: None,
                fulltext_format: None,
                journal: None,
            }])
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _cursor: &str,
        ) -> anyhow::Result<(Vec<Paper>, Option<String>)> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }

    /// Configurable adapter: fixed id, one paper carrying `title`, and a
    /// declared politeness interval — enough to observe shadowing.
    struct NamedSource {
        id: &'static str,
        title: &'static str,
        interval: Duration,
    }
    #[async_trait]
    impl Source for NamedSource {
        fn id(&self) -> &'static str {
            self.id
        }
        fn min_interval(&self) -> Duration {
            self.interval
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> anyhow::Result<Vec<Paper>> {
            Ok(vec![Paper {
                source: self.id.to_string(),
                source_id: format!("{}-{}", self.id, self.title),
                title: self.title.to_string(),
                authors: Vec::new(),
                year: None,
                published: None,
                doi: None,
                external_ids: Default::default(),
                abstract_text: None,
                url: format!("urn:{}:{}", self.id, self.title),
                fulltext_url: None,
                fulltext_format: None,
                journal: None,
            }])
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _cursor: &str,
        ) -> anyhow::Result<(Vec<Paper>, Option<String>)> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }

    struct ErrSource;
    #[async_trait]
    impl Source for ErrSource {
        fn id(&self) -> &'static str {
            "boom"
        }
        fn min_interval(&self) -> Duration {
            Duration::ZERO
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> anyhow::Result<Vec<Paper>> {
            Err(anyhow::anyhow!("boom-adapter-failed"))
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _cursor: &str,
        ) -> anyhow::Result<(Vec<Paper>, Option<String>)> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }
}
