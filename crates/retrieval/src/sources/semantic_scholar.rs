//! Semantic Scholar — Graph API v1 paper search, JSON. Unauthenticated
//! callers share a strict pool (1 rps, frequent 429s); the limiter + retry
//! with Retry-After is what makes this source usable at all.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, SourceId, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper};

const DEFAULT_BASE: &str = "https://api.semanticscholar.org/graph/v1";
const FIELDS: &str = "title,authors,abstract,year,externalIds,url,openAccessPdf,venue";

pub const INITIAL_CURSOR: &str = "0";

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<Vec<Paper>> {
    let (papers, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(papers)
}

/// One page. Cursor is the S2 `offset`; the API caps it below 10 000.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(Vec<Paper>, Option<String>)> {
    let offset: usize = cursor.parse().unwrap_or(0);
    let limit = ctx.limit.min(100);
    let base = ctx.base(SourceId::SemanticScholar, DEFAULT_BASE);
    let url = format!(
        "{base}/paper/search?query={q}&limit={limit}&offset={offset}&fields={FIELDS}",
        q = url_encode(query)
    );
    let (body, _cached) = ctx.fetch_cached(SourceId::SemanticScholar, &url).await?;
    let papers = parse(&body)?;
    let next =
        (papers.len() >= limit && offset + limit <= 9_999).then(|| (offset + limit).to_string());
    Ok((papers, next))
}

/// Pure parser over the S2 search response.
pub fn parse(body: &[u8]) -> Result<Vec<Paper>> {
    let root: Value = serde_json::from_slice(body)?;
    let mut papers = Vec::new();
    for item in root
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let Some(paper) = parse_paper(&item) {
            papers.push(paper);
        }
    }
    Ok(papers)
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
        source: SourceId::SemanticScholar.as_str().to_string(),
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
        let papers = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(papers.len(), 1);
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
