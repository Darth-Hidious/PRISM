#![allow(unused_mut, dead_code)]
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

use crate::tui::state::{App, FocusZone, Workspace};

/// A sidebar item that can be selected and activated
#[derive(Debug, Clone)]
pub enum SidebarItem {
    Section(String),
    Selectable {
        label: String,
        action: Option<String>,
        icon: Option<String>,
    },
    Toggle {
        label: String,
        value: bool,
    },
    Slider {
        label: String,
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    Spacer,
}

/// Build sidebar items for current workspace
pub fn build_items(app: &App) -> Vec<SidebarItem> {
    match app.workspace {
        Workspace::Chat => build_explorer(),
        Workspace::Models => build_models(app),
        Workspace::Mesh => build_mesh(app),
        Workspace::Compute => build_compute(app),
        Workspace::Data => build_data(app),
        Workspace::Marketplace => build_marketplace(app),
        Workspace::Workflows => build_workflows(app),
        Workspace::Settings => build_settings(app),
        _ => build_explorer(),
    }
}

/// Count selectable items (for bounds checking)
pub fn selectable_count(items: &[SidebarItem]) -> usize {
    items
        .iter()
        .filter(|i| {
            matches!(
                i,
                SidebarItem::Selectable { .. }
                    | SidebarItem::Toggle { .. }
                    | SidebarItem::Slider { .. }
            )
        })
        .count()
}

/// Get the action for the selected selectable item
pub fn get_selected_action(items: &[SidebarItem], selected: usize) -> Option<String> {
    let mut sel_idx = 0;
    for item in items {
        match item {
            SidebarItem::Selectable { action, .. } => {
                if sel_idx == selected {
                    return action.clone();
                }
                sel_idx += 1;
            }
            SidebarItem::Toggle { .. } | SidebarItem::Slider { .. } => {
                sel_idx += 1;
            }
            _ => {}
        }
    }
    None
}

pub fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == FocusZone::Sidebar;
    let items = build_items(app);
    let title = match app.workspace {
        Workspace::Chat => " Explorer ",
        Workspace::Models => " Models ",
        Workspace::Mesh => " Mesh & Nodes ",
        Workspace::Compute => " Compute ",
        Workspace::Data => " Data ",
        Workspace::Marketplace => " Marketplace ",
        Workspace::Workflows => " Workflows ",
        Workspace::Settings => " Settings ",
        _ => " Explorer ",
    };

    let mut list_items: Vec<ListItem> = Vec::new();
    let mut selectable_idx: usize = 0;

    for item in &items {
        match item {
            SidebarItem::Section(text) => {
                list_items.push(ListItem::new(Line::from(Span::styled(
                    format!(" {text}"),
                    Style::default()
                        .fg(Color::Rgb(200, 200, 200))
                        .add_modifier(Modifier::BOLD),
                ))));
            }
            SidebarItem::Selectable { label, icon, .. } => {
                let is_selected = focused && selectable_idx == app.sidebar_scroll as usize;
                let bg = if is_selected {
                    Color::Rgb(30, 55, 75)
                } else {
                    Color::Reset
                };
                let fg = if is_selected {
                    Color::White
                } else {
                    Color::Rgb(160, 160, 160)
                };
                let prefix = if is_selected { " \u{25b8} " } else { "   " };
                let icon_str = icon.as_deref().unwrap_or("");

                list_items.push(ListItem::new(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Cyan).bg(bg)),
                    Span::styled(
                        icon_str.to_string(),
                        Style::default().fg(Color::Rgb(100, 160, 160)).bg(bg),
                    ),
                    Span::styled(label.clone(), Style::default().fg(fg).bg(bg)),
                ])));
                selectable_idx += 1;
            }
            SidebarItem::Toggle { label, value } => {
                let is_selected = focused && selectable_idx == app.sidebar_scroll as usize;
                let bg = if is_selected {
                    Color::Rgb(30, 55, 75)
                } else {
                    Color::Reset
                };
                let fg = if is_selected {
                    Color::White
                } else {
                    Color::Rgb(160, 160, 160)
                };
                let prefix = if is_selected { " \u{25b8} " } else { "   " };
                let toggle = if *value { "[\u{25cf}]" } else { "[\u{25cb}]" };
                let toggle_color = if *value {
                    Color::Green
                } else {
                    Color::Rgb(80, 80, 80)
                };

                list_items.push(ListItem::new(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Cyan).bg(bg)),
                    Span::styled(format!("{label} "), Style::default().fg(fg).bg(bg)),
                    Span::styled(toggle, Style::default().fg(toggle_color).bg(bg)),
                ])));
                selectable_idx += 1;
            }
            SidebarItem::Slider {
                label,
                value,
                min,
                max,
                ..
            } => {
                let is_selected = focused && selectable_idx == app.sidebar_scroll as usize;
                let bg = if is_selected {
                    Color::Rgb(30, 55, 75)
                } else {
                    Color::Reset
                };
                let fg = if is_selected {
                    Color::White
                } else {
                    Color::Rgb(160, 160, 160)
                };
                let prefix = if is_selected { " \u{25b8} " } else { "   " };

                let pct = ((value - min) / (max - min)).clamp(0.0, 1.0);
                let bar_width = 8;
                let filled = (pct * bar_width as f64).round() as usize;
                let empty = bar_width - filled;
                let bar = format!(
                    "[{}{}]",
                    "\u{2588}".repeat(filled),
                    "\u{2591}".repeat(empty)
                );

                list_items.push(ListItem::new(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Cyan).bg(bg)),
                    Span::styled(format!("{label} "), Style::default().fg(fg).bg(bg)),
                    Span::styled(bar, Style::default().fg(Color::Cyan).bg(bg)),
                    Span::styled(
                        format!(" {value:.1}"),
                        Style::default().fg(Color::Rgb(120, 120, 120)).bg(bg),
                    ),
                ])));
                selectable_idx += 1;
            }
            SidebarItem::Spacer => {
                list_items.push(ListItem::new(Line::from("")));
            }
        }
    }

    let border_color = if focused {
        Color::Cyan
    } else {
        Color::Rgb(60, 60, 60)
    };
    let hint = if focused { " (↑↓ Enter)" } else { "" };

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!("{title}{hint}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));

    let list = List::new(list_items).block(block);
    f.render_widget(list, area);
}

// ── Item builders per workspace ─────────────────────────────────────

fn build_explorer() -> Vec<SidebarItem> {
    let mut items = Vec::new();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let dir_name = cwd.rsplit('/').next().unwrap_or(&cwd);

    items.push(SidebarItem::Section(dir_name.to_string()));

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

        for (name, is_dir) in files.into_iter().take(30) {
            let icon = if is_dir {
                Some("\u{25b8} ".to_string())
            } else {
                None
            };
            items.push(SidebarItem::Selectable {
                label: name.clone(),
                action: Some(format!("/read {name}")),
                icon,
            });
        }
    }
    items
}

fn build_models(app: &App) -> Vec<SidebarItem> {
    let mut items = vec![
        SidebarItem::Section("Active".into()),
        SidebarItem::Selectable {
            label: app
                .status
                .as_ref()
                .and_then(|s| s.model.as_deref())
                .unwrap_or("none")
                .to_string(),
            action: Some("/model".into()),
            icon: Some("\u{25cf} ".into()),
        },
        SidebarItem::Spacer,
        SidebarItem::Section(format!("Catalog ({})", app.cached_models.len())),
    ];

    let mut current_prov = "";
    let mut count = 0;
    for m in &app.cached_models {
        if m.provider != current_prov {
            current_prov = &m.provider;
            items.push(SidebarItem::Section(current_prov.to_string()));
            count = 0;
        }
        if count < 5 {
            items.push(SidebarItem::Selectable {
                label: m.model_id.clone(),
                action: Some(format!("/model {}", m.model_id)),
                icon: Some(format!("{} ", m.badges())),
            });
            count += 1;
        }
    }

    items.push(SidebarItem::Spacer);
    items.push(SidebarItem::Section("Actions".into()));
    items.push(SidebarItem::Selectable {
        label: "Search models".into(),
        action: Some("/models search".into()),
        icon: None,
    });
    items.push(SidebarItem::Selectable {
        label: "Model picker".into(),
        action: Some("/model".into()),
        icon: None,
    });
    items
}

fn build_mesh(app: &App) -> Vec<SidebarItem> {
    let node_online = app.node_count.is_some();
    vec![
        SidebarItem::Section("Local Node".into()),
        SidebarItem::Selectable {
            label: if node_online {
                "online :7327".into()
            } else {
                "offline".into()
            },
            action: Some("/node status".into()),
            icon: Some(if node_online {
                "\u{25cf} ".into()
            } else {
                "\u{25cb} ".into()
            }),
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Peers".into()),
        SidebarItem::Selectable {
            label: format!("{} discovered", app.peer_count.unwrap_or(0)),
            action: Some("/mesh discover".into()),
            icon: None,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Actions".into()),
        SidebarItem::Selectable {
            label: "Discover peers".into(),
            action: Some("/mesh discover".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Publish dataset".into(),
            action: Some("/mesh publish".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Start node".into(),
            action: Some("/node up".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Stop node".into(),
            action: Some("/node down".into()),
            icon: None,
        },
    ]
}

fn build_compute(app: &App) -> Vec<SidebarItem> {
    vec![
        SidebarItem::Section("GPU Resources".into()),
        SidebarItem::Selectable {
            label: format!("{} types", app.gpu_count.unwrap_or(0)),
            action: None,
            icon: None,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Providers".into()),
        SidebarItem::Selectable {
            label: "RunPod".into(),
            action: None,
            icon: Some("\u{25cf} ".into()),
        },
        SidebarItem::Selectable {
            label: "Lambda".into(),
            action: None,
            icon: Some("\u{25cf} ".into()),
        },
        SidebarItem::Selectable {
            label: "PRISM Nodes".into(),
            action: None,
            icon: Some("\u{25cf} ".into()),
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Actions".into()),
        SidebarItem::Selectable {
            label: "Deployments".into(),
            action: Some("/deploy list".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Submit job".into(),
            action: Some("/run".into()),
            icon: None,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("BYOC".into()),
        SidebarItem::Selectable {
            label: "SSH backend".into(),
            action: Some("/run --ssh".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Kubernetes".into(),
            action: Some("/run --k8s-context".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "SLURM".into(),
            action: Some("/run --slurm".into()),
            icon: None,
        },
    ]
}

fn build_data(app: &App) -> Vec<SidebarItem> {
    vec![
        SidebarItem::Section("Knowledge Graph".into()),
        SidebarItem::Selectable {
            label: format!("Entities: {}", app.entity_count.as_deref().unwrap_or("...")),
            action: None,
            icon: None,
        },
        SidebarItem::Selectable {
            label: format!("Corpora: {}", app.corpus_count.unwrap_or(0)),
            action: None,
            icon: None,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Actions".into()),
        SidebarItem::Selectable {
            label: "Query graph".into(),
            action: Some("/query".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Semantic search".into(),
            action: Some("/query --semantic".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Ingest data".into(),
            action: Some("/ingest".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Research loop".into(),
            action: Some("/research".into()),
            icon: None,
        },
    ]
}

fn build_marketplace(app: &App) -> Vec<SidebarItem> {
    vec![
        SidebarItem::Section(format!(
            "Resources ({})",
            app.marketplace_count.unwrap_or(0)
        )),
        SidebarItem::Spacer,
        SidebarItem::Section("Categories".into()),
        SidebarItem::Selectable {
            label: "Datasets".into(),
            action: Some("/marketplace search dataset".into()),
            icon: Some("\u{25b8} ".into()),
        },
        SidebarItem::Selectable {
            label: "Models".into(),
            action: Some("/marketplace search model".into()),
            icon: Some("\u{25b8} ".into()),
        },
        SidebarItem::Selectable {
            label: "Plugins".into(),
            action: Some("/marketplace search plugin".into()),
            icon: Some("\u{25b8} ".into()),
        },
        SidebarItem::Selectable {
            label: "CLI Tools".into(),
            action: Some("/marketplace search cli".into()),
            icon: Some("\u{25b8} ".into()),
        },
        SidebarItem::Spacer,
        SidebarItem::Selectable {
            label: "Browse all".into(),
            action: Some("/marketplace search".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Install".into(),
            action: Some("/marketplace install".into()),
            icon: None,
        },
    ]
}

fn build_workflows(app: &App) -> Vec<SidebarItem> {
    let mut items = vec![SidebarItem::Section("Available".into())];
    if app.workflow_names.is_empty() {
        items.push(SidebarItem::Selectable {
            label: "forge (built-in)".into(),
            action: Some("/workflow show forge".into()),
            icon: Some("\u{25b7} ".into()),
        });
    } else {
        for name in &app.workflow_names {
            items.push(SidebarItem::Selectable {
                label: name.clone(),
                action: Some(format!("/workflow show {name}")),
                icon: Some("\u{25b7} ".into()),
            });
        }
    }
    items.push(SidebarItem::Spacer);
    items.push(SidebarItem::Section("Actions".into()));
    items.push(SidebarItem::Selectable {
        label: "List workflows".into(),
        action: Some("/workflow list".into()),
        icon: None,
    });
    items.push(SidebarItem::Selectable {
        label: "Run workflow".into(),
        action: Some("/workflow run".into()),
        icon: None,
    });
    items.push(SidebarItem::Spacer);
    items.push(SidebarItem::Section("Custom".into()));
    items.push(SidebarItem::Selectable {
        label: "~/.prism/workflows/".into(),
        action: None,
        icon: None,
    });
    items
}

fn build_settings(app: &App) -> Vec<SidebarItem> {
    let model = app
        .status
        .as_ref()
        .and_then(|s| s.model.as_deref())
        .unwrap_or("none");
    vec![
        SidebarItem::Section("LLM".into()),
        SidebarItem::Selectable {
            label: format!("Model: {model}"),
            action: Some("/model".into()),
            icon: None,
        },
        SidebarItem::Slider {
            label: "Temperature".into(),
            value: 0.1,
            min: 0.0,
            max: 2.0,
            step: 0.1,
        },
        SidebarItem::Slider {
            label: "Max tokens".into(),
            value: 4096.0,
            min: 256.0,
            max: 32000.0,
            step: 256.0,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Auth".into()),
        SidebarItem::Selectable {
            label: "Login".into(),
            action: Some("/login".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Logout".into(),
            action: Some("/logout".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Permissions".into(),
            action: Some("/permissions".into()),
            icon: None,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Usage".into()),
        SidebarItem::Selectable {
            label: format!("Cost: ${:.4}", app.total_cost),
            action: Some("/usage".into()),
            icon: None,
        },
        SidebarItem::Selectable {
            label: "Billing".into(),
            action: Some("/billing".into()),
            icon: None,
        },
        SidebarItem::Spacer,
        SidebarItem::Section("Tools".into()),
        SidebarItem::Selectable {
            label: format!("{} loaded", app.tool_count),
            action: Some("/tools".into()),
            icon: None,
        },
        SidebarItem::Toggle {
            label: "Auto-approve".into(),
            value: app.status.as_ref().map(|s| s.auto_approve).unwrap_or(false),
        },
        SidebarItem::Spacer,
        SidebarItem::Section("BYOK".into()),
        SidebarItem::Selectable {
            label: "Manage API keys".into(),
            action: Some("/config".into()),
            icon: None,
        },
    ]
}
