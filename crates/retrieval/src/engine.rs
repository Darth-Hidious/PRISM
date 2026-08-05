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
use crate::sources::{self, FetchCtx, SourceId, all_sources};

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
    limiters: HashMap<SourceId, Arc<RateLimiter>>,
}

impl RetrievalEngine {
    pub fn new(cfg: EngineConfig) -> Self {
        let mut limiters = HashMap::new();
        for id in &cfg.sources {
            limiters.insert(*id, Arc::new(RateLimiter::new(id.min_interval())));
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
        }
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
    /// deduplicated papers plus an honest per-source status log.
    pub async fn search(&self, query: &str, per_source_limit: usize) -> SearchOutcome {
        let start = Instant::now();
        let ctx = self.fetch_ctx_for(per_source_limit);
        let timeout = Duration::from_secs(self.cfg.per_source_timeout_secs);

        let futures = self.cfg.sources.iter().map(|id| {
            let ctx = &ctx;
            async move {
                let source_start = Instant::now();
                let result =
                    tokio::time::timeout(timeout, sources::fetch_source(*id, ctx, query)).await;
                (id, source_start.elapsed(), result)
            }
        });
        let outcomes = join_all(futures).await;

        let mut papers: Vec<Paper> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut duplicates_merged = 0usize;
        let mut source_status: Vec<SourceStatus> = Vec::new();

        for (id, elapsed, result) in outcomes {
            let latency_ms = elapsed.as_secs_f64() * 1000.0;
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
                        source: id.as_str().to_string(),
                        status: "ok".to_string(),
                        count,
                        latency_ms,
                        cache_hit: ctx
                            .cache_hits
                            .lock()
                            .expect("cache_hits poisoned")
                            .get(id)
                            .copied()
                            .unwrap_or(false),
                        error: None,
                    });
                }
                Ok(Err(e)) => source_status.push(SourceStatus {
                    source: id.as_str().to_string(),
                    status: "error".to_string(),
                    count: 0,
                    latency_ms,
                    cache_hit: false,
                    error: Some(format!("{e:#}")),
                }),
                Err(_) => source_status.push(SourceStatus {
                    source: id.as_str().to_string(),
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
    use super::*;

    #[test]
    fn default_config_targets_all_sources() {
        let cfg = EngineConfig::default();
        assert_eq!(cfg.sources.len(), all_sources().len());
        assert!(cfg.per_source_timeout_secs > 0);
    }
}
