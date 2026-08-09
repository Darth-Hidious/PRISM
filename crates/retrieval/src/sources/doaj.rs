//! DOAJ — Directory of Open Access Journals search API, JSON. Every record
//! here has an open-access full text link by definition of the index.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, strip_markup, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "https://doaj.org/api/search/articles";

pub const ID: &str = "doaj";
pub const INITIAL_CURSOR: &str = "1";
/// Largest `pageSize` this translator will put on the wire. Declared as the
/// adapter's capability and verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the 1-based `page` number. The continuation gate uses
/// the RAW result count: a skipped record (missing `bibjson`) must not end
/// the chain.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let page_no: usize = cursor.parse().unwrap_or(1);
    let page_size = ctx.limit.min(MAX_PAGE_SIZE);
    let base = ctx.base(ID, DEFAULT_BASE);
    let url = format!(
        "{base}/{q}?pageSize={page_size}&page={page_no}",
        q = url_encode(query)
    );
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let page = parse(&body)?;
    let next = (page.raw_count >= page_size).then(|| (page_no + 1).to_string());
    Ok((page, next))
}

/// Pure parser over the DOAJ search response. `available` is the response's
/// `total`; `raw_count` is every result served, parsed or not.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let root: Value = serde_json::from_slice(body)?;
    let results = root
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut papers = Vec::new();
    for entry in &results {
        if let Some(paper) = parse_entry(entry) {
            papers.push(paper);
        }
    }
    Ok(SourcePage {
        papers,
        raw_count: results.len(),
        available: root.get("total").and_then(|v| v.as_u64()),
    })
}

fn parse_entry(entry: &Value) -> Option<Paper> {
    let bib = entry.get("bibjson")?;
    let title = strip_markup(bib.get("title").and_then(|v| v.as_str())?.trim());
    if title.is_empty() {
        return None;
    }
    let doaj_id = entry
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut doi = None;
    let mut external_ids = std::collections::BTreeMap::new();
    if !doaj_id.is_empty() {
        external_ids.insert("doaj".to_string(), doaj_id.clone());
    }
    if let Some(identifiers) = bib.get("identifier").and_then(|v| v.as_array()) {
        for ident in identifiers {
            let kind = ident.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let value = ident.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if kind.eq_ignore_ascii_case("doi") {
                doi = normalize_doi(value);
            }
        }
    }

    let authors = bib
        .get("author")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let year = bib
        .get("year")
        .and_then(|v| v.as_str())
        .and_then(|y| y.parse::<i32>().ok());
    let journal = bib
        .get("journal")
        .and_then(|j| j.get("title"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Full-text links: prefer one marked fulltext; note format when known.
    let mut fulltext_url = None;
    let mut fulltext_format = None;
    if let Some(links) = bib.get("link").and_then(|v| v.as_array()) {
        for link in links {
            let link_type = link.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let url = link.get("url").and_then(|v| v.as_str()).unwrap_or("");
            if url.is_empty() {
                continue;
            }
            if link_type == "fulltext" || fulltext_url.is_none() {
                fulltext_url = Some(url.to_string());
                let ctype = link
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                fulltext_format = if ctype.contains("pdf") || url.to_lowercase().ends_with(".pdf") {
                    Some(FulltextFormat::Pdf)
                } else {
                    None
                };
                if link_type == "fulltext" {
                    break;
                }
            }
        }
    }

    let url = fulltext_url
        .clone()
        .unwrap_or_else(|| format!("https://doaj.org/article/{doaj_id}"));

    Some(Paper {
        source: ID.to_string(),
        source_id: doaj_id,
        title,
        authors,
        year,
        published: None,
        doi,
        external_ids,
        abstract_text: bib
            .get("abstract")
            .and_then(|v| v.as_str())
            .map(strip_markup)
            .filter(|s| !s.is_empty()),
        url,
        fulltext_url,
        fulltext_format,
        journal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "total": 1,
      "results": [
        {
          "id": "abc123def",
          "bibjson": {
            "title": "Open access study of corrosion",
            "author": [{"name": "L. Garcia"}],
            "year": "2021",
            "abstract": "<p>Corrosion results.</p>",
            "identifier": [{"type": "doi", "id": "10.3390/ma140000"}],
            "journal": {"title": "Materials"},
            "link": [
              {"type": "fulltext", "url": "https://example.org/corrosion.pdf", "content_type": "application/pdf"}
            ]
          }
        }
      ]
    }"#;

    #[test]
    fn parses_doaj_records() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        let papers = &page.papers;
        assert_eq!(papers.len(), 1);
        assert_eq!(page.raw_count, 1);
        assert_eq!(page.available, Some(1), "the DOAJ total must be kept");
        let p = &papers[0];
        assert_eq!(p.doi.as_deref(), Some("10.3390/ma140000"));
        assert_eq!(p.year, Some(2021));
        assert_eq!(p.journal.as_deref(), Some("Materials"));
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
        assert_eq!(p.abstract_text.as_deref(), Some("Corrosion results."));
    }
}
