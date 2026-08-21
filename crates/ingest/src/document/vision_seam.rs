//! The vision endpoint as a supervised dependency: one provider key flowing
//! through the composability seam ([`prism_runtime::seam`]), end to end.
//!
//! # The gap this closes
//!
//! [`VisionUnderstanding::readiness`](super::VisionUnderstanding) answers for
//! the RASTERISER only — a dead or drifted vision endpoint still reports
//! `Ready`, is handed the damaged pages, fails mid-read, and escalation
//! records the failure as one more skip note. A corpus run against a hung
//! endpoint paid that endpoint's full request timeout over and over, one
//! warning at a time, with nothing remembering between files that the
//! endpoint was gone.
//!
//! # The seam version
//!
//! The endpoint is a *key* ([`VISION_ENDPOINT_KEY`]), provided for the whole
//! ingest run. [`VisionReaderComponent`] consumes it: present, it installs
//! the live vision reader in the understanding registry; withdrawn, its
//! runtime-derived inverse swaps in a tombstone whose readiness names the key
//! and the reason, so `escalate` skips it with a sentence instead of a
//! timeout storm, and the skip note says exactly what to fix.
//!
//! # The reader is its own health signal
//!
//! There is deliberately NO probe in this module. Two earlier rounds gated
//! the reader behind a cheap synthetic check (`GET /models` with its own
//! client, its own 5-second budget, and at one point its own copy of the
//! auth rule), and both times the check was wrong about the reader it
//! vouched for: first a re-derived auth header 401'd a working endpoint and
//! parked the whole corpus; then a memoized 5-second blip disabled a working
//! endpoint for an entire run. The probe's budget was strictly stricter than
//! the reader's on every axis — no retries, no `timeout_secs`, cold
//! connections — and a gate stricter than the thing it gates will always
//! eventually park working capability.
//!
//! So the only evidence is real reads — and only the reads that actually
//! reached the wire. [`VisionSeam`] counts consecutive ENDPOINT-side
//! failures (`SkipKind::Failed(FailureOrigin::Remote)` notes from
//! escalation); after [`MAX_CONSECUTIVE_FAILURES`] with no success between
//! them it withdraws the key — the inverse installs the tombstone and the
//! rest of the corpus stops paying for the dead endpoint. A LOCAL failure
//! — poppler crashing on an encrypted PDF, the `image` crate refusing a
//! render — is a fact about that file or this machine and neither counts a
//! strike nor resets one: three broken PDFs in a row must not park a
//! healthy endpoint for the rest of the corpus. Recovery is also a real
//! read: on a bounded backoff the key is re-provided and the next
//! document's read is the trial — same auth, same timeouts, same retry
//! policy as every read, because it *is* one. Never park a working
//! endpoint on one failure; never pay a broken one four hundred times.
//!
//! # A deliberate reversal from round two
//!
//! Round two pinned "an endpoint that ANSWERS must never be parked over a
//! failed read" (`a_live_reader_failing_against_an_answering_host_is_not_
//! parked`). Round three reversed that, deliberately: a host that answers
//! and fails every read — a 500 on each call, a 400 from a text-only model
//! handed a PNG — bills the corpus per file exactly like a dead host, so
//! three consecutive remote failures park it whether or not the TCP
//! connection opened. What the origin distinction above preserves is the
//! narrower truth the old test was after: a failure that never reached the
//! endpoint says nothing about it, and can never park it.
//!
//! # Parking defers pages; it never discards a document
//!
//! The consumer-side decision is [`vision_deferral_reason`] (audit F2):
//! pages that needed vision while the key is withdrawn — or whose
//! endpoint-side vision read failed — are DEFERRED: named in the ingest
//! summary with the reason,
//! and readable again by a re-run once the endpoint answers. Everything the
//! text layer read soundly is extracted and stored NOW. A tool that quietly
//! ingests nothing and exits 0 is worse than one that ingests degraded
//! text; deferral gates only the work that actually depends on vision.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;

use prism_runtime::seam::{Activation, Component, Ctx, FiberState, FiberStatus, Key, Runtime};

use super::{
    CommandRasteriser, FailureOrigin, PageRasteriser, ReadOutcome, Readiness, SkipKind,
    VisionUnderstanding, register_understanding, replace_understanding,
};

/// The one key of stage 1: where the vision-capable model answers.
pub const VISION_ENDPOINT_KEY: Key = Key::new("llm.vision.endpoint");

/// The seam name of the consumer component, for `status()` lookups.
pub const VISION_READER_COMPONENT: &str = "vision-reader";

/// The value published under [`VISION_ENDPOINT_KEY`]: everything needed to
/// build the reader against the endpoint.
#[derive(Clone)]
pub struct VisionEndpoint {
    pub cfg: prism_llm::LlmConfig,
}

/// Consumes [`VISION_ENDPOINT_KEY`]; installs the vision reader while the key
/// is provided. There is no teardown method to read here — that is the point:
/// the tombstone swap below is registered as the inverse of the installation,
/// at the moment the installation happens.
pub struct VisionReaderComponent {
    rasteriser: Arc<dyn PageRasteriser>,
}

impl VisionReaderComponent {
    pub fn new(rasteriser: Arc<dyn PageRasteriser>) -> Self {
        Self { rasteriser }
    }

    /// The production rasteriser, matching what the CLI registered directly
    /// before the seam existed.
    pub fn poppler() -> Self {
        Self::new(Arc::new(CommandRasteriser::poppler()))
    }
}

static NEEDS: &[Key] = &[VISION_ENDPOINT_KEY];

#[async_trait]
impl Component for VisionReaderComponent {
    fn name(&self) -> &str {
        VISION_READER_COMPONENT
    }

    fn needs(&self) -> &[Key] {
        NEEDS
    }

    fn provides(&self) -> &[Key] {
        &[]
    }

    async fn activate(&mut self, ctx: &Ctx) -> Result<Activation> {
        let Some(endpoint) = ctx.get::<VisionEndpoint>(&VISION_ENDPOINT_KEY) else {
            return Ok(Activation::Parked(VISION_ENDPOINT_KEY));
        };
        // The live reader, ungated: its own reads are the health signal, and
        // they already carry the operator's auth, timeout and retry policy.
        let reader = Arc::new(VisionUnderstanding::new(
            self.rasteriser.clone(),
            Arc::new(prism_llm::LlmClient::new(endpoint.cfg.clone())),
        ));
        // First activation registers; reactivation displaces the tombstone
        // the previous deactivation left behind.
        if register_understanding(reader.clone()).is_err() {
            replace_understanding(reader)?;
        }

        // The inverse of installing a live reader is NOT removing it — an
        // absent adapter is silently nonexistent, the exact hole this plane
        // documents against. It is a tombstone whose readiness carries the
        // withdrawal reason into the existing `skipped` reporting, verbatim.
        let watch = ctx.watch(VISION_ENDPOINT_KEY);
        ctx.effect(move || {
            let reason = watch
                .park_reason()
                .unwrap_or_else(|| "the vision endpoint was withdrawn".to_string());
            if let Err(error) = replace_understanding(Arc::new(ParkedVision { reason })) {
                tracing::warn!(%error, "could not park the vision reader in the registry");
            }
        });
        Ok(Activation::Active)
    }
}

/// What stands in the registry while the endpoint key is withdrawn. Never
/// `Ready`, so escalation skips it — cheaply, with the reason — instead of
/// timing out against a dead endpoint once per damaged page.
struct ParkedVision {
    reason: String,
}

#[async_trait]
impl super::DocumentUnderstanding for ParkedVision {
    fn id(&self) -> &'static str {
        // The SAME id as the live reader: this is that reader's parked state,
        // not a second adapter.
        "vision"
    }
    fn media_types(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn modality(&self) -> super::Modality {
        super::Modality::Vision
    }
    fn readiness(&self) -> Readiness {
        Readiness::Unavailable(format!(
            "parked — waiting for {VISION_ENDPOINT_KEY}: {}",
            self.reason
        ))
    }
    async fn understand(&self, doc: &super::SourceDocument<'_>) -> Result<super::Understanding> {
        // Reachable only through `Policy::Only("vision")`, which promises no
        // substitution — so the honest answer is the same sentence readiness
        // gives, as an error.
        anyhow::bail!(
            "the vision reader is parked while reading {}: waiting for {VISION_ENDPOINT_KEY} ({})",
            doc.label,
            self.reason
        )
    }
}

/// Consecutive ENDPOINT-side read failures, with no success between them,
/// that withdraw the endpoint key. Local failures (rendering, cropping)
/// neither count nor reset — they are not evidence in either direction.
///
/// Why 3: every vision read already carries the reader's own transient
/// handling — `prism_runtime::retry::retrying` replays 429/503/refused
/// connections with backoff, under the operator's own `timeout_secs` — so
/// one failure that survived all of that is already persistent-for-one-read.
/// But one read can still die to a mid-read crash or a server restart, and
/// parking on it re-muzzles a working endpoint (round one's bug). Three
/// consecutive documents failing with zero successes between them is
/// endpoint-shaped, not page-shaped, and it caps what a dead endpoint can
/// cost at three reads' worth of the budget the operator already accepted
/// per read — not one payment per file (round two's 400-file bill). Any
/// successful vision read resets the count to zero.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// First trial delay after a withdrawal, and the ceiling the schedule
/// doubles up to. A trial is one REAL read, costing at most what the
/// operator already accepted per read — so the first chance comes quickly
/// (an endpoint that blipped is usually back in a minute) and the ceiling
/// keeps a dead weekend at dozens of trials, not thousands.
const TRIAL_MIN_DELAY: Duration = Duration::from_secs(60);
const TRIAL_MAX_DELAY: Duration = Duration::from_secs(900);

/// The breaker's half-open state: when the next real read may be let
/// through, and how far the schedule has backed off.
struct Trial {
    delay: Duration,
    next_at: Instant,
}

impl Trial {
    fn new() -> Self {
        Self {
            delay: TRIAL_MIN_DELAY,
            next_at: Instant::now() + TRIAL_MIN_DELAY,
        }
    }

    /// Exponential, capped. Written out by hand because this is a schedule
    /// consulted across documents, not a retry loop around one future
    /// (which `prism_runtime::retry` owns).
    fn back_off(&mut self) {
        self.delay = (self.delay * 2).min(TRIAL_MAX_DELAY);
        self.next_at = Instant::now() + self.delay;
    }

    fn due(&self) -> bool {
        Instant::now() >= self.next_at
    }
}

/// The vision seam for one ingest run: the supervising [`Runtime`] plus the
/// health signal that drives it — the live reader's own results.
///
/// One value for the whole run (audit F3): one reader activation, one
/// tombstone inverse, and a mid-corpus endpoint death remembered across
/// files instead of each file rebuilding a runtime that forgot the last
/// file's discovery.
pub struct VisionSeam {
    runtime: Runtime,
    /// Re-published for a trial read after a withdrawal (the withdrawal
    /// removes the board's copy).
    endpoint: VisionEndpoint,
    /// Live-reader read failures with no success between them.
    consecutive_failures: u32,
    /// `Some` from the first withdrawal until a trial read succeeds.
    trial: Option<Trial>,
}

impl VisionSeam {
    /// Load the reader component, publish the endpoint key, and require the
    /// activation to have actually happened. No host is contacted and no
    /// credential leaves the machine here — activation only constructs the
    /// reader; the first network traffic is the first page that needs it.
    pub async fn start(cfg: prism_llm::LlmConfig) -> Result<Self> {
        Self::with_rasteriser(cfg, Arc::new(CommandRasteriser::poppler())).await
    }

    async fn with_rasteriser(
        cfg: prism_llm::LlmConfig,
        rasteriser: Arc<dyn PageRasteriser>,
    ) -> Result<Self> {
        let endpoint = VisionEndpoint { cfg };
        let mut runtime = Runtime::new();
        runtime.load(Box::new(VisionReaderComponent::new(rasteriser)))?;
        runtime
            .provide(VISION_ENDPOINT_KEY, endpoint.clone())
            .await?;
        // `settle` swallows an activation `Err` into fiber status by design
        // (other components should keep settling), so the one wiring the CLI
        // ships must check the state it actually reached: an Inactive fiber
        // here means NO reader was installed and NO tombstone will ever run
        // — the silent flat-text outcome this seam exists to prevent (audit
        // F11). The caller falls back to direct registration.
        let status = runtime.status();
        match status.first() {
            Some(fiber) if fiber.state == FiberState::Active => Ok(Self {
                runtime,
                endpoint,
                consecutive_failures: 0,
                trial: None,
            }),
            other => anyhow::bail!(
                "the vision reader component did not activate: {}",
                other
                    .map(|fiber| fiber.to_string())
                    .unwrap_or_else(|| "no component loaded".to_string()),
            ),
        }
    }

    /// The operator's view, for the deferral decision and status surfaces.
    pub fn status(&self) -> Vec<FiberStatus> {
        self.runtime.status()
    }

    /// Park the reader now, with the reason an operator will read. The seam
    /// derives the teardown: the tombstone swap registered at activation
    /// runs before the key is gone, and a trial read is scheduled on the
    /// bounded backoff.
    pub async fn withdraw(&mut self, reason: &str) {
        self.runtime.withdraw(&VISION_ENDPOINT_KEY, reason).await;
        match &mut self.trial {
            Some(trial) => trial.back_off(),
            None => self.trial = Some(Trial::new()),
        }
    }

    /// Give a withdrawn endpoint its recovery chance: once the backoff has
    /// elapsed, re-provide the key so the NEXT document's read runs against
    /// the live reader again. That read is the trial — same auth, same
    /// timeouts, same retry policy as every read, because it IS one; there
    /// is no separate probe to disagree with it. [`Self::observe`] settles
    /// the verdict: a success closes the breaker, a failure re-withdraws at
    /// once and backs the schedule off further.
    pub async fn retry_endpoint_if_due(&mut self) {
        let Some(trial) = &self.trial else {
            return;
        };
        if !trial.due() {
            return;
        }
        if self.runtime.watch(VISION_ENDPOINT_KEY).is_provided() {
            // A trial is already in flight: the key was re-provided and no
            // vision verdict has arrived yet (the last document may simply
            // not have needed vision).
            return;
        }
        if let Err(error) = self
            .runtime
            .provide(VISION_ENDPOINT_KEY, self.endpoint.clone())
            .await
        {
            // Unreachable (the key was just checked absent), but a seam bug
            // must be loud, not a silently skipped recovery.
            tracing::warn!(%error, "could not re-provide the vision endpoint for a trial read");
        }
    }

    /// Read the health signal out of one document's completed read.
    ///
    /// A vision read that returned closes the breaker (whether its output
    /// rescued the page is a per-page report, not a health question). A
    /// vision read that failed ON THE ENDPOINT SIDE
    /// (`SkipKind::Failed(FailureOrigin::Remote)`) counts toward
    /// [`MAX_CONSECUTIVE_FAILURES`] — or, during a trial, re-withdraws
    /// immediately: the breaker is half-open there, not reset. A LOCAL
    /// failure — the rasteriser dying on this file — neither counts nor
    /// resets: it is not evidence about the endpoint in either direction,
    /// and a run of encrypted PDFs must not park a healthy host. A document
    /// that never invoked vision says nothing either way. Returns the
    /// withdrawal reason when this observation parked the reader.
    pub async fn observe(&mut self, outcome: &ReadOutcome) -> Option<String> {
        if outcome
            .understanding
            .adapter_id
            .split('+')
            .any(|id| id == "vision")
        {
            self.consecutive_failures = 0;
            self.trial = None;
            return None;
        }
        let failure = outcome.skipped.iter().rev().find(|note| {
            note.adapter_id == "vision" && note.kind == SkipKind::Failed(FailureOrigin::Remote)
        })?;
        self.consecutive_failures += 1;
        if self.trial.is_none() && self.consecutive_failures < MAX_CONSECUTIVE_FAILURES {
            return None;
        }
        let reason = format!(
            "the vision reader failed {} consecutive read(s); last: {}",
            self.consecutive_failures, failure.reason
        );
        self.withdraw(&reason).await;
        Some(reason)
    }

    /// End of run: run the component's inverses NOW, explicitly. `Runtime`
    /// has no `Drop` by design, so dropping the slot with the fiber Active
    /// would leave the tombstone inverse unrun and a stale reader registered
    /// process-wide (watch mode, the TUI, a test binary). The withdrawal
    /// carries an honest reason for whatever reads the registry next; the
    /// next run's activation displaces the tombstone with a live reader.
    pub async fn retire(mut self) {
        self.runtime
            .withdraw(
                &VISION_ENDPOINT_KEY,
                "the ingest run ended; the next run re-provides the endpoint",
            )
            .await;
        if let Err(error) = self.runtime.retire(VISION_READER_COMPONENT).await {
            tracing::warn!(%error, "could not retire the vision reader component");
        }
    }

    /// Test-only: make the pending trial due now. The schedule's arithmetic
    /// is pinned by its own test; state transitions should not need
    /// wall-clock sleeps.
    #[cfg(test)]
    fn force_trial_due(&mut self) {
        if let Some(trial) = &mut self.trial {
            trial.next_at = Instant::now();
        }
    }
}

/// The consumer-side deferral decision: which sentence explains the pages of
/// this document that still owe a vision read?
///
/// `Some` when pages remain unrecovered AND vision never actually saw them
/// for a reason a re-run against a working endpoint can fix: either the
/// reader is parked waiting for [`VISION_ENDPOINT_KEY`], or the live
/// reader's ENDPOINT call failed (a `SkipKind::Failed(FailureOrigin::
/// Remote)` note) — the below-breaker window, where the endpoint is not
/// yet condemned but these pages were still never read; without this arm
/// the fully-scanned PDF whose one vision read failed would be an error
/// instead of a deferral, and the document that most needs vision would be
/// the only one excluded from vision recovery. The caller records the
/// deferral in the ingest summary and PROCEEDS — sound pages are extracted
/// and stored now (audit F2); nothing is discarded.
///
/// Everything else keeps today's behaviour: a sound document ingests
/// without vision; a reader that RAN and answered but could not fix a page
/// is honest degradation (reported, not deferred); a read that failed
/// LOCALLY — poppler dying on this PDF — is likewise degradation, because
/// no amount of waiting on the endpoint renders an encrypted file; and a
/// reader inactive for any *other* reason (no rasteriser, an activation
/// bug) must not blame a key that waiting cannot fix.
pub fn vision_deferral_reason(status: &[FiberStatus], outcome: &ReadOutcome) -> Option<String> {
    let unrecovered: Vec<String> = outcome
        .unrecovered()
        .map(|note| format!("p{} ({})", note.number, note.damage))
        .collect();
    if unrecovered.is_empty() {
        return None;
    }
    let fiber = status
        .iter()
        .find(|fiber| fiber.name == VISION_READER_COMPONENT)?;
    if matches!(fiber.state, FiberState::Active) {
        let failure = outcome.skipped.iter().rev().find(|note| {
            note.adapter_id == "vision" && note.kind == SkipKind::Failed(FailureOrigin::Remote)
        })?;
        return Some(format!(
            "{} page(s) still need the vision reader, whose read failed \
             ({}); pages: {} — these pages are deferred, the rest of this \
             document is ingested now, and a re-run once the endpoint \
             answers reads them fully.",
            unrecovered.len(),
            failure.reason,
            unrecovered.join(", "),
        ));
    }
    if !fiber.waiting_on.contains(&VISION_ENDPOINT_KEY) {
        return None;
    }
    let why = if fiber.park_reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", fiber.park_reasons.join("; "))
    };
    Some(format!(
        "{} page(s) still need the vision reader, which is parked waiting \
         for {VISION_ENDPOINT_KEY}{why}; pages: {} — these pages are \
         deferred, the rest of this document is ingested now, and a re-run \
         once the endpoint answers reads them fully.",
        unrecovered.len(),
        unrecovered.join(", "),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::{
        Damage, DocumentUnderstanding, Modality, PageNote, PageText, SkipNote, SourceDocument,
        Understanding, registry,
    };
    use super::*;

    /// Removes the "vision" adapter the component installed, so this file's
    /// tests cannot poison the process-wide registry other tests assert
    /// against (`policy_only_never_substitutes_a_different_reader` requires
    /// "vision" to be absent).
    struct RestoreRegistry;
    impl Drop for RestoreRegistry {
        fn drop(&mut self) {
            let _ = super::super::deregister_understanding("vision");
        }
    }

    struct StubRasteriser;
    impl PageRasteriser for StubRasteriser {
        fn id(&self) -> &'static str {
            "stub"
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        fn render(&self, _pdf: &[u8], _page: u32, _dpi: u32) -> Result<Vec<u8>> {
            anyhow::bail!("the stub never renders")
        }
    }

    fn endpoint_cfg() -> prism_llm::LlmConfig {
        prism_llm::LlmConfig {
            // TEST-NET-1: provably never contacted — these tests perform no
            // network read, and any contact would hang on an unroutable host.
            base_url: "http://192.0.2.1:1/v1".into(),
            model: "stub-vlm".into(),
            ..Default::default()
        }
    }

    fn endpoint() -> VisionEndpoint {
        VisionEndpoint {
            cfg: endpoint_cfg(),
        }
    }

    fn damaged_outcome() -> ReadOutcome {
        ReadOutcome {
            understanding: Understanding {
                adapter_id: "text-layer".into(),
                modality: Modality::TextLayer,
                pages: vec![PageText {
                    number: 2,
                    text: String::new(),
                }],
            },
            notes: vec![PageNote {
                number: 2,
                damage: Damage::new("empty", "no text recovered"),
                recovered_by: None,
            }],
            skipped: Vec::new(),
        }
    }

    /// A read in which the live reader RAN and its ENDPOINT call failed —
    /// the note shape [`escalate`](super::super::read) produces for a
    /// failed adapter whose error carries [`super::super::EndpointCall`],
    /// pinned against production by
    /// `a_failing_adapter_produces_a_failed_skip_note` in the plane's own
    /// tests.
    fn failed_vision_outcome() -> ReadOutcome {
        let mut outcome = damaged_outcome();
        outcome.skipped.push(SkipNote {
            adapter_id: "vision".into(),
            reason: "reading page 2 tile r0c0: connection refused".into(),
            kind: SkipKind::Failed(FailureOrigin::Remote),
        });
        outcome
    }

    /// A read in which the live reader ran and failed BEFORE anything left
    /// the machine — poppler dying on this file. Same production note
    /// shape, opposite side of the wire.
    fn locally_failed_vision_outcome() -> ReadOutcome {
        let mut outcome = damaged_outcome();
        outcome.skipped.push(SkipNote {
            adapter_id: "vision".into(),
            reason: "rendering page 2: pdftoppm failed rendering page 2: encrypted".into(),
            kind: SkipKind::Failed(FailureOrigin::Local),
        });
        outcome
    }

    /// A read in which the live reader ran and ANSWERED (it recovered the
    /// page): the success signal that closes the breaker.
    fn successful_vision_outcome() -> ReadOutcome {
        let mut outcome = damaged_outcome();
        outcome.understanding.adapter_id = "text-layer+vision".into();
        outcome.notes[0].recovered_by = Some("vision".into());
        outcome
    }

    /// The key's whole life, through the PRODUCTION seam and the PRODUCTION
    /// process-wide registry: absent → the reader parks and the registry is
    /// untouched; provided → the live reader is installed; withdrawn → the
    /// tombstone stands with the withdrawal reason and the damaged document's
    /// pages defer; provided again → the live reader is back.
    #[tokio::test]
    async fn the_vision_endpoint_key_flows_through_the_seam_end_to_end() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let _restore = RestoreRegistry;

        let mut rt = Runtime::new();
        rt.load(Box::new(VisionReaderComponent::new(Arc::new(
            StubRasteriser,
        ))))
        .unwrap();

        // No key yet: parked, and the registry must be UNTOUCHED — a parked
        // component has performed no effect there is anything to undo.
        rt.settle().await;
        let status = rt.status();
        assert_eq!(status[0].state, FiberState::Inactive);
        assert_eq!(status[0].waiting_on, vec![VISION_ENDPOINT_KEY]);
        assert!(
            registry().get("vision").is_none(),
            "a parked component must not have touched the registry",
        );
        // A damaged document's pages DEFER while the reader waits for the key.
        assert!(
            vision_deferral_reason(&status, &damaged_outcome())
                .is_some_and(|reason| reason.contains("llm.vision.endpoint")),
            "damaged pages must defer, naming the key",
        );

        // Key provided: the live reader is installed.
        rt.provide(VISION_ENDPOINT_KEY, endpoint()).await.unwrap();
        assert_eq!(rt.status()[0].state, FiberState::Active);
        let installed = registry().get("vision").expect("reader installed");
        assert!(installed.readiness().is_ready(), "live reader is ready");
        assert!(
            vision_deferral_reason(&rt.status(), &damaged_outcome()).is_none(),
            "with the reader active and no failed read, degradation is \
             reported, never deferred",
        );

        // Endpoint confirmed dead: the runtime-derived inverse swaps in the
        // tombstone, which carries the reason into `skipped`.
        rt.withdraw(
            &VISION_ENDPOINT_KEY,
            "the vision reader failed 3 consecutive read(s)",
        )
        .await;
        assert_eq!(rt.status()[0].state, FiberState::Reloading);
        let parked = registry().get("vision").expect("tombstone installed");
        match parked.readiness() {
            Readiness::Unavailable(reason) => {
                assert!(reason.contains("llm.vision.endpoint"), "{reason}");
                assert!(reason.contains("3 consecutive"), "{reason}");
            }
            Readiness::Ready => panic!("a parked reader must never report Ready"),
        }
        let reason = vision_deferral_reason(&rt.status(), &damaged_outcome())
            .expect("damaged pages must defer while the key is withdrawn");
        assert!(reason.contains("3 consecutive"), "{reason}");
        assert!(
            reason.contains("p2"),
            "the deferred pages are named: {reason}"
        );
        assert!(
            reason.contains("deferred") && reason.contains("ingested now"),
            "the reason must promise deferral, not discard (audit F2): {reason}",
        );

        // The endpoint returns: the live reader displaces the tombstone.
        rt.provide(VISION_ENDPOINT_KEY, endpoint()).await.unwrap();
        assert_eq!(rt.status()[0].state, FiberState::Active);
        assert!(
            registry()
                .get("vision")
                .expect("reader reinstalled")
                .readiness()
                .is_ready(),
            "reactivation must reinstall the LIVE reader",
        );
    }

    /// The deferral decision gates ONLY work that owes a vision read. A
    /// sound document, an honest post-vision degradation, and a reader
    /// inactive for a non-key reason all keep today's behaviour; a FAILED
    /// vision read defers even while the reader is still Active (the
    /// below-breaker window — those pages were never actually seen).
    #[tokio::test]
    async fn deferral_gates_only_vision_dependent_work() {
        let parked = FiberStatus {
            name: VISION_READER_COMPONENT.into(),
            state: FiberState::Reloading,
            waiting_on: vec![VISION_ENDPOINT_KEY],
            park_reasons: vec!["llm.vision.endpoint: the vision reader failed".into()],
            blocked_by_cycle: false,
            last_error: None,
        };
        let active = FiberStatus {
            name: VISION_READER_COMPONENT.into(),
            state: FiberState::Active,
            waiting_on: Vec::new(),
            park_reasons: Vec::new(),
            blocked_by_cycle: false,
            last_error: None,
        };

        // A clean document never defers, whatever the reader's state.
        let clean = ReadOutcome {
            understanding: Understanding {
                adapter_id: "text-layer".into(),
                modality: Modality::TextLayer,
                pages: vec![PageText {
                    number: 1,
                    text: "sound body text".into(),
                }],
            },
            notes: Vec::new(),
            skipped: Vec::new(),
        };
        assert!(vision_deferral_reason(std::slice::from_ref(&parked), &clean).is_none());

        // A damaged document's pages defer while the reader waits for the key…
        assert!(
            vision_deferral_reason(std::slice::from_ref(&parked), &damaged_outcome()).is_some()
        );

        // …and while the reader is ACTIVE but this read failed on the
        // ENDPOINT side (arm 2): the pages were never seen for a reason a
        // re-run against an answering endpoint can fix, so they are
        // deferred with that failure.
        let deferral =
            vision_deferral_reason(std::slice::from_ref(&active), &failed_vision_outcome())
                .expect("a failed endpoint call defers the pages it was asked for");
        assert!(deferral.contains("connection refused"), "{deferral}");
        assert!(deferral.contains("ingested now"), "{deferral}");

        // …but NOT when the read failed LOCALLY: no amount of waiting on
        // the endpoint renders an encrypted PDF, so promising "a re-run
        // once the endpoint answers reads them fully" would be a lie. That
        // page is honest degradation, reported by the caller's warning.
        assert!(
            vision_deferral_reason(
                std::slice::from_ref(&active),
                &locally_failed_vision_outcome()
            )
            .is_none(),
            "a local failure must degrade honestly, never defer behind the endpoint",
        );

        // …nor when the reader ran and answered (honest degradation)…
        assert!(
            vision_deferral_reason(&[active], &damaged_outcome()).is_none(),
            "a reader that answered and could not fix a page is a per-page \
             report, not a deferral",
        );

        // …nor when the reader is inactive for a reason waiting cannot fix
        // (activation failed; it is not waiting on the key).
        let broken = FiberStatus {
            name: VISION_READER_COMPONENT.into(),
            state: FiberState::Inactive,
            waiting_on: Vec::new(),
            park_reasons: Vec::new(),
            blocked_by_cycle: false,
            last_error: Some("no rasteriser installed".into()),
        };
        assert!(
            vision_deferral_reason(&[broken], &damaged_outcome()).is_none(),
            "a provisioning gap must degrade loudly, not stall forever",
        );

        // …and not when no seam is running at all (no vision configured).
        assert!(vision_deferral_reason(&[], &damaged_outcome()).is_none());
    }

    /// The breaker: three consecutive ENDPOINT-side failures withdraw the
    /// key and install the tombstone; any successful read resets the count,
    /// so a flaky-but-working endpoint is never parked; and a LOCAL failure
    /// neither counts nor resets — three encrypted PDFs in a row are three
    /// facts about the corpus, not one about the host. No probe exists to
    /// be wrong — the reads themselves are the evidence.
    #[tokio::test]
    async fn three_consecutive_failed_reads_trip_the_breaker_and_a_success_resets_it() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let _restore = RestoreRegistry;

        let mut seam = VisionSeam::with_rasteriser(endpoint_cfg(), Arc::new(StubRasteriser))
            .await
            .expect("the seam starts");
        assert_eq!(seam.status()[0].state, FiberState::Active);

        // Two failures, then a success: the count resets, nothing parks.
        assert!(seam.observe(&failed_vision_outcome()).await.is_none());
        assert!(seam.observe(&failed_vision_outcome()).await.is_none());
        assert!(seam.observe(&successful_vision_outcome()).await.is_none());
        assert_eq!(
            seam.status()[0].state,
            FiberState::Active,
            "a success between failures must keep the reader live",
        );

        // Local failures alone can NEVER park the reader, however many:
        // the corpus's broken files are not the endpoint's health.
        for _ in 0..2 * MAX_CONSECUTIVE_FAILURES {
            assert!(
                seam.observe(&locally_failed_vision_outcome())
                    .await
                    .is_none()
            );
        }
        assert_eq!(
            seam.status()[0].state,
            FiberState::Active,
            "a run of local failures must not trip the breaker",
        );
        assert_eq!(
            seam.consecutive_failures, 0,
            "local failures are not strikes"
        );

        // Three consecutive remote failures — with a local failure in the
        // middle, which neither counts nor resets: withdrawn, tombstone
        // installed.
        assert!(seam.observe(&failed_vision_outcome()).await.is_none());
        assert!(seam.observe(&failed_vision_outcome()).await.is_none());
        assert!(
            seam.observe(&locally_failed_vision_outcome())
                .await
                .is_none()
        );
        let reason = seam
            .observe(&failed_vision_outcome())
            .await
            .expect("the third consecutive endpoint-side failure must park the reader");
        assert!(reason.contains("3 consecutive"), "{reason}");
        assert!(reason.contains("connection refused"), "{reason}");
        assert_eq!(seam.status()[0].state, FiberState::Reloading);
        assert!(
            !registry()
                .get("vision")
                .expect("tombstone installed")
                .readiness()
                .is_ready(),
            "the tombstone must stand after the breaker trips",
        );
        // And the deferral decision now names the breaker's reason.
        assert!(
            vision_deferral_reason(&seam.status(), &damaged_outcome())
                .is_some_and(|deferral| deferral.contains("3 consecutive")),
        );
    }

    /// Recovery is a REAL read on a bounded backoff: the key is re-provided
    /// when the trial is due (reinstalling the live reader), a failing trial
    /// re-withdraws immediately (half-open, not reset) and backs the
    /// schedule off, and a succeeding trial closes the breaker fully.
    #[tokio::test]
    async fn a_withdrawn_endpoint_earns_real_trial_reads_on_a_bounded_backoff() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let _restore = RestoreRegistry;

        let mut seam = VisionSeam::with_rasteriser(endpoint_cfg(), Arc::new(StubRasteriser))
            .await
            .expect("the seam starts");
        for _ in 0..3 {
            seam.observe(&failed_vision_outcome()).await;
        }
        assert!(
            !seam.runtime.watch(VISION_ENDPOINT_KEY).is_provided(),
            "the breaker has tripped",
        );

        // Not due yet: the key stays withdrawn — no per-file re-billing.
        seam.retry_endpoint_if_due().await;
        assert!(
            !seam.runtime.watch(VISION_ENDPOINT_KEY).is_provided(),
            "a trial before its backoff elapses must not re-provide the key",
        );

        // Due: the key returns and the LIVE reader is reinstalled for one
        // real read.
        seam.force_trial_due();
        seam.retry_endpoint_if_due().await;
        assert_eq!(seam.status()[0].state, FiberState::Active);
        assert!(
            registry()
                .get("vision")
                .expect("reader reinstalled for the trial")
                .readiness()
                .is_ready(),
        );

        // The trial fails: re-withdrawn at once (half-open, not a fresh
        // count of three) and the schedule backs off.
        let reason = seam
            .observe(&failed_vision_outcome())
            .await
            .expect("a failing trial must re-park immediately");
        assert!(reason.contains("connection refused"), "{reason}");
        assert_eq!(seam.status()[0].state, FiberState::Reloading);
        seam.retry_endpoint_if_due().await;
        assert!(
            !seam.runtime.watch(VISION_ENDPOINT_KEY).is_provided(),
            "the failed trial must have pushed the next one out",
        );

        // A later trial succeeds: the breaker closes fully — and a single
        // later failure is one strike again, not an instant park.
        seam.force_trial_due();
        seam.retry_endpoint_if_due().await;
        assert!(seam.observe(&successful_vision_outcome()).await.is_none());
        assert!(
            seam.trial.is_none(),
            "a successful trial closes the breaker"
        );
        assert_eq!(seam.consecutive_failures, 0);
        assert!(
            seam.observe(&failed_vision_outcome()).await.is_none(),
            "after recovery one failure must not park the reader",
        );
        assert_eq!(seam.status()[0].state, FiberState::Active);
    }

    /// The trial schedule: starts at the minimum, doubles per failed trial,
    /// and caps at the ceiling — bounded on both ends by construction.
    #[test]
    fn the_trial_schedule_doubles_and_caps() {
        let mut trial = Trial::new();
        assert_eq!(trial.delay, TRIAL_MIN_DELAY);
        trial.back_off();
        assert_eq!(trial.delay, TRIAL_MIN_DELAY * 2);
        for _ in 0..16 {
            trial.back_off();
        }
        assert_eq!(
            trial.delay, TRIAL_MAX_DELAY,
            "the schedule must cap, not grow unboundedly",
        );
        assert!(!trial.due(), "a freshly backed-off trial is never due");
    }

    /// End of run: `retire` runs the tombstone inverse explicitly, with a
    /// reason that names the run's end — no stale live reader left in the
    /// process-wide registry for the next surface to trust.
    #[tokio::test]
    async fn retiring_the_seam_runs_the_tombstone_inverse() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let _restore = RestoreRegistry;

        let seam = VisionSeam::with_rasteriser(endpoint_cfg(), Arc::new(StubRasteriser))
            .await
            .expect("the seam starts");
        assert!(
            registry()
                .get("vision")
                .expect("live reader installed")
                .readiness()
                .is_ready(),
        );

        seam.retire().await;
        match registry()
            .get("vision")
            .expect("the tombstone stands after retirement")
            .readiness()
        {
            Readiness::Unavailable(reason) => {
                assert!(reason.contains("run ended"), "{reason}");
            }
            Readiness::Ready => {
                panic!("retiring the run must not leave a stale live reader registered")
            }
        }
    }

    /// A first-tier reader that succeeds but leaves page 2 damaged, so
    /// escalation asks the real vision reader for it.
    struct DamagedFirst;
    #[async_trait]
    impl DocumentUnderstanding for DamagedFirst {
        fn id(&self) -> &'static str {
            "text-layer"
        }
        fn media_types(&self) -> &'static [&'static str] {
            &["pdf"]
        }
        fn modality(&self) -> Modality {
            Modality::TextLayer
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        async fn understand(&self, _doc: &SourceDocument<'_>) -> Result<Understanding> {
            Ok(Understanding {
                adapter_id: "text-layer".into(),
                modality: Modality::TextLayer,
                pages: vec![PageText {
                    number: 2,
                    text: String::new(),
                }],
            })
        }
    }

    /// A rasteriser that renders every page — so the only thing that can
    /// fail downstream of it is the endpoint call itself.
    struct PngRasteriser;
    impl PageRasteriser for PngRasteriser {
        fn id(&self) -> &'static str {
            "png-stub"
        }
        fn readiness(&self) -> Readiness {
            Readiness::Ready
        }
        fn render(&self, _pdf: &[u8], _page: u32, _dpi: u32) -> Result<Vec<u8>> {
            let img = image::RgbImage::from_pixel(64, 48, image::Rgb([200, 200, 200]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgb8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .expect("encode stub page");
            Ok(out)
        }
    }

    /// The seam consumes the notes escalation actually produces: the REAL
    /// `escalate` (the same private function `read` dispatches to), given
    /// the component-installed live reader whose ENDPOINT call genuinely
    /// fails (an answering host returning 400 to every read), records a
    /// `Failed(Remote)` note — and that note, unmodified, trips the
    /// breaker. This is the cross-module pin: change what escalation
    /// records (its kind, its id, its origin, its construction) and this
    /// goes red here, not in a distant corpus run.
    #[tokio::test]
    async fn escalations_own_failure_notes_drive_the_breaker() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let _restore = RestoreRegistry;

        // An ANSWERING host that fails every read: the round-three reversal
        // in the module docs, driven live — 400s bill per file exactly like
        // a dead host, so they park the reader all the same.
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(400).set_body_string("no vision-capable model is loaded"),
            )
            .mount(&server)
            .await;
        let cfg = prism_llm::LlmConfig {
            base_url: format!("{}/v1", server.uri()),
            model: "stub-vlm".into(),
            ..Default::default()
        };

        let mut seam = VisionSeam::with_rasteriser(cfg, Arc::new(PngRasteriser))
            .await
            .expect("the seam starts");
        let reader = registry().get("vision").expect("live reader installed");
        let candidates: Vec<Arc<dyn DocumentUnderstanding>> = vec![Arc::new(DamagedFirst), reader];
        let doc = SourceDocument::whole(b"%PDF-1.7", "pdf", "x.pdf");

        let mut last_reason = None;
        for _ in 0..3 {
            let outcome =
                super::super::escalate(&doc, &candidates, &super::super::DamagePolicy::default())
                    .await
                    .expect("a later adapter failing is not fatal to the read");
            assert!(
                outcome.skipped.iter().any(|note| {
                    note.adapter_id == "vision"
                        && note.kind == SkipKind::Failed(FailureOrigin::Remote)
                }),
                "escalation must record the endpoint call's failure as remote: {:?}",
                outcome.skipped,
            );
            last_reason = seam.observe(&outcome).await;
        }
        let reason = last_reason.expect("three real endpoint failures must trip the breaker");
        assert!(
            reason.contains("400"),
            "the endpoint's own answer reaches the park reason: {reason}",
        );
        assert_eq!(seam.status()[0].state, FiberState::Reloading);
    }

    /// The other side of the same pin: the REAL `escalate`, given the
    /// component-installed live reader whose RASTERISER fails — poppler
    /// crashing on this file, no network anywhere — records a
    /// `Failed(Local)` note, and however many of those arrive, the breaker
    /// never trips and nothing is deferred. This is the measured round-four
    /// bug: three encrypted PDFs used to park a healthy endpoint for the
    /// rest of the corpus.
    #[tokio::test]
    async fn a_rasteriser_failure_through_real_escalation_never_trips_the_breaker() {
        let _guard = crate::connectors::connector::GLOBAL_REGISTRY_TEST_LOCK
            .lock()
            .await;
        let _restore = RestoreRegistry;

        let mut seam = VisionSeam::with_rasteriser(endpoint_cfg(), Arc::new(StubRasteriser))
            .await
            .expect("the seam starts");
        let reader = registry().get("vision").expect("live reader installed");
        let candidates: Vec<Arc<dyn DocumentUnderstanding>> = vec![Arc::new(DamagedFirst), reader];
        let doc = SourceDocument::whole(b"%PDF-1.7", "pdf", "x.pdf");

        for _ in 0..3 {
            let outcome =
                super::super::escalate(&doc, &candidates, &super::super::DamagePolicy::default())
                    .await
                    .expect("a later adapter failing is not fatal to the read");
            assert!(
                outcome.skipped.iter().any(|note| {
                    note.adapter_id == "vision"
                        && note.kind == SkipKind::Failed(FailureOrigin::Local)
                }),
                "a render failure must be recorded on the local side: {:?}",
                outcome.skipped,
            );
            assert!(
                seam.observe(&outcome).await.is_none(),
                "a local failure must never count against the endpoint",
            );
            assert!(
                vision_deferral_reason(&seam.status(), &outcome).is_none(),
                "a local failure is honest degradation, never a deferral",
            );
        }
        assert_eq!(
            seam.status()[0].state,
            FiberState::Active,
            "three local failures must leave the reader live",
        );
        assert!(
            registry()
                .get("vision")
                .expect("reader still installed")
                .readiness()
                .is_ready(),
            "no tombstone may stand over a healthy endpoint",
        );
    }
}
