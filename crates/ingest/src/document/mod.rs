//! Document understanding: turning document bytes into text worth extracting
//! facts from.
//!
//! The sixth adapter plane, alongside file [connectors](crate::connectors),
//! literature sources, materials providers, identity plugins and
//! [ontologies](crate::ontologies). What makes it its own plane rather than a
//! function call is that reading a PDF is genuinely plural: the embedded text
//! layer, a vision model over rendered pages, MinerU, a commercial OCR
//! service. They read the SAME bytes with different cost, different
//! availability and different trustworthiness, so which one ran is a fact
//! about the data, not an implementation detail.
//!
//! ```text
//! bytes ──> [ text-layer ] ──damaged?──> [ vision ] ──> per-page text + modality
//! ```
//!
//! [`read`] is the entry point. Under [`Policy::Escalate`] it reads the cheap
//! text layer, asks [`quality::text_layer_damage`] which pages came out
//! broken, and re-reads only those pages with the next adapter that is
//! available. Sound pages keep their text-layer text, so a paper with one
//! scanned appendix does not pay to have thirty-nine good pages re-read.

mod quality;
mod rasterise;
mod text_layer;
mod understanding;
mod vision;

pub use quality::{
    BUILTIN_SIGNALS, Damage, DamagePolicy, DamageSignal, is_degenerate, text_layer_damage,
    text_layer_damage_with,
};
pub use rasterise::CommandRasteriser;
pub use text_layer::TextLayerUnderstanding;
pub use understanding::{
    DocumentUnderstanding, Modality, PageText, Policy, Readiness, SourceDocument, Understanding,
    UnderstandingRegistry, register_understanding, registry, replace_understanding,
};
pub use vision::{PageRasteriser, VisionUnderstanding};

use anyhow::{Result, bail};

/// What a read produced, and everything that happened on the way — including
/// what did NOT happen. A page recovered by vision, a page left damaged
/// because no vision adapter was installed, and a page that was fine all along
/// are three different outcomes, and a caller that cannot tell them apart
/// cannot report honestly on the data it just ingested.
#[derive(Debug, Clone)]
pub struct ReadOutcome {
    pub understanding: Understanding,
    /// Per-page notes, in page order. Empty when every page read cleanly on
    /// the first adapter.
    pub notes: Vec<PageNote>,
    /// Adapters that could have helped but could not run, with the reason
    /// each gave. Surfaced to the user verbatim: "vision is unavailable
    /// because X" is actionable, a silently missing capability is not.
    pub skipped: Vec<String>,
}

/// What happened to one page.
#[derive(Debug, Clone)]
pub struct PageNote {
    pub number: u32,
    pub damage: Damage,
    /// The adapter that recovered it, or `None` if nothing could.
    pub recovered_by: Option<String>,
}

impl ReadOutcome {
    /// Pages still damaged after every available adapter had a turn.
    pub fn unrecovered(&self) -> impl Iterator<Item = &PageNote> {
        self.notes.iter().filter(|n| n.recovered_by.is_none())
    }
}

/// Read a document into text.
///
/// Under [`Policy::Only`] exactly the named adapter runs and its result is
/// returned as-is: a caller that asked for vision must not silently receive
/// text-layer output, because every fact extracted afterwards would carry the
/// wrong modality in its provenance.
///
/// Under [`Policy::Escalate`] adapters run in registration order — cheap
/// first — and each one is asked only for the pages its predecessors could not
/// read cleanly.
///
/// Each adapter owns its own blocking: the text layer parses on a blocking
/// thread and turns a `pdf-extract` panic into a described per-file error,
/// rather than every caller having to know which readers are CPU-bound.
pub async fn read(doc: &SourceDocument<'_>, policy: &Policy) -> Result<ReadOutcome> {
    let candidates = registry().candidates(doc.media_type);
    if candidates.is_empty() {
        bail!(
            "no document-understanding adapter reads '{}' (reading {})",
            doc.media_type,
            doc.label
        );
    }

    match policy {
        Policy::Only(id) => {
            let Some(adapter) = candidates.iter().find(|a| a.id() == id) else {
                bail!(
                    "document-understanding adapter '{id}' is not registered for '{}'; \
                     registered readers are: {}",
                    doc.media_type,
                    candidates
                        .iter()
                        .map(|a| a.id())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            };
            if let Readiness::Unavailable(reason) = adapter.readiness() {
                bail!("document-understanding adapter '{id}' cannot run: {reason}");
            }
            let understanding = adapter.understand(doc).await?;
            // Damage is still REPORTED under Only — the caller chose this
            // adapter and gets its output, but must not be told the pages
            // were clean when they were not.
            let notes = understanding
                .pages
                .iter()
                .filter_map(|page| {
                    text_layer_damage(&page.text, &DamagePolicy::default()).map(|damage| PageNote {
                        number: page.number,
                        damage,
                        recovered_by: None,
                    })
                })
                .collect();
            Ok(ReadOutcome {
                understanding,
                notes,
                skipped: Vec::new(),
            })
        }
        Policy::Escalate(damage) => escalate(doc, &candidates, damage).await,
    }
}

async fn escalate(
    doc: &SourceDocument<'_>,
    candidates: &[std::sync::Arc<dyn DocumentUnderstanding>],
    damage_policy: &DamagePolicy,
) -> Result<ReadOutcome> {
    let mut skipped = Vec::new();
    let mut pages: Vec<PageText> = Vec::new();
    let mut modality = None;
    let mut adapter_ids: Vec<&str> = Vec::new();
    // Page number -> the damage the FIRST adapter found, kept so the note
    // explains why the page was escalated at all, not what the last adapter
    // thought of its own output.
    let mut damaged: Vec<(u32, Damage)> = Vec::new();
    let mut recovered: Vec<(u32, String)> = Vec::new();

    let mut first = true;
    for adapter in candidates {
        // Nothing left to fix.
        if !first && damaged.is_empty() {
            break;
        }
        if let Readiness::Unavailable(reason) = adapter.readiness() {
            skipped.push(format!("{}: {reason}", adapter.id()));
            continue;
        }

        let wanted: Vec<u32> = damaged.iter().map(|(n, _)| *n).collect();
        let scoped = SourceDocument {
            bytes: doc.bytes,
            media_type: doc.media_type,
            label: doc.label,
            pages: if first { doc.pages } else { Some(&wanted) },
        };

        let result = match adapter.understand(&scoped).await {
            Ok(result) => result,
            Err(error) => {
                // A later adapter failing is not fatal: the pages it was
                // meant to rescue stay damaged and are reported as such.
                // The FIRST adapter failing means we have nothing at all.
                if first {
                    return Err(error);
                }
                skipped.push(format!("{}: {error:#}", adapter.id()));
                continue;
            }
        };
        adapter_ids.push(adapter.id());

        if first {
            modality = Some(result.modality);
            for page in result.pages {
                if let Some(damage) = text_layer_damage(&page.text, damage_policy) {
                    damaged.push((page.number, damage));
                }
                pages.push(page);
            }
            first = false;
            continue;
        }

        // A later adapter only replaces a page when its own output is
        // BETTER: not damaged, and not a repetition loop. A model that loops
        // on a micrograph must not overwrite the text layer's honest text.
        for page in result.pages {
            // Only pages this round actually ASKED for may be replaced. The
            // adapter contract permits returning every page (an adapter that
            // cannot read selectively is allowed to ignore the request), so
            // without this a later reader can overwrite a page the first
            // reader got right — replacing a sound "1000 MPa" with a fluent
            // "9000 MPa" purely because the new prose passes the syntactic
            // checks. Sound pages are never up for replacement.
            if !damaged.iter().any(|(n, _)| *n == page.number) {
                continue;
            }
            let sound = text_layer_damage(&page.text, damage_policy).is_none()
                && !is_degenerate(&page.text, damage_policy);
            if !sound {
                continue;
            }
            let Some(slot) = pages.iter_mut().find(|p| p.number == page.number) else {
                continue;
            };
            slot.text = page.text;
            recovered.push((page.number, adapter.id().to_string()));
            damaged.retain(|(n, _)| *n != page.number);
        }
    }

    let mut notes: Vec<PageNote> = damaged
        .iter()
        .map(|(number, damage)| PageNote {
            number: *number,
            damage: damage.clone(),
            recovered_by: None,
        })
        .chain(recovered.iter().map(|(number, by)| PageNote {
            number: *number,
            // Recovered pages record what was wrong before the rescue.
            damage: Damage::new("recovered", "recovered by a later reader"),
            recovered_by: Some(by.clone()),
        }))
        .collect();
    notes.sort_by_key(|n| n.number);

    Ok(ReadOutcome {
        understanding: Understanding {
            adapter_id: adapter_ids.join("+"),
            modality: modality.unwrap_or(Modality::TextLayer),
            pages,
        },
        notes,
        skipped,
    })
}

#[cfg(test)]
mod tests {

    /// An adapter is ALLOWED to return pages nobody asked for (the contract
    /// says a reader that cannot select may return everything). It must not
    /// be able to overwrite a page the first reader already got right — a
    /// fluent "9000 MPa" replacing a sound "1000 MPa" purely because the new
    /// prose is syntactically clean.
    #[tokio::test]
    async fn an_unrequested_page_cannot_overwrite_a_sound_one() {
        let good_page_1 = sound(1);
        let broken = (2u32, String::new());
        let text = scripted(
            "text-layer",
            Modality::TextLayer,
            true,
            vec![good_page_1.clone(), broken],
        );
        // An oversharing reader: asked for page 2, answers with 1 AND 2, and
        // its page 1 is clean prose that contradicts the original.
        let oversharing = Arc::new(Scripted {
            id: "vision",
            modality: Modality::Vision,
            ready: true,
            pages: vec![
                (
                    1,
                    "Alloy A exhibits a yield strength of 9000 MPa under all \
                     conditions, which is a fluent and entirely clean sentence \
                     that nonetheless contradicts the sound original page."
                        .to_string(),
                ),
                sound(2),
            ],
            asked: std::sync::Mutex::new(Vec::new()),
            // Deliberately ignores the page request, as the contract permits.
            ignore_page_request: true,
        });

        let mut reg = UnderstandingRegistry::new();
        reg.register(text as Arc<dyn DocumentUnderstanding>)
            .unwrap();
        reg.register(oversharing as Arc<dyn DocumentUnderstanding>)
            .unwrap();
        let candidates = reg.candidates("pdf");
        let doc = SourceDocument::whole(b"%PDF-1.7", "pdf", "t.pdf");
        let outcome = escalate(&doc, &candidates, &DamagePolicy::default())
            .await
            .expect("escalation reads");

        let page1 = outcome
            .understanding
            .pages
            .iter()
            .find(|p| p.number == 1)
            .expect("page 1 present");
        assert_eq!(
            page1.text, good_page_1.1,
            "a sound page must never be replaced by an unrequested rewrite",
        );
        // Page 2, which WAS asked for, is still recovered.
        let page2 = outcome
            .understanding
            .pages
            .iter()
            .find(|p| p.number == 2)
            .expect("page 2 present");
        assert_eq!(page2.text, sound(2).1);
    }

    /// `read()` had NO test — its only caller is the CLI — so the whole
    /// `Policy::Only` branch was unguarded. An audit replaced the id lookup
    /// with "just take the first candidate" and nothing failed: a caller
    /// asking for vision would silently receive text-layer output, and every
    /// fact extracted after it would carry the wrong modality in provenance.
    #[tokio::test]
    async fn policy_only_never_substitutes_a_different_reader() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let doc = SourceDocument::whole(b"%PDF-1.7", "pdf", "t.pdf");

        // Only the built-in text layer is registered; asking for vision must
        // FAIL rather than quietly returning the text layer's output.
        let err = read(&doc, &Policy::Only("vision".into()))
            .await
            .expect_err("an absent reader must not be substituted");
        let msg = format!("{err:#}");
        assert!(msg.contains("vision"), "{msg}");
        assert!(msg.contains("not registered"), "{msg}");
        // The message names what IS available, so the error is actionable.
        assert!(msg.contains("text-layer"), "{msg}");

        // And the reader that IS registered can still be asked for by name.
        let outcome = read(&doc, &Policy::Only("text-layer".into())).await;
        match outcome {
            // Either it read, or it failed parsing these 8 bytes — both are
            // the TEXT-LAYER answering. What must never happen is another
            // adapter answering in its place.
            Ok(outcome) => assert_eq!(outcome.understanding.adapter_id, "text-layer"),
            Err(error) => assert!(
                format!("{error:#}").contains("t.pdf"),
                "the failure must come from reading THIS document: {error:#}",
            ),
        }
    }

    /// A media type nothing claims is refused by name, not silently empty.
    #[tokio::test]
    async fn an_unclaimed_media_type_is_refused() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let doc = SourceDocument::whole(b"...", "zzz-unclaimed", "t.zzz");
        let err = read(&doc, &Policy::default())
            .await
            .expect_err("no reader claims this");
        assert!(format!("{err:#}").contains("zzz-unclaimed"), "{err:#}");
    }
    use super::*;
    use std::sync::Arc;

    /// An adapter that returns scripted per-page text, and records which
    /// pages it was ASKED for — the assertion that escalation is scoped.
    struct Scripted {
        id: &'static str,
        modality: Modality,
        ready: bool,
        pages: Vec<(u32, String)>,
        asked: std::sync::Mutex<Vec<u32>>,
        /// Answer with EVERY page regardless of what was requested — which
        /// the adapter contract explicitly permits.
        ignore_page_request: bool,
    }

    #[async_trait::async_trait]
    impl DocumentUnderstanding for Scripted {
        fn id(&self) -> &'static str {
            self.id
        }
        fn media_types(&self) -> &'static [&'static str] {
            &["pdf"]
        }
        fn modality(&self) -> Modality {
            self.modality
        }
        fn readiness(&self) -> Readiness {
            if self.ready {
                Readiness::Ready
            } else {
                Readiness::Unavailable("no rasteriser installed".into())
            }
        }
        async fn understand(&self, doc: &SourceDocument<'_>) -> Result<Understanding> {
            let wants = |n: u32| self.ignore_page_request || doc.wants_page(n);
            let wanted: Vec<u32> = self
                .pages
                .iter()
                .map(|(n, _)| *n)
                .filter(|n| wants(*n))
                .collect();
            *self.asked.lock().unwrap() = wanted.clone();
            Ok(Understanding {
                adapter_id: self.id.into(),
                modality: self.modality,
                pages: self
                    .pages
                    .iter()
                    .filter(|(n, _)| wants(*n))
                    .map(|(n, t)| PageText {
                        number: *n,
                        text: t.clone(),
                    })
                    .collect(),
            })
        }
    }

    /// A page of genuine body text — comfortably above the sparse floor, so
    /// these fixtures exercise escalation rather than tripping it.
    fn sound(n: u32) -> (u32, String) {
        (
            n,
            format!(
                "Page {n}: a genuine paragraph of body text about nickel superalloy disks, \
                 powder metallurgy processing, and thermal cracking behaviour in alloys \
                 with high refractory content such as molybdenum, niobium, tantalum and \
                 tungsten, which are more prone to cracking than IN718."
            ),
        )
    }

    fn scripted(
        id: &'static str,
        modality: Modality,
        ready: bool,
        pages: Vec<(u32, String)>,
    ) -> Arc<Scripted> {
        Arc::new(Scripted {
            id,
            modality,
            ready,
            pages,
            asked: std::sync::Mutex::new(Vec::new()),
            ignore_page_request: false,
        })
    }

    async fn read_with(
        adapters: Vec<Arc<Scripted>>,
        policy: Policy,
    ) -> (ReadOutcome, Vec<Arc<Scripted>>) {
        let mut reg = UnderstandingRegistry::new();
        for a in &adapters {
            reg.register(a.clone() as Arc<dyn DocumentUnderstanding>)
                .expect("test adapters register");
        }
        let candidates = reg.candidates("pdf");
        let doc = SourceDocument::whole(b"%PDF-1.7", "pdf", "t.pdf");
        let outcome = match policy {
            Policy::Escalate(ref d) => escalate(&doc, &candidates, d)
                .await
                .expect("escalation reads"),
            Policy::Only(_) => unreachable!("Only is exercised through read()"),
        };
        (outcome, adapters)
    }

    /// THE point of the plane: a page whose text layer is broken is rescued
    /// by the next adapter, and the sound pages are NOT re-read.
    #[tokio::test]
    async fn only_damaged_pages_escalate_and_they_get_recovered() {
        let broken = (2u32, "\u{01}\u{1a}(%\u{18}))\u{01}9&\u{16}(".to_string());
        let text = scripted(
            "text-layer",
            Modality::TextLayer,
            true,
            vec![sound(1), broken, sound(3)],
        );
        let vision = scripted(
            "vision",
            Modality::Vision,
            true,
            vec![
                sound(1),
                // The real table that the broken CMap above destroyed.
                (
                    2,
                    "Process/parameters. Electron Beam Melting (EBM): vacuum, pre-heat \
                     beam passes about ten, melt scan speed of order one thousand \
                     millimetres per second, elevated build temperature, minimal induced \
                     residual stress."
                        .to_string(),
                ),
                sound(3),
            ],
        );
        let (outcome, adapters) = read_with(vec![text, vision], Policy::default()).await;

        // Vision was asked for page 2 ALONE — a forty-page paper does not pay
        // for one broken page.
        assert_eq!(*adapters[1].asked.lock().unwrap(), vec![2]);

        let page2 = outcome
            .understanding
            .pages
            .iter()
            .find(|p| p.number == 2)
            .expect("page 2 present");
        assert!(page2.text.contains("Electron Beam Melting"), "{page2:?}");
        assert!(
            outcome.unrecovered().next().is_none(),
            "nothing left broken"
        );
        assert_eq!(
            outcome.notes.iter().filter(|n| n.number == 2).count(),
            1,
            "page 2 is reported as escalated",
        );
        assert_eq!(
            outcome.notes[0].recovered_by.as_deref(),
            Some("vision"),
            "the note names who rescued it",
        );
        // Sound pages keep the cheap reader's text verbatim.
        assert_eq!(
            outcome.understanding.pages[0].text,
            sound(1).1,
            "a sound page is never overwritten",
        );
    }

    /// A model that loops must NOT overwrite the text layer's honest text —
    /// the guard that keeps a degenerate transcription out of the graph.
    #[tokio::test]
    async fn a_looping_model_never_overwrites_the_text_layer() {
        // Sparse but real: a figure page the text layer under-reads.
        let thin = (1u32, "Figure 3. Cross-section of the disk.".to_string());
        let text = scripted("text-layer", Modality::TextLayer, true, vec![thin.clone()]);
        let looper = scripted(
            "vision",
            Modality::Vision,
            true,
            vec![(1, "10 mm\n".repeat(60))],
        );
        let (outcome, _) = read_with(vec![text, looper], Policy::default()).await;

        assert_eq!(
            outcome.understanding.pages[0].text, thin.1,
            "a repetition loop must not replace real text",
        );
        assert_eq!(
            outcome.unrecovered().count(),
            1,
            "the page is honestly reported as still damaged",
        );
    }

    /// An unavailable adapter is named with its reason rather than silently
    /// not existing — the difference between an actionable message and a
    /// capability that appears never to have been built.
    #[tokio::test]
    async fn an_unavailable_adapter_is_reported_with_its_reason() {
        let text = scripted(
            "text-layer",
            Modality::TextLayer,
            true,
            vec![(1, String::new())],
        );
        let vision = scripted("vision", Modality::Vision, false, vec![sound(1)]);
        let (outcome, _) = read_with(vec![text, vision], Policy::default()).await;

        assert_eq!(outcome.skipped.len(), 1);
        assert!(
            outcome.skipped[0].contains("vision"),
            "{:?}",
            outcome.skipped
        );
        assert!(
            outcome.skipped[0].contains("no rasteriser installed"),
            "the reason must survive to the user: {:?}",
            outcome.skipped,
        );
        assert_eq!(outcome.unrecovered().count(), 1);
    }

    /// A clean document costs exactly one adapter run — escalation is not a
    /// tax on documents that never needed it.
    #[tokio::test]
    async fn a_clean_document_never_reaches_the_expensive_adapter() {
        let text = scripted(
            "text-layer",
            Modality::TextLayer,
            true,
            vec![sound(1), sound(2)],
        );
        let vision = scripted("vision", Modality::Vision, true, vec![sound(1), sound(2)]);
        let (outcome, adapters) = read_with(vec![text, vision], Policy::default()).await;

        assert!(
            adapters[1].asked.lock().unwrap().is_empty(),
            "vision must not run on a sound document",
        );
        assert!(outcome.notes.is_empty(), "nothing to report");
        assert_eq!(outcome.understanding.modality, Modality::TextLayer);
    }
}
