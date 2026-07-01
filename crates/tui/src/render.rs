//! Ratatui rendering — pure view function.
//!
//! All colors come from the active [`crate::theme::Theme`] (read via
//! `app.theme()`), never hardcoded — so the whole UI recolors uniformly
//! when the theme changes.

use crate::app::{App, ChatLine, Focus, LineKind, Modal, Role, WorkspaceTab};
use crate::command;
use crate::gh;
use crate::keymap;
use crate::theme::Theme;
use crate::toast::ToastKind;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, Wrap};

pub fn draw(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();

    // Paint the whole screen `background` (opencode paints its background).
    f.render_widget(
        Block::default().style(Style::default().bg(t.overlay_bg)),
        area,
    );

    // Columns: left content column + right Workspace panel (opencode-style).
    let sidebar_w = (area.width / 3).clamp(24, 42);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(sidebar_w)])
        .split(area);

    // Left column: header / transcript / prompt / footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header bar
            Constraint::Min(3),    // transcript
            Constraint::Length(5), // bordered prompt box (1 + 3 + 1)
            Constraint::Length(1), // footer
        ])
        .split(cols[0]);

    draw_header(f, app, chunks[0]);
    draw_chat(f, app, chunks[1]);
    draw_prompt(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);
    draw_workspace(f, app, cols[1]);

    // Overlays: approval popup (safety-critical) > command palette >
    // theme picker > which-key panel > modal.
    if app.approval_pending.is_some() {
        draw_approval_popup(f, app);
    } else if app.palette.open {
        draw_command_palette(f, app);
    } else if app.theme_picker.open {
        draw_theme_picker(f, app);
    } else if app.which_key.open {
        draw_which_key(f, app);
    } else if app.gh.open {
        draw_gh_panel(f, app);
    } else if let Some(modal) = app.modal {
        draw_modal(f, modal, app);
    }

    // Toasts float over everything, last and non-blocking.
    draw_toasts(f, app);
}

/// Header bar — a distinct strip: ‹ back affordance, session title,
/// model pill, and live hints (opencode top bar), on a panel background.
fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let model = clean_model_name(&app.model);
    let mut spans = vec![
        Span::styled(" ‹ back ", Style::default().fg(t.muted).bg(t.status_bg)),
        Span::styled(" ", Style::default().bg(t.status_bg)),
        Span::styled(
            clip(&app.session_title, 38),
            Style::default()
                .fg(t.accent)
                .add_modifier(Modifier::BOLD)
                .bg(t.status_bg),
        ),
    ];
    if !model.is_empty() {
        spans.push(Span::styled(
            format!("   ◆ {model}"),
            Style::default().fg(t.dim).bg(t.status_bg),
        ));
    }
    spans.push(Span::styled(
        format!("    {} tools", app.tool_count),
        Style::default().fg(t.muted).bg(t.status_bg),
    ));
    spans.push(Span::styled(
        "    Ctrl-P · ? ",
        Style::default().fg(t.muted).bg(t.status_bg),
    ));
    let line = Line::from(spans);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(t.status_bg)),
        area,
    );
}

// ── Toasts ────────────────────────────────────────────────────────
//
// Transient, non-blocking notifications (opencode `ui/toast`). Rendered
// last so they float over every overlay, but they never intercept keys.
// Stack at the bottom-center, just above the status bar.

fn draw_toasts(f: &mut Frame, app: &App) {
    let t = app.theme();
    // Defensive: also hide any that expired between ticks.
    let live: Vec<&crate::toast::Toast> = app.toasts.iter().filter(|x| !x.is_expired()).collect();
    if live.is_empty() {
        return;
    }
    let area = f.area();
    let width: u16 = 50;
    let count = live.len().min(5) as u16;
    let x = area.width.saturating_sub(width) / 2;
    let y = area.height.saturating_sub(4).saturating_sub(count);
    let rect = Rect::new(x, y, width, count);
    f.render_widget(Clear, rect);

    let lines: Vec<Line> = live
        .into_iter()
        .take(count as usize)
        .map(|toast| {
            let color = match toast.kind {
                ToastKind::Info => t.accent,
                ToastKind::Ok => t.ok,
                ToastKind::Warn => t.warn,
                ToastKind::Err => t.err,
            };
            Line::from(vec![
                Span::styled("▌", Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    clip(&toast.message, width as usize - 3),
                    Style::default().fg(t.text),
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rect);
}

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let mut lines: Vec<Line> = Vec::new();
    let mut thinking_shown = false;

    for msg in &app.messages {
        // Thinking tokens: show collapsed indicator or full text
        if matches!(msg.kind, LineKind::Thinking) {
            if app.thinking_expanded {
                // Show full thinking text, dimmed
                for (i, line_text) in msg.text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            Span::styled("◇ ", Style::default().fg(t.system)),
                            Span::styled(line_text.to_string(), Style::default().fg(t.dim)),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(line_text.to_string(), Style::default().fg(t.dim)),
                        ]));
                    }
                }
                if lines.last().is_some() {
                    lines.push(Line::raw(""));
                }
            } else if !thinking_shown {
                // Show a single collapsed indicator
                let char_count = msg.text.chars().count();
                lines.push(Line::from(vec![
                    Span::styled("◇ ", Style::default().fg(t.system)),
                    Span::styled(
                        format!("[thinking… {} chars — Ctrl-T to expand]", char_count),
                        Style::default().fg(t.dim),
                    ),
                ]));
                thinking_shown = true;
            }
            continue;
        }

        let prefix = match msg.role {
            Role::User => Span::styled(
                "> ",
                Style::default().fg(t.user).add_modifier(Modifier::BOLD),
            ),
            Role::Assistant => Span::styled("◆ ", Style::default().fg(t.accent)),
            Role::System => Span::styled("· ", Style::default().fg(t.system)),
            Role::Tool => Span::styled("⚙ ", Style::default().fg(t.warn)),
        };

        let style = match &msg.kind {
            LineKind::Error(_) => Style::default().fg(t.err),
            LineKind::Status(_) => Style::default().fg(t.system),
            LineKind::ToolStart { .. } => Style::default().fg(t.warn),
            LineKind::ToolResult { success: true, .. } => Style::default().fg(t.ok),
            LineKind::ToolResult { success: false, .. } => Style::default().fg(t.err),
            LineKind::Approval { .. } => {
                Style::default().fg(t.approval).add_modifier(Modifier::BOLD)
            }
            LineKind::View { .. } => Style::default().fg(t.user),
            LineKind::Thinking => Style::default().fg(t.dim),
            LineKind::Text if matches!(msg.role, Role::User) => {
                Style::default().fg(t.text).add_modifier(Modifier::BOLD)
            }
            LineKind::Text if matches!(msg.role, Role::Assistant) => Style::default().fg(t.text),
            LineKind::Text => Style::default(),
        };

        for (i, line_text) in msg.text.lines().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    prefix.clone(),
                    Span::styled(line_text.to_string(), style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(line_text.to_string(), style),
                ]));
            }
        }
        // Blank line between messages
        if lines.last().is_some() {
            lines.push(Line::raw(""));
        }
    }

    // If waiting and no tokens yet, show a loading spinner
    if app.is_waiting && app.first_token_time.is_none() {
        let spinner = match std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() % 4)
            .unwrap_or(0)
        {
            0 => "⠋",
            1 => "⠙",
            2 => "⠹",
            _ => "⠸",
        };
        lines.push(Line::from(vec![
            Span::styled("◆ ", Style::default().fg(t.accent)),
            Span::styled(
                spinner,
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" waiting for response…", Style::default().fg(t.system)),
        ]));
    } else if app.is_waiting {
        // Streaming — show pulse
        lines.push(Line::from(vec![
            Span::styled("◆ ", Style::default().fg(t.accent)),
            Span::styled(
                "…",
                Style::default()
                    .fg(t.accent)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
        ]));
    }

    let title = if app.model.is_empty() {
        " PRISM ".to_string()
    } else {
        format!(" PRISM · {} ", clean_model_name(&app.model))
    };
    let _ = title; // title now lives in the header bar; kept for diffs only.

    // Compute scroll bounds: auto-follow sticks to the bottom, manual scroll
    // clamps to content. `view_max_scroll` is read back by the key handlers,
    // which otherwise don't know the viewport height.
    let viewport = area.height;
    let content_lines = lines.len() as u16;
    let max_scroll = content_lines.saturating_sub(viewport);
    app.view_max_scroll.set(max_scroll);
    let effective_scroll = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    let paragraph = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0));

    f.render_widget(paragraph, area);

    // Scrollbar whenever the transcript overflows, so scrolling is discoverable.
    if max_scroll > 0 {
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut ratatui::widgets::ScrollbarState::new(content_lines as usize)
                .position(effective_scroll as usize),
        );
    }
}

/// Footer — live status + hints (opencode bottom bar), replacing the old status bar.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let model_display = if app.model.is_empty() {
        "—"
    } else {
        &app.model
    };
    let status = if app.is_waiting {
        "busy"
    } else {
        &app.status_text
    };
    let focus_indicator = match app.focus {
        Focus::Chat => " [CHAT] ",
        Focus::Input => " [INPUT] ",
        Focus::Workspace => " [WORKSPACE] ",
        Focus::Approval => " [APPROVAL] ",
    };

    let mut spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(
            format!(" {} ", status),
            Style::default().fg(t.status_fg).bg(t.status_bg),
        ),
        Span::raw(" "),
        Span::styled("model:", Style::default().fg(t.system)),
        Span::raw(" "),
        Span::styled(model_display, Style::default().fg(t.text)),
        Span::raw("  "),
    ];

    // Show tokens/sec when streaming (if metrics enabled)
    if app.show_metrics && app.tokens_per_sec > 0.0 {
        spans.push(Span::styled("tok/s:", Style::default().fg(t.system)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{:.1}", app.tokens_per_sec),
            Style::default().fg(t.ok),
        ));
        spans.push(Span::raw("  "));
    }

    // Show cost only if enabled (hide for local models)
    if app.show_cost && app.session_cost > 0.0 {
        spans.push(Span::styled("cost:", Style::default().fg(t.system)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("${:.4}", app.session_cost),
            Style::default().fg(t.text),
        ));
        spans.push(Span::raw("  "));
    }

    // Show collapsed thinking indicator
    let has_thinking = app
        .messages
        .iter()
        .any(|m| matches!(m.kind, LineKind::Thinking));
    if has_thinking && !app.thinking_expanded {
        spans.push(Span::styled(
            "[thinking hidden]",
            Style::default().fg(t.dim),
        ));
        spans.push(Span::raw("  "));
    }

    spans.push(Span::styled(focus_indicator, Style::default().fg(t.warn)));
    spans.push(Span::styled("   Ctrl-C quit", Style::default().fg(t.muted)));

    let line = Line::from(spans);
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(t.overlay_bg)),
        area,
    );
}

/// Bordered prompt box — the prominent input (opencode-style), with the
/// textarea rendered inside the block's inner area.
fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if app.focus == Focus::Input {
            t.accent
        } else {
            t.divider
        }))
        .title(Span::styled(
            " Prompt ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(t.panel));

    if app.focus == Focus::Input {
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(&app.input, inner);
    } else {
        let text = app.input.lines().join(" ");
        let display = if text.is_empty() {
            "type a message…  (press i to focus · ↵ send)".to_string()
        } else {
            text
        };
        let para = Paragraph::new(display)
            .style(Style::default().fg(t.muted).bg(t.panel))
            .block(block);
        f.render_widget(para, area);
    }
}

// ── Workspace sidebar ─────────────────────────────────────────────
//
// The right-hand panel. Derived purely from the message stream so the
// render stays a pure function of App state: tool executions, the
// activity feed, and touched files are all reconstructed from
// `app.messages`.

#[derive(Clone, Copy, PartialEq)]
enum ToolStatus {
    Running,
    Ok,
    Err,
}

struct ToolEntry {
    name: String,
    status: ToolStatus,
    elapsed_ms: Option<u64>,
    finding: Option<String>,
}

struct ActivityItem {
    kind: &'static str,
    label: String,
    glyph: &'static str,
    gcolor: Color,
}

struct FileEntry {
    status: &'static str,
    path: String,
}

fn draw_workspace(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    // Panel sidebar — opencode `backgroundPanel`, left-bordered.
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(t.divider))
        .style(Style::default().bg(t.panel));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let w = inner.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Workspace",
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    )));
    lines.push(workspace_tabs_line(app, t));
    if let Some(stats) = workspace_stats_line(app, t) {
        lines.push(stats);
    }
    if let Some(goal) = &app.goal {
        lines.push(Line::from(vec![
            Span::styled(" 🎯 ", Style::default().fg(t.accent)),
            Span::styled(clip(goal, w.saturating_sub(4)), Style::default().fg(t.text)),
        ]));
    }
    lines.push(Line::raw(""));

    match app.workspace_tab {
        WorkspaceTab::Tools => build_tools_lines(app, t, &mut lines, w),
        WorkspaceTab::Activity => build_activity_lines(app, t, &mut lines, w),
        WorkspaceTab::Files => build_files_lines(app, t, &mut lines, w),
    }

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.panel))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);
}

fn workspace_tabs_line(app: &App, t: Theme) -> Line<'static> {
    let tabs = [
        (WorkspaceTab::Activity, "Activity"),
        (WorkspaceTab::Tools, "Tools"),
        (WorkspaceTab::Files, "Files"),
    ];
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, (tab, label)) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        if *tab == app.workspace_tab {
            spans.push(Span::styled(
                format!("[{label}]"),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                (*label).to_string(),
                Style::default().fg(t.muted),
            ));
        }
    }
    Line::from(spans)
}

fn status_glyph(status: ToolStatus, t: Theme) -> (&'static str, Color) {
    match status {
        ToolStatus::Ok => ("✓", t.ok),
        ToolStatus::Err => ("✗", t.err),
        ToolStatus::Running => ("⚙", t.warn),
    }
}

/// Human-friendly model label for the header — drops any path prefix and the
/// `.gguf` extension so the header reads e.g. `PRISM · gemma-4-12B-it-…`.
fn clean_model_name(model: &str) -> String {
    let base = model.rsplit('/').next().unwrap_or(model);
    base.strip_suffix(".gguf").unwrap_or(base).to_string()
}

/// Compact session summary shown under the tabs on every workspace tab:
/// total tool calls, ✓/✗ counts, and a live "working" indicator.
fn workspace_stats_line(app: &App, t: Theme) -> Option<Line<'static>> {
    let tools = derive_tools(app);
    if tools.is_empty() && !app.is_waiting {
        return None;
    }
    let ok = tools.iter().filter(|x| x.status == ToolStatus::Ok).count();
    let err = tools.iter().filter(|x| x.status == ToolStatus::Err).count();
    let mut spans = vec![Span::styled(
        format!(" {} tools", tools.len()),
        Style::default().fg(t.dim),
    )];
    if ok > 0 {
        spans.push(Span::styled(
            format!(" · {ok} ✓"),
            Style::default().fg(t.ok),
        ));
    }
    if err > 0 {
        spans.push(Span::styled(
            format!(" · {err} ✗"),
            Style::default().fg(t.err),
        ));
    }
    if app.is_waiting {
        spans.push(Span::styled(" · ▶ working", Style::default().fg(t.warn)));
    }
    Some(Line::from(spans))
}

/// First non-empty line of a tool result, for the one-line finding preview.
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit" | "edit_file" | "create_file" | "apply_patch" | "file"
    )
}

/// Extract a file path from a write/edit tool's result, e.g.
/// "Updated crates/tui/src/app.rs" -> "crates/tui/src/app.rs".
fn extract_path(content: &str) -> Option<String> {
    let first = first_line(content);
    for kw in ["Updated ", "Wrote ", "Created ", "Modified ", "Edited "] {
        if let Some(rest) = first.strip_prefix(kw) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Reconstruct the list of tool executions from the message stream.
fn derive_tools(app: &App) -> Vec<ToolEntry> {
    let mut out: Vec<ToolEntry> = Vec::new();
    for m in &app.messages {
        match &m.kind {
            LineKind::ToolStart { tool_name, .. } => {
                out.push(ToolEntry {
                    name: tool_name.clone(),
                    status: ToolStatus::Running,
                    elapsed_ms: None,
                    finding: None,
                });
            }
            LineKind::ToolResult {
                tool_name,
                content,
                elapsed_ms,
                success,
            } => {
                let status = if *success {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Err
                };
                if let Some(e) = out
                    .iter_mut()
                    .rev()
                    .find(|e| e.name == *tool_name && e.status == ToolStatus::Running)
                {
                    e.status = status;
                    e.elapsed_ms = Some(*elapsed_ms);
                    e.finding = Some(first_line(content));
                } else {
                    out.push(ToolEntry {
                        name: tool_name.clone(),
                        status,
                        elapsed_ms: Some(*elapsed_ms),
                        finding: Some(first_line(content)),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn build_tools_lines(app: &App, t: Theme, lines: &mut Vec<Line<'static>>, w: usize) {
    let tools = derive_tools(app);
    if tools.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no tool activity yet)",
            Style::default().fg(t.muted),
        )));
        return;
    }
    let sel = app.workspace_selected.min(tools.len().saturating_sub(1));
    for (i, x) in tools.iter().enumerate() {
        let focused = app.focus == Focus::Workspace && i == sel;
        let prefix = if focused { "▸ " } else { "  " };
        let (glyph, gcolor) = status_glyph(x.status, t);
        let mut spans = vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(format!("{glyph} "), Style::default().fg(gcolor)),
            Span::styled(x.name.clone(), Style::default().fg(t.text)),
        ];
        if let Some(ms) = x.elapsed_ms {
            spans.push(Span::styled(
                format!("  {ms}ms"),
                Style::default().fg(t.muted),
            ));
        }
        lines.push(Line::from(spans));
        if let Some(finding) = &x.finding
            && !finding.is_empty()
        {
            lines.push(Line::from(Span::styled(
                format!("  {}", clip(finding, w.saturating_sub(2))),
                Style::default().fg(t.dim),
            )));
        }
    }
}

fn derive_activity(app: &App) -> Vec<ActivityItem> {
    let t = app.theme();
    let mut out: Vec<ActivityItem> = Vec::new();
    for m in &app.messages {
        match (&m.role, &m.kind) {
            (Role::User, LineKind::Text) => out.push(ActivityItem {
                kind: "prompt",
                label: format!("\"{}\"", m.text.trim()),
                glyph: "•",
                gcolor: t.accent,
            }),
            (
                _,
                LineKind::ToolResult {
                    tool_name,
                    content,
                    success,
                    ..
                },
            ) => {
                let (glyph, gcolor) = status_glyph(
                    if *success {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Err
                    },
                    t,
                );
                out.push(ActivityItem {
                    kind: "tool",
                    label: tool_name.clone(),
                    glyph,
                    gcolor,
                });
                if is_file_tool(tool_name)
                    && let Some(path) = extract_path(content)
                {
                    out.push(ActivityItem {
                        kind: "file",
                        label: path,
                        glyph: "~",
                        gcolor: t.dim,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn build_activity_lines(app: &App, t: Theme, lines: &mut Vec<Line<'static>>, w: usize) {
    let items = derive_activity(app);
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no activity yet)",
            Style::default().fg(t.muted),
        )));
        return;
    }
    let sel = app.workspace_selected.min(items.len().saturating_sub(1));
    for (i, it) in items.iter().enumerate() {
        let focused = app.focus == Focus::Workspace && i == sel;
        let prefix = if focused { "▸ " } else { "  " };
        let n = i + 1;
        let lead = format!("{prefix}{n}. {} ", it.kind);
        let budget = w.saturating_sub(lead.chars().count() + 2).max(3);
        let label = clip(&it.label, budget);
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(format!("{n}. "), Style::default().fg(t.muted)),
            Span::styled(format!("{} ", it.kind), Style::default().fg(t.muted)),
            Span::styled(label, Style::default().fg(t.text)),
            Span::raw(" "),
            Span::styled(it.glyph.to_string(), Style::default().fg(it.gcolor)),
        ]));
    }
}

fn derive_files(app: &App) -> Vec<FileEntry> {
    let mut out: Vec<FileEntry> = Vec::new();
    for m in &app.messages {
        if let LineKind::ToolResult {
            tool_name,
            content,
            success,
            ..
        } = &m.kind
            && *success
            && is_file_tool(tool_name)
            && let Some(path) = extract_path(content)
            && !out.iter().any(|f| f.path == path)
        {
            out.push(FileEntry { status: "~", path });
        }
    }
    out
}

fn build_files_lines(app: &App, t: Theme, lines: &mut Vec<Line<'static>>, w: usize) {
    let files = derive_files(app);
    if files.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no files touched yet)",
            Style::default().fg(t.muted),
        )));
        return;
    }
    let sel = app.workspace_selected.min(files.len().saturating_sub(1));
    for (i, fe) in files.iter().enumerate() {
        let focused = app.focus == Focus::Workspace && i == sel;
        let prefix = if focused { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(format!("{} ", fe.status), Style::default().fg(t.warn)),
            Span::styled(
                clip(&fe.path, w.saturating_sub(4)),
                Style::default().fg(t.text),
            ),
        ]));
        if focused && app.workspace_expanded {
            lines.push(Line::from(Span::styled(
                format!("  path: {}", clip(&fe.path, w.saturating_sub(8))),
                Style::default().fg(t.dim),
            )));
            lines.push(Line::from(Span::styled(
                "  action: modified",
                Style::default().fg(t.dim),
            )));
        }
    }
}

// ── Modals (help / cost) ──────────────────────────────────────────

fn draw_modal(f: &mut Frame, modal: Modal, app: &App) {
    let t = app.theme();
    let (title, lines) = match modal {
        Modal::Help => ("Help — keys & commands", help_lines(t)),
        Modal::Cost => ("Session cost & tokens", cost_lines(app, t)),
        Modal::Model => ("Model", model_lines(app, t)),
        Modal::Tools => ("Tools & MCP", tools_lines(t, app)),
    };
    let area = centered_rect(62, 70, f.area());
    f.render_widget(Clear, area);
    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    format!(" {title} "),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn kv_row(t: Theme, k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {k:<16}"),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(v.to_string(), Style::default().fg(t.dim)),
    ])
}

fn section(t: Theme, label: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {label}"),
        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
    ))
}

fn help_lines(t: Theme) -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        section(t, "Navigation"),
        kv_row(t, "Tab", "cycle focus: input → workspace → chat"),
        kv_row(t, "i / Esc", "focus input / leave a panel"),
        kv_row(t, "PgUp / PgDn", "scroll transcript (from any focus)"),
        kv_row(t, "mouse wheel", "scroll transcript"),
        kv_row(t, "↑ ↓ / k j", "scroll one line (chat focus)"),
        kv_row(t, "g / G", "jump to top / bottom (chat focus)"),
        Line::raw(""),
        section(t, "Workspace sidebar"),
        kv_row(t, "← / →", "switch Activity / Tools / Files"),
        kv_row(t, "↑ / ↓", "move selection"),
        kv_row(t, "Enter / Space", "expand selected item"),
        Line::raw(""),
        section(t, "Approvals & display"),
        kv_row(t, "y / a / n", "allow / allow-all / deny a tool"),
        kv_row(t, "Ctrl-T", "toggle thinking tokens"),
        kv_row(t, "Ctrl-M / Ctrl-$", "toggle metrics / cost"),
        Line::raw(""),
        section(t, "Commands"),
        kv_row(t, "Ctrl-P", "command palette — run any command"),
        kv_row(t, "?", "keybindings panel (scrollable)"),
        kv_row(t, "/help", "this screen"),
        kv_row(t, "/cost", "token & cost breakdown"),
        kv_row(t, "/model", "active model + how to switch"),
        kv_row(t, "/mcp", "tools & MCP configuration"),
        kv_row(
            t,
            "/goal <text>",
            "standing goal, sent to the agent each turn",
        ),
        Line::raw(""),
        Line::from(Span::styled(
            "  press any key to close",
            Style::default().fg(t.muted),
        )),
    ]
}

fn tools_lines(t: Theme, app: &App) -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        kv_row(t, "Tools loaded", &format!("{}", app.tool_count)),
        Line::raw(""),
        section(t, "How tools load"),
        Line::from(Span::styled(
            "  Native + Python tools come from the PRISM",
            Style::default().fg(t.dim),
        )),
        Line::from(Span::styled(
            "  registry; MCP servers from .mcp.json.",
            Style::default().fg(t.dim),
        )),
        Line::from(Span::styled(
            "  Edit .mcp.json and relaunch to add/remove MCP.",
            Style::default().fg(t.dim),
        )),
        Line::raw(""),
        section(t, "Discover at runtime"),
        Line::from(Span::styled(
            "  /tools    list the live catalog (backend)",
            Style::default().fg(t.dim),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "  press any key to close",
            Style::default().fg(t.muted),
        )),
    ]
}

fn model_lines(app: &App, t: Theme) -> Vec<Line<'static>> {
    let model = {
        let m = clean_model_name(&app.model);
        if m.is_empty() { "—".to_string() } else { m }
    };
    vec![
        Line::raw(""),
        kv_row(t, "Active model", &model),
        kv_row(t, "Tools loaded", &format!("{}", app.tool_count)),
        Line::raw(""),
        section(t, "Switch model"),
        Line::from(Span::styled(
            "  /model <name>   ask the backend to switch",
            Style::default().fg(t.dim),
        )),
        Line::from(Span::styled(
            "  or set it in your PRISM config / launch flags",
            Style::default().fg(t.dim),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "  press any key to close",
            Style::default().fg(t.muted),
        )),
    ]
}

fn cost_lines(app: &App, t: Theme) -> Vec<Line<'static>> {
    let model = clean_model_name(&app.model);
    let mut lines = vec![
        Line::raw(""),
        kv_row(t, "Model", if model.is_empty() { "—" } else { &model }),
        kv_row(t, "Session cost", &format!("${:.4}", app.session_cost)),
        kv_row(t, "This turn", &format!("${:.4}", app.turn_cost)),
        kv_row(t, "Tokens (turn)", &format!("{}", app.tokens_received)),
        kv_row(t, "Throughput", &format!("{:.1} tok/s", app.tokens_per_sec)),
    ];
    if app.session_cost == 0.0 {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  (local model — no metered cost)",
            Style::default().fg(t.muted),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  press any key to close",
        Style::default().fg(t.muted),
    )));
    lines
}

// ── Command palette (Ctrl-P) ──────────────────────────────────────
//
// opencode-style fuzzy command launcher.  Reads the palette query from
// `App` and runs the pure [`command::fuzzy_sorted`] filter, so the view
// stays a pure function of state (no I/O).  The selected row is
// highlighted with reverse video.

// ── GitHub panel ──────────────────────────────────────────────────
//
// Issues / PRs / CI status, backed by `/gh` (which shells to `gh`) and the
// `ui.gh.data` notification. Fuzzy-filterable list, tab switch, link action.

fn draw_gh_panel(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(82, 80, f.area());
    f.render_widget(Clear, area);

    let rows = gh::filtered_rows(&app.gh);
    let sel = app.gh.selected.min(rows.len().saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();

    // Tab bar.
    let mut tab_spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, tab) in gh::GhTab::ALL.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::raw("  "));
        }
        if *tab == app.gh.tab {
            tab_spans.push(Span::styled(
                format!("[{}]", tab.as_str()),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(
                tab.as_str().to_string(),
                Style::default().fg(t.muted),
            ));
        }
    }
    tab_spans.push(Span::raw("    "));
    tab_spans.push(Span::styled(
        if app.gh.repo.is_empty() {
            String::new()
        } else {
            format!("◆ {}", app.gh.repo)
        },
        Style::default().fg(t.dim),
    ));
    lines.push(Line::from(tab_spans));

    // Query / status line.
    let qdisp = if app.gh.query.is_empty() {
        if app.gh.loading {
            "loading…".to_string()
        } else {
            "type to filter…".to_string()
        }
    } else {
        app.gh.query.clone()
    };
    let qcolor = if app.gh.query.is_empty() {
        t.muted
    } else {
        t.text
    };
    lines.push(Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(qdisp, Style::default().fg(qcolor)),
        Span::styled(
            format!("    ({} shown)", rows.len()),
            Style::default().fg(t.muted),
        ),
    ]));
    lines.push(Line::raw(""));

    if let Some(err) = &app.gh.error {
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {err}"),
            Style::default().fg(t.err),
        )));
        lines.push(Line::from(Span::styled(
            "  Is `gh` installed and authenticated? (`gh auth status`)",
            Style::default().fg(t.muted),
        )));
    } else if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            if app.gh.loading {
                "  fetching…".to_string()
            } else {
                "  (none)".to_string()
            },
            Style::default().fg(t.muted),
        )));
    } else {
        for (i, row) in rows.iter().enumerate() {
            let focused = i == sel;
            let mut spans = vec![
                Span::styled(format!("  {:<7}", row.key), Style::default().fg(t.accent)),
                Span::styled(clip(&row.title, 48), Style::default().fg(t.text)),
                Span::raw("  "),
                Span::styled(clip(&row.detail, 24), Style::default().fg(t.dim)),
            ];
            if focused {
                spans.push(Span::styled("  ◀", Style::default().fg(t.accent)));
            }
            let mut line = Line::from(spans);
            if focused {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(line);
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ←/→ tabs · filter · j/k move · ↵ post link · Esc close",
        Style::default().fg(t.muted),
    )));

    let title = format!(" GitHub — {} ", app.gh.tab.as_str());
    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    title,
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

fn draw_command_palette(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(70, 60, f.area());
    f.render_widget(Clear, area);

    let cmds = command::fuzzy_sorted(&app.palette.query);
    let sel = app.palette.selected.min(cmds.len().saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();

    // Query echo line — doubles as the search input affordance.
    let query_display = if app.palette.query.is_empty() {
        "type to search…".to_string()
    } else {
        app.palette.query.clone()
    };
    let query_color = if app.palette.query.is_empty() {
        t.muted
    } else {
        t.text
    };
    lines.push(Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(query_display, Style::default().fg(query_color)),
    ]));
    lines.push(Line::raw(""));

    if cmds.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matching commands",
            Style::default().fg(t.muted),
        )));
    } else {
        for (i, c) in cmds.iter().enumerate() {
            let focused = i == sel;
            let mut spans = vec![
                Span::styled(
                    format!("  {:<20}", c.title),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(clip(c.description, 30), Style::default().fg(t.dim)),
            ];
            // Right-aligned keybind hint, padded to the inner width.
            let used = 2 + 20 + c.description.chars().count().min(30);
            let inner_w = area.width.saturating_sub(2) as usize;
            let pad = inner_w.saturating_sub(used + c.keybind.chars().count() + 1);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                c.keybind.to_string(),
                Style::default().fg(t.muted),
            ));
            let mut row = Line::from(spans);
            if focused {
                row = row.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(row);
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ navigate · ↵ run · Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.accent))
            .title(Span::styled(
                " Commands ",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(para, area);
}

// ── Theme picker ──────────────────────────────────────────────────
//
// opencode-style `dialog-theme-list`. Each row shows a swatch rendered in
// the candidate theme's accent, so the user previews the palette inline.
// The active theme is only changed on Enter.

fn draw_theme_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(60, 50, f.area());
    f.render_widget(Clear, area);

    let last = crate::theme::THEMES.len().saturating_sub(1);
    let sel = app.theme_picker.selected.min(last);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  active: {}", t.name),
        Style::default().fg(t.muted),
    )));
    lines.push(Line::raw(""));

    for (i, th) in crate::theme::THEMES.iter().enumerate() {
        let focused = i == sel;
        let active = i == app.theme_index;
        let mark = if active { "● " } else { "  " };
        let mut spans = vec![
            Span::styled(format!("  {mark}"), Style::default().fg(th.accent)),
            Span::styled(
                format!("{:<10}", th.name),
                Style::default().fg(th.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  sample ", Style::default().fg(th.dim)),
            Span::styled("ok ", Style::default().fg(th.ok)),
            Span::styled("err ", Style::default().fg(th.err)),
            Span::styled("warn", Style::default().fg(th.warn)),
        ];
        if focused {
            spans.push(Span::styled("  ◀", Style::default().fg(t.accent)));
        }
        let mut row = Line::from(spans);
        if focused {
            row = row.style(Style::default().add_modifier(Modifier::REVERSED));
        }
        lines.push(row);
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ choose · ↵ apply · Esc cancel",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.accent))
            .title(Span::styled(
                " Theme ",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(para, area);
}

// ── Which-key panel (`?`) ──────────────────────────────────────────
//
// opencode-style keymap reference: every binding from `keymap::KEYMAP`,
// grouped by category, scrollable. The renderer writes the max scroll
// offset back into `App` so the key handler can clamp (same pattern as
// the chat viewport). The view stays a pure function of state.

fn draw_which_key(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(80, 80, f.area());
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    for cat in keymap::categories() {
        lines.push(Line::from(Span::styled(
            format!(" {cat}"),
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        )));
        for b in keymap::bindings_in(cat) {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{:<22}", b.keys), Style::default().fg(t.accent)),
                Span::styled(b.description.to_string(), Style::default().fg(t.dim)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Scroll bounds: content height minus the inner viewport (area − 2 borders).
    let content_lines = lines.len() as u16;
    let viewport = area.height.saturating_sub(2);
    let max_scroll = content_lines.saturating_sub(viewport);
    app.whichkey_max_scroll.set(max_scroll);
    let effective_scroll = app.which_key.scroll.min(max_scroll);

    let para = Paragraph::new(lines).scroll((effective_scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.accent))
            .title(Span::styled(
                " Keybindings — j/k scroll · ? or Esc close ",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(para, area);
}

fn draw_approval_popup(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(60, 20, f.area());
    f.render_widget(Clear, area);

    let (tool, message) = app.approval_pending.as_ref().unwrap();

    let popup = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "  ⚠ APPROVAL REQUIRED  ",
            Style::default()
                .fg(t.overlay_bg)
                .bg(t.approval)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Tool: "),
            Span::styled(
                tool,
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(message, Style::default().fg(t.text)),
        ]),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::raw("  [y] "),
            Span::styled("Allow", Style::default().fg(t.ok)),
            Span::raw("   [a] "),
            Span::styled("Allow all", Style::default().fg(t.warn)),
            Span::raw("   [n] "),
            Span::styled("Deny", Style::default().fg(t.err)),
        ]),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.approval)),
    )
    .alignment(Alignment::Left);

    f.render_widget(popup, area);
}

/// Helper: centered rect for popups.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
