//! Crossref — `api.crossref.org/works`. JSON. Abstracts may carry JATS
//! markup; it is stripped to plain text (tags removed, text preserved).

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, SourceId, normalize_doi, strip_markup, url_encode};
use crate::model::{FulltextFormat, Paper};

const DEFAULT_BASE: &str = "https://api.crossref.org";

pub const INITIAL_CURSOR: &str = "0";

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
    let (papers, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(papers)
}

/// One page. Cursor is the Crossref `offset`. Crossref refuses offsets past
/// 10 000; callers (sweeps) cap page counts accordingly.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(Vec<Paper>, Option<String>)> {
    let offset: usize = cursor.parse().unwrap_or(0);
    let rows = ctx.limit.min(100);
    let base = ctx.base(SourceId::Crossref, DEFAULT_BASE);
    let mut url = format!(
        "{base}/works?query={q}&rows={rows}&offset={offset}",
        q = url_encode(query)
    );
    if let Some(mailto) = &ctx.mailto {
        url.push_str(&format!("&mailto={}", url_encode(mailto)));
    }
    let (body, _cached) = ctx.fetch_cached(SourceId::Crossref, &url).await?;
    let papers = parse(&body)?;
    let next =
        (papers.len() >= rows && offset + rows <= 10_000).then(|| (offset + rows).to_string());
    Ok((papers, next))
}

/// Pure parser over the Crossref `/works` response.
pub fn parse(body: &[u8]) -> Result<Vec<Paper>> {
    let root: Value = serde_json::from_slice(body)?;
    let items = root
        .pointer("/message/items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut papers = Vec::new();
    for item in &items {
        if let Some(paper) = parse_item(item) {
            papers.push(paper);
        }
    }
    Ok(papers)
}

fn parse_item(item: &Value) -> Option<Paper> {
    let doi = normalize_doi(item.get("DOI").and_then(|v| v.as_str())?)?;
    let title = item
        .get("title")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(strip_markup)
        .filter(|t| !t.is_empty())?;

    let authors = item
        .get("author")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let given = a.get("given").and_then(|v| v.as_str()).unwrap_or("");
                    let family = a.get("family").and_then(|v| v.as_str()).unwrap_or("");
                    let full = format!("{given} {family}").trim().to_string();
                    (!full.is_empty()).then_some(full)
                })
                .collect()
        })
        .unwrap_or_default();

    // Crossref dates live under "published" or "issued" as date-parts.
    let date_parts = item
        .pointer("/published/date-parts")
        .or_else(|| item.pointer("/issued/date-parts"));
    let year = date_parts
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_i64())
        .map(|y| y as i32);

    // A direct PDF link when advertised.
    let mut fulltext_url = None;
    if let Some(links) = item.get("link").and_then(|v| v.as_array()) {
        for link in links {
            let ctype = link
                .get("content-type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if ctype.contains("pdf") {
                fulltext_url = link.get("URL").and_then(|v| v.as_str()).map(str::to_string);
                break;
            }
        }
    }

    let abstract_text = item
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(strip_markup)
        .filter(|t| !t.is_empty());

    let journal = item
        .get("container-title")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let url = item
        .get("URL")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://doi.org/{doi}"));

    Some(Paper {
        source: SourceId::Crossref.as_str().to_string(),
        source_id: doi.clone(),
        title,
        authors,
        year,
        published: None,
        doi: Some(doi),
        external_ids: std::collections::BTreeMap::new(),
        abstract_text,
        url,
        fulltext_url: fulltext_url.clone(),
        fulltext_format: fulltext_url.map(|_| FulltextFormat::Pdf),
        journal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "status": "ok",
      "message": {
        "items": [
          {
            "DOI": "10.1016/j.actamat.2022.118000",
            "title": ["<jats:p>Deformation in refractory alloys</jats:p>"],
            "author": [
              {"given": "A.", "family": "Author"},
              {"family": "Solo"}
            ],
            "published": {"date-parts": [[2022, 6, 15]]},
            "abstract": "<jats:p>We report <jats:italic>in situ</jats:italic> data.</jats:p>",
            "container-title": ["Acta Materialia"],
            "URL": "https://doi.org/10.1016/j.actamat.2022.118000",
            "link": [
              {"URL": "https://example.org/full.xml", "content-type": "text/xml"},
              {"URL": "https://example.org/full.pdf", "content-type": "application/pdf"}
            ]
          },
          {"title": ["no doi record"]}
        ]
      }
    }"#;

    #[test]
    fn parses_items_strips_jats_and_picks_pdf_link() {
        let papers = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(papers.len(), 1);
        let p = &papers[0];
        assert_eq!(p.doi.as_deref(), Some("10.1016/j.actamat.2022.118000"));
        assert_eq!(p.title, "Deformation in refractory alloys");
        assert_eq!(p.abstract_text.as_deref(), Some("We report in situ data."));
        assert_eq!(p.authors, vec!["A. Author", "Solo"]);
        assert_eq!(p.year, Some(2022));
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://example.org/full.pdf")
        );
        assert_eq!(p.journal.as_deref(), Some("Acta Materialia"));
    }
}
