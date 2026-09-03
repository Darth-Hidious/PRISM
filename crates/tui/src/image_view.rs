//! Drawing an artifact where the human is already looking.
//!
//! The notebook used to render `[plot saved: /path/cell-1-0.png]` and stop
//! there. A path is not a picture: the figure existed, the human could not see
//! it, and nothing in the pane said whether the plot was right or empty or
//! upside down. This renders it as a real ratatui widget instead.
//!
//! It is deliberately a WIDGET and not a second process. `ratatui-image` takes
//! a [`Rect`] like any other widget, so PRISM keeps ONE layout engine — the
//! notebook pane cannot be drawn over by something that does not know where it
//! is, which is exactly the failure recorded in `tui-3-still-broken.png`.
//!
//! Terminal support is discovered, never assumed: Kitty, Sixel and iTerm2 are
//! used when the terminal answers for them, and halfblocks — coloured Unicode
//! blocks, no graphics protocol at all — is the floor. The floor still shows
//! the picture. There is no configuration in which this silently shows nothing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::{Resize, StatefulImage, protocol::StatefulProtocol};

/// Why an artifact could not be drawn. Each variant is a distinct, reportable
/// cause — never an empty pane standing in for an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDrawError {
    /// The file is not on disk (deleted, or the kernel reported a path it
    /// never wrote).
    Missing(String),
    /// On disk but not decodable as an image.
    Undecodable(String),
}

impl ImageDrawError {
    /// One line, for rendering where the picture would have been. Says what
    /// failed and names the path, so the reader is never left wondering why
    /// the pane is blank.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Missing(path) => format!("[figure missing on disk: {path}]"),
            Self::Undecodable(reason) => format!("[figure could not be decoded: {reason}]"),
        }
    }
}

/// Times [`ImageView::detect`] has queried the terminal this process.
static DETECTIONS: AtomicUsize = AtomicUsize::new(0);
/// How many times the graphics query — the one that spawns a stdin-reading
/// thread — has actually been sent. See [`ImageView::queries`].
static QUERIES: AtomicUsize = AtomicUsize::new(0);

/// Whether PRISM is running inside tmux or GNU screen.
///
/// Both refuse to forward a terminal graphics query unless passthrough is
/// configured, so asking is a guaranteed 2s stall plus an orphaned thread that
/// disables raw mode after the fact.
fn in_multiplexer() -> bool {
    multiplexer_from(
        std::env::var_os("TMUX").is_some(),
        std::env::var_os("STY").is_some(),
        std::env::var("TERM").ok().as_deref(),
    )
}

/// The rule itself, taking its inputs rather than reading the environment, so
/// it can be tested without racing every other test for the process env.
fn multiplexer_from(tmux: bool, sty: bool, term: Option<&str>) -> bool {
    tmux || sty || term.is_some_and(|t| t.starts_with("screen") || t.starts_with("tmux"))
}

/// Whether this session arrived over ssh.
///
/// A remote terminal is never probed. The graphics query's reader thread
/// blocks in a plain `read` on stdin until the terminal answers the last of
/// its questions (ratatui-image 11.0.6, `picker.rs`, `query_stdio_capabilities`);
/// over a slow link the device-attributes handshake can come back inside
/// crossterm's two seconds while the kitty and cell-size answers are still in
/// flight, and the thread then eats the reader's next keystroke as "the
/// reply". Halfblocks are bounded and honest: the figure still draws, the
/// systems panel says why it is coarse, and no thread ever owns stdin.
fn remote_session() -> bool {
    remote_from(
        std::env::var_os("SSH_CONNECTION").is_some(),
        std::env::var_os("SSH_TTY").is_some(),
    )
}

/// The rule itself, taking its inputs rather than reading the environment.
fn remote_from(ssh_connection: bool, ssh_tty: bool) -> bool {
    ssh_connection || ssh_tty
}

/// What to do about the terminal's graphics: ask it, or settle for
/// halfblocks and say why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Halfblocks, with the reason the terminal was not asked (or answered
    /// nothing) — shown in the systems panel, so a coarse figure is
    /// explainable.
    Halfblocks(&'static str),
    /// Ask the terminal.
    Query,
}

/// The decision, taking its inputs rather than reading the environment, and
/// taking the handshake as a closure so it is paid ONLY when it decides
/// something: a multiplexer and a remote session are settled before it,
/// and never wait its two seconds.
///
/// Inside a multiplexer the query is not just useless, it is harmful: tmux
/// and screen do not forward it, so its reader thread hit the timeout and
/// then turned raw mode off underneath the TUI (measured 2026-08-26). A
/// remote session is not asked for the reason on [`remote_session`]. A
/// terminal that stays silent to the handshake would stay silent to the
/// query too, and leave the same thread parked on stdin.
#[must_use]
pub fn probe_policy(multiplexed: bool, remote: bool, answers: impl FnOnce() -> bool) -> Probe {
    if multiplexed {
        return Probe::Halfblocks("multiplexer — not probed");
    }
    if remote {
        return Probe::Halfblocks("remote session — not probed");
    }
    if !answers() {
        return Probe::Halfblocks("terminal did not answer — not probed");
    }
    Probe::Query
}

/// Proof that raw mode is on.
///
/// Only [`RawModeOn::enable`] makes one, and the handshake and the probe
/// take it by reference, so the compiler holds the order: raw mode first,
/// then anything that reads the terminal's answers. The graphics query's
/// reader thread restores, on its way out, whatever termios it found when
/// it started — found cooked, it dropped a running TUI to cooked mode. And
/// crossterm's handshake disables raw mode on its way out when it found it
/// off. Swapping the two calls in `run_with_config` used to be a one-line
/// change that every test survived; now it does not compile.
pub struct RawModeOn(());

impl RawModeOn {
    /// Turn raw mode on and hand back the proof.
    pub fn enable() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self(()))
    }
}

/// Terminal graphics capability plus a decode cache.
///
/// One per app. [`Picker::from_query_stdio`] talks to the terminal with escape
/// sequences and must run ONCE, before the draw loop owns stdout — querying
/// per frame would both stall rendering and interleave escape sequences with
/// the frame being drawn.
pub struct ImageView {
    picker: Picker,
    /// Why the terminal was not asked, or answered nothing, when it was not
    /// — `None` when the protocol came from the terminal's own answer.
    reason: Option<&'static str>,
    /// Decoded protocol per path. A plot file is written once and then drawn
    /// on every frame while the pane is open; decoding a PNG 60 times a second
    /// would be the most expensive thing in the TUI.
    cache: RefCell<HashMap<String, StatefulProtocol>>,
    /// Paths in the order they were last drawn, oldest first. Evicting needs
    /// to know which entry is coldest, and a `HashMap` has no order.
    order: RefCell<Vec<String>>,
}

/// How many decoded figures to hold.
///
/// A decode is RGBA in memory — roughly 1.2 MB for a default matplotlib
/// figure, more for a large one — and every figure a session ever draws used
/// to stay resident, so a long notebook session grew the TUI by hundreds of
/// megabytes of pictures nobody was looking at any more. This is a cache
/// bound, not a limit on what can be shown: an evicted figure is decoded
/// again the next time it is drawn, and nothing is hidden or refused.
const CACHE_ENTRIES: usize = 16;

impl ImageView {
    /// Ask the terminal what it can draw — but only a terminal that has
    /// already answered one question.
    ///
    /// `terminal_answered` is the verdict of [`ImageView::terminal_answers`],
    /// run by the caller on its own thread before this. Falls back to
    /// halfblocks when the query fails — over a pipe, in CI, or on a terminal
    /// that ignores the query. Halfblocks need no protocol support at all, so
    /// this cannot end in "no image".
    ///
    /// The query is `from_query_stdio`, which spawns a thread that reads
    /// stdin until the terminal answers a Device Status Report and gives up
    /// waiting for that thread after its timeout — but cannot stop it. A
    /// terminal that answers device attributes answers the status report
    /// too, so the thread ends on its own. A terminal that stays silent (a
    /// headless driver, a CI pty) would leave it ORPHANED on stdin: it
    /// swallows the first keystrokes as "the reply", then dies and restores
    /// the termios it saved when it started. Measured 2026-09-02 under the
    /// tui-driver: no key reached the event loop at all, and after the first
    /// one every further key echoed raw across the frame. A silent terminal
    /// is therefore never queried, and the caller turns raw mode on before
    /// asking, so even a thread that outlives a slow answer restores raw.
    ///
    /// The decision is [`probe_policy`], taken lazily: a multiplexer and a
    /// remote session are never asked anything, so they never pay the
    /// handshake's two-second timeout either. `raw` is the proof that raw
    /// mode is already on — see [`RawModeOn`] for why the order is held by
    /// the type and not by a comment.
    #[must_use]
    pub fn detect(raw: &RawModeOn) -> Self {
        DETECTIONS.fetch_add(1, Ordering::Relaxed);
        Self::from_policy(probe_policy(in_multiplexer(), remote_session(), || {
            Self::terminal_answers(raw)
        }))
    }

    /// Carry out a probe decision: halfblocks with the reason it was not
    /// probed, or the graphics query itself.
    #[must_use]
    pub fn from_policy(policy: Probe) -> Self {
        match policy {
            Probe::Halfblocks(reason) => Self::with_reason(Picker::halfblocks(), Some(reason)),
            Probe::Query => {
                QUERIES.fetch_add(1, Ordering::Relaxed);
                match Picker::from_query_stdio() {
                    Ok(picker) => Self::with_reason(picker, None),
                    Err(_) => Self::with_reason(
                        Picker::halfblocks(),
                        Some("terminal did not answer the graphics query"),
                    ),
                }
            }
        }
    }

    /// Whether the terminal answers questions at all.
    ///
    /// One primary-device-attributes round trip on the calling thread,
    /// bounded by crossterm's own timeout, through crossterm's own event
    /// reader — so a keystroke typed meanwhile is kept for the event loop,
    /// not lost. Every real terminal answers; a headless pty that does not
    /// would also never answer the graphics query, and is not asked it.
    ///
    /// Takes the raw-mode proof because crossterm's handshake, finding raw
    /// mode OFF, turns it on for the round trip and off again on the way out
    /// (crossterm 0.28.1, `terminal/sys/unix.rs`, `supports_keyboard_enhancement`).
    /// Called before the TUI's own `enable_raw_mode`, that is harmless;
    /// called after it, that would drop the running TUI to cooked mode with
    /// every test green. The token makes the wrong order fail to compile.
    #[must_use]
    pub fn terminal_answers(_raw: &RawModeOn) -> bool {
        crossterm::terminal::supports_keyboard_enhancement().is_ok()
    }

    /// How many times the graphics query itself has been sent this process.
    ///
    /// The query is the only thing that spawns the stdin-reading thread, so a
    /// test can prove a silent terminal never gets one: the fallback and a
    /// successful query both end in a working `Picker`, and this count is
    /// the only observable difference.
    #[must_use]
    pub fn queries() -> usize {
        QUERIES.load(Ordering::Relaxed)
    }

    /// How many times the terminal has been queried this process.
    ///
    /// Exists so a test can prove the RENDER path never queries: the query
    /// writes to the terminal and reads its reply off stdin, so running it
    /// from inside a frame stalls the draw and takes keystrokes from the event
    /// reader. The count is the only observable difference — the fallback and
    /// a successful query both end in a working `Picker`.
    #[must_use]
    pub fn detections() -> usize {
        DETECTIONS.load(Ordering::Relaxed)
    }

    /// The floor: coloured Unicode blocks, no graphics protocol and no query.
    ///
    /// Used wherever detection has not run — tests, headless callers — because
    /// querying a terminal that is not there costs a two-second timeout and
    /// still ends up here.
    #[must_use]
    pub fn halfblocks() -> Self {
        Self::with_picker(Picker::halfblocks())
    }

    /// Build one with an explicit picker — the seam tests use to exercise
    /// drawing without a terminal to query.
    #[must_use]
    pub fn with_picker(picker: Picker) -> Self {
        Self::with_reason(picker, None)
    }

    /// A picker plus why it is the one in use when the terminal was not the
    /// one that decided.
    #[must_use]
    pub fn with_reason(picker: Picker, reason: Option<&'static str>) -> Self {
        Self {
            picker,
            reason,
            cache: RefCell::new(HashMap::new()),
            order: RefCell::new(Vec::new()),
        }
    }

    /// The protocol actually in use, for the status line. An operator seeing a
    /// blocky figure should be able to find out it is on halfblocks rather
    /// than assume the plot itself is low quality.
    #[must_use]
    pub fn protocol(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    /// True when the terminal answered for a real graphics protocol.
    #[must_use]
    pub fn is_true_graphics(&self) -> bool {
        !matches!(self.picker.protocol_type(), ProtocolType::Halfblocks)
    }

    /// The protocol in use and, when the terminal was not the one that
    /// decided, why — for the systems panel: "halfblocks — remote session,
    /// not probed" explains a coarse figure; "halfblocks" alone does not.
    #[must_use]
    pub fn graphics_note(&self) -> String {
        let protocol = format!("{:?}", self.picker.protocol_type()).to_ascii_lowercase();
        match self.reason {
            Some(reason) => format!("{protocol} — {reason}"),
            None => protocol,
        }
    }

    /// Draw `path` into `area`.
    ///
    /// `Resize::Fit` keeps the aspect ratio: a figure the human compares
    /// against a published one must not be silently stretched.
    pub fn draw(&self, f: &mut Frame, area: Rect, path: &str) -> Result<(), ImageDrawError> {
        if area.width == 0 || area.height == 0 {
            return Ok(());
        }
        let mut cache = self.cache.borrow_mut();
        if !cache.contains_key(path) {
            let source = std::path::Path::new(path);
            if !source.exists() {
                return Err(ImageDrawError::Missing(path.to_string()));
            }
            let decoded = image::ImageReader::open(source)
                .map_err(|e| ImageDrawError::Undecodable(format!("{path}: {e}")))?
                .with_guessed_format()
                .map_err(|e| ImageDrawError::Undecodable(format!("{path}: {e}")))?
                .decode()
                .map_err(|e| ImageDrawError::Undecodable(format!("{path}: {e}")))?;
            cache.insert(path.to_string(), self.picker.new_resize_protocol(decoded));
        }
        // Touch: newest last, so the front of `order` is the coldest entry.
        {
            let mut order = self.order.borrow_mut();
            order.retain(|p| p != path);
            order.push(path.to_string());
            while order.len() > CACHE_ENTRIES {
                let coldest = order.remove(0);
                cache.remove(&coldest);
            }
        }
        let protocol = cache
            .get_mut(path)
            .expect("just inserted when absent, so the entry is present");
        f.render_stateful_widget(
            StatefulImage::<StatefulProtocol>::default().resize(Resize::Fit(None)),
            area,
            protocol,
        );
        Ok(())
    }

    /// Drop a cached decode — for a path rewritten by a re-run of the same
    /// cell, where the file name stays the same and the pixels do not.
    pub fn invalidate(&self, path: &str) {
        self.cache.borrow_mut().remove(path);
        self.order.borrow_mut().retain(|p| p != path);
    }

    /// How many decodes are resident — for the eviction test.
    #[must_use]
    pub fn cached(&self) -> usize {
        self.cache.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path the kernel named but never wrote must be reported as missing,
    /// not drawn as an empty rectangle. A blank pane and a missing figure look
    /// identical to a reader, and only one of them is a bug worth chasing.
    #[test]
    fn a_missing_file_is_named_not_silently_blank() {
        let view = ImageView::with_picker(Picker::halfblocks());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).unwrap();
        let mut outcome = Ok(());
        terminal
            .draw(|f| {
                outcome = view.draw(f, Rect::new(0, 0, 40, 20), "/nonexistent/figure.png");
            })
            .unwrap();
        let err = outcome.expect_err("a missing file must not report success");
        assert_eq!(
            err,
            ImageDrawError::Missing("/nonexistent/figure.png".to_string())
        );
        assert!(err.line().contains("/nonexistent/figure.png"));
    }

    /// A file that exists but is not an image is a DIFFERENT failure from one
    /// that is absent, and the reader needs to be able to tell them apart.
    #[test]
    fn a_non_image_file_reports_decode_failure_not_absence() {
        let dir = std::env::temp_dir().join("prism_image_view_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-an-image.png");
        std::fs::write(&path, b"this is not a PNG").unwrap();

        let view = ImageView::with_picker(Picker::halfblocks());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).unwrap();
        let mut outcome = Ok(());
        let path_str = path.to_string_lossy().to_string();
        terminal
            .draw(|f| {
                outcome = view.draw(f, Rect::new(0, 0, 40, 20), &path_str);
            })
            .unwrap();
        match outcome.expect_err("a non-image must not report success") {
            ImageDrawError::Undecodable(reason) => assert!(reason.contains("not-an-image.png")),
            other => panic!("expected a decode failure, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Decodes do not accumulate for the life of the session.
    ///
    /// Every figure a session drew used to stay resident as RGBA — about
    /// 1.2 MB each for a default matplotlib figure — so a long notebook run
    /// grew the TUI by hundreds of megabytes of pictures nobody was looking
    /// at. Eviction costs a re-decode and hides nothing.
    #[test]
    fn the_decode_cache_does_not_grow_without_end() {
        let dir = std::env::temp_dir().join("prism_image_view_cache_test");
        std::fs::create_dir_all(&dir).unwrap();
        let view = ImageView::with_picker(Picker::halfblocks());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).unwrap();

        let mut paths = Vec::new();
        for i in 0..(CACHE_ENTRIES * 2) {
            let path = dir.join(format!("figure-{i}.png"));
            let img = image::RgbaImage::new(4, 4);
            img.save(&path).unwrap();
            paths.push(path.to_string_lossy().to_string());
        }
        for path in &paths {
            let mut outcome = Ok(());
            terminal
                .draw(|f| {
                    outcome = view.draw(f, Rect::new(0, 0, 40, 20), path);
                })
                .unwrap();
            outcome.expect("a real PNG must draw");
        }
        assert!(
            view.cached() <= CACHE_ENTRIES,
            "the cache held {} decodes after drawing {} figures; the bound is \
             {CACHE_ENTRIES}",
            view.cached(),
            paths.len()
        );
        for path in &paths {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Never query the terminal from inside tmux or screen.
    ///
    /// `Picker::from_query_stdio` spawns a thread that enables raw mode,
    /// writes a graphics query, waits for a reply, and disables raw mode on
    /// its way out. A multiplexer does not forward that query, so the wait
    /// always hits its 2s timeout -- and the ORPHANED thread then turns raw
    /// mode off underneath the TUI that has meanwhile finished starting.
    ///
    /// MEASURED 2026-08-26, driving the real binary in tmux: every keystroke
    /// echoed into the status line as literal text (`^[[C`, `hello123`) and
    /// the prompt never received a character. The same build with the query
    /// skipped put typed text in the prompt box, as did the release binary
    /// that predates the change. The TUI was unusable inside tmux.
    #[test]
    fn a_multiplexer_is_never_asked_what_it_can_draw() {
        assert!(
            multiplexer_from(true, false, Some("xterm-256color")),
            "TMUX set"
        );
        assert!(
            multiplexer_from(false, true, Some("xterm-256color")),
            "STY set"
        );
        assert!(multiplexer_from(false, false, Some("screen-256color")));
        assert!(multiplexer_from(false, false, Some("tmux-256color")));
        // A real terminal outside a multiplexer must still be asked -- losing
        // kitty/sixel detection everywhere would trade one bug for a worse one.
        assert!(!multiplexer_from(false, false, Some("xterm-kitty")));
        assert!(!multiplexer_from(false, false, Some("xterm-256color")));
        assert!(!multiplexer_from(false, false, None));
    }

    /// A terminal that did not answer the device-attributes handshake is
    /// never sent the graphics query: that query's reader thread would wait
    /// on stdin for a reply that never comes, eat the first keystrokes, and
    /// restore the pre-query termios under the running TUI. The count of
    /// queries is the only observable difference — both paths end in a
    /// working picker — so the count is what is asserted.
    #[test]
    fn a_silent_terminal_is_never_asked_what_it_can_draw() {
        assert_eq!(
            probe_policy(false, false, || false),
            Probe::Halfblocks("terminal did not answer — not probed")
        );
        let before = ImageView::queries();
        let view =
            ImageView::from_policy(Probe::Halfblocks("terminal did not answer — not probed"));
        assert_eq!(
            ImageView::queries(),
            before,
            "a halfblocks verdict must not send the graphics query"
        );
        assert_eq!(view.protocol(), ProtocolType::Halfblocks);
        assert_eq!(
            view.graphics_note(),
            "halfblocks — terminal did not answer — not probed"
        );
        assert_eq!(ImageView::halfblocks().graphics_note(), "halfblocks");
    }

    /// A multiplexer and a remote session are settled before the handshake
    /// and never pay its two seconds: the handshake closure is not invoked.
    /// A terminal that answers is asked.
    #[test]
    fn a_multiplexer_or_remote_session_never_waits_for_the_handshake() {
        assert_eq!(
            probe_policy(true, false, || panic!("a multiplexer must not be asked")),
            Probe::Halfblocks("multiplexer — not probed")
        );
        assert_eq!(
            probe_policy(false, true, || panic!("a remote session must not be asked")),
            Probe::Halfblocks("remote session — not probed")
        );
        assert_eq!(probe_policy(false, false, || true), Probe::Query);
    }

    /// A session that arrived over ssh is remote, by either variable sshd
    /// sets; a local one is not.
    #[test]
    fn ssh_variables_mean_a_remote_session() {
        assert!(remote_from(true, false), "SSH_CONNECTION set");
        assert!(remote_from(false, true), "SSH_TTY set");
        assert!(!remote_from(false, false));
    }

    /// Halfblocks are the floor, not an error: on a terminal with no graphics
    /// protocol the figure is still drawn, just coarsely.
    #[test]
    fn halfblocks_is_a_working_floor_not_a_failure() {
        let view = ImageView::with_picker(Picker::halfblocks());
        assert_eq!(view.protocol(), ProtocolType::Halfblocks);
        assert!(
            !view.is_true_graphics(),
            "halfblocks must be reported as the fallback so a blocky figure is explainable"
        );
    }
}
