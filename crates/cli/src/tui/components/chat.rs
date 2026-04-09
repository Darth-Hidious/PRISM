use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::markdown;
use crate::tui::state::{App, ChatElement};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let mut last_was_user = false;

    for element in &app.chat_history {
        match element {
            ChatElement::UserMessage(msg) => {
                // Separator before user message (if not first)
                if !lines.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                        Style::default().fg(Color::Rgb(35, 35, 35)),
                    )));
                }
                lines.push(Line::from(vec![Span::styled(
                    " \u{25cf} you ",
                    Style::default()
                        .fg(Color::Rgb(0, 200, 255))
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )]));
                lines.push(Line::from(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(Color::White),
                )));
                lines.push(Line::from(""));
                last_was_user = true;
            }
            ChatElement::Text(t) => {
                // Agent label before first text after user message
                if last_was_user {
                    lines.push(Line::from(vec![Span::styled(
                        " \u{25cf} prism ",
                        Style::default()
                            .fg(Color::Rgb(0, 255, 100))
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    )]));
                    last_was_user = false;
                }
                // Parse markdown → styled spans, indent by 1 space
                for line in markdown::render_markdown(t) {
                    let mut indented = vec![Span::raw(" ".to_string())];
                    indented.extend(line.spans);
                    lines.push(Line::from(indented));
                }
                lines.push(Line::from(""));
            }
            ChatElement::Cost(c) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  \u{25cb} {}in/{}out ", c.input_tokens, c.output_tokens),
                        Style::default().fg(Color::Rgb(50, 50, 50)),
                    ),
                    Span::styled(
                        format!("${:.4}", c.turn_cost),
                        Style::default().fg(Color::Rgb(70, 70, 70)),
                    ),
                ]));
                last_was_user = false;
            }
            ChatElement::ToolStart(ts) => {
                if last_was_user {
                    lines.push(Line::from(vec![Span::styled(
                        " \u{25cf} prism ",
                        Style::default()
                            .fg(Color::Rgb(0, 255, 100))
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    )]));
                    last_was_user = false;
                }
                lines.push(Line::from(vec![
                    Span::styled("  \u{26a1} ", Style::default().fg(Color::Yellow)),
                    Span::styled(ts.verb.clone(), Style::default().fg(Color::Cyan)),
                    Span::styled(
                        if let Some(ref preview) = ts.preview {
                            format!(" ({preview})")
                        } else {
                            String::new()
                        },
                        Style::default().fg(Color::Rgb(80, 80, 80)),
                    ),
                ]));
            }
            ChatElement::Card(c) => {
                last_was_user = false;
                let is_error = c.card_type == "error";
                let border_color = if is_error {
                    Color::Rgb(180, 60, 60)
                } else {
                    Color::Rgb(50, 80, 50)
                };
                let icon = if is_error { "\u{2717}" } else { "\u{2713}" };
                let icon_color = if is_error { Color::Red } else { Color::Green };

                // Tool card header
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  \u{250c}\u{2500} {icon} "),
                        Style::default().fg(border_color),
                    ),
                    Span::styled(
                        c.tool_name.clone(),
                        Style::default()
                            .fg(icon_color)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" \u{2500} {}ms \u{2500}\u{2510}", c.elapsed_ms),
                        Style::default().fg(Color::Rgb(60, 60, 60)),
                    ),
                ]));

                // Tool card content — render as code/markdown
                let content = &c.content;
                if content.len() > 200 {
                    // Long content: show first 3 lines + truncation
                    for line in content.lines().take(4) {
                        lines.push(Line::from(vec![
                            Span::styled("  \u{2502} ", Style::default().fg(border_color)),
                            Span::styled(
                                if line.len() > 100 {
                                    format!("{}...", &line[..97])
                                } else {
                                    line.to_string()
                                },
                                Style::default().fg(Color::Rgb(170, 170, 170)),
                            ),
                        ]));
                    }
                    if content.lines().count() > 4 {
                        lines.push(Line::from(vec![
                            Span::styled("  \u{2502} ", Style::default().fg(border_color)),
                            Span::styled(
                                format!("... {} more lines", content.lines().count() - 4),
                                Style::default().fg(Color::Rgb(80, 80, 80)),
                            ),
                        ]));
                    }
                } else {
                    // Short content: show all
                    for line in content.lines() {
                        lines.push(Line::from(vec![
                            Span::styled("  \u{2502} ", Style::default().fg(border_color)),
                            Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::Rgb(170, 170, 170)),
                            ),
                        ]));
                    }
                }

                // Tool card footer
                lines.push(Line::from(Span::styled(
                    "  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}",
                    Style::default().fg(border_color),
                )));
                lines.push(Line::from(""));
            }
        }
    }

    // Streaming text (in progress, not yet flushed)
    if !app.streaming_text.is_empty() {
        if last_was_user {
            lines.push(Line::from(vec![Span::styled(
                " \u{25cf} prism ",
                Style::default()
                    .fg(Color::Rgb(0, 255, 100))
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )]));
        }
        // Show streaming indicator
        lines.push(Line::from(vec![
            Span::styled(
                " \u{2591}\u{2592}\u{2593} ",
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("streaming...", Style::default().fg(Color::Rgb(80, 80, 80))),
        ]));
        for line in markdown::render_markdown(&app.streaming_text) {
            let mut indented = vec![Span::raw(" ".to_string())];
            indented.extend(line.spans);
            lines.push(Line::from(indented));
        }
    }

    let is_focused = app.focus == crate::tui::state::FocusZone::Chat;
    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::Rgb(70, 70, 70)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            if is_focused {
                " PRISM Agent (↑↓ scroll) "
            } else {
                " PRISM Agent "
            },
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));

    // Clamp scroll: content stays at top until it overflows the viewport,
    // then follows new content. inner_area height = area minus borders (2 lines).
    let viewport_height = area.height.saturating_sub(2) as usize;
    let total_lines = lines.len();
    let max_scroll = if total_lines > viewport_height {
        (total_lines - viewport_height) as u16
    } else {
        0
    };
    let clamped_scroll = app.chat_scroll.min(max_scroll);

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true })
        .scroll((clamped_scroll, 0));

    f.render_widget(p, area);
}
