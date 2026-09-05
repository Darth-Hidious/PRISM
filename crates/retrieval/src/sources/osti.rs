//! OSTI.GOV — the US Department of Energy's research index,
//! `www.osti.gov/api/v1/records`. JSON, no key. DOE-funded materials work
//! (national laboratories, fusion and fission materials, additive
//! manufacturing) that the journal indexes carry late or not at all, with a
//! `fulltext` link on most records that resolves to the accepted manuscript.
//!
//! Wire contract, verified live 2026-09-05:
//! * pagination is `page` (1-based) and `rows`; the server honoured
//!   `rows=200` live, so the 100-row cap below is this translator's own
//!   politeness posture, matching the other adapters, not a server limit;
//! * the body is a bare JSON array of records — the server total travels in
//!   the `X-Total-Count` response header and the successor page in a `Link`
//!   header, neither of which the body parser sees, so `available` is left
//!   unknown rather than guessed;
//! * authors are `"Last, First [Affiliation] (ORCID:…)"` strings.

use anyhow::Result;
use serde_json::Value;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "https://www.osti.gov/api/v1";
const PUBLIC_HOST: &str = "https://www.osti.gov";

pub const ID: &str = "osti";
/// Pages are 1-based on the wire.
pub const INITIAL_CURSOR: &str = "1";
/// Largest `rows` this translator puts on the wire. The server accepts more
/// (200 observed live); 100 matches the politeness posture of the other
/// adapters and is verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page. Cursor is the 1-based page number. The continuation gate uses
/// the RAW result count the server returned — a skipped record must not end
/// the chain.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let page_no: usize = cursor.parse().unwrap_or(1).max(1);
    let rows = ctx.limit.min(MAX_PAGE_SIZE);
    let base = ctx.base(ID, DEFAULT_BASE);
    let url = format!(
        "{base}/records?q={q}&rows={rows}&page={page_no}",
        q = url_encode(query)
    );
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let page = parse(&body)?;
    let next = (page.raw_count >= rows).then(|| (page_no + 1).to_string());
    Ok((page, next))
}

/// Pure parser over the records response: a bare array. `raw_count` is every
/// record served, parsed or not; `available` is unknown from the body alone.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let root: Value = serde_json::from_slice(body)?;
    let results = root.as_array().cloned().unwrap_or_default();
    let mut papers = Vec::new();
    for item in &results {
        if let Some(paper) = parse_item(item) {
            papers.push(paper);
        }
    }
    Ok(SourcePage {
        papers,
        raw_count: results.len(),
        available: None,
    })
}

fn parse_item(item: &Value) -> Option<Paper> {
    // `osti_id` is a string live; take a number too rather than depend on it.
    let id = match item.get("osti_id")? {
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
    let authors: Vec<String> = item
        .get("authors")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| a.as_str())
                .filter_map(author_name)
                .collect()
        })
        .unwrap_or_default();
    let published = item
        .get("publication_date")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_string);
    let year = published
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i32>().ok());
    // The DOI arrives as a full `https://doi.org/…` URL; normalise to the bare id.
    let doi = item
        .get("doi")
        .and_then(|v| v.as_str())
        .and_then(normalize_doi);
    let link = |rel: &str| {
        item.get("links")
            .and_then(|v| v.as_array())
            .and_then(|list| {
                list.iter().find_map(|l| {
                    (l.get("rel").and_then(|v| v.as_str()) == Some(rel))
                        .then(|| l.get("href").and_then(|v| v.as_str()))
                        .flatten()
                })
            })
            .map(str::to_string)
    };
    let url = link("citation").unwrap_or_else(|| format!("{PUBLIC_HOST}/biblio/{id}"));
    let fulltext_url = link("fulltext");
    let abstract_text = item
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let journal = item
        .get("journal_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|j| !j.is_empty())
        .map(str::to_string);

    let mut external_ids = std::collections::BTreeMap::new();
    external_ids.insert("osti".to_string(), id.clone());

    Some(Paper {
        source: ID.to_string(),
        source_id: id,
        title,
        authors,
        year,
        published,
        doi,
        external_ids,
        abstract_text,
        url,
        fulltext_url: fulltext_url.clone(),
        // The `fulltext` link serves the accepted manuscript as a PDF.
        fulltext_format: fulltext_url.map(|_| FulltextFormat::Pdf),
        journal,
    })
}

/// `"Last, First [Affiliation] (ORCID:0000…)"` → `"Last, First"`.
fn author_name(raw: &str) -> Option<String> {
    let name = raw.split(['[', '(']).next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field shapes are the live API's (2026-09-05): a bare array, string
    /// `osti_id`, bracketed affiliations and ORCIDs on authors, ISO dates,
    /// `description` as the abstract, and `links[]` with `citation` and
    /// `fulltext` rels. The second record has no title: skipped from
    /// `papers` but counted raw.
    const FIXTURE: &str = r#"[
      {
        "osti_id": "3682468",
        "title": "The U.S. Fusion Materials Community Roadmap",
        "authors": ["Ferry, Sara [Massachusetts Institute of Technology]",
                    "Kato, Yutai [ORNL] (ORCID:0000000194945862)"],
        "publication_date": "2026-08-01T00:00:00Z",
        "doi": "https://doi.org/10.1016/j.cossms.2026.101291",
        "description": "Near-term research priorities for fusion materials.",
        "journal_name": "Current Opinion in Solid State & Materials Science",
        "product_type": "Journal Article",
        "links": [
          {"rel": "citation", "href": "https://www.osti.gov/biblio/3682468"},
          {"rel": "fulltext", "href": "https://www.osti.gov/servlets/purl/3682468"}
        ]
      },
      {"osti_id": "1", "authors": []}
    ]"#;

    #[test]
    fn parses_live_shapes_and_counts_unparsed_raw() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        assert_eq!(page.raw_count, 2, "every record served is counted");
        assert_eq!(page.papers.len(), 1, "the title-less record is skipped");
        assert!(
            page.available.is_none(),
            "the total is a header the body cannot see"
        );
        let p = &page.papers[0];
        assert_eq!(p.source, "osti");
        assert_eq!(p.source_id, "3682468");
        assert_eq!(p.title, "The U.S. Fusion Materials Community Roadmap");
        assert_eq!(p.authors, vec!["Ferry, Sara", "Kato, Yutai"]);
        assert_eq!(p.year, Some(2026));
        assert_eq!(p.published.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(p.doi.as_deref(), Some("10.1016/j.cossms.2026.101291"));
        assert_eq!(
            p.external_ids.get("osti").map(String::as_str),
            Some("3682468")
        );
        assert_eq!(
            p.abstract_text.as_deref(),
            Some("Near-term research priorities for fusion materials.")
        );
        assert_eq!(p.url, "https://www.osti.gov/biblio/3682468");
        assert_eq!(
            p.fulltext_url.as_deref(),
            Some("https://www.osti.gov/servlets/purl/3682468")
        );
        assert_eq!(p.fulltext_format, Some(FulltextFormat::Pdf));
        assert_eq!(
            p.journal.as_deref(),
            Some("Current Opinion in Solid State & Materials Science")
        );
    }

    #[test]
    fn a_record_without_links_still_has_a_landing_page() {
        let body = r#"[{"osti_id": 42, "title": "Bare record"}]"#;
        let page = parse(body.as_bytes()).unwrap();
        assert_eq!(page.papers.len(), 1);
        assert_eq!(page.papers[0].url, "https://www.osti.gov/biblio/42");
        assert!(page.papers[0].fulltext_url.is_none());
    }

    #[test]
    fn author_names_lose_affiliation_and_orcid() {
        assert_eq!(
            author_name("Ferry, Sara [MIT]").as_deref(),
            Some("Ferry, Sara")
        );
        assert_eq!(
            author_name("Kato, Yutai [ORNL] (ORCID:0000000194945862)").as_deref(),
            Some("Kato, Yutai")
        );
        assert_eq!(author_name("  "), None);
    }
}
