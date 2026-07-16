//! Ratatui rendering — pure view function.
//!
//! All colors come from the active [`crate::theme::Theme`] (read via
//! `app.theme()`), never hardcoded — so the whole UI recolors uniformly
//! when the theme changes.

use crate::app::{App, ChatLine, Focus, LineKind, Modal, Role, WorkspaceTab, first_line};
use crate::command;
use crate::gh;
use crate::keymap;
use crate::markdown;
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
    } else if app.form.is_some() {
        draw_form_pane(f, app);
    } else if app.knowledge.open {
        draw_knowledge_pane(f, app);
    } else if app.notebook.open {
        draw_notebook_pane(f, app);
    } else if app.theme_picker.open {
        draw_theme_picker(f, app);
    } else if app.which_key.open {
        draw_which_key(f, app);
    } else if app.link_picker.open {
        draw_link_picker(f, app);
    } else if app.gh.open {
        draw_gh_panel(f, app);
    } else if app.model_picker.open {
        draw_model_picker(f, app);
    } else if app.gpu_picker.open {
        draw_gpu_picker(f, app);
    } else if app.node_picker.open {
        draw_node_picker(f, app);
    } else if app.account.open {
        draw_account(f, app);
    } else if app.session_picker.open {
        draw_session_picker(f, app);
    } else if app.view.open {
        draw_view_panel(f, app);
    } else if app.tools_window.open {
        draw_tools_window(f, app);
    } else if app.status_window.open {
        draw_status_window(f, app);
    } else if app.config_window.open {
        draw_config_window(f, app);
    } else if app.apikey_window.open {
        draw_apikey_window(f, app);
    } else if app.home.open {
        draw_home(f, app);
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

    for (idx, msg) in app.messages.iter().enumerate() {
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

        match (&msg.role, &msg.kind) {
            // ── User turn: labeled header + colored gutter bar ──────
            (Role::User, LineKind::Text) => {
                lines.push(Line::from(Span::styled(
                    "❯ You",
                    Style::default().fg(t.user).add_modifier(Modifier::BOLD),
                )));
                for line_text in msg.text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("▌ ", Style::default().fg(t.user)),
                        Span::styled(line_text.to_string(), Style::default().fg(t.text)),
                    ]));
                }
            }
            // ── Assistant turn: labeled header + markdown body ──────
            (Role::Assistant, LineKind::Text) => {
                lines.push(Line::from(Span::styled(
                    "◆ PRISM",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )));
                for md in markdown::markdown_lines(&msg.text, t, area.width.saturating_sub(2)) {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(md.spans);
                    lines.push(Line::from(spans));
                }
            }
            // ── Tool activity: indented + grouped under the turn ────
            (Role::Tool, kind) => {
                let (glyph, gcolor, style) = match kind {
                    LineKind::ToolResult { success: false, .. } | LineKind::Error(_) => {
                        ("✗", t.err, Style::default().fg(t.err))
                    }
                    LineKind::ToolResult { .. } => ("✓", t.ok, Style::default().fg(t.dim)),
                    _ => ("⚙", t.warn, Style::default().fg(t.dim)),
                };
                for (i, line_text) in msg.text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(format!("{glyph} "), Style::default().fg(gcolor)),
                            Span::styled(line_text.to_string(), style),
                        ]));
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(line_text.to_string(), style),
                        ]));
                    }
                }
            }
            // ── System: status lines, errors, approval records ──────
            _ => {
                let style = match &msg.kind {
                    LineKind::Error(_) => Style::default().fg(t.err),
                    LineKind::Approval { .. } => {
                        Style::default().fg(t.approval).add_modifier(Modifier::BOLD)
                    }
                    _ => Style::default().fg(t.system),
                };
                for (i, line_text) in msg.text.lines().enumerate() {
                    let lead = if i == 0 {
                        Span::styled("· ", Style::default().fg(t.system))
                    } else {
                        Span::raw("  ")
                    };
                    lines.push(Line::from(vec![
                        lead,
                        Span::styled(line_text.to_string(), style),
                    ]));
                }
            }
        }

        // Blank line between turns; consecutive tool rows stay grouped.
        let next_is_tool = app
            .messages
            .get(idx + 1)
            .is_some_and(|m| matches!(m.role, Role::Tool));
        if !(matches!(msg.role, Role::Tool) && next_is_tool) {
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

    // Compute scroll bounds from the ACTUAL wrapped height, not the raw line
    // count. The transcript wraps long lines, and `Paragraph::scroll` counts
    // in wrapped rows — so measuring with ratatui's own `line_count(width)`
    // is what makes the offset map 1:1 to what's on screen. Using the
    // unwrapped `lines.len()` left the final wrapped rows unreachable and
    // drifted the scrollbar off-axis.
    let viewport = area.height;
    let paragraph = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .wrap(Wrap { trim: false });
    let content_lines = paragraph.line_count(area.width) as u16;
    let max_scroll = content_lines.saturating_sub(viewport);
    app.view_max_scroll.set(max_scroll);
    let effective_scroll = if app.auto_scroll {
        max_scroll
    } else {
        crate::app::clamp_scroll(app.scroll_offset, content_lines, viewport)
    };

    f.render_widget(paragraph.scroll((effective_scroll, 0)), area);

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

    // Org credit balance (platform billing). Only shown when known — a failed
    // fetch leaves it absent rather than displaying a misleading zero.
    if let Some(millicredits) = app.credits {
        spans.push(Span::styled("credits:", Style::default().fg(t.system)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            prism_client::billing::format_credits(millicredits),
            Style::default().fg(t.ok),
        ));
        spans.push(Span::raw("  "));
    }

    // Show tokens/sec when streaming (if metrics enabled)
    if app.show_metrics && app.tokens_per_sec > 0.0 {
        spans.push(Span::styled("tok/s:", Style::default().fg(t.system)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("~{:.1}", app.tokens_per_sec),
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

    // Collapsed-thinking affordance. Reads as a hint (with its toggle key),
    // not a status — the stress-test watcher flagged the old "[thinking
    // hidden]" text as a stuck state because nothing said how to act on it.
    let has_thinking = app
        .messages
        .iter()
        .any(|m| matches!(m.kind, LineKind::Thinking));
    if has_thinking && !app.thinking_expanded {
        spans.push(Span::styled(
            "[thinking · Ctrl-T]",
            Style::default().fg(t.dim),
        ));
        spans.push(Span::raw("  "));
    }

    // Copy mode is a modal input state — surface it prominently so the user
    // knows mouse selection is enabled and how to leave.
    if app.copy_mode {
        spans.push(Span::styled(
            " COPY MODE — mouse selection enabled, Ctrl-Y to exit ",
            Style::default()
                .fg(t.status_fg)
                .bg(t.accent)
                .add_modifier(Modifier::BOLD),
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
            // Mode-aware hint: while an approval modal has focus, Enter
            // approves the tool — telling the user "↵ send" there is a lie.
            if app.focus == Focus::Approval {
                "tool approval pending…  (y allow · a always allow this tool · n deny)".to_string()
            } else {
                "type a message…  (press i to focus · ↵ send)".to_string()
            }
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

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
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
    // The Tools tab shows the LIVE catalog (the actual tools), with any
    // run-activity beneath it.
    if !app.tool_catalog.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" Catalog · {} tools", app.tool_catalog.len()),
            Style::default().fg(t.dim),
        )));
        let sel = app.workspace_selected.min(app.tool_catalog.len() - 1);
        for (i, tool) in app.tool_catalog.iter().enumerate() {
            let focused = app.focus == Focus::Workspace && i == sel;
            let prefix = if focused { "▸ " } else { "  " };
            let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let approval = tool
                .get("approval")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let (mark, mcolor) = if approval {
                ("⚠", t.warn)
            } else {
                ("✓", t.ok)
            };
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
                Span::styled(format!("{mark} "), Style::default().fg(mcolor)),
                Span::styled(clip(name, w.saturating_sub(4)), Style::default().fg(t.text)),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            " Run activity",
            Style::default().fg(t.dim),
        )));
    }

    let tools = derive_tools(app);
    if tools.is_empty() {
        if app.tool_catalog.is_empty() {
            lines.push(Line::from(Span::styled(
                "  loading tools…",
                Style::default().fg(t.muted),
            )));
        }
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

fn build_activity_lines(app: &App, t: Theme, lines: &mut Vec<Line<'static>>, w: usize) {
    let items = app.derive_activity();
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
        let (glyph, gcolor) = match (it.kind, it.ok) {
            ("prompt", _) => ("•", t.accent),
            ("file", _) => ("~", t.dim),
            (_, Some(false)) => status_glyph(ToolStatus::Err, t),
            _ => status_glyph(ToolStatus::Ok, t),
        };
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
            Span::styled(glyph.to_string(), Style::default().fg(gcolor)),
        ]));
    }
}

fn build_files_lines(app: &App, t: Theme, lines: &mut Vec<Line<'static>>, w: usize) {
    let files = app.derive_files();
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
            Span::styled("~ ".to_string(), Style::default().fg(t.warn)),
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
        kv_row(t, "o", "open a link from the transcript (chat focus)"),
        Line::raw(""),
        section(t, "Workspace sidebar"),
        kv_row(t, "← / →", "switch Activity / Tools / Files"),
        kv_row(t, "↑ / ↓", "move selection"),
        kv_row(t, "Enter", "open details for the selected item"),
        kv_row(t, "Space", "expand selected item inline"),
        Line::raw(""),
        section(t, "Approvals & display"),
        kv_row(t, "y / a / n", "allow / allow-all / deny a tool"),
        kv_row(t, "Ctrl-T", "toggle thinking tokens"),
        kv_row(t, "Ctrl-M / Ctrl-$", "toggle metrics / cost"),
        kv_row(
            t,
            "Ctrl-Y",
            "copy mode — drag-select/copy (mouse capture off)",
        ),
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
    let mut lines = vec![
        Line::raw(""),
        kv_row(t, "Tools loaded", &format!("{}", app.tool_count)),
        Line::raw(""),
        section(t, "MCP servers (~/.prism/mcp.json)"),
    ];
    // Group MCP-sourced catalog entries by their server (source_detail).
    let mut by_server: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for tool in &app.tool_catalog {
        if tool.get("source").and_then(|s| s.as_str()) == Some("mcp") {
            let server = tool
                .get("source_detail")
                .and_then(|s| s.as_str())
                .unwrap_or("?")
                .to_string();
            let name = tool
                .get("name")
                .and_then(|s| s.as_str())
                .unwrap_or("?")
                .to_string();
            by_server.entry(server).or_default().push(name);
        }
    }
    if by_server.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none connected — add servers to ~/.prism/mcp.json",
            Style::default().fg(t.dim),
        )));
        lines.push(Line::from(Span::styled(
            "  and relaunch (stdio transport: command + args)",
            Style::default().fg(t.dim),
        )));
    } else {
        for (server, tools) in &by_server {
            lines.push(Line::from(Span::styled(
                format!("  {server} — {} tools", tools.len()),
                Style::default().fg(t.dim),
            )));
            for tool in tools {
                lines.push(Line::from(Span::styled(
                    format!("    {tool}"),
                    Style::default().fg(t.muted),
                )));
            }
        }
    }
    lines.extend([
        Line::raw(""),
        section(t, "How tools load"),
        Line::from(Span::styled(
            "  Native + Python tools come from the PRISM",
            Style::default().fg(t.dim),
        )),
        Line::from(Span::styled(
            "  registry; MCP tools from ~/.prism/mcp.json.",
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
    ]);
    lines
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
            "  or ask the agent to list available models",
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
        kv_row(
            t,
            "Tokens (turn)",
            &format!("~{} (est)", app.tokens_received),
        ),
        kv_row(
            t,
            "Throughput",
            &format!("~{:.1} tok/s", app.tokens_per_sec),
        ),
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

// ── Model picker ──────────────────────────────────────────────────
//
// opencode-style fuzzy model switcher over the hosted catalog. Populated
// from `/models list` (ui.model.list); Enter sends `/model <id>`.

fn model_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

// ── Account (MARC27 login/logout) ─────────────────────────────────
//
// Reads ~/.prism/credentials.json for status (client-side) and dispatches
// Login/Logout to the backend's existing /login (device flow) and /logout.

// ── Session picker (list / resume) ────────────────────────────────

/// Visible row window `[start, end)` that keeps `sel` in view (centered),
/// so long lists scroll to follow the selection. `viewport` = max rows shown.
fn scroll_window(sel: usize, total: usize, viewport: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let vp = viewport.min(total);
    let mut start = sel.saturating_sub(vp / 2);
    let max_start = total.saturating_sub(vp);
    if start > max_start {
        start = max_start;
    }
    (start, start + vp)
}

fn fmt_time(ts: f64) -> String {
    // Render a unix timestamp as a short UTC date-time. Best-effort.
    if ts <= 0.0 {
        return String::new();
    }
    let secs = ts as i64;
    let days = secs / 86400;
    let (y, mo, d) = (
        (days / 365) + 1970,
        ((days % 365) / 30) + 1,
        (days % 30) + 1,
    );
    let hh = (secs % 86400) / 3600;
    let mm = (secs % 3600) / 60;
    format!("{y}-{mo:02}-{d:02} {hh:02}:{mm:02}")
}

// ── View panel (tabbed / scrollable results) ──────────────────────
//
// Renders `ui.view` payloads (from /tools /status /context /files /tasks
// /memory /permissions /usage /doctor /config /diff …) as a tabbed,
// scrollable surface instead of a flat chat dump.

fn draw_view_panel(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(86, 86, f.area());
    f.render_widget(Clear, area);

    let ntabs = app.view.tabs.len().max(1);
    let active = app.view.active_tab.min(ntabs - 1);
    let (tab_title, body) = app.view.tabs.get(active).cloned().unwrap_or_default();

    // Header: title + tab bar.
    let mut header_spans: Vec<Span> = vec![Span::styled(
        format!(" {}", app.view.title),
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    )];
    if ntabs > 1 {
        header_spans.push(Span::raw("   "));
        for (i, (tt, _)) in app.view.tabs.iter().enumerate() {
            if i > 0 {
                header_spans.push(Span::raw("  "));
            }
            if i == active {
                header_spans.push(Span::styled(
                    format!("[{tt}]"),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ));
            } else {
                header_spans.push(Span::styled(tt.clone(), Style::default().fg(t.muted)));
            }
        }
    }

    // Body lines + scroll bounds (content height − inner viewport).
    let body_empty = body.trim().is_empty();
    let body_lines: Vec<Line> = if body_empty {
        vec![Line::from(Span::styled(
            format!(
                "  nothing to show for {} yet — start a conversation",
                app.view.title
            ),
            Style::default().fg(t.muted),
        ))]
    } else {
        body.lines()
            .map(|l| {
                // Diff-aware coloring: additions green, deletions red, hunk
                // headers accent, file headers bold. Makes /diff a real patch viewer.
                if l.starts_with("+++") || l.starts_with("---") {
                    Line::styled(
                        l.to_string(),
                        Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                    )
                } else if l.starts_with("diff ") || l.starts_with("Index:") {
                    Line::styled(
                        l.to_string(),
                        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                    )
                } else if l.starts_with("@@") {
                    Line::styled(l.to_string(), Style::default().fg(t.accent))
                } else if l.starts_with('+') {
                    Line::styled(l.to_string(), Style::default().fg(t.ok))
                } else if l.starts_with('-') {
                    Line::styled(l.to_string(), Style::default().fg(t.err))
                } else {
                    Line::raw(l.to_string())
                }
            })
            .collect()
    };
    let content = body_lines.len() as u16;
    let viewport = area.height.saturating_sub(4); // borders + header + footer
    let max_scroll = content.saturating_sub(viewport);
    app.view.max_scroll.set(max_scroll);
    let scroll = app.view.scroll.min(max_scroll);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(header_spans));
    lines.push(Line::raw(""));
    lines.extend(body_lines);
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        if ntabs > 1 {
            "  ←/→ tabs · j/k scroll · Esc close".to_string()
        } else {
            "  j/k scroll · Esc close".to_string()
        },
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .scroll((scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent)),
        );
    f.render_widget(para, area);
}

fn draw_session_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(82, 80, f.area());
    f.render_widget(Clear, area);

    let indices = app.session_filtered_indices();
    let sel = app
        .session_picker
        .selected
        .min(indices.len().saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    let qdisp = if app.session_picker.query.is_empty() {
        if app.session_picker.loading {
            "loading sessions…".to_string()
        } else {
            "type to filter…".to_string()
        }
    } else {
        app.session_picker.query.clone()
    };
    let qcolor = if app.session_picker.query.is_empty() {
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
            format!("    ({} shown)", indices.len()),
            Style::default().fg(t.muted),
        ),
    ]));
    lines.push(Line::raw(""));

    if app.session_picker.loading && app.session_picker.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  fetching sessions…",
            Style::default().fg(t.muted),
        )));
    } else if indices.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no sessions match",
            Style::default().fg(t.muted),
        )));
    } else {
        let total = indices.len();
        let viewport = (area.height.saturating_sub(10)).max(6) as usize;
        let (start, end) = scroll_window(sel, total, viewport);
        if start > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} above", start),
                Style::default().fg(t.muted),
            )));
        }
        for rank in start..end {
            let Some(&idx) = indices.get(rank) else {
                continue;
            };
            let focused = rank == sel;
            let s = &app.session_picker.sessions[idx];
            let id = s.get("session_id").and_then(|v| v.as_str()).unwrap_or("?");
            let model = s.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let turns = s.get("turn_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let created = s.get("created_at").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let latest = s
                .get("is_latest")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mark = if latest { "●" } else { " " };
            let mut spans = vec![
                Span::styled(format!(" {mark} "), Style::default().fg(t.accent)),
                Span::styled(fmt_time(created), Style::default().fg(t.dim)),
                Span::raw("  "),
                Span::styled(format!("{turns:>3}t"), Style::default().fg(t.muted)),
                Span::raw("  "),
                Span::styled(clip(model, 22), Style::default().fg(t.text)),
                Span::raw("  "),
                Span::styled(clip(id, 20), Style::default().fg(t.muted)),
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
        if end < total {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} below", total - end),
                Style::default().fg(t.muted),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  filter · j/k move · ↵ resume · Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Sessions ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

fn draw_account(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(60, 50, f.area());
    f.render_widget(Clear, area);
    let s = &app.account.status;

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    if s.logged_in {
        lines.push(Line::from(vec![Span::styled(
            "  ● logged in",
            Style::default().fg(t.ok).add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  user:    ", Style::default().fg(t.muted)),
            Span::styled(s.user.clone(), Style::default().fg(t.text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  org:     ", Style::default().fg(t.muted)),
            Span::styled(s.org.clone(), Style::default().fg(t.text)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  project: ", Style::default().fg(t.muted)),
            Span::styled(s.project.clone(), Style::default().fg(t.text)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "  ○ not logged in",
            Style::default().fg(t.muted),
        )));
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "  MARC27 platform tools need an account.",
            Style::default().fg(t.dim),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        if app.account.busy {
            "  working…"
        } else {
            "  [l] login   [o] logout   [r] refresh"
        },
        Style::default().fg(t.accent),
    )));
    lines.push(Line::from(Span::styled(
        "  login uses the device flow — approve in your browser",
        Style::default().fg(t.muted),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Account ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

// ── Tools window (bespoke) ────────────────────────────────────────
//
// Purpose-built tool catalog: header with counts, fuzzy filter, tools
// grouped by approval (auto vs needs-approval), name + description columns,
// scroll that follows the selection.

// ── Status window (bespoke, from live state) ──────────────────────

// ── Config window (bespoke file viewer) ───────────────────────────

// ── API-key window ────────────────────────────────────────────────

fn draw_apikey_window(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(64, 62, f.area());
    f.render_widget(Clear, area);

    let providers = crate::app::API_PROVIDERS;
    let idx = app.apikey_window.provider_idx.min(providers.len() - 1);
    let (provider_name, env_var) = providers[idx];

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));

    // Provider selector.
    let mut pspans: Vec<Span> = vec![Span::styled("  ", Style::default())];
    for (i, (name, _)) in providers.iter().enumerate() {
        if i > 0 {
            pspans.push(Span::raw("  "));
        }
        if i == idx {
            pspans.push(Span::styled(
                format!("[{name}]"),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            pspans.push(Span::styled(
                (*name).to_string(),
                Style::default().fg(t.muted),
            ));
        }
    }
    lines.push(Line::from(pspans));
    lines.push(Line::raw(""));

    // Current status for all providers.
    lines.push(Line::from(Span::styled(
        "  Current keys:",
        Style::default().fg(t.dim),
    )));
    for (env, has) in &app.apikey_window.status {
        let (mark, color) = if *has { ("✓", t.ok) } else { ("✗", t.err) };
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{mark} "), Style::default().fg(color)),
            Span::styled(
                env.clone(),
                Style::default().fg(if *has { t.text } else { t.muted }),
            ),
        ]));
    }
    lines.push(Line::raw(""));

    // Key input (masked).
    let masked: String = "•".repeat(app.apikey_window.key_input.len());
    let disp = if masked.is_empty() {
        format!(" paste your {env_var}…")
    } else {
        format!(" {masked}")
    };
    let color = if app.apikey_window.key_input.is_empty() {
        t.muted
    } else {
        t.text
    };
    lines.push(Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(disp, Style::default().fg(color)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ←/→ provider · type key · ↵ save · Esc close",
        Style::default().fg(t.muted),
    )));

    let title = format!(" API Keys — {provider_name} ");
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

fn draw_config_window(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(86, 86, f.area());
    f.render_widget(Clear, area);

    let nfiles = app.config_window.files.len().max(1);
    let active = app.config_window.active.min(nfiles - 1);
    let (label, body) = app
        .config_window
        .files
        .get(active)
        .cloned()
        .unwrap_or_default();

    // Header: file tabs.
    let mut header: Vec<Span> = Vec::new();
    for (i, (name, _)) in app.config_window.files.iter().enumerate() {
        if i > 0 {
            header.push(Span::raw("  "));
        }
        if i == active {
            header.push(Span::styled(
                format!("[{name}]"),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            header.push(Span::styled(name.clone(), Style::default().fg(t.muted)));
        }
    }

    let body_lines: Vec<Line> = body.lines().map(Line::raw).collect();
    let viewport = area.height.saturating_sub(4);
    let max_scroll = (body_lines.len() as u16).saturating_sub(viewport);
    app.config_window.max_scroll.set(max_scroll);
    let scroll = app.config_window.scroll.min(max_scroll);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(header));
    lines.push(Line::raw(""));
    lines.extend(body_lines);
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ←/→ file · j/k scroll · Esc close",
        Style::default().fg(t.muted),
    )));

    let title = format!(" Config — {label} ");
    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .scroll((scroll, 0))
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

fn draw_status_window(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(58, 60, f.area());
    f.render_widget(Clear, area);

    let kv = |k: &str, v: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {k:<14}",), Style::default().fg(t.muted)),
            Span::styled(v, Style::default().fg(t.text)),
        ])
    };
    let model = clean_model_name(&app.model);
    let mode = app.session_mode.clone();
    let goal = app.goal.clone().unwrap_or_else(|| "—".to_string());

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Runtime",
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    )));
    lines.push(kv(
        "model",
        if model.is_empty() {
            "—".into()
        } else {
            model
        },
    ));
    lines.push(kv("mode", mode));
    lines.push(kv("session", clip(&app.session_title, 36)));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Usage",
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    )));
    lines.push(kv("messages", format!("{}", app.message_count)));
    lines.push(kv("tools loaded", format!("{}", app.tool_count)));
    lines.push(kv("catalog", format!("{}", app.tool_catalog.len())));
    lines.push(kv(
        "tokens (turn)",
        format!(
            "~{}  ·  ~{:.1} tok/s (est)",
            app.tokens_received, app.tokens_per_sec
        ),
    ));
    lines.push(kv("session cost", format!("${:.4}", app.session_cost)));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Goal",
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(clip(&goal, 48), Style::default().fg(t.text)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Status ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

/// Mission Control home — the launch screen. A glanceable, honest dashboard
/// built from live App state only (see docs/design/PLATFORM_ARCHITECTURE.md §3):
/// where a field isn't reported yet, it says so rather than inventing a number.
fn draw_home(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(80, 80, f.area());
    f.render_widget(Clear, area);

    let section = |label: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
    };
    // A live tile row: status glyph · one-line real state · key hint.
    let row = |glyph: &str, body: String, hint: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("   {glyph} "), Style::default().fg(t.ok)),
            Span::styled(body, Style::default().fg(t.text)),
            Span::styled(format!("   {hint}"), Style::default().fg(t.muted)),
        ])
    };
    // An honest "not reported / not wired yet" line — never a fabricated number.
    let muted = |s: String| -> Line<'static> {
        Line::from(Span::styled(
            format!("     {s}"),
            Style::default().fg(t.muted),
        ))
    };

    let total = app.tool_catalog.len();
    let need_approval = app
        .tool_catalog
        .iter()
        .filter(|x| x.get("approval").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();
    let model = clean_model_name(&app.model);

    // WORKFLOWS — no live run list wired to the client yet (honest).
    let mut lines: Vec<Line> = vec![
        Line::raw(""),
        section("WORKFLOWS"),
        muted("no live run list wired yet — talk to the agent to start one".to_string()),
        Line::raw(""),
        // TOOLS — real counts; location honestly "not reported" (no data path yet).
        section("TOOLS"),
    ];
    if total == 0 {
        lines.push(muted("loading tool catalog…".to_string()));
    } else {
        lines.push(row(
            "▣",
            format!(
                "{total} tools · {need_approval} need approval · {} auto",
                total.saturating_sub(need_approval)
            ),
            "t open",
        ));
        lines.push(muted(
            "location (cloud/local/remote) not reported — pending tool tags".to_string(),
        ));
    }
    lines.extend([
        Line::raw(""),
        // NOTEBOOKS — live entirely outside the agent world today (honest).
        section("NOTEBOOKS"),
        muted("not wired in-app yet — will be agent-watched + editable".to_string()),
        Line::raw(""),
        // SYSTEMS — live App state only.
        section("SYSTEMS"),
        row(
            "●",
            format!(
                "model  {}",
                if model.is_empty() {
                    "—".to_string()
                } else {
                    model
                }
            ),
            "s status",
        ),
        row(
            "●",
            format!("session cost  ${:.4}", app.session_cost),
            "as of last checkpoint",
        ),
    ]);
    lines.push(match app.credits {
        Some(mc) => muted(format!("credits  {:.3}", mc as f64 / 1000.0)),
        None => muted("credits  not reported (unauthed / not fetched)".to_string()),
    });
    lines.extend([
        muted("compute · nodes · knowledge · ingestion — open via ⌘K".to_string()),
        Line::raw(""),
        Line::from(Span::styled(
            "  ⏎ talk to the agent    t tools    s systems    ⌘K commands    ? keys",
            Style::default().fg(t.muted),
        )),
    ]);

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " PRISM · materials research workspace ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

fn draw_tools_window(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(82, 84, f.area());
    f.render_widget(Clear, area);

    let indices = app.tools_window_filtered();
    let total = app.tool_catalog.len();
    let sel = app
        .tools_window
        .selected
        .min(indices.len().saturating_sub(1));
    let need_approval = app
        .tool_catalog
        .iter()
        .filter(|x| x.get("approval").and_then(|v| v.as_bool()).unwrap_or(false))
        .count();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} tools", total),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   · {} need approval", need_approval),
            Style::default().fg(t.warn),
        ),
        Span::styled(
            format!("   · {} auto", total.saturating_sub(need_approval)),
            Style::default().fg(t.ok),
        ),
    ]));
    let qdisp = if app.tools_window.query.is_empty() {
        "filter by name / description…".to_string()
    } else {
        app.tools_window.query.clone()
    };
    let qcolor = if app.tools_window.query.is_empty() {
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
            format!("    ({} shown)", indices.len()),
            Style::default().fg(t.muted),
        ),
    ]));
    lines.push(Line::raw(""));

    if indices.is_empty() {
        lines.push(Line::from(Span::styled(
            if total == 0 {
                "  loading tools…"
            } else {
                "  no tools match"
            },
            Style::default().fg(t.muted),
        )));
    } else {
        let viewport = (area.height.saturating_sub(10)).max(6) as usize;
        let (start, end) = scroll_window(sel, indices.len(), viewport);
        if start > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} above", start),
                Style::default().fg(t.muted),
            )));
        }
        let mut prev_approval: Option<bool> = None;
        for rank in start..end {
            let Some(&idx) = indices.get(rank) else {
                continue;
            };
            let tool = &app.tool_catalog[idx];
            let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let desc = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let approval = tool
                .get("approval")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Group header when the approval bucket changes.
            if prev_approval != Some(approval) {
                prev_approval = Some(approval);
                let (label, color) = if approval {
                    ("Needs approval", t.warn)
                } else {
                    ("Auto-approved", t.ok)
                };
                lines.push(Line::from(Span::styled(
                    format!(" {label}"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )));
            }
            let focused = rank == sel;
            let mut spans = vec![
                Span::styled(
                    if focused {
                        "▸ ".to_string()
                    } else {
                        "  ".to_string()
                    },
                    Style::default().fg(t.accent),
                ),
                Span::styled(clip(name, 28), Style::default().fg(t.text)),
                Span::raw("  "),
                Span::styled(clip(desc, 44), Style::default().fg(t.dim)),
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
        if end < indices.len() {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} below", indices.len() - end),
                Style::default().fg(t.muted),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  filter · j/k move · Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Tools ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

fn draw_model_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(78, 80, f.area());
    f.render_widget(Clear, area);

    let indices = app.model_filtered_indices();
    let sel = app
        .model_picker
        .selected
        .min(indices.len().saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    if !app.model_picker.current.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  current: ", Style::default().fg(t.muted)),
            Span::styled(
                app.model_picker.current.clone(),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    let total_catalog = app.model_picker.models.len();
    let qdisp = if app.model_picker.query.is_empty() {
        if app.model_picker.loading {
            "loading catalog…".to_string()
        } else {
            format!("recommended — type to search all {total_catalog} models…")
        }
    } else {
        app.model_picker.query.clone()
    };
    let qcolor = if app.model_picker.query.is_empty() {
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
            format!("    ({} shown)", indices.len()),
            Style::default().fg(t.muted),
        ),
    ]));
    lines.push(Line::raw(""));

    if app.model_picker.loading && app.model_picker.models.is_empty() {
        lines.push(Line::from(Span::styled(
            "  fetching models from the catalog…",
            Style::default().fg(t.muted),
        )));
    } else if indices.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no models match",
            Style::default().fg(t.muted),
        )));
    } else {
        // Provider-grouped, scroll-windowed list that follows the selection.
        let total = indices.len();
        let viewport = (area.height.saturating_sub(10)).max(6) as usize;
        let (start, end) = scroll_window(sel, total, viewport);
        if start > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} above", start),
                Style::default().fg(t.muted),
            )));
        }
        let mut prev_provider = String::new();
        for rank in start..end {
            let Some(&idx) = indices.get(rank) else {
                continue;
            };
            let m = &app.model_picker.models[idx];
            let id = model_field(m, "id");
            let label = model_field(m, "label");
            let provider = model_field(m, "provider");
            let free = m.get("free").and_then(|v| v.as_bool()).unwrap_or(false);
            let is_current = id == app.model_picker.current;
            // Provider section header when the provider changes.
            if provider != prev_provider {
                prev_provider = provider.clone();
                lines.push(Line::from(Span::styled(
                    format!(" {provider}"),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )));
            }
            let focused = rank == sel;
            let mark = if is_current { "●" } else { " " };
            let mut spans = vec![
                Span::styled(format!(" {mark} "), Style::default().fg(t.accent)),
                Span::styled(clip(&label, 46), Style::default().fg(t.text)),
                Span::raw("  "),
                Span::styled(clip(&id, 28), Style::default().fg(t.muted)),
            ];
            if free {
                spans.push(Span::styled(" free", Style::default().fg(t.ok)));
            }
            if focused {
                spans.push(Span::styled("  ◀", Style::default().fg(t.accent)));
            }
            let mut row = Line::from(spans);
            if focused {
                row = row.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(row);
        }
        if end < total {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} below", total - end),
                Style::default().fg(t.muted),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  filter · j/k move · ↵ switch · Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Switch model ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

fn draw_gpu_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(78, 80, f.area());
    f.render_widget(Clear, area);

    let total = app.gpu_picker.gpus.len();
    let sel = app.gpu_picker.selected.min(total.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();

    // Column header — widths must stay in sync with the row spans below.
    lines.push(Line::from(Span::styled(
        format!(
            "   {:<19}{:>7}  {:<10}{:<10}{:>11}",
            "GPU", "VRAM", "Region", "Provider", "$/hr"
        ),
        Style::default().fg(t.muted).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    if app.gpu_picker.loading && app.gpu_picker.gpus.is_empty() {
        lines.push(Line::from(Span::styled(
            "  fetching live GPU offers…",
            Style::default().fg(t.muted),
        )));
    } else if total == 0 {
        lines.push(Line::from(Span::styled(
            "  no GPU offers available",
            Style::default().fg(t.muted),
        )));
    } else {
        // Scroll-windowed list that follows the selection.
        let viewport = (area.height.saturating_sub(9)).max(6) as usize;
        let (start, end) = scroll_window(sel, total, viewport);
        if start > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} above", start),
                Style::default().fg(t.muted),
            )));
        }
        for rank in start..end {
            let g = &app.gpu_picker.gpus[rank];
            let gpu_type = model_field(g, "gpu_type");
            let region = model_field(g, "region");
            let provider = model_field(g, "provider");
            let vram = g.get("vram_gb").and_then(|v| v.as_u64()).unwrap_or(0);
            let price = g
                .get("price_per_hour_usd")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let available = g.get("available").and_then(|v| v.as_bool()).unwrap_or(true);
            let focused = rank == sel;
            // Unavailable rows render fully dimmed so live (purchasable)
            // offers stand out.
            let (fg, aux, price_fg) = if available {
                (t.text, t.dim, t.accent)
            } else {
                (t.muted, t.muted, t.muted)
            };
            let mark = if available { "●" } else { "○" };
            let mut spans = vec![
                Span::styled(format!(" {mark} "), Style::default().fg(aux)),
                Span::styled(
                    format!("{:<19}", clip(&gpu_type, 18)),
                    Style::default().fg(fg),
                ),
                Span::styled(
                    format!("{:>7}", format!("{vram} GB")),
                    Style::default().fg(aux),
                ),
                Span::styled(
                    format!("  {:<10}", clip(&region, 10)),
                    Style::default().fg(fg),
                ),
                Span::styled(
                    format!("{:<10}", clip(&provider, 10)),
                    Style::default().fg(aux),
                ),
                Span::styled(
                    format!("{:>11}", format!("${price:.2}/hr")),
                    Style::default().fg(price_fg).add_modifier(Modifier::BOLD),
                ),
            ];
            if !available {
                spans.push(Span::styled("  unavailable", Style::default().fg(t.muted)));
            }
            if focused {
                spans.push(Span::styled("  ◀", Style::default().fg(t.accent)));
            }
            let mut row = Line::from(spans);
            if focused {
                row = row.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(row);
        }
        if end < total {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} below", total - end),
                Style::default().fg(t.muted),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ · ↵ use · Esc",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Procure GPU compute ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

/// GPU name from a `profile.gpus[]` element, which may be a bare string
/// (`"A100-80GB"`) or an object (`{"name": ...}` / `{"model": ...}`).
fn node_gpu_name(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    v.get("name")
        .or_else(|| v.get("model"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// Short hardware summary from a node's free-form `profile` — e.g.
/// `"1× A100-80GB · 12 cores · 24 GB · aarch64"`. Every field is optional;
/// missing pieces are simply dropped (older/sparse profiles never panic).
fn node_hw_summary(profile: Option<&serde_json::Value>) -> String {
    let Some(profile) = profile.filter(|v| v.is_object()) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(gpus) = profile.get("gpus").and_then(|v| v.as_array())
        && !gpus.is_empty()
    {
        let name = node_gpu_name(&gpus[0]).unwrap_or_else(|| "GPU".to_string());
        parts.push(format!("{}× {}", gpus.len(), name));
    }
    if let Some(cores) = profile.get("cpu_cores").and_then(|v| v.as_u64()) {
        parts.push(format!("{cores} cores"));
    }
    if let Some(ram) = profile.get("ram_gb").and_then(|v| v.as_f64()) {
        parts.push(format!("{} GB", ram.round() as u64));
    }
    if let Some(arch) = profile
        .get("labels")
        .and_then(|l| l.get("arch"))
        .and_then(|a| a.as_str())
    {
        parts.push(arch.to_string());
    }
    parts.join(" · ")
}

/// Relative "last seen" for an offline node from an RFC 3339 timestamp.
/// Best-effort: an unparseable/missing timestamp renders a plain "offline".
fn fmt_last_seen(iso: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(t) => {
            let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds();
            if secs < 60 {
                "last seen just now".to_string()
            } else if secs < 3600 {
                format!("last seen {}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("last seen {}h ago", secs / 3600)
            } else if secs < 604_800 {
                format!("last seen {}d ago", secs / 86_400)
            } else {
                format!("last seen {}w ago", secs / 604_800)
            }
        }
        Err(_) => "offline".to_string(),
    }
}

fn draw_node_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = centered_rect(78, 80, f.area());
    f.render_widget(Clear, area);

    let total = app.node_picker.nodes.len();
    let sel = app.node_picker.selected.min(total.saturating_sub(1));
    let online = app
        .node_picker
        .nodes
        .iter()
        .filter(|n| model_field(n, "status") == "online")
        .count();

    let mut lines: Vec<Line> = Vec::new();

    // Column header — widths must stay in sync with the row spans below.
    lines.push(Line::from(Span::styled(
        format!(
            "   {:<20}{:<26}{:<9}{}",
            "Node", "Hardware", "Access", "Last seen"
        ),
        Style::default().fg(t.muted).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    if app.node_picker.loading && app.node_picker.nodes.is_empty() {
        lines.push(Line::from(Span::styled(
            "  fetching your nodes…",
            Style::default().fg(t.muted),
        )));
    } else if total == 0 {
        lines.push(Line::from(Span::styled(
            "  No nodes yet — \"Node up\" in the palette connects this machine",
            Style::default().fg(t.muted),
        )));
    } else {
        // Scroll-windowed list that follows the selection.
        let viewport = (area.height.saturating_sub(9)).max(6) as usize;
        let (start, end) = scroll_window(sel, total, viewport);
        if start > 0 {
            lines.push(Line::from(Span::styled(
                format!("  ↑ {} above", start),
                Style::default().fg(t.muted),
            )));
        }
        for rank in start..end {
            let n = &app.node_picker.nodes[rank];
            let name = model_field(n, "name");
            let status = model_field(n, "status");
            let visibility = model_field(n, "visibility");
            let hw = node_hw_summary(n.get("profile"));
            let focused = rank == sel;

            // Status drives the glyph + colour; offline rows render dimmed so
            // live (reachable) nodes stand out.
            let (mark, mark_fg, name_fg) = match status.as_str() {
                "online" => ("●", t.ok, t.text),
                "provisioning" => ("●", t.warn, t.text),
                _ => ("○", t.muted, t.muted),
            };
            let seen = match status.as_str() {
                "online" => "online now".to_string(),
                "provisioning" => "provisioning…".to_string(),
                _ => n
                    .get("last_seen_at")
                    .and_then(|v| v.as_str())
                    .map(fmt_last_seen)
                    .unwrap_or_else(|| "offline".to_string()),
            };
            let aux = if status == "online" || status == "provisioning" {
                t.dim
            } else {
                t.muted
            };

            let mut spans = vec![
                Span::styled(format!(" {mark} "), Style::default().fg(mark_fg)),
                Span::styled(
                    format!("{:<20}", clip(&name, 19)),
                    Style::default().fg(name_fg),
                ),
                Span::styled(format!("{:<26}", clip(&hw, 25)), Style::default().fg(aux)),
                Span::styled(
                    format!("{:<9}", clip(&visibility, 8)),
                    Style::default().fg(aux),
                ),
                Span::styled(seen, Style::default().fg(aux)),
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
        if end < total {
            lines.push(Line::from(Span::styled(
                format!("  ↓ {} below", total - end),
                Style::default().fg(t.muted),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ · ⏎ detail · Esc",
        Style::default().fg(t.muted),
    )));

    let title = if total == 0 {
        " Nodes ".to_string()
    } else {
        format!(" Nodes — {online} of {total} online ")
    };
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
        // Two presentations:
        // - browse (empty query): SECTION HEADERS per category — organized,
        //   not a flat dump. Headers are decoration; selection indexes still
        //   refer to commands only, so key handling is untouched.
        // - filtering: flat ranked list with a per-row category tag.
        let browsing = app.palette.query.is_empty();

        enum Row<'a> {
            Header(&'a str),
            Cmd(usize, &'a command::Command),
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut last_cat = "";
        for (i, c) in cmds.iter().enumerate() {
            if browsing && c.category != last_cat {
                rows.push(Row::Header(c.category));
                last_cat = c.category;
            }
            rows.push(Row::Cmd(i, c));
        }

        // Scroll over DISPLAY rows (headers included) so the window math
        // matches what's on screen; keep the selected command visible.
        let sel_display = rows
            .iter()
            .position(|r| matches!(r, Row::Cmd(i, _) if *i == sel))
            .unwrap_or(0);
        let viewport = (area.height as usize).saturating_sub(6).max(3);
        let (start, end) = scroll_window(sel_display, rows.len(), viewport);

        let inner_w = area.width.saturating_sub(2) as usize;
        for row in rows.iter().take(end).skip(start) {
            match row {
                Row::Header(cat) => {
                    let label = format!("── {} ", cat.to_uppercase());
                    let fill = "─".repeat(inner_w.saturating_sub(label.chars().count() + 3));
                    lines.push(Line::from(Span::styled(
                        format!("  {label}{fill}"),
                        Style::default().fg(t.muted),
                    )));
                }
                Row::Cmd(i, c) => {
                    let focused = *i == sel;
                    // While filtering, a per-row tag keeps orientation; in
                    // browse mode the header already says it.
                    let tag = if browsing {
                        "    ".to_string()
                    } else {
                        format!("  {:>11} ▸ ", c.category.to_ascii_lowercase())
                    };
                    let fixed = tag.chars().count() + 24;
                    let desc_max = inner_w
                        .saturating_sub(fixed + c.keybind.chars().count() + 2)
                        .min(44);
                    let mut spans = vec![
                        Span::styled(tag, Style::default().fg(t.muted)),
                        Span::styled(
                            format!("{:<24}", c.title),
                            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(clip(c.description, desc_max), Style::default().fg(t.dim)),
                    ];
                    let used = fixed + c.description.chars().count().min(desc_max);
                    let pad = inner_w.saturating_sub(used + c.keybind.chars().count() + 1);
                    spans.push(Span::raw(" ".repeat(pad)));
                    spans.push(Span::styled(
                        c.keybind.to_string(),
                        Style::default().fg(t.muted),
                    ));
                    let mut line = Line::from(spans);
                    if focused {
                        line = line.style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                    lines.push(line);
                }
            }
        }
        if end < rows.len() {
            let hidden = rows[end..]
                .iter()
                .filter(|r| matches!(r, Row::Cmd(..)))
                .count();
            lines.push(Line::from(Span::styled(
                format!("  … {hidden} more — ↓ to scroll or type to filter"),
                Style::default().fg(t.muted),
            )));
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

// ── Link picker (`o`) ─────────────────────────────────────────────
//
// Lists http(s) URLs collected from the transcript (newest first) and
// asks for explicit confirmation before opening one in the browser.

fn draw_link_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let lp = &app.link_picker;

    // Confirm dialog: "do you want to go to this website?"
    if lp.confirm {
        let area = centered_rect(64, 28, f.area());
        f.render_widget(Clear, area);
        let url = lp.urls.get(lp.selected).cloned().unwrap_or_default();
        let lines = vec![
            Line::raw(""),
            Line::from(Span::styled(
                "  Do you want to go to this website?",
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    url,
                    Style::default()
                        .fg(t.user)
                        .add_modifier(Modifier::UNDERLINED),
                ),
            ]),
            Line::raw(""),
            Line::from(vec![
                Span::raw("  [y/↵] "),
                Span::styled("Open in browser", Style::default().fg(t.ok)),
                Span::raw("   [n/Esc] "),
                Span::styled("Cancel", Style::default().fg(t.muted)),
            ]),
        ];
        let para = Paragraph::new(lines)
            .style(Style::default().bg(t.overlay_bg))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(t.accent))
                    .title(Span::styled(
                        " Open link ",
                        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                    )),
            );
        f.render_widget(para, area);
        return;
    }

    // List of collected links, newest turn first. 1-9 jump straight to
    // the confirm dialog for that row.
    let area = centered_rect(72, 60, f.area());
    f.render_widget(Clear, area);
    let sel = lp.selected.min(lp.urls.len().saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "  {} link(s) in the transcript — newest first",
            lp.urls.len()
        ),
        Style::default().fg(t.muted),
    )));
    lines.push(Line::raw(""));

    let viewport = (area.height.saturating_sub(8)).max(4) as usize;
    let (start, end) = scroll_window(sel, lp.urls.len(), viewport);
    if start > 0 {
        lines.push(Line::from(Span::styled(
            format!("  ↑ {} above", start),
            Style::default().fg(t.muted),
        )));
    }
    for rank in start..end {
        let focused = rank == sel;
        let num = if rank < 9 {
            format!("{} ", rank + 1)
        } else {
            "  ".to_string()
        };
        let mut spans = vec![
            Span::styled(format!("  {num}"), Style::default().fg(t.accent)),
            Span::styled(
                clip(&lp.urls[rank], area.width.saturating_sub(10) as usize),
                Style::default()
                    .fg(t.user)
                    .add_modifier(Modifier::UNDERLINED),
            ),
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
    if end < lp.urls.len() {
        lines.push(Line::from(Span::styled(
            format!("  ↓ {} below", lp.urls.len() - end),
            Style::default().fg(t.muted),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  1-9 open · j/k move · ↵ open · Esc close",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Links ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

// ── Form pane (generic structured input) ──────────────────────────
//
// Renders `app.form` — the reusable form widget behind the deep-verb
// panes and backend-requested forms. Centered like the other modals;
// the focused field row is reversed (picker-row convention).

/// One rendered row per form field: label, value, optional dim note.
/// `active` controls whether the focused row is highlighted (a form
/// embedded in a pane whose focus is elsewhere passes `false`).
fn form_field_lines(form: &crate::form::Form, t: Theme, active: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    for (i, field) in form.fields.iter().enumerate() {
        let focused = active && i == form.focused;
        let prefix = if focused { "▸ " } else { "  " };
        let value_spans: Vec<Span> = match &field.kind {
            crate::form::FieldKind::Text { value } => {
                let (disp, color) = if value.is_empty() {
                    ("type…".to_string(), t.muted)
                } else {
                    (value.clone(), t.text)
                };
                vec![Span::styled(disp, Style::default().fg(color))]
            }
            crate::form::FieldKind::Toggle { value } => {
                let (mark, color) = if *value {
                    ("[x] on", t.ok)
                } else {
                    ("[ ] off", t.muted)
                };
                vec![Span::styled(mark.to_string(), Style::default().fg(color))]
            }
            crate::form::FieldKind::Select { options, selected } => {
                let opt = options.get(*selected).cloned().unwrap_or_default();
                vec![Span::styled(
                    format!("‹ {opt} ›"),
                    Style::default().fg(t.text),
                )]
            }
            crate::form::FieldKind::Stepper { value, min, max } => vec![
                Span::styled(format!("‹ {value} ›"), Style::default().fg(t.text)),
                Span::styled(format!("  ({min}–{max})"), Style::default().fg(t.muted)),
            ],
        };

        let mut spans = vec![
            Span::styled(format!("  {prefix}"), Style::default().fg(t.accent)),
            Span::styled(
                format!("{:<18}", clip(&field.label, 18)),
                Style::default().fg(if focused { t.accent } else { t.text }),
            ),
        ];
        spans.extend(value_spans);
        if let Some(note) = &field.note {
            spans.push(Span::styled(
                format!("  {note}"),
                Style::default().fg(t.muted),
            ));
        }
        let mut row = Line::from(spans);
        if focused {
            row = row.style(Style::default().add_modifier(Modifier::REVERSED));
        }
        lines.push(row);
    }
    lines
}

fn draw_form_pane(f: &mut Frame, app: &App) {
    let t = app.theme();
    let Some(pane) = app.form.as_ref() else {
        return;
    };
    let form = &pane.form;
    // Size to content (fields + padding + footer + borders) instead of
    // a fixed percentage — forms are small; a mostly-empty modal reads
    // as broken. Clamped to the terminal height.
    let full = f.area();
    let height = (form.fields.len() as u16 + 5).min(full.height.saturating_sub(2));
    let width = (full.width * 64 / 100).clamp(40.min(full.width), full.width);
    let x = full.x + (full.width.saturating_sub(width)) / 2;
    let y = full.y + (full.height.saturating_sub(height)) / 2;
    let area = Rect::new(x, y, width, height);
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));
    lines.extend(form_field_lines(form, t, true));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  ↑↓ move · Space/←→ adjust · ↵ {} · Esc",
            form.submit_label
        ),
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    format!(" {} ", form.title),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

// ── Knowledge pane (Search | Ingest) ──────────────────────────────
//
// One flow for the knowledge verbs: a Search tab (query + scope
// toggles) and an Ingest tab (file browser → optional metadata).

fn draw_knowledge_pane(f: &mut Frame, app: &App) {
    use crate::knowledge::{IngestPhase, KnowledgeTab};

    let t = app.theme();
    let pane = &app.knowledge;
    let area = centered_rect(72, 70, f.area());
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();

    // Mode tab bar (config-window convention).
    let mut tab_spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, (tab, label)) in [
        (KnowledgeTab::Search, "Search"),
        (KnowledgeTab::Ingest, "Ingest"),
    ]
    .iter()
    .enumerate()
    {
        if i > 0 {
            tab_spans.push(Span::raw("  "));
        }
        if *tab == pane.active_tab() {
            tab_spans.push(Span::styled(
                format!("[{label}]"),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(
                (*label).to_string(),
                Style::default().fg(t.muted),
            ));
        }
    }
    lines.push(Line::from(tab_spans));
    lines.push(Line::raw(""));

    let footer = match (pane.active_tab(), pane.phase) {
        (KnowledgeTab::Search, _) => {
            lines.extend(form_field_lines(&pane.search_form, t, true));
            "  Tab mode · ↑↓ move · Space toggle · ↵ search · Esc close"
        }
        (KnowledgeTab::Ingest, IngestPhase::Browse) => {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    crate::app::tilde_path(&pane.browser.cwd),
                    Style::default().fg(t.dim),
                ),
                Span::styled(
                    format!("   (.{})", crate::knowledge::INGEST_EXTENSIONS.join(" .")),
                    Style::default().fg(t.muted),
                ),
            ]));
            lines.push(Line::raw(""));
            let total = pane.browser.entries.len();
            if total == 0 {
                lines.push(Line::from(Span::styled(
                    "  (no ingestable files here)",
                    Style::default().fg(t.muted),
                )));
            } else {
                let sel = pane.browser.selected.min(total - 1);
                let viewport = (area.height.saturating_sub(9)).max(4) as usize;
                let (start, end) = scroll_window(sel, total, viewport);
                if start > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("  ↑ {} above", start),
                        Style::default().fg(t.muted),
                    )));
                }
                for rank in start..end {
                    let e = &pane.browser.entries[rank];
                    let focused = rank == sel;
                    let prefix = if focused { "▸ " } else { "  " };
                    let (name, color) = if e.is_dir {
                        (format!("{}/", e.name), t.accent)
                    } else {
                        (e.name.clone(), t.text)
                    };
                    let mut spans = vec![
                        Span::styled(format!("  {prefix}"), Style::default().fg(t.accent)),
                        Span::styled(
                            clip(&name, area.width.saturating_sub(10) as usize),
                            Style::default().fg(color),
                        ),
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
                if end < total {
                    lines.push(Line::from(Span::styled(
                        format!("  ↓ {} below", total - end),
                        Style::default().fg(t.muted),
                    )));
                }
            }
            "  Tab mode · ↑↓ move · ↵ open/pick · ←/Bksp up · Esc close"
        }
        (KnowledgeTab::Ingest, IngestPhase::Meta) => {
            let file = pane
                .ingest_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled("  file  ", Style::default().fg(t.muted)),
                Span::styled(
                    clip(&file, area.width.saturating_sub(10) as usize),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::raw(""));
            lines.extend(form_field_lines(&pane.meta_form, t, true));
            "  ↑↓ move · ↵ ingest · Esc back to files"
        }
    };

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Knowledge ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

// ── Notebook pane ─────────────────────────────────────────────────
//
// The in-app Python notebook: a scrollable cell history (In[n]/Out[n],
// stderr + errors in red, plots shown as saved file paths) over a
// multi-line code editor. The kernel is shared with the agent, so cells
// the agent runs appear here too.
fn draw_notebook_pane(f: &mut Frame, app: &App) {
    let t = app.theme();
    let pane = &app.notebook;
    let area = centered_rect(82, 82, f.area());
    f.render_widget(Clear, area);

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.accent))
        .style(Style::default().bg(t.overlay_bg))
        .title(Span::styled(
            " Notebook ",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    // header (status) · history (fill) · editor (7) · footer (1)
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(inner);

    // Header: kernel status + running indicator.
    let status = if pane.running {
        format!("{}  · running…", pane.kernel_status)
    } else {
        pane.kernel_status.clone()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {status}"),
            Style::default().fg(t.muted),
        ))),
        rows[0],
    );

    // Cell history.
    let mut lines: Vec<Line> = Vec::new();
    if pane.cells.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No cells yet — write Python below and press Ctrl-R.",
            Style::default().fg(t.muted),
        )));
    }
    let code_width = rows[1].width.saturating_sub(8) as usize;
    for cell in &pane.cells {
        let marker = format!("In[{}]", cell.execution_count);
        let origin = if cell.origin == "agent" {
            Span::styled("  (agent)", Style::default().fg(t.accent))
        } else {
            Span::raw("")
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {marker} "),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            origin,
        ]));
        for code_line in cell.code.lines() {
            lines.push(Line::from(Span::styled(
                format!("   {}", clip(code_line, code_width)),
                Style::default().fg(t.text),
            )));
        }
        for out_line in cell.stdout.lines() {
            lines.push(Line::from(Span::styled(
                format!("   {}", clip(out_line, code_width)),
                Style::default().fg(t.dim),
            )));
        }
        if let Some(result) = &cell.result {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" Out[{}] ", cell.execution_count),
                    Style::default().fg(t.muted),
                ),
                Span::styled(clip(result, code_width), Style::default().fg(t.text)),
            ]));
        }
        for path in &cell.image_paths {
            lines.push(Line::from(Span::styled(
                format!("   [plot saved: {}]", clip(path, code_width)),
                Style::default().fg(t.accent),
            )));
        }
        for err_line in cell.stderr.lines() {
            lines.push(Line::from(Span::styled(
                format!("   {}", clip(err_line, code_width)),
                Style::default().fg(Color::Red),
            )));
        }
        if let Some(error) = &cell.error {
            for err_line in error.lines() {
                lines.push(Line::from(Span::styled(
                    format!("   {}", clip(err_line, code_width)),
                    Style::default().fg(Color::Red),
                )));
            }
        }
        lines.push(Line::raw(""));
    }
    let history = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((pane.scroll, 0))
        .style(Style::default().bg(t.overlay_bg));
    f.render_widget(history, rows[1]);

    // Code editor.
    let editor_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.divider))
        .title(Span::styled(" Cell ", Style::default().fg(t.muted)));
    let editor_inner = editor_block.inner(rows[2]);
    f.render_widget(editor_block, rows[2]);
    f.render_widget(&pane.input, editor_inner);

    // Footer hints.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Ctrl-R run · Enter newline · PgUp/PgDn scroll · Esc close",
            Style::default().fg(t.muted),
        ))),
        rows[3],
    );
}

fn draw_approval_popup(f: &mut Frame, app: &App) {
    let t = app.theme();
    let (tool, message) = app.approval_pending.as_ref().unwrap();

    // When the prompt carries code (notebook_exec — arbitrary Python on the
    // kernel SHARED with the human), the popup must show the WHOLE cell so
    // the human can read exactly what they approve. Wrapped, bounded height,
    // scrollable with ↑/↓ when it doesn't fit.
    if let Some(code) = &app.approval_code {
        let area = centered_rect(72, 70, f.area());
        f.render_widget(Clear, area);

        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.approval));
        let inner = outer.inner(area);
        f.render_widget(outer, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // banner + tool/message
                Constraint::Min(3),    // code block
                Constraint::Length(1), // key hints
            ])
            .split(inner);

        f.render_widget(
            Paragraph::new(vec![
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
                        tool.clone(),
                        Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  —  "),
                    Span::styled(message.clone(), Style::default().fg(t.text)),
                ]),
            ]),
            rows[0],
        );

        let code_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.divider))
            .title(Span::styled(
                " Cell code — review before answering ",
                Style::default().fg(t.muted),
            ));
        let code_area = code_block.inner(rows[1]);
        f.render_widget(code_block, rows[1]);

        // Manual wrap so the scroll bound is exact (Paragraph::wrap gives no
        // rendered-line count on stable ratatui).
        let wrapped = wrap_plain(code, code_area.width.max(1) as usize);
        let visible = code_area.height as usize;
        let max_scroll = wrapped.len().saturating_sub(visible) as u16;
        app.approval_max_scroll.set(max_scroll);
        let scroll = app.approval_scroll.min(max_scroll) as usize;
        let lines: Vec<Line> = wrapped
            .iter()
            .skip(scroll)
            .take(visible)
            .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(t.text))))
            .collect();
        f.render_widget(Paragraph::new(lines), code_area);

        let mut hints = vec![
            Span::raw("  [y] "),
            Span::styled("Allow", Style::default().fg(t.ok)),
            Span::raw("   [a] "),
            Span::styled("Allow all", Style::default().fg(t.warn)),
            Span::raw("   [n] "),
            Span::styled("Deny", Style::default().fg(t.err)),
        ];
        if max_scroll > 0 {
            hints.push(Span::styled(
                format!(
                    "   ↑/↓ scroll code ({}/{})",
                    scroll + visible.min(wrapped.len()),
                    wrapped.len()
                ),
                Style::default().fg(t.muted),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(hints)), rows[2]);
        return;
    }

    let area = centered_rect(60, 20, f.area());
    f.render_widget(Clear, area);

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

/// Hard-wrap plain text to `width` characters per line (no word splitting
/// smarts — code must never be reflowed in a way that hides content). Every
/// input line yields at least one output line, so nothing is dropped.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut count = 0;
        for ch in line.chars() {
            current.push(ch);
            count += 1;
            if count == width {
                out.push(std::mem::take(&mut current));
                count = 0;
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
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
