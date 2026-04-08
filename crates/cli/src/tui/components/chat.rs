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
                let icon = if c.card_type == "error" {
                    Span::styled("\u{2717} ", Style::default().fg(Color::Red))
                } else {
                    Span::styled("\u{2713} ", Style::default().fg(Color::Green))
                };
                lines.push(Line::from(vec![
                    icon,
                    Span::styled(
                        format!("{} ", c.tool_name),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(c.content.clone()),
                    Span::styled(
                        format!(" ({}ms)", c.elapsed_ms),
                        Style::default().fg(Color::Rgb(80, 80, 80)),
                    ),
                ]));
            }
        }
    }

    // Streaming text (in progress, not yet flushed)
    if !app.streaming_text.is_empty() {
        lines.extend(markdown::render_markdown(&app.streaming_text));
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

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true })
        .scroll((app.chat_scroll, 0));

    f.render_widget(p, area);
}
