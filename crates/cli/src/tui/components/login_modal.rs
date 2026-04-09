#![allow(dead_code)]
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Draw a login modal when auth fails (401/token expired)
pub fn draw(f: &mut Frame, device_code: &str, verification_url: &str, area: Rect) {
    let width = 50.min(area.width.saturating_sub(4));
    let height = 12.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect::new(x, y, width, height);

    f.render_widget(Clear, modal);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Session expired",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Open in browser:",
            Style::default().fg(Color::Rgb(150, 150, 150)),
        )),
        Line::from(Span::styled(
            format!("  {verification_url}"),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Enter code:",
            Style::default().fg(Color::Rgb(150, 150, 150)),
        )),
        Line::from(Span::styled(
            format!("  {device_code}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Waiting for approval...",
            Style::default().fg(Color::Rgb(80, 80, 80)),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " Login Required ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, modal);
}

/// Draw an inline auth error banner (non-modal, at top of chat)
pub fn draw_auth_banner(f: &mut Frame, area: Rect) {
    let banner = Paragraph::new(Line::from(vec![
        Span::styled(" \u{26a0} ", Style::default().fg(Color::Yellow)),
        Span::styled(
            "Token expired — run ",
            Style::default().fg(Color::Rgb(180, 150, 50)),
        ),
        Span::styled(
            "/login",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " to re-authenticate",
            Style::default().fg(Color::Rgb(180, 150, 50)),
        ),
    ]));
    f.render_widget(banner, area);
}
