//! The ingestion surface.
//!
//! A literature source is a plugin implementing [`Source`]: a stable id, a
//! politeness interval, a paging start point, a serving-capability
//! declaration, and two async fetchers with a typed failure taxonomy. The
//! engine and sweep talk to sources **only** through [`SourceRegistry`]
//! dispatch — there is no `match` over an enum anywhere on the fetch path,
//! and selection (engine config, sweep plans, the CLI) names sources by
//! their registry id STRING, so an adapter registered at runtime can be
//! chosen without any enum edit.
//! Adding a source means writing one module that exposes `ID`,
//! `INITIAL_CURSOR`, `fetch`, `fetch_page`, then one `register(...)` line in
//! [`SourceRegistry::builtin`]. Swapping a built-in for your own
//! implementation is [`SourceRegistry::replace`] — `register` refuses a
//! taken id so accidental collisions fail loudly, `replace` refuses a free
//! id so a typo cannot silently add instead of replacing.
//!
//! The built-in [`crate::sources::SourceId`] enum is retained only as a CLI
//! convenience (name parsing, default selection); it is not consulted by the
//! fetch path or by selection.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::SourcePage;

use super::FetchCtx;
use super::{
    arxiv, chemrxiv, crossref, doaj, europepmc, ntrs, openalex, osti, pubmed, semantic_scholar,
};

/// What an adapter can actually SERVE, declared per adapter and verified
/// against its own translator by tests (`tests/capability_declarations.rs`)
/// — never merely asserted in prose or a manifest.
///
/// Deliberately narrow: literature sources have NO caller-selectable filter
/// surface beyond the free-text query. Every fetch takes exactly `query` plus
/// the paging inputs (`ctx.limit`, cursor); no field filters, date ranges or
/// sort orders exist anywhere in the pipeline (CLI: `--query/--limit`;
/// `SweepPlan`: query + paging). Declaring a filter vocabulary here would be
/// a capability model with nothing behind it — the exact "advertised field
/// the translator silently drops" lie this declaration exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceCaps {
    /// Largest per-page row count the translator will actually put on the
    /// wire. Requests above it are clamped; verified by matching the mock
    /// server's received page-size parameter against this declaration.
    pub max_page_size: usize,
    /// Deepest cursor position the translator will request a page at, when
    /// the source has an offset ceiling (Crossref, Semantic Scholar).
    /// `None` means the translator imposes no ceiling of its own — it says
    /// nothing about limits the server may still enforce.
    pub max_offset: Option<u64>,
}

/// Typed failure taxonomy for source fetches — Declaration 4 of the adapter
/// contract. Callers get a machine-readable kind (retry/backoff/reporting can
/// branch on it) while the human-readable error strings stay exactly what
/// they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The source rejected or requires credentials (HTTP 401/403/407).
    Auth,
    /// The source is throttling us (HTTP 429 surviving the retry budget).
    RateLimited,
    /// The source refused the request itself (HTTP 400/404/405/414/422) —
    /// retrying the same query cannot succeed.
    UnsupportedQuery,
    /// The source answered but the body could not be parsed.
    Malformed,
    /// The network failed: connect/DNS/TLS, a 5xx surviving retries, or the
    /// offline policy refusing the host.
    Transport,
    /// The caller's deadline ended the fetch (the per-source timeout).
    Cancelled,
}

/// A source fetch failure: a [`FailureKind`] plus the underlying error.
///
/// Display is DELEGATED to the wrapped error (honouring `{:#}` alternate
/// formatting), so status reporting strings are byte-identical to the old
/// bare `anyhow::Error` path — the taxonomy adds information, it does not
/// reword anything.
#[derive(Debug)]
pub struct SourceError {
    kind: FailureKind,
    source: anyhow::Error,
}

impl SourceError {
    /// Wrap an error under an explicitly chosen kind. Third-party adapters
    /// use this to state their own taxonomy mapping.
    pub fn new(kind: FailureKind, source: impl Into<anyhow::Error>) -> Self {
        Self {
            kind,
            source: source.into(),
        }
    }

    /// Build from a plain message under an explicitly chosen kind.
    pub fn msg(
        kind: FailureKind,
        message: impl std::fmt::Display + std::fmt::Debug + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: anyhow::Error::msg(message),
        }
    }

    pub fn kind(&self) -> FailureKind {
        self.kind
    }

    /// Classify an error escaping the built-in fetch pipeline. The mapping
    /// covers every escape that pipeline has: a typed
    /// [`crate::http::HttpStatusFailure`] (mapped by status), a
    /// `reqwest::Error` (transport), and body-parse failures
    /// (`serde_json`/`quick_xml` → malformed). Anything unrecognized is
    /// `Transport`: the only untyped escapes today are the offline-policy
    /// refusal and connect failures, both transport-layer.
    pub fn classify(err: anyhow::Error) -> Self {
        let mut kind = None;
        for cause in err.chain() {
            kind = if let Some(http) = cause.downcast_ref::<crate::http::HttpStatusFailure>() {
                Some(match http.status.as_u16() {
                    401 | 403 | 407 => FailureKind::Auth,
                    429 => FailureKind::RateLimited,
                    400 | 404 | 405 | 414 | 422 => FailureKind::UnsupportedQuery,
                    _ => FailureKind::Transport,
                })
            } else if cause.downcast_ref::<reqwest::Error>().is_some() {
                Some(FailureKind::Transport)
            } else if cause.downcast_ref::<serde_json::Error>().is_some()
                || cause.downcast_ref::<quick_xml::Error>().is_some()
            {
                Some(FailureKind::Malformed)
            } else {
                None
            };
            if kind.is_some() {
                break;
            }
        }
        Self {
            kind: kind.unwrap_or(FailureKind::Transport),
            source: err,
        }
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `{:#}` on a SourceError prints the wrapped chain exactly as `{:#}`
        // on the bare anyhow::Error did — pinned by the reporting tests.
        if f.alternate() {
            write!(f, "{:#}", self.source)
        } else {
            write!(f, "{}", self.source)
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // Display already carries the top message; the chain below it is
        // the cause.
        self.source.chain().nth(1)
    }
}

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
    /// What this adapter's translator can actually serve. Declarations are
    /// verified against the wire requests the translator emits — a claimed
    /// capability the translator does not honour is a test failure.
    fn capabilities(&self) -> SourceCaps;
    /// Fetch the first page for `query`. An empty page means the source had
    /// nothing; use `Err` to report failure (they are reported differently),
    /// stating the failure's [`FailureKind`].
    /// The page carries completeness accounting (`raw_count`, `available`) so
    /// a caller can tell "got everything" from "got less than exists".
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError>;
    /// Fetch one page identified by a source-specific cursor. Returns the
    /// page and the successor cursor, when the source says there may be more.
    /// The successor decision must derive from the RAW record count the
    /// server returned, never from how many records survived parsing.
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError>;
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

    /// The ten built-in literature sources, in canonical order.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        let builtins: [Arc<dyn Source>; 10] = [
            Arc::new(Arxiv),
            Arc::new(Openalex),
            Arc::new(Crossref),
            Arc::new(Pubmed),
            Arc::new(SemanticScholar),
            Arc::new(Preprints),
            Arc::new(Chemrxiv),
            Arc::new(Doaj),
            Arc::new(Ntrs),
            Arc::new(Osti),
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
/// NASA Technical Reports Server.
pub struct Ntrs;
/// OSTI.GOV adapter — see [`osti`].
pub struct Osti;

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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: arxiv::MAX_PAGE_SIZE,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        arxiv::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        arxiv::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: openalex::MAX_PAGE_SIZE,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        openalex::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        openalex::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: crossref::MAX_PAGE_SIZE,
            max_offset: Some(crossref::MAX_OFFSET),
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        crossref::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        crossref::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: pubmed::MAX_PAGE_SIZE,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        pubmed::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        pubmed::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: semantic_scholar::MAX_PAGE_SIZE,
            max_offset: Some(semantic_scholar::MAX_OFFSET),
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        semantic_scholar::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        semantic_scholar::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: europepmc::MAX_PAGE_SIZE,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        europepmc::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        europepmc::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: chemrxiv::MAX_PAGE_SIZE,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        chemrxiv::fetch(ctx, query)
            .await
            .map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        chemrxiv::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
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
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: doaj::MAX_PAGE_SIZE,
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        doaj::fetch(ctx, query).await.map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        doaj::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
    }
}

#[async_trait]
impl Source for Ntrs {
    fn id(&self) -> &'static str {
        ntrs::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn initial_cursor(&self) -> &'static str {
        ntrs::INITIAL_CURSOR
    }
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: ntrs::MAX_PAGE_SIZE,
            max_offset: Some(ntrs::MAX_OFFSET),
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        ntrs::fetch(ctx, query).await.map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        ntrs::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
    }
}

#[async_trait]
impl Source for Osti {
    fn id(&self) -> &'static str {
        osti::ID
    }
    fn min_interval(&self) -> Duration {
        Duration::from_millis(500)
    }
    fn initial_cursor(&self) -> &'static str {
        osti::INITIAL_CURSOR
    }
    fn capabilities(&self) -> SourceCaps {
        SourceCaps {
            max_page_size: osti::MAX_PAGE_SIZE,
            // No paging ceiling is published or was observed live.
            max_offset: None,
        }
    }
    async fn fetch(&self, ctx: &FetchCtx, query: &str) -> Result<SourcePage, SourceError> {
        osti::fetch(ctx, query).await.map_err(SourceError::classify)
    }
    async fn fetch_page(
        &self,
        ctx: &FetchCtx,
        query: &str,
        cursor: &str,
    ) -> Result<(SourcePage, Option<String>), SourceError> {
        osti::fetch_page(ctx, query, cursor)
            .await
            .map_err(SourceError::classify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_has_nine_sources_in_order() {
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
                "doaj",
                "ntrs",
                "osti"
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

    /// Pins ALL TEN politeness intervals through the registry. These were
    /// duplicated from the old `SourceId::min_interval()` match into the
    /// adapters; nothing structural keeps them in agreement with each
    /// source's published guidance, and silent drift on a rate limit is how
    /// the tool gets banned.
    #[test]
    fn builtin_intervals_match_published_politeness_guidance() {
        let reg = SourceRegistry::builtin();
        let expected: [(&str, u64); 10] = [
            ("arxiv", 3000),
            ("openalex", 200),
            ("crossref", 200),
            ("pubmed", 340),
            ("semantic_scholar", 1000),
            ("preprints_europepmc", 500),
            ("chemrxiv", 500),
            ("doaj", 500),
            ("ntrs", 500),
            ("osti", 500),
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

    /// Every enum variant is registered and every registered built-in has a
    /// variant: a source added to one list but not the other is reachable by
    /// name in one place and unknown in the other.
    #[test]
    fn the_enum_and_the_registry_name_the_same_sources() {
        let reg = SourceRegistry::builtin();
        let from_enum: Vec<&str> = crate::sources::all_sources()
            .into_iter()
            .map(|id| id.as_str())
            .collect();
        let from_registry: Vec<&str> = reg.all().iter().map(|s| s.id()).collect();
        assert_eq!(from_enum, from_registry);
        assert_eq!(
            crate::sources::SourceId::from_name("osti"),
            Some(crate::sources::SourceId::Osti)
        );
    }

    #[test]
    fn registering_a_test_source_does_not_touch_the_enum() {
        // A source unknown to SourceId can be registered and looked up purely
        // through the registry — no enum variant, no match arm involved.
        let mut reg = SourceRegistry::builtin();
        reg.register(Arc::new(Demo)).expect("free id must register");
        assert_eq!(reg.get("demo").map(|s| s.id()), Some("demo"));
        // The built-ins are untouched: ten of them, plus the demo.
        assert_eq!(reg.all().len(), 11);
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
        fn capabilities(&self) -> SourceCaps {
            SourceCaps {
                max_page_size: 10,
                max_offset: None,
            }
        }
        async fn fetch(&self, _: &FetchCtx, _: &str) -> Result<SourcePage, SourceError> {
            Ok(SourcePage::default())
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _: &str,
        ) -> Result<(SourcePage, Option<String>), SourceError> {
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
        fn capabilities(&self) -> SourceCaps {
            SourceCaps {
                max_page_size: 10,
                max_offset: None,
            }
        }
        async fn fetch(&self, _: &FetchCtx, _: &str) -> Result<SourcePage, SourceError> {
            Ok(SourcePage::default())
        }
        async fn fetch_page(
            &self,
            ctx: &FetchCtx,
            query: &str,
            _: &str,
        ) -> Result<(SourcePage, Option<String>), SourceError> {
            self.fetch(ctx, query).await.map(|p| (p, None))
        }
    }

    /// Pins the classification table at the unit level: every HTTP status
    /// class maps to ITS kind, parse failures map to `Malformed`, and the
    /// documented fallback is `Transport`. Collapsing any two kinds into one
    /// fails here (and again at engine level in `tests/failure_taxonomy.rs`).
    #[test]
    fn classification_keeps_the_kinds_distinct() {
        let http = |code: u16| {
            SourceError::classify(anyhow::Error::new(crate::http::HttpStatusFailure {
                status: reqwest::StatusCode::from_u16(code).unwrap(),
                url: "http://x.example/q".to_string(),
                attempts: 1,
            }))
            .kind()
        };
        assert_eq!(http(401), FailureKind::Auth);
        assert_eq!(http(403), FailureKind::Auth);
        assert_eq!(http(429), FailureKind::RateLimited);
        assert_eq!(http(400), FailureKind::UnsupportedQuery);
        assert_eq!(http(404), FailureKind::UnsupportedQuery);
        assert_eq!(http(500), FailureKind::Transport);
        assert_eq!(http(503), FailureKind::Transport);

        let parse = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        assert_eq!(
            SourceError::classify(anyhow::Error::new(parse)).kind(),
            FailureKind::Malformed
        );
        // The documented fallback: untyped escapes are transport-layer.
        assert_eq!(
            SourceError::classify(anyhow::anyhow!("offline mode refused host")).kind(),
            FailureKind::Transport
        );
    }

    /// The taxonomy adds information without rewording: `{:#}` on a
    /// classified error prints exactly what `{:#}` printed on the bare
    /// anyhow chain — status reporting strings are pinned elsewhere and
    /// must not shift underneath them.
    #[test]
    fn classified_errors_report_the_same_strings() {
        let bare = anyhow::Error::new(crate::http::HttpStatusFailure {
            status: reqwest::StatusCode::from_u16(429).unwrap(),
            url: "http://x.example/q".to_string(),
            attempts: 3,
        })
        .context("fetching page 2");
        let bare_text = format!("{bare:#}");
        let classified = SourceError::classify(bare);
        assert_eq!(format!("{classified:#}"), bare_text);
        assert_eq!(
            bare_text,
            "fetching page 2: HTTP 429 Too Many Requests from http://x.example/q after 3 attempt(s)"
        );
    }
}
