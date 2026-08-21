//! The document-understanding surface.
//!
//! A way of turning document BYTES into TEXT is a plugin implementing
//! [`DocumentUnderstanding`]: a stable id, the media types it handles, the
//! [`Modality`] it reads with, and one method that produces [`PageText`] per
//! page. The ingest path asks "how do I read this document?" only through
//! [`UnderstandingRegistry`] dispatch, so adding MinerU, a cloud VLM, or a
//! commercial OCR service means writing one module and ONE registration call
//! — no match arm and no consumer edit.
//!
//! **This plane's claims are deliberately NON-exclusive, and that is the one
//! way it differs from every other adapter plane in PRISM.** A file connector
//! owns `.csv` outright: two connectors claiming it is an accident, so
//! [`crate::connectors::ConnectorRegistry`] refuses the second. Here, several
//! adapters claiming `pdf` is the entire point — the text layer and a vision
//! model are two ways to read the SAME bytes, and choosing between them is a
//! policy decision (see [`Policy`]), not a routing collision. Ids stay
//! exclusive and keep the plane's shared two-call contract: `register`
//! refuses a taken id, `replace` refuses a free one.
//!
//! The modality is carried on the result, not discarded. Text copied out of a
//! PDF's text layer and text a vision model read off a rendered chart are not
//! the same kind of evidence, and provenance has to be able to say which one
//! a fact came from.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock, RwLockReadGuard};

use anyhow::{Result, bail};
use async_trait::async_trait;

/// How an adapter recovered the text. Recorded on every [`Understanding`] and
/// carried into provenance: a value a vision model read off a plotted axis is
/// weaker evidence than one copied from an intact text layer, and a store that
/// cannot tell them apart cannot be audited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Modality {
    /// Characters copied from the document's own embedded text layer.
    TextLayer,
    /// Characters a vision model read from a rendered page image.
    Vision,
}

impl Modality {
    pub fn as_str(self) -> &'static str {
        match self {
            Modality::TextLayer => "text-layer",
            Modality::Vision => "vision",
        }
    }
}

/// One page's recovered text. Pages are kept separate rather than concatenated
/// so damage can be assessed per page and a single bad page can be re-read with
/// a different adapter without re-reading the document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PageText {
    /// 1-based page number, as printed in the document's page order.
    pub number: u32,
    pub text: String,
}

/// What an adapter produced, and how.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Understanding {
    /// The adapter that produced this — the registry id, so a stored fact can
    /// name the exact reader it came from.
    pub adapter_id: String,
    pub modality: Modality,
    pub pages: Vec<PageText>,
}

impl Understanding {
    /// All pages joined in order. The form the extraction prompt consumes.
    pub fn plain_text(&self) -> String {
        self.pages
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The SAME joined string as [`plain_text`], plus each page's byte range
    /// in it. The ranges TILE the string exactly — each page's range carries
    /// the joiner that follows it — so structure-aware segmentation
    /// (`prism_ingest::batching::chunk_structured`) can pack pages as whole
    /// units without any byte falling between them.
    pub fn plain_text_with_page_ranges(&self) -> (String, Vec<(usize, usize)>) {
        let mut text = String::new();
        let mut ranges = Vec::with_capacity(self.pages.len());
        for (i, page) in self.pages.iter().enumerate() {
            let start = text.len();
            text.push_str(&page.text);
            if i + 1 < self.pages.len() {
                text.push_str("\n\n");
            }
            ranges.push((start, text.len()));
        }
        (text, ranges)
    }

    pub fn is_empty(&self) -> bool {
        self.pages.iter().all(|p| p.text.trim().is_empty())
    }
}

/// The bytes to read, plus what they are.
pub struct SourceDocument<'a> {
    pub bytes: &'a [u8],
    /// Lowercase media type WITHOUT the dot, matching
    /// [`DocumentUnderstanding::media_types`] — e.g. `"pdf"`.
    pub media_type: &'a str,
    /// Human label for errors and provenance (usually the file name).
    pub label: &'a str,
    /// Which pages to read, 1-based; `None` reads all of them.
    ///
    /// Escalation sets this to exactly the pages whose text layer was found
    /// damaged, so recovering one broken figure page out of forty costs one
    /// page of model time rather than forty. An adapter that cannot read
    /// selectively may ignore this and return every page — the caller keys
    /// results by [`PageText::number`], not by position.
    pub pages: Option<&'a [u32]>,
}

impl<'a> SourceDocument<'a> {
    /// Read the whole document.
    pub fn whole(bytes: &'a [u8], media_type: &'a str, label: &'a str) -> Self {
        Self {
            bytes,
            media_type,
            label,
            pages: None,
        }
    }

    /// Whether this adapter should produce page `number`.
    pub fn wants_page(&self, number: u32) -> bool {
        self.pages.is_none_or(|wanted| wanted.contains(&number))
    }
}

/// Whether an adapter can run right now.
///
/// Separate from `understand` returning `Err` on purpose: "this machine has no
/// rasteriser installed" is a provisioning fact known BEFORE any document is
/// read, and the escalation policy must be able to skip an unavailable adapter
/// without first handing it bytes and catching a failure. The reason is a
/// sentence a user can act on, not a diagnostic.
#[derive(Debug, Clone)]
pub enum Readiness {
    Ready,
    Unavailable(String),
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        matches!(self, Readiness::Ready)
    }
}

/// One way of reading a document.
#[async_trait]
pub trait DocumentUnderstanding: Send + Sync {
    /// Stable machine id, e.g. `"text-layer"`. The registry lookup key and the
    /// value recorded in [`Understanding::adapter_id`].
    fn id(&self) -> &'static str;

    /// Media types this adapter reads, lowercase and without the dot. Claims
    /// are NON-exclusive here — see the module docs.
    fn media_types(&self) -> &'static [&'static str];

    /// The modality this adapter reads with, stamped onto its results.
    fn modality(&self) -> Modality;

    /// Whether this adapter can run on this machine right now. Called before
    /// dispatch; an unavailable adapter is skipped, never handed bytes.
    fn readiness(&self) -> Readiness;

    /// Read the document.
    ///
    /// Async because the interesting readers are network-backed — a vision
    /// model, MinerU, a cloud OCR service. An adapter doing CPU-bound work
    /// (the text layer parses PDFs inline, and `pdf-extract` can PANIC on
    /// malformed input) is responsible for its own `spawn_blocking`, so one
    /// adapter's parser cannot stall the runtime or tear down the run.
    async fn understand(&self, doc: &SourceDocument<'_>) -> Result<Understanding>;
}

/// An adapter's declaration, captured by calling the trait methods ONCE,
/// outside any lock — adapter code never runs while the process-wide registry
/// lock is held, so an adapter whose `id()`/`media_types()` re-enters the
/// registry cannot deadlock registration.
#[derive(Clone, Copy)]
struct Declaration {
    id: &'static str,
    media_types: &'static [&'static str],
}

impl Declaration {
    fn of(adapter: &dyn DocumentUnderstanding) -> Result<Self> {
        let decl = Self {
            id: adapter.id(),
            media_types: adapter.media_types(),
        };
        if decl.id.trim().is_empty() {
            bail!("document-understanding adapter id must be non-empty");
        }
        if decl.media_types.is_empty() {
            bail!(
                "document-understanding adapter '{}' claims no media types",
                decl.id
            );
        }
        for (i, media) in decl.media_types.iter().enumerate() {
            if media.is_empty()
                || media.contains('.')
                || media.chars().any(|c| c.is_ascii_uppercase())
            {
                bail!(
                    "document-understanding adapter '{}' media claim '{media}' must be \
                     non-empty, lowercase, and without the dot",
                    decl.id
                );
            }
            if decl.media_types[..i].contains(media) {
                bail!(
                    "document-understanding adapter '{}' claims media type '{media}' twice",
                    decl.id
                );
            }
        }
        Ok(decl)
    }
}

/// Which adapter reads a document.
#[derive(Debug, Clone, PartialEq)]
pub enum Policy {
    /// Use exactly this adapter. If it is unavailable or fails, the read
    /// fails — no silent substitution, because a caller that asked for vision
    /// and quietly got text-layer output would be told a falsehood about the
    /// provenance of every fact that follows.
    Only(String),
    /// Try adapters in registration order, skipping unavailable ones, and
    /// take the first result whose text layer is not damaged, judged by the
    /// carried [`DamagePolicy`]. This is how a scanned or broken-CMap PDF
    /// reaches a vision model without anyone configuring anything — and the
    /// thresholds travel WITH the request, so a corpus that needs different
    /// ones does not need a different build.
    Escalate(super::DamagePolicy),
}

impl Default for Policy {
    fn default() -> Self {
        Policy::Escalate(super::DamagePolicy::default())
    }
}

/// Ordered registry of document-understanding adapters — the ONE place the
/// "how do I read this?" decision is made. Iteration order is registration
/// order, which is also escalation order under [`Policy::Escalate`]: cheap
/// local readers register before expensive model-backed ones.
pub struct UnderstandingRegistry {
    adapters: Vec<Arc<dyn DocumentUnderstanding>>,
    decls: Vec<Declaration>,
    by_id: HashMap<&'static str, usize>,
}

impl UnderstandingRegistry {
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
            decls: Vec::new(),
            by_id: HashMap::new(),
        }
    }

    /// The built-in adapters, in escalation order: the free local text layer
    /// first, model-backed vision second.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(super::TextLayerUnderstanding))
            .expect("built-in adapter declarations are valid and unique");
        reg
    }

    /// Add an adapter under a FREE id.
    ///
    /// Unlike the connector plane this does NOT refuse a taken media type:
    /// several adapters reading `pdf` is the design, not a collision.
    pub fn register(&mut self, adapter: Arc<dyn DocumentUnderstanding>) -> Result<()> {
        let decl = Declaration::of(adapter.as_ref())?;
        self.insert_new(decl, adapter)
    }

    /// Deliberately swap the adapter registered under the SAME id. The id must
    /// be taken — a typo cannot silently ADD an adapter while the one the
    /// caller meant to displace keeps running. Returns the displaced adapter.
    pub fn replace(
        &mut self,
        adapter: Arc<dyn DocumentUnderstanding>,
    ) -> Result<Arc<dyn DocumentUnderstanding>> {
        let decl = Declaration::of(adapter.as_ref())?;
        let displaced = self.swap(decl, adapter)?;
        tracing::info!(id = decl.id, "document-understanding adapter replaced");
        Ok(displaced)
    }

    fn insert_new(
        &mut self,
        decl: Declaration,
        adapter: Arc<dyn DocumentUnderstanding>,
    ) -> Result<()> {
        if self.by_id.contains_key(decl.id) {
            bail!(
                "document-understanding adapter id '{}' is already registered; swap it \
                 deliberately with UnderstandingRegistry::replace (replace_understanding \
                 for the process-wide registry)",
                decl.id
            );
        }
        let idx = self.adapters.len();
        self.by_id.insert(decl.id, idx);
        self.adapters.push(adapter);
        self.decls.push(decl);
        Ok(())
    }

    fn swap(
        &mut self,
        decl: Declaration,
        adapter: Arc<dyn DocumentUnderstanding>,
    ) -> Result<Arc<dyn DocumentUnderstanding>> {
        let Some(&idx) = self.by_id.get(decl.id) else {
            bail!(
                "no document-understanding adapter '{}' registered to replace; add it with \
                 UnderstandingRegistry::register (register_understanding for the \
                 process-wide registry)",
                decl.id
            );
        };
        self.decls[idx] = decl;
        Ok(std::mem::replace(&mut self.adapters[idx], adapter))
    }

    /// Look up an adapter by its stable id.
    pub fn get(&self, id: &str) -> Option<Arc<dyn DocumentUnderstanding>> {
        self.by_id.get(id).map(|&idx| self.adapters[idx].clone())
    }

    /// Remove the adapter registered under `id`, returning it. The id must be
    /// taken — a typo must not report success while the adapter it meant to
    /// remove keeps answering. Escalation order of the remaining adapters is
    /// preserved.
    ///
    /// This is the plane's removal inverse: now that an adapter can be
    /// installed by a supervised component (see
    /// [`super::vision_seam::VisionReaderComponent`]), installation has to be
    /// undoable without leaving a stand-in behind — a registry that can only
    /// ever grow cannot be recovered exactly.
    pub fn deregister(&mut self, id: &str) -> Result<Arc<dyn DocumentUnderstanding>> {
        let Some(&idx) = self.by_id.get(id) else {
            bail!("no document-understanding adapter '{id}' registered to deregister");
        };
        let adapter = self.adapters.remove(idx);
        self.decls.remove(idx);
        // Removal shifts every later adapter down one slot; rebuild the index
        // rather than patching it, so it cannot drift from the vectors.
        self.by_id = self
            .decls
            .iter()
            .enumerate()
            .map(|(i, decl)| (decl.id, i))
            .collect();
        Ok(adapter)
    }

    /// Every adapter claiming `media_type`, in registration (escalation)
    /// order. Availability is NOT filtered here — the caller reports which
    /// adapters it skipped and why, so an unavailable vision adapter produces
    /// an actionable message instead of silently not existing.
    pub fn candidates(&self, media_type: &str) -> Vec<Arc<dyn DocumentUnderstanding>> {
        let media = media_type.to_ascii_lowercase();
        self.decls
            .iter()
            .enumerate()
            .filter(|(_, decl)| decl.media_types.contains(&media.as_str()))
            .map(|(idx, _)| self.adapters[idx].clone())
            .collect()
    }

    /// All registered adapters, in registration order.
    pub fn all(&self) -> &[Arc<dyn DocumentUnderstanding>] {
        &self.adapters
    }
}

impl Default for UnderstandingRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

/// The process-wide registry: starts as [`UnderstandingRegistry::builtin`] and
/// is extendable at runtime through [`register_understanding`].
static REGISTRY: LazyLock<RwLock<UnderstandingRegistry>> =
    LazyLock::new(|| RwLock::new(UnderstandingRegistry::builtin()));

/// Read access to the process-wide registry. Hold the guard only for the
/// query — never across an `.await`.
pub fn registry() -> RwLockReadGuard<'static, UnderstandingRegistry> {
    REGISTRY
        .read()
        .expect("document-understanding registry lock poisoned")
}

/// Register an adapter in the process-wide registry. The declaration is
/// captured BEFORE the write lock is taken, so adapter code that re-enters the
/// registry cannot deadlock registration.
pub fn register_understanding(adapter: Arc<dyn DocumentUnderstanding>) -> Result<()> {
    let decl = Declaration::of(adapter.as_ref())?;
    REGISTRY
        .write()
        .expect("document-understanding registry lock poisoned")
        .insert_new(decl, adapter)
}

/// Deliberately swap an adapter in the process-wide registry — how a caller
/// takes over `"text-layer"` or `"vision"` with their own implementation
/// (MinerU, a cloud OCR service). Returns the displaced adapter.
pub fn replace_understanding(
    adapter: Arc<dyn DocumentUnderstanding>,
) -> Result<Arc<dyn DocumentUnderstanding>> {
    let decl = Declaration::of(adapter.as_ref())?;
    let displaced = REGISTRY
        .write()
        .expect("document-understanding registry lock poisoned")
        .swap(decl, adapter)?;
    tracing::info!(
        id = decl.id,
        "document-understanding adapter replaced in the process-wide registry"
    );
    Ok(displaced)
}

/// Remove an adapter from the process-wide registry — the inverse of
/// [`register_understanding`]. Returns the removed adapter.
pub fn deregister_understanding(id: &str) -> Result<Arc<dyn DocumentUnderstanding>> {
    let removed = REGISTRY
        .write()
        .expect("document-understanding registry lock poisoned")
        .deregister(id)?;
    tracing::info!(
        id,
        "document-understanding adapter removed from the process-wide registry"
    );
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        id: &'static str,
        media: &'static [&'static str],
        modality: Modality,
        ready: bool,
        text: &'static str,
    }

    #[async_trait]
    impl DocumentUnderstanding for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn media_types(&self) -> &'static [&'static str] {
            self.media
        }
        fn modality(&self) -> Modality {
            self.modality
        }
        fn readiness(&self) -> Readiness {
            if self.ready {
                Readiness::Ready
            } else {
                Readiness::Unavailable("fake is switched off".into())
            }
        }
        async fn understand(&self, _doc: &SourceDocument<'_>) -> Result<Understanding> {
            Ok(Understanding {
                adapter_id: self.id.into(),
                modality: self.modality,
                pages: vec![PageText {
                    number: 1,
                    text: self.text.into(),
                }],
            })
        }
    }

    fn fake(id: &'static str, media: &'static [&'static str]) -> Arc<dyn DocumentUnderstanding> {
        Arc::new(Fake {
            id,
            media,
            modality: Modality::Vision,
            ready: true,
            text: "t",
        })
    }

    /// THE property that separates this plane from every other one: two
    /// adapters may claim the same media type, and BOTH stay reachable in
    /// registration order. On the connector plane this is a refused
    /// collision; here it is the feature.
    #[test]
    fn several_adapters_may_claim_the_same_media_type() {
        let mut reg = UnderstandingRegistry::builtin();
        reg.register(fake("zzz-vision", &["pdf"]))
            .expect("a second pdf reader must register");
        reg.register(fake("zzz-third", &["pdf", "tiff"]))
            .expect("a third pdf reader must register");

        let ids: Vec<&str> = reg.candidates("pdf").iter().map(|a| a.id()).collect();
        assert_eq!(
            ids,
            ["text-layer", "zzz-vision", "zzz-third"],
            "every claimant is reachable, in registration (escalation) order",
        );
        // Claims are matched case-insensitively, and an unclaimed type is empty.
        assert_eq!(reg.candidates("PDF").len(), 3);
        assert_eq!(reg.candidates("tiff").len(), 1);
        assert!(reg.candidates("zzz-unclaimed").is_empty());
    }

    /// The shared two-call contract still governs IDS, even though media
    /// claims are open.
    #[test]
    fn register_refuses_taken_id_and_replace_refuses_free_id() {
        let mut reg = UnderstandingRegistry::builtin();

        let err = reg
            .register(fake("text-layer", &["pdf"]))
            .expect_err("a taken id must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("already registered"), "{msg}");
        assert!(msg.contains("replace_understanding"), "{msg}");

        let err = match reg.replace(fake("zzz-absent", &["pdf"])) {
            Err(e) => e,
            Ok(_) => panic!("replacing an unregistered id must be refused"),
        };
        assert!(format!("{err:#}").contains("no document-understanding"),);

        // Neither refusal changed anything.
        assert_eq!(reg.all().len(), 1);
        assert_eq!(reg.candidates("pdf").len(), 1);
    }

    /// Replacement swaps in place and hands back the displaced adapter, so a
    /// caller can restore the built-in.
    #[test]
    fn replace_swaps_in_place_and_returns_the_displaced() {
        let mut reg = UnderstandingRegistry::builtin();
        let displaced = reg
            .replace(fake("text-layer", &["pdf", "tiff"]))
            .expect("a registered id must be replaceable");
        assert_eq!(displaced.id(), "text-layer");
        assert_eq!(displaced.modality(), Modality::TextLayer);

        // The replacement is what dispatch now finds, with ITS claims.
        assert_eq!(
            reg.get("text-layer").expect("registered").modality(),
            Modality::Vision,
        );
        assert_eq!(reg.candidates("tiff").len(), 1, "new claim is live");
        assert_eq!(reg.all().len(), 1, "replaced in place, not appended");
    }

    /// Deregistration is exact: the named adapter goes, everything else keeps
    /// its escalation order, and an absent id is refused rather than
    /// reporting a removal that never happened.
    #[test]
    fn deregister_removes_exactly_the_named_adapter() {
        let mut reg = UnderstandingRegistry::builtin();
        reg.register(fake("zzz-vision", &["pdf"])).unwrap();
        reg.register(fake("zzz-third", &["pdf"])).unwrap();

        let removed = reg
            .deregister("zzz-vision")
            .expect("a registered id must be removable");
        assert_eq!(removed.id(), "zzz-vision");

        let ids: Vec<&str> = reg.candidates("pdf").iter().map(|a| a.id()).collect();
        assert_eq!(
            ids,
            ["text-layer", "zzz-third"],
            "the survivors keep their escalation order",
        );
        // The index survived the shift: the shifted adapter is still
        // reachable by id, and the removed one is gone.
        assert_eq!(
            reg.get("zzz-third").expect("still registered").id(),
            "zzz-third"
        );
        assert!(reg.get("zzz-vision").is_none());

        let err = match reg.deregister("zzz-vision") {
            Err(e) => e,
            Ok(_) => panic!("an absent id must be refused"),
        };
        assert!(format!("{err:#}").contains("no document-understanding"));
    }

    #[test]
    fn malformed_declarations_are_refused() {
        let mut reg = UnderstandingRegistry::new();
        let malformed: &[(&'static str, &'static [&'static str])] = &[
            ("bad-dotted", &[".pdf"]),
            ("bad-uppercase", &["PDF"]),
            ("bad-empty-claim", &[""]),
            ("bad-no-claims", &[]),
            ("", &["pdf"]),
            ("bad-duplicate", &["pdf", "pdf"]),
        ];
        for &(id, media) in malformed {
            assert!(
                reg.register(fake(id, media)).is_err(),
                "declaration id={id:?} media={media:?} must be refused",
            );
        }
        assert!(reg.all().is_empty());
    }

    /// `plain_text_with_page_ranges` yields the SAME string as `plain_text`
    /// (so the extraction prompt and segmentation cannot drift), with ranges
    /// that tile it exactly — every byte inside exactly one page's range.
    #[test]
    fn page_ranges_tile_the_plain_text_exactly() {
        let understanding = Understanding {
            adapter_id: "text-layer".into(),
            modality: Modality::TextLayer,
            pages: vec![
                PageText {
                    number: 1,
                    text: "first page".into(),
                },
                PageText {
                    number: 2,
                    text: "second page".into(),
                },
                PageText {
                    number: 3,
                    text: "third".into(),
                },
            ],
        };
        let (text, ranges) = understanding.plain_text_with_page_ranges();
        assert_eq!(text, understanding.plain_text());
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, text.len());
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "ranges must tile with no gap");
        }
        // Each page's text sits inside its own range (the range also carries
        // the joiner that follows).
        for (range, page) in ranges.iter().zip(&understanding.pages) {
            assert!(text[range.0..range.1].starts_with(&page.text));
        }

        let empty = Understanding {
            adapter_id: "text-layer".into(),
            modality: Modality::TextLayer,
            pages: vec![],
        };
        let (text, ranges) = empty.plain_text_with_page_ranges();
        assert!(text.is_empty() && ranges.is_empty());
    }

    /// Unavailability is reportable BEFORE any bytes are handed over — the
    /// escalation policy must be able to skip an adapter without failing a
    /// read to discover it cannot run.
    #[test]
    fn readiness_is_answerable_without_reading_a_document() {
        let off = Fake {
            id: "zzz-off",
            media: &["pdf"],
            modality: Modality::Vision,
            ready: false,
            text: "",
        };
        match off.readiness() {
            Readiness::Unavailable(reason) => assert!(!reason.trim().is_empty()),
            Readiness::Ready => panic!("must report unavailable"),
        }
        assert!(!off.readiness().is_ready());
    }
}
