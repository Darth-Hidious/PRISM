use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::state::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let display_idx = app.input_buffer.len() as u16;

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(70, 70, 70)));

    let p = Paragraph::new(format!("> {}", app.input_buffer)).block(input_block);

    f.render_widget(p, area);

    // Position cursor
    if app.active_view.is_none() && app.active_prompt.is_none() {
        // Safe to cast, assuming input fits on screen for this prototype.
        f.set_cursor(area.x + 2 + display_idx, area.y + 1);
    }
}
