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
use crate::relevance::{RelevancePolicy, RelevanceReport, filter_papers};
use crate::selector::{Selector, SelectorPolicy, SelectorReport, select_papers};
use crate::sources::source::FailureKind;
use crate::sources::{self, FetchCtx, Source, SourceRegistry, all_sources};

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
    /// Sources to fan out to, in deterministic reporting order — REGISTRY id
    /// strings (`"arxiv"`, ...), so a runtime-registered adapter is selectable
    /// exactly like a built-in. The [`crate::sources::SourceId`] enum is only
    /// a CLI convenience for producing these strings; selection never
    /// consults it.
    pub sources: Vec<String>,
    pub per_source_timeout_secs: u64,
    pub cache_dir: Option<std::path::PathBuf>,
    pub cache_ttl_secs: u64,
    pub max_attempts: u32,
    /// Test/mirror overrides, keyed by registry id string.
    pub base_overrides: HashMap<String, String>,
    /// `None` preserves the legacy search result set and order. `Some` runs
    /// semantic relevance after deduplication.
    #[serde(default)]
    pub relevance: Option<RelevancePolicy>,
    /// `None` leaves the result set as the embedding stage returned it.
    /// `Some` runs the batched LLM selector after the embedding filter —
    /// precision after recall. The judge itself is supplied with
    /// [`RetrievalEngine::with_selector`]; a policy without a judge is
    /// reported honestly as unavailable and drops nothing.
    #[serde(default)]
    pub selector: Option<SelectorPolicy>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            mailto: None,
            sources: all_sources()
                .iter()
                .map(|id| id.as_str().to_string())
                .collect(),
            per_source_timeout_secs: 30,
            cache_dir: default_cache_dir(),
            cache_ttl_secs: 24 * 60 * 60,
            max_attempts: 3,
            base_overrides: HashMap::new(),
            relevance: None,
            selector: None,
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
    /// Lazy because the native backend may perform blocking model setup on
    /// first use. The cell also caches honest unavailability and init failure.
    relevance_backend:
        tokio::sync::OnceCell<Result<Option<Arc<dyn prism_embed::EmbedBackend>>, String>>,
    /// The optional precision judge for the selector stage. `None` (the
    /// default) models "no LLM configured": with a selector policy enabled
    /// the stage keeps every paper and says so in its report.
    selector: Option<Arc<dyn Selector>>,
}

impl RetrievalEngine {
    pub fn new(cfg: EngineConfig) -> Self {
        Self::with_registry(cfg, SourceRegistry::builtin())
    }

    /// Build an engine over an explicit registry — the third-party seam:
    /// register your own adapters, then select them by registry id string
    /// in `cfg.sources` exactly like a built-in. [`RetrievalEngine::new`]
    /// passes the built-ins.
    pub fn with_registry(cfg: EngineConfig, registry: SourceRegistry) -> Self {
        let mut limiters = HashMap::new();
        let mut selected: Vec<Arc<dyn Source>> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for id in &cfg.sources {
            // Resolve the configured id string to its registry adapter — the
            // registry is the only naming authority; no enum is consulted. A
            // configured id with no adapter (cannot happen for the eight
            // built-ins) is remembered and reported by search(), never
            // silently dropped.
            match registry.get(id) {
                Some(source) => {
                    limiters.insert(
                        source.id().to_string(),
                        Arc::new(RateLimiter::new(source.min_interval())),
                    );
                    selected.push(source);
                }
                None => missing.push(id.clone()),
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
            relevance_backend: tokio::sync::OnceCell::new(),
            selector: None,
        }
    }

    /// Enable semantic relevance for this engine with a caller-overridable
    /// corpus policy. Without this (or `EngineConfig.relevance`), search keeps
    /// its exact legacy result set and order.
    pub fn with_relevance_policy(mut self, policy: RelevancePolicy) -> Self {
        self.cfg.relevance = Some(policy);
        self
    }

    /// Supply an embedding backend directly, including explicit `None` to
    /// model an unavailable deployment. This is also the deterministic seam
    /// for tests and callers with their own backend lifecycle. Call it before
    /// the first relevance-enabled search.
    pub fn with_relevance_backend(
        self,
        backend: Option<Arc<dyn prism_embed::EmbedBackend>>,
    ) -> Self {
        assert!(
            self.relevance_backend.set(Ok(backend)).is_ok(),
            "relevance backend was already initialized"
        );
        self
    }

    /// Enable the LLM selector stage for this engine with a caller-overridable
    /// policy. The judge itself comes from [`RetrievalEngine::with_selector`];
    /// enabling the policy without one is reported honestly as unavailable
    /// and drops nothing.
    pub fn with_selector_policy(mut self, policy: SelectorPolicy) -> Self {
        self.cfg.selector = Some(policy);
        self
    }

    /// Supply the precision judge, including explicit `None` to model a
    /// deployment with no LLM configured. This is also the deterministic
    /// seam for tests and callers with their own LLM lifecycle.
    pub fn with_selector(mut self, selector: Option<Arc<dyn Selector>>) -> Self {
        self.selector = selector;
        self
    }

    /// Register an ADDITIONAL source at runtime and add it to the fan-out
    /// selection. This is the plugin seam: a source unknown to [`SourceId`]
    /// can be served without touching the enum or any match arm.
    ///
    /// The two-call contract shared by every adapter plane: a taken id is
    /// refused — an accidental collision must fail loudly, never silently
    /// shadow — and swapping an existing adapter is the deliberate call
    /// [`RetrievalEngine::replace_source`]. On refusal nothing changes:
    /// no limiter, no fan-out slot, no registry entry.
    pub fn register_source(&mut self, source: Arc<dyn Source>) -> anyhow::Result<()> {
        let id = source.id().to_string();
        // Refuse FIRST: a refused registration must not touch the limiter
        // table, the missing list, or the fan-out selection.
        self.registry.register(source.clone())?;
        self.limiters.insert(
            id.clone(),
            Arc::new(RateLimiter::new(source.min_interval())),
        );
        self.missing.retain(|m| *m != id);
        // The id was free in the registry, so it cannot already hold a
        // fan-out slot (selection only ever holds registry-resolved ids).
        self.selected.push(source);
        Ok(())
    }

    /// Deliberately swap the adapter behind an ALREADY-registered id. The
    /// replacement wins everywhere the id appears: registry lookup, the
    /// fan-out slot (kept in place — one slot, one status entry), and the
    /// politeness limiter, which adopts the replacement's `min_interval`.
    ///
    /// Strict on purpose: a free id is refused, because a typo'd id must
    /// not silently ADD a source while the adapter the caller meant to
    /// displace keeps running. If the id is registered but not part of
    /// this engine's fan-out selection, the selection stays unchanged —
    /// which sources run is the configuration's decision; `replace_source`
    /// only changes WHO serves an id.
    ///
    /// Returns the displaced adapter — hand it back to this function to
    /// restore the original.
    pub fn replace_source(&mut self, source: Arc<dyn Source>) -> anyhow::Result<Arc<dyn Source>> {
        let id = source.id().to_string();
        let displaced = self.registry.replace(source.clone())?;
        self.limiters.insert(
            id.clone(),
            Arc::new(RateLimiter::new(source.min_interval())),
        );
        if let Some(slot) = self.selected.iter_mut().find(|s| s.id() == id) {
            *slot = source;
        }
        Ok(displaced)
    }

    /// Read-only access to the registry (lookups, iteration).
    pub fn registry(&self) -> &SourceRegistry {
        &self.registry
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    async fn relevance_backend(
        &self,
    ) -> Result<Option<Arc<dyn prism_embed::EmbedBackend>>, String> {
        self.relevance_backend
            .get_or_init(|| async {
                let backend = tokio::task::spawn_blocking(prism_embed::from_config)
                    .await
                    .map_err(|error| {
                        format!("embedding backend initialization task failed: {error}")
                    })?;
                Ok(backend.map(Arc::from))
            })
            .await
            .clone()
    }

    /// Both relevance stages in order: cheap embedding RECALL, then the
    /// optional LLM selector for PRECISION. Each stage fails open on its own,
    /// so a failed embedding pass hands the selector every paper and a failed
    /// selector drops nothing. When the selector removed papers, the
    /// embedding report's `returned_unfiltered` is cleared so the flag stays
    /// a true statement about the returned set.
    async fn apply_relevance(&self, query: &str, papers: &mut Vec<Paper>) -> RelevanceReport {
        let mut report = self.apply_embedding_relevance(query, papers).await;
        report.selector = self.apply_selector(query, papers).await;
        if report
            .selector
            .as_ref()
            .is_some_and(|selector| selector.dropped > 0)
        {
            report.returned_unfiltered = false;
        }
        report
    }

    async fn apply_embedding_relevance(
        &self,
        query: &str,
        papers: &mut Vec<Paper>,
    ) -> RelevanceReport {
        let Some(policy) = &self.cfg.relevance else {
            return RelevanceReport::disabled(papers.len());
        };
        if let Err(reason) = policy.validate() {
            return RelevanceReport::failed(papers.len(), policy, None, reason);
        }

        let backend = match self.relevance_backend().await {
            Ok(Some(backend)) => backend,
            Ok(None) => return RelevanceReport::unavailable(papers.len(), policy),
            Err(reason) => return RelevanceReport::failed(papers.len(), policy, None, reason),
        };
        let backend_id = backend.id().to_string();
        match filter_papers(query, papers, policy, backend.as_ref()).await {
            Ok(report) => report,
            Err(reason) => RelevanceReport::failed(papers.len(), policy, Some(backend_id), reason),
        }
    }

    /// The precision stage. `None` means the stage was not configured; every
    /// failure path keeps every paper and reports why.
    async fn apply_selector(&self, query: &str, papers: &mut Vec<Paper>) -> Option<SelectorReport> {
        let policy = self.cfg.selector.as_ref()?;
        if let Err(reason) = policy.validate() {
            return Some(SelectorReport::failed(papers.len(), None, reason));
        }
        let Some(selector) = &self.selector else {
            return Some(SelectorReport::unavailable(papers.len()));
        };
        match select_papers(query, papers, policy, selector.as_ref()).await {
            Ok(report) => Some(report),
            Err(reason) => Some(SelectorReport::failed(
                papers.len(),
                Some(selector.id()),
                reason,
            )),
        }
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
            base_overrides: self.cfg.base_overrides.clone(),
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
    /// deduplicated papers plus honest source and relevance status logs.
    pub async fn search(&self, query: &str, per_source_limit: usize) -> SearchOutcome {
        let start = Instant::now();
        let ctx = self.fetch_ctx_for(per_source_limit);
        let timeout = Duration::from_secs(self.cfg.per_source_timeout_secs);

        let futures = self.selected.iter().map(|source| {
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
                Ok(Ok(page)) => {
                    let count = page.papers.len();
                    for paper in page.papers {
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
                        // The server's own total: `count < available` says
                        // this ok is a SLICE of what exists, not all of it.
                        available: page.available,
                        latency_ms,
                        cache_hit: ctx
                            .cache_hits
                            .lock()
                            .expect("cache_hits poisoned")
                            .get(source_id)
                            .copied()
                            .unwrap_or(false),
                        error: None,
                        failure_kind: None,
                    });
                }
                Ok(Err(e)) => source_status.push(SourceStatus {
                    source: source_id.to_string(),
                    status: "error".to_string(),
                    count: 0,
                    available: None,
                    latency_ms,
                    cache_hit: false,
                    error: Some(format!("{e:#}")),
                    failure_kind: Some(e.kind()),
                }),
                Err(_) => source_status.push(SourceStatus {
                    source: source_id.to_string(),
                    status: "timeout".to_string(),
                    count: 0,
                    available: None,
                    latency_ms,
                    cache_hit: false,
                    error: Some(format!(
                        "exceeded per-source timeout of {}s",
                        self.cfg.per_source_timeout_secs
                    )),
                    // Our deadline ended the fetch; the source neither
                    // answered nor failed on its own.
                    failure_kind: Some(FailureKind::Cancelled),
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
                available: None,
                latency_ms: 0.0,
                cache_hit: false,
                error: Some(format!("no adapter registered for source '{id}'")),
                // A configuration failure, not a source failure — the
                // taxonomy describes what SOURCES do.
                failure_kind: None,
            });
        }

        // Relevance runs after deduplication so absorbed abstracts contribute
        // to the scored text. On any unavailable or failed embedding path the
        // helper leaves `papers` untouched and reports the unfiltered return.
        let relevance = self.apply_relevance(query, &mut papers).await;

        SearchOutcome {
            papers,
            duplicates_merged,
            source_status,
            relevance,
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
    use crate::model::{Paper, SourcePage};
    use crate::sources::source::{SourceCaps, SourceError};
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
        engine
            .register_source(registered.clone())
            .expect("a free id must register");

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

    /// The two-call contract at the engine surface: registering a taken id
    /// is REFUSED (and the refusal changes nothing — the first adapter
    /// keeps running), while `replace_source` deliberately swaps it: one
    /// fan-out slot, one status entry, and the replacement's behaviour and
    /// politeness interval win everywhere (selection, limiter, registry).
    #[tokio::test]
    async fn replace_source_swaps_selection_limiter_and_registry() {
        let mut engine = isolated_engine();
        engine
            .register_source(Arc::new(NamedSource {
                id: "dup",
                title: "first",
                interval: Duration::from_millis(700),
            }))
            .expect("a free id must register");

        // Accidental collision: refused loudly, first adapter untouched.
        let err = engine
            .register_source(Arc::new(NamedSource {
                id: "dup",
                title: "accidental",
                interval: Duration::from_millis(1),
            }))
            .expect_err("a taken id must be refused");
        assert!(format!("{err:#}").contains("already registered"), "{err:#}");
        assert_eq!(
            engine.limiters["dup"].min_interval(),
            Duration::from_millis(700),
            "a refused registration must not touch the limiter"
        );

        // Deliberate replacement: the displaced adapter comes back.
        let displaced = engine
            .replace_source(Arc::new(NamedSource {
                id: "dup",
                title: "second",
                interval: Duration::from_millis(250),
            }))
            .expect("a registered id must be replaceable");
        assert_eq!(displaced.min_interval(), Duration::from_millis(700));

        let outcome = engine.search("anything", 10).await;
        // ONE status for the id — the old adapter no longer runs.
        assert_eq!(
            outcome.source_status.len(),
            1,
            "replaced id must produce exactly one status entry"
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

    /// THE named requirement, at PRODUCTION dispatch: replace a BUILT-IN
    /// source on an engine built by [`RetrievalEngine::new`] — the builtin
    /// registry, arxiv selected by ordinary configuration — and `search`
    /// must be served by the replacement (canned papers, no network). This
    /// test dies if `replace_source` stops swapping the registry, the
    /// fan-out slot, or if search stops dispatching through them.
    #[tokio::test]
    async fn replacing_builtin_arxiv_serves_search_from_the_replacement() {
        let mut engine = RetrievalEngine::new(EngineConfig {
            sources: vec!["arxiv".to_string()],
            cache_dir: None,
            ..EngineConfig::default()
        });

        let displaced = engine
            .replace_source(Arc::new(NamedSource {
                id: "arxiv",
                title: "my-own-arxiv",
                interval: Duration::from_millis(1),
            }))
            .expect("the built-in arxiv adapter must be replaceable");
        // We displaced the genuine built-in (its published politeness
        // interval identifies it), not some test residue.
        assert_eq!(displaced.id(), "arxiv");
        assert_eq!(displaced.min_interval(), Duration::from_millis(3000));

        let outcome = engine.search("anything", 5).await;
        assert_eq!(outcome.source_status.len(), 1);
        assert_eq!(outcome.source_status[0].source, "arxiv");
        assert_eq!(
            outcome.source_status[0].status, "ok",
            "the replacement must serve the id: {:?}",
            outcome.source_status[0].error
        );
        assert_eq!(outcome.papers.len(), 1);
        assert_eq!(
            outcome.papers[0].title, "my-own-arxiv",
            "search must be served by the REPLACEMENT adapter"
        );
    }

    /// A configured source with no adapter must surface as an error status —
    /// the same vocabulary the sweep uses — never silently vanish. And
    /// registering the adapter afterwards heals the report.
    #[tokio::test]
    async fn configured_source_with_no_adapter_reports_error_status() {
        let mut engine = RetrievalEngine::with_registry(
            EngineConfig {
                sources: vec!["arxiv".to_string()],
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
        assert_eq!(
            status.failure_kind, None,
            "a configuration failure is not a source failure"
        );

        // Registering an adapter for the missing id clears the error. (The
        // id is absent from this engine's EMPTY registry, so this is a
        // genuine registration, not a replacement.)
        engine
            .register_source(Arc::new(NamedSource {
                id: "arxiv",
                title: "healed",
                interval: Duration::from_millis(1),
            }))
            .expect("an id missing from the registry must register");
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
        engine
            .register_source(Arc::new(ErrSource))
            .expect("a free id must register");
        let outcome = engine.search("anything", 10).await;

        assert_eq!(outcome.papers.len(), 0);
        assert_eq!(outcome.source_status.len(), 1);
        let status = &outcome.source_status[0];
        assert_eq!(status.source, "boom");
        assert_eq!(status.status, "error");
        assert_eq!(status.count, 0);
        assert!(!status.cache_hit);
        assert_eq!(status.error.as_deref(), Some("boom-adapter-failed"));
        assert_eq!(
            status.failure_kind,
            Some(FailureKind::Transport),
            "the adapter's declared failure kind must reach the status"
        );
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
        fn capabilities(&self) -> SourceCaps {
            SourceCaps {
                max_page_size: 10,
                max_offset: None,
            }
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> Result<SourcePage, SourceError> {
            Ok(SourcePage {
                papers: vec![Paper {
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
                }],
                raw_count: 1,
                available: None,
            })
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _cursor: &str,
        ) -> Result<(SourcePage, Option<String>), SourceError> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }

    /// Configurable adapter: fixed id, one paper carrying `title`, and a
    /// declared politeness interval — enough to observe replacement.
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
        fn capabilities(&self) -> SourceCaps {
            SourceCaps {
                max_page_size: 10,
                max_offset: None,
            }
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> Result<SourcePage, SourceError> {
            Ok(SourcePage {
                papers: vec![Paper {
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
                }],
                raw_count: 1,
                available: None,
            })
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _cursor: &str,
        ) -> Result<(SourcePage, Option<String>), SourceError> {
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
        fn capabilities(&self) -> SourceCaps {
            SourceCaps {
                max_page_size: 10,
                max_offset: None,
            }
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> Result<SourcePage, SourceError> {
            Err(SourceError::msg(
                FailureKind::Transport,
                "boom-adapter-failed",
            ))
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _cursor: &str,
        ) -> Result<(SourcePage, Option<String>), SourceError> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }
}
