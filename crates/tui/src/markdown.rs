//! Minimal markdown → ratatui converter for assistant transcript text.
//!
//! Hand-rolled on purpose (no markdown crate): the transcript only needs
//! the subset models actually emit — headers, bullets, bold/italic,
//! inline code, fenced code blocks, and links. The input arrives as a
//! stream, so the parser must be resilient to partially-streamed
//! markdown: any unmatched marker renders literally instead of eating
//! text. LaTeX and other notations pass through untouched.
//!
//! All colors come from the active [`Theme`] so markdown recolors with
//! the rest of the UI.

use crate::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

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

/// Convert a markdown message to styled ratatui lines.
///
/// Supported (deliberately small): `#`–`######` headers, `-`/`*`/`+`
/// bullets, `**bold**`, `*italic*`, `` `code` ``, ``` fenced blocks,
/// `[label](url)` links (label shown, URL kept in the raw message for
/// the link opener), and `---` rules. Everything else is passed through
/// with the base text style.
pub fn markdown_lines(text: &str, t: Theme) -> Vec<Line<'static>> {
    let base = Style::default().fg(t.text);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;

    for raw in text.lines() {
        let trimmed = raw.trim_start();

        // Fenced code block delimiters toggle code mode. The fence line
        // itself renders dimmed so the block stays visually delimited.
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            out.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(t.muted),
            )));
            continue;
        }
        if in_code_block {
            out.push(Line::from(Span::styled(raw.to_string(), code_style(t))));
            continue;
        }

        // Headers: strip the #'s, render bold in the accent color.
        if let Some(rest) = header_text(trimmed) {
            out.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // Horizontal rule.
        if trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-') {
            out.push(Line::from(Span::styled(
                "─".repeat(24),
                Style::default().fg(t.divider),
            )));
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
            continue;
        }

        out.push(Line::from(inline_spans(raw, t, base)));
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

    /// Concatenated visible text of a line.
    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn headers_drop_hashes_and_bold() {
        let lines = markdown_lines("## Results\nplain", t());
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
        let lines = markdown_lines("#hashtag", t());
        assert_eq!(text_of(&lines[0]), "#hashtag");
    }

    #[test]
    fn bold_spans_strip_markers() {
        let lines = markdown_lines("a **bold** word", t());
        assert_eq!(text_of(&lines[0]), "a bold word");
        let bold = &lines[0].spans[1];
        assert_eq!(bold.content.as_ref(), "bold");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn italic_spans_strip_markers() {
        let lines = markdown_lines("an *italic* word", t());
        assert_eq!(text_of(&lines[0]), "an italic word");
        let italic = &lines[0].spans[1];
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn multiplication_stays_literal() {
        let lines = markdown_lines("2 * 3 * 4", t());
        assert_eq!(text_of(&lines[0]), "2 * 3 * 4");
    }

    #[test]
    fn unmatched_markers_render_literally() {
        for s in ["**unclosed bold", "`unclosed code", "*", "[label](open"] {
            let lines = markdown_lines(s, t());
            assert_eq!(text_of(&lines[0]), s, "input {s:?} must pass through");
        }
    }

    #[test]
    fn inline_code_gets_distinct_style() {
        let lines = markdown_lines("run `cargo test` now", t());
        assert_eq!(text_of(&lines[0]), "run cargo test now");
        assert_eq!(lines[0].spans[1].style, code_style(t()));
    }

    #[test]
    fn bullets_become_dot_with_indent() {
        let lines = markdown_lines("- one\n  - nested", t());
        assert_eq!(text_of(&lines[0]), "• one");
        assert_eq!(text_of(&lines[1]), "  • nested");
    }

    #[test]
    fn links_show_label_only() {
        let lines = markdown_lines("see [the docs](https://example.org/d) here", t());
        assert_eq!(text_of(&lines[0]), "see the docs here");
        let link = &lines[0].spans[1];
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn fenced_code_blocks_skip_inline_parsing() {
        let lines = markdown_lines("```py\nx = a * b # **not bold**\n```", t());
        assert_eq!(text_of(&lines[1]), "x = a * b # **not bold**");
        assert_eq!(lines[1].spans[0].style, code_style(t()));
    }

    #[test]
    fn latex_passes_through_untouched() {
        let lines = markdown_lines(r"the $\gamma'$ phase", t());
        assert_eq!(text_of(&lines[0]), r"the $\gamma'$ phase");
    }

    #[test]
    fn horizontal_rule_renders_divider() {
        let lines = markdown_lines("---", t());
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
}
