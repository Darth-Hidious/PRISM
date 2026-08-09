//! Resumable sweeps.
//!
//! A sweep pages through sources until exhausted or capped. Progress is
//! checkpointed to disk after EVERY page, so an interrupted sweep resumes
//! where it stopped instead of restarting. Completed pages are replayed from
//! the response cache on resume (no re-fetch, no re-paging drift): the
//! cursor chain is walked from the cached bodies until the first incomplete
//! page is reached.
//!
//! Nothing is ever invented: a sweep that finds nothing reports zero papers.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::engine::RetrievalEngine;
use crate::model::{Paper, SourceStatus};
use crate::ratelimit::RateLimiter;
use crate::sources::SourceId;

/// What a sweep will do. Changing the plan invalidates a saved state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SweepPlan {
    pub query: String,
    pub sources: Vec<SourceId>,
    /// Hard cap of pages per source (protects against cursor loops and
    /// source-side offset ceilings).
    pub max_pages_per_source: usize,
    /// Page size requested from each source.
    pub per_page_limit: usize,
}

impl Default for SweepPlan {
    fn default() -> Self {
        Self {
            query: String::new(),
            sources: Vec::new(),
            max_pages_per_source: 1,
            per_page_limit: 10,
        }
    }
}

/// Checkpointed sweep progress.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SweepState {
    pub plan: SweepPlan,
    /// "source|cursor" items fully completed.
    pub completed: BTreeSet<String>,
    pub papers_seen: usize,
}

impl SweepState {
    fn item_key(id: SourceId, cursor: &str) -> String {
        format!("{}|{}", id.as_str(), cursor)
    }

    pub fn is_done(&self, id: SourceId, cursor: &str) -> bool {
        self.completed.contains(&Self::item_key(id, cursor))
    }

    fn mark_done(&mut self, id: SourceId, cursor: &str) {
        self.completed.insert(Self::item_key(id, cursor));
    }
}

/// Outcome of one (possibly resumed) sweep run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepOutcome {
    pub papers: Vec<Paper>,
    pub duplicates_merged: usize,
    pub pages_fetched: usize,
    pub pages_from_cache: usize,
    pub finished: bool,
    pub source_status: Vec<SourceStatus>,
    pub elapsed_ms: f64,
}

fn load_state(path: &Path, plan: &SweepPlan) -> Result<Option<SweepState>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading sweep state {path:?}")),
    };
    let state: SweepState =
        serde_json::from_str(&raw).with_context(|| format!("parsing sweep state {path:?}"))?;
    if state.plan != *plan {
        bail!(
            "sweep state at {} belongs to a different plan; delete it or use the same plan",
            path.display()
        );
    }
    Ok(Some(state))
}

fn save_state(path: &Path, state: &SweepState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl RetrievalEngine {
    /// Run (or resume) a sweep, checkpointing to `state_path` after each
    /// page. Sources are walked one at a time WITHIN a source (cursor chains
    /// are inherently sequential) but this keeps politeness trivially
    /// correct; the concurrent path is `search()` across sources.
    pub async fn run_sweep(&self, plan: &SweepPlan, state_path: &Path) -> Result<SweepOutcome> {
        let start = Instant::now();
        let mut state = load_state(state_path, plan)?.unwrap_or_else(|| SweepState {
            plan: plan.clone(),
            ..Default::default()
        });

        let mut ctx = self.fetch_ctx_for(plan.per_page_limit);
        // The same per-source deadline search() enforces: a hung adapter
        // must stall one source for at most this long, never the sweep.
        let per_source_timeout = Duration::from_secs(self.config().per_source_timeout_secs);
        let mut papers: Vec<Paper> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        // Counted fresh each run: on a resume every completed page is
        // replayed, so its merges are re-observed. Seeding from the
        // checkpoint would count every duplicate twice (once per run).
        let mut duplicates_merged = 0usize;
        let mut pages_fetched = 0usize;
        let mut pages_from_cache = 0usize;
        let mut finished = true;
        let mut source_status: Vec<SourceStatus> = Vec::new();

        for id in &plan.sources {
            let source_start = Instant::now();
            // Resolve the configured source to its registry adapter. The eight
            // built-ins always resolve; an unresolvable id is reported
            // honestly and skipped, never fabricated.
            let Some(src) = self.registry().get(id.as_str()) else {
                source_status.push(SourceStatus {
                    source: id.as_str().to_string(),
                    status: "error".to_string(),
                    count: 0,
                    latency_ms: source_start.elapsed().as_secs_f64() * 1000.0,
                    cache_hit: false,
                    error: Some(format!(
                        "no adapter registered for source '{}'",
                        id.as_str()
                    )),
                });
                finished = false;
                continue;
            };
            // Politeness must not depend on the source appearing in
            // `cfg.sources`: the engine pre-populates limiters only for its
            // configured selection, while a plan may sweep any registered
            // id. Derive the limiter from the resolved adapter's declared
            // interval and share it for the whole run.
            ctx.limiters
                .entry(src.id().to_string())
                .or_insert_with(|| Arc::new(RateLimiter::new(src.min_interval())));
            let mut cursor = src.initial_cursor().to_string();
            let mut pages_this_source = 0usize;
            let mut count_this_source = 0usize;
            let mut error_this_source: Option<String> = None;
            let mut timed_out_this_source = false;
            let mut exhausted = false;
            // Where this source's contribution to the run begins. Sources are
            // walked strictly sequentially, so everything it pushes is a
            // contiguous tail — which is what makes a restart able to undo it.
            let papers_before_source = papers.len();
            let duplicates_before_source = duplicates_merged;

            loop {
                if pages_this_source >= plan.max_pages_per_source {
                    break;
                }
                if state.is_done(*id, &cursor) {
                    // Replay from cache to find the successor cursor. The
                    // replayed page's papers still belong in the outcome —
                    // a resume must not silently drop completed work — and
                    // the page still counts against the cap, so re-running
                    // a finished sweep is idempotent.
                    let network_before = ctx.network_fetches.load(Ordering::SeqCst);
                    match tokio::time::timeout(
                        per_source_timeout,
                        src.fetch_page(&ctx, &plan.query, &cursor),
                    )
                    .await
                    {
                        Ok(Ok((replayed, next))) => {
                            // pages_from_cache measures "no network happened",
                            // not "was marked done": a completed page whose
                            // cache entry is gone was refetched over the
                            // network and must count as fetched.
                            if ctx.network_fetches.load(Ordering::SeqCst) == network_before {
                                pages_from_cache += 1;
                            } else {
                                pages_fetched += 1;
                            }
                            pages_this_source += 1;
                            count_this_source += replayed.len();
                            for paper in replayed {
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
                            match next {
                                Some(n) => cursor = n,
                                None => {
                                    exhausted = true;
                                    break;
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            // Cache miss on a "completed" page means the chain
                            // cannot be replayed honestly; restart this source
                            // from its first page.
                            tracing::warn!(
                                "sweep replay failed for {} at cursor {cursor}: {e:#}; \
                                 restarting source from the beginning",
                                id.as_str()
                            );
                            cursor = src.initial_cursor().to_string();
                            state
                                .completed
                                .retain(|k| !k.starts_with(&format!("{}|", id.as_str())));
                            // Reset the page budget: the source restarts from
                            // page one, and the pages consumed before the
                            // restart must not starve the re-fetched chain —
                            // that is how resumes silently lost tail papers.
                            pages_this_source = 0;
                            count_this_source = 0;
                            // Roll back what this source already contributed.
                            // The restart rewinds its cursor, its completed
                            // markers and its budgets; leaving its papers in
                            // `seen` makes the re-walk re-observe every one of
                            // them and report each as a MERGED DUPLICATE — a
                            // merge the corpus never contained. `dedup_key`
                            // collisions are how two DISTINCT records become
                            // one work; re-reading the same record is not a
                            // merge, and saying so in the outcome JSON is a
                            // lie about the corpus.
                            for replaced in papers.drain(papers_before_source..) {
                                seen.remove(&replaced.dedup_key());
                            }
                            duplicates_merged = duplicates_before_source;
                        }
                        Err(_) => {
                            // A replay that re-fetches over the network can
                            // hang exactly like a fresh fetch; same deadline,
                            // same honest report.
                            error_this_source = Some(format!(
                                "exceeded per-source timeout of {}s",
                                self.config().per_source_timeout_secs
                            ));
                            timed_out_this_source = true;
                            finished = false;
                            break;
                        }
                    }
                    continue;
                }

                let network_before = ctx.network_fetches.load(Ordering::SeqCst);
                let page_result = tokio::time::timeout(
                    per_source_timeout,
                    src.fetch_page(&ctx, &plan.query, &cursor),
                )
                .await;
                match page_result {
                    Ok(Ok((found, next))) => {
                        // Same honesty as the replay path: count by real
                        // network traffic, not by bookkeeping.
                        if ctx.network_fetches.load(Ordering::SeqCst) == network_before {
                            pages_from_cache += 1;
                        } else {
                            pages_fetched += 1;
                        }
                        pages_this_source += 1;
                        count_this_source += found.len();
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
                        state.mark_done(*id, &cursor);
                        state.papers_seen = papers.len() + duplicates_merged;
                        save_state(state_path, &state)?;
                        match next {
                            Some(n) => cursor = n,
                            None => {
                                exhausted = true;
                                break;
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error_this_source = Some(format!("{e:#}"));
                        finished = false;
                        break;
                    }
                    Err(_) => {
                        error_this_source = Some(format!(
                            "exceeded per-source timeout of {}s",
                            self.config().per_source_timeout_secs
                        ));
                        timed_out_this_source = true;
                        finished = false;
                        break;
                    }
                }
            }

            // A source that stopped at its page cap without exhausting the
            // cursor chain left pages unfetched: the sweep is not finished,
            // and reporting finished would claim completeness it does not
            // have. A short sweep that says so is fine.
            if !exhausted && error_this_source.is_none() {
                finished = false;
            }

            let latency_ms = source_start.elapsed().as_secs_f64() * 1000.0;
            source_status.push(SourceStatus {
                source: src.id().to_string(),
                status: if timed_out_this_source {
                    "timeout".to_string()
                } else if error_this_source.is_some() {
                    "error".to_string()
                } else {
                    "ok".to_string()
                },
                count: count_this_source,
                latency_ms,
                cache_hit: false,
                error: error_this_source,
            });
        }

        Ok(SweepOutcome {
            papers,
            duplicates_merged,
            pages_fetched,
            pages_from_cache,
            finished,
            source_status,
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

/// Convenience: default state path for a plan under the engine cache dir.
pub fn default_state_path(state_dir: &Path, plan: &SweepPlan) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(plan.query.as_bytes());
    for source in &plan.sources {
        hasher.update(b"|");
        hasher.update(source.as_str().as_bytes());
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .take(16)
        .collect();
    state_dir.join(format!("sweep-{hex}.json"))
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::engine::EngineConfig;
    use crate::sources::{FetchCtx, Source, SourceRegistry};

    fn plan() -> SweepPlan {
        SweepPlan {
            query: "test".to_string(),
            sources: vec![SourceId::Arxiv],
            max_pages_per_source: 3,
            per_page_limit: 10,
        }
    }

    fn paper(source_id: &str) -> Paper {
        Paper {
            source: "arxiv".to_string(),
            source_id: source_id.to_string(),
            title: source_id.to_string(),
            authors: Vec::new(),
            year: None,
            published: None,
            doi: None,
            external_ids: Default::default(),
            abstract_text: None,
            url: format!("urn:test:{source_id}"),
            fulltext_url: None,
            fulltext_format: None,
            journal: None,
        }
    }

    /// Shadows "arxiv" and records the limiter the context hands out on
    /// every page — the observable politeness state.
    #[derive(Default)]
    struct CaptureSource {
        limiters_seen: std::sync::Mutex<Vec<Arc<RateLimiter>>>,
    }
    #[async_trait]
    impl Source for CaptureSource {
        fn id(&self) -> &'static str {
            "arxiv"
        }
        fn min_interval(&self) -> Duration {
            Duration::from_millis(250)
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
            self.fetch_page(ctx, query, "0").await.map(|(p, _)| p)
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            _query: &str,
            cursor: &str,
        ) -> Result<(Vec<Paper>, Option<String>)> {
            self.limiters_seen
                .lock()
                .expect("limiters_seen poisoned")
                .push(ctx.limiter("arxiv"));
            if cursor == "0" {
                Ok((vec![paper("page-one")], Some("10".to_string())))
            } else {
                Ok((vec![paper("page-two")], None))
            }
        }
    }

    /// Sleeps far past the deadline, then WOULD return a paper — so if the
    /// sweep's timeout wrapper is removed, the tests fail on their status
    /// assertions after the sleep instead of hanging forever.
    struct HangSource;
    #[async_trait]
    impl Source for HangSource {
        fn id(&self) -> &'static str {
            "arxiv"
        }
        fn min_interval(&self) -> Duration {
            Duration::ZERO
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, _ctx: &FetchCtx, _query: &str) -> Result<Vec<Paper>> {
            tokio::time::sleep(Duration::from_secs(4)).await;
            Ok(vec![paper("too-late")])
        }
        async fn fetch_page(
            &self,
            _ctx: &FetchCtx,
            _query: &str,
            _cursor: &str,
        ) -> Result<(Vec<Paper>, Option<String>)> {
            tokio::time::sleep(Duration::from_secs(4)).await;
            Ok((vec![paper("too-late")], None))
        }
    }

    fn hang_engine() -> RetrievalEngine {
        let mut reg = SourceRegistry::new();
        reg.register(Arc::new(HangSource));
        RetrievalEngine::with_registry(
            EngineConfig {
                sources: Vec::new(),
                cache_dir: None,
                per_source_timeout_secs: 1,
                ..EngineConfig::default()
            },
            reg,
        )
    }

    /// Defect-1 regression: the engine pre-populates limiters only from
    /// `cfg.sources`, so a sweep over a source ABSENT from the config used
    /// to fall back to a fresh zero-interval limiter per lookup — no rate
    /// limiting at all. The sweep must derive a shared limiter from the
    /// adapter it resolves.
    #[tokio::test]
    async fn sweep_source_absent_from_config_gets_the_adapters_real_interval() {
        let capture = Arc::new(CaptureSource::default());
        let mut reg = SourceRegistry::new();
        reg.register(capture.clone() as Arc<dyn Source>);
        let engine = RetrievalEngine::with_registry(
            EngineConfig {
                sources: Vec::new(), // NOT configured with arxiv
                cache_dir: None,
                ..EngineConfig::default()
            },
            reg,
        );
        let dir = tempfile::tempdir().unwrap();
        let outcome = engine
            .run_sweep(&plan(), &dir.path().join("state.json"))
            .await
            .unwrap();
        assert_eq!(outcome.papers.len(), 2);

        let seen = capture.limiters_seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "adapter must observe a limiter per page");
        assert_eq!(
            seen[0].min_interval(),
            Duration::from_millis(250),
            "sweep must derive the limiter from the adapter's declared \
             min_interval, not fall back to zero"
        );
        assert!(
            Arc::ptr_eq(&seen[0], &seen[1]),
            "the limiter must be shared across the run, not rebuilt per lookup"
        );
    }

    /// Defect-3 regression: sweep fetches now run under the same per-source
    /// deadline as search(), and report the same "timeout" vocabulary.
    #[tokio::test]
    async fn hung_adapter_times_out_instead_of_hanging_the_sweep() {
        let engine = hang_engine();
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let outcome = engine
            .run_sweep(&plan(), &dir.path().join("state.json"))
            .await
            .unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "the deadline must fire before the adapter returns"
        );
        assert!(!outcome.finished);
        assert_eq!(outcome.papers.len(), 0);
        assert_eq!(outcome.source_status.len(), 1);
        let status = &outcome.source_status[0];
        assert_eq!(status.status, "timeout");
        assert_eq!(
            status.error.as_deref(),
            Some("exceeded per-source timeout of 1s")
        );
    }

    /// The replay path re-fetches over the network when the cache entry is
    /// gone — it can hang exactly like a fresh fetch and gets the same
    /// deadline.
    #[tokio::test]
    async fn hung_replay_times_out_too() {
        let engine = hang_engine();
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        // Pre-seed a checkpoint marking page "0" done so the sweep takes
        // the REPLAY path.
        let mut state = SweepState {
            plan: plan(),
            ..Default::default()
        };
        state.mark_done(SourceId::Arxiv, "0");
        save_state(&state_path, &state).unwrap();

        let outcome = engine.run_sweep(&plan(), &state_path).await.unwrap();
        assert!(!outcome.finished);
        assert_eq!(outcome.source_status.len(), 1);
        assert_eq!(outcome.source_status[0].status, "timeout");
        assert_eq!(
            outcome.source_status[0].error.as_deref(),
            Some("exceeded per-source timeout of 1s")
        );
    }

    #[test]
    fn state_roundtrip_and_plan_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = SweepState {
            plan: plan(),
            ..Default::default()
        };
        state.mark_done(SourceId::Arxiv, "0");
        save_state(&path, &state).unwrap();
        let loaded = load_state(&path, &plan()).unwrap().unwrap();
        assert!(loaded.is_done(SourceId::Arxiv, "0"));
        assert!(!loaded.is_done(SourceId::Arxiv, "10"));

        let mut other = plan();
        other.query = "different".to_string();
        assert!(load_state(&path, &other).is_err());
    }

    #[test]
    fn default_state_path_is_stable_per_plan() {
        let dir = PathBuf::from("/tmp");
        let a = default_state_path(&dir, &plan());
        let b = default_state_path(&dir, &plan());
        assert_eq!(a, b);
        let mut other = plan();
        other.query = "x".to_string();
        assert_ne!(a, default_state_path(&dir, &other));
    }
}
