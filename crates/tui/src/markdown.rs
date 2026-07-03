//! Minimal markdown → ratatui converter for assistant transcript text.
//!
//! Hand-rolled on purpose (no markdown crate): the transcript only needs
//! the subset models actually emit — headers, bullets, bold/italic,
//! inline code, fenced code blocks, links, and GFM tables. The input
//! arrives as a stream, so the parser must be resilient to partially-
//! streamed markdown: any unmatched marker renders literally instead of
//! eating text. Inline/display LaTeX math (`$…$`, `$$…$$`, `\(…\)`,
//! `\[…\]`) is converted to a Unicode approximation via [`crate::latex`].
//!
//! All colors come from the active [`Theme`] so markdown recolors with
//! the rest of the UI.

use crate::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Style for `inline code` and fenced code block content.
fn code_style(t: Theme) -> Style {
    Style::default().fg(t.warn).bg(t.status_bg)
}

/// Style for link labels (`[label](url)`).
fn link_style(t: Theme) -> Style {
    Style::default()
        .fg(t.user)
        .add_modifier(Modifier::UNDERLINED)
}

/// Convert a markdown message to styled ratatui lines, wrapped/truncated
/// to fit `width` display columns (used for GFM table sizing; prose is
/// left for the caller's [`ratatui::widgets::Wrap`] to reflow).
///
/// Supported (deliberately small): `#`–`######` headers, `-`/`*`/`+`
/// bullets, `**bold**`, `*italic*`, `` `code` ``, ``` fenced blocks,
/// `[label](url)` links (label shown, URL kept in the raw message for
/// the link opener), `---` rules, and GFM pipe tables. Everything else
/// is passed through with the base text style.
pub fn markdown_lines(text: &str, t: Theme, width: u16) -> Vec<Line<'static>> {
    let base = Style::default().fg(t.text);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let src: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < src.len() {
        let raw = src[i];
        let trimmed = raw.trim_start();

        // Fenced code block delimiters toggle code mode. The fence line
        // itself renders dimmed so the block stays visually delimited.
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            out.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(t.muted),
            )));
            i += 1;
            continue;
        }
        if in_code_block {
            out.push(Line::from(Span::styled(raw.to_string(), code_style(t))));
            i += 1;
            continue;
        }

        // GFM pipe table: a header row containing `|` immediately followed
        // by a delimiter row (`|---|:--:|` …). Both rows must contain `|`
        // so a bare `---` still reads as a horizontal rule, not a 1-col
        // table. Consumes the whole block (header + delimiter + data rows).
        if raw.contains('|')
            && i + 1 < src.len()
            && src[i + 1].contains('|')
            && let Some(aligns) = table_delimiter(src[i + 1])
        {
            let header = split_row(raw);
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut j = i + 2;
            while j < src.len() && src[j].contains('|') && !src[j].trim().is_empty() {
                rows.push(split_row(src[j]));
                j += 1;
            }
            out.extend(render_table(&header, &aligns, &rows, t, width));
            i = j;
            continue;
        }

        // Headers: strip the #'s, render bold in the accent color.
        if let Some(rest) = header_text(trimmed) {
            out.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )));
            i += 1;
            continue;
        }

        // Horizontal rule.
        if trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-') {
            out.push(Line::from(Span::styled(
                "─".repeat(24),
                Style::default().fg(t.divider),
            )));
            i += 1;
            continue;
        }

        // Bullets: "- " / "* " / "+ " → "• ", indent preserved.
        if let Some(rest) = bullet_text(trimmed) {
            let indent = raw.chars().count() - trimmed.chars().count();
            let mut spans = vec![Span::styled(
                format!("{}• ", " ".repeat(indent)),
                Style::default().fg(t.dim),
            )];
            spans.extend(inline_spans(rest, t, base));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }

        out.push(Line::from(inline_spans(raw, t, base)));
        i += 1;
    }
    out
}

/// `## Header` → `Header`. Requires 1–6 hashes followed by a space.
fn header_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        line[hashes..].strip_prefix(' ').map(str::trim)
    } else {
        None
    }
}

/// `- item` → `item` for the three common bullet markers.
fn bullet_text(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
}

// ── GFM tables ───────────────────────────────────────────────────────

/// Column alignment parsed from a table's delimiter row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

/// Split a pipe-table row into trimmed cells. Optional leading/trailing
/// `|` are dropped. (Escaped `\|` inside a cell is not handled — models
/// rarely emit it in transcript tables.)
fn split_row(line: &str) -> Vec<String> {
    let s = line.trim();
    let s = s.strip_prefix('|').unwrap_or(s);
    let s = s.strip_suffix('|').unwrap_or(s);
    s.split('|').map(|c| c.trim().to_string()).collect()
}

/// Parse a delimiter row (`|:---|---:|:--:|`) into per-column alignment,
/// or `None` if the line is not a valid delimiter row. Every cell must be
/// dashes with optional leading/trailing `:`.
fn table_delimiter(line: &str) -> Option<Vec<Align>> {
    let cells = split_row(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in &cells {
        let c = cell.trim();
        let core = c.trim_start_matches(':').trim_end_matches(':');
        if core.is_empty() || !core.chars().all(|ch| ch == '-') {
            return None;
        }
        aligns.push(match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    Some(aligns)
}

/// Render a parsed table as aligned, box-drawn rows that fit `width`
/// columns. Column widths shrink (widest first, cells truncated with `…`)
/// until the table fits.
fn render_table(
    header: &[String],
    aligns: &[Align],
    rows: &[Vec<String>],
    t: Theme,
    width: u16,
) -> Vec<Line<'static>> {
    let ncols = header.len().max(1);

    // Natural column widths = widest cell (header + data), min 1.
    let mut widths: Vec<usize> = (0..ncols)
        .map(|c| {
            let mut w = UnicodeWidthStr::width(cell_at(header, c));
            for r in rows {
                w = w.max(UnicodeWidthStr::width(cell_at(r, c)));
            }
            w.max(1)
        })
        .collect();

    // Shrink to fit: rendered width = (ncols+1) bars + Σ(w+2). Reduce the
    // widest column (never below 1) until the row fits `width`.
    let budget = (width as usize).saturating_sub(3 * ncols + 1);
    while widths.iter().sum::<usize>() > budget.max(ncols) {
        let Some(idx) = widths
            .iter()
            .enumerate()
            .filter(|&(_, &w)| w > 1)
            .max_by_key(|&(_, &w)| w)
            .map(|(i, _)| i)
        else {
            break;
        };
        widths[idx] -= 1;
    }

    let bstyle = Style::default().fg(t.divider);
    let make_row = |cells: &[String], cell_style: Style| -> Line<'static> {
        let mut spans = vec![Span::styled("│".to_string(), bstyle)];
        for (c, &w) in widths.iter().enumerate() {
            let align = aligns.get(c).copied().unwrap_or(Align::Left);
            let text = fit_cell(cell_at(cells, c), w, align);
            spans.push(Span::styled(format!(" {text} "), cell_style));
            spans.push(Span::styled("│".to_string(), bstyle));
        }
        Line::from(spans)
    };

    let mut lines = Vec::with_capacity(rows.len() + 3);
    lines.push(Line::from(Span::styled(
        table_border(&widths, '┌', '┬', '┐'),
        bstyle,
    )));
    lines.push(make_row(
        header,
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(Span::styled(
        table_border(&widths, '├', '┼', '┤'),
        bstyle,
    )));
    for r in rows {
        lines.push(make_row(r, Style::default().fg(t.text)));
    }
    lines.push(Line::from(Span::styled(
        table_border(&widths, '└', '┴', '┘'),
        bstyle,
    )));
    lines
}

/// Cell text at column `c`, or `""` when the row is short.
fn cell_at(row: &[String], c: usize) -> &str {
    row.get(c).map(String::as_str).unwrap_or("")
}

/// Build a horizontal border line (`┌──┬──┐` style) for the given widths.
fn table_border(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut s = String::new();
    s.push(left);
    for (i, &w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        s.push(if i + 1 == widths.len() { right } else { mid });
    }
    s
}

/// Fit `text` to exactly `w` display columns: pad per `align`, or truncate
/// with a trailing `…` when it overflows.
fn fit_cell(text: &str, w: usize, align: Align) -> String {
    let tw = UnicodeWidthStr::width(text);
    if tw <= w {
        let pad = w - tw;
        return match align {
            Align::Left => format!("{text}{}", " ".repeat(pad)),
            Align::Right => format!("{}{text}", " ".repeat(pad)),
            Align::Center => {
                let l = pad / 2;
                format!("{}{text}{}", " ".repeat(l), " ".repeat(pad - l))
            }
        };
    }
    // Truncate to w-1 columns, then append the ellipsis (width 1).
    let mut acc = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        acc.push(ch);
        used += cw;
    }
    acc.push('…');
    used += 1;
    acc.push_str(&" ".repeat(w.saturating_sub(used)));
    acc
}

/// Parse inline markdown (`**bold**`, `*italic*`, `` `code` ``,
/// `[label](url)`) into styled spans. Unmatched markers render literally.
fn inline_spans(text: &str, t: Theme, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    fn flush(plain: &mut String, spans: &mut Vec<Span<'static>>, base: Style) {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base));
        }
    }

    while i < chars.len() {
        // Escaped dollar (`\$`) → a literal `$`, never a math delimiter.
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'$') {
            plain.push('$');
            i += 2;
            continue;
        }
        // Display math `$$…$$` (single line). Checked before inline `$…$`.
        if chars[i] == '$'
            && chars.get(i + 1) == Some(&'$')
            && let Some(close) = find_double_dollar(&chars, i + 2)
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 2..close].iter().collect();
            spans.push(Span::styled(crate::latex::render_math(&inner), base));
            i = close + 2;
            continue;
        }
        // Inline math `$…$` — a lone `$` (e.g. "$5") stays literal.
        if chars[i] == '$'
            && let Some(close) = find_char(&chars, i + 1, '$')
            && close > i + 1
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 1..close].iter().collect();
            spans.push(Span::styled(crate::latex::render_math(&inner), base));
            i = close + 1;
            continue;
        }
        // Inline math `\( … \)`.
        if chars[i] == '\\'
            && chars.get(i + 1) == Some(&'(')
            && let Some(close) = find_escaped(&chars, i + 2, ')')
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 2..close].iter().collect();
            spans.push(Span::styled(crate::latex::render_math(&inner), base));
            i = close + 2;
            continue;
        }
        // Display math `\[ … \]`.
        if chars[i] == '\\'
            && chars.get(i + 1) == Some(&'[')
            && let Some(close) = find_escaped(&chars, i + 2, ']')
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 2..close].iter().collect();
            spans.push(Span::styled(crate::latex::render_math(&inner), base));
            i = close + 2;
            continue;
        }
        // `code`
        if chars[i] == '`'
            && let Some(close) = find_char(&chars, i + 1, '`')
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 1..close].iter().collect();
            spans.push(Span::styled(inner, code_style(t)));
            i = close + 1;
            continue;
        }
        // **bold**
        if chars[i] == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(close) = find_double_star(&chars, i + 2)
            && close > i + 2
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 2..close].iter().collect();
            spans.push(Span::styled(inner, base.add_modifier(Modifier::BOLD)));
            i = close + 2;
            continue;
        }
        // *italic* — the opener must hug text ("2 * 3" stays literal).
        if chars[i] == '*'
            && chars
                .get(i + 1)
                .is_some_and(|c| *c != '*' && !c.is_whitespace())
            && let Some(close) = find_char(&chars, i + 1, '*')
            && !chars[close - 1].is_whitespace()
        {
            flush(&mut plain, &mut spans, base);
            let inner: String = chars[i + 1..close].iter().collect();
            spans.push(Span::styled(inner, base.add_modifier(Modifier::ITALIC)));
            i = close + 1;
            continue;
        }
        // [label](url) — show the label styled as a link; the URL stays in
        // the raw message text for the link opener (`o`).
        if chars[i] == '['
            && let Some(close_bracket) = find_char(&chars, i + 1, ']')
            && chars.get(close_bracket + 1) == Some(&'(')
            && let Some(close_paren) = find_char(&chars, close_bracket + 2, ')')
        {
            flush(&mut plain, &mut spans, base);
            let label: String = chars[i + 1..close_bracket].iter().collect();
            spans.push(Span::styled(label, link_style(t)));
            i = close_paren + 1;
            continue;
        }
        plain.push(chars[i]);
        i += 1;
    }
    flush(&mut plain, &mut spans, base);
    spans
}

/// Index of the next `needle` at or after `from`, if any.
fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == needle)
        .map(|p| from + p)
}

/// Index of the next `**` at or after `from`, if any.
fn find_double_star(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Index of the next `$$` at or after `from`, if any (display-math close).
fn find_double_dollar(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '$' && chars[i + 1] == '$' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Index of an escaped closer `\<closer>` (e.g. `\)`/`\]`) at/after `from`.
fn find_escaped(chars: &[char], from: usize, closer: char) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '\\' && chars[i + 1] == closer {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Collect `http(s)://` URLs from raw text, in order, deduplicated.
///
/// Used by the transcript link opener (`o`). A URL runs until whitespace
/// or an obvious delimiter; trailing punctuation is trimmed so prose like
/// "see https://x.org." yields `https://x.org`.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')' | ']' | '}')
                })
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            if url.len() > "https://".len() && !out.iter().any(|u| u == url) {
                out.push(url.to_string());
            }
            i += end.max(1);
        } else {
            // Advance one full character (not byte) to stay on a char boundary.
            i += rest.chars().next().map_or(1, |c| c.len_utf8());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    fn t() -> Theme {
        theme::get(theme::DEFAULT)
    }

    /// Wide default so non-table tests never trip table sizing.
    const W: u16 = 80;

    /// Concatenated visible text of a line.
    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn headers_drop_hashes_and_bold() {
        let lines = markdown_lines("## Results\nplain", t(), W);
        assert_eq!(text_of(&lines[0]), "Results");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "headers must render bold"
        );
        assert_eq!(text_of(&lines[1]), "plain");
    }

    #[test]
    fn non_header_hash_line_is_literal() {
        // "#hashtag" (no space) is not a header.
        let lines = markdown_lines("#hashtag", t(), W);
        assert_eq!(text_of(&lines[0]), "#hashtag");
    }

    #[test]
    fn bold_spans_strip_markers() {
        let lines = markdown_lines("a **bold** word", t(), W);
        assert_eq!(text_of(&lines[0]), "a bold word");
        let bold = &lines[0].spans[1];
        assert_eq!(bold.content.as_ref(), "bold");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn italic_spans_strip_markers() {
        let lines = markdown_lines("an *italic* word", t(), W);
        assert_eq!(text_of(&lines[0]), "an italic word");
        let italic = &lines[0].spans[1];
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn multiplication_stays_literal() {
        let lines = markdown_lines("2 * 3 * 4", t(), W);
        assert_eq!(text_of(&lines[0]), "2 * 3 * 4");
    }

    #[test]
    fn unmatched_markers_render_literally() {
        for s in ["**unclosed bold", "`unclosed code", "*", "[label](open"] {
            let lines = markdown_lines(s, t(), W);
            assert_eq!(text_of(&lines[0]), s, "input {s:?} must pass through");
        }
    }

    #[test]
    fn inline_code_gets_distinct_style() {
        let lines = markdown_lines("run `cargo test` now", t(), W);
        assert_eq!(text_of(&lines[0]), "run cargo test now");
        assert_eq!(lines[0].spans[1].style, code_style(t()));
    }

    #[test]
    fn bullets_become_dot_with_indent() {
        let lines = markdown_lines("- one\n  - nested", t(), W);
        assert_eq!(text_of(&lines[0]), "• one");
        assert_eq!(text_of(&lines[1]), "  • nested");
    }

    #[test]
    fn links_show_label_only() {
        let lines = markdown_lines("see [the docs](https://example.org/d) here", t(), W);
        assert_eq!(text_of(&lines[0]), "see the docs here");
        let link = &lines[0].spans[1];
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn fenced_code_blocks_skip_inline_parsing() {
        let lines = markdown_lines("```py\nx = a * b # **not bold**\n```", t(), W);
        assert_eq!(text_of(&lines[1]), "x = a * b # **not bold**");
        assert_eq!(lines[1].spans[0].style, code_style(t()));
    }

    #[test]
    fn latex_inline_renders_unicode() {
        let lines = markdown_lines(r"the $\gamma'$ phase", t(), W);
        assert_eq!(text_of(&lines[0]), "the γ′ phase");
    }

    #[test]
    fn latex_display_and_paren_forms_render() {
        assert_eq!(
            text_of(&markdown_lines(r"$$E = mc^2$$", t(), W)[0]),
            "E = mc²"
        );
        assert_eq!(
            text_of(&markdown_lines(r"energy \(E=mc^2\) here", t(), W)[0]),
            "energy E=mc² here"
        );
        assert_eq!(
            text_of(&markdown_lines(r"\[\alpha + \beta\]", t(), W)[0]),
            "α + β"
        );
    }

    #[test]
    fn lone_dollar_stays_literal() {
        let lines = markdown_lines("cost is $5 today", t(), W);
        assert_eq!(text_of(&lines[0]), "cost is $5 today");
    }

    #[test]
    fn escaped_dollar_is_literal() {
        let lines = markdown_lines(r"price \$5 net", t(), W);
        assert_eq!(text_of(&lines[0]), "price $5 net");
    }

    #[test]
    fn horizontal_rule_renders_divider() {
        let lines = markdown_lines("---", t(), W);
        assert!(text_of(&lines[0]).starts_with('─'));
    }

    #[test]
    fn extract_urls_finds_and_trims() {
        let urls = extract_urls(
            "see https://example.org/a. and [x](https://b.io/p?q=1) plus http://c.de/x, done",
        );
        assert_eq!(
            urls,
            vec![
                "https://example.org/a",
                "https://b.io/p?q=1",
                "http://c.de/x"
            ]
        );
    }

    #[test]
    fn extract_urls_dedupes_and_handles_unicode() {
        let urls = extract_urls("γ′ https://x.org and again https://x.org");
        assert_eq!(urls, vec!["https://x.org"]);
    }

    #[test]
    fn extract_urls_empty_when_none() {
        assert!(extract_urls("no links here").is_empty());
    }

    // ── GFM tables ───────────────────────────────────────────────────

    #[test]
    fn gfm_table_renders_aligned_and_bordered() {
        let md = "\
| Name | Score |
|:-----|------:|
| Al   | 3     |
| Beatrice | 100 |";
        let lines = markdown_lines(md, t(), W);
        // Border + header + separator + 2 data rows + border = 6 lines.
        assert_eq!(lines.len(), 6, "table should be 6 rendered lines");

        let top = text_of(&lines[0]);
        let bottom = text_of(&lines[5]);
        assert!(
            top.starts_with('┌') && top.ends_with('┐'),
            "top border: {top:?}"
        );
        assert!(
            bottom.starts_with('└') && bottom.ends_with('┘'),
            "bottom border: {bottom:?}"
        );

        // Every rendered row is the SAME display width — the definition of
        // "well-formed / aligned".
        let w0 = UnicodeWidthStr::width(top.as_str());
        for (n, l) in lines.iter().enumerate() {
            let w = UnicodeWidthStr::width(text_of(l).as_str());
            assert_eq!(w, w0, "row {n} width {w} != {w0}: {:?}", text_of(l));
        }

        // Header cells present and bold.
        let header = text_of(&lines[1]);
        assert!(
            header.contains("Name") && header.contains("Score"),
            "{header:?}"
        );
        assert!(
            lines[1].spans.iter().any(
                |s| s.content.contains("Name") && s.style.add_modifier.contains(Modifier::BOLD)
            )
        );

        // Right-aligned "Score" column: the value hugs the right border.
        let row = text_of(&lines[3]); // | Al | 3 |
        assert!(row.contains('3'), "{row:?}");
        assert!(row.trim_end().ends_with("3 │"), "right-align: {row:?}");
    }

    #[test]
    fn gfm_table_truncates_to_width() {
        let md = "\
| Column One | Column Two |
|------------|------------|
| some long value here | another long value |";
        let narrow = markdown_lines(md, t(), 24);
        let top = text_of(&narrow[0]);
        let w = UnicodeWidthStr::width(top.as_str());
        assert!(w <= 24, "table must fit width 24, got {w}: {top:?}");
        // Truncated cells carry the ellipsis marker.
        assert!(
            narrow.iter().any(|l| text_of(l).contains('…')),
            "narrow table should truncate with …"
        );
    }

    #[test]
    fn bare_dashes_stay_horizontal_rule_not_table() {
        // A line with `|` followed by a bare `---` (no pipe) is NOT a table.
        let lines = markdown_lines("a | b\n---", t(), W);
        assert_eq!(text_of(&lines[0]), "a | b");
        assert!(text_of(&lines[1]).starts_with('─'));
    }
}
