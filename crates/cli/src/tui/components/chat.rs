use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::markdown;
use crate::tui::state::{App, ChatElement};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for element in &app.chat_history {
        match element {
            ChatElement::UserMessage(msg) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "\u{25cf} you: ",
                        Style::default().fg(Color::Rgb(0, 200, 255)),
                    ),
                    Span::styled(msg.clone(), Style::default().fg(Color::White)),
                ]));
                lines.push(Line::from(""));
            }
            ChatElement::Text(t) => {
                // Parse markdown → styled spans
                lines.extend(markdown::render_markdown(t));
                lines.push(Line::from(""));
            }
            ChatElement::Cost(c) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {}in/{}out ", c.input_tokens, c.output_tokens),
                        Style::default().fg(Color::Rgb(60, 60, 60)),
                    ),
                    Span::styled(
                        format!("${:.4}", c.turn_cost),
                        Style::default().fg(Color::Rgb(80, 80, 80)),
                    ),
                ]));
            }
            ChatElement::ToolStart(ts) => {
                lines.push(Line::from(vec![
                    Span::styled("\u{26a1} ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("Running {}", ts.tool_name),
                        Style::default().fg(Color::Cyan),
                    ),
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(50, 50, 50)))
        .title(Span::styled(
            " PRISM Agent ",
            Style::default().fg(Color::Cyan),
        ));

    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });

    f.render_widget(p, area);
}
