use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::state::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let mode = app
        .status
        .as_ref()
        .map(|s| s.session_mode.as_str())
        .unwrap_or("idle");

    let model = app
        .status
        .as_ref()
        .and_then(|s| s.model.as_deref())
        .unwrap_or("none");

    let msg_count = app.chat_history.len();

    let sep = Span::styled(" \u{2502} ", Style::default().fg(Color::Rgb(60, 60, 60)));

    let line = Line::from(vec![
        // Mode badge
        Span::styled(
            format!(" {mode} "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        sep.clone(),
        // Model
        Span::styled(model, Style::default().fg(Color::Rgb(200, 200, 200))),
        sep.clone(),
        // Messages
        Span::styled(
            format!("{msg_count} msgs"),
            Style::default().fg(Color::Rgb(150, 150, 150)),
        ),
        sep.clone(),
        // Cost
        Span::styled(
            format!("${:.4}", app.total_cost),
            Style::default().fg(Color::Rgb(150, 150, 150)),
        ),
        sep.clone(),
        // Tools
        Span::styled(
            format!("{} tools", app.model_count.unwrap_or(106)),
            Style::default().fg(Color::Rgb(130, 130, 130)),
        ),
        // Shortcuts
        Span::styled(
            "  Ctrl+E sidebar \u{2502} Ctrl+1-9 tabs \u{2502} / commands",
            Style::default().fg(Color::Rgb(80, 80, 80)),
        ),
    ]);

    f.render_widget(Paragraph::new(line), area);
}
