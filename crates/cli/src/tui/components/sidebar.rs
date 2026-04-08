#![allow(unused_mut)]
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::tui::state::{App, Workspace};

/// Draw the sidebar panel — content depends on active workspace tab.
/// This is a BROWSER, not an info panel. Shows selectable items.
pub fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    match app.workspace {
        Workspace::Chat => draw_file_tree(f, app, area),
        Workspace::Models => draw_model_browser(f, app, area),
        Workspace::Mesh => draw_mesh_browser(f, app, area),
        Workspace::Compute => draw_compute_browser(f, app, area),
        Workspace::Data => draw_data_browser(f, app, area),
        Workspace::Marketplace => draw_marketplace_browser(f, app, area),
        Workspace::Workflows => draw_workflow_browser(f, app, area),
        Workspace::Settings => draw_settings_panel(f, app, area),
        _ => draw_file_tree(f, app, area),
    }
}

fn draw_file_tree(f: &mut Frame, _app: &App, area: Rect) {
    // Show project file tree like VS Code explorer
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let dir_name = cwd.rsplit('/').next().unwrap_or(&cwd);

    let mut items = vec![ListItem::new(Line::from(vec![Span::styled(
        format!(" \u{25bc} {dir_name}"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]))];

    // List files in current directory (first 20)
    if let Ok(entries) = std::fs::read_dir(".") {
        let mut files: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    format!("   \u{25b8} {name}/")
                } else {
                    format!("     {name}")
                }
            })
            .collect();
        files.sort();
        for f_name in files.iter().take(25) {
            let is_dir = f_name.contains('\u{25b8}');
            items.push(ListItem::new(Line::from(Span::styled(
                f_name.clone(),
                Style::default().fg(if is_dir {
                    Color::Cyan
                } else {
                    Color::Rgb(160, 160, 160)
                }),
            ))));
        }
    }

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Explorer ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_model_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        section_header("Active"),
        tree_item(
            app.status
                .as_ref()
                .and_then(|s| s.model.as_deref())
                .unwrap_or("none"),
            true,
        ),
        spacer(),
    ];

    if app.cached_models.is_empty() {
        items.push(tree_item("Loading...", false));
    } else {
        // Group by provider, show first few
        let mut current_prov = "";
        let mut count = 0;
        for m in &app.cached_models {
            if m.provider != current_prov {
                current_prov = &m.provider;
                items.push(section_header(current_prov));
                count = 0;
            }
            if count < 5 {
                let badges = m.badges();
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("   {badges} "),
                        Style::default().fg(Color::Rgb(100, 100, 50)),
                    ),
                    Span::styled(&m.model_id, Style::default().fg(Color::Rgb(180, 180, 180))),
                ])));
                count += 1;
            } else if count == 5 {
                items.push(tree_item(
                    &format!(
                        "   ... +{} more",
                        app.cached_models
                            .iter()
                            .filter(|x| x.provider == current_prov)
                            .count()
                            - 5
                    ),
                    false,
                ));
                count += 1;
            }
        }
    }

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Models ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_mesh_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        section_header("Local Node"),
        tree_item("Status: offline", false),
        tree_item("Port: 7327", false),
        spacer(),
        section_header("Peers"),
    ];

    match app.peer_count {
        Some(0) | None => items.push(tree_item("No peers found", false)),
        Some(n) => items.push(tree_item(&format!("{n} peers connected"), true)),
    }

    items.push(spacer());
    items.push(section_header("Actions"));
    items.push(tree_item("/mesh discover", false));
    items.push(tree_item("/node up", false));

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Mesh ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_compute_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        section_header("GPUs"),
        tree_item(
            &format!("{} types available", app.gpu_count.unwrap_or(0)),
            false,
        ),
        spacer(),
        section_header("Deployments"),
        tree_item("Run /deploy list", false),
        spacer(),
        section_header("Jobs"),
        tree_item("Run /job-status <id>", false),
    ];

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Compute ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_data_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        section_header("Knowledge Graph"),
        tree_item(
            &format!("Entities: {}", app.entity_count.as_deref().unwrap_or("...")),
            false,
        ),
        tree_item(
            &format!("Corpora: {}", app.corpus_count.unwrap_or(0)),
            false,
        ),
        spacer(),
        section_header("Local Datasets"),
        tree_item("Run /discover_capabilities", false),
    ];

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Data ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_marketplace_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        section_header("Resources"),
        tree_item(
            &format!("{} available", app.marketplace_count.unwrap_or(0)),
            false,
        ),
        spacer(),
        section_header("Categories"),
        tree_item("Datasets", false),
        tree_item("Models", false),
        tree_item("Plugins", false),
        tree_item("CLI Tools", false),
    ];

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Marketplace ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_workflow_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![section_header("Workflows")];

    if app.workflow_names.is_empty() {
        items.push(tree_item("No workflows found", false));
    } else {
        for name in &app.workflow_names {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("   \u{25b7} ", Style::default().fg(Color::Cyan)),
                Span::styled(name.clone(), Style::default().fg(Color::Rgb(180, 180, 180))),
            ])));
        }
    }

    items.push(spacer());
    items.push(section_header("Actions"));
    items.push(tree_item("/workflow list", false));
    items.push(tree_item("/workflow run", false));

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Workflows ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_settings_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut items = vec![
        section_header("LLM Config"),
        tree_item(
            &format!(
                "Model: {}",
                app.status
                    .as_ref()
                    .and_then(|s| s.model.as_deref())
                    .unwrap_or("none")
            ),
            false,
        ),
        tree_item("Temperature: 0.1", false),
        tree_item("Max tokens: 4096", false),
        spacer(),
        section_header("Auth"),
        tree_item("/login", false),
        tree_item("/logout", false),
        spacer(),
        section_header("Permissions"),
        tree_item("/permissions", false),
        spacer(),
        section_header("Usage"),
        tree_item(&format!("Cost: ${:.4}", app.total_cost), false),
        tree_item("/usage", false),
        tree_item("/billing", false),
    ];

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            " Settings ",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

// Helper functions

fn section_header(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!(" {text}"),
        Style::default()
            .fg(Color::Rgb(140, 140, 140))
            .add_modifier(Modifier::BOLD),
    )))
}

fn tree_item(text: &str, highlight: bool) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!("   {text}"),
        Style::default().fg(if highlight {
            Color::Cyan
        } else {
            Color::Rgb(120, 120, 120)
        }),
    )))
}

fn spacer() -> ListItem<'static> {
    ListItem::new(Line::from(""))
}
