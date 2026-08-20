//! OpenAlex — `api.openalex.org/works`. JSON. Reconstructs abstracts from
//! the inverted index OpenAlex serves instead of raw text.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "https://api.openalex.org";

pub const ID: &str = "openalex";
pub const INITIAL_CURSOR: &str = "1";
/// Largest `per-page` this translator will put on the wire. Declared as the
/// adapter's capability and verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 200;

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
    let per_page = ctx.limit.min(MAX_PAGE_SIZE);
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

/// `https://www.ncbi.nlm.nih.gov/pmc/articles/PMC5501191` -> `PMC5501191`.
///
/// Tolerates the bare form and a trailing slash, and leaves anything that does
/// not contain a `PMC…` segment untouched rather than inventing one.
fn normalize_pmcid(raw: &str) -> String {
    raw.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|seg| seg.starts_with("PMC"))
        .unwrap_or(raw.trim())
        .to_string()
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
    // OpenAlex advertises an open-access location in three places and this read
    // only one of them.
    //
    // Measured 2026-08-20 over a real run's 264 papers: 109 came back with NO
    // full-text URL at all — the single largest reason PRISM could not extract
    // facts, bigger than every publisher block combined. Many of those are open
    // access; their PDF simply is not on `best_oa_location.pdf_url`. It sits in
    // another `locations[]` entry (frequently the arXiv or PMC copy, which are
    // the hosts that actually answer an unattended fetcher), or only
    // `open_access.oa_url` is populated.
    //
    // Ordered by how likely the result is to be retrievable, not by how
    // canonical OpenAlex considers it.
    // (url, is_declared_pdf). The flag matters: `fetch_fulltext` TRUSTS a
    // declared format over sniffing the bytes, and `open_access.oa_url` is
    // frequently a landing page rather than a PDF. Declaring Pdf for one would
    // send an HTML page into the PDF parser and fail for a reason no log
    // explains. A `pdf_url` field is a publisher's own claim that it is a PDF;
    // for anything else, leave the format unset and let `sniff()` read the
    // bytes.
    let fulltext: Option<(String, bool)> = oa
        .and_then(|l| l.get("pdf_url"))
        .and_then(|v| v.as_str())
        .filter(|u| !u.trim().is_empty())
        .map(|u| (u.to_string(), true))
        .or_else(|| {
            work.get("locations")?
                .as_array()?
                .iter()
                .filter_map(|loc| loc.get("pdf_url")?.as_str())
                .find(|u| !u.trim().is_empty())
                .map(|u| (u.to_string(), true))
        })
        .or_else(|| {
            work.pointer("/open_access/oa_url")
                .and_then(|v| v.as_str())
                .filter(|u| !u.trim().is_empty())
                .map(|u| (u.to_string(), false))
        });
    let fulltext_url = fulltext.as_ref().map(|(u, _)| u.clone());
    let fulltext_format = fulltext
        .as_ref()
        .and_then(|(_, is_pdf)| is_pdf.then_some(FulltextFormat::Pdf));
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
        // OpenAlex returns this as a URL
        // (`https://www.ncbi.nlm.nih.gov/pmc/articles/PMC5501191`), not a bare
        // id. Every consumer wants the id: `papers_ingest pmc=<id>` fetches the
        // open-access JATS XML, and passing a URL there simply fails. Storing
        // the URL made the identifier look present while being unusable.
        external_ids.insert("pmc".to_string(), normalize_pmcid(pmcid));
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
        fulltext_url,
        fulltext_format,
        journal,
    })
}

#[cfg(test)]
mod oa_location_tests {
    use super::*;

    fn work(json: serde_json::Value) -> Paper {
        parse_work(&json).expect("fixture must parse")
    }

    fn base(extra: serde_json::Value) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": "https://openalex.org/W1",
            "display_name": "A paper",
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    /// 109 of 264 papers in a real run arrived with NO full-text URL — the
    /// single biggest reason PRISM could not extract facts, bigger than every
    /// publisher block combined. Many are open access with the PDF somewhere
    /// other than `best_oa_location.pdf_url`.
    #[test]
    fn an_oa_pdf_is_found_outside_best_oa_location() {
        let p = work(base(serde_json::json!({
            "best_oa_location": { "pdf_url": null },
            "locations": [
                { "pdf_url": null },
                { "pdf_url": "https://arxiv.org/pdf/2509.05344v1" }
            ]
        })));
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://arxiv.org/pdf/2509.05344v1"),
            "a PDF in locations[] must not be discarded"
        );
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
    }

    /// `oa_url` is often a LANDING PAGE. Declaring it a PDF sends HTML into the
    /// PDF parser, because `fetch_fulltext` trusts a declared format over
    /// sniffing the bytes.
    #[test]
    fn an_oa_url_is_used_but_never_declared_a_pdf() {
        let p = work(base(serde_json::json!({
            "open_access": { "oa_url": "https://example.org/article/123" }
        })));
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://example.org/article/123")
        );
        assert_eq!(
            p.fulltext_format, None,
            "format must stay unset so the bytes decide"
        );
    }

    /// OpenAlex returns pmcid as a URL. `papers_ingest pmc=<id>` needs the id;
    /// storing the URL made the identifier look present while being unusable.
    #[test]
    fn a_pmc_id_is_stored_as_an_id_not_a_url() {
        let p = work(base(serde_json::json!({
            "ids": { "pmcid": "https://www.ncbi.nlm.nih.gov/pmc/articles/PMC5501191" }
        })));
        assert_eq!(
            p.external_ids.get("pmc").map(String::as_str),
            Some("PMC5501191")
        );

        assert_eq!(normalize_pmcid("PMC42"), "PMC42", "already-bare id is kept");
        assert_eq!(
            normalize_pmcid("https://example.org/nothing/here"),
            "https://example.org/nothing/here",
            "a value with no PMC segment is left alone, never invented"
        );
    }

    #[test]
    fn a_paper_with_no_open_access_anywhere_still_reports_none() {
        let p = work(base(serde_json::json!({ "best_oa_location": null })));
        assert_eq!(p.fulltext_url, None);
        assert_eq!(p.fulltext_format, None);
    }
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
