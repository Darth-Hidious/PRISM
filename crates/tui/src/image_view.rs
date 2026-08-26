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

/// Terminal graphics capability plus a decode cache.
///
/// One per app. [`Picker::from_query_stdio`] talks to the terminal with escape
/// sequences and must run ONCE, before the draw loop owns stdout — querying
/// per frame would both stall rendering and interleave escape sequences with
/// the frame being drawn.
pub struct ImageView {
    picker: Picker,
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
    /// Ask the terminal what it can draw.
    ///
    /// Falls back to halfblocks when the query fails — over a pipe, in CI, or
    /// on a terminal that ignores the query. Halfblocks need no protocol
    /// support at all, so this cannot end in "no image".
    #[must_use]
    pub fn detect() -> Self {
        DETECTIONS.fetch_add(1, Ordering::Relaxed);
        // Inside a multiplexer the query is not just useless, it is HARMFUL.
        //
        // `from_query_stdio` spawns a thread that enables raw mode, writes the
        // query, waits for a reply, and disables raw mode on its way out.
        // tmux and screen do not forward that query, so the wait always hits
        // its 2s timeout -- and the orphaned thread then turns raw mode OFF
        // underneath the TUI that has meanwhile finished starting. Measured
        // 2026-08-26 in tmux: every keystroke echoed into the status line and
        // the prompt never received a character. The binary was unusable.
        //
        // Halfblocks need no query and no thread. They are a working floor,
        // not a failure: the picture still draws, coarsely. A terminal that
        // can do better is still detected when PRISM runs outside a
        // multiplexer.
        let picker = if in_multiplexer() {
            Picker::halfblocks()
        } else {
            Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
        };
        Self::with_picker(picker)
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
        Self {
            picker,
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
