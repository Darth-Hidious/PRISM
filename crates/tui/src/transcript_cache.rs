//! Per-message render cache for the transcript.
//!
//! Live complaint 2026-09-06: "after PRISM has run for a while it becomes too
//! slow to scroll." Every frame rebuilt every message's lines (markdown,
//! reference annotation), measured every non-blank line with its own
//! `Paragraph`, and re-wrapped the whole transcript to count rows — at ten
//! frames a second even idle — so the cost of a frame grew with the session.
//!
//! A message's lines depend on the message, the width, the theme, the focus
//! and the reference registry; the renderer hashes those into a key and
//! rebuilds only entries whose key changed. Row counts are measured once, at
//! build time, so placing lines on screen is arithmetic.

use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

/// One message, built: its lines and everything the frame needs to place
/// them, in coordinates relative to the message's first line.
pub struct Built {
    pub lines: Vec<Line<'static>>,
    /// Wrapped rows per line at the width the entry was built for.
    pub rows: Vec<u16>,
    /// The "❯ You" header line of a user message, if this is one.
    pub user_header: Option<usize>,
    /// Inline figures: (line, image path).
    pub figures: Vec<(usize, String)>,
    /// Reference marks: (line, col_start, col_end, id).
    pub refs: Vec<(usize, u16, u16, String)>,
    /// Every non-blank line as (line, text, identity) — identity is the text
    /// without a running tool's changing age, so the cursor can follow it.
    pub texts: Vec<(usize, String, String)>,
    /// A thinking message: drawn without a trailing blank line.
    pub is_thinking: bool,
    /// This entry drew the collapsed "[thinking…]" line, which appears once.
    pub sets_thinking_shown: bool,
}

impl Built {
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        width: u16,
        lines: Vec<Line<'static>>,
        user_header: Option<usize>,
        figures: Vec<(usize, String)>,
        refs: Vec<(usize, u16, u16, String)>,
        volatile: Vec<(usize, String)>,
        is_thinking: bool,
        sets_thinking_shown: bool,
    ) -> Self {
        let rows = lines
            .iter()
            .map(|line| {
                Paragraph::new(vec![line.clone()])
                    .wrap(Wrap { trim: false })
                    .line_count(width)
                    .min(usize::from(u16::MAX)) as u16
            })
            .collect();
        let texts = lines
            .iter()
            .enumerate()
            .filter_map(|(at, line)| {
                let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
                if text.trim().is_empty() {
                    return None;
                }
                // Two strings per row, because they answer two questions.
                // `text` is what was on screen — what `e` quotes back, byte
                // for byte. The identity is what the row is FOUND by, and it
                // must hold still while the row is on screen, so anything
                // that redraws itself (a running tool's age) is cut off it.
                let identity = volatile
                    .iter()
                    .find(|(row, _)| *row == at)
                    .and_then(|(_, tail)| text.strip_suffix(tail.as_str()))
                    .map_or_else(|| text.clone(), str::to_string);
                Some((at, text, identity))
            })
            .collect();
        Self {
            lines,
            rows,
            user_header,
            figures,
            refs,
            texts,
            is_thinking,
            sets_thinking_shown,
        }
    }
}

#[derive(Default)]
pub struct TranscriptCache {
    entries: Vec<Option<(u64, Built)>>,
    built: u64,
}

impl TranscriptCache {
    /// Lines built since the cache was created — the cost the cache exists
    /// to bound.
    pub fn lines_built(&self) -> u64 {
        self.built
    }

    /// Forget entries past `len` (a cleared or shortened transcript).
    pub fn truncate(&mut self, len: usize) {
        self.entries.truncate(len);
    }

    /// The entry for message `idx`, rebuilt when its key changed.
    pub fn get_or_build(&mut self, idx: usize, key: u64, build: impl FnOnce() -> Built) -> &Built {
        if self.entries.len() <= idx {
            self.entries.resize_with(idx + 1, || None);
        }
        if self.entries[idx].as_ref().is_some_and(|(k, _)| *k == key) {
            return &self.entries[idx].as_ref().expect("checked").1;
        }
        let built = build();
        self.built += built.lines.len() as u64;
        self.entries[idx] = Some((key, built));
        &self.entries[idx].as_ref().expect("just stored").1
    }
}
