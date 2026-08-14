//! Batch and window sizing for extraction — derived from the model's
//! context window, never decreed.
//!
//! PRISM used to hardcode both halves of this: exactly 10 sample rows of any
//! dataset (a 10,000-row dataset lost 9,990 rows without a word) and the
//! first 60,000 bytes of any document (a 362,000-character NASA deck was
//! read to ~1/6th). The binding constraint on one extraction call is the
//! model's CONTEXT WINDOW, which PRISM already tracks
//! (`LlmConfig::context_window`, GGUF metadata, and the serving runtime's
//! own report via `LlmClient::probe_context_window`). This module turns
//! that one constraint into batch sizes; operators may override through
//! `prism.toml [ingest]`, and the whole input is processed either way.

/// Context window assumed when NOTHING reports one — a config gap, not a
/// policy. Callers must SAY the fallback was used (the report/progress
/// surface carries it); this constant only decides how big the gap-bridging
/// batches are. 8192 tokens is documented as llama.cpp's long-standing
/// default serving allocation: small enough not to overflow any plausible
/// server, large enough to be useful.
pub const FALLBACK_CONTEXT_TOKENS: u64 = 8_192;

/// The same ~4-bytes-per-token estimate `prism_llm` budgets requests with
/// (`LlmClient::estimate_tokens`). Both sides of the batching arithmetic
/// must use ONE conversion or the derived batch and the client's output
/// clamp disagree about the same request.
pub const EST_BYTES_PER_TOKEN: u64 = 4;

/// Bytes of raw input data (row text or document text) budgeted into one
/// extraction call: a QUARTER of the context window, converted at
/// [`EST_BYTES_PER_TOKEN`]. The other three quarters hold the fixed prompt
/// scaffold, the model's reasoning (thinking models spend freely before
/// answering), and the JSON reply — which for dense tabular data can run as
/// large as the input itself. Numerically `cw/4 tokens × 4 bytes/token`
/// makes the byte budget equal to the window's token count.
pub fn input_byte_budget(context_window: Option<u64>) -> usize {
    let cw = context_window.unwrap_or(FALLBACK_CONTEXT_TOKENS);
    // Never smaller than one real row/paragraph of data: a pathological
    // reported window (llama.cpp will serve n_ctx=512) still yields a
    // usable, if tiny, batch.
    (((cw / 4) * EST_BYTES_PER_TOKEN) as usize).max(512)
}

/// Prompt bytes one row costs — the `Row N: [...]` line the tabular prompt
/// builder writes, estimated with the same `{row:?}` rendering it uses.
fn row_prompt_bytes(row: &[String]) -> usize {
    // "Row NNNN: " + debug-rendered cells + newline.
    format!("{row:?}").len() + 12
}

/// Split `rows` into consecutive batches.
///
/// `override_rows` (the `[ingest] batch_rows` knob) chunks by exact count.
/// Otherwise batches are PACKED to `budget_bytes` of rendered row text —
/// derived, not decreed: however many rows fit the window's input share is
/// how many go. A single row larger than the budget still ships alone
/// (batch of one) rather than being dropped; the server's own limit is the
/// honest failure for a row that genuinely cannot fit.
///
/// Returns `(start, end)` index ranges covering EVERY row exactly once —
/// no row is sampled out.
pub fn pack_row_batches(
    rows: &[Vec<String>],
    budget_bytes: usize,
    override_rows: Option<usize>,
) -> Vec<(usize, usize)> {
    if rows.is_empty() {
        return Vec::new();
    }
    if let Some(n) = override_rows.filter(|n| *n > 0) {
        return (0..rows.len())
            .step_by(n)
            .map(|start| (start, (start + n).min(rows.len())))
            .collect();
    }
    let mut batches = Vec::new();
    let mut start = 0usize;
    let mut used = 0usize;
    for (i, row) in rows.iter().enumerate() {
        let cost = row_prompt_bytes(row);
        if i > start && used + cost > budget_bytes {
            batches.push((start, i));
            start = i;
            used = 0;
        }
        used += cost;
    }
    batches.push((start, rows.len()));
    batches
}

/// Overlap between consecutive text windows: a tenth of the window, floored
/// at 512 bytes so even tiny (fallback-sized) windows still overlap by more
/// than a sentence — a fact spanning a boundary must be seen WHOLE by at
/// least one window, and materials facts ("the UTS of Ti-6Al-4V measured at
/// 298 K in air was 1140 MPa") span a few hundred bytes at most.
pub fn window_overlap(budget_bytes: usize) -> usize {
    (budget_bytes / 10).max(512)
}

/// Split `text` into byte-range windows of at most `budget_bytes`, each
/// consecutive pair overlapping by [`window_overlap`] bytes (adjusted to
/// UTF-8 boundaries). Every byte of the document falls inside at least one
/// window — nothing is truncated, which is the point of this module.
pub fn chunk_windows(text: &str, budget_bytes: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }
    let budget = budget_bytes.max(1024);
    if text.len() <= budget {
        return vec![(0, text.len())];
    }
    let overlap = window_overlap(budget);
    // Overlap is what the NEXT window re-reads; the window must advance by
    // more than it re-reads or chunking cannot terminate.
    let advance = budget - overlap;
    debug_assert!(advance > 0, "overlap must be smaller than the window");
    let mut windows = Vec::new();
    let mut start = 0usize;
    loop {
        let end = floor_char_boundary(text, (start + budget).min(text.len()));
        windows.push((start, end));
        if end >= text.len() {
            return windows;
        }
        start = floor_char_boundary(text, start + advance);
    }
}

/// Structure-aware segmentation: pack WHOLE units — pages, then blank-line
/// paragraphs — into chunks of at most `budget_bytes`, never cutting inside
/// a unit that fits the budget.
///
/// Byte windows cut mid-sentence and mid-table by construction; a chunk that
/// starts inside a sentence hands the model a fragment whose subject is in
/// the previous window. Packing whole structural units keeps every fact's
/// local context intact at zero model cost. The hierarchy degrades honestly:
///
/// - `page_ranges` are byte ranges of pages in `text` (see
///   `Understanding::plain_text_with_page_ranges`); a page that fits is a
///   unit. Pass `&[]` when the source has no page structure — the whole text
///   becomes one page.
/// - A page larger than the budget is split into blank-line paragraphs.
/// - A paragraph STILL larger than the budget falls back to today's
///   overlapped [`chunk_windows`] FOR THAT UNIT ONLY — so structureless text
///   (no pages, no blank lines) degrades to exactly the old behaviour, and a
///   document that fits the budget is ONE window regardless of structure.
///
/// Ranges from callers that do not tile `text` are healed by extending each
/// unit to the next unit's start, so every byte of the document falls inside
/// at least one chunk — the same coverage guarantee [`chunk_windows`] gives.
pub fn chunk_structured(
    text: &str,
    page_ranges: &[(usize, usize)],
    budget_bytes: usize,
) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }
    let budget = budget_bytes.max(1024);
    if text.len() <= budget {
        return vec![(0, text.len())];
    }

    let whole = [(0usize, text.len())];
    let pages: &[(usize, usize)] = if page_ranges.is_empty() {
        &whole
    } else {
        page_ranges
    };

    // Expand pages into units that each fit the budget (or are overlapped
    // fallback windows of an oversized paragraph).
    let mut units: Vec<(usize, usize)> = Vec::new();
    for &(page_start, page_end) in pages {
        let (page_start, page_end) = (page_start.min(text.len()), page_end.min(text.len()));
        if page_end <= page_start {
            continue;
        }
        if page_end - page_start <= budget {
            units.push((page_start, page_end));
            continue;
        }
        for (para_start, para_end) in paragraph_ranges(&text[page_start..page_end]) {
            let (para_start, para_end) = (page_start + para_start, page_start + para_end);
            if para_end - para_start <= budget {
                units.push((para_start, para_end));
            } else {
                units.extend(
                    chunk_windows(&text[para_start..para_end], budget)
                        .into_iter()
                        .map(|(s, e)| (para_start + s, para_start + e)),
                );
            }
        }
    }

    if units.is_empty() {
        // Degenerate page ranges (all empty after clamping): the coverage
        // guarantee outranks the structure claim.
        return chunk_windows(text, budget);
    }

    // Heal coverage: a caller's ranges may leave joiner bytes between units.
    // (Fallback windows OVERLAP their successor — those are left alone.)
    for i in 0..units.len().saturating_sub(1) {
        if units[i + 1].0 > units[i].1 {
            units[i].1 = units[i + 1].0;
        }
    }
    if let Some(first) = units.first_mut() {
        first.0 = 0;
    }
    if let Some(last) = units.last_mut() {
        last.1 = text.len();
    }
    // Healing a LARGE gap (a badly non-tiling caller) can push a unit past
    // the budget; re-split any such unit so the documented "at most
    // `budget_bytes` per chunk" bound holds for every caller, not just
    // tiling ones.
    let units: Vec<(usize, usize)> = units
        .into_iter()
        .flat_map(|(start, end)| {
            if end - start <= budget {
                vec![(start, end)]
            } else {
                chunk_windows(&text[start..end], budget)
                    .into_iter()
                    .map(|(s, e)| (start + s, start + e))
                    .collect()
            }
        })
        .collect();

    // Greedy packing of consecutive units: a chunk spans whole units while
    // the SPAN stays inside the budget. Never splits a unit — a unit that
    // does not fit the current chunk starts the next one.
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    for (start, end) in units {
        match chunks.last_mut() {
            Some((chunk_start, chunk_end)) if end - *chunk_start <= budget => {
                *chunk_end = (*chunk_end).max(end);
            }
            _ => chunks.push((start, end)),
        }
    }
    chunks
}

/// Byte ranges of blank-line paragraphs in `text`, tiling it exactly: each
/// paragraph's range carries the blank-line separator that follows it, so no
/// byte falls between units.
fn paragraph_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0usize; // byte offset of the current line's start
    let mut para_start: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        match (para_start, blank) {
            (None, false) => para_start = Some(cursor),
            (Some(start), true) => {
                // The separator attaches to the paragraph it ends: extend to
                // the end of this blank line (further blank lines extend the
                // previous range below).
                ranges.push((start, cursor + line.len()));
                para_start = None;
            }
            (None, true) => {
                // Leading or consecutive blank line: attach to the previous
                // paragraph, or (at the very start) to the first one later.
                if let Some(last) = ranges.last_mut() {
                    last.1 = cursor + line.len();
                }
            }
            (Some(_), false) => {}
        }
        cursor += line.len();
    }
    match para_start {
        Some(start) => ranges.push((start, text.len())),
        None => {
            if let Some(last) = ranges.last_mut() {
                last.1 = text.len();
            }
        }
    }
    // Leading blank lines before the FIRST paragraph: fold into it.
    if let Some(first) = ranges.first_mut() {
        first.0 = 0;
    }
    if ranges.is_empty() && !text.is_empty() {
        ranges.push((0, text.len()));
    }
    ranges
}

/// Largest char boundary ≤ `index` (stable stand-in for
/// `str::floor_char_boundary`).
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_derived_from_the_window_not_decreed() {
        // 25% share at 4 bytes/token ⇒ budget bytes == window tokens.
        assert_eq!(input_byte_budget(Some(65_536)), 65_536);
        assert_eq!(input_byte_budget(Some(32_768)), 32_768);
        // Unknown window ⇒ the DOCUMENTED fallback, not a silent guess.
        assert_eq!(input_byte_budget(None), FALLBACK_CONTEXT_TOKENS as usize);
        // A pathological window still yields a usable batch.
        assert_eq!(input_byte_budget(Some(0)), 512);
    }

    #[test]
    fn packed_batches_cover_every_row_exactly_once() {
        let rows: Vec<Vec<String>> = (0..100)
            .map(|i| vec![format!("alloy_{i}"), format!("{}", 900 + i)])
            .collect();
        for budget in [64usize, 300, 10_000, 1_000_000] {
            let batches = pack_row_batches(&rows, budget, None);
            let mut next = 0usize;
            for (start, end) in &batches {
                assert_eq!(*start, next, "gap or overlap at row {next}");
                assert!(end > start, "empty batch");
                next = *end;
            }
            assert_eq!(next, rows.len(), "rows lost at budget {budget}");
        }
    }

    #[test]
    fn packing_respects_the_byte_budget_when_rows_fit() {
        let rows: Vec<Vec<String>> = (0..50).map(|i| vec![format!("row_{i:04}")]).collect();
        let per_row = row_prompt_bytes(&rows[0]);
        let batches = pack_row_batches(&rows, per_row * 10, None);
        for (start, end) in &batches {
            let used: usize = rows[*start..*end].iter().map(|r| row_prompt_bytes(r)).sum();
            assert!(
                used <= per_row * 10,
                "batch {start}..{end} exceeds its budget"
            );
        }
        assert!(batches.len() >= 5, "expected ~5 batches, got {batches:?}");
    }

    #[test]
    fn a_row_larger_than_the_budget_still_ships_alone() {
        let rows = vec![
            vec!["small".to_string()],
            vec!["x".repeat(10_000)],
            vec!["small2".to_string()],
        ];
        let batches = pack_row_batches(&rows, 64, None);
        let covered: usize = batches.iter().map(|(s, e)| e - s).sum();
        assert_eq!(covered, 3, "the oversized row must not be dropped");
    }

    #[test]
    fn operator_override_chunks_by_exact_count() {
        let rows: Vec<Vec<String>> = (0..25).map(|i| vec![format!("r{i}")]).collect();
        let batches = pack_row_batches(&rows, 1_000_000, Some(10));
        assert_eq!(batches, vec![(0, 10), (10, 20), (20, 25)]);
    }

    #[test]
    fn windows_cover_the_whole_document_with_overlap() {
        let text = "abcdefghij".repeat(2_000); // 20,000 bytes
        let budget = 4_096;
        let windows = chunk_windows(&text, budget);
        assert!(windows.len() > 1);
        assert_eq!(windows[0].0, 0);
        assert_eq!(windows.last().unwrap().1, text.len());
        let overlap = window_overlap(budget);
        for pair in windows.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            // Full coverage: the next window starts before this one ends…
            assert!(b.0 < a.1, "gap between windows {a:?} and {b:?}");
            // …by roughly the declared overlap (UTF-8 stepping may shave a
            // few bytes), so a fact spanning the boundary is whole in one.
            assert!(
                a.1 - b.0 >= overlap.saturating_sub(8),
                "overlap too small between {a:?} and {b:?}"
            );
            assert!(b.1 > a.1, "windows must advance");
        }
        for (s, e) in windows {
            assert!(e - s <= budget, "window exceeds budget");
        }
    }

    /// The property the overlap exists for: a fact string spanning a window
    /// boundary appears INTACT in at least one window.
    #[test]
    fn a_boundary_spanning_fact_is_whole_in_some_window() {
        let budget = 4_096usize;
        let fact = "the UTS of Ti-6Al-4V measured at 298 K in air was 1140 MPa";
        // Plant the fact straddling the first boundary.
        let mut text = "x".repeat(budget - fact.len() / 2);
        text.push_str(fact);
        text.push_str(&"y".repeat(2 * budget));
        let windows = chunk_windows(&text, budget);
        assert!(
            windows.iter().any(|(s, e)| text[*s..*e].contains(fact)),
            "no window holds the boundary-spanning fact whole: {windows:?}"
        );
    }

    #[test]
    fn short_text_is_one_window_and_empty_is_none() {
        assert_eq!(chunk_windows("short", 4_096), vec![(0, 5)]);
        assert!(chunk_windows("", 4_096).is_empty());
    }

    #[test]
    fn windows_never_split_a_utf8_char() {
        let text = "\u{4e16}\u{754c}".repeat(2_000); // 3-byte chars, 12,000 bytes
        for (s, e) in chunk_windows(&text, 2_048) {
            assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
        }
    }

    // ── Structure-aware segmentation ──

    /// Pages joined the way `Understanding::plain_text_with_page_ranges`
    /// joins them: ranges tile the text, each carrying its trailing joiner.
    fn join_pages(pages: &[&str]) -> (String, Vec<(usize, usize)>) {
        let mut text = String::new();
        let mut ranges = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            let start = text.len();
            text.push_str(page);
            if i + 1 < pages.len() {
                text.push_str("\n\n");
            }
            ranges.push((start, text.len()));
        }
        (text, ranges)
    }

    /// The coverage property: every byte of the document falls inside at
    /// least one chunk, whatever mix of fitting pages, oversized pages, and
    /// oversized paragraphs the input holds.
    #[test]
    fn structured_chunks_cover_every_byte() {
        let big_para = "word ".repeat(600); // ~3000 bytes, over a 2048 budget
        let middle = format!("para A.\n\npara B.\n\n{big_para}");
        let pages = ["small page one.", middle.as_str(), "small page three."];
        let (text, ranges) = join_pages(&pages);
        let chunks = chunk_structured(&text, &ranges, 2_048);
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].0, 0);
        assert_eq!(chunks.last().unwrap().1, text.len());
        let mut covered_to = 0usize;
        for &(s, e) in &chunks {
            assert!(s <= covered_to, "gap before byte {covered_to}: {chunks:?}");
            assert!(e > s, "empty chunk: {chunks:?}");
            covered_to = covered_to.max(e);
            assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
        }
        assert_eq!(covered_to, text.len(), "bytes lost: {chunks:?}");
    }

    /// No cut inside a unit that fits: every fitting page appears WHOLE in
    /// some chunk, and chunks pack several such units up to the budget.
    #[test]
    fn a_fitting_unit_is_never_cut() {
        let pages: Vec<String> = (0..8)
            .map(|i| {
                format!(
                    "page {i}: {}",
                    format!("sentence about alloy {i}. ").repeat(20)
                )
            })
            .collect();
        let page_refs: Vec<&str> = pages.iter().map(String::as_str).collect();
        let (text, ranges) = join_pages(&page_refs);
        // ~460 bytes/page against the 1024-byte floor: ~2 pages per chunk.
        let chunks = chunk_structured(&text, &ranges, 1024);
        assert!(chunks.len() > 1, "fixture must actually pack: {chunks:?}");
        for page in &pages {
            assert!(
                chunks
                    .iter()
                    .any(|&(s, e)| text[s..e].contains(page.as_str())),
                "page cut across chunks: {page:?} in {chunks:?}"
            );
        }
    }

    /// An oversized unit — and ONLY that unit — falls back to today's
    /// overlapped byte windows; its neighbours stay whole.
    #[test]
    fn an_oversized_unit_falls_back_to_overlapped_windows_alone() {
        let budget = 2_048usize;
        let oversized = "x".repeat(3 * budget);
        let pages = ["intact page one.", oversized.as_str(), "intact page three."];
        let (text, ranges) = join_pages(&pages);
        let chunks = chunk_structured(&text, &ranges, budget);
        for page in ["intact page one.", "intact page three."] {
            assert!(
                chunks.iter().any(|&(s, e)| text[s..e].contains(page)),
                "a fitting neighbour must stay whole: {chunks:?}"
            );
        }
        // The oversized page produced several budget-bounded, overlapping
        // chunks — the chunk_windows shape.
        let inside: Vec<&(usize, usize)> = chunks
            .iter()
            .filter(|&&(s, e)| s >= ranges[1].0 && e <= ranges[1].1)
            .collect();
        assert!(inside.len() >= 3, "{chunks:?}");
        for pair in inside.windows(2) {
            assert!(
                pair[1].0 < pair[0].1,
                "fallback windows must overlap: {chunks:?}"
            );
        }
        for &&(s, e) in &inside {
            assert!(e - s <= budget);
        }
    }

    /// The two degradation contracts: a document that FITS is ONE window
    /// regardless of structure, and structureless oversized text (no pages,
    /// no blank lines) degrades to exactly today's chunk_windows.
    #[test]
    fn structured_chunking_degrades_to_the_old_behaviour() {
        let (text, ranges) = join_pages(&["page one.", "page two."]);
        assert_eq!(
            chunk_structured(&text, &ranges, 4_096),
            vec![(0, text.len())],
            "a fitting document is ONE window"
        );

        let structureless = "abcdefghij".repeat(2_000); // no \n at all
        assert_eq!(
            chunk_structured(&structureless, &[], 4_096),
            chunk_windows(&structureless, 4_096),
            "no structure ⇒ exactly the old overlapped windows"
        );
        assert!(chunk_structured("", &[], 4_096).is_empty());
    }

    /// A badly non-tiling caller (huge gap between its ranges) still gets
    /// both documented guarantees: every byte covered AND no chunk larger
    /// than the budget — gap-healing must re-split what it inflates.
    #[test]
    fn healed_gaps_never_exceed_the_budget() {
        let text = "y".repeat(6_000);
        let ranges = [(0usize, 10usize), (5_000, 5_010)];
        let budget = 1_024;
        let chunks = chunk_structured(&text, &ranges, budget);
        assert_eq!(chunks.first().unwrap().0, 0);
        assert_eq!(chunks.last().unwrap().1, text.len());
        let mut covered_to = 0usize;
        for &(s, e) in &chunks {
            assert!(e - s <= budget, "chunk over budget: {chunks:?}");
            assert!(s <= covered_to, "gap at {covered_to}: {chunks:?}");
            covered_to = covered_to.max(e);
        }
        assert_eq!(covered_to, text.len());
    }

    /// Blank-line paragraphs inside an oversized page are units of their
    /// own: chunks never cut inside a fitting paragraph.
    #[test]
    fn paragraphs_inside_an_oversized_page_stay_whole() {
        let budget = 1_024usize;
        let paras: Vec<String> = (0..12)
            .map(|i| {
                format!(
                    "paragraph {i}: {}",
                    format!("a self-contained fact about alloy {i}. ").repeat(8)
                )
            })
            .collect();
        let page = paras.join("\n\n"); // ~300 bytes/para, ~3.7K total > budget
        assert!(page.len() > budget, "fixture must exceed the budget");
        let chunks = chunk_structured(&page, &[(0, page.len())], budget);
        assert!(chunks.len() > 1);
        for para in &paras {
            assert!(
                chunks
                    .iter()
                    .any(|&(s, e)| page[s..e].contains(para.as_str())),
                "paragraph cut across chunks: {para:?}"
            );
        }
    }
}
