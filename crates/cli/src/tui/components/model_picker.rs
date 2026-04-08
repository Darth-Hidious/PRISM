#![allow(dead_code, unused_variables, clippy::explicit_counter_loop)]
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

/// Cached model info from the API
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub model_id: String,
    pub display_name: String,
    pub provider: String,
    pub context_window: Option<u64>,
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub supports_reasoning: bool,
    pub supports_vision: bool,
    pub supports_tools: bool,
}

impl ModelInfo {
    /// Capability badges
    pub fn badges(&self) -> String {
        let mut b = String::new();
        if self.supports_reasoning {
            b.push('\u{25c6}'); // ◆ reasoning
        }
        if self.supports_vision {
            b.push('\u{2295}'); // ⊕ vision
        }
        if self.supports_tools {
            b.push('\u{26a1}'); // ⚡ tools
        }
        if b.is_empty() {
            b.push('\u{25c7}'); // ◇ basic
        }
        b
    }

    pub fn price_str(&self) -> String {
        match (self.input_price, self.output_price) {
            (Some(i), Some(o)) => format!("${:.2}/${:.2}", i, o),
            _ => "free".to_string(),
        }
    }

    pub fn context_str(&self) -> String {
        match self.context_window {
            Some(c) if c >= 1_000_000 => format!("{}M", c / 1_000_000),
            Some(c) if c >= 1_000 => format!("{}K", c / 1_000),
            Some(c) => format!("{c}"),
            None => "?".to_string(),
        }
    }
}

/// Parse models from the API response
pub fn parse_models(data: &serde_json::Value) -> Vec<ModelInfo> {
    let arr = if let Some(a) = data.as_array() {
        a.clone()
    } else if let Some(a) = data.get("models").and_then(|m| m.as_array()) {
        a.clone()
    } else {
        return Vec::new();
    };

    arr.iter()
        .filter_map(|m| {
            let model_id = m.get("model_id").and_then(|v| v.as_str())?.to_string();
            let display_name = m
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&model_id)
                .to_string();
            let provider = m
                .get("provider")
                .or(m.get("provider_slug"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            Some(ModelInfo {
                model_id,
                display_name,
                provider,
                context_window: m.get("context_window").and_then(|v| v.as_u64()),
                input_price: m.get("input_price").and_then(|v| v.as_f64()),
                output_price: m.get("output_price").and_then(|v| v.as_f64()),
                supports_reasoning: m
                    .get("supports_reasoning")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                supports_vision: m
                    .get("supports_vision")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                supports_tools: m
                    .get("supports_function_calling")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// Filter models by search query and provider
pub fn filter_models<'a>(
    models: &'a [ModelInfo],
    query: &str,
    provider_filter: Option<&str>,
) -> Vec<&'a ModelInfo> {
    let q = query.to_lowercase();
    models
        .iter()
        .filter(|m| {
            if let Some(pf) = provider_filter {
                if m.provider != pf {
                    return false;
                }
            }
            if q.is_empty() {
                return true;
            }
            m.model_id.to_lowercase().contains(&q)
                || m.display_name.to_lowercase().contains(&q)
                || m.provider.to_lowercase().contains(&q)
        })
        .collect()
}

/// Get unique providers from models
pub fn providers(models: &[ModelInfo]) -> Vec<String> {
    let mut provs: Vec<String> = models
        .iter()
        .map(|m| m.provider.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    provs.sort();
    provs
}

/// Draw the model picker overlay
pub fn draw(
    f: &mut Frame,
    filtered: &[&ModelInfo],
    selected_idx: usize,
    search_query: &str,
    provider_filter: Option<&str>,
    provider_list: &[String],
    area: Rect,
) {
    f.render_widget(Clear, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Search bar + provider filter
            Constraint::Min(0),    // Model list
            Constraint::Length(1), // Footer
        ])
        .split(area);

    // Search bar
    let provider_label = provider_filter.unwrap_or("all");
    let search = Paragraph::new(Line::from(vec![
        Span::styled(" Search: ", Style::default().fg(Color::Rgb(100, 100, 100))),
        Span::styled(
            if search_query.is_empty() {
                "type to filter..."
            } else {
                search_query
            },
            Style::default().fg(Color::White),
        ),
        Span::styled("\u{2588}", Style::default().fg(Color::Rgb(80, 80, 80))),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(50, 50, 50)))
            .title(Span::styled(
                format!(
                    " Models ({}) \u{2502} provider: {} \u{25be} ",
                    filtered.len(),
                    provider_label
                ),
                Style::default().fg(Color::Rgb(120, 120, 120)),
            )),
    );
    f.render_widget(search, chunks[0]);

    // Model list grouped by provider
    let mut items: Vec<ListItem> = Vec::new();
    let mut current_provider = "";
    let mut visual_idx = 0;

    for model in filtered.iter().take(50) {
        // Provider header
        if model.provider != current_provider {
            current_provider = &model.provider;
            items.push(ListItem::new(Line::from(Span::styled(
                format!(" {current_provider}"),
                Style::default()
                    .fg(Color::Rgb(140, 140, 140))
                    .add_modifier(Modifier::BOLD),
            ))));
        }

        let is_selected = visual_idx == selected_idx;
        let bg = if is_selected {
            Color::Rgb(25, 45, 65)
        } else {
            Color::Reset
        };

        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                if is_selected { " \u{25b8} " } else { "   " },
                Style::default().fg(Color::Cyan).bg(bg),
            ),
            Span::styled(
                model.badges(),
                Style::default()
                    .fg(if model.supports_reasoning {
                        Color::Rgb(255, 200, 50)
                    } else {
                        Color::Rgb(80, 80, 80)
                    })
                    .bg(bg),
            ),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                format!("{:<36}", model.model_id),
                Style::default()
                    .fg(if is_selected {
                        Color::White
                    } else {
                        Color::Rgb(200, 200, 200)
                    })
                    .bg(bg),
            ),
            Span::styled(
                format!("{:>5} ", model.context_str()),
                Style::default().fg(Color::Rgb(80, 120, 80)).bg(bg),
            ),
            Span::styled(
                model.price_str(),
                Style::default().fg(Color::Rgb(100, 100, 100)).bg(bg),
            ),
        ])));
        visual_idx += 1;
    }

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(50, 50, 50))),
    );
    f.render_widget(list, chunks[1]);

    // Footer
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            " \u{2191}\u{2193}",
            Style::default().fg(Color::Rgb(80, 80, 80)),
        ),
        Span::styled(" navigate ", Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled("Enter", Style::default().fg(Color::Rgb(80, 80, 80))),
        Span::styled(" select ", Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled("Tab", Style::default().fg(Color::Rgb(80, 80, 80))),
        Span::styled(
            " filter provider ",
            Style::default().fg(Color::Rgb(50, 50, 50)),
        ),
        Span::styled("Esc", Style::default().fg(Color::Rgb(80, 80, 80))),
        Span::styled(" close ", Style::default().fg(Color::Rgb(50, 50, 50))),
        Span::styled(
            " \u{25c6}=reasoning \u{2295}=vision \u{26a1}=tools",
            Style::default().fg(Color::Rgb(60, 60, 60)),
        ),
    ]));
    f.render_widget(footer, chunks[2]);
}
