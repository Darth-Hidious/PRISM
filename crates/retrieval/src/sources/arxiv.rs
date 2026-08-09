//! arXiv — Atom API at `export.arxiv.org/api/query`. Machine-readable Atom
//! XML; no rendered page involved.

use anyhow::Result;
use quick_xml::events::Event;

use super::{FetchCtx, normalize_doi, url_encode};
use crate::model::{FulltextFormat, Paper, SourcePage};

const DEFAULT_BASE: &str = "http://export.arxiv.org/api/query";

pub const ID: &str = "arxiv";
pub const INITIAL_CURSOR: &str = "0";
/// Largest `max_results` this translator will put on the wire. Declared as
/// the adapter's capability and verified against the actual request in
/// `tests/capability_declarations.rs`.
pub const MAX_PAGE_SIZE: usize = 100;

pub async fn fetch(ctx: &FetchCtx, query: &str) -> Result<SourcePage> {
    let (page, _) = fetch_page(ctx, query, INITIAL_CURSOR).await?;
    Ok(page)
}

/// One page of results. Cursor is the `start` offset. A full page of RAW
/// entries means there may be more; an empty/short page ends the chain. The
/// gate and the cursor advance both use the raw entry count — a parser skip
/// (title-less entry) must neither end the chain nor re-read the tail.
pub async fn fetch_page(
    ctx: &FetchCtx,
    query: &str,
    cursor: &str,
) -> Result<(SourcePage, Option<String>)> {
    let start: usize = cursor.parse().unwrap_or(0);
    let base = ctx.base(ID, DEFAULT_BASE);
    let url = format!(
        "{base}?search_query=all:{q}&start={start}&max_results={n}",
        q = url_encode(query),
        n = ctx.limit.min(MAX_PAGE_SIZE)
    );
    let (body, _cached) = ctx.fetch_cached(ID, &url).await?;
    let page = parse(&body)?;
    let next = (page.raw_count >= ctx.limit.min(MAX_PAGE_SIZE))
        .then(|| (start + page.raw_count).to_string());
    Ok((page, next))
}

#[derive(Default)]
struct EntryDraft {
    id: String,
    title: String,
    summary: String,
    published: String,
    authors: Vec<String>,
    doi: Option<String>,
    pdf_url: Option<String>,
    journal_ref: Option<String>,
}

/// Parse the Atom feed. Pure function — unit-tested against fixture XML.
///
/// Returns every entry the feed carried in `raw_count` (parseable or not)
/// plus the feed-level `opensearch:totalResults` as `available`.
pub fn parse(body: &[u8]) -> Result<SourcePage> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);

    let mut papers: Vec<Paper> = Vec::new();
    let mut raw_count = 0usize;
    let mut available: Option<u64> = None;
    let mut buf: Vec<u8> = Vec::new();
    let mut draft: Option<EntryDraft> = None;
    // Which entry-local element is collecting text right now.
    let mut field: Option<&'static str> = None;
    // Feed-level opensearch:totalResults is collected outside any entry.
    let mut in_total_results = false;
    // Text accumulates here across Text/GeneralRef events until the element
    // ends, so entity references splitting a chunk cannot tear a value.
    let mut text_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Text(e) => text_buf.push_str(&e.decode().unwrap_or_default()),
            Event::GeneralRef(e) => {
                text_buf.push_str(&super::resolve_reference(&e.decode().unwrap_or_default()))
            }
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"entry" => draft = Some(EntryDraft::default()),
                b"totalResults" if draft.is_none() => {
                    in_total_results = true;
                    text_buf.clear();
                }
                b"id" if draft.is_some() => field = Some("id"),
                b"title" if draft.is_some() => field = Some("title"),
                b"summary" if draft.is_some() => field = Some("summary"),
                b"published" if draft.is_some() => field = Some("published"),
                b"name" if draft.is_some() => field = Some("author"),
                b"doi" if draft.is_some() => field = Some("doi"),
                b"journal_ref" if draft.is_some() => field = Some("journal_ref"),
                b"link" if draft.is_some() => {
                    let mut title_attr = None;
                    let mut href = None;
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"title" => title_attr = Some(attr.value.to_vec()),
                            b"href" => href = Some(attr.value.to_vec()),
                            _ => {}
                        }
                    }
                    if title_attr.as_deref() == Some(b"pdf")
                        && let Some(href) = href
                    {
                        draft.as_mut().unwrap().pdf_url =
                            Some(String::from_utf8_lossy(&href).into_owned());
                    }
                }
                _ => {}
            },
            Event::End(e) => {
                let local = e.local_name();
                if in_total_results && local.as_ref() == b"totalResults" {
                    available = std::mem::take(&mut text_buf).trim().parse::<u64>().ok();
                    in_total_results = false;
                    buf.clear();
                    continue;
                }
                let ended_field: Option<&'static str> = match local.as_ref() {
                    b"id" => Some("id"),
                    b"title" => Some("title"),
                    b"summary" => Some("summary"),
                    b"published" => Some("published"),
                    b"name" => Some("author"),
                    b"doi" => Some("doi"),
                    b"journal_ref" => Some("journal_ref"),
                    _ => None,
                };
                if let Some(f) = ended_field
                    && field == Some(f)
                    && let Some(d) = draft.as_mut()
                {
                    let value = std::mem::take(&mut text_buf);
                    match f {
                        "id" => d.id.push_str(value.trim()),
                        "title" => d.title.push_str(value.trim()),
                        "summary" => d.summary.push_str(value.trim()),
                        "published" => d.published.push_str(value.trim()),
                        "author" => {
                            let name = value.trim().to_string();
                            if !name.is_empty() {
                                d.authors.push(name);
                            }
                        }
                        "doi" => d.doi = normalize_doi(&value),
                        "journal_ref" if !value.trim().is_empty() => {
                            d.journal_ref = Some(value.trim().to_string());
                        }
                        _ => {}
                    }
                    field = None;
                }
                if local.as_ref() == b"entry"
                    && let Some(d) = draft.take()
                {
                    raw_count += 1;
                    if let Some(paper) = finalize(d) {
                        papers.push(paper);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(SourcePage {
        papers,
        raw_count,
        available,
    })
}

fn finalize(d: EntryDraft) -> Option<Paper> {
    // The entry id looks like http://arxiv.org/abs/2401.12345v1 — keep the
    // bare arXiv id as the source identifier.
    let arxiv_id = d.id.rsplit('/').next().unwrap_or(&d.id).to_string();
    if d.title.trim().is_empty() || arxiv_id.trim().is_empty() {
        return None;
    }
    let mut external_ids = std::collections::BTreeMap::new();
    external_ids.insert("arxiv".to_string(), arxiv_id.clone());
    let year = d.published.get(..4).and_then(|y| y.parse::<i32>().ok());
    let url = if d.id.starts_with("http") {
        d.id.clone()
    } else {
        format!("https://arxiv.org/abs/{arxiv_id}")
    };
    let has_pdf = d.pdf_url.is_some();
    Some(Paper {
        source: ID.to_string(),
        source_id: arxiv_id,
        title: collapse_whitespace(&d.title),
        authors: d.authors,
        year,
        published: Some(d.published).filter(|s| !s.is_empty()),
        doi: d.doi,
        external_ids,
        abstract_text: Some(collapse_whitespace(&d.summary)).filter(|s| !s.is_empty()),
        url,
        fulltext_url: d.pdf_url,
        fulltext_format: has_pdf.then_some(FulltextFormat::Pdf),
        journal: d.journal_ref,
    })
}

/// Collapse internal whitespace (Atom pretty-prints titles across lines).
pub fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom" xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/">
  <opensearch:totalResults>250</opensearch:totalResults>
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <published>2024-01-22T18:00:00Z</published>
    <title>Lattice thermal conductivity of
      high entropy alloys</title>
    <summary>We measure thermal transport.</summary>
    <author><name>A. Researcher</name></author>
    <author><name>B. Colleague</name></author>
    <arxiv:doi>10.1234/HEA.2024</arxiv:doi>
    <arxiv:journal_ref>Phys. Rev. B 99, 014301 (2024)</arxiv:journal_ref>
    <link href="http://arxiv.org/abs/2401.12345v1" rel="alternate" type="text/html"/>
    <link title="pdf" href="http://arxiv.org/pdf/2401.12345v1" rel="related" type="application/pdf"/>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2402.00001v2</id>
    <published>2024-02-01T00:00:00Z</published>
    <title>Second paper</title>
    <summary>Short.</summary>
    <author><name>C. Person</name></author>
  </entry>
</feed>"#;

    #[test]
    fn parses_entries_with_metadata() {
        let page = parse(FIXTURE.as_bytes()).unwrap();
        let papers = &page.papers;
        assert_eq!(papers.len(), 2);
        assert_eq!(page.raw_count, 2, "every raw entry counts, parsed or not");
        assert_eq!(
            page.available,
            Some(250),
            "opensearch:totalResults is the server's own total; it must not be discarded"
        );
        let first = &papers[0];
        assert_eq!(first.source_id, "2401.12345v1");
        assert_eq!(
            first.title,
            "Lattice thermal conductivity of high entropy alloys"
        );
        assert_eq!(first.authors, vec!["A. Researcher", "B. Colleague"]);
        assert_eq!(first.year, Some(2024));
        assert_eq!(first.doi.as_deref(), Some("10.1234/hea.2024"));
        assert_eq!(
            first.fulltext_url.as_deref(),
            Some("http://arxiv.org/pdf/2401.12345v1")
        );
        assert_eq!(first.fulltext_format, Some(FulltextFormat::Pdf));
        assert_eq!(
            first.journal.as_deref(),
            Some("Phys. Rev. B 99, 014301 (2024)")
        );
        assert_eq!(first.dedup_key(), "doi:10.1234/hea.2024");
        // Second entry: no DOI — dedup falls back to the arXiv id.
        assert_eq!(papers[1].dedup_key(), "arxiv:2402.00001v2");
    }

    /// Pins the `journal_ref` arm's GUARD, which nothing else exercises.
    ///
    /// The arm is written `"journal_ref" if !value.trim().is_empty()`, so a
    /// blank one must fall through to `_ => {}` and leave `journal` as `None`.
    /// Both existing fixtures miss this: the first carries a non-empty
    /// journal_ref, the second omits the element entirely — that tests "field
    /// never entered", a different path. So the guard's false branch had no
    /// coverage, and rewriting it as an unguarded arm with the check dropped
    /// would still have passed every test in the crate.
    ///
    /// Empty and whitespace-only are separate cases on purpose: `is_empty()`
    /// alone would accept `"   "` and store a blank journal string.
    #[test]
    fn a_blank_journal_ref_is_not_recorded_as_a_journal() {
        let feed = |inner: &str| {
            format!(
                r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2403.00009v1</id>
    <title>Blank journal</title>
    {inner}
  </entry>
</feed>"#
            )
        };

        for inner in [
            "<arxiv:journal_ref></arxiv:journal_ref>",
            "<arxiv:journal_ref>   </arxiv:journal_ref>",
            "<arxiv:journal_ref>\n\t</arxiv:journal_ref>",
        ] {
            let papers = parse(feed(inner).as_bytes()).unwrap().papers;
            assert_eq!(papers.len(), 1, "fixture should yield one entry: {inner}");
            assert_eq!(
                papers[0].journal, None,
                "blank journal_ref must not be recorded: {inner:?}"
            );
        }

        // The positive half, so the assertions above cannot pass against a
        // parser that simply never populates `journal`.
        let papers = parse(feed("<arxiv:journal_ref> Nature 1 </arxiv:journal_ref>").as_bytes())
            .unwrap()
            .papers;
        assert_eq!(papers[0].journal.as_deref(), Some("Nature 1"));
    }

    #[test]
    fn empty_feed_yields_nothing_not_garbage() {
        let page = parse(b"<feed xmlns=\"http://www.w3.org/2005/Atom\"></feed>").unwrap();
        assert!(page.papers.is_empty());
        assert_eq!(page.raw_count, 0);
        assert_eq!(page.available, None, "no total reported means None, not 0");
    }

    /// A skippable entry (title-less) must count toward `raw_count` — that
    /// count is what pagination gates on, so losing it ends a chain early.
    #[test]
    fn a_skipped_entry_still_counts_as_raw() {
        let feed = r#"<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.00001v1</id>
    <title>Kept entry</title>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2401.00002v1</id>
    <title>   </title>
  </entry>
</feed>"#;
        let page = parse(feed.as_bytes()).unwrap();
        assert_eq!(page.papers.len(), 1, "the blank-title entry is skipped");
        assert_eq!(page.raw_count, 2, "but it was served, so it counts as raw");
    }
}
