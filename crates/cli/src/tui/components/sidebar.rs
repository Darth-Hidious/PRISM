#![allow(unused_mut)]
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::tui::state::{App, Workspace};

pub fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    // All sidebar drawers now use the focused variant
    let focused = app.focus == crate::tui::state::FocusZone::Sidebar;
    match app.workspace {
        Workspace::Chat => draw_explorer(f, app, area, focused),
        Workspace::Models => draw_models(f, app, area, focused),
        Workspace::Mesh => draw_mesh(f, app, area, focused),
        Workspace::Compute => draw_compute(f, app, area, focused),
        Workspace::Data => draw_data(f, app, area, focused),
        Workspace::Marketplace => draw_marketplace(f, app, area, focused),
        Workspace::Workflows => draw_workflows(f, app, area, focused),
        Workspace::Settings => draw_settings(f, app, area, focused),
        _ => draw_explorer(f, app, area, focused),
    }
}

// ── Explorer (Chat tab) ─────────────────────────────────────────────

fn draw_explorer(f: &mut Frame, _app: &App, area: Rect, focused: bool) {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let dir_name = cwd.rsplit('/').next().unwrap_or(&cwd);

    let mut items = vec![ListItem::new(Line::from(Span::styled(
        format!(" \u{25bc} {dir_name}"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )))];

    if let Ok(entries) = std::fs::read_dir(".") {
        let mut files: Vec<(String, bool)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (name, is_dir)
            })
            .collect();
        files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (name, is_dir) in files.iter().take(30) {
            let icon = if *is_dir { "\u{25b8} " } else { "  " };
            items.push(ListItem::new(Line::from(Span::styled(
                format!("   {icon}{name}"),
                Style::default().fg(if *is_dir {
                    Color::Cyan
                } else {
                    Color::Rgb(140, 140, 140)
                }),
            ))));
        }
    }

    render_panel_focused(f, " Explorer ", items, area, focused);
}

// ── Models ──────────────────────────────────────────────────────────

fn draw_models(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut items = vec![
        section("Active Model"),
        item_highlight(
            app.status
                .as_ref()
                .and_then(|s| s.model.as_deref())
                .unwrap_or("none"),
        ),
        spacer(),
    ];

    // Model count by provider
    if !app.cached_models.is_empty() {
        items.push(section(&format!("Catalog ({})", app.cached_models.len())));
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for m in &app.cached_models {
            *counts.entry(&m.provider).or_default() += 1;
        }
        for (prov, count) in &counts {
            items.push(item(&format!("{prov}: {count} models")));
        }
        items.push(spacer());
    }

    items.push(section("Actions"));
    items.push(action("/model", "Select model"));
    items.push(action("/models search", "Search"));
    items.push(spacer());
    items.push(section("Bring Your Own Key"));
    items.push(item("Store API keys for:"));
    items.push(item("  Anthropic, OpenAI,"));
    items.push(item("  Google, DeepSeek"));
    items.push(action("/config", "Manage keys"));

    render_panel_focused(f, " Models ", items, area, focused);
}

// ── Mesh ────────────────────────────────────────────────────────────

fn draw_mesh(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut items = vec![section("Local Node")];

    let node_online = app.node_count.is_some();
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            if node_online {
                "   \u{25cf} "
            } else {
                "   \u{25cb} "
            },
            Style::default().fg(if node_online {
                Color::Green
            } else {
                Color::Rgb(80, 80, 80)
            }),
        ),
        Span::styled(
            if node_online {
                "online :7327"
            } else {
                "offline"
            },
            Style::default().fg(Color::Rgb(140, 140, 140)),
        ),
    ])));

    items.push(spacer());
    items.push(section("Peers"));
    match app.peer_count {
        Some(0) | None => items.push(item("No peers discovered")),
        Some(n) => {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("   \u{25cf} ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{n} peer(s) connected"),
                    Style::default().fg(Color::Rgb(140, 140, 140)),
                ),
            ])));
        }
    }

    items.push(spacer());
    items.push(section("Actions"));
    items.push(action("/mesh discover", "Find LAN peers"));
    items.push(action("/mesh publish", "Share dataset"));
    items.push(action("/mesh subscribe", "Subscribe"));
    items.push(action("/node up", "Start node"));
    items.push(action("/node down", "Stop node"));
    items.push(action("/node status", "Node info"));

    render_panel_focused(f, " Mesh & Nodes ", items, area, focused);
}

// ── Compute ─────────────────────────────────────────────────────────

fn draw_compute(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut items = vec![
        section("GPU Resources"),
        item(&format!("{} types available", app.gpu_count.unwrap_or(0))),
        spacer(),
        section("Providers"),
        item("  RunPod"),
        item("  Lambda"),
        item("  PRISM Nodes"),
        spacer(),
        section("Actions"),
        action("/deploy list", "Deployments"),
        action("/deploy create", "New deployment"),
        action("/run", "Submit job"),
        action("/job-status", "Check job"),
        spacer(),
        section("Bring Your Own Compute"),
        item("Connect external HPCs:"),
        action("/run --ssh", "SSH backend"),
        action("/run --k8s-context", "Kubernetes"),
        action("/run --slurm", "SLURM cluster"),
    ];

    render_panel_focused(f, " Compute ", items, area, focused);
}

// ── Data ────────────────────────────────────────────────────────────

fn draw_data(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut items = vec![
        section("Knowledge Graph"),
        item(&format!(
            "Entities: {}",
            app.entity_count.as_deref().unwrap_or("...")
        )),
        item(&format!("Corpora: {}", app.corpus_count.unwrap_or(0))),
        spacer(),
        section("Sources"),
        item("  MARC27 Knowledge Service"),
        item("  OPTIMADE (20+ providers)"),
        item("  Materials Project"),
        item("  Local Neo4j / Qdrant"),
        spacer(),
        section("Actions"),
        action("/query", "Query graph"),
        action("/query --semantic", "Semantic search"),
        action("/ingest", "Ingest data"),
        action("/research", "Research loop"),
    ];

    render_panel_focused(f, " Data ", items, area, focused);
}

// ── Marketplace ─────────────────────────────────────────────────────

fn draw_marketplace(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut items = vec![
        section(&format!(
            "Resources ({})",
            app.marketplace_count.unwrap_or(0)
        )),
        spacer(),
        section("Categories"),
        item("  \u{25b8} Datasets"),
        item("  \u{25b8} Models (MACE, CHGNet)"),
        item("  \u{25b8} Plugins"),
        item("  \u{25b8} CLI Tools (QE)"),
        spacer(),
        section("Actions"),
        action("/marketplace search", "Browse"),
        action("/marketplace install", "Install"),
        spacer(),
        item("Click opens model card"),
        item("on marc27.com"),
    ];

    render_panel_focused(f, " Marketplace ", items, area, focused);
}

// ── Workflows ───────────────────────────────────────────────────────

fn draw_workflows(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let mut items = vec![section("Available")];

    if app.workflow_names.is_empty() {
        items.push(item("forge (built-in)"));
    } else {
        for name in &app.workflow_names {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("   \u{25b7} ", Style::default().fg(Color::Cyan)),
                Span::styled(name.clone(), Style::default().fg(Color::Rgb(160, 160, 160))),
            ])));
        }
    }

    items.push(spacer());
    items.push(section("Step Types"));
    items.push(item("  set, message, http, tool"));
    items.push(item("  if, parallel, workflow"));
    items.push(item("  + retries, hooks, OPA"));
    items.push(spacer());
    items.push(section("Actions"));
    items.push(action("/workflow list", "List all"));
    items.push(action("/workflow show", "Inspect"));
    items.push(action("/workflow run", "Execute"));
    items.push(spacer());
    items.push(section("Custom Workflows"));
    items.push(item("Drop YAML in:"));
    items.push(item("  ~/.prism/workflows/"));

    render_panel_focused(f, " Workflows ", items, area, focused);
}

// ── Settings ────────────────────────────────────────────────────────

fn draw_settings(f: &mut Frame, app: &App, area: Rect, focused: bool) {
    let model = app
        .status
        .as_ref()
        .and_then(|s| s.model.as_deref())
        .unwrap_or("none");

    let mut items = vec![
        section("LLM"),
        item(&format!("Model: {model}")),
        item("Temperature: 0.1"),
        item("Max tokens: 4096"),
        action("/config", "Edit config"),
        action("/model", "Switch model"),
        spacer(),
        section("Auth & RBAC"),
        action("/login", "Re-authenticate"),
        action("/logout", "Sign out"),
        action("/permissions", "View permissions"),
        spacer(),
        section("Usage & Billing"),
        item(&format!("Session: ${:.4}", app.total_cost)),
        action("/usage", "Token usage"),
        action("/billing", "Credit balance"),
        spacer(),
        section("Tools"),
        item(&format!("{} loaded", app.tool_count)),
        item("Custom: ~/.prism/tools/"),
        action("/tools", "Browse tools"),
        spacer(),
        section("Policies"),
        item("OPA/Rego engine active"),
        item("Custom: ~/.prism/policies/"),
        spacer(),
        section("BYOK (API Keys)"),
        item("Store provider keys:"),
        item("  prism config set"),
        item("  anthropic_key sk-..."),
    ];

    render_panel_focused(f, " Settings ", items, area, focused);
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Render sidebar panel with focus-aware border
pub fn render_panel_focused(
    f: &mut Frame,
    title: &str,
    items: Vec<ListItem<'static>>,
    area: Rect,
    focused: bool,
) {
    let border_color = if focused {
        Color::Cyan
    } else {
        Color::Rgb(60, 60, 60)
    };
    let title_suffix = if focused { " (↑↓ scroll)" } else { "" };
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!("{title}{title_suffix}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn section(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!(" {text}"),
        Style::default()
            .fg(Color::Rgb(200, 200, 200))
            .add_modifier(Modifier::BOLD),
    )))
}

fn item(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!("   {text}"),
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )))
}

fn item_highlight(text: &str) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(
        format!("   {text}"),
        Style::default().fg(Color::Cyan),
    )))
}

fn action(cmd: &str, desc: &str) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(format!("   {cmd}"), Style::default().fg(Color::Cyan)),
        Span::styled(
            format!("  {desc}"),
            Style::default().fg(Color::Rgb(130, 130, 130)),
        ),
    ]))
}

fn spacer() -> ListItem<'static> {
    ListItem::new(Line::from(""))
}
