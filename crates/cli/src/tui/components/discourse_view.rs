#![allow(dead_code)]
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// A single agent's output in a discourse
#[derive(Debug, Clone)]
pub struct AgentPanel {
    pub name: String,
    pub role: String,
    pub text: String,
    pub status: String, // "thinking", "done", "waiting"
}

/// Draw a discourse with parallel agent panels side by side
pub fn draw(f: &mut Frame, agents: &[AgentPanel], area: Rect) {
    if agents.is_empty() {
        let p = Paragraph::new("No discourse agents active")
            .block(Block::default().borders(Borders::ALL).title(" Discourse "));
        f.render_widget(p, area);
        return;
    }

    // Split area into N columns
    let constraints: Vec<Constraint> = agents
        .iter()
        .map(|_| Constraint::Ratio(1, agents.len() as u32))
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (i, agent) in agents.iter().enumerate() {
        let status_color = match agent.status.as_str() {
            "thinking" => Color::Yellow,
            "done" => Color::Green,
            _ => Color::Rgb(80, 80, 80),
        };

        let status_icon = match agent.status.as_str() {
            "thinking" => "\u{2026}", // …
            "done" => "\u{2713}",     // ✓
            _ => "\u{25cb}",          // ○
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
            .title(Line::from(vec![
                Span::styled(
                    format!(" {status_icon} "),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    &agent.name,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" ({}) ", agent.role),
                    Style::default().fg(Color::Rgb(100, 100, 100)),
                ),
            ]));

        let p = Paragraph::new(agent.text.clone())
            .block(block)
            .wrap(Wrap { trim: true });

        f.render_widget(p, chunks[i]);
    }
}
