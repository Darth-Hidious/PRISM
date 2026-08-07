//! The ingestion surface.
//!
//! A literature source is a plugin implementing [`Source`]: a stable id, a
//! politeness interval, a paging start point, and two async fetchers. The
//! engine and sweep talk to sources **only** through [`SourceRegistry`]
//! dispatch — there is no `match` over an enum anywhere on the fetch path.
//! Adding a source means writing one module that exposes `ID`,
//! `INITIAL_CURSOR`, `fetch`, `fetch_page`, then one `register(...)` line in
//! [`SourceRegistry::builtin`].
//!
//! The built-in [`crate::sources::SourceId`] enum is retained only as a CLI
//! convenience (name parsing, default selection); it is not consulted by the
//! fetch path.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::Paper;

use super::FetchCtx;
use super::{arxiv, chemrxiv, crossref, doaj, europepmc, openalex, pubmed, semantic_scholar};

/// One federated literature source. The contract the engine and sweep depend
/// on; everything else is an implementation detail of the adapter.
///
/// `fetch`/`fetch_page` read the per-search row cap from `ctx.limit`, exactly
/// as the underlying free functions always have — the cap is not a parameter
/// here so the surface matches what adapters actually consume.
#[async_trait]
pub trait Source: Send + Sync {
    /// Stable machine id, e.g. `"arxiv"`. Registry key and the value reported
    /// in `source_status.source` / `Paper.source`.
    fn id(&self) -> &'static str;
    /// Minimum interval between two requests to this source — the politeness
    /// contract the rate limiter enforces.
    fn min_interval(&self) -> Duration;
    /// Where paging starts for this source.
    fn initial_cursor(&self) -> &'static str;
    /// Fetch the first page for `query`. An empty `Vec` means the source had
    /// nothing; use `Err` to report failure (they are reported differently).
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>>;
    /// Fetch one page identified by a source-specific cursor. Returns the
    /// papers and the successor cursor, when the source says there may be more.
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)>;
}

/// Ordered registry of adapters. Iteration order is registration order, which
/// is what makes per-source reporting deterministic.
#[derive(Clone)]
pub struct SourceRegistry {
    sources: Vec<Arc<dyn Source>>,
    by_id: std::collections::HashMap<String, usize>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            by_id: std::collections::HashMap::new(),
        }
    }

    /// The eight built-in literature sources, in canonical order.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(Arxiv));
        reg.register(Arc::new(Openalex));
        reg.register(Arc::new(Crossref));
        reg.register(Arc::new(Pubmed));
        reg.register(Arc::new(SemanticScholar));
        reg.register(Arc::new(Preprints));
        reg.register(Arc::new(Chemrxiv));
        reg.register(Arc::new(Doaj));
        reg
    }

    /// Add a source. Later registrations win on id collision (shadowing the
    /// earlier entry) so a caller can override a built-in without rebuilding
    /// the whole registry.
    pub fn register(&mut self, source: Arc<dyn Source>) {
        let id = source.id().to_string();
        if let Some(idx) = self.by_id.get(&id).copied() {
            self.sources[idx] = source;
        } else {
            self.by_id.insert(id, self.sources.len());
            self.sources.push(source);
        }
    }

    /// Look up a source by id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Source>> {
        self.by_id.get(id).map(|&idx| self.sources[idx].clone())
    }

    /// All registered sources, in registration order.
    pub fn all(&self) -> &[Arc<dyn Source>] {
        &self.sources
    }
}

impl Default for SourceRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

// ── Built-in adapters ──────────────────────────────────────────────────────
//
// Each adapter is a zero-state struct that delegates to its module's free
// functions. Behaviour is unchanged: only the dispatch moves behind the trait.

pub struct Arxiv;
pub struct Openalex;
pub struct Crossref;
pub struct Pubmed;
pub struct SemanticScholar;
/// bioRxiv/ChemRxiv preprints via Europe PMC's search index.
pub struct Preprints;
pub struct Chemrxiv;
pub struct Doaj;

#[async_trait]
impl Source for Arxiv {
    fn id(&self) -> &'static str {
        arxiv::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(3000)
    }
    fn initial_cursor(&self) -> &'static str {
        arxiv::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        arxiv::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        arxiv::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for Openalex {
    fn id(&self) -> &'static str {
        openalex::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(200)
    }
    fn initial_cursor(&self) -> &'static str {
        openalex::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        openalex::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        openalex::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for Crossref {
    fn id(&self) -> &'static str {
        crossref::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(200)
    }
    fn initial_cursor(&self) -> &'static str {
        crossref::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        crossref::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        crossref::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for Pubmed {
    fn id(&self) -> &'static str {
        pubmed::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(340)
    }
    fn initial_cursor(&self) -> &'static str {
        pubmed::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        pubmed::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        pubmed::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for SemanticScholar {
    fn id(&self) -> &'static str {
        semantic_scholar::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(1000)
    }
    fn initial_cursor(&self) -> &'static str {
        semantic_scholar::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        semantic_scholar::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        semantic_scholar::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for Preprints {
    fn id(&self) -> &'static str {
        europepmc::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn initial_cursor(&self) -> &'static str {
        europepmc::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        europepmc::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        europepmc::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for Chemrxiv {
    fn id(&self) -> &'static str {
        chemrxiv::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn initial_cursor(&self) -> &'static str {
        chemrxiv::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        chemrxiv::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        chemrxiv::fetch_page(ctx, query, cursor).await
    }
}

#[async_trait]
impl Source for Doaj {
    fn id(&self) -> &'static str {
        doaj::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn initial_cursor(&self) -> &'static str {
        doaj::INITIAL_CURSOR
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
        doaj::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(Vec<Paper>, Option<String>)> {
        doaj::fetch_page(ctx, query, cursor).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_has_eight_sources_in_order() {
        let reg = SourceRegistry::builtin();
        let ids: Vec<&str> = reg.all().iter().map(|s| s.id()).collect();
        assert_eq!(
            ids,
            [
                "arxiv",
                "openalex",
                "crossref",
                "pubmed",
                "semantic_scholar",
                "preprints_europepmc",
                "chemrxiv",
                "doaj"
            ]
        );
    }

    #[test]
    fn lookup_by_id_returns_the_right_adapter() {
        let reg = SourceRegistry::builtin();
        let s = reg.get("arxiv").expect("arxiv registered");
        assert_eq!(s.id(), "arxiv");
        assert_eq!(s.initial_cursor(), arxiv::INITIAL_CURSOR);
        assert_eq!(s.min_interval(), Duration::from_millis(3000));
    }

    #[test]
    fn registering_a_test_source_does_not_touch_the_enum() {
        // A source unknown to SourceId can be registered and looked up purely
        // through the registry — no enum variant, no match arm involved.
        let mut reg = SourceRegistry::builtin();
        reg.register(Arc::new(Demo));
        assert_eq!(reg.get("demo").map(|s| s.id()), Some("demo"));
        // The built-ins are untouched.
        assert_eq!(reg.all().len(), 9);
    }

    struct Demo;
    #[async_trait]
    impl Source for Demo {
        fn id(&self) -> &'static str {
            "demo"
        }
        fn min_interval(&self) -> Duration {
            Duration::from_millis(0)
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, _: &FetchCtx, _: &str) -> Result<Vec<Paper>> {
            Ok(Vec::new())
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _: &str,
        ) -> Result<(Vec<Paper>, Option<String>)> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }
}
