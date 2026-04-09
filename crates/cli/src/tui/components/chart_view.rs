#![allow(dead_code)]
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

/// A data point for scatter/line plots
#[derive(Debug, Clone)]
pub struct DataPoint {
    pub x: f64,
    pub y: f64,
    pub label: Option<String>,
}

/// Draw a scatter plot in the given area
pub fn draw_scatter(
    f: &mut Frame,
    title: &str,
    points: &[DataPoint],
    x_label: &str,
    y_label: &str,
    area: Rect,
) {
    if points.is_empty() {
        let p = ratatui::widgets::Paragraph::new("No data")
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(p, area);
        return;
    }

    let x_min = points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let x_max = points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    let y_min = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let y_max = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);

    let x_range = (x_max - x_min).max(1.0);
    let y_range = (y_max - y_min).max(1.0);

    let coords: Vec<(f64, f64)> = points.iter().map(|p| (p.x, p.y)).collect();

    let canvas = Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
                .title(Span::styled(
                    format!(" {title} ({x_label} vs {y_label}) "),
                    Style::default().fg(Color::Cyan),
                )),
        )
        .x_bounds([x_min - x_range * 0.05, x_max + x_range * 0.05])
        .y_bounds([y_min - y_range * 0.05, y_max + y_range * 0.05])
        .marker(Marker::Braille)
        .paint(move |ctx| {
            ctx.draw(&Points {
                coords: &coords,
                color: Color::Cyan,
            });
        });

    f.render_widget(canvas, area);
}

/// Draw a simple bar chart (horizontal bars)
pub fn draw_bars(f: &mut Frame, title: &str, bars: &[(String, f64)], area: Rect) {
    use ratatui::text::Line;
    use ratatui::widgets::{List, ListItem};

    let max_val = bars.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
    let bar_width = (area.width as usize).saturating_sub(25);

    let items: Vec<ListItem> = bars
        .iter()
        .map(|(label, value)| {
            let pct = if max_val > 0.0 {
                (value / max_val).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let filled = (pct * bar_width as f64).round() as usize;
            let bar = "\u{2588}".repeat(filled);

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        " {:<15}",
                        if label.len() > 15 {
                            &label[..15]
                        } else {
                            label
                        }
                    ),
                    Style::default().fg(Color::Rgb(160, 160, 160)),
                ),
                Span::styled(bar, Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!(" {value:.1}"),
                    Style::default().fg(Color::Rgb(100, 100, 100)),
                ),
            ]))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(Color::Cyan),
        ));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}
