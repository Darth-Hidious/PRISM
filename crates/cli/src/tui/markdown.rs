//! Lightweight markdown → ratatui styled spans converter.
//!
//! Handles: **bold**, *italic*, `code`, ```code blocks```, # headings.
//! Not a full parser — just enough to make LLM output look good in a terminal.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Convert a markdown string into styled ratatui Lines.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let mut in_code_block = false;

    for raw_line in text.lines() {
        // Code block toggle
        if raw_line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                lines.push(Line::from(Span::styled(
                    "┌─────────────────────────────────────────",
                    Style::default().fg(Color::Rgb(60, 60, 60)),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "└─────────────────────────────────────────",
                    Style::default().fg(Color::Rgb(60, 60, 60)),
                )));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                format!("│ {raw_line}"),
                Style::default().fg(Color::Rgb(180, 220, 180)),
            )));
            continue;
        }

        // Headings
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("### ") {
            lines.push(Line::from(Span::styled(
                trimmed[4..].to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if trimmed.starts_with("## ") {
            lines.push(Line::from(Span::styled(
                trimmed[3..].to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if trimmed.starts_with("# ") {
            lines.push(Line::from(Span::styled(
                trimmed[2..].to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // Inline formatting
        lines.push(Line::from(parse_inline_markdown(raw_line)));
    }

    lines
}

/// Parse inline markdown: **bold**, *italic*, `code`
fn parse_inline_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        // Bold: **text**
        if let Some(start) = rest.find("**") {
            if start > 0 {
                spans.push(Span::raw(rest[..start].to_string()));
            }
            let after = &rest[start + 2..];
            if let Some(end) = after.find("**") {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 2..];
                continue;
            } else {
                spans.push(Span::raw(rest[start..].to_string()));
                break;
            }
        }

        // Inline code: `text`
        if let Some(start) = rest.find('`') {
            if start > 0 {
                spans.push(Span::raw(rest[..start].to_string()));
            }
            let after = &rest[start + 1..];
            if let Some(end) = after.find('`') {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default().fg(Color::Rgb(180, 220, 180)),
                ));
                rest = &after[end + 1..];
                continue;
            } else {
                spans.push(Span::raw(rest[start..].to_string()));
                break;
            }
        }

        // Italic: *text* (only if not **)
        if let Some(start) = rest.find('*') {
            if start > 0 {
                spans.push(Span::raw(rest[..start].to_string()));
            }
            let after = &rest[start + 1..];
            if let Some(end) = after.find('*') {
                spans.push(Span::styled(
                    after[..end].to_string(),
                    Style::default().add_modifier(Modifier::ITALIC),
                ));
                rest = &after[end + 1..];
                continue;
            } else {
                spans.push(Span::raw(rest[start..].to_string()));
                break;
            }
        }

        // No more markers — emit rest as plain text
        spans.push(Span::raw(rest.to_string()));
        break;
    }

    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }

    spans
}
