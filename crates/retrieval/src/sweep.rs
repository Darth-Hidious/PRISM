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
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::engine::RetrievalEngine;
use crate::model::{Paper, SourceStatus};
use crate::sources::{self, SourceId};

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
    pub duplicates_merged: usize,
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

        let ctx = self.fetch_ctx_for(plan.per_page_limit);
        let mut papers: Vec<Paper> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut duplicates_merged = state.duplicates_merged;
        let mut pages_fetched = 0usize;
        let mut pages_from_cache = 0usize;
        let mut finished = true;
        let mut source_status: Vec<SourceStatus> = Vec::new();

        for id in &plan.sources {
            let source_start = Instant::now();
            let mut cursor = sources::initial_cursor(*id).to_string();
            let mut pages_this_source = 0usize;
            let mut count_this_source = 0usize;
            let mut error_this_source: Option<String> = None;

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
                    match sources::fetch_page(*id, &ctx, &plan.query, &cursor).await {
                        Ok((replayed, next)) => {
                            pages_from_cache += 1;
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
                                None => break,
                            }
                        }
                        Err(e) => {
                            // Cache miss on a "completed" page means the chain
                            // cannot be replayed honestly; restart this source
                            // from its first page.
                            tracing::warn!(
                                "sweep replay failed for {} at cursor {cursor}: {e:#}; \
                                 restarting source from the beginning",
                                id.as_str()
                            );
                            cursor = sources::initial_cursor(*id).to_string();
                            state
                                .completed
                                .retain(|k| !k.starts_with(&format!("{}|", id.as_str())));
                        }
                    }
                    continue;
                }

                let page_result = sources::fetch_page(*id, &ctx, &plan.query, &cursor).await;
                match page_result {
                    Ok((found, next)) => {
                        pages_fetched += 1;
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
                        state.duplicates_merged = duplicates_merged;
                        save_state(state_path, &state)?;
                        match next {
                            Some(n) => cursor = n,
                            None => break,
                        }
                    }
                    Err(e) => {
                        error_this_source = Some(format!("{e:#}"));
                        finished = false;
                        break;
                    }
                }
            }

            let latency_ms = source_start.elapsed().as_secs_f64() * 1000.0;
            source_status.push(SourceStatus {
                source: id.as_str().to_string(),
                status: if error_this_source.is_some() {
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
    use super::*;

    fn plan() -> SweepPlan {
        SweepPlan {
            query: "test".to_string(),
            sources: vec![SourceId::Arxiv],
            max_pages_per_source: 3,
            per_page_limit: 10,
        }
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
