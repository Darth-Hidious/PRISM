//! NTRS — the NASA Technical Reports Server, `ntrs.nasa.gov/api/citations`.
//! JSON. A US government source: records are generally public domain, and
//! each record carries its own `distribution`/copyright block stating so.
//!
//! Wire contract, verified live 2026-08-09:
//! * pagination is `page[size]` / `page[from]` — the bare `size`/`from`
//!   query parameters are silently IGNORED by the GET endpoint (a request
//!   carrying only those gets the default 10 rows back);
//! * the server total is `stats.total`;
//! * the Elasticsearch window rejects `from + size > 10_000` with HTTP 400
//!   (`search_phase_execution_exception`).

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "https://ntrs.nasa.gov/api";
/// Public host that download paths and landing pages hang off. Records give
/// `downloads[].links.pdf` as an absolute PATH (`/api/citations/...`), not a
/// URL.
const PUBLIC_HOST: &str = "https://ntrs.nasa.gov";

pub const ID: &str = "ntrs";
pub const INITIAL_CURSOR: &str = "0";
/// Largest `page[size]` this translator will put on the wire. The server
/// accepts far more (1000+ observed live), but 100 matches the politeness
/// posture of the other adapters. Declared as the adapter's capability and
/// verified against the actual request in `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;
/// Deepest `page[from]` this translator will request a page at. The live
/// window rejects `from + size > 10_000`, so with pages up to
/// [`MAX_PAGE_SIZE`] the deepest start that can never trip the window is
/// `10_000 - 100`. Declared and verified likewise.
pub const MAX_OFFSET: u64 = 9_900;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the `page[from]` offset. The continuation gate uses
/// the RAW result count the server returned — a skipped record (no id or no
/// title) must not end the chain — and never advances past [`MAX_OFFSET`].
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let offset: usize = cursor.parse().unwrap_or(0);
    let rows = ctx.limit.min(MAX_PAGE_SIZE);
    let base = ctx.base(ID, DEFAULT_BASE);
    let url = format!(
        "{base}/citations/search?q={q}&page%5Bsize%5D={rows}&page%5Bfrom%5D={offset}",
        q = url_encode(query)
    );
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let page = parse(&body)?;
    let next = (page.raw_count >= rows && (offset + rows) as u64 <= MAX_OFFSET)
        .then(|| (offset + rows).to_string());
    Ok((page, next))
}

/// Pure parser over the citations/search response. `available` is
/// `stats.total`; `raw_count` is every result served, parsed or not.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let root: Value = serde_json::from_slice(body)?;
    let results = root
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut papers = Vec::new();
    for item in &results {
        if let Some(paper) = parse_item(item) {
            papers.push(paper);
        }
    }
    Ok(SourcePage {
        papers,
        raw_count: results.len(),
        available: root.pointer("/stats/total").and_then(|v| v.as_u64()),
    })
}

fn parse_item(item: &Value) -> Option<Paper> {
    // The record id is a plain number live (`"id": 20150002086`), but take a
    // string form too rather than depending on that representational detail.
    let id = match item.get("id")? {
        Value::Number(n) => n.to_string(),
        Value::String(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return None,
    };
    let title = item
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())?
        .to_string();

    // Authors ride in `authorAffiliations[].meta.author.name`, ordered by
    // `sequence`.
    let mut affiliated: Vec<(u64, String)> = item
        .get("authorAffiliations")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let name = a
                        .pointer("/meta/author/name")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|n| !n.is_empty())?;
                    let seq = a.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
                    Some((seq, name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    affiliated.sort_by_key(|(seq, _)| *seq);
    let authors: Vec<String> = affiliated.into_iter().map(|(_, name)| name).collect();

    // `publications[]` is present on published works and can carry the DOI,
    // the venue and the publication date; a bare report has none of it.
    let publications = item
        .get("publications")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let doi = publications
        .iter()
        .find_map(|p| p.get("doi").and_then(|v| v.as_str()))
        .and_then(normalize_doi);
    let journal = publications
        .iter()
        .find_map(|p| p.get("publicationName").and_then(|v| v.as_str()))
        .map(str::to_string);
    let published = publications
        .iter()
        .find_map(|p| p.get("publicationDate").and_then(|v| v.as_str()))
        .or_else(|| item.get("distributionDate").and_then(|v| v.as_str()))
        .map(str::to_string);
    let year = published
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());

    // First advertised PDF download. Paths are host-relative.
    let fulltext_url = item
        .get("downloads")
        .and_then(|v| v.as_array())
        .and_then(|list| {
            list.iter()
                .find_map(|d| d.pointer("/links/pdf").and_then(|v| v.as_str()))
        })
        .map(|path| {
            if path.starts_with("http") {
                path.to_string()
            } else {
                format!("{PUBLIC_HOST}{path}")
            }
        });

    let abstract_text = item
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    let mut external_ids = std::collections::BTreeMap::new();
    external_ids.insert("ntrs".to_string(), id.clone());

    Some(Paper {
        source: ID.to_string(),
        source_id: id.clone(),
        title,
        authors,
        year,
        published,
        doi,
        external_ids,
        abstract_text,
        url: format!("{PUBLIC_HOST}/citations/{id}"),
        fulltext_url: fulltext_url.clone(),
        fulltext_format: fulltext_url.map(|_| FulltextFormat::Pdf),
        journal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field shapes are the live API's: numeric `id`, path-relative download
    /// links, `authorAffiliations` with sequences out of order, and a
    /// `publications` entry carrying doi/venue/date. The second record is
    /// id-less: skipped from `papers` but counted raw.
    const FIXTURE: &str = r#"{
      "stats": {"took": 12, "total": 1159, "estimate": false},
      "results": [
        {
          "id": 20150002086,
          "title": "Fabrication of Turbine Disk Materials by Additive Manufacturing",
          "abstract": "Powder bed fusion of LSHR and ME209 disk superalloys.",
          "distributionDate": "2019-07-13T00:00:00.0000000+00:00",
          "authorAffiliations": [
            {"sequence": 1, "meta": {"author": {"name": "Second Author"}}},
            {"sequence": 0, "meta": {"author": {"name": "First Author"}}}
          ],
          "publications": [
            {"publicationDate": "2015-05-11T00:00:00.0000000+00:00",
             "doi": "10.1016/j.ijfatigue.2020.105953",
             "publicationName": "International Journal of Fatigue"}
          ],
          "downloads": [
            {"mimetype": "application/pdf",
             "links": {"pdf": "/api/citations/20150002086/downloads/20150002086.pdf"}}
          ]
        },
        {"title": "record without an id"}
      ]
    }"#;

    #[test]
    fn parses_live_shapes_and_counts_unparsed_raw() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(page.papers.len(), 1);
        // The id-less record is skipped from `papers` but counted as raw;
        // the server total survives.
        assert_eq!(page.raw_count, 2);
        assert_eq!(page.available, Some(1159));

        let p = &page.papers[0];
        assert_eq!(p.source, "ntrs");
        assert_eq!(p.source_id, "20150002086");
        assert_eq!(
            p.title,
            "Fabrication of Turbine Disk Materials by Additive Manufacturing"
        );
        // Ordered by sequence, not by array position.
        assert_eq!(p.authors, vec!["First Author", "Second Author"]);
        assert_eq!(p.year, Some(2015), "year comes from publicationDate");
        assert_eq!(p.doi.as_deref(), Some("10.1016/j.ijfatigue.2020.105953"));
        assert_eq!(
            p.journal.as_deref(),
            Some("International Journal of Fatigue")
        );
        assert_eq!(
            p.abstract_text.as_deref(),
            Some("Powder bed fusion of LSHR and ME209 disk superalloys.")
        );
        assert_eq!(p.url, "https://ntrs.nasa.gov/citations/20150002086");
        // Host-relative download path is joined onto the public host.
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://ntrs.nasa.gov/api/citations/20150002086/downloads/20150002086.pdf")
        );
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
        assert_eq!(
            p.external_ids.get("ntrs").map(String::as_str),
            Some("20150002086")
        );
    }

    #[test]
    fn a_bare_report_without_publications_uses_the_distribution_date() {
        let body = r#"{"stats": {"total": 1}, "results": [{
            "id": "20205008876",
            "title": "Component Applications using Metal Additive Manufacturing",
            "distributionDate": "2020-11-05T00:00:00.0000000+00:00"
        }]}"#;
        let page = parse(body.as_bytes()).unwrap();
        let p = &page.papers[0];
        assert_eq!(p.source_id, "20205008876", "string ids are accepted too");
        assert_eq!(p.year, Some(2020));
        assert_eq!(p.doi, None);
        assert_eq!(p.journal, None);
        assert_eq!(p.fulltext_url, None);
        assert_eq!(p.fulltext_format, None);
    }
}
