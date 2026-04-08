use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;

use crate::tui::markdown;
use crate::tui::state::App;

/// Draw a view panel inside the main content area (NOT as overlay).
/// Used for /models, /tools, /deploy, etc. when a workspace is active.
pub fn draw_view_panel(f: &mut Frame, app: &App, area: Rect) {
    let Some(ref view) = app.active_view else {
        return;
    };

    let tabs = &view.tabs;
    let tab_idx = app.view_tab_index.min(tabs.len().saturating_sub(1));
    let active_tab = tabs.get(tab_idx);
    let body = active_tab.map(|t| t.body.as_str()).unwrap_or("");

    // Layout: [tab bar | content | footer]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if tabs.len() > 1 { 2 } else { 0 }),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    // Tab bar
    if tabs.len() > 1 {
        let tab_titles: Vec<Line> = tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let style = if i == tab_idx {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Rgb(80, 80, 80))
                };
                Line::from(Span::styled(format!(" {} ", t.title), style))
            })
            .collect();

        let tab_bar = Tabs::new(tab_titles)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(Color::Rgb(40, 40, 40))),
            )
            .select(tab_idx)
            .highlight_style(Style::default().fg(Color::Cyan));

        f.render_widget(tab_bar, chunks[0]);
    }

    // Content — render markdown
    let lines = markdown::render_markdown(body);
    let scroll = app.view_scroll;

    let content = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(50, 50, 50)))
                .title(Span::styled(
                    format!(" {} ", view.title),
                    Style::default().fg(Color::White),
                )),
        )
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0));

    f.render_widget(content, chunks[1]);

    // Footer
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" tab", Style::default().fg(Color::Rgb(80, 80, 80))),
        Span::styled(" switch ", Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled(
            "\u{2191}\u{2193}",
            Style::default().fg(Color::Rgb(80, 80, 80)),
        ),
        Span::styled(" scroll ", Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled("esc", Style::default().fg(Color::Rgb(80, 80, 80))),
        Span::styled(" close", Style::default().fg(Color::Rgb(50, 50, 50))),
    ]));
    f.render_widget(footer, chunks[2]);
}

/// Draw approval modal — centered overlay that blocks everything
pub fn draw_approval(f: &mut Frame, app: &App, area: Rect) {
    let Some(ref prompt) = app.active_prompt else {
        return;
    };

    // Center a box
    let width = 60.min(area.width.saturating_sub(4));
    let height = 12.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, modal_area);

    let permission_color = match prompt.permission_mode.as_deref() {
        Some("full-access") => Color::Red,
        Some("workspace-write") => Color::Yellow,
        _ => Color::Cyan,
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", prompt.message),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  Tool: ",
                Style::default().fg(Color::Rgb(100, 100, 100)),
            ),
            Span::styled(&prompt.tool_name, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled(
                "  Mode: ",
                Style::default().fg(Color::Rgb(100, 100, 100)),
            ),
            Span::styled(
                prompt.permission_mode.as_deref().unwrap_or("unknown"),
                Style::default().fg(permission_color),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [y]", Style::default().fg(Color::Green)),
            Span::raw(" Allow  "),
            Span::styled("[n]", Style::default().fg(Color::Red)),
            Span::raw(" Deny  "),
            Span::styled("[a]", Style::default().fg(Color::Cyan)),
            Span::raw(" Allow all  "),
            Span::styled("[b]", Style::default().fg(Color::Rgb(180, 80, 80))),
            Span::raw(" Block"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " Approval Required ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, modal_area);
}
