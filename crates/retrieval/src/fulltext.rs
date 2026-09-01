//! Full-text retrieval and structure-preserving extraction.
//!
//! JATS XML (PMC open access) is preferred: it carries sections, tables and
//! captions natively. PDF is the fallback and yields flat text only — the
//! locator says so honestly instead of pretending to have structure.
//!
//! Locators are the bridge to EMMO ingestion: a claim extracted from a block
//! cites the block's section/label/offset so a human can find it.

use anyhow::{Context, Result};
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};

use crate::model::{FulltextFormat, Paper};
use crate::sources::FetchCtx;

/// Europe PMC full-text endpoint: one GET returns JATS XML directly for any
/// PMC-indexed open-access article. This is the primary route — NCBI's own
/// OA packages are referenced by FTP hrefs that no longer resolve over HTTPS.
const EUROPE_PMC_FULLTEXT: &str = "https://www.ebi.ac.uk/europepmc/webservices/rest";

/// PMC OA web service (fallback): given a PMCID it answers with a link to
/// the open access package (a tar.gz holding the JATS `.nxml`).
const PMC_OA_SERVICE: &str = "https://www.ncbi.nlm.nih.gov/pmc/utils/oa/oa.fcgi";

/// What kind of document region a block came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Title,
    Abstract,
    Body,
    Table,
    Caption,
}

/// Where a block lives inside its document. Enough for a human or an
/// extractor to locate a claim's context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Locator {
    pub kind: BlockKind,
    /// Section title path, e.g. `["2. Methods", "2.1 Synthesis"]`.
    pub section_path: Vec<String>,
    /// Label such as "Table 1" or "Figure 2" for tables/figures.
    pub label: Option<String>,
    /// Character offset of this block in `Fulltext::plain_text`.
    pub char_offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub locator: Locator,
    pub text: String,
}

/// A retrieved, parsed full text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fulltext {
    /// The URL actually fetched.
    pub source_url: String,
    pub format: FulltextFormat,
    pub blocks: Vec<TextBlock>,
    /// All block text concatenated (blocks carry their offsets into this).
    pub plain_text: String,
}

impl Fulltext {
    pub fn char_count(&self) -> usize {
        self.plain_text.chars().count()
    }
}

/// Fetch the best available full text for a paper.
///
/// Priority: PMC JATS (structured) > declared JATS URL > declared PDF/other
/// URL (sniffed). Returns `Ok(None)` when nothing is advertised — an absent
/// full text is reported as absent, never papered over.
pub async fn fetch_fulltext(ctx: &FetchCtx, paper: &Paper) -> Result<Option<Fulltext>> {
    // Priority 1: PMC open-access JATS (structured) when we know the PMCID.
    if let Some(pmcid) = paper.external_ids.get("pmc") {
        match fetch_pmc_jats(ctx, pmcid).await {
            Ok(Some(fulltext)) => return Ok(Some(fulltext)),
            Ok(None) => {
                tracing::debug!("{pmcid} is not in the PMC open-access subset");
            }
            Err(e) => {
                tracing::warn!("PMC JATS fetch failed for {pmcid}: {e:#}");
            }
        }
    }

    // Priority 2: whatever full-text location the metadata advertised.
    if let Some(url) = &paper.fulltext_url {
        let declared_format = paper.fulltext_format;
        match download(ctx, url).await {
            Ok(body) => {
                let format = declared_format.unwrap_or_else(|| sniff(&body));
                let parsed = match format {
                    FulltextFormat::Jats => parse_jats(&body),
                    FulltextFormat::Pdf => parse_pdf(&body),
                };
                match parsed {
                    Ok(fulltext) => {
                        return Ok(Some(Fulltext {
                            source_url: url.clone(),
                            ..fulltext
                        }));
                    }
                    Err(e) => {
                        tracing::warn!("full-text parse failed for {url}: {e:#}");
                    }
                }
            }
            Err(e) => {
                tracing::warn!("full-text download failed for {url}: {e:#}");
            }
        }
    }
    Ok(None)
}

/// Resolve a PMCID to its JATS full text via the PMC OA web service:
/// `oa.fcgi` answers with a tar.gz package link; the package holds the
/// JATS `.nxml`. Papers outside the OA subset yield `Ok(None)` — absent is
/// reported as absent.
/// Resolve a PMCID to its JATS full text.
///
/// Route 1: Europe PMC `fullTextXML` — one GET, JATS directly.
/// Route 2: NCBI OA web service → tar.gz package → `.nxml`.
/// Papers with no open-access full text yield `Ok(None)` — absent is
/// reported as absent.
pub async fn fetch_pmc_jats(ctx: &FetchCtx, pmcid: &str) -> Result<Option<Fulltext>> {
    // Route 1: Europe PMC.
    let epmc_url = format!("{EUROPE_PMC_FULLTEXT}/{pmcid}/fullTextXML");
    match download(ctx, &epmc_url).await {
        Ok(body) => {
            let head = body
                .iter()
                .copied()
                .skip_while(|b| b.is_ascii_whitespace())
                .take(1)
                .collect::<Vec<u8>>();
            if head.first() == Some(&b'<') {
                match parse_jats(&body) {
                    Ok(mut fulltext) => {
                        fulltext.source_url = epmc_url.clone();
                        return Ok(Some(fulltext));
                    }
                    Err(e) => {
                        tracing::warn!("Europe PMC JATS parse failed for {pmcid}: {e:#}")
                    }
                }
            } else {
                tracing::debug!("Europe PMC returned no XML for {pmcid}");
            }
        }
        Err(e) => tracing::warn!("Europe PMC full text failed for {pmcid}: {e:#}"),
    }

    // Route 2: NCBI OA listing + package.
    let service_url = format!("{PMC_OA_SERVICE}?id={pmcid}");
    let listing = download(ctx, &service_url)
        .await
        .with_context(|| format!("querying PMC OA service for {pmcid}"))?;
    let Some(package_url) = parse_oa_listing(&listing)? else {
        return Ok(None);
    };
    // NCBI serves the same public path over HTTPS; never speak FTP.
    let https_url = package_url.replacen("ftp://", "https://", 1);
    let package = download(ctx, &https_url)
        .await
        .with_context(|| format!("downloading PMC OA package {https_url}"))?;
    let nxml =
        extract_nxml(&package).with_context(|| format!("unpacking PMC OA package for {pmcid}"))?;
    let mut fulltext = parse_jats(&nxml)?;
    fulltext.source_url = https_url;
    Ok(Some(fulltext))
}

/// Extract the `href` of the tgz link from an oa.fcgi response. `Ok(None)`
/// when the record is missing or an error element is returned.
pub fn parse_oa_listing(body: &[u8]) -> Result<Option<String>> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buf: Vec<u8> = Vec::new();
    let mut href: Option<String> = None;
    let mut errored = false;
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"link" => {
                    let mut format_attr = None;
                    let mut href_attr = None;
                    for attr in e.attributes().flatten() {
                        match attr.key.as_ref() {
                            b"format" => format_attr = Some(attr.value.to_vec()),
                            b"href" => href_attr = Some(attr.value.to_vec()),
                            _ => {}
                        }
                    }
                    if format_attr.as_deref() == Some(b"tgz")
                        && let Some(h) = href_attr
                    {
                        href = Some(String::from_utf8_lossy(&h).into_owned());
                    }
                }
                b"error" => errored = true,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if errored && href.is_none() {
        return Ok(None);
    }
    Ok(href)
}

/// Find the first `.nxml` member of a tar.gz package and return its bytes.
pub fn extract_nxml(package: &[u8]) -> Result<Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(package);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("reading tar entries")? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path.to_string_lossy();
        if name.ends_with(".nxml") {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut out)?;
            return Ok(out);
        }
    }
    anyhow::bail!("no .nxml member found in PMC OA package")
}

async fn download(ctx: &FetchCtx, url: &str) -> Result<Vec<u8>> {
    // Full texts live on many hosts; one shared polite limiter keeps the
    // engine from bursting at any of them.
    let body = crate::http::get_with_retry(
        &ctx.client,
        &ctx.fulltext_limiter,
        url,
        &ctx.headers,
        ctx.max_attempts,
    )
    .await
    .with_context(|| format!("downloading full text from {url}"))?;
    Ok(body.to_vec())
}

/// Decide format from the bytes themselves: publishers mislabel links.
pub fn sniff(body: &[u8]) -> FulltextFormat {
    let head: Vec<u8> = body
        .iter()
        .take(1024)
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .take(64)
        .collect();
    if head.starts_with(b"%PDF") {
        FulltextFormat::Pdf
    } else {
        FulltextFormat::Jats
    }
}

/// Parse PDF bytes to flat text. One Body block; the locator carries no
/// section path because PDF extraction cannot honestly recover one.
/// Run a parser and turn a PANIC into an ordinary `Err`.
///
/// Extracted so the containment itself is testable: a fixture that merely makes
/// `pdf_extract` return `Err` proves nothing about unwinding, and the first
/// version of this test passed with the containment removed.
fn contain_parser_panic<T>(f: impl FnOnce() -> T) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        anyhow::anyhow!("pdf parser panicked on this document: {detail}")
    })
}

pub fn parse_pdf(body: &[u8]) -> Result<Fulltext> {
    // `pdf_extract` PANICS on some real-world PDFs rather than returning Err —
    // measured 2026-08-20: "missing unicode map and encoding" (pdf-extract
    // 0.12.0) killed an entire `papers corpus` harvest after 32 documents had
    // already landed. One malformed file from one publisher ended the run.
    //
    // A panic is neither "reported" nor "skipped", so it breaks this command's
    // stated contract that papers whose full text is not retrievable are
    // REPORTED, not silently dropped. Catching it turns a fatal third-party
    // panic into an ordinary per-document error the caller already knows how to
    // record and move past.
    //
    // The payload is a `&[u8]` and the closure borrows nothing else, so there is
    // no broken invariant to observe afterwards: `AssertUnwindSafe` is honest
    // here rather than a way to silence the compiler.
    let extracted = contain_parser_panic(|| pdf_extract::extract_text_from_mem(body))?;
    let text = extracted.map_err(|e| anyhow::anyhow!("pdf text extraction failed: {e}"))?;
    let text = text.trim().to_string();
    if text.is_empty() {
        anyhow::bail!("pdf contained no extractable text (scanned image?)");
    }
    let blocks = vec![TextBlock {
        locator: Locator {
            kind: BlockKind::Body,
            section_path: Vec::new(),
            label: None,
            char_offset: 0,
        },
        text: text.clone(),
    }];
    Ok(Fulltext {
        source_url: String::new(),
        format: FulltextFormat::Pdf,
        blocks,
        plain_text: text,
    })
}

/// Parse JATS XML into located blocks. Sections nest; tables and figure
/// wraps are captured with their labels and captions.
///
/// Wraps are captured from THREE zones, not just `<body>`: measured on a
/// real PMC document (PMC13302085, MDPI *Materials*), every `<table-wrap>`
/// and every `<fig>` lived in `<floats-group>` (after `</back>`) or in
/// `<back>/<app-group>/<app>` — the body held only `<xref>` pointers to
/// them. A body-only parser silently dropped ALL five tables and ALL eight
/// figure captions of that paper. `<ref-list>` stays excluded: bibliography
/// text is not document content.
///
/// Table structure is preserved into the text, because the text is the only
/// thing the reader and the citation spans ever see:
/// - each row is one line (both JATS table models),
/// - cells within a row are separated by `|`, and an EMPTY cell keeps its
///   `|` so later cells stay under their own column headers,
/// - the wrap's label is visible: a captioned wrap renders as
///   `"Table 1. <caption>"`, an uncaptioned one carries a `"Table 1"` line
///   above its rows. Invisible labels made every table unfindable by name —
///   searching "Table 3" hit prose mentions only, never the table itself.
pub fn parse_jats(body: &[u8]) -> Result<Fulltext> {
    let mut reader = quick_xml::Reader::from_reader(body);
    reader.config_mut().trim_text(true);

    let mut blocks: Vec<(BlockKind, Vec<String>, Option<String>, String)> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();

    // Open sections, tagged with their <sec> nesting depth so title-less
    // sections still close correctly.
    let mut section_stack: Vec<(usize, String)> = Vec::new();
    let mut depth_front: i32 = 0;
    let mut depth_body: i32 = 0;
    // <floats-group> and <app> hold the tables and figures many publishers
    // (MDPI among them) keep OUT of <body>.
    let mut depth_floats: i32 = 0;
    let mut depth_app: i32 = 0;
    let mut depth_sec: usize = 0;
    // The most recent <sec> is waiting for its <title>.
    let mut awaiting_sec_title = false;
    // Inside <object-id>: publisher-internal identifiers
    // ("materials-19-02487-t0A1_Table A1"), not document text.
    let mut in_object_id = false;
    // The current wrap's caption block was emitted (and carries the label
    // heading), so the wrap-close block must not repeat the label.
    let mut caption_emitted = false;

    #[derive(Clone, Copy, PartialEq)]
    enum Sink {
        Title,
        Abstract,
        Paragraph,
        Wrap,
        Caption,
        SecTitle,
    }
    let mut sink: Option<Sink> = None;
    // Which element opened the current wrap: "table-wrap" or "fig".
    let mut wrap_element: Option<&'static str> = None;
    // Inside a <xref ref-type="bibr"> whose text is being wrapped in [...]
    // so it reads as the bracketed citation marker it is by construction.
    let mut wrapping_bibr_xref = false;
    let mut current_label: Option<String> = None;
    let mut text = String::new();

    let section_path = |stack: &[(usize, String)]| -> Vec<String> {
        stack
            .iter()
            .map(|(_, t)| t.clone())
            .filter(|t| !t.is_empty())
            .collect()
    };

    loop {
        let chunk: Option<String> = match reader.read_event_into(&mut buf)? {
            Event::Text(e) => Some(e.decode().unwrap_or_default().into_owned()),
            Event::GeneralRef(e) => Some(crate::sources::resolve_reference(
                &e.decode().unwrap_or_default(),
            )),
            Event::Start(e) => {
                let mut start_chunk: Option<String> = None;
                match e.local_name().as_ref() {
                    b"front" => depth_front += 1,
                    b"body" => depth_body += 1,
                    b"floats-group" => depth_floats += 1,
                    b"app" => depth_app += 1,
                    b"object-id" => in_object_id = true,
                    b"sec" if depth_body > 0 => {
                        depth_sec += 1;
                        awaiting_sec_title = true;
                    }
                    b"article-title" if depth_front > 0 && sink.is_none() => {
                        sink = Some(Sink::Title);
                        text.clear();
                    }
                    b"abstract" if depth_front > 0 && sink.is_none() => {
                        sink = Some(Sink::Abstract);
                        text.clear();
                    }
                    b"title" if depth_body > 0 && awaiting_sec_title => {
                        sink = Some(Sink::SecTitle);
                        text.clear();
                    }
                    b"table-wrap"
                        if (depth_body > 0 || depth_floats > 0 || depth_app > 0)
                            && sink.is_none() =>
                    {
                        sink = Some(Sink::Wrap);
                        wrap_element = Some("table-wrap");
                        text.clear();
                        current_label = None;
                        caption_emitted = false;
                    }
                    b"fig"
                        if (depth_body > 0 || depth_floats > 0 || depth_app > 0)
                            && sink.is_none() =>
                    {
                        sink = Some(Sink::Wrap);
                        wrap_element = Some("fig");
                        text.clear();
                        current_label = None;
                        caption_emitted = false;
                    }
                    b"label" if sink == Some(Sink::Wrap) && current_label.is_none() => {
                        // Sentinel: the next text chunk is this wrap's label.
                        text.push('\u{1}');
                    }
                    b"caption" if sink == Some(Sink::Wrap) => {
                        sink = Some(Sink::Caption);
                        text.clear();
                    }
                    b"tr" | b"row" if sink == Some(Sink::Wrap) => {
                        // Preserve row structure: each row starts on its own
                        // line so evidence spans never straddle rows. Rows
                        // joined with spaces fuse the whole table into one
                        // span, letting a number from one row "support" a
                        // claim whose subject lives in another row.
                        //
                        // JATS permits TWO table models: XHTML (<tr>) and
                        // OASIS/CALS (<tgroup>/<row>/<entry>); publishers use
                        // both, so <row> is a row boundary exactly like <tr>.
                        text.push('\n');
                    }
                    // Preserve CELL structure: cells joined with bare
                    // spaces destroyed column identity — "Inconel 718
                    // 1375" cannot be split back into alloy and value,
                    // and a multi-word cell swallows its neighbours.
                    // ~85% of reported compositions/properties live in
                    // tables (DiSCoMaT, ACL 2023); the delimiter is what
                    // lets a reader bind a value to its column header.
                    b"td" | b"th" | b"entry"
                        if sink == Some(Sink::Wrap)
                            && !text.is_empty()
                            && !text.ends_with('\n') =>
                    {
                        text.push_str(" |");
                    }
                    b"p" if (depth_body > 0 || depth_app > 0) && sink.is_none() => {
                        // <app> paragraphs are document content (appendix
                        // derivations, symbol definitions); <notes> and
                        // <ref-list> paragraphs stay excluded because neither
                        // zone opens a capture depth.
                        sink = Some(Sink::Paragraph);
                        text.clear();
                    }
                    // The text of a bibliographic-reference xref is a
                    // citation marker by construction. Superscript numbering
                    // renders it as a bare number ("studied 1140."), which
                    // the claims guard cannot tell from a measurement; wrap
                    // it in [...] so it reads as the bracketed citation it
                    // is and the existing citation guard refuses it.
                    b"xref" if sink.is_some() => {
                        let is_bibr = e
                            .attributes()
                            .flatten()
                            .any(|a| a.key.as_ref() == b"ref-type" && a.value.as_ref() == b"bibr");
                        if is_bibr {
                            wrapping_bibr_xref = true;
                            start_chunk = Some("[".to_string());
                        }
                    }
                    _ => {}
                }
                start_chunk
            }
            // A self-closing tag carries no content and never gets an End
            // event: it must not open a sink or a depth, or the parser
            // wedges on it and silently drops the rest of the document
            // (e.g. a bare <table-wrap/>).
            //
            // One exception acts without opening anything: a self-closing
            // cell (<td/>) is an EMPTY CELL, not nothing. It must keep its
            // delimiter or every later cell in the row shifts left one
            // column and binds to the wrong header.
            Event::Empty(e) => {
                if sink == Some(Sink::Wrap)
                    && matches!(e.local_name().as_ref(), b"td" | b"th" | b"entry")
                    && !text.is_empty()
                    && !text.ends_with('\n')
                {
                    text.push_str(" |");
                }
                None
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    b"front" => depth_front -= 1,
                    b"body" => depth_body -= 1,
                    b"floats-group" => depth_floats -= 1,
                    b"app" => depth_app -= 1,
                    b"object-id" => in_object_id = false,
                    b"sec" if depth_body >= 0 => {
                        // Close every section entry opened at this depth (there is
                        // at most one per depth).
                        section_stack.retain(|(d, _)| *d < depth_sec);
                        depth_sec = depth_sec.saturating_sub(1);
                        awaiting_sec_title = false;
                    }
                    b"article-title" if sink == Some(Sink::Title) => {
                        blocks.push((BlockKind::Title, vec![], None, take(&mut text)));
                        sink = None;
                    }
                    b"abstract" if sink == Some(Sink::Abstract) => {
                        blocks.push((BlockKind::Abstract, vec![], None, take(&mut text)));
                        sink = None;
                    }
                    b"title" if sink == Some(Sink::SecTitle) => {
                        let title = take(&mut text);
                        section_stack.push((depth_sec, title));
                        awaiting_sec_title = false;
                        sink = None;
                    }
                    b"p" if sink == Some(Sink::Paragraph) => {
                        let body_text = take(&mut text);
                        if !body_text.is_empty() {
                            blocks.push((
                                BlockKind::Body,
                                section_path(&section_stack),
                                None,
                                body_text,
                            ));
                        }
                        sink = None;
                    }
                    b"caption" if sink == Some(Sink::Caption) => {
                        let caption = take(&mut text);
                        if !caption.is_empty() {
                            // The label joins the visible text: "Table 1.
                            // Measured conductivity…" is how the document
                            // itself names the table, and it is the string a
                            // reader searches for. A label held only in the
                            // locator made every table unfindable by name.
                            let caption = match current_label.as_deref() {
                                Some(label) => {
                                    format!("{}. {caption}", label.trim_end_matches('.'))
                                }
                                None => caption,
                            };
                            blocks.push((
                                BlockKind::Caption,
                                section_path(&section_stack),
                                current_label.clone(),
                                caption,
                            ));
                            caption_emitted = true;
                        }
                        sink = Some(Sink::Wrap);
                    }
                    b"xref" if wrapping_bibr_xref => {
                        if sink.is_some() {
                            text.push(']');
                        }
                        wrapping_bibr_xref = false;
                    }
                    b"table-wrap" | b"fig"
                        if sink == Some(Sink::Wrap) || sink == Some(Sink::Caption) =>
                    {
                        let content = take(&mut text);
                        let is_fig = wrap_element == Some("fig");
                        // When no caption carried the label into the text,
                        // the wrap's own block does: a "Table 1" line above
                        // the rows is what makes the table findable at the
                        // table, not only in prose mentions of it.
                        let content = match current_label.as_deref() {
                            Some(label) if !caption_emitted && !content.is_empty() => {
                                format!("{label}\n{content}")
                            }
                            Some(label) if !caption_emitted => label.to_string(),
                            _ => content,
                        };
                        if !content.is_empty() {
                            blocks.push((
                                if is_fig {
                                    BlockKind::Caption
                                } else {
                                    BlockKind::Table
                                },
                                section_path(&section_stack),
                                current_label.clone(),
                                content,
                            ));
                        }
                        sink = None;
                        wrap_element = None;
                        current_label = None;
                        caption_emitted = false;
                    }
                    _ => {}
                }
                None
            }
            Event::Eof => break,
            _ => None,
        };
        if let Some(chunk) = chunk
            && let Some(kind) = sink
            // <object-id> text is a publisher-internal identifier
            // ("materials-19-02487-t0A1_Table A1"), not document text. For a
            // CAPTIONED wrap the caption arm's text.clear() happens to wipe
            // it; for an uncaptioned wrap nothing does, and the junk would
            // lead the table's evidence text. Excluded at the source so
            // neither shape depends on that accident.
            && !in_object_id
        {
            let trimmed = chunk.trim().to_string();
            if trimmed.is_empty() {
                // nothing to append
            } else if kind == Sink::Wrap && text.ends_with('\u{1}') {
                text.pop();
                current_label = Some(trimmed);
            } else {
                // No joiner space at a row start: a '\n' already separates,
                // and the space it used to add put every table row behind a
                // leading blank (" 200 | 800").
                if !text.is_empty()
                    && !text.ends_with(' ')
                    && !text.ends_with('[')
                    && !text.ends_with('\n')
                {
                    text.push(' ');
                }
                text.push_str(&trimmed);
            }
        }
        buf.clear();
    }

    if blocks.is_empty() {
        anyhow::bail!("no recognizable JATS structure found (not a JATS document?)");
    }

    // Assemble blocks with character offsets into the concatenated text.
    let mut plain = String::new();
    let mut located: Vec<TextBlock> = Vec::new();
    for (kind, section_path, label, block_text) in blocks {
        if block_text.trim().is_empty() {
            continue;
        }
        if !plain.is_empty() {
            plain.push('\n');
        }
        let offset = plain.chars().count();
        plain.push_str(&block_text);
        located.push(TextBlock {
            locator: Locator {
                kind,
                section_path,
                label,
                char_offset: offset,
            },
            text: block_text,
        });
    }
    Ok(Fulltext {
        source_url: String::new(),
        format: FulltextFormat::Jats,
        blocks: located,
        plain_text: plain,
    })
}

fn take(text: &mut String) -> String {
    let out = text.trim().to_string();
    text.clear();
    out
}

#[cfg(test)]
mod tests {
    /// A third-party parser panic must not end the run.
    ///
    /// Measured 2026-08-20: `pdf_extract` panicked with "missing unicode map and
    /// encoding" on one publisher's PDF and killed a `papers corpus` harvest
    /// that had already written 32 full texts. `parse_pdf` returns `Result`, so
    /// every caller was ready to skip a bad document — but a panic unwinds past
    /// all of them.
    ///
    /// This tests the CONTAINMENT, not the parser. An earlier version fed
    /// garbage bytes to `parse_pdf` and passed even with the containment
    /// removed, because those bytes made `pdf_extract` return `Err` rather than
    /// panic — a test that proved the wrong thing.
    #[test]
    fn a_parser_panic_becomes_an_error_and_the_process_survives() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // keep the test output readable
        let outcome = super::contain_parser_panic(|| -> &str { panic!("missing unicode map") });
        std::panic::set_hook(previous);

        let error = outcome.expect_err("a panicking parser must yield an Err");
        let message = format!("{error:#}");
        assert!(message.contains("panicked on this document"), "{message}");
        assert!(
            message.contains("missing unicode map"),
            "the real cause must survive into the error: {message}"
        );

        // Still running, which is the whole point.
        assert!(super::contain_parser_panic(|| 7).is_ok());
    }

    use super::*;

    const JATS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group>
        <article-title>Thermal conductivity of high entropy alloys</article-title>
      </title-group>
      <abstract><p>We report measurements on five alloys.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Introduction</title>
      <p>High entropy alloys are interesting.</p>
      <sec>
        <title>1.1 Motivation</title>
        <p>Low thermal conductivity is desired.</p>
      </sec>
    </sec>
    <sec>
      <title>2. Results</title>
      <table-wrap>
        <label>Table 1</label>
        <caption><p>Measured conductivity at 300 K.</p></caption>
        <table><tr><td>Alloy</td><td>W/m-K</td></tr><tr><td>CoCrFeNi</td><td>11.5</td></tr></table>
      </table-wrap>
    </sec>
  </body>
  <back><ref-list><ref><citation>Old citation text.</citation></ref></ref-list></back>
</article>"#;

    #[test]
    fn jats_blocks_carry_section_paths_and_labels() {
        let ft = parse_jats(JATS_FIXTURE.as_bytes()).unwrap();
        assert_eq!(ft.format, FulltextFormat::Jats);

        let kinds: Vec<BlockKind> = ft.blocks.iter().map(|b| b.locator.kind).collect();
        assert!(kinds.contains(&BlockKind::Title));
        assert!(kinds.contains(&BlockKind::Abstract));
        assert!(kinds.contains(&BlockKind::Body));
        assert!(kinds.contains(&BlockKind::Table));
        assert!(kinds.contains(&BlockKind::Caption));

        let intro = ft
            .blocks
            .iter()
            .find(|b| b.text == "High entropy alloys are interesting.")
            .unwrap();
        assert_eq!(intro.locator.section_path, vec!["1. Introduction"]);

        let nested = ft
            .blocks
            .iter()
            .find(|b| b.text == "Low thermal conductivity is desired.")
            .unwrap();
        assert_eq!(
            nested.locator.section_path,
            vec!["1. Introduction", "1.1 Motivation"]
        );

        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(table.locator.label.as_deref(), Some("Table 1"));
        assert!(table.text.contains("CoCrFeNi"));

        let caption = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Caption)
            .unwrap();
        assert_eq!(caption.locator.label.as_deref(), Some("Table 1"));
        // The label is IN the visible text — "Table 1. …" is the string a
        // reader searches for; a locator-only label left the table
        // unfindable by name.
        assert_eq!(caption.text, "Table 1. Measured conductivity at 300 K.");

        // Offsets point into plain_text faithfully.
        for block in &ft.blocks {
            let offset = block.locator.char_offset;
            let snippet: String = ft
                .plain_text
                .chars()
                .skip(offset)
                .take(block.text.chars().count())
                .collect();
            assert_eq!(
                snippet, block.text,
                "offset mismatch in block {:?}",
                block.locator.kind
            );
        }
        // Back-matter (references) is excluded.
        assert!(!ft.plain_text.contains("Old citation text"));
    }

    /// A self-closing <table-wrap/> must not wedge the sink: the parser
    /// used to open the wrap state on it and never receive the End event,
    /// silently truncating every block after it.
    #[test]
    fn self_closing_table_wrap_does_not_wedge_the_sink() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Wedge probe</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Before the wedge.</p>
      <table-wrap/>
      <fig/>
      <p>After the wedge.</p>
    </sec>
  </body>
</article>"#;
        let ft = parse_jats(body.as_bytes()).unwrap();
        assert!(
            ft.blocks.iter().any(|b| b.text == "Before the wedge."),
            "blocks: {:?}",
            ft.blocks.iter().map(|b| &b.text).collect::<Vec<_>>()
        );
        assert!(
            ft.blocks.iter().any(|b| b.text == "After the wedge."),
            "a self-closing wrap wedged the sink; blocks: {:?}",
            ft.blocks.iter().map(|b| &b.text).collect::<Vec<_>>()
        );
    }

    /// F2 regression (fabrication path 1): the JATS sink used to join
    /// every chunk of a table with a space, so the whole table was ONE
    /// evidence span: a number from one row could "support" a claim whose
    /// subject appeared in a different row. Rows must stay separate lines
    /// so `supporting_spans` splits the table into rows.
    #[test]
    fn jats_table_rows_stay_separate_lines() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Row structure probe</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <table-wrap>
        <label>Table 1</label>
        <caption><p>Mechanical properties.</p></caption>
        <table>
          <tr><th>Alloy</th><th>UTS (MPa)</th></tr>
          <tr><td>Ti-6Al-4V</td><td>950</td></tr>
          <tr><td>Inconel 718</td><td>1375</td></tr>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = parse_jats(body.as_bytes()).unwrap();
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(table.locator.label.as_deref(), Some("Table 1"));
        let rows: Vec<&str> = table.text.lines().map(str::trim).collect();
        // Exact row structure: three separate rows, so the two alloys'
        // numbers (950 / 1375) never share one — and cells keep their
        // boundaries, so "Inconel 718 | 1375" splits back into alloy and
        // value instead of fusing into an unparseable "Inconel 718 1375".
        // This equality is the assertion; a weaker `.any()` after it could
        // never fail first.
        assert_eq!(
            rows,
            vec!["Alloy | UTS (MPa)", "Ti-6Al-4V | 950", "Inconel 718 | 1375"]
        );
    }

    /// A self-closing `<td/>` is an EMPTY CELL, not nothing: it must keep
    /// its `|` or every later cell in the row shifts left one column and
    /// binds to the wrong header. Under the old space-join the porosity
    /// column below would silently vanish and 950 would read as porosity.
    #[test]
    fn empty_cells_hold_their_column_position() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Empty cell probe</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <table-wrap>
        <label>Table 1</label>
        <table>
          <tr><th>Alloy</th><th>Porosity (%)</th><th>UTS (MPa)</th></tr>
          <tr><td>Ti-6Al-4V</td><td/><td>950</td></tr>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = parse_jats(body.as_bytes()).unwrap();
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        let rows: Vec<&str> = table.text.lines().map(str::trim).collect();
        assert_eq!(
            rows,
            vec![
                "Table 1",
                "Alloy | Porosity (%) | UTS (MPa)",
                "Ti-6Al-4V | | 950"
            ]
        );
    }

    /// Measured on PMC13302085 (MDPI *Materials*): EVERY `<table-wrap>` and
    /// EVERY `<fig>` lived in `<floats-group>` or `<back>/<app-group>`, not
    /// in `<body>` — a body-only parser dropped all of them silently. Wraps
    /// from both zones must reach the block stream; `<ref-list>` text must
    /// not.
    #[test]
    fn floats_group_and_appendix_wraps_are_captured() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Floats probe</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Process parameters are listed in Table 1.</p>
    </sec>
  </body>
  <back>
    <app-group>
      <app>
        <title>Appendix A</title>
        <p>HD denotes hatch distance.</p>
        <table-wrap>
          <object-id pub-id-type="pii">materials-00-00000-t0A1_Table A1</object-id>
          <label>Table A1</label>
          <table>
            <tr><th>Ref</th><th>Power (W)</th></tr>
            <tr><td>Smith 2020</td><td>400</td></tr>
          </table>
        </table-wrap>
      </app>
    </app-group>
    <ref-list><ref><mixed-citation>Old citation text.</mixed-citation></ref></ref-list>
  </back>
  <floats-group>
    <table-wrap>
      <label>Table 1</label>
      <caption><p>Process parameters.</p></caption>
      <table>
        <tr><th>Power (W)</th><th>Speed (mm/s)</th></tr>
        <tr><td>200</td><td>800</td></tr>
      </table>
    </table-wrap>
    <fig>
      <label>Figure 1</label>
      <caption><p>Melt pool geometry.</p></caption>
      <graphic xlink:href="fig1.jpg"/>
    </fig>
  </floats-group>
</article>"#;
        let ft = parse_jats(body.as_bytes()).unwrap();
        let table_1 = ft
            .blocks
            .iter()
            .find(|b| {
                b.locator.kind == BlockKind::Table && b.locator.label.as_deref() == Some("Table 1")
            })
            .expect("the floats-group table must reach the block stream");
        assert_eq!(
            table_1.text.lines().collect::<Vec<_>>(),
            vec!["Power (W) | Speed (mm/s)", "200 | 800"]
        );
        assert!(
            ft.plain_text.contains("Table 1. Process parameters."),
            "the floats-group table's caption must carry its label: {}",
            ft.plain_text
        );
        assert!(
            ft.plain_text.contains("Figure 1. Melt pool geometry."),
            "the floats-group figure caption must reach the text: {}",
            ft.plain_text
        );

        let table_a1 = ft
            .blocks
            .iter()
            .find(|b| {
                b.locator.kind == BlockKind::Table && b.locator.label.as_deref() == Some("Table A1")
            })
            .expect("the appendix table must reach the block stream");
        // Uncaptioned on purpose: this is the shape where <object-id> junk
        // would lead the evidence text (a caption's text.clear() is what
        // wipes it for captioned wraps), and where the label must ride the
        // table block itself.
        assert_eq!(
            table_a1.text.lines().collect::<Vec<_>>(),
            vec!["Table A1", "Ref | Power (W)", "Smith 2020 | 400"]
        );
        assert!(
            !table_a1.text.contains("materials-00-00000"),
            "object-id junk must not enter the evidence text: {}",
            table_a1.text
        );
        assert!(
            ft.plain_text.contains("HD denotes hatch distance."),
            "appendix prose must reach the text: {}",
            ft.plain_text
        );
        // Bibliography text is still not document content.
        assert!(!ft.plain_text.contains("Old citation text"));
    }

    /// H6: the text of a <xref ref-type="bibr"> is a citation marker by
    /// construction; superscript numbering parses it to a bare number the
    /// claims guard cannot tell from a measurement. The parser wraps it in
    /// [...] so the existing bracketed-citation guard refuses it. Non-bibr
    /// xrefs pass through untouched: the wrap must not reach beyond
    /// bibliographic references.
    #[test]
    fn bibr_xref_text_is_wrapped_as_citation_marker() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>Citation probe</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <p>Ti-6Al-4V has been widely studied<sup><xref ref-type="bibr" rid="b1">1140</xref></sup>.</p>
      <p>Properties are shown in <xref ref-type="fig">Fig. 2</xref>.</p>
    </sec>
  </body>
</article>"#;
        let ft = parse_jats(body.as_bytes()).unwrap();
        let texts: Vec<&str> = ft.blocks.iter().map(|b| b.text.as_str()).collect();
        assert!(
            texts.contains(&"Ti-6Al-4V has been widely studied [1140] ."),
            "bibr xref not wrapped as a citation marker: {texts:?}"
        );
        assert!(
            texts.contains(&"Properties are shown in Fig. 2 ."),
            "a non-bibr xref must pass through untouched: {texts:?}"
        );
    }

    /// H1: JATS permits TWO table models. The OASIS/CALS model uses
    /// <tgroup>/<row>/<entry> instead of <tr>/<td>, and half the corpus
    /// uses it. A <row> must delimit rows exactly like <tr>, or the whole
    /// OASIS table fuses into one span and a number from one row
    /// "supports" a claim whose subject lives in another row.
    #[test]
    fn oasis_table_rows_stay_separate_lines() {
        let body = r#"<?xml version="1.0"?>
<article xmlns:xlink="http://www.w3.org/1999/xlink">
  <front>
    <article-meta>
      <title-group><article-title>OASIS row probe</article-title></title-group>
      <abstract><p>Abstract text.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>1. Section</title>
      <table-wrap>
        <label>Table 1</label>
        <table>
          <tgroup cols="2">
            <tbody>
              <row><entry>Ti-6Al-4V</entry><entry>950</entry></row>
              <row><entry>Inconel 718</entry><entry>1375</entry></row>
            </tbody>
          </tgroup>
        </table>
      </table-wrap>
    </sec>
  </body>
</article>"#;
        let ft = parse_jats(body.as_bytes()).unwrap();
        let table = ft
            .blocks
            .iter()
            .find(|b| b.locator.kind == BlockKind::Table)
            .unwrap();
        assert_eq!(table.locator.label.as_deref(), Some("Table 1"));
        let rows: Vec<&str> = table.text.lines().map(str::trim).collect();
        // Exact row structure: two separate rows, so the two alloys'
        // numbers (950 / 1375) never share one. The wrap has a label but no
        // caption, so the label rides the table block itself as its first
        // line — otherwise "Table 1" would exist only in the locator and the
        // table would be unfindable by name.
        assert_eq!(
            rows,
            vec!["Table 1", "Ti-6Al-4V | 950", "Inconel 718 | 1375"]
        );
    }

    #[test]
    fn garbage_is_refused_not_fabricated() {
        assert!(parse_jats(b"this is not xml").is_err());
        assert!(parse_pdf(b"%PDF-1.4 garbage not really a pdf").is_err());
    }

    #[test]
    fn sniff_detects_pdf_magic() {
        assert_eq!(sniff(b"%PDF-1.7 ..."), FulltextFormat::Pdf);
        assert_eq!(sniff(b"<?xml version=\"1.0\"?>"), FulltextFormat::Jats);
    }

    #[test]
    fn oa_listing_yields_tgz_href() {
        let body = br#"<OA><records returned-count="1"><record id="PMC1">
            <link format="tgz" href="ftp://ftp.ncbi.nlm.nih.gov/pub/pmc/oa_package/x/y/PMC1.tar.gz" />
        </record></records></OA>"#;
        let href = parse_oa_listing(body).unwrap().unwrap();
        assert!(href.ends_with("PMC1.tar.gz"));
    }

    #[test]
    fn oa_listing_without_record_is_none() {
        let body = br#"<OA><error>id not found</error></OA>"#;
        assert!(parse_oa_listing(body).unwrap().is_none());
    }

    #[test]
    fn extract_nxml_finds_the_jats_member() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let nxml = b"<article><body></body></article>";
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(nxml.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "PMC1/main.nxml", &nxml[..])
            .unwrap();
        let tarball = builder.into_inner().unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tarball).unwrap();
        let package = encoder.finish().unwrap();
        assert_eq!(extract_nxml(&package).unwrap(), nxml);
    }

    #[test]
    fn extract_nxml_refuses_packages_without_nxml() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut builder = tar::Builder::new(Vec::new());
        let data = b"not jats";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "PMC1/readme.txt", &data[..])
            .unwrap();
        let tarball = builder.into_inner().unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tarball).unwrap();
        let package = encoder.finish().unwrap();
        assert!(extract_nxml(&package).is_err());
    }
}
