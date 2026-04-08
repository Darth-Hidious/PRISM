use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Tabs};
use ratatui::Frame;

use super::{chat, command_palette, input_bar, model_picker, overlays, sidebar, status_bar};
use crate::tui::state::{Activity, App, Workspace};

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.size();

    // ┌─ Tab bar ────────────────────────────────────┐
    // ├──────────┬───────────────────────────────────┤
    // │ Sidebar  │ Main content                      │
    // │          ├───────────────────────────────────┤
    // │          │ Input                             │
    // ├──────────┴───────────────────────────────────┤
    // │ Status bar                                   │
    // └──────────────────────────────────────────────┘

    // Vertical: [tab bar | content | status bar]
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Tab bar
            Constraint::Min(0),    // Content area
            Constraint::Length(1), // Status bar
        ])
        .split(size);

    let tab_area = v_chunks[0];
    let content_area = v_chunks[1];
    let status_area = v_chunks[2];

    // Tab bar — horizontal workspace tabs
    let activities = Activity::all();
    let tab_titles: Vec<Line> = activities
        .iter()
        .map(|a| Line::from(Span::raw(format!(" {} {} ", a.icon(), a.label()))))
        .collect();

    let tabs = Tabs::new(tab_titles)
        .select(app.activity_bar_idx)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().fg(Color::Rgb(140, 140, 140)))
        .divider(Span::styled(
            "\u{2502}",
            Style::default().fg(Color::Rgb(60, 60, 60)),
        ));

    f.render_widget(tabs, tab_area);

    // Horizontal: [sidebar | main]
    let h_constraints = if app.sidebar_visible {
        vec![Constraint::Length(26), Constraint::Min(0)]
    } else {
        vec![Constraint::Length(0), Constraint::Min(0)]
    };

    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(h_constraints)
        .split(content_area);

    let sidebar_area = h_chunks[0];
    let main_area = h_chunks[1];

    // Sidebar
    if app.sidebar_visible {
        sidebar::draw_panel(f, app, sidebar_area);
    }

    // Main content: depends on workspace
    match app.workspace {
        Workspace::Chat => {
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(main_area);

            chat::draw(f, app, main_chunks[0]);
            input_bar::draw(f, app, main_chunks[1]);

            // Command palette (autocomplete popup above input)
            if app.palette_visible {
                let commands = command_palette::all_commands();
                let query = app
                    .input_buffer
                    .strip_prefix('/')
                    .unwrap_or(&app.input_buffer);
                let filtered = command_palette::filter_commands(&commands, query);
                command_palette::draw(f, &filtered, app.palette_selected, main_chunks[1]);
            }

            // Model picker (replaces main area when active)
            if app.model_picker_visible && !app.cached_models.is_empty() {
                let provider_filter = if app.model_picker_provider_idx == 0 {
                    None
                } else {
                    app.cached_providers
                        .get(app.model_picker_provider_idx - 1)
                        .map(|s| s.as_str())
                };
                let filtered = model_picker::filter_models(
                    &app.cached_models,
                    &app.model_picker_search,
                    provider_filter,
                );
                let filtered_refs: Vec<&model_picker::ModelInfo> = filtered.into_iter().collect();
                model_picker::draw(
                    f,
                    &filtered_refs,
                    app.model_picker_selected,
                    &app.model_picker_search,
                    provider_filter,
                    &app.cached_providers,
                    main_chunks[0],
                );
            }
        }
        _ => {
            if let Some(ref _view) = app.active_view {
                overlays::draw_view_panel(f, app, main_area);
            } else {
                let placeholder = ratatui::widgets::Paragraph::new(format!(
                    " {} workspace\n\n Use slash commands or press Enter to load.",
                    app.current_activity().label()
                ))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Rgb(50, 50, 50)))
                        .title(format!(" {} ", app.current_activity().label())),
                );
                f.render_widget(placeholder, main_area);
            }
        }
    }

    // Status bar
    status_bar::draw(f, app, status_area);

    // Approval modal (overlays everything)
    if app.active_prompt.is_some() {
        overlays::draw_approval(f, app, size);
    }
}
