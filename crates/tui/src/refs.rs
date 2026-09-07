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
    /// A tool the agent ran (`tool://lookup_structure`).
    ///
    /// A tool name is the most-pointed-at word on the screen — it is what the
    /// reader sees when they ask "why did it do that?" — and it was the only
    /// coloured word that resolved to nothing.
    Tool,
    /// A source a tool result named (`provenance://<card>/<row>`): the
    /// database, engine or file a number came from. Opens the tool's own
    /// record of that source, held by the app since the result arrived.
    Provenance,
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
    /// Tokens that two entries with DIFFERENT ids have both claimed.
    ///
    /// Such a word identifies neither thing, so the matcher skips it: a mark
    /// that resolves to the wrong identity is worse than plain text. The
    /// measured case is the sigil — `id_sigil` keeps eight characters of the
    /// body, so two `file://` ids in one directory tree collide, and the
    /// earliest-insertion tie-break silently handed the first file every
    /// later file's clicks. Basenames collide the same way (`mod.rs`).
    ///
    /// Contested is PERMANENT for the session, not recomputed from current
    /// claims: entries are replaced wholesale on re-insertion, and if the ban
    /// lapsed the moment one claimant was re-registered, the other id would
    /// still abbreviate to the same word — the re-registered entry would
    /// steal it back.
    contested: std::collections::BTreeSet<String>,
    /// Bumped on every insert, so a cache keyed on it refreshes exactly when
    /// annotation could change.
    generation: u64,
}

impl ReferenceRegistry {
    /// Add or replace an entry. Re-inserting the same id replaces it, because
    /// a re-run of the same tool describes the same thing more recently.
    pub fn insert(&mut self, entry: ReferenceEntry) {
        self.generation += 1;
        for token in &entry.tokens {
            if !token.is_empty()
                && self
                    .entries
                    .iter()
                    .any(|e| e.id != entry.id && e.tokens.contains(token))
            {
                self.contested.insert(token.clone());
            }
        }
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

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
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

/// Find the next machine identifier in `hay` at or after `from`.
///
/// Returns `(start, end, written_id)` — the id AS WRITTEN, which may be an
/// abbreviation. Models paste these into prose whatever the system prompt
/// says — and the prompt cannot be relied on, because the model is swappable.
/// So the TUI recognises them itself, elisions included: the live transcript
/// wrote "cache://9a13e307…/structure.cif" for a 64-hex id, and an ellipsis
/// that stopped the scan left the head captured, unregistered, and the tail
/// as debris. [`resolve_written_id`] decides what an abbreviation stands for.
fn next_machine_id(hay: &str, from: usize) -> Option<(usize, usize, String)> {
    const PREFIXES: &[&str] = &["cache://", "file://"];
    /// Bytes of the run of id characters at the start of `s`.
    fn id_run(s: &str) -> usize {
        s.char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0)
    }
    /// Trim trailing sentence punctuation so "…cif." keeps its full stop in
    /// the prose — but keep a trailing run of three or more dots: that is an
    /// ELISION the model wrote ("cache://9a13e307..."), part of the id as
    /// written, not a full stop.
    fn trim_sentence_punct(hay: &str, start: usize, mut end: usize) -> usize {
        let dots = hay[start..end]
            .bytes()
            .rev()
            .take_while(|b| *b == b'.')
            .count();
        if dots >= 3 {
            return end;
        }
        while end > start && matches!(hay.as_bytes()[end - 1], b'.' | b'-') {
            end -= 1;
        }
        end
    }
    let mut best: Option<(usize, usize, String)> = None;
    for prefix in PREFIXES {
        let Some(rel) = hay[from..].find(prefix) else {
            continue;
        };
        let start = from + rel;
        let body = start + prefix.len();
        let mut end = body + id_run(&hay[body..]);
        if end <= body {
            continue;
        }
        // An ellipsis immediately after the run belongs to the id AS WRITTEN,
        // and so does the path tail after it: "cache://9a13e307…/structure.cif"
        // is one abbreviated id, not an id that stops at the ellipsis with
        // debris behind it. The ASCII "..." spelling needs no handling here —
        // '.' is an id character, so it is already inside the run.
        if hay[end..].starts_with('…') {
            end += '…'.len_utf8();
            end += id_run(&hay[end..]);
        }
        end = trim_sentence_punct(hay, start, end);
        if best.as_ref().is_none_or(|(bs, _, _)| start < *bs) {
            best = Some((start, end, hay[start..end].to_string()));
        }
    }
    best
}

/// The registered id a WRITTEN machine id stands for, if it identifies
/// exactly one.
///
/// Models abbreviate ids in their own prose — "cache://9a13e307…" for a
/// 64-hex id — and a swappable model abbreviates however it likes, so the
/// abbreviation habits are not ours to choose. A written id resolves when it
/// unambiguously identifies one registered entry:
///
/// 1. verbatim — it IS a registered id;
/// 2. elided — the text before the first `…`/`...` is a prefix of exactly
///    one registered id whose end also matches the text after it (an 8-hex
///    head disambiguates in practice);
/// 3. bare — the whole written id is a prefix of exactly one registered id.
///
/// Two candidates resolve to NOTHING: guessing between identities is worse
/// than leaving the text plain — the same rule that keeps unregistered ids
/// uncoloured and contested tokens unmatched.
fn resolve_written_id(written: &str, reg: &ReferenceRegistry) -> Option<String> {
    if reg.get(written).is_some() {
        return Some(written.to_string());
    }
    let (head, tail) = match written.find('…') {
        Some(i) => (&written[..i], &written[i + '…'.len_utf8()..]),
        None => match written.find("...") {
            Some(i) => (&written[..i], &written[i + 3..]),
            None => (written, ""),
        },
    };
    let mut candidates = reg.entries().filter(|e| {
        // The length guard keeps head and tail from overlapping inside a
        // short id: "cache://abc…bc" must not resolve to "cache://abc".
        e.id.len() >= head.len() + tail.len() && e.id.starts_with(head) && e.id.ends_with(tail)
    });
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => Some(only.id.clone()),
        _ => None,
    }
}

/// The short form shown in prose for an id PRISM knows.
///
/// Deterministic and purely presentational: the full id is never lost, it
/// lives in the panel the mark opens. The prose stops paying forty hex
/// characters for something nobody reads character by character.
#[must_use]
pub fn id_sigil(id: &str) -> String {
    let body = id.split_once("://").map(|(_, rest)| rest).unwrap_or(id);
    let head: String = body.chars().take(8).collect();
    let scheme = id.split_once("://").map(|(s, _)| s).unwrap_or("id");
    format!("{scheme}:{head}…")
}

/// The earliest standalone token match in `hay` at or after `from`, with the
/// id of the entry the token stands for. Earliest wins, then longest, so an
/// entry whose token contains another's wins the span.
fn next_token_match<'r>(
    hay: &str,
    from: usize,
    reg: &'r ReferenceRegistry,
) -> Option<(usize, usize, &'r str)> {
    let mut best: Option<(usize, usize, &str)> = None;
    for entry in reg.entries() {
        for token in &entry.tokens {
            // A contested token — one that two different ids have claimed —
            // identifies neither and matches nothing. See
            // `ReferenceRegistry::contested`.
            if token.is_empty() || reg.contested.contains(token) {
                continue;
            }
            let Some(rel) = hay[from..].find(token.as_str()) else {
                continue;
            };
            let start = from + rel;
            let end = start + token.len();
            if !is_standalone(hay, start, end) {
                continue;
            }
            let better = match best {
                None => true,
                Some((bs, be, _)) => start < bs || (start == bs && end > be),
            };
            if better {
                best = Some((start, end, entry.id.as_str()));
            }
        }
    }
    best
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

/// The style every pointable mark wears.
///
/// One place, not a per-call-site copy: the convention the reader learns is
/// "this colour means I can open it", and two hand-kept styles would drift
/// into teaching two rules. A dedicated colour, not `warn` — `warn` also
/// paints loading messages, approval prompts and tagged rows, so sharing it
/// taught the rule and then broke it on the same screen.
#[must_use]
pub fn mark_style(t: Theme) -> Style {
    Style::default()
        .fg(t.reference)
        .add_modifier(Modifier::UNDERLINED)
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
    let style = mark_style(t);
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
                // A raw machine id wins over token matching AT THE SAME
                // POSITION: it IS the identity, not a word that stands for
                // one — and when a registered token is itself the raw id,
                // both matchers find the same characters and only this path
                // collapses a known id to its sigil. But it must not eat the
                // words BEFORE it. An early `continue` here used to emit the
                // head of "MoNbTaW stored at cache://x" unexamined, so a
                // label was only matchable in sentences that mentioned no id
                // after it. Both candidates are found first; the earlier one
                // is processed; the loop comes back for the other.
                //
                // Written ids that RESOLVE — verbatim, elided, or by unique
                // prefix, see `resolve_written_id` — collapse to a sigil and
                // stay pointable, carrying the FULL registered id. Ids that
                // resolve to nothing (unregistered, or ambiguous between two
                // registrations) are left exactly as written, and deliberately
                // NOT given the reference colour — that colour promises a
                // resolvable identity, and painting a token PRISM cannot
                // resolve with it would be a lie.
                let machine = next_machine_id(&text, cursor);
                let token = next_token_match(&text, cursor, reg);
                let machine_first = match (&machine, &token) {
                    (Some((ms, ..)), Some((ts, ..))) => ms <= ts,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if machine_first {
                    let Some((ms, me, written)) = machine else {
                        unreachable!("machine_first implies a machine id")
                    };
                    if ms > cursor {
                        let head = text[cursor..ms].to_string();
                        col += width_of(&head);
                        spans.push(Span::styled(head, span.style));
                    }
                    if let Some(full) = resolve_written_id(&written, reg) {
                        let sigil = id_sigil(&full);
                        let w = width_of(&sigil);
                        regions.push(RefRegion {
                            row,
                            col_start: col,
                            col_end: col + w,
                            id: full,
                        });
                        col += w;
                        spans.push(Span::styled(sigil, style));
                    } else {
                        let raw = text[ms..me].to_string();
                        col += width_of(&raw);
                        spans.push(Span::styled(raw, span.style));
                    }
                    cursor = me;
                    emitted_any = true;
                    continue;
                }
                let Some((start, end, id)) = token else { break };
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
    preferred_width: u16,
) -> RefPanelLayout {
    // The caller asks for what its content needs — an abstract is prose and
    // reads badly at the 56 columns that suit a cache key or a file path — and
    // the terminal still decides. `preferred_width` can only ever be reduced
    // here, never granted beyond what fits.
    let width = preferred_width
        .max(12)
        .min(area_width.saturating_sub(2))
        .max(12);
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

    /// A token BEFORE a machine id in the same span still marks.
    ///
    /// The machine-id branch used to `continue` past the head unexamined, so
    /// a label was only matchable in sentences that mentioned no id after it
    /// — and the sentence a tool result actually writes is "<label> stored at
    /// <id>". Both the word and the id must come back pointable.
    #[test]
    fn a_token_before_a_machine_id_in_the_same_span_still_marks() {
        let t = crate::theme::get(0);
        let reg = reg_with("cache://x", "MoNbTaW");
        let lines = crate::markdown::markdown_lines("MoNbTaW stored at cache://x today.", t, 200);
        let (out, regions) = annotate_references(lines, &reg, t);

        assert_eq!(
            regions.len(),
            2,
            "the label and the id must both mark: {regions:?}"
        );
        assert_eq!(regions[0].id, "cache://x");
        assert_eq!(
            extract(&out, &regions[0]),
            "MoNbTaW",
            "the head before the id must still be token-matched"
        );
        assert_eq!(
            extract(&out, &regions[1]),
            "cache:x…",
            "the id after the token must still collapse to its sigil"
        );
    }

    /// An id registered as its own token marks ONCE, as a sigil.
    ///
    /// Registration now lists the raw id among the tokens, so both matchers
    /// find the same characters. The machine path must win the tie: it is the
    /// one that collapses a known id, and letting the token path take it
    /// would leave the full forty-character id eating the line, marked but
    /// never shortened.
    #[test]
    fn a_raw_id_registered_as_its_own_token_still_collapses_once() {
        let t = crate::theme::get(0);
        let mut reg = ReferenceRegistry::default();
        reg.insert(ReferenceEntry {
            id: "cache://deadbeef".into(),
            kind: RefKind::Structure,
            tokens: vec!["W".into(), "cache://deadbeef".into()],
        });
        let lines = crate::markdown::markdown_lines("Stored at cache://deadbeef now.", t, 200);
        let (out, regions) = annotate_references(lines, &reg, t);

        assert_eq!(regions.len(), 1, "one id, one region: {regions:?}");
        assert_eq!(
            extract(&out, &regions[0]),
            "cache:deadbeef…",
            "the machine path must win the tie and collapse the id"
        );
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
                let plan = super::plan_ref_panel(w, h, BODY, SOURCES, 56);

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
        let plan = super::plan_ref_panel(200, 60, 30, 5, 56);
        assert_eq!(plan.body_shown, 30);
        assert_eq!(plan.sources_shown, 5);
        assert_eq!(plan.body_hidden(30), 0);
        assert!(plan.ontology_shown);
    }

    /// Every machine id in the prose is accounted for EXACTLY once on screen:
    /// a known id as a pointable mark, an unknown id verbatim. None vanishes.
    ///
    /// This is the layer the guarantee lives at. The model is swappable, so
    /// nothing may depend on it choosing not to paste ids — a system-prompt
    /// rule saying "do not print cache refs" was removed for exactly that
    /// reason. Presentation is the TUI's job, and this asserts it does it.
    #[test]
    fn every_machine_id_is_accounted_for_exactly_once() {
        let t = crate::theme::get(0);
        let mut reg = super::ReferenceRegistry::default();
        reg.insert(super::ReferenceEntry {
            id: "cache://known123/structure.cif".into(),
            kind: super::RefKind::Structure,
            tokens: vec!["Cu4".into()],
        });

        let prose = "Imported cache://known123/structure.cif and also \
                     cache://unregistered999/structure.cif for comparison.";
        let lines = crate::markdown::markdown_lines(prose, t, 200);
        let (out, regions) = super::annotate_references(lines, &reg, t);

        let rendered: String = out
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string()))
            .collect();

        // The known id collapsed, and is pointable, carrying its FULL id.
        assert_eq!(regions.len(), 1, "one known id, one region: {regions:?}");
        assert_eq!(regions[0].id, "cache://known123/structure.cif");
        assert!(
            !rendered.contains("cache://known123/structure.cif"),
            "a known id must collapse to a sigil, not keep eating the line: \
             {rendered}"
        );
        assert!(
            rendered.contains("cache:known123"),
            "the sigil must still name the thing: {rendered}"
        );

        // The unknown id survives EXACTLY as written. Never silently dropped.
        assert!(
            rendered.contains("cache://unregistered999/structure.cif"),
            "an id PRISM does not know must be left verbatim, not hidden: \
             {rendered}"
        );
        // ...and must NOT be dressed as resolvable.
        assert!(
            !regions.iter().any(|r| r.id.contains("unregistered")),
            "an unregistered id must not be given a reference region — the \
             colour promises an identity PRISM can resolve"
        );
    }

    /// An abbreviation that matches TWO registrations marks nothing.
    ///
    /// Guards the resolver against guessing: with two structures sharing an
    /// 8-hex head, "cache://9a13e307…/structure.cif" identifies neither, and
    /// a mark that resolves to the wrong one is worse than plain text — the
    /// same rule that keeps unregistered ids uncoloured. The written form
    /// stays verbatim so the reader sees exactly what the model said.
    #[test]
    fn an_ambiguous_abbreviation_marks_nothing_and_stays_verbatim() {
        let t = crate::theme::get(0);
        let mut reg = ReferenceRegistry::default();
        for tail in ["aaaa", "bbbb"] {
            reg.insert(ReferenceEntry {
                id: format!("cache://9a13e307{tail}/structure.cif"),
                kind: RefKind::Structure,
                tokens: Vec::new(),
            });
        }
        for prose in [
            "stored as cache://9a13e307…/structure.cif today.",
            "stored as cache://9a13e307.../structure.cif today.",
        ] {
            let lines = crate::markdown::markdown_lines(prose, t, 200);
            let (out, regions) = annotate_references(lines, &reg, t);
            assert!(
                regions.is_empty(),
                "{prose:?}: an ambiguous abbreviation must not mark; got \
                 {regions:?}"
            );
            let rendered: String = out
                .iter()
                .flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string()))
                .collect();
            assert!(
                rendered.contains("cache://9a13e307"),
                "{prose:?}: the written id must survive verbatim: {rendered}"
            );
        }
    }

    /// A token that two different ids both claim identifies neither, so it
    /// marks nothing — permanently.
    ///
    /// The measured case is the sigil: `id_sigil` keeps 8 characters of the
    /// body, so two `file://` ids in one directory tree collide, and the
    /// earliest-insertion tie-break in `next_token_match` handed the first
    /// file every later file's clicks. Silently the wrong identity — worse
    /// than no mark. The ban must survive re-insertion: both ids still
    /// abbreviate to the same word, so a re-registered entry must not steal
    /// it back.
    #[test]
    fn a_token_two_ids_both_claim_marks_neither() {
        let t = crate::theme::get(0);
        let entry = |name: &str| {
            let id = format!("file:///home/user/project/src/{name}");
            ReferenceEntry {
                tokens: vec![name.to_string(), id.clone(), id_sigil(&id)],
                id,
                kind: RefKind::FileLine,
            }
        };
        // The collision is real: same tree, same sigil.
        assert_eq!(
            id_sigil("file:///home/user/project/src/foo.rs"),
            id_sigil("file:///home/user/project/src/bar.rs"),
        );
        let mut reg = ReferenceRegistry::default();
        reg.insert(entry("foo.rs"));
        reg.insert(entry("bar.rs"));

        // The shared sigil marks NOTHING — resolving it to either file would
        // be a guess.
        let lines = crate::markdown::markdown_lines("See file:/home/us… here.", t, 200);
        let (_, regions) = annotate_references(lines, &reg, t);
        assert!(
            regions.is_empty(),
            "a token both ids claim must mark neither; got {regions:?}"
        );

        // ...and re-inserting one claimant must not let it steal the word.
        reg.insert(entry("bar.rs"));
        let lines = crate::markdown::markdown_lines("See file:/home/us… here.", t, 200);
        let (_, regions) = annotate_references(lines, &reg, t);
        assert!(
            regions.is_empty(),
            "the ban must survive re-insertion; got {regions:?}"
        );

        // Uncontested tokens still work: each basename is one file's alone.
        let lines = crate::markdown::markdown_lines("Open bar.rs now.", t, 200);
        let (_, regions) = annotate_references(lines, &reg, t);
        assert_eq!(regions.len(), 1, "unique tokens must still mark");
        assert_eq!(regions[0].id, "file:///home/user/project/src/bar.rs");
    }

    /// The sigil is short, deterministic, and keeps the scheme.
    #[test]
    fn the_sigil_is_short_and_says_what_it_points_at() {
        assert_eq!(
            super::id_sigil("cache://c1d48df2abc/structure.cif"),
            "cache:c1d48df2…"
        );
        assert_eq!(super::id_sigil("file:///tmp/x/note.md"), "file:/tmp/x/n…");
        // Same input, same output — nothing time- or state-dependent.
        assert_eq!(
            super::id_sigil("cache://abc"),
            super::id_sigil("cache://abc")
        );
    }
}
