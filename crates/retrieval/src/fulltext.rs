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
pub fn parse_pdf(body: &[u8]) -> Result<Fulltext> {
    let text = pdf_extract::extract_text_from_mem(body)
        .map_err(|e| anyhow::anyhow!("pdf text extraction failed: {e}"))?;
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
    let mut depth_sec: usize = 0;
    // The most recent <sec> is waiting for its <title>.
    let mut awaiting_sec_title = false;

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
                match e.local_name().as_ref() {
                    b"front" => depth_front += 1,
                    b"body" => depth_body += 1,
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
                    b"table-wrap" if depth_body > 0 && sink.is_none() => {
                        sink = Some(Sink::Wrap);
                        wrap_element = Some("table-wrap");
                        text.clear();
                        current_label = None;
                    }
                    b"fig" if depth_body > 0 && sink.is_none() => {
                        sink = Some(Sink::Wrap);
                        wrap_element = Some("fig");
                        text.clear();
                        current_label = None;
                    }
                    b"label" if sink == Some(Sink::Wrap) && current_label.is_none() => {
                        // Sentinel: the next text chunk is this wrap's label.
                        text.push('\u{1}');
                    }
                    b"caption" if sink == Some(Sink::Wrap) => {
                        sink = Some(Sink::Caption);
                        text.clear();
                    }
                    b"p" if depth_body > 0 && sink.is_none() => {
                        sink = Some(Sink::Paragraph);
                        text.clear();
                    }
                    _ => {}
                }
                None
            }
            // A self-closing tag carries no content and never gets an End
            // event: it must not open a sink or a depth, or the parser
            // wedges on it and silently drops the rest of the document
            // (e.g. a bare <table-wrap/>).
            Event::Empty(_) => None,
            Event::End(e) => {
                match e.local_name().as_ref() {
                    b"front" => depth_front -= 1,
                    b"body" => depth_body -= 1,
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
                            blocks.push((
                                BlockKind::Caption,
                                section_path(&section_stack),
                                current_label.clone(),
                                caption,
                            ));
                        }
                        sink = Some(Sink::Wrap);
                    }
                    b"table-wrap" | b"fig"
                        if sink == Some(Sink::Wrap) || sink == Some(Sink::Caption) =>
                    {
                        let content = take(&mut text);
                        let is_fig = wrap_element == Some("fig");
                        if !content.is_empty() || current_label.is_some() {
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
        {
            let trimmed = chunk.trim().to_string();
            if trimmed.is_empty() {
                // nothing to append
            } else if kind == Sink::Wrap && text.ends_with('\u{1}') {
                text.pop();
                current_label = Some(trimmed);
            } else {
                if !text.is_empty() && !text.ends_with(' ') {
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
        assert_eq!(caption.text, "Measured conductivity at 300 K.");

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
