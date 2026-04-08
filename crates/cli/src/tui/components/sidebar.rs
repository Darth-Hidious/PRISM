use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::tui::state::{Activity, App, Workspace};

/// Draw the activity bar — thin icon strip on the far left (like VS Code)
pub fn draw_activity_bar(f: &mut Frame, app: &App, area: Rect) {
    let activities = Activity::all();
    let items: Vec<ListItem> = activities
        .iter()
        .enumerate()
        .map(|(i, act)| {
            let selected = i == app.activity_bar_idx;
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(80, 80, 80))
            };
            ListItem::new(Line::from(vec![
                Span::raw(if selected { "\u{2590}" } else { " " }), // ▐ selection indicator
                Span::styled(act.icon(), style),
            ]))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

/// Draw the sidebar panel — contextual content based on selected activity
pub fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    let activity = app.current_activity();

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(40, 40, 40)))
        .title(Span::styled(
            format!(" {} ", activity.label()),
            Style::default().fg(Color::White),
        ));

    match app.workspace {
        Workspace::Chat => {
            let mut items = vec![
                info_item("Session", app.status.as_ref().map(|s| s.session_mode.as_str()).unwrap_or("chat")),
                info_item("Messages", &app.chat_history.len().to_string()),
                info_item("Model", app.status.as_ref().and_then(|s| s.model.as_deref()).unwrap_or("none")),
                info_item("Cost", &format!("${:.4}", app.total_cost)),
                spacer(),
                header("Quick Commands"),
                cmd_item("/tools", "Tool catalog"),
                cmd_item("/models", "Switch model"),
                cmd_item("/context", "Prompt budget"),
                cmd_item("/help", "All commands"),
            ];
            if app.status.as_ref().is_some_and(|s| s.has_plan) {
                items.insert(4, info_item("Plan", "active"));
            }
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Models => {
            let items = vec![
                info_item("Hosted", &app.model_count.map(|c| c.to_string()).unwrap_or("...".into())),
                info_item("Active", app.status.as_ref().and_then(|s| s.model.as_deref()).unwrap_or("none")),
                spacer(),
                header("Actions"),
                cmd_item("/models list", "Browse all"),
                cmd_item("/model <id>", "Switch model"),
                cmd_item("/models search", "Search"),
            ];
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Mesh => {
            let items = vec![
                info_item("Peers", &app.peer_count.map(|c| c.to_string()).unwrap_or("...".into())),
                info_item("Nodes", &app.node_count.map(|c| c.to_string()).unwrap_or("...".into())),
                spacer(),
                header("Actions"),
                cmd_item("/mesh discover", "Find peers"),
                cmd_item("/mesh publish", "Share dataset"),
                cmd_item("/mesh subscribe", "Subscribe"),
                cmd_item("/node status", "Node info"),
                cmd_item("/node up", "Start node"),
            ];
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Compute => {
            let items = vec![
                info_item("GPUs", &app.gpu_count.map(|c| c.to_string()).unwrap_or("...".into())),
                spacer(),
                header("Actions"),
                cmd_item("/deploy list", "Deployments"),
                cmd_item("/run", "Submit job"),
                cmd_item("/job-status", "Check job"),
            ];
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Workflows => {
            let mut items = vec![
                info_item("Available", &app.workflow_names.len().to_string()),
                spacer(),
                header("Workflows"),
            ];
            for name in &app.workflow_names {
                items.push(cmd_item(name, ""));
            }
            items.push(spacer());
            items.push(header("Actions"));
            items.push(cmd_item("/workflow list", "List all"));
            items.push(cmd_item("/workflow run", "Execute"));
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Marketplace => {
            let items = vec![
                info_item("Resources", &app.marketplace_count.map(|c| c.to_string()).unwrap_or("...".into())),
                spacer(),
                header("Actions"),
                cmd_item("/marketplace search", "Browse"),
                cmd_item("/marketplace install", "Install"),
            ];
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Data => {
            let items = vec![
                info_item("Corpora", &app.corpus_count.map(|c| c.to_string()).unwrap_or("...".into())),
                info_item("Entities", app.entity_count.as_deref().unwrap_or("...")),
                spacer(),
                header("Actions"),
                cmd_item("/ingest", "Ingest file"),
                cmd_item("/query", "Query graph"),
                cmd_item("/research", "Research loop"),
            ];
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        Workspace::Settings => {
            let items = vec![
                info_item("Provider", app.status.as_ref().and_then(|s| s.model.as_deref()).unwrap_or("none")),
                spacer(),
                header("Actions"),
                cmd_item("/config", "View config"),
                cmd_item("/permissions", "Permissions"),
                cmd_item("/usage", "Token usage"),
                cmd_item("/billing", "Credits"),
                cmd_item("/login", "Re-authenticate"),
                cmd_item("/logout", "Sign out"),
            ];
            let list = List::new(items).block(block);
            f.render_widget(list, area);
        }
        _ => {
            let p = Paragraph::new("").block(block);
            f.render_widget(p, area);
        }
    }
}

fn info_item(key: &str, value: &str) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(
            format!(" {key}: "),
            Style::default().fg(Color::Rgb(100, 100, 100)),
        ),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ]))
}

fn cmd_item(cmd: &str, desc: &str) -> ListItem<'static> {
    let mut spans = vec![Span::styled(
        format!(" {cmd}"),
        Style::default().fg(Color::Cyan),
    )];
    if !desc.is_empty() {
        spans.push(Span::styled(
            format!(" \u{2014} {desc}"),
            Style::default().fg(Color::Rgb(80, 80, 80)),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn header(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!(" {text}"),
        Style::default()
            .fg(Color::Rgb(140, 140, 140))
            .add_modifier(Modifier::BOLD),
    )))
}

fn spacer() -> ListItem<'static> {
    ListItem::new(Line::from(""))
}
