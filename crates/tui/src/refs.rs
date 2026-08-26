//! Words in the transcript that are backed by something you can open.
//!
//! The agent already knows the identity of what it produced — a structure's
//! `cache://…`, a paper's DOI, a `file:line`. That identity used to stop at the
//! tool result and the prose arrived as flat text, so a reader who saw
//! "MoNbTaW" had no way to ask what it was.
//!
//! Nothing here resolves anything. An entry holds an ID and the words that
//! stand for it, never a payload: what a reference POINTS AT is fetched when
//! the pointer arrives, not when the text was written. Most marks are never
//! hovered, and rendering them all at write time would be work thrown away.
//!
//! ## Why matching, rather than markers in the text
//!
//! The obvious design is for the model to write a marker the renderer strips.
//! Two facts kill it. Assistant text arrives as `ui.text.delta` FRAGMENTS, so a
//! marker splits across deltas (`"<<ref:struct"` then `"ure:cache://x>>"`) and
//! stripping needs a stateful cross-fragment parser — one missed split leaks
//! raw syntax into the transcript. And the model is swappable: GLM today, Qwen
//! or GPT tomorrow, none of them obliged to emit our syntax. A side-channel of
//! byte offsets fails for the same streaming reason — an offset addresses a
//! buffer that is still being appended to.
//!
//! Identities come from tool results instead, which the ENGINE controls, and
//! the match runs over whatever prose any model happens to write.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// What kind of thing a reference points at. Decides which widget renders it
/// when the pointer lands, and nothing else here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A crystal structure in the shared cache (`cache://…`).
    Structure,
    /// A paper, by DOI.
    Doi,
    /// A place in the source, `path:line`.
    FileLine,
}

/// One referenceable thing, and the words that stand for it in prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceEntry {
    /// Stable identity, e.g. `cache://e129a2e9…`. What hover resolves.
    pub id: String,
    pub kind: RefKind,
    /// Words that mean this entry when they appear in the transcript — a
    /// formula, a DOI string, a file name. Never a payload.
    pub tokens: Vec<String>,
}

/// Everything referenceable this session, by id.
#[derive(Debug, Default)]
pub struct ReferenceRegistry {
    entries: Vec<ReferenceEntry>,
}

impl ReferenceRegistry {
    /// Add or replace an entry. Re-inserting the same id replaces it, because
    /// a re-run of the same tool describes the same thing more recently.
    pub fn insert(&mut self, entry: ReferenceEntry) {
        if let Some(slot) = self.entries.iter_mut().find(|e| e.id == entry.id) {
            *slot = entry;
        } else {
            self.entries.push(entry);
        }
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ReferenceEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &ReferenceEntry> {
        self.entries.iter()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Where a reference landed on screen, in the coordinates of the lines handed
/// back — `row` indexes the returned `Vec<Line>`, columns are display cells
/// from the start of that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefRegion {
    pub row: usize,
    pub col_start: u16,
    pub col_end: u16,
    pub id: String,
}

/// Whether a match at `start..end` inside `hay` stands alone rather than
/// sitting inside a longer word.
///
/// Without this, a token like `Al4` would match inside `Al4Cu` and mark a
/// different material — the reader would hover the right-looking word and get
/// the wrong thing.
fn is_standalone(hay: &str, start: usize, end: usize) -> bool {
    let before = hay[..start].chars().next_back();
    let after = hay[end..].chars().next();
    let boundary = |c: Option<char>| match c {
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '_'),
    };
    boundary(before) && boundary(after)
}

/// Paint every known reference in `lines` and say where each one landed.
///
/// Operates on the ALREADY-RENDERED lines, so the coordinates it returns
/// describe what is actually on screen. Computing them from the source text
/// instead would drift the moment markdown wrapped or reflowed a paragraph,
/// and the reader would hover a correctly-coloured word while the hit region
/// covered its neighbour — the wrong panel, silently, with no panic.
#[must_use]
pub fn annotate_references(
    lines: Vec<Line<'static>>,
    reg: &ReferenceRegistry,
    t: Theme,
) -> (Vec<Line<'static>>, Vec<RefRegion>) {
    if reg.is_empty() {
        return (lines, Vec::new());
    }
    // A dedicated colour, not `warn`. The convention the reader learns is
    // "this colour means I can open it"; `warn` also paints loading messages,
    // approval prompts and tagged rows, so sharing it taught the rule and then
    // broke it on the same screen.
    let style = Style::default()
        .fg(t.reference)
        .add_modifier(Modifier::UNDERLINED);
    let mut regions = Vec::new();
    let mut out = Vec::with_capacity(lines.len());

    for (row, line) in lines.into_iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len());
        // Display column of the start of the span being examined.
        let mut col: u16 = 0;
        for span in line.spans {
            let text = span.content.to_string();
            // Earliest match in this span wins; a span is small, so scanning
            // every entry over it is cheap and keeps the rule obvious.
            let mut cursor = 0usize;
            let mut emitted_any = false;
            loop {
                let mut best: Option<(usize, usize, &str)> = None;
                for entry in reg.entries() {
                    for token in &entry.tokens {
                        if token.is_empty() {
                            continue;
                        }
                        let Some(rel) = text[cursor..].find(token.as_str()) else {
                            continue;
                        };
                        let start = cursor + rel;
                        let end = start + token.len();
                        if !is_standalone(&text, start, end) {
                            continue;
                        }
                        // Prefer the earliest match, then the longest, so an
                        // entry whose token contains another's wins the span.
                        let better = match best {
                            None => true,
                            Some((bs, be, _)) => start < bs || (start == bs && end > be),
                        };
                        if better {
                            best = Some((start, end, entry.id.as_str()));
                        }
                    }
                }
                let Some((start, end, id)) = best else { break };
                if start > cursor {
                    let head = text[cursor..start].to_string();
                    col += width_of(&head);
                    spans.push(Span::styled(head, span.style));
                }
                let marked = text[start..end].to_string();
                let marked_w = width_of(&marked);
                regions.push(RefRegion {
                    row,
                    col_start: col,
                    col_end: col + marked_w,
                    id: id.to_string(),
                });
                col += marked_w;
                spans.push(Span::styled(marked, style));
                cursor = end;
                emitted_any = true;
            }
            let tail = text[cursor..].to_string();
            if !tail.is_empty() || !emitted_any {
                col += width_of(&tail);
                spans.push(Span::styled(tail, span.style));
            }
        }
        out.push(Line::from(spans));
    }
    (out, regions)
}

fn width_of(s: &str) -> u16 {
    use unicode_width::UnicodeWidthStr;
    u16::try_from(s.width()).unwrap_or(u16::MAX)
}

/// How many rows each part of a reference panel gets, decided bottom-up from
/// what the panel is FOR rather than top-down from the screen.
///
/// The panel exists to answer two questions: where did this come from, and
/// what governs it. So identity, sources and ontology are never cut. The body
/// — a preview of the thing itself — is sacrificial and elides FIRST, which is
/// the inverse of the obvious implementation, where clamping the height cuts
/// whatever happens to be last and that is always the two sections the panel
/// exists for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefPanelLayout {
    pub width: u16,
    pub height: u16,
    /// Body lines that fit. The rest are counted, never dropped in silence.
    pub body_shown: usize,
    /// Source lines that fit.
    pub sources_shown: usize,
    /// True when the ontology line survived.
    pub ontology_shown: bool,
}

impl RefPanelLayout {
    /// Body lines withheld, for the marker. Zero means nothing was cut.
    #[must_use]
    pub fn body_hidden(&self, body_total: usize) -> usize {
        body_total.saturating_sub(self.body_shown)
    }
    /// Source lines withheld.
    #[must_use]
    pub fn sources_hidden(&self, sources_total: usize) -> usize {
        sources_total.saturating_sub(self.sources_shown)
    }
}

/// Plan a panel that always fits.
///
/// Pure: takes sizes, returns sizes. Testable as a property across terminal
/// dimensions we never enumerate, rather than as a snapshot of one.
#[must_use]
pub fn plan_ref_panel(
    area_width: u16,
    area_height: u16,
    body_total: usize,
    sources_total: usize,
) -> RefPanelLayout {
    let width = 56.min(area_width.saturating_sub(2)).max(12);
    // Two borders, the identity line, the "sources" label, the "ontology"
    // label and its one line. Everything below is spent from what remains.
    let fixed: u16 = 2 + 1 + 1 + 1 + 1;
    let budget = area_height.saturating_sub(1);

    // Sources come before the body: they are half the reason the panel exists.
    let room_after_fixed = budget.saturating_sub(fixed);
    let sources_shown = (sources_total as u16).min(room_after_fixed) as usize;
    let sources_hidden = sources_total.saturating_sub(sources_shown);
    // A "+N more sources" marker costs a row, and only when something is cut.
    let sources_marker =
        u16::from(sources_hidden > 0).min(room_after_fixed.saturating_sub(sources_shown as u16));

    let room_for_body = room_after_fixed
        .saturating_sub(sources_shown as u16)
        .saturating_sub(sources_marker);
    // Reserve one row for the elision marker when the body will not fit whole.
    let body_shown = if (body_total as u16) <= room_for_body {
        body_total
    } else {
        room_for_body.saturating_sub(1) as usize
    };
    let body_marker =
        u16::from(body_total > body_shown).min(room_for_body.saturating_sub(body_shown as u16));

    let height = (fixed + sources_shown as u16 + sources_marker + body_shown as u16 + body_marker)
        .min(budget);
    RefPanelLayout {
        width,
        height,
        body_shown,
        sources_shown,
        // The ontology line is inside `fixed`, so it survives whenever the
        // panel has room to exist at all.
        ontology_shown: height >= fixed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg_with(id: &str, token: &str) -> ReferenceRegistry {
        let mut reg = ReferenceRegistry::default();
        reg.insert(ReferenceEntry {
            id: id.to_string(),
            kind: RefKind::Structure,
            tokens: vec![token.to_string()],
        });
        reg
    }

    /// Read the marked text back out of the RENDERED line using the region's
    /// own coordinates. This is the assertion that matters: it proves the
    /// coordinates address the words they claim to, rather than proving the
    /// matcher found something somewhere.
    fn extract(lines: &[Line<'static>], r: &RefRegion) -> String {
        use unicode_width::UnicodeWidthStr;
        let mut col = 0u16;
        let mut out = String::new();
        for span in &lines[r.row].spans {
            for ch in span.content.chars() {
                let w = u16::try_from(ch.to_string().width()).unwrap_or(0);
                if col >= r.col_start && col < r.col_end {
                    out.push(ch);
                }
                col += w;
            }
        }
        out
    }

    /// A known reference in ordinary prose is marked, and its coordinates
    /// re-extract exactly the token from what was rendered.
    ///
    /// The failure this guards is silent: a matcher that finds the word but
    /// reports coordinates one span off highlights the right text while the
    /// hit region covers its neighbour, so hovering resolves the WRONG
    /// reference and nothing panics. Asserting against the rendered output —
    /// not the source string — is the only way to see it.
    #[test]
    fn a_marked_reference_reports_the_coordinates_of_its_own_word() {
        let t = crate::theme::get(0);
        let reg = reg_with("cache://x", "MoNbTaW");
        let lines = crate::markdown::markdown_lines("Results for MoNbTaW are in.", t, 80);
        let (out, regions) = annotate_references(lines, &reg, t);

        assert_eq!(regions.len(), 1, "one token, one region: {regions:?}");
        let r = &regions[0];
        assert_eq!(r.id, "cache://x");
        assert_eq!(
            extract(&out, r),
            "MoNbTaW",
            "the region must re-extract its own token from the RENDERED line; \
             anything else means the coordinates drifted and hover would open \
             the wrong thing"
        );
    }

    /// A token inside a longer word is a different thing and must not be
    /// marked. `Al4` inside `Al4Cu` is a different material.
    #[test]
    fn a_token_inside_a_longer_word_is_not_a_reference() {
        let t = crate::theme::get(0);
        let reg = reg_with("cache://y", "Al4");
        for prose in ["The Al4Cu phase.", "See xAl4y here.", "Al4Cu3 forms."] {
            let lines = crate::markdown::markdown_lines(prose, t, 80);
            let (_, regions) = annotate_references(lines, &reg, t);
            assert!(
                regions.is_empty(),
                "{prose:?} must yield no region; got {regions:?}"
            );
        }
        // ...but the standalone word still matches, so the negative case above
        // is not passing merely because matching is broken.
        let lines = crate::markdown::markdown_lines("The Al4 cell.", t, 80);
        let (_, regions) = annotate_references(lines, &reg, t);
        assert_eq!(regions.len(), 1, "the standalone token must still match");
    }

    /// With nothing registered, the lines come back untouched — no allocation
    /// churn and no styling, so a session that never ran a tool pays nothing.
    #[test]
    fn an_empty_registry_changes_nothing() {
        let t = crate::theme::get(0);
        let reg = ReferenceRegistry::default();
        let before = crate::markdown::markdown_lines("Plain prose here.", t, 80);
        let expected: Vec<String> = before
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        let (after, regions) = annotate_references(before, &reg, t);
        assert!(regions.is_empty());
        let got: Vec<String> = after
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(got, expected);
    }

    /// Re-inserting an id replaces it rather than duplicating, so a tool that
    /// runs twice does not put two regions on one word.
    #[test]
    fn reinserting_an_id_replaces_it() {
        let mut reg = reg_with("cache://x", "MoNbTaW");
        reg.insert(ReferenceEntry {
            id: "cache://x".into(),
            kind: RefKind::Structure,
            tokens: vec!["MoNbTaW".into()],
        });
        assert_eq!(reg.entries().count(), 1);
        let t = crate::theme::get(0);
        let lines = crate::markdown::markdown_lines("MoNbTaW again.", t, 80);
        let (_, regions) = annotate_references(lines, &reg, t);
        assert_eq!(regions.len(), 1, "one entry, one region: {regions:?}");
    }

    /// The panel fits at EVERY terminal size, and never cuts anything in
    /// silence.
    ///
    /// A property, not a snapshot: it has to hold for sizes nobody enumerated.
    /// The failure it guards is the obvious implementation's — clamp the
    /// height and whatever is last gets cut, which is always `sources` and
    /// `ontology`, the two things the panel exists to show.
    #[test]
    fn the_panel_always_fits_and_never_cuts_in_silence() {
        const BODY: usize = 30;
        const SOURCES: usize = 5;
        for w in [5u16, 20, 40, 80, 120, 200] {
            for h in [3u16, 10, 16, 24, 40, 60] {
                let plan = super::plan_ref_panel(w, h, BODY, SOURCES);

                assert!(
                    plan.height <= h.saturating_sub(1),
                    "{w}x{h}: panel height {} exceeds the screen",
                    plan.height
                );
                assert!(
                    plan.width <= w.max(12),
                    "{w}x{h}: panel width {} exceeds the screen",
                    plan.width
                );
                assert_eq!(
                    plan.body_shown + plan.body_hidden(BODY),
                    BODY,
                    "{w}x{h}: body lines went missing rather than being counted"
                );
                assert_eq!(
                    plan.sources_shown + plan.sources_hidden(SOURCES),
                    SOURCES,
                    "{w}x{h}: source lines went missing rather than being counted"
                );
                // The body is sacrificial BEFORE the sources are.
                if plan.sources_hidden(SOURCES) > 0 {
                    assert_eq!(
                        plan.body_shown, 0,
                        "{w}x{h}: cut a source while still showing {} body \
                         lines — the preview is sacrificial, the provenance is not",
                        plan.body_shown
                    );
                }
            }
        }
    }

    /// With room for everything, everything is shown and nothing is marked as
    /// withheld — so the elision path cannot mask a permanent truncation.
    #[test]
    fn a_large_terminal_shows_the_whole_panel() {
        let plan = super::plan_ref_panel(200, 60, 30, 5);
        assert_eq!(plan.body_shown, 30);
        assert_eq!(plan.sources_shown, 5);
        assert_eq!(plan.body_hidden(30), 0);
        assert!(plan.ontology_shown);
    }
}
