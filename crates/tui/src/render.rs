//! Ratatui rendering — pure view function.

use crate::app::{App, ChatLine, Focus, LineKind, Role};
use ratatui::layout::{Constraint, Direction, Layout, Rect, Alignment};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap, Scrollbar, ScrollbarOrientation};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),   // chat scrollback
            Constraint::Length(1), // status bar
            Constraint::Length(3), // input
        ])
        .split(f.area());

    draw_chat(f, app, chunks[0]);
    draw_status_bar(f, app, chunks[1]);
    draw_input(f, app, chunks[2]);

    // Approval popup overlay (if pending)
    if app.approval_pending.is_some() {
        draw_approval_popup(f, app);
    }
}

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let mut thinking_shown = false;

    for msg in &app.messages {
        // Thinking tokens: show collapsed indicator or full text
        if matches!(msg.kind, LineKind::Thinking) {
            if app.thinking_expanded {
                // Show full thinking text, dimmed
                for (i, line_text) in msg.text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            Span::styled("◇ ", Style::default().fg(Color::DarkGray)),
                            Span::styled(line_text.to_string(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(line_text.to_string(), Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
                        ]));
                    }
                }
                if lines.last().is_some() {
                    lines.push(Line::raw(""));
                }
            } else if !thinking_shown {
                // Show a single collapsed indicator
                let char_count = msg.text.chars().count();
                lines.push(Line::from(vec![
                    Span::styled("◇ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("[thinking… {} chars — Ctrl-T to expand]", char_count),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                    ),
                ]));
                thinking_shown = true;
            }
            continue;
        }

        let prefix = match msg.role {
            Role::User => Span::styled("> ", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
            Role::Assistant => Span::styled("◆ ", Style::default().fg(Color::Cyan)),
            Role::System => Span::styled("· ", Style::default().fg(Color::DarkGray)),
            Role::Tool => Span::styled("⚙ ", Style::default().fg(Color::Yellow)),
        };

        let style = match &msg.kind {
            LineKind::Error(_) => Style::default().fg(Color::Red),
            LineKind::Status(_) => Style::default().fg(Color::DarkGray),
            LineKind::ToolStart { .. } => Style::default().fg(Color::Yellow),
            LineKind::ToolResult { success: true, .. } => Style::default().fg(Color::Green),
            LineKind::ToolResult { success: false, .. } => Style::default().fg(Color::Red),
            LineKind::Approval { .. } => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            LineKind::View { .. } => Style::default().fg(Color::Blue),
            LineKind::Thinking => Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            LineKind::Text if matches!(msg.role, Role::User) => Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            LineKind::Text if matches!(msg.role, Role::Assistant) => Style::default().fg(Color::Cyan),
            LineKind::Text => Style::default(),
        };

        for (i, line_text) in msg.text.lines().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    prefix.clone(),
                    Span::styled(line_text.to_string(), style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(line_text.to_string(), style),
                ]));
            }
        }
        // Blank line between messages
        if lines.last().is_some() {
            lines.push(Line::raw(""));
        }
    }

    // If waiting and no tokens yet, show a loading spinner
    if app.is_waiting && app.first_token_time.is_none() {
        let spinner = match std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() % 4)
            .unwrap_or(0)
        {
            0 => "⠋",
            1 => "⠙",
            2 => "⠹",
            _ => "⠸",
        };
        lines.push(Line::from(vec![
            Span::styled("◆ ", Style::default().fg(Color::Cyan)),
            Span::styled(spinner, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" waiting for response…", Style::default().fg(Color::DarkGray)),
        ]));
    } else if app.is_waiting {
        // Streaming — show pulse
        lines.push(Line::from(vec![
            Span::styled("◆ ", Style::default().fg(Color::Cyan)),
            Span::styled("…", Style::default().fg(Color::Cyan).add_modifier(Modifier::SLOW_BLINK)),
        ]));
    }

    let title = if app.tool_count > 0 {
        format!(" PRISM v{} · {} tools ", app.prism_version, app.tool_count)
    } else {
        " PRISM ".to_string()
    };

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(Span::styled(
                    title,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));

    f.render_widget(paragraph, area);

    // Render scrollbar if focused on chat
    if app.focus == Focus::Chat {
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut ratatui::widgets::ScrollbarState::new(app.messages.len()).position(app.scroll_offset as usize),
        );
    }
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let model_display = if app.model.is_empty() { "—" } else { &app.model };
    let status = if app.is_waiting { "busy" } else { &app.status_text };
    let focus_indicator = match app.focus {
        Focus::Chat => " [CHAT] ",
        Focus::Input => " [INPUT] ",
        Focus::Approval => " [APPROVAL] ",
    };

    let mut spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(format!(" {} ", status), Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::raw(" "),
        Span::styled("model:", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(model_display, Style::default().fg(Color::White)),
        Span::raw("  "),
    ];

    // Show tokens/sec when streaming (if metrics enabled)
    if app.show_metrics && app.tokens_per_sec > 0.0 {
        spans.push(Span::styled("tok/s:", Style::default().fg(Color::DarkGray)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{:.1}", app.tokens_per_sec),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::raw("  "));
    }

    // Show cost only if enabled (hide for local models)
    if app.show_cost && app.session_cost > 0.0 {
        spans.push(Span::styled("cost:", Style::default().fg(Color::DarkGray)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("${:.4}", app.session_cost),
            Style::default().fg(Color::White),
        ));
        spans.push(Span::raw("  "));
    }

    // Show collapsed thinking indicator
    let has_thinking = app.messages.iter().any(|m| matches!(m.kind, LineKind::Thinking));
    if has_thinking && !app.thinking_expanded {
        spans.push(Span::styled("[thinking hidden]", Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)));
        spans.push(Span::raw("  "));
    }

    spans.push(Span::styled(focus_indicator, Style::default().fg(Color::Yellow)));

    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    if app.focus == Focus::Input {
        // Render the textarea widget directly
        f.render_widget(&app.input, area);
    } else {
        // Show a dimmed placeholder when not focused
        let text = app.input.lines().join(" ");
        let display = if text.is_empty() {
            "Type a message... (press 'i' to focus input)".to_string()
        } else {
            text
        };
        let para = Paragraph::new(display)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP));
        f.render_widget(para, area);
    }
}

fn draw_approval_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 20, f.area());
    f.render_widget(Clear, area);

    let (tool, message) = app.approval_pending.as_ref().unwrap();

    let popup = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  ⚠ APPROVAL REQUIRED  ", Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Tool: "),
            Span::styled(tool, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(message, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::raw("  [y] "),
            Span::styled("Allow", Style::default().fg(Color::Green)),
            Span::raw("   [a] "),
            Span::styled("Allow all", Style::default().fg(Color::Yellow)),
            Span::raw("   [n] "),
            Span::styled("Deny", Style::default().fg(Color::Red)),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    )
    .alignment(Alignment::Left);

    f.render_widget(popup, area);
}

/// Helper: centered rect for popups.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}