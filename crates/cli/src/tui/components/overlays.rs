use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs};
use ratatui::style::{Style, Color, Modifier};
use ratatui::text::{Line, Span};

use crate::tui::state::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if let Some(view) = &app.active_view {
        draw_view_panel(f, view, area);
    } else if let Some(prompt) = &app.active_prompt {
        draw_prompt_modal(f, prompt, area);
    }
}

fn draw_view_panel(f: &mut Frame, view: &crate::tui::protocol::UiView, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", view.title));

    let inner_area = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(inner_area);

    let tab_titles: Vec<Line> = view.tabs.iter()
        .map(|t| Line::from(t.title.clone()))
        .collect();

    // Default to 0 if not found
    let selected_index = view.tabs.iter().position(|t| t.id == view.selected_tab).unwrap_or(0);

    let tabs = Tabs::new(tab_titles)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan))
        .select(selected_index);

    f.render_widget(tabs, chunks[0]);

    if let Some(tab) = view.tabs.get(selected_index) {
        let content = Paragraph::new(tab.body.clone());
        f.render_widget(content, chunks[1]);
    }
}

fn draw_prompt_modal(f: &mut Frame, prompt: &crate::tui::protocol::UiPrompt, area: Rect) {
    let modal_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(40),
            Constraint::Percentage(30),
        ])
        .split(area)[1];

    let modal_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(60),
            Constraint::Percentage(20),
        ])
        .split(modal_area)[1];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Approval Required ")
        .border_style(Style::default().fg(Color::Yellow));

    let content = vec![
        Line::from(prompt.message.clone()),
        Line::from(vec![
            Span::styled(format!("Tool: {}", prompt.tool_name), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(format!("Args: {:?}", prompt.tool_args)),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [Y]es ", Style::default().fg(Color::Green)),
            Span::styled(" [N]o ", Style::default().fg(Color::Red)),
            Span::styled(" [A]llow All ", Style::default().fg(Color::LightGreen)),
            Span::styled(" [B]lock All ", Style::default().fg(Color::LightRed)),
        ]),
    ];

    let p = Paragraph::new(content).block(block);

    f.render_widget(Clear, modal_area);
    f.render_widget(p, modal_area);
}
