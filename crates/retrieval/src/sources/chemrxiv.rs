//! ChemRxiv — public engage API, JSON. Supports keyword search via `term`.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{Paper, SourcePage};

const DEFAULT_BASE: &str = "https://chemrxiv.org/engage/chemrxiv/public-api/v1";

pub const ID: &str = "chemrxiv";
pub const INITIAL_CURSOR: &str = "0";
/// Largest `limit` this translator will put on the wire. Declared as the
/// adapter's capability and verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the `skip` count. The continuation gate and the skip
/// advance both use the RAW hit count: a skipped record (blank title) must
/// neither end the chain nor re-read the tail.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let skip: usize = cursor.parse().unwrap_or(0);
    let limit = ctx.limit.min(MAX_PAGE_SIZE);
    let base = ctx.base(ID, DEFAULT_BASE);
    let url = format!(
        "{base}/items?term={q}&limit={limit}&skip={skip}",
        q = url_encode(query)
    );
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let page = parse(&body)?;
    let next = (page.raw_count >= limit).then(|| (skip + page.raw_count).to_string());
    Ok((page, next))
}

/// Pure parser over the ChemRxiv items response. `available` is the
/// response's `totalCount`; `raw_count` is every hit served, parsed or not.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let root: Value = serde_json::from_slice(body)?;
    let mut papers = Vec::new();
    let hits = root
        .get("itemHits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for hit in &hits {
        let Some(item) = hit.get("item") else {
            continue;
        };
        if let Some(paper) = parse_item(item) {
            papers.push(paper);
        }
    }
    Ok(SourcePage {
        papers,
        raw_count: hits.len(),
        available: root.get("totalCount").and_then(|v| v.as_u64()),
    })
}

fn parse_item(item: &Value) -> Option<Paper> {
    let title = item
        .get("title")
        .and_then(|v| v.as_str())?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let item_id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let doi = item
        .get("doi")
        .and_then(|v| v.as_str())
        .and_then(normalize_doi);

    let authors = item
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let first = a.get("firstName").and_then(|v| v.as_str()).unwrap_or("");
                    let last = a.get("lastName").and_then(|v| v.as_str()).unwrap_or("");
                    let full = format!("{first} {last}").trim().to_string();
                    (!full.is_empty()).then_some(full)
                })
                .collect()
        })
        .unwrap_or_default();

    let published = item
        .get("publishedDate")
        .or_else(|| item.get("statusDate"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let year = published
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let mut external_ids = std::collections::BTreeMap::new();
    if !item_id.is_empty() {
        external_ids.insert("chemrxiv".to_string(), item_id.clone());
    }

    Some(Paper {
        source: ID.to_string(),
        source_id: item_id.clone(),
        title,
        authors,
        year,
        published,
        doi,
        external_ids,
        abstract_text: item
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        url: format!("https://chemrxiv.org/engage/chemrxiv/article-details/{item_id}"),
        fulltext_url: None, // the public list endpoint does not serve PDF links
        fulltext_format: None,
        journal: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "totalCount": 41,
      "itemHits": [
        {
          "item": {
            "id": "6531abcd",
            "title": "Battery cathode screening",
            "doi": "10.26434/chemrxiv-6531abcd",
            "authors": [
              {"firstName": "K.", "lastName": "M\u00fcller"},
              {"firstName": "P.", "lastName": "Novak"}
            ],
            "publishedDate": "2023-10-19",
            "abstract": "High throughput screening."
          }
        },
        {"item": {"id": "x", "title": "   "}}
      ]
    }"#;

    #[test]
    fn parses_item_hits() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        let papers = &page.papers;
        assert_eq!(papers.len(), 1);
        // The blank-title hit is skipped from `papers` but still counted as
        // raw — pagination gates on raw, and the server's total is kept.
        assert_eq!(page.raw_count, 2);
        assert_eq!(page.available, Some(41));
        let p = &papers[0];
        assert_eq!(p.source_id, "6531abcd");
        assert_eq!(p.doi.as_deref(), Some("10.26434/chemrxiv-6531abcd"));
        assert_eq!(p.authors.len(), 2);
        assert_eq!(p.year, Some(2023));
        assert!(p.url.contains("6531abcd"));
    }
}
