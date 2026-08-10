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
}
