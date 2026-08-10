//! Reading a document's own embedded text layer.
//!
//! The cheap path, and the default first step of [`Policy::Escalate`]: no
//! model, no network, no GPU, no rasteriser — bytes in, text out. It is right
//! for the majority of born-digital papers and wrong in two specific ways that
//! [`crate::document::quality`] detects and the escalation policy answers.
//!
//! [`Policy::Escalate`]: crate::document::Policy::Escalate

use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{DocumentUnderstanding, Modality, PageText, Readiness, SourceDocument, Understanding};

/// The built-in text-layer reader, backed by `pdf-extract`.
pub struct TextLayerUnderstanding;

#[async_trait]
impl DocumentUnderstanding for TextLayerUnderstanding {
    fn id(&self) -> &'static str {
        "text-layer"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn modality(&self) -> Modality {
        Modality::TextLayer
    }

    /// Always ready: it is pure computation over the bytes it is given, with
    /// nothing to provision.
    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    async fn understand(&self, doc: &SourceDocument<'_>) -> Result<Understanding> {
        // Per PAGE, not per document. Damage is a per-page property — one
        // broken figure page in a sound paper should escalate alone — and
        // page numbers must be real, so they come from the extractor rather
        // than being invented after a whole-document split.
        //
        // On a blocking thread, and the panic is caught there: `pdf-extract`
        // aborts on some malformed files, and one bad PDF in a directory
        // ingest must be one reported failure, not a dead run.
        let bytes = doc.bytes.to_vec();
        let label = doc.label.to_string();
        let pages = tokio::task::spawn_blocking(move || {
            pdf_extract::extract_text_from_mem_by_pages(&bytes)
        })
        .await
        .map_err(|join| {
            if join.is_panic() {
                anyhow::anyhow!(
                    "the text-layer reader panicked on {label}; the file is likely \
                     malformed or unsupported"
                )
            } else {
                anyhow::anyhow!(join)
            }
        })?
        .with_context(|| format!("extracting the text layer of {}", doc.label))?;

        Ok(Understanding {
            adapter_id: self.id().into(),
            modality: self.modality(),
            pages: pages
                .into_iter()
                .enumerate()
                // 1-based, matching how the document numbers itself.
                .map(|(i, text)| PageText {
                    number: (i + 1) as u32,
                    text,
                })
                .filter(|page| doc.wants_page(page.number))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_itself_as_the_cheap_text_reader() {
        let adapter = TextLayerUnderstanding;
        assert_eq!(adapter.id(), "text-layer");
        assert_eq!(adapter.media_types(), ["pdf"]);
        assert_eq!(adapter.modality(), Modality::TextLayer);
        assert!(adapter.readiness().is_ready());
    }

    /// Malformed bytes must come back as a described error, not a panic and
    /// not an empty success that downstream reads as "this document is blank".
    #[tokio::test]
    async fn malformed_bytes_are_an_error_naming_the_document() {
        let adapter = TextLayerUnderstanding;
        let err = adapter
            .understand(&SourceDocument::whole(
                b"not a pdf at all",
                "pdf",
                "broken.pdf",
            ))
            .await
            .expect_err("malformed bytes must not succeed");
        assert!(format!("{err:#}").contains("broken.pdf"), "{err:#}");
    }
}
