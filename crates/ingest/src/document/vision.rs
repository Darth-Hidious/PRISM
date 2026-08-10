//! Reading a page by looking at it.
//!
//! Renders the page to an image and asks a vision model what it says. This is
//! the path for everything the text layer cannot give you: a scanned page, a
//! table whose font encoding is broken, an axis label that only exists inside
//! a plotted figure.
//!
//! # Two measured facts that shape this module
//!
//! **One image costs a fixed token budget, whatever its size.** Measured on
//! Gemma 4 12B through llama.cpp: an 880×680 page cost 254 image tokens and a
//! 1650×1275 render of the SAME page cost 268 — the encoder downsamples to a
//! fixed grid, so a whole page of body text arrives as roughly a 16×16 patch
//! grid and is not legible. Handing the model a full page produced a correct
//! title (the only text large enough to survive) and invented everything else,
//! including alloys that do not appear in the document. So pages are read in
//! [`TILES`]: each tile gets its own budget, which is what actually buys
//! resolution. The same page read as quadrants transcribed author names,
//! affiliations and a process-parameter table correctly.
//!
//! **Small vision models loop on repetitive imagery.** A tile of micrographs
//! each captioned `10 mm` produced `10 mm` to the token limit; a tile of table
//! rules produced `| | | |`. A repetition penalty does not fix it — it changes
//! which text repeats. So every tile is checked with
//! [`is_degenerate`](super::is_degenerate) and a looping tile is DISCARDED. A
//! loop fed to the fact extractor would mint hundreds of identical assertions,
//! which is worse than the missing page it replaces.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use super::{
    DamagePolicy, DocumentUnderstanding, Modality, PageText, Readiness, SourceDocument,
    Understanding, is_degenerate,
};

/// How a page becomes an image.
///
/// Its own adapter surface because rasterisers are as plural as readers —
/// poppler, pdfium, mupdf, a rendering service — and they differ in licence
/// as much as in capability, which decides what a given build may even link.
/// The vision reader depends on this contract, never on a particular one.
pub trait PageRasteriser: Send + Sync {
    /// Stable machine id, e.g. `"poppler"`.
    fn id(&self) -> &'static str;

    /// Whether this rasteriser can run here, with an actionable reason if not.
    fn readiness(&self) -> Readiness;

    /// Render one 1-based page to PNG bytes at `dpi`.
    fn render(&self, pdf: &[u8], page: u32, dpi: u32) -> Result<Vec<u8>>;
}

/// Render resolution. High enough that a tile of body text survives the
/// encoder's fixed grid; the tiling below is what turns those pixels into
/// legibility.
const RENDER_DPI: u32 = 200;

/// Tiles per page, as (columns, rows). Two-by-two was the coarsest split that
/// read a dense conference poster correctly; each tile is one model call, so
/// this is also the per-page cost multiplier.
const TILES: (u32, u32) = (2, 2);

/// Fraction of a tile's width and height that overlaps its neighbour, so a
/// line of text falling on a tile boundary is whole in at least one tile.
const TILE_OVERLAP: f64 = 0.08;

/// Output bound for ONE tile, in tokens.
///
/// Not a policy cap on the model — the bound of the thing being asked about.
/// A quarter of a printed page holds on the order of 400 words even when
/// densely set, so ~2000 tokens is several times more than any honest
/// transcription of one tile needs. Without it a looping model generates
/// until the context is exhausted: a single micrograph tile ran for over five
/// minutes and timed out before its output could be judged degenerate at all.
/// With it, a loop costs one bounded call and is then discarded.
const TILE_TOKEN_BOUND: u64 = 2_048;

/// What the model is asked for. Transcription, deliberately — NOT extraction.
///
/// The fact extractor already exists, is ontology-driven, and is the one
/// place that decides what a fact is. A vision model asked to "find the
/// properties" would be a second, unontologised extractor whose output
/// nothing validates. Its job here is narrow: turn pixels into the text that
/// is printed, and let the existing pipeline do the rest.
const TRANSCRIBE_PROMPT: &str = "\
Transcribe the visible text in this image exactly as printed, in reading order. \
Include table cells, axis labels and figure captions. Do not describe, explain, \
summarise, or add anything that is not printed. If a region is unreadable, skip \
it rather than guessing. Stop when you have transcribed everything visible.";

/// A vision model reading rendered pages.
pub struct VisionUnderstanding {
    rasteriser: Arc<dyn PageRasteriser>,
    llm: Arc<prism_llm::LlmClient>,
    damage: DamagePolicy,
}

impl VisionUnderstanding {
    pub fn new(rasteriser: Arc<dyn PageRasteriser>, llm: Arc<prism_llm::LlmClient>) -> Self {
        Self {
            rasteriser,
            llm,
            damage: DamagePolicy::default(),
        }
    }

    /// Override the thresholds used to discard a looping tile.
    pub fn with_damage_policy(mut self, damage: DamagePolicy) -> Self {
        self.damage = damage;
        self
    }

    /// Read one page as overlapping tiles, dropping any tile that loops.
    async fn read_page(&self, pdf: &[u8], page: u32) -> Result<String> {
        let png = self
            .rasteriser
            .render(pdf, page, RENDER_DPI)
            .with_context(|| format!("rendering page {page}"))?;

        let (cols, rows) = TILES;
        let mut parts: Vec<String> = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let tile = crop_tile(&png, col, row, cols, rows, TILE_OVERLAP)
                    .with_context(|| format!("cropping page {page} tile r{row}c{col}"))?;
                let text = self
                    .llm
                    .describe_image(TRANSCRIBE_PROMPT, &tile, TILE_TOKEN_BOUND)
                    .await
                    .with_context(|| format!("reading page {page} tile r{row}c{col}"))?;
                if is_degenerate(&text, &self.damage) {
                    // Reported, not silently dropped: a page that came back
                    // short because a model looped is a different fact from a
                    // page that was genuinely near-empty.
                    tracing::warn!(
                        page,
                        tile = format!("r{row}c{col}"),
                        "vision output was a repetition loop; tile discarded"
                    );
                    continue;
                }
                parts.push(text.trim().to_string());
            }
        }
        Ok(parts.join("\n"))
    }
}

#[async_trait]
impl DocumentUnderstanding for VisionUnderstanding {
    fn id(&self) -> &'static str {
        "vision"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn modality(&self) -> Modality {
        Modality::Vision
    }

    fn readiness(&self) -> Readiness {
        self.rasteriser.readiness()
    }

    async fn understand(&self, doc: &SourceDocument<'_>) -> Result<Understanding> {
        let Some(wanted) = doc.pages else {
            // Refusing is the honest answer: reading every page of a paper
            // through a vision model is a large, billable cost, and this
            // adapter has no way to know the page count without a rasteriser
            // round-trip. Escalation always names the pages it needs.
            bail!(
                "the vision reader needs an explicit page list; reading a whole document \
                 by sight is a deliberate request, not a default"
            );
        };

        let mut pages = Vec::new();
        for &number in wanted {
            let text = self.read_page(doc.bytes, number).await?;
            pages.push(PageText { number, text });
        }
        Ok(Understanding {
            adapter_id: self.id().into(),
            modality: self.modality(),
            pages,
        })
    }
}

/// Crop one overlapping tile out of a rendered page.
fn crop_tile(
    png: &[u8],
    col: u32,
    row: u32,
    cols: u32,
    rows: u32,
    overlap: f64,
) -> Result<Vec<u8>> {
    use image::GenericImageView as _;
    let img = image::load_from_memory(png).context("decoding the rendered page")?;
    let (w, h) = img.dimensions();
    let (tw, th) = (w as f64 / cols as f64, h as f64 / rows as f64);
    // Grow each tile toward its neighbours, clamped at the page edges.
    let x0 = ((col as f64 * tw) - tw * overlap).max(0.0) as u32;
    let y0 = ((row as f64 * th) - th * overlap).max(0.0) as u32;
    let x1 = (((col + 1) as f64 * tw) + tw * overlap).min(w as f64) as u32;
    let y1 = (((row + 1) as f64 * th) + th * overlap).min(h as f64) as u32;
    let tile = img.crop_imm(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0));
    let mut out = Vec::new();
    tile.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .context("encoding the tile")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// Tiles must OVERLAP and must together cover the whole page — a line of
    /// text on a tile boundary has to be whole somewhere, and no strip of the
    /// page may go unread.
    #[test]
    fn tiles_overlap_and_cover_the_whole_page() {
        use image::GenericImageView as _;
        let png = page_png(1000, 800);
        let (cols, rows) = (2u32, 2u32);
        let mut total = 0u64;
        for row in 0..rows {
            for col in 0..cols {
                let tile = crop_tile(&png, col, row, cols, rows, 0.08).expect("crop");
                let (tw, th) = image::load_from_memory(&tile).unwrap().dimensions();
                // Each tile is BIGGER than an exact quarter — that is the overlap.
                assert!(
                    tw > 1000 / cols,
                    "tile width {tw} must exceed a bare quarter"
                );
                assert!(
                    th > 800 / rows,
                    "tile height {th} must exceed a bare quarter"
                );
                total += u64::from(tw) * u64::from(th);
            }
        }
        // Overlapping tiles cover strictly more area than the page itself,
        // which is what guarantees no unread seam.
        assert!(total > 1000 * 800, "tiles must more than cover the page");
    }

    /// A single-tile split is still a valid crop covering everything — the
    /// geometry must not depend on there being more than one tile.
    #[test]
    fn a_single_tile_is_the_whole_page() {
        use image::GenericImageView as _;
        let png = page_png(640, 480);
        let tile = crop_tile(&png, 0, 0, 1, 1, 0.0).expect("crop");
        assert_eq!(
            image::load_from_memory(&tile).unwrap().dimensions(),
            (640, 480),
        );
    }

    /// Reads a real page with a real vision model, end to end: rasteriser →
    /// tiles → model → assembled text. Ignored by default because it needs
    /// both poppler and a vision-capable endpoint; this is the only check
    /// that the whole chain works rather than each link separately.
    ///
    /// ```text
    /// PRISM_TEST_PDF=paper.pdf PRISM_TEST_VISION_URL=http://127.0.0.1:8081/v1 \
    ///   PRISM_TEST_VISION_MODEL=gemma \
    ///   cargo test -p prism-ingest vision::tests::reads -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires poppler and a vision-capable LLM endpoint"]
    async fn reads_a_real_page_with_a_real_model() {
        let pdf = std::fs::read(std::env::var("PRISM_TEST_PDF").expect("PRISM_TEST_PDF"))
            .expect("read the pdf");
        let llm = prism_llm::LlmClient::new(prism_llm::LlmConfig {
            base_url: std::env::var("PRISM_TEST_VISION_URL").expect("PRISM_TEST_VISION_URL"),
            model: std::env::var("PRISM_TEST_VISION_MODEL").expect("PRISM_TEST_VISION_MODEL"),
            ..Default::default()
        });
        let reader = VisionUnderstanding::new(
            Arc::new(super::super::CommandRasteriser::poppler()),
            Arc::new(llm),
        );
        assert!(reader.readiness().is_ready(), "poppler must be installed");

        let understanding = reader
            .understand(&SourceDocument {
                bytes: &pdf,
                media_type: "pdf",
                label: "test.pdf",
                pages: Some(&[1]),
            })
            .await
            .expect("vision read");

        let text = understanding.plain_text();
        println!("--- vision read {} chars ---\n{text}", text.len());
        assert_eq!(understanding.modality, Modality::Vision);
        assert!(
            text.len() > 200,
            "a real page must yield real text, got {} chars",
            text.len(),
        );
        assert!(
            !is_degenerate(&text, &DamagePolicy::default()),
            "assembled output must not be a repetition loop",
        );
    }

    /// The prompt asks for transcription and forbids invention. This is the
    /// guard against the vision reader quietly becoming a second, unvalidated
    /// fact extractor that bypasses the ontology.
    #[test]
    fn the_prompt_asks_only_for_what_is_printed() {
        let p = TRANSCRIBE_PROMPT;
        assert!(p.contains("exactly as printed"));
        assert!(p.contains("skip"), "unreadable regions must be skipped");
        for forbidden in ["describe", "explain", "summarise"] {
            assert!(
                p.contains(forbidden),
                "the prompt must explicitly forbid '{forbidden}'",
            );
        }
    }
}
