use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

use super::{chat, input_bar, overlays, sidebar, status_bar};
use crate::tui::state::{App, Workspace};

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.size();

    // ┌───┬──────────┬────────────────────────────────────┐
    // │ A │ Sidebar  │ Main content                       │
    // │ c │ Panel    │ (chat / models / mesh / etc.)      │
    // │ t │          │                                    │
    // │   │          ├────────────────────────────────────┤
    // │   │          │ Input                              │
    // ├───┴──────────┴────────────────────────────────────┤
    // │ Status bar                                        │
    // └───────────────────────────────────────────────────┘

    // Vertical: [content area | status bar]
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(size);

    let content_area = v_chunks[0];
    let status_area = v_chunks[1];

    // Horizontal: [activity bar | sidebar panel | main]
    let h_constraints = if app.sidebar_visible {
        vec![
            Constraint::Length(3),  // Activity bar (icons)
            Constraint::Length(25), // Sidebar panel
            Constraint::Min(0),    // Main content
        ]
    } else {
        vec![
            Constraint::Length(3),  // Activity bar always visible
            Constraint::Length(0),  // Sidebar hidden
            Constraint::Min(0),    // Main content
        ]
    };

    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(h_constraints)
        .split(content_area);

    let activity_area = h_chunks[0];
    let sidebar_area = h_chunks[1];
    let main_area = h_chunks[2];

    // Activity bar (always visible)
    sidebar::draw_activity_bar(f, app, activity_area);

    // Sidebar panel (when visible)
    if app.sidebar_visible {
        sidebar::draw_panel(f, app, sidebar_area);
    }

    // Main content: depends on workspace
    match app.workspace {
        Workspace::Chat => {
            // Chat gets input bar
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(main_area);

            chat::draw(f, app, main_chunks[0]);
            input_bar::draw(f, app, main_chunks[1]);
        }
        _ => {
            // Other workspaces: show view panel or placeholder
            // Views from slash commands render here, not as overlays
            if let Some(ref _view) = app.active_view {
                overlays::draw_view_panel(f, app, main_area);
            } else {
                // Placeholder for workspace content
                let placeholder = ratatui::widgets::Paragraph::new(format!(
                    " {} workspace\n\n Use slash commands or press Enter to load data.",
                    app.current_activity().label()
                ))
                .block(
                    ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .border_style(ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(50, 50, 50)))
                        .title(format!(" {} ", app.current_activity().label())),
                );
                f.render_widget(placeholder, main_area);
            }
        }
    }

    // Status bar (always visible)
    status_bar::draw(f, app, status_area);

    // Approval modal (overlays everything when active)
    if app.active_prompt.is_some() {
        overlays::draw_approval(f, app, size);
    }
}
