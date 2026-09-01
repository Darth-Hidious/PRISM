//! Federated literature sources.
//!
//! Every source here is a machine-readable API — Atom, JSON, OAI-style or
//! E-utilities. None requires rendering a JavaScript page; headless browsers
//! are deliberately not part of this design.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use reqwest::header::HeaderMap;

use crate::cache::DiskCache;
use crate::http::{get_with_retry, identification_headers};
use crate::ratelimit::RateLimiter;

pub mod arxiv;
pub mod chemrxiv;
pub mod crossref;
pub mod doaj;
pub mod europepmc;
pub mod ntrs;
pub mod openalex;
pub mod pubmed;
pub mod semantic_scholar;

/// The plugin surface: a [`source::Source`] trait plus a [`source::SourceRegistry`].
/// The engine and sweep dispatch exclusively through the registry; there is
/// no `match` over [`SourceId`] on any fetch path.
pub mod source;

pub use source::{FailureKind, Source, SourceCaps, SourceError, SourceRegistry};

/// The sources this engine federates across.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SourceId {
    Arxiv,
    Openalex,
    Crossref,
    Pubmed,
    SemanticScholar,
    /// bioRxiv/ChemRxiv/other preprints via Europe PMC's search index —
    /// bioRxiv's own API has no keyword search endpoint, Europe PMC's does.
    Preprints,
    Chemrxiv,
    Doaj,
    /// NASA Technical Reports Server — US government technical reports,
    /// generally public domain.
    Ntrs,
}

impl SourceId {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceId::Arxiv => "arxiv",
            SourceId::Openalex => "openalex",
            SourceId::Crossref => "crossref",
            SourceId::Pubmed => "pubmed",
            SourceId::SemanticScholar => "semantic_scholar",
            SourceId::Preprints => "preprints_europepmc",
            SourceId::Chemrxiv => "chemrxiv",
            SourceId::Doaj => "doaj",
            SourceId::Ntrs => "ntrs",
        }
    }

    /// Parse a CLI/API source name back into a SourceId.
    pub fn from_name(name: &str) -> Option<SourceId> {
        all_sources()
            .into_iter()
            .find(|id| id.as_str() == name.trim())
    }

    /// Polite minimum interval between two requests to this source. Values
    /// follow each source's published guidance (arXiv: 3 s between API calls;
    /// Semantic Scholar unauthenticated: 1 rps; PubMed without key: 3 rps;
    /// the polite pools get a conservative 200 ms).
    pub fn min_interval(self) -> Duration {
        match self {
            SourceId::Arxiv => Duration::from_millis(3000),
            SourceId::SemanticScholar => Duration::from_millis(1000),
            SourceId::Pubmed => Duration::from_millis(340),
            SourceId::Openalex | SourceId::Crossref => Duration::from_millis(200),
            SourceId::Preprints => Duration::from_millis(500),
            SourceId::Chemrxiv => Duration::from_millis(500),
            SourceId::Doaj => Duration::from_millis(500),
            // NTRS publishes no rate guidance; match the conservative pools.
            SourceId::Ntrs => Duration::from_millis(500),
        }
    }
}

pub fn all_sources() -> Vec<SourceId> {
    vec![
        SourceId::Arxiv,
        SourceId::Openalex,
        SourceId::Crossref,
        SourceId::Pubmed,
        SourceId::SemanticScholar,
        SourceId::Preprints,
        SourceId::Chemrxiv,
        SourceId::Doaj,
        SourceId::Ntrs,
    ]
}

/// Everything one fetch needs. Base URLs are overridable per source (tests,
/// mirrors). Maps are keyed by the source's id string (e.g. `"arxiv"`) — the
/// same key the registry, the disk cache and `source_status` use — so a
/// source needs no [`SourceId`] variant to participate.
pub struct FetchCtx {
    pub client: reqwest::Client,
    pub headers: HeaderMap,
    pub mailto: Option<String>,
    pub limit: usize,
    pub base_overrides: HashMap<String, String>,
    pub limiters: HashMap<String, Arc<RateLimiter>>,
    pub cache: Option<DiskCache>,
    pub max_attempts: u32,
    /// Which sources were served entirely from cache this round, keyed by id.
    pub cache_hits: std::sync::Mutex<HashMap<String, bool>>,
    /// Requests actually issued to the network this round.
    pub network_fetches: std::sync::atomic::AtomicUsize,
    /// Responses served from the disk cache this round.
    pub cache_fetches: std::sync::atomic::AtomicUsize,
    /// Shared politeness limiter for full-text downloads across all hosts.
    pub fulltext_limiter: Arc<RateLimiter>,
    /// Per-source auth headers, keyed by source id — e.g. Semantic Scholar's
    /// `x-api-key`. Merged over the shared identification headers for that
    /// source's requests ONLY; other sources never see another source's
    /// credentials. Populated by [`auth_headers_from_env`].
    pub extra_headers: HashMap<String, HeaderMap>,
}

impl FetchCtx {
    /// Polite limiter for `id`. Both engine paths guarantee an entry exists
    /// before an adapter runs — `search` pre-populates one per selected
    /// source, `run_sweep` inserts one when it resolves the adapter, each
    /// derived from the adapter's `min_interval` — so the unlimited
    /// fallback is unreachable through the engine. It remains only for
    /// hand-built contexts (adapter unit tests).
    pub fn limiter(&self, id: &str) -> Arc<RateLimiter> {
        self.limiters
            .get(id)
            .cloned()
            .unwrap_or_else(|| Arc::new(RateLimiter::new(Duration::ZERO)))
    }

    pub fn base(&self, id: &str, default: &str) -> String {
        self.base_overrides
            .get(id)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// GET with cache-first semantics: a fresh cache hit is parsed without
    /// touching the network. Returns (body, cache_hit).
    pub async fn fetch_cached(&self, id: &str, url: &str) -> Result<(Vec<u8>, bool)> {
        self.fetch_cached_with_attempts(id, url, self.max_attempts)
            .await
    }

    /// [`FetchCtx::fetch_cached`] with an adapter-chosen retry budget.
    ///
    /// For a source whose failure is measured DETERMINISTIC within the
    /// backoff horizon — Semantic Scholar's unauthenticated shared pool
    /// 429s instantly and stays exhausted for minutes, so the 1 s + 2 s
    /// in-band retries burned ~3.7 s per search and never once succeeded —
    /// retrying only delays the honest answer. The adapter states its own
    /// budget; everything else keeps the engine's.
    pub async fn fetch_cached_with_attempts(
        &self,
        id: &str,
        url: &str,
        max_attempts: u32,
    ) -> Result<(Vec<u8>, bool)> {
        if let Some(cache) = &self.cache
            && let Some(body) = cache.get(id, url)
        {
            // A source counts as cache-hit only when ALL its requests this
            // round came from cache (PubMed makes two).
            let mut hits = self.cache_hits.lock().expect("cache_hits poisoned");
            hits.entry(id.to_string()).or_insert(true);
            self.cache_fetches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok((body, true));
        }
        {
            let mut hits = self.cache_hits.lock().expect("cache_hits poisoned");
            hits.insert(id.to_string(), false);
        }
        self.network_fetches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let limiter = self.limiter(id);
        let headers = match self.extra_headers.get(id) {
            Some(extra) => {
                let mut merged = self.headers.clone();
                for (name, value) in extra {
                    merged.insert(name.clone(), value.clone());
                }
                merged
            }
            None => self.headers.clone(),
        };
        let body = get_with_retry(&self.client, &limiter, url, &headers, max_attempts)
            .await?
            .to_vec();
        if let Some(cache) = &self.cache
            && let Err(e) = cache.put(id, url, &body)
        {
            tracing::warn!("cache write failed for {id}: {e}");
        }
        Ok((body, false))
    }
}

/// Build default identification headers for the engine.
pub fn default_headers(user_agent: &str) -> Result<HeaderMap> {
    identification_headers(user_agent)
}

/// Per-source auth headers from the environment, keyed by source id.
///
/// Semantic Scholar's unauthenticated pool is shared by every anonymous
/// caller worldwide and 429s on effectively every request (measured
/// 2026-08-31: instant `Too Many Requests` pointing at the API-key form).
/// `SEMANTIC_SCHOLAR_API_KEY` moves requests onto the operator's own pool
/// via the `x-api-key` header. A key that is not a valid header value is
/// ignored rather than poisoning every request.
pub fn auth_headers_from_env() -> HashMap<String, HeaderMap> {
    let mut map = HashMap::new();
    if let Ok(key) = std::env::var("SEMANTIC_SCHOLAR_API_KEY") {
        let key = key.trim();
        if !key.is_empty()
            && let Ok(mut value) = reqwest::header::HeaderValue::from_str(key)
        {
            value.set_sensitive(true);
            let mut headers = HeaderMap::new();
            headers.insert("x-api-key", value);
            map.insert(semantic_scholar::ID.to_string(), headers);
        }
    }
    map
}

/// Percent-encode for query components (RFC 3986 unreserved set kept).
pub fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Normalize a DOI to the trimmed lowercase form used for dedup.
pub fn normalize_doi(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip common resolver prefixes.
    let stripped = trimmed
        .strip_prefix("https://doi.org/")
        .or_else(|| trimmed.strip_prefix("http://doi.org/"))
        .or_else(|| trimmed.strip_prefix("doi:"))
        .unwrap_or(trimmed);
    let lower = stripped.trim().to_lowercase();
    (!lower.is_empty()).then_some(lower)
}

/// Strip XML/HTML markup keeping only text content (Crossref/DOAJ abstracts
/// arrive with JATS tags). Empty result stays empty — no text is invented.
pub fn strip_markup(input: &str) -> String {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(input);
    reader.config_mut().trim_text(false);
    reader.config_mut().allow_dangling_amp = true;
    let mut out = String::new();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(e)) => out.push_str(&e.decode().unwrap_or_default()),
            Ok(Event::GeneralRef(e)) => {
                out.push_str(&resolve_reference(&e.decode().unwrap_or_default()))
            }
            Ok(Event::CData(e)) => out.push_str(&String::from_utf8_lossy(e.as_ref())),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Resolve one entity/character reference name (quick-xml 0.41 emits
/// `GeneralRef` events for them inside text). Unknown named entities
/// resolve to nothing — no text is invented.
pub fn resolve_reference(name: &str) -> String {
    match name {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        _ => {
            if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_handles_spaces_and_symbols() {
        assert_eq!(url_encode("high entropy alloy"), "high%20entropy%20alloy");
        assert_eq!(url_encode("a+b=c"), "a%2Bb%3Dc");
    }

    #[test]
    fn doi_normalization_strips_resolvers() {
        assert_eq!(
            normalize_doi("https://doi.org/10.1234/ABC").as_deref(),
            Some("10.1234/abc")
        );
        assert_eq!(normalize_doi("  ").as_deref(), None);
        assert_eq!(
            normalize_doi("10.26434/chemrxiv-1").as_deref(),
            Some("10.26434/chemrxiv-1")
        );
    }

    #[test]
    fn strip_markup_keeps_text_drops_tags() {
        assert_eq!(
            strip_markup("<jats:p>We report <jats:italic>in situ</jats:italic> data.</jats:p>"),
            "We report in situ data."
        );
        assert_eq!(strip_markup("plain text"), "plain text");
        assert_eq!(strip_markup("<p></p>"), "");
        assert_eq!(strip_markup("a &amp; b &#65;"), "a & b A");
    }

    #[test]
    fn references_resolve_known_names_and_numeric() {
        assert_eq!(resolve_reference("amp"), "&");
        assert_eq!(resolve_reference("#65"), "A");
        assert_eq!(resolve_reference("#x41"), "A");
        assert_eq!(resolve_reference("nbsp"), "");
    }
}
