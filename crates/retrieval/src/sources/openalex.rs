//! OpenAlex — `api.openalex.org/works`. JSON. Reconstructs abstracts from
//! the inverted index OpenAlex serves instead of raw text.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "https://api.openalex.org";

pub const ID: &str = "openalex";
pub const INITIAL_CURSOR: &str = "1";

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the 1-based `page` number; a full page of RAW works
/// may have a successor — a parser skip (blank display_name) must not end
/// the chain.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let page_no: usize = cursor.parse().unwrap_or(1);
    let per_page = ctx.limit.min(200);
    let base = ctx.base(ID, DEFAULT_BASE);
    let mut url = format!(
        "{base}/works?search={q}&per-page={per_page}&page={page_no}",
        q = url_encode(query)
    );
    if let Some(mailto) = &ctx.mailto {
        url.push_str(&format!("&mailto={}", url_encode(mailto)));
    }
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let page = parse(&body)?;
    let next = (page.raw_count >= per_page).then(|| (page_no + 1).to_string());
    Ok((page, next))
}

/// Pure parser over the OpenAlex `/works` response. `available` is
/// `meta.count`; `raw_count` is every work served, parsed or not.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let root: Value = serde_json::from_slice(body)?;
    let works = root
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut papers = Vec::new();
    for work in &works {
        if let Some(paper) = parse_work(work) {
            papers.push(paper);
        }
    }
    Ok(SourcePage {
        papers,
        raw_count: works.len(),
        available: root.pointer("/meta/count").and_then(|v| v.as_u64()),
    })
}

fn parse_work(work: &Value) -> Option<Paper> {
    let title = work.get("display_name")?.as_str()?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let openalex_id = work
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|u| u.rsplit('/').next())
        .unwrap_or_default()
        .to_string();

    let doi = work
        .get("doi")
        .and_then(|v| v.as_str())
        .and_then(normalize_doi);

    let authors = work
        .get("authorships")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| a.pointer("/author/display_name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let year = work
        .get("publication_year")
        .and_then(|v| v.as_i64())
        .map(|y| y as i32);
    let published = work
        .get("publication_date")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Best open-access location gives both a landing page and, when present,
    // a direct PDF.
    let oa = work.get("best_oa_location");
    let fulltext_url = oa
        .and_then(|l| l.get("pdf_url"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let landing = oa
        .and_then(|l| l.get("landing_page_url"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            work.get("primary_location")?
                .get("landing_page_url")?
                .as_str()
                .map(str::to_string)
        });

    let journal = work
        .pointer("/primary_location/source/display_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut external_ids = std::collections::BTreeMap::new();
    if !openalex_id.is_empty() {
        external_ids.insert("openalex".to_string(), openalex_id.clone());
    }
    if let Some(pmcid) = work.pointer("/ids/pmcid").and_then(|v| v.as_str()) {
        external_ids.insert("pmc".to_string(), pmcid.to_string());
    }
    if let Some(pmid) = work.pointer("/ids/pmid").and_then(|v| v.as_str()) {
        external_ids.insert("pmid".to_string(), pmid.to_string());
    }

    let url = landing.unwrap_or_else(|| format!("https://api.openalex.org/works/{openalex_id}"));

    Some(Paper {
        source: ID.to_string(),
        source_id: openalex_id,
        title,
        authors,
        year,
        published,
        doi,
        external_ids,
        abstract_text: abstract_from_inverted_index(work),
        url,
        fulltext_url: fulltext_url.clone(),
        fulltext_format: fulltext_url.map(|_| FulltextFormat::Pdf),
        journal,
    })
}

/// OpenAlex serves abstracts as {word: [positions...]}. Reassemble in
/// position order; missing words become nothing (no invented text).
fn abstract_from_inverted_index(work: &Value) -> Option<String> {
    let index = work.get("abstract_inverted_index")?;
    let obj = index.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let mut positions: Vec<(usize, &str)> = Vec::new();
    for (word, pos_list) in obj {
        for pos in pos_list.as_array().into_iter().flatten() {
            if let Some(p) = pos.as_u64() {
                positions.push((p as usize, word.as_str()));
            }
        }
    }
    positions.sort_by_key(|(p, _)| *p);
    let text = positions
        .into_iter()
        .map(|(_, w)| w)
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "meta": {"count": 512, "page": 1, "per_page": 25},
      "results": [
        {
          "id": "https://openalex.org/W4312345678",
          "display_name": "A study of thermal barrier coatings",
          "publication_year": 2023,
          "publication_date": "2023-05-01",
          "doi": "https://doi.org/10.5555/TBC.2023",
          "authorships": [
            {"author": {"display_name": "Jane Smith"}},
            {"author": {"display_name": "Ryo Tanaka"}}
          ],
          "ids": {"pmcid": "PMC1234567", "pmid": "37654321"},
          "abstract_inverted_index": {"We": [0], "measure": [1], "conductivity.": [2]},
          "best_oa_location": {
            "landing_page_url": "https://example.org/tbc",
            "pdf_url": "https://example.org/tbc.pdf"
          },
          "primary_location": {"source": {"display_name": "Acta Materialia"}}
        },
        {"display_name": "   "}
      ]
    }"#;

    #[test]
    fn parses_works_and_reconstructs_abstract() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        let papers = &page.papers;
        assert_eq!(papers.len(), 1);
        // The blank-name work is skipped from `papers` but still raw; the
        // server's meta.count survives.
        assert_eq!(page.raw_count, 2);
        assert_eq!(page.available, Some(512));
        let p = &papers[0];
        assert_eq!(p.source_id, "W4312345678");
        assert_eq!(p.doi.as_deref(), Some("10.5555/tbc.2023"));
        assert_eq!(p.abstract_text.as_deref(), Some("We measure conductivity."));
        assert_eq!(
            p.external_ids.get("pmc").map(String::as_str),
            Some("PMC1234567")
        );
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://example.org/tbc.pdf")
        );
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
        assert_eq!(p.journal.as_deref(), Some("Acta Materialia"));
    }
}
