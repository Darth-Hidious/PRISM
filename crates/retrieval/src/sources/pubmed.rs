//! PubMed — NCBI E-utilities (esearch → esummary), JSON mode. Two requests
//! per search; the rate limiter keeps us inside the 3 rps no-key limit.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{Paper, SourcePage};

const DEFAULT_BASE: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";

pub const ID: &str = "pubmed";
pub const INITIAL_CURSOR: &str = "0";
/// Largest `retmax` this translator will put on the wire. Declared as the
/// adapter's capability and verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the esearch `retstart`. Continuation was already
/// gated on the RAW esearch id count (before esummary parsing can skip a
/// record); `available` is esearch's own `count`.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let retstart: usize = cursor.parse().unwrap_or(0);
    let retmax = ctx.limit.min(MAX_PAGE_SIZE);
    let base = ctx.base(ID, DEFAULT_BASE);

    // 1. Find matching PMIDs for this page.
    let search_url = format!(
        "{base}/esearch.fcgi?db=pubmed&retmode=json&retmax={retmax}&retstart={retstart}&term={q}",
        q = url_encode(query)
    );
    let (search_body, _) = ctx.fetch_cached(ID, &search_url).await?;
    let search_root: Value = serde_json::from_slice(&search_body)?;
    let ids: Vec<String> = search_root
        .pointer("/esearchresult/idlist")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // esearch reports its total as a JSON string (e.g. "2431").
    let available = search_root
        .pointer("/esearchresult/count")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());
    if ids.is_empty() {
        return Ok((
            SourcePage {
                papers: Vec::new(),
                raw_count: 0,
                available,
            },
            None,
        ));
    }

    // 2. Summarize them.
    let summary_url = format!(
        "{base}/esummary.fcgi?db=pubmed&retmode=json&id={}",
        url_encode(&ids.join(","))
    );
    let (summary_body, _) = ctx.fetch_cached(ID, &summary_url).await?;
    let papers = parse_summary(&summary_body, &ids)?;
    let next = (ids.len() >= retmax).then(|| (retstart + ids.len()).to_string());
    Ok((
        SourcePage {
            papers,
            raw_count: ids.len(),
            available,
        },
        next,
    ))
}

/// Pure parser over the esummary response. `ids` preserves esearch order so
/// relevance ranking survives.
pub fn parse_summary(body: &[u8], ids: &[String]) -> Result<Vec<Paper>> {
    let root: Value = serde_json::from_slice(body)?;
    let result = root.get("result").cloned().unwrap_or(Value::Null);
    let mut papers = Vec::new();
    for pmid in ids {
        let Some(record) = result.get(pmid) else {
            continue;
        };
        if let Some(paper) = parse_record(record, pmid) {
            papers.push(paper);
        }
    }
    Ok(papers)
}

fn parse_record(record: &Value, pmid: &str) -> Option<Paper> {
    let title = record
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())?
        .to_string();

    let authors = record
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // articleids carries doi and pmc among others.
    let mut doi = None;
    let mut external_ids = std::collections::BTreeMap::new();
    external_ids.insert("pmid".to_string(), pmid.to_string());
    if let Some(ids) = record.get("articleids").and_then(|v| v.as_array()) {
        for entry in ids {
            let id_type = entry.get("idtype").and_then(|v| v.as_str()).unwrap_or("");
            let value = entry.get("value").and_then(|v| v.as_str()).unwrap_or("");
            match id_type {
                "doi" => doi = normalize_doi(value),
                "pmc" => {
                    external_ids.insert("pmc".to_string(), value.to_string());
                }
                _ => {}
            }
        }
    }

    let published = record
        .get("pubdate")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let year = published
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    let journal = record
        .get("fulljournalname")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Some(Paper {
        source: ID.to_string(),
        source_id: pmid.to_string(),
        title,
        authors,
        year,
        published,
        doi,
        external_ids,
        abstract_text: None, // esummary does not serve abstracts; honesty over padding
        url: format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/"),
        fulltext_url: None,
        fulltext_format: None,
        journal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_esearch_ids() {
        let body = br#"{"esearchresult": {"count": "2", "idlist": ["111", "222"]}}"#;
        let root: Value = serde_json::from_slice(body).unwrap();
        let ids: Vec<String> = root
            .pointer("/esearchresult/idlist")
            .and_then(|v| v.as_array())
            .map(|l| {
                l.iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(ids, vec!["111", "222"]);
    }

    #[test]
    fn parses_esummary_records_in_order() {
        let body = br#"{
          "result": {
            "uids": ["222", "111"],
            "111": {
              "title": "First paper",
              "pubdate": "2021 Mar",
              "fulljournalname": "J Biol Chem",
              "authors": [{"name": "Doe J"}],
              "articleids": [
                {"idtype": "doi", "value": "10.9999/FIRST"},
                {"idtype": "pmc", "value": "PMC7654321"}
              ]
            },
            "222": {"title": "Second paper", "pubdate": "2020", "authors": []}
          }
        }"#;
        let ids = vec!["111".to_string(), "222".to_string()];
        let papers = parse_summary(body, &ids).unwrap();
        assert_eq!(papers.len(), 2);
        assert_eq!(papers[0].source_id, "111");
        assert_eq!(papers[0].doi.as_deref(), Some("10.9999/first"));
        assert_eq!(
            papers[0].external_ids.get("pmc").map(String::as_str),
            Some("PMC7654321")
        );
        assert_eq!(papers[0].year, Some(2021));
        assert_eq!(papers[1].source_id, "222");
        assert!(papers[1].abstract_text.is_none());
    }

    #[test]
    fn missing_record_is_skipped_not_fabricated() {
        let body = br#"{"result": {"uids": []}}"#;
        let papers = parse_summary(body, &["999".to_string()]).unwrap();
        assert!(papers.is_empty());
    }
}
