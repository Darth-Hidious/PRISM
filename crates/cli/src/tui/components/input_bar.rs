use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::tui::state::{App, FocusZone};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focus == FocusZone::Input;

    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::Rgb(70, 70, 70)
    };

    let prompt = if is_focused { "> " } else { "  " };

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let p = Paragraph::new(format!("{prompt}{}", app.input_buffer)).block(input_block);

    f.render_widget(p, area);

    // Show cursor at correct position when input is focused
    if is_focused && app.active_view.is_none() && app.active_prompt.is_none() {
        // Count display width up to cursor position
        let display_chars = app.input_buffer[..app.input_cursor].chars().count() as u16;
        f.set_cursor(area.x + 2 + display_chars, area.y + 1);
    }
}
