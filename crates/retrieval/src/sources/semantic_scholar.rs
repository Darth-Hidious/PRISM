//! Semantic Scholar — Graph API v1 paper search, JSON. Unauthenticated
//! callers share one worldwide pool that 429s on effectively every request
//! (measured 2026-08-31: instant refusal, no Retry-After, pointing at the
//! API-key form), so an unkeyed fetch makes ONE attempt and names the
//! remedy. `SEMANTIC_SCHOLAR_API_KEY` moves requests onto a dedicated pool
//! (`x-api-key`, wired through `FetchCtx::extra_headers`) with the engine's
//! full retry budget.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "https://api.semanticscholar.org/graph/v1";
const FIELDS: &str = "title,authors,abstract,year,externalIds,url,openAccessPdf,venue";

pub const ID: &str = "semantic_scholar";
pub const INITIAL_CURSOR: &str = "0";
/// Largest `limit` this translator will put on the wire. Declared as the
/// adapter's capability and verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;
/// Deepest `offset` this translator will request a page at — the S2 API
/// caps offsets below 10 000. Declared and verified likewise.
pub const MAX_OFFSET: u64 = 9_999;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the S2 `offset`; the API caps it below 10 000. The
/// continuation gate uses the RAW item count: a skipped record (empty
/// title) must not end the chain.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let offset: usize = cursor.parse().unwrap_or(0);
    let limit = ctx.limit.min(MAX_PAGE_SIZE);
    let base = ctx.base(ID, DEFAULT_BASE);
    let url = format!(
        "{base}/paper/search?query={q}&limit={limit}&offset={offset}&fields={FIELDS}",
        q = url_encode(query)
    );
    // Unauthenticated, the shared pool's 429 is deterministic within the
    // retry horizon (measured: the 1 s + 2 s in-band retries burned ~3.7 s
    // per search, never once succeeding), so one attempt gets the honest
    // answer fast. A configured key restores the engine's full budget —
    // a keyed 429 is a genuine, transient rate limit.
    let attempts = if ctx.extra_headers.contains_key(ID) {
        ctx.max_attempts
    } else {
        1
    };
    let (body, _cached) = ctx
        .fetch_cached_with_attempts(ID, &url, attempts)
        .await
        .map_err(|err| {
            // The shared unauthenticated pool 429s on effectively every call
            // (measured 2026-08-31: instant refusal pointing at the key form).
            // Name the remedy in the status a caller actually sees, but only
            // when no key was sent — a keyed 429 is a genuine rate limit.
            let rate_limited = err.chain().any(|cause| {
                cause
                    .downcast_ref::<crate::http::HttpStatusFailure>()
                    .is_some_and(|http| http.status.as_u16() == 429)
            });
            if rate_limited && !ctx.extra_headers.contains_key(ID) {
                err.context(
                    "Semantic Scholar's shared unauthenticated pool is exhausted; set \
                 SEMANTIC_SCHOLAR_API_KEY for a dedicated pool \
                 (https://www.semanticscholar.org/product/api#api-key-form)",
                )
            } else {
                err
            }
        })?;
    let page = parse(&body)?;
    let next = (page.raw_count >= limit && (offset + limit) as u64 <= MAX_OFFSET)
        .then(|| (offset + limit).to_string());
    Ok((page, next))
}

/// Pure parser over the S2 search response. `available` is the response's
/// `total`; `raw_count` is every item served, parsed or not.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let root: Value = serde_json::from_slice(body)?;
    let items = root
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut papers = Vec::new();
    for item in &items {
        if let Some(paper) = parse_paper(item) {
            papers.push(paper);
        }
    }
    Ok(SourcePage {
        papers,
        raw_count: items.len(),
        available: root.get("total").and_then(|v| v.as_u64()),
    })
}

fn parse_paper(item: &Value) -> Option<Paper> {
    let title = item
        .get("title")
        .and_then(|v| v.as_str())?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let paper_id = item
        .get("paperId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut external_ids = std::collections::BTreeMap::new();
    let mut doi = None;
    if let Some(ids) = item.get("externalIds").and_then(|v| v.as_object()) {
        for (kind, value) in ids {
            let Some(s) = value.as_str() else { continue };
            match kind.to_lowercase().as_str() {
                "doi" => doi = normalize_doi(s),
                "arxiv" => {
                    external_ids.insert("arxiv".to_string(), s.to_string());
                }
                "pubmed" => {
                    external_ids.insert("pmid".to_string(), s.to_string());
                }
                "pubmedcentral" => {
                    external_ids.insert("pmc".to_string(), s.to_string());
                }
                other => {
                    external_ids.insert(other.to_string(), s.to_string());
                }
            }
        }
    }

    let authors = item
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let fulltext_url = item
        .pointer("/openAccessPdf/url")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let url = item
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://www.semanticscholar.org/paper/{paper_id}"));

    Some(Paper {
        source: ID.to_string(),
        source_id: paper_id,
        title,
        authors,
        year: item.get("year").and_then(|v| v.as_i64()).map(|y| y as i32),
        published: None,
        doi,
        external_ids,
        abstract_text: item
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        url,
        fulltext_url: fulltext_url.clone(),
        fulltext_format: fulltext_url.map(|_| FulltextFormat::Pdf),
        journal: item
            .get("venue")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "total": 2,
      "data": [
        {
          "paperId": "abc123",
          "title": "Machine learning for alloy design",
          "year": 2024,
          "authors": [{"name": "X. Li"}, {"name": "M. Chen"}],
          "abstract": "We survey methods.",
          "externalIds": {"DOI": "10.8888/ML.ALLOY", "ArXiv": "2401.00001", "PubMed": "39999999"},
          "url": "https://www.semanticscholar.org/paper/abc123",
          "openAccessPdf": {"url": "https://example.org/ml-alloy.pdf"},
          "venue": "npj Computational Materials"
        },
        {"paperId": "def456", "title": ""}
      ]
    }"#;

    #[test]
    fn parses_papers_and_normalizes_ids() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        let papers = &page.papers;
        assert_eq!(papers.len(), 1);
        // The empty-title record is skipped from `papers` but still raw; the
        // server's `total` survives.
        assert_eq!(page.raw_count, 2);
        assert_eq!(page.available, Some(2));
        let p = &papers[0];
        assert_eq!(p.doi.as_deref(), Some("10.8888/ml.alloy"));
        assert_eq!(
            p.external_ids.get("arxiv").map(String::as_str),
            Some("2401.00001")
        );
        assert_eq!(
            p.external_ids.get("pmid").map(String::as_str),
            Some("39999999")
        );
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
        assert_eq!(p.journal.as_deref(), Some("npj Computational Materials"));
        // DOI wins the dedup key over arXiv id.
        assert_eq!(p.dedup_key(), "doi:10.8888/ml.alloy");
    }
}
