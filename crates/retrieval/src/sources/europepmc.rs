//! Preprints (bioRxiv / ChemRxiv / others) via Europe PMC's search index.
//!
//! bioRxiv's own API exposes details-by-DOI and date-range listings but no
//! keyword search. Europe PMC indexes the preprint servers and offers a real
//! query endpoint, so that is the machine-readable path used here.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper};

const DEFAULT_BASE: &str = "https://www.ebi.ac.uk/europepmc/webservices/rest";

pub const ID: &str = "preprints_europepmc";
pub const INITIAL_CURSOR: &str = "";

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
    let (papers, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(papers)
}

/// One page. Cursor is Europe PMC's `cursorMark` (empty means start with
/// `*`); the server returns the successor mark, so the chain is replayable
/// from cache after an interruption.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(Vec<Paper>, Option<String>)> {
    let mark = if cursor.is_empty() { "*" } else { cursor };
    let base = ctx.base(ID, DEFAULT_BASE);
    let scoped = format!("({query}) AND SRC:PPR");
    let url = format!(
        "{base}/search?format=json&pageSize={n}&cursorMark={m}&query={q}",
        n = ctx.limit.min(25),
        m = url_encode(mark),
        q = url_encode(&scoped)
    );
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let root: Value = serde_json::from_slice(&body)?;
    let papers = parse_items(&root);
    let next_mark = root
        .get("nextCursorMark")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|m2| !papers.is_empty() && m2 != mark);
    Ok((papers, next_mark))
}

/// Pure parser over the Europe PMC search response.
pub fn parse(body: &[u8]) -> Result<Vec<Paper>> {
    let root: Value = serde_json::from_slice(body)?;
    Ok(parse_items(&root))
}

fn parse_items(root: &Value) -> Vec<Paper> {
    let mut papers = Vec::new();
    let results = root
        .pointer("/resultList/result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for item in &results {
        if let Some(paper) = parse_item(item) {
            papers.push(paper);
        }
    }
    papers
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
    let europepmc_id = item
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let doi = item
        .get("doi")
        .and_then(|v| v.as_str())
        .and_then(normalize_doi);

    let mut external_ids = std::collections::BTreeMap::new();
    if !europepmc_id.is_empty() {
        external_ids.insert("europepmc".to_string(), europepmc_id.clone());
    }
    // bioRxiv DOIs carry the 10.1101 prefix; record the server when we can.
    if let Some(source) = item.get("source").and_then(|v| v.as_str()) {
        external_ids.insert("epmc_source".to_string(), source.to_string());
    }

    let authors = item
        .get("authorString")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let published = item
        .get("firstPublicationDate")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let year = published
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let fulltext_url = item
        .pointer("/fullTextUrlList/fullTextUrl")
        .and_then(|v| v.as_array())
        .and_then(|list| {
            list.iter()
                .find(|ft| ft.get("documentStyle").and_then(|s| s.as_str()) == Some("pdf"))
                .or_else(|| list.first())
        })
        .and_then(|ft| ft.get("url").and_then(|u| u.as_str()))
        .map(str::to_string);

    // Prefer the HTML landing link when the record lists one; otherwise the
    // stable Europe PMC article page for this identifier.
    let html_url = item
        .pointer("/fullTextUrlList/fullTextUrl")
        .and_then(|v| v.as_array())
        .and_then(|list| {
            list.iter()
                .find(|ft| ft.get("documentStyle").and_then(|s| s.as_str()) == Some("html"))
        })
        .and_then(|ft| ft.get("url").and_then(|u| u.as_str()))
        .map(str::to_string);

    Some(Paper {
        source: ID.to_string(),
        source_id: europepmc_id.clone(),
        title,
        authors,
        year,
        published,
        doi,
        external_ids,
        abstract_text: item
            .get("abstractText")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        url: html_url
            .unwrap_or_else(|| format!("https://europepmc.org/article/PPR/{europepmc_id}")),
        fulltext_url: fulltext_url.clone(),
        fulltext_format: fulltext_url.map(|_| FulltextFormat::Pdf),
        journal: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "hitCount": 1,
      "resultList": {
        "result": [
          {
            "id": "PPR123456",
            "source": "PPR",
            "title": "A ChemRxiv preprint on solid electrolytes",
            "doi": "10.26434/chemrxiv-2024-abc",
            "authorString": "Smith J, Doe A",
            "firstPublicationDate": "2024-03-11",
            "abstractText": "Preprint abstract.",
            "fullTextUrlList": {
              "fullTextUrl": [
                {"documentStyle": "html", "url": "https://example.org/preprint"},
                {"documentStyle": "pdf", "url": "https://example.org/preprint.pdf"}
              ]
            }
          }
        ]
      }
    }"#;

    #[test]
    fn parses_preprint_records() {
        let papers = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(papers.len(), 1);
        let p = &papers[0];
        assert_eq!(p.source_id, "PPR123456");
        assert_eq!(p.doi.as_deref(), Some("10.26434/chemrxiv-2024-abc"));
        assert_eq!(p.authors, vec!["Smith J", "Doe A"]);
        assert_eq!(p.year, Some(2024));
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://example.org/preprint.pdf")
        );
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
    }
}
