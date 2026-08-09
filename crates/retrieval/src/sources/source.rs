//! The ingestion surface.
//!
//! A literature source is a plugin implementing [`Source`]: a stable id, a
//! politeness interval, a paging start point, and two async fetchers. The
//! engine and sweep talk to sources **only** through [`SourceRegistry`]
//! dispatch — there is no `match` over an enum anywhere on the fetch path.
//! Adding a source means writing one module that exposes `ID`,
//! `INITIAL_CURSOR`, `fetch`, `fetch_page`, then one `register(...)` line in
//! [`SourceRegistry::builtin`]. Swapping a built-in for your own
//! implementation is [`SourceRegistry::replace`] — `register` refuses a
//! taken id so accidental collisions fail loudly, `replace` refuses a free
//! id so a typo cannot silently add instead of replacing.
//!
//! The built-in [`crate::sources::SourceId`] enum is retained only as a CLI
//! convenience (name parsing, default selection); it is not consulted by the
//! fetch path.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::SourcePage;

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
    /// Fetch the first page for `query`. An empty page means the source had
    /// nothing; use `Err` to report failure (they are reported differently).
    /// The page carries completeness accounting (`raw_count`, `available`) so
    /// a caller can tell "got everything" from "got less than exists".
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage>;
    /// Fetch one page identified by a source-specific cursor. Returns the
    /// page and the successor cursor, when the source says there may be more.
    /// The successor decision must derive from the RAW record count the
    /// server returned, never from how many records survived parsing.
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)>;
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
        let builtins: [Arc<dyn Source>; 8] = [
            Arc::new(Arxiv),
            Arc::new(Openalex),
            Arc::new(Crossref),
            Arc::new(Pubmed),
            Arc::new(SemanticScholar),
            Arc::new(Preprints),
            Arc::new(Chemrxiv),
            Arc::new(Doaj),
        ];
        for source in builtins {
            reg.register(source)
                .expect("built-in source ids are unique");
        }
        reg
    }

    /// Add a source under a FREE id.
    ///
    /// The two-call contract shared by every adapter plane: `register`
    /// refuses a taken id, because an accidental collision — two adapters
    /// both believing they own an id — must fail loudly instead of one
    /// silently winning. Taking over a built-in (or any registered id) is
    /// a deliberate act with its own call: [`SourceRegistry::replace`].
    pub fn register(&mut self, source: Arc<dyn Source>) -> Result<()> {
        let id = source.id().to_string();
        if self.by_id.contains_key(&id) {
            anyhow::bail!(
                "source '{id}' is already registered; use replace() to swap it deliberately"
            );
        }
        self.by_id.insert(id, self.sources.len());
        self.sources.push(source);
        Ok(())
    }

    /// Deliberately swap the adapter behind an ALREADY-registered id,
    /// keeping its slot in the iteration order. Strict on purpose: a free
    /// id is refused, because a typo'd id must not silently ADD a source
    /// while the adapter the caller meant to displace keeps running.
    ///
    /// Returns the displaced adapter — hand it back to this function to
    /// restore the original — and logs what was displaced.
    pub fn replace(&mut self, source: Arc<dyn Source>) -> Result<Arc<dyn Source>> {
        let id = source.id();
        let Some(&idx) = self.by_id.get(id) else {
            anyhow::bail!("no source '{id}' registered to replace; use register() to add it");
        };
        let displaced = std::mem::replace(&mut self.sources[idx], source);
        tracing::info!(source = id, "source adapter deliberately replaced");
        Ok(displaced)
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        arxiv::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        openalex::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        crossref::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        pubmed::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        semantic_scholar::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        europepmc::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        chemrxiv::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
        doaj::fetch(ctx, query).await
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>)> {
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

    /// Pins ALL EIGHT politeness intervals through the registry. These were
    /// duplicated from the old `SourceId::min_interval()` match into the
    /// eight adapters; nothing structural keeps them in agreement with each
    /// source's published guidance, and silent drift on a rate limit is how
    /// the tool gets banned.
    #[test]
    fn builtin_intervals_match_published_politeness_guidance() {
        let reg = SourceRegistry::builtin();
        let expected: [(&str, u64); 8] = [
            ("arxiv", 3000),
            ("openalex", 200),
            ("crossref", 200),
            ("pubmed", 340),
            ("semantic_scholar", 1000),
            ("preprints_europepmc", 500),
            ("chemrxiv", 500),
            ("doaj", 500),
        ];
        for (id, millis) in expected {
            let source = reg
                .get(id)
                .unwrap_or_else(|| panic!("source '{id}' must be registered"));
            assert_eq!(
                source.min_interval(),
                Duration::from_millis(millis),
                "politeness interval drifted for source '{id}'"
            );
        }
    }

    #[test]
    fn registering_a_test_source_does_not_touch_the_enum() {
        // A source unknown to SourceId can be registered and looked up purely
        // through the registry — no enum variant, no match arm involved.
        let mut reg = SourceRegistry::builtin();
        reg.register(Arc::new(Demo)).expect("free id must register");
        assert_eq!(reg.get("demo").map(|s| s.id()), Some("demo"));
        // The built-ins are untouched.
        assert_eq!(reg.all().len(), 9);
    }

    /// The two-call contract: `register` refuses a taken id (accidental
    /// collision is loud, nothing silently wins) and names the deliberate
    /// path; `replace` refuses a free id (a typo cannot silently ADD while
    /// the adapter the caller meant to displace keeps running).
    #[test]
    fn register_and_replace_are_strict_both_ways() {
        let mut reg = SourceRegistry::builtin();
        let before = reg.all().len();

        // Demo2 claims "arxiv" — a built-in id — so this is the accidental
        // collision case.
        let err = reg
            .register(Arc::new(Demo2))
            .expect_err("a taken id must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("already registered"), "{msg}");
        assert!(
            msg.contains("replace"),
            "the refusal must name the deliberate path: {msg}"
        );

        let err = match reg.replace(Arc::new(Demo)) {
            Err(e) => e,
            Ok(_) => panic!("replacing an id that is not registered must be refused"),
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("no source 'demo'"), "{msg}");

        assert_eq!(reg.all().len(), before, "refusals must change nothing");
    }

    /// Deliberate replacement: the displaced adapter comes back (the
    /// restore path), the slot keeps its iteration order, and lookups
    /// resolve to the replacement.
    #[test]
    fn replace_swaps_in_place_and_returns_the_displaced_adapter() {
        let mut reg = SourceRegistry::builtin();
        let order_before: Vec<&str> = reg.all().iter().map(|s| s.id()).collect();

        let displaced = reg
            .replace(Arc::new(Demo2))
            .expect("a registered id must be replaceable");
        assert_eq!(displaced.id(), "arxiv");
        assert_eq!(
            displaced.min_interval(),
            Duration::from_millis(3000),
            "we displaced the genuine built-in"
        );
        assert_eq!(
            reg.get("arxiv").map(|s| s.min_interval()),
            Some(Duration::ZERO),
            "lookup must resolve to the replacement"
        );
        let order_after: Vec<&str> = reg.all().iter().map(|s| s.id()).collect();
        assert_eq!(
            order_after, order_before,
            "replacement keeps the slot, order and count"
        );

        // Round-trip restore: hand the displaced adapter back.
        let mine = reg.replace(displaced).expect("restore must succeed");
        assert_eq!(mine.min_interval(), Duration::ZERO);
        assert_eq!(
            reg.get("arxiv").map(|s| s.min_interval()),
            Some(Duration::from_millis(3000))
        );
    }

    /// Claims the BUILT-IN id "arxiv" with a zero interval — the test
    /// double for both the accidental collision and the deliberate
    /// replacement of a built-in.
    struct Demo2;
    #[async_trait]
    impl Source for Demo2 {
        fn id(&self) -> &'static str {
            "arxiv"
        }
        fn min_interval(&self) -> Duration {
            Duration::ZERO
        }
        fn initial_cursor(&self) -> &'static str {
            "0"
        }
        async fn fetch(&self, _: &FetchCtx, _: &str) -> Result<SourcePage> {
            Ok(SourcePage::default())
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _: &str,
        ) -> Result<(SourcePage, Option<String>)> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
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
        async fn fetch(&self, _: &FetchCtx, _: &str) -> Result<SourcePage> {
            Ok(SourcePage::default())
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _: &str,
        ) -> Result<(SourcePage, Option<String>)> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }
}
