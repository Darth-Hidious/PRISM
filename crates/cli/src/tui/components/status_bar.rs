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
        .map(|s| s.session_mode.clone())
        .unwrap_or_else(|| "Idle".into());
    let model = app
        .status
        .as_ref()
        .and_then(|s| s.model.clone())
        .unwrap_or_else(|| "Unknown".into());

    let line = Line::from(vec![
        Span::styled(
            format!(" MODE: {} ", mode),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | "),
        Span::styled(
            format!(" MODEL: {} ", model),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" | [Ctrl+Q] Quit | [Ctrl+E] Toggle Sidebar | [ESC] Close Overlays"),
    ]);

    f.render_widget(Paragraph::new(line), area);
}
