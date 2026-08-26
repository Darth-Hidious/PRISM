//! Ratatui rendering — pure view function.
//!
//! All colors come from the active [`crate::theme::Theme`] (read via
//! `app.theme()`), never hardcoded — so the whole UI recolors uniformly
//! when the theme changes.

use crate::app::{
    App, Focus, LineKind, Modal, ObjectKind, ObjectStatus, Role, WorkspaceTab, evidence_token,
    first_line,
};
use crate::artifact::{ArtifactPromotion, ArtifactStoreState, format_bytes};
use crate::command;
use crate::gh;
use crate::hit_map::HitTarget;
use crate::keymap;
use crate::markdown;
use crate::structures::{StructuresStoreState, UNKNOWN};
use crate::theme::Theme;
use crate::toast::ToastKind;
use prism_provenance::EvidenceClass;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, Wrap};
use unicode_truncate::UnicodeTruncateStr;
use unicode_width::UnicodeWidthStr;

/// The frame split every widget agrees on.
///
/// One source of truth for who owns which cells. `draw` renders into it and
/// [`overlay_bounds`] reads it, so an overlay can never disagree with the
/// widget whose rows it would otherwise land on.
struct FrameLayout {
    /// Left content column (the whole frame when the sidebar is hidden).
    content: Rect,
    header: Rect,
    transcript: Rect,
    prompt: Rect,
    footer: Rect,
    /// Right-hand Workspace panel, or `None` on a narrow terminal.
    sidebar: Option<Rect>,
}

fn frame_layout(area: Rect) -> FrameLayout {
    // Columns: left content column + right Workspace panel (opencode-style).
    // Below the threshold the sidebar is hidden entirely — a clipped sidebar
    // is worse than none, and the content column needs the room.
    const SIDEBAR_MIN_WIDTH: u16 = 100;
    let sidebar_w = if area.width >= SIDEBAR_MIN_WIDTH {
        (area.width / 3).clamp(24, 42)
    } else {
        0
    };
    let cols = if sidebar_w > 0 {
        Layout::default()
            .direction(Direction::Horizontal)
            // One column of daylight between the content boxes and the
            // sidebar divider — without it the prompt box border touches
            // the divider and reads as a double-drawn rule.
            .spacing(1)
            .constraints([Constraint::Min(0), Constraint::Length(sidebar_w)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0)])
            .split(area)
    };

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

    FrameLayout {
        content: cols[0],
        header: chunks[0],
        transcript: chunks[1],
        prompt: chunks[2],
        footer: chunks[3],
        sidebar: (sidebar_w > 0).then(|| cols[1]),
    }
}

/// The only region an overlay may claim: the content column between the
/// header bar and the prompt box — exactly the rows the transcript owns.
///
/// Every overlay is drawn *after* the header, prompt, footer and sidebar, and
/// each one starts with `Clear`. An overlay centred on the whole frame
/// therefore wipes cells another widget already painted: at 100x30 the Tools
/// pane left the prompt box as the fragments `┌ Prompt` / `│ Type a` on the
/// left edge and cut the sidebar's border out of every row it covered.
/// Centring inside these bounds instead cannot reach either.
fn overlay_bounds(area: Rect) -> Rect {
    let l = frame_layout(area);
    Rect::new(
        l.content.x,
        l.transcript.y,
        l.content.width,
        l.transcript.height,
    )
}

/// The share of the screen an overlay asks for, cropped to the region
/// overlays may own and centred in it.
///
/// Cropping rather than re-taking the percentage inside the bounds keeps each
/// overlay as large as it has always been wherever there is room — the
/// notebook approval popup has to show every line of the cell it is asking
/// you to run, and 72% of the content column is not 72% of the screen.
fn overlay_area(f: &Frame, percent_x: u16, percent_y: u16) -> Rect {
    let bounds = overlay_bounds(f.area());
    let want = centered_rect(percent_x, percent_y, f.area());
    let width = want.width.min(bounds.width);
    let height = want.height.min(bounds.height);
    Rect::new(
        bounds.x + (bounds.width - width) / 2,
        bounds.y + (bounds.height - height) / 2,
        width,
        height,
    )
}

pub fn draw(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = f.area();

    // Last frame's regions describe a layout that no longer exists — scroll
    // offset, terminal size and which overlay is open all move things — so the
    // map is emptied here and refilled as this frame paints. Overlays draw
    // last and are looked up first, so a popup answers for the cells it covers.
    app.hit_map.borrow_mut().clear();

    // Paint the whole screen `background` (opencode paints its background).
    f.render_widget(
        Block::default().style(Style::default().bg(t.overlay_bg)),
        area,
    );

    let layout = frame_layout(area);

    draw_header(f, app, layout.header);
    draw_chat(f, app, layout.transcript);
    draw_prompt(f, app, layout.prompt);
    draw_footer(f, app, layout.footer);
    if let Some(sidebar) = layout.sidebar {
        draw_workspace(f, app, sidebar);
    }

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
        // The home panel lives in the content column so it shares an origin
        // and a width with the prompt box and footer stacked around it —
        // never over the workspace sidebar column.
        draw_home(f, app, overlay_bounds(area));
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
    // The tool COUNT is deliberately not in the always-visible header. A
    // headline "170 tools" is an invitation to be asked about all 170, and
    // the inventory is implementation detail rather than a capability a
    // researcher can act on. It stays exactly one keypress away (`t`, the
    // tools pane) and in the usage stats — surfaces you reach by asking.
    // Nothing is hidden; it is simply not advertised.
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
// Stack at the bottom-center of the TRANSCRIPT, not of the frame.
//
// The frame was the bug. Measuring from `f.area()` and backing off a fixed
// four rows put the toast ON the prompt box at the shipped 100x30: the
// committed `toast_visible_100x30` snapshot read
// `│ Type a message... (Ente▌ theme: forest` — the placeholder cut mid-word,
// the prompt's right border gone, and the sidebar divider gone with it. The
// magic `4` was standing in for "however tall the prompt and status bar
// happen to be", which is exactly the number `frame_layout` already knows.
//
// `overlay_bounds` is that answer, and it is the same region every other
// overlay was moved onto — a toast is not a special case, it is the last
// widget that had its own idea of where the screen ends.

fn draw_toasts(f: &mut Frame, app: &App) {
    let t = app.theme();
    // Defensive: also hide any that expired between ticks.
    let live: Vec<&crate::toast::Toast> = app.toasts.iter().filter(|x| !x.is_expired()).collect();
    if live.is_empty() {
        return;
    }
    let bounds = overlay_bounds(f.area());
    let count = live.len().min(5) as u16;
    // Narrow terminals hide the sidebar, so `bounds` can be narrower than the
    // toast's natural width; clamp rather than overflow the content column.
    let width = 50.min(bounds.width);
    let x = bounds.x + bounds.width.saturating_sub(width) / 2;
    // Bottom of the transcript. No fixed offset: `bounds` already ends where
    // the prompt begins.
    let y = bounds.y + bounds.height.saturating_sub(count);
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
                    clip(&toast.message, (width as usize).saturating_sub(3)),
                    Style::default().fg(t.text),
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rect);
}

/// Rows reserved for one inline figure in the transcript.
const FIGURE_ROWS: u16 = 12;

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let t = app.theme();
    let mut lines: Vec<Line> = Vec::new();
    let mut thinking_shown = false;
    // Index in `lines` of the newest `❯ You` header, for the scroll anchor.
    let mut last_user_line: Option<usize> = None;
    // Figures to paint over reserved blank rows, as (index in `lines`, path).
    let mut inline_figures: Vec<(usize, String)> = Vec::new();
    // Where each message starts, so a click or a selection can say WHICH
    // message it landed in — the thing that makes "explain this line" possible.
    let mut message_lines: Vec<(usize, usize)> = Vec::new();

    for (idx, msg) in app.messages.iter().enumerate() {
        message_lines.push((lines.len(), idx));
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
                // Recorded BEFORE the header is pushed, so the anchor lands on
                // the header row itself rather than the first body row.
                last_user_line = Some(lines.len());
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
                    // Tool RESULTS are content the user reads, not chrome:
                    // they carry the numbers and citations the whole product
                    // exists to produce, so they get `text` like any other
                    // body copy. `dim` made the most substantive thing on
                    // screen the hardest to read.
                    LineKind::ToolResult { .. } => ("✓", t.ok, Style::default().fg(t.text)),
                    // The "⚙ Running x" progress line IS chrome — it stays
                    // secondary so the eye goes to the result, not the noise.
                    _ => ("⚙", t.warn, Style::default().fg(t.dim)),
                };
                let evidence_class = match kind {
                    LineKind::ToolResult { evidence_class, .. } => Some(*evidence_class),
                    LineKind::Error(_) => Some(EvidenceClass::Indeterminate),
                    _ => None,
                };
                // A finished RESULT is prose the reader studies, so its body
                // goes through the SAME markdown renderer as PRISM's own
                // replies. It never did: `markdown_lines` was called from
                // exactly one place — the assistant branch a few lines above —
                // so a table a tool emitted arrived as raw `|` pipes and `$x^2$`
                // as literal dollar signs, while identical content written by
                // PRISM rendered as a bordered, aligned table. Same bytes, two
                // different qualities of display, decided by who said it.
                //
                // Progress and error lines are NOT routed through it: they are
                // chrome, they carry their own colour (dim / red), and markdown
                // styling would override the very distinction that keeps the
                // eye on the result instead of the noise.
                let render_body_as_markdown =
                    matches!(kind, LineKind::ToolResult { success: true, .. });
                let mut body = msg.text.lines();
                if let Some(line_text) = body.next() {
                    let mut spans = vec![
                        Span::raw("  "),
                        Span::styled(format!("{glyph} "), Style::default().fg(gcolor)),
                    ];
                    if let Some(evidence_class) = evidence_class {
                        let token = evidence_token(evidence_class);
                        spans.push(Span::styled(
                            token.clone(),
                            Style::default()
                                .fg(evidence_color(evidence_class, t))
                                .add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::styled(
                            line_text
                                .strip_prefix(&token)
                                .unwrap_or(line_text)
                                .to_string(),
                            style,
                        ));
                    } else {
                        spans.push(Span::styled(line_text.to_string(), style));
                    }
                    lines.push(Line::from(spans));
                }
                let rest: Vec<&str> = body.collect();
                if render_body_as_markdown && !rest.join("").trim().is_empty() {
                    // Width is reduced by the 4-column indent so a table sizes
                    // its columns to the room it will actually occupy.
                    for md in
                        markdown::markdown_lines(&rest.join("\n"), t, area.width.saturating_sub(4))
                    {
                        let mut spans = vec![Span::raw("    ")];
                        spans.extend(md.spans);
                        lines.push(Line::from(spans));
                    }
                } else {
                    for line_text in rest {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(line_text.to_string(), style),
                        ]));
                    }
                }
                // Reserve room for each figure and remember where it goes. The
                // rows are blank on purpose: the transcript is one wrapped
                // `Paragraph`, so a picture cannot be a `Line`. It is painted
                // over these rows afterwards, once the scroll offset is known.
                if let LineKind::ToolResult { image_paths, .. } = kind {
                    for path in image_paths {
                        inline_figures.push((lines.len(), path.clone()));
                        for _ in 0..FIGURE_ROWS {
                            lines.push(Line::raw(""));
                        }
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
    // Where the newest user turn sits, in WRAPPED rows — the same unit
    // `Paragraph::scroll` counts in, measured with the same wrap settings.
    // Counting raw `Line`s here would drift the moment any message wrapped.
    // Computed before `lines` is moved into the paragraph below.
    // Wrapped-row offset of each reserved figure, measured the same way the
    // transcript is. Raw line indices would drift the moment anything above a
    // figure wrapped, painting the picture over someone else's text.
    //
    // Measured in ONE accumulating pass, not one pass per mark. Wrapping is
    // additive under `Wrap { trim: false }` — a prefix and its remainder
    // measure the same as the whole (asserted by `wrapping_is_additive`) — so
    // the chunks between marks can simply be summed. Measuring each mark's
    // full prefix separately re-wrapped the entire transcript once per figure,
    // every frame, over a transcript that is never trimmed.
    //
    // Rows accumulate in u32: `as u16` on a longer transcript truncates
    // modulo 65,536, which does not clamp the view to the end — it teleports
    // it into the middle. Saturating keeps the view at the last reachable row.
    //
    // Message starts join the same pass. Adding marks costs almost nothing:
    // the chunks are disjoint, so cloning them all sums to one clone of the
    // whole transcript however finely it is cut.
    let mut marks: Vec<usize> = inline_figures.iter().map(|(idx, _)| *idx).collect();
    marks.extend(message_lines.iter().map(|(line, _)| *line));
    let anchor_idx = if app.anchor_user_turn.get() {
        last_user_line
    } else {
        None
    };
    if let Some(idx) = anchor_idx {
        marks.push(idx);
    }
    for m in &mut marks {
        *m = (*m).min(lines.len());
    }
    marks.sort_unstable();
    marks.dedup();

    let measure = |chunk: &[Line<'_>]| -> u32 {
        Paragraph::new(chunk.to_vec())
            .wrap(Wrap { trim: false })
            .line_count(area.width) as u32
    };
    let mut rows_at: Vec<(usize, u32)> = Vec::with_capacity(marks.len());
    let mut acc: u32 = 0;
    let mut prev = 0usize;
    for idx in marks {
        if idx > prev {
            acc = acc.saturating_add(measure(&lines[prev..idx]));
            prev = idx;
        }
        rows_at.push((idx, acc));
    }
    let line_count = lines.len();
    let rows_for = |idx: usize| -> u16 {
        let idx = idx.min(line_count);
        rows_at
            .iter()
            .find(|(at, _)| *at == idx)
            .map(|(_, rows)| (*rows).min(u16::MAX as u32) as u16)
            .unwrap_or(0)
    };
    let figure_rows: Vec<(u16, String)> = inline_figures
        .iter()
        .map(|(idx, path)| (rows_for(*idx), path.clone()))
        .collect();
    let anchor_rows = anchor_idx.map(rows_for);
    // The tail is only measured when the pass above already ran; with no marks
    // the paragraph measures itself below without cloning anything.
    let tail_rows = (prev > 0).then(|| acc.saturating_add(measure(&lines[prev..])));

    let paragraph = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .wrap(Wrap { trim: false });
    let content_lines = tail_rows
        .unwrap_or_else(|| paragraph.line_count(area.width) as u32)
        .min(u16::MAX as u32) as u16;
    let max_scroll = content_lines.saturating_sub(viewport);
    app.view_max_scroll.set(max_scroll);
    let effective_scroll = if let Some(rows) = anchor_rows {
        // The reader's own turn goes to the top and the reply fills downward.
        // Clamped to `max_scroll` so a turn near the end of a short transcript
        // does not try to scroll past the final row.
        rows.min(max_scroll)
    } else if app.auto_scroll {
        max_scroll
    } else {
        crate::app::clamp_scroll(app.scroll_offset, content_lines, viewport)
    };

    // What was actually drawn, so a key handler taking over from auto-follow or
    // the anchor resumes from the row the reader is looking at.
    app.view_scroll.set(effective_scroll);

    // Which message occupies which rows on screen. A message owns every row
    // from its own first line down to the next message's, so pointing anywhere
    // inside a reply — not only at its first line — identifies that reply.
    {
        let mut map = app.hit_map.borrow_mut();
        for (n, (line, index)) in message_lines.iter().enumerate() {
            let start = rows_for(*line);
            let end = message_lines
                .get(n + 1)
                .map(|(next, _)| rows_for(*next))
                .unwrap_or(content_lines);
            // Clip to the visible window; a message scrolled off screen has no
            // cells and must not answer for anyone else's.
            let top = start.max(effective_scroll);
            let bottom = end.min(effective_scroll.saturating_add(area.height));
            if bottom <= top {
                continue;
            }
            map.push(
                Rect::new(
                    area.x,
                    area.y + (top - effective_scroll),
                    area.width,
                    bottom - top,
                ),
                HitTarget::TranscriptMessage { index: *index },
            );
        }
    }

    f.render_widget(paragraph.scroll((effective_scroll, 0)), area);

    // Paint figures over their reserved rows. A figure straddling either edge
    // draws the part that fits, the same way at the top as at the bottom —
    // skipping the top case left up to 11 reserved rows blank, so the picture
    // blinked out and popped back while scrolling past it.
    for (row, path) in &figure_rows {
        let (offset, hidden) = match row.checked_sub(effective_scroll) {
            Some(offset) => (offset, 0),
            // Top edge is above the viewport: how much of the figure is gone.
            None => (0, effective_scroll - row),
        };
        if offset >= area.height || hidden >= FIGURE_ROWS {
            continue;
        }
        let height = (FIGURE_ROWS - hidden).min(area.height - offset);
        let rect = Rect::new(
            area.x + 4,
            area.y + offset,
            area.width.saturating_sub(4),
            height,
        );
        if let Err(err) = app.image_view().draw(f, rect, path) {
            // Never a blank gap: reserved rows that could not be filled say why.
            f.render_widget(
                Paragraph::new(err.line()).style(Style::default().fg(Color::Red)),
                rect,
            );
        }
    }

    // Scrollbar whenever the transcript overflows, so scrolling is discoverable.
    //
    // `ScrollbarState::new` takes the number of SCROLLABLE POSITIONS, not the
    // content height. `position` ranges 0..=max_scroll, so passing
    // `content_lines` made the thumb top out at
    // `(content - viewport) / content` — never reaching the end of the track,
    // and stopping further short the taller the viewport was relative to the
    // transcript. It read as "the scrollbar stops in the middle and doesn't
    // scale as the chat grows". The two numbers must describe the same range.
    if max_scroll > 0 {
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut ratatui::widgets::ScrollbarState::new(max_scroll as usize)
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
// activity feed, and touched files are reconstructed from `app.messages`;
// artifact state arrives through the same typed backend event stream.

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
    evidence_class: EvidenceClass,
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
    let (tabs_line, tab_spans) = workspace_tabs_line(app, t, w);
    let tabs_line_index = lines.len();
    lines.push(tabs_line);
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

    // Which entry each line belongs to, filled as the tab's rows are built.
    // Recorded at the source rather than reconstructed afterwards: the
    // builders skip, clip and expand entries, so counting lines from outside
    // would drift the moment any of them changed.
    let mut rows: PanelRows = Vec::new();
    match app.workspace_tab {
        WorkspaceTab::Tools => build_tools_lines(app, t, &mut lines, &mut rows, w),
        WorkspaceTab::Activity => build_activity_lines(app, t, &mut lines, &mut rows, w),
        WorkspaceTab::Files => build_files_lines(app, t, &mut lines, &mut rows, w),
        WorkspaceTab::Objects => build_objects_lines(app, t, &mut lines, &mut rows, w),
        WorkspaceTab::Structures => {
            let available = usize::from(inner.height).saturating_sub(lines.len());
            build_structures_lines(app, t, &mut lines, &mut rows, w, available);
        }
        WorkspaceTab::Artifacts => {
            let available = usize::from(inner.height).saturating_sub(lines.len());
            build_artifact_lines(app, t, &mut lines, &mut rows, w, available);
        }
    }

    // Record what landed where, before `lines` is moved into the paragraph.
    //
    // Line index is not screen row: the panel wraps, so a long entry pushes
    // everything under it down. Measured with the same wrap settings the
    // paragraph uses, accumulating once through the marks in order — the same
    // additive property `draw_chat` relies on.
    {
        let mut marks: Vec<usize> = rows.iter().map(|(line, _)| *line).collect();
        marks.push(tabs_line_index);
        marks.sort_unstable();
        marks.dedup();
        let mut row_of: Vec<(usize, u16)> = Vec::with_capacity(marks.len());
        let mut acc: u16 = 0;
        let mut prev = 0usize;
        for mark in marks {
            let mark = mark.min(lines.len());
            if mark > prev {
                acc = acc.saturating_add(
                    Paragraph::new(lines[prev..mark].to_vec())
                        .wrap(Wrap { trim: false })
                        .line_count(inner.width) as u16,
                );
                prev = mark;
            }
            row_of.push((mark, acc));
        }
        let screen_row = |line: usize| -> Option<u16> {
            let line = line.min(lines.len());
            let offset = row_of.iter().find(|(at, _)| *at == line)?.1;
            (offset < inner.height).then_some(inner.y + offset)
        };

        let mut map = app.hit_map.borrow_mut();
        if let Some(row) = screen_row(tabs_line_index) {
            for (tab, col, width) in tab_spans {
                map.push(
                    Rect::new(inner.x + col, row, width, 1),
                    HitTarget::WorkspaceTab(tab),
                );
            }
        }
        // Each entry owns every row from its own first line up to the next
        // entry's, so clicking a tool's finding line selects that tool rather
        // than nothing.
        for (n, (line, entry)) in rows.iter().enumerate() {
            let Some(top) = screen_row(*line) else {
                continue;
            };
            let next = rows
                .get(n + 1)
                .and_then(|(next_line, _)| screen_row(*next_line))
                .unwrap_or(inner.y + inner.height);
            let height = next.saturating_sub(top).max(1);
            map.push(
                Rect::new(inner.x, top, inner.width, height),
                HitTarget::WorkspaceRow {
                    tab: app.workspace_tab,
                    index: *entry,
                },
            );
        }
    }

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.panel))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);
}

/// Which entry each built panel line belongs to: `(line index, entry index)`,
/// recorded at the moment the line is pushed. One entry may own several lines
/// (a finding, an expanded detail), and it owns every line up to the next
/// entry's first.
type PanelRows = Vec<(usize, usize)>;

/// The tab strip, and where each label landed.
///
/// The columns come back with the line because they are only knowable while
/// it is being built — the ladder below drops labels, shortens them and elides
/// whole tabs, so nothing downstream can work out from the finished text which
/// cells belong to which tab. Offsets are relative to the start of the line.
type TabSpans = Vec<(WorkspaceTab, u16, u16)>;

fn workspace_tabs_line(app: &App, t: Theme, w: usize) -> (Line<'static>, TabSpans) {
    // Full labels exceed a narrow sidebar. The sidebar is narrower on a
    // small terminal, and the paragraph wraps — "Objects" dropped onto its own
    // line, ate a row of the panel and shoved every entry down (caught at
    // 40x12). Abbreviate instead of wrapping: a cramped strip is legible, a
    // wrapped one silently costs a row of content. With six tabs the full
    // set can never fit the 42-column sidebar ceiling, so the degradation
    // ladder is three-letter labels, then two-letter initials, then whole
    // tabs elided behind a `‹`/`›` marker.
    const SHORT: [(WorkspaceTab, &str); 6] = [
        (WorkspaceTab::Activity, "Act"),
        (WorkspaceTab::Tools, "Too"),
        (WorkspaceTab::Files, "Fil"),
        (WorkspaceTab::Objects, "Obj"),
        (WorkspaceTab::Structures, "Str"),
        (WorkspaceTab::Artifacts, "Art"),
    ];
    const MIN: [(WorkspaceTab, &str); 6] = [
        (WorkspaceTab::Activity, "Ac"),
        (WorkspaceTab::Tools, "To"),
        (WorkspaceTab::Files, "Fi"),
        (WorkspaceTab::Objects, "Ob"),
        (WorkspaceTab::Structures, "St"),
        (WorkspaceTab::Artifacts, "Ar"),
    ];
    // Rendered width: a leading space, a space between each, and the active
    // label gains two brackets.
    let width_of = |set: &[(WorkspaceTab, &str)]| -> usize {
        1 + set.iter().map(|(_, label)| label.width()).sum::<usize>() + (set.len() - 1) + 2
    };
    let tabs: &[(WorkspaceTab, &str)] = if width_of(&SHORT) <= w { &SHORT } else { &MIN };

    // Bottom rung. When even the initials overflow, emitting the whole set
    // anyway does not shorten it — the paragraph WRAPS, so the trailing tabs
    // land on the next row and steal a line of panel content, which is the
    // failure this ladder exists to prevent. Elide whole tabs instead, and
    // say so: `‹` and `›` mark tabs dropped off that side, so a missing tab
    // reads as elided rather than as absent. Cutting the labels further is
    // not an option — "St" shortened again is "S", which reads as a
    // different tab, and nothing on screen would admit the cut.
    let active = tabs
        .iter()
        .position(|(tab, _)| *tab == app.workspace_tab)
        .unwrap_or(0);
    // Rendered width of the window `lo..hi` including the markers it needs.
    let window_width = |lo: usize, hi: usize| -> usize {
        let labels: usize = tabs[lo..hi].iter().map(|(_, l)| l.width()).sum();
        let markers = 2 * usize::from(lo > 0) + 2 * usize::from(hi < tabs.len());
        1 + labels + (hi - lo - 1) + 2 + markers
    };
    // Shrink from the end, then from the front, never past the active tab —
    // a strip that hides the tab you are on lies about where you are.
    let (mut lo, mut hi) = (0usize, tabs.len());
    while hi - lo > 1 && window_width(lo, hi) > w {
        if hi - 1 > active {
            hi -= 1;
        } else {
            lo += 1;
        }
    }
    if window_width(lo, hi) > w {
        // Not even one tab and its markers fit. Say which tab is active in
        // whatever room there is, ellipsised, rather than overflow a row.
        let (tab, label) = tabs[active];
        let text = clip(&format!(" [{label}]"), w);
        let extent = vec![(tab, 1u16, text.width().saturating_sub(1) as u16)];
        return (
            Line::from(Span::styled(
                text,
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )),
            extent,
        );
    }

    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let mut extents: TabSpans = Vec::new();
    // Columns are counted as the spans are pushed, so the extents cannot drift
    // from the text: every branch that adds width adds it to both.
    let mut col: usize = 1;
    if lo > 0 {
        spans.push(Span::styled("‹ ", Style::default().fg(t.muted)));
        col += 2;
    }
    for (i, (tab, label)) in tabs[lo..hi].iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            col += 1;
        }
        let text = if *tab == app.workspace_tab {
            spans.push(Span::styled(
                format!("[{label}]"),
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
            format!("[{label}]")
        } else {
            spans.push(Span::styled(
                (*label).to_string(),
                Style::default().fg(t.muted),
            ));
            (*label).to_string()
        };
        extents.push((*tab, col as u16, text.width() as u16));
        col += text.width();
    }
    if hi < tabs.len() {
        spans.push(Span::styled(" ›", Style::default().fg(t.muted)));
    }
    (Line::from(spans), extents)
}

fn status_glyph(status: ToolStatus, t: Theme) -> (&'static str, Color) {
    match status {
        ToolStatus::Ok => ("✓", t.ok),
        ToolStatus::Err => ("✗", t.err),
        ToolStatus::Running => ("⚙", t.warn),
    }
}

fn evidence_color(evidence_class: EvidenceClass, t: Theme) -> Color {
    match evidence_class {
        EvidenceClass::ReferenceValidated => t.ok,
        EvidenceClass::Screening => t.warn,
        EvidenceClass::Research => t.accent,
        EvidenceClass::Indeterminate => t.err,
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
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let (head, _) = s.unicode_truncate(max.saturating_sub(1));
    format!("{head}…")
}

fn pad_right_display(s: &str, minimum_columns: usize) -> String {
    let padding = minimum_columns.saturating_sub(s.width());
    format!("{s}{}", " ".repeat(padding))
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
                    evidence_class: EvidenceClass::Indeterminate,
                });
            }
            LineKind::ToolResult {
                tool_name,
                content,
                elapsed_ms,
                success,
                evidence_class,
                ..
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
                    e.evidence_class = *evidence_class;
                } else {
                    out.push(ToolEntry {
                        name: tool_name.clone(),
                        status,
                        elapsed_ms: Some(*elapsed_ms),
                        finding: Some(first_line(content)),
                        evidence_class: *evidence_class,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn build_tools_lines(
    app: &App,
    t: Theme,
    lines: &mut Vec<Line<'static>>,
    rows: &mut PanelRows,
    w: usize,
) {
    // The Tools tab shows the LIVE catalog (the actual tools), with any
    // run-activity beneath it.
    if !app.tool_catalog.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" Catalog · {} tools", app.tool_catalog.len()),
            Style::default().fg(t.dim),
        )));
        let sel = app.workspace_selected.min(app.tool_catalog.len() - 1);
        for (i, tool) in app.tool_catalog.iter().enumerate() {
            rows.push((lines.len(), i));
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
        rows.push((lines.len(), i));
        let focused = app.focus == Focus::Workspace && i == sel;
        let prefix = if focused { "▸ " } else { "  " };
        let (glyph, gcolor) = status_glyph(x.status, t);
        let mut spans = vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(format!("{glyph} "), Style::default().fg(gcolor)),
            Span::styled(x.name.clone(), Style::default().fg(t.text)),
        ];
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            evidence_token(x.evidence_class),
            Style::default()
                .fg(evidence_color(x.evidence_class, t))
                .add_modifier(Modifier::BOLD),
        ));
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

fn build_activity_lines(
    app: &App,
    t: Theme,
    lines: &mut Vec<Line<'static>>,
    rows: &mut PanelRows,
    w: usize,
) {
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
        rows.push((lines.len(), i));
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
        let budget = w.saturating_sub(lead.width() + 2).max(3);
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

fn build_files_lines(
    app: &App,
    t: Theme,
    lines: &mut Vec<Line<'static>>,
    rows: &mut PanelRows,
    w: usize,
) {
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
        rows.push((lines.len(), i));
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

/// Whether an unavailable-store reason is the backend refusing the method.
///
/// The agent answers every method it does not implement with JSON-RPC
/// -32601 and this exact text (`emit_error(-32601, &format!("Method not
/// found: {method}"), id)`, crates/agent/src/protocol.rs). The reason
/// reaches the renderer verbatim. A refused method is a feature that was
/// never wired; a store that failed to open is a feature that was. They
/// have different owners and different fixes, so they must not render as
/// the same sentence.
fn backend_refused_the_method(reason: &str) -> bool {
    reason.starts_with("Method not found")
}

fn build_objects_lines(
    app: &App,
    t: Theme,
    lines: &mut Vec<Line<'static>>,
    rows: &mut PanelRows,
    w: usize,
) {
    if app.objects.is_empty() {
        // Fed by `ui.object.update` (emitted beside the tool card, parsed in
        // `msg.rs`). The tab never sends a request, so no error can reach it,
        // and nothing backfills it from the cache or from history — it fills
        // only when a tool in THIS session creates something. Report the one
        // thing that is known, that no update arrived, and do not promise the
        // reader that doing work will fill it.
        lines.push(Line::from(Span::styled(
            "  No object updates received",
            Style::default().fg(t.muted),
        )));
        return;
    }
    let sel = app
        .workspace_selected
        .min(app.objects.len().saturating_sub(1));
    for (i, obj) in app.objects.iter().enumerate() {
        rows.push((lines.len(), i));
        let focused = app.focus == Focus::Workspace && i == sel;
        let prefix = if focused { "▸ " } else { "  " };
        let glyph = obj.kind.glyph();
        let (status_str, status_color) = match obj.status {
            ObjectStatus::Running => {
                if let Some((cur, tot)) = obj.progress {
                    if tot > 0 {
                        let pct = (cur as f64 / tot as f64 * 100.0) as u64;
                        (format!("{pct}%"), t.warn)
                    } else {
                        ("running".to_string(), t.warn)
                    }
                } else {
                    ("running".to_string(), t.warn)
                }
            }
            ObjectStatus::Completed => ("done".to_string(), t.ok),
            ObjectStatus::Failed => ("FAILED".to_string(), t.err),
            // Warn colour, not ok: we do not know this is fine.
            ObjectStatus::Unknown => ("?".to_string(), t.warn),
        };
        let tag_marker = if obj.tagged { " ★" } else { "" };
        let label_budget = w.saturating_sub(12).max(3);
        let label = clip(&obj.label, label_budget);
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(format!("{glyph} "), Style::default().fg(t.dim)),
            Span::styled(
                // Unknown kinds can be wide Unicode. Preserve the existing
                // ten-column cap, then pad short names in display columns.
                format!("{} ", pad_right_display(&clip(obj.kind.as_str(), 10), 6)),
                Style::default().fg(t.muted),
            ),
            Span::styled(label, Style::default().fg(t.text)),
            Span::styled(tag_marker.to_string(), Style::default().fg(t.warn)),
            Span::raw(" "),
            Span::styled(status_str, Style::default().fg(status_color)),
        ]));
        // Inline expanded detail for the focused row.
        if focused && app.workspace_expanded {
            if let Some((cur, tot)) = obj.progress {
                lines.push(Line::from(Span::styled(
                    format!("  progress: {cur}/{tot}"),
                    Style::default().fg(t.dim),
                )));
            }
            if let Some(detail) = &obj.detail {
                let detail_line = clip(detail, w.saturating_sub(4));
                lines.push(Line::from(Span::styled(
                    format!("  {detail_line}"),
                    Style::default().fg(t.dim),
                )));
            }
            if obj.tagged {
                lines.push(Line::from(Span::styled(
                    "  ★ tagged for agent",
                    Style::default().fg(t.warn),
                )));
            }
        }
    }
}

/// The Structures tab — the materials plane of the sidebar. Shows each
/// structure this session touched: formula first (the identity a materials
/// person scans for), then atom count · composition · source, then the
/// `cache://` reference. Missing meta fields render as `unknown` / `?` —
/// never as plausible defaults. Empty and unavailable are DIFFERENT facts
/// and read differently.
fn build_structures_lines(
    app: &App,
    t: Theme,
    lines: &mut Vec<Line<'static>>,
    rows: &mut PanelRows,
    w: usize,
    available_lines: usize,
) {
    let structures = match &app.structure_store {
        StructuresStoreState::Loading => {
            lines.push(Line::from(Span::styled(
                "  Loading structures…",
                Style::default().fg(t.warn),
            )));
            lines.push(Line::from(Span::styled(
                "  Structure cache data is not ready yet.",
                Style::default().fg(t.dim),
            )));
            return;
        }
        StructuresStoreState::Unavailable(reason) => {
            // `workspace.structures.list` has no handler in the agent today,
            // so this is the arm the user actually lands in, carrying a raw
            // -32601 string. Name the wiring gap; keep the reason as the
            // evidence line.
            let headline = if backend_refused_the_method(reason) {
                "  Structures not connected"
            } else {
                "  Structure cache unavailable"
            };
            lines.push(Line::from(Span::styled(
                headline,
                Style::default().fg(t.err).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("  {}", clip(reason, w.saturating_sub(2))),
                Style::default().fg(t.dim),
            )));
            return;
        }
        StructuresStoreState::Ready(structures) if structures.is_empty() => {
            // The session id on the response is an envelope stamp — it says
            // which turn answered, not which turn the rows belong to. The rows
            // come from the shared structure cache and carry no session at
            // all, so naming the session here would claim a scope the data
            // does not have. Empty means the cache is empty.
            lines.push(Line::from(Span::styled(
                "  No structures in the cache",
                Style::default().fg(t.muted),
            )));
            return;
        }
        StructuresStoreState::Ready(structures) => structures,
    };

    let policy_limited =
        u64::try_from(structures.len()).is_ok_and(|count| count >= app.structure_policy.list_limit);
    let mut item_viewport_lines = available_lines;
    if policy_limited {
        lines.push(Line::from(Span::styled(
            format!(
                "  Newest {} · policy limit reached",
                app.structure_policy.list_limit
            ),
            Style::default().fg(t.warn),
        )));
        item_viewport_lines = item_viewport_lines.saturating_sub(1);
    }

    let selected = app
        .workspace_selected
        .min(structures.len().saturating_sub(1));
    let expanded_lines = if app.workspace_expanded {
        app.structure_policy.expanded_lines
    } else {
        0
    };
    let visible_items = item_viewport_lines
        .saturating_sub(expanded_lines)
        .checked_div(app.structure_policy.item_lines.max(1))
        .unwrap_or(0)
        .max(1);
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_items)
        .min(structures.len().saturating_sub(visible_items));
    let end = start.saturating_add(visible_items).min(structures.len());

    for (index, structure) in structures.iter().enumerate().take(end).skip(start) {
        rows.push((lines.len(), index));
        let focused = app.focus == Focus::Workspace && index == selected;
        let prefix = if focused { "▸ " } else { "  " };

        // Line 1 — formula leads: it is the identity a materials person
        // scans for. Bold so it reads as the row's headline.
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(
                format!("{} ", ObjectKind::Structure.glyph()),
                Style::default().fg(t.dim),
            ),
            Span::styled(
                clip(structure.formula_display(), w.saturating_sub(6)).to_string(),
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            ),
        ]));

        // Line 2 — atom count · composition · source. `?` for an unknown
        // count (the Objects-tab convention); the literal word `unknown`
        // for text fields PRISM never received.
        let atoms = structure
            .n_atoms
            .map(|count| format!("{count} atoms"))
            .unwrap_or_else(|| "? atoms".to_string());
        let facts = format!(
            "{atoms} · {} · {}",
            structure.composition.as_deref().unwrap_or(UNKNOWN),
            structure.source_display(),
        );
        lines.push(Line::from(Span::styled(
            format!("    {}", clip(&facts, w.saturating_sub(4))),
            Style::default().fg(t.dim),
        )));

        // Line 3 — the cache reference (clipped; the full ref is in the
        // Enter detail view).
        lines.push(Line::from(Span::styled(
            format!(
                "    {}",
                clip(structure.cache_ref_display(), w.saturating_sub(4))
            ),
            Style::default().fg(t.muted),
        )));

        if focused && app.workspace_expanded {
            if let Some(name) = &structure.name {
                lines.push(Line::from(Span::styled(
                    format!("    name: {}", clip(name, w.saturating_sub(10))),
                    Style::default().fg(t.dim),
                )));
            }
            if let Some(tool) = &structure.tool {
                lines.push(Line::from(Span::styled(
                    format!("    tool: {}", clip(tool, w.saturating_sub(10))),
                    Style::default().fg(t.dim),
                )));
            }
            if let Some(created_at) = &structure.created_at {
                lines.push(Line::from(Span::styled(
                    format!("    cached: {}", clip(created_at, w.saturating_sub(12))),
                    Style::default().fg(t.dim),
                )));
            }
        }
    }
}

fn build_artifact_lines(
    app: &App,
    t: Theme,
    lines: &mut Vec<Line<'static>>,
    rows: &mut PanelRows,
    w: usize,
    available_lines: usize,
) {
    let artifacts = match &app.artifact_store {
        ArtifactStoreState::Loading => {
            lines.push(Line::from(Span::styled(
                "  Loading artifacts…",
                Style::default().fg(t.warn),
            )));
            lines.push(Line::from(Span::styled(
                "  Artifact data is not ready yet.",
                Style::default().fg(t.dim),
            )));
            return;
        }
        ArtifactStoreState::Unavailable(reason) => {
            // Same rule as the Structures tab: a method the backend does not
            // implement is not a store that failed to open.
            let headline = if backend_refused_the_method(reason) {
                "  Artifacts not connected"
            } else {
                "  Artifact store unavailable"
            };
            lines.push(Line::from(Span::styled(
                headline,
                Style::default().fg(t.err).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("  {}", clip(reason, w.saturating_sub(2))),
                Style::default().fg(t.dim),
            )));
            return;
        }
        ArtifactStoreState::Ready(artifacts) if artifacts.is_empty() => {
            // The backend lists artifacts for the current session only
            // (`list_artifacts` is called with `session`), and rows from any
            // other session are rejected before they reach here. So an empty
            // list is evidence about this session, NOT about the store: the
            // shipped store can hold rows written under an earlier session id
            // and this pane will still be empty. Name the scope.
            lines.push(Line::from(Span::styled(
                "  No artifacts in this session",
                Style::default().fg(t.muted),
            )));
            return;
        }
        ArtifactStoreState::Ready(artifacts) => artifacts,
    };

    let policy_limited =
        u64::try_from(artifacts.len()).is_ok_and(|count| count >= app.artifact_policy.list_limit);
    let mut item_viewport_lines = available_lines;
    if policy_limited {
        lines.push(Line::from(Span::styled(
            format!(
                "  Newest {} · policy limit reached",
                app.artifact_policy.list_limit
            ),
            Style::default().fg(t.warn),
        )));
        item_viewport_lines = item_viewport_lines.saturating_sub(1);
    }

    let selected = app
        .workspace_selected
        .min(artifacts.len().saturating_sub(1));
    let expanded_lines = if app.workspace_expanded {
        app.artifact_policy.expanded_lines
    } else {
        0
    };
    let visible_items = item_viewport_lines
        .saturating_sub(expanded_lines)
        .checked_div(app.artifact_policy.item_lines.max(1))
        .unwrap_or(0)
        .max(1);
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_items)
        .min(artifacts.len().saturating_sub(visible_items));
    let end = start.saturating_add(visible_items).min(artifacts.len());

    for (index, artifact) in artifacts.iter().enumerate().take(end).skip(start) {
        rows.push((lines.len(), index));
        let focused = app.focus == Focus::Workspace && index == selected;
        let prefix = if focused { "▸ " } else { "  " };
        let (badge, badge_color) = match &artifact.promotion {
            ArtifactPromotion::Promoted => ("◆ KG", t.ok),
            ArtifactPromotion::NotPromoted => ("◇ local", t.muted),
            ArtifactPromotion::Unknown(_) => ("? KG", t.warn),
        };
        let fixed_width = prefix.width() + badge.width() + 1;
        let content_width = w.saturating_sub(fixed_width);
        let suffix_budget = content_width / 2;
        let suffix = clip(&format!(" · {}", artifact.age), suffix_budget);
        let tool_budget = content_width.saturating_sub(suffix.width());
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(t.accent)),
            Span::styled(
                badge.to_string(),
                Style::default()
                    .fg(badge_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                clip(&artifact.tool, tool_budget),
                Style::default().fg(t.text),
            ),
            Span::styled(suffix, Style::default().fg(t.muted)),
        ]));
        lines.push(Line::from(Span::styled(
            format!("    {}", clip(&artifact.summary, w.saturating_sub(4))),
            Style::default().fg(t.text),
        )));

        let records = artifact
            .record_count
            .map(|count| format!("{count} records"))
            .unwrap_or_else(|| "records n/a".to_string());
        let mut metadata = format!("{records} · {}", format_bytes(artifact.bytes_size));
        if let ArtifactPromotion::Unknown(raw) = &artifact.promotion {
            metadata.push_str(" · KG=");
            metadata.push_str(raw.as_deref().unwrap_or("not reported"));
        }
        lines.push(Line::from(Span::styled(
            format!("    {}", clip(&metadata, w.saturating_sub(4))),
            Style::default().fg(t.dim),
        )));

        if focused && app.workspace_expanded {
            lines.push(Line::from(Span::styled(
                format!("    id: {}", clip(&artifact.id, w.saturating_sub(8))),
                Style::default().fg(t.dim),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "    created: {}",
                    clip(&artifact.created_at, w.saturating_sub(13))
                ),
                Style::default().fg(t.dim),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "    session: {}",
                    clip(&artifact.session_id, w.saturating_sub(13))
                ),
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
    let area = overlay_area(f, 62, 70);
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
        kv_row(
            t,
            "← / →",
            "switch Activity / Tools / Files / Objects / Structures / Artifacts",
        ),
        kv_row(t, "↑ / ↓", "move selection"),
        kv_row(t, "Enter", "open details for the selected item"),
        kv_row(t, "Space", "expand selected item inline"),
        kv_row(t, "t", "tag/untag object for agent (Objects tab)"),
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
        kv_row(
            t,
            "/browse <url>",
            "read a web page in a headless browser (JS renders)",
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

// ── Account (platform login/logout) ───────────────────────────────
//
// Reads ~/.prism/credentials.json for status (client-side). Logout is sent
// to the backend; login is intentionally non-interactive and reports the
// exact CLI commands instead.

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
    let area = overlay_area(f, 86, 86);
    f.render_widget(Clear, area);

    let ntabs = app.view.tabs.len().max(1);
    let active = app.view.active_tab.min(ntabs - 1);
    let (_, body) = app.view.tabs.get(active).cloned().unwrap_or_default();

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
    let area = overlay_area(f, 82, 80);
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
    let area = overlay_area(f, 60, 50);
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
            "  Hosted platform tools need an account.",
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
        "  login is non-interactive — use `prism login --token <PAT>`",
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
    let area = overlay_area(f, 64, 62);
    f.render_widget(Clear, area);

    if app.apikey_window.adding {
        draw_add_provider_form(f, app, area);
        return;
    }

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
        "  ←/→ provider · type key · ↵ save · ^N add a provider · Esc close",
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

/// Add a provider PRISM does not ship: a name, an OpenAI-compatible base
/// URL, and a key. Three fields, one screen.
///
/// The registry always supported this — `Provider` is data and the user file
/// merges over the built-ins — but writing an entry meant opening
/// `~/.prism/providers.toml` in an editor. A mechanism nobody can reach from
/// the product is not a feature.
fn draw_add_provider_form(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let t = app.theme();
    let w = &app.apikey_window;

    // The key is masked; name and URL are not — they are not secrets, and
    // hiding a URL only makes a typo impossible to spot.
    let masked: String = "•".repeat(w.key_input.len());
    let fields: [(&str, &str, &str); 3] = [
        ("Name", w.new_name.as_str(), "Alibaba DashScope"),
        (
            "Base URL",
            w.new_url.as_str(),
            "https://…/compatible-mode/v1",
        ),
        ("API key", masked.as_str(), "paste it here"),
    ];

    let mut lines: Vec<Line> = vec![Line::raw("")];
    for (i, (label, value, placeholder)) in fields.iter().enumerate() {
        let focused = i == w.field_idx;
        let (shown, colour) = if value.is_empty() {
            (*placeholder, t.muted)
        } else {
            (*value, t.text)
        };
        lines.push(Line::from(vec![
            Span::styled(
                if focused { "> " } else { "  " },
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{label:<9} "),
                Style::default().fg(if focused { t.accent } else { t.muted }),
            ),
            Span::styled(shown.to_string(), Style::default().fg(colour)),
        ]));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(Span::styled(
        "  The key is stored for you; the URL is written to ~/.prism/providers.toml.",
        Style::default().fg(t.muted),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Tab field · ↵ save · Esc back",
        Style::default().fg(t.muted),
    )));

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " Add a provider ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, area);
}

fn draw_config_window(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = overlay_area(f, 86, 86);
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
    let area = overlay_area(f, 58, 60);
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
fn draw_home(f: &mut Frame, app: &App, bounds: Rect) {
    let t = app.theme();
    // The panel fills the bounds it is given (the content column's transcript
    // area), so it shares an origin and a width with the prompt box and
    // footer stacked around it. No centering trickery — a panel that floats
    // inset from its column reads as accidental, not composed.
    f.render_widget(Clear, bounds);
    f.render_widget(
        Block::default().style(Style::default().bg(t.overlay_bg)),
        bounds,
    );

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
        // Same reasoning as the header: the home view says the plane is
        // ready, not how many parts it has. `t` opens the full inventory
        // with the counts and the approval split intact.
        lines.push(row("▣", "tools ready".to_string(), "t open"));
        lines.push(muted(
            "location (cloud/local/remote) not reported — pending tool tags".to_string(),
        ));
    }
    lines.extend([
        Line::raw(""),
        // NOTEBOOKS. This used to read "not wired in-app yet — will be
        // agent-watched + editable", which had stopped being true: the pane
        // exists (`crate::notebook`), the kernel runs in the backend, and
        // the agent shares it through `notebook_exec`. A dashboard that
        // denies a working feature is worse than one that omits it — nobody
        // types `/notebook open` for something the app says is unbuilt.
        section("NOTEBOOKS"),
        muted("/notebook open — Python cells; agent shares kernel".to_string()),
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

    // On a short pane the blank spacer rows go first: drop them rather than
    // clipping the key-hint footer off the bottom of the panel.
    let inner_height = usize::from(bounds.height.saturating_sub(2));
    if lines.len() > inner_height {
        lines.retain(|l| l.width() > 0);
    }

    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.overlay_bg))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.accent))
                .title(Span::styled(
                    " PRISM · materials research workspace ",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )),
        );
    f.render_widget(para, bounds);
}

fn draw_tools_window(f: &mut Frame, app: &App) {
    let t = app.theme();
    let area = overlay_area(f, 82, 84);
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
    let area = overlay_area(f, 78, 80);
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
    // Provenance banner: honest about a stale/offline list.
    if let Some(notice) = &app.model_picker.notice {
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {notice}"),
            Style::default().fg(t.warn),
        )));
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
    let area = overlay_area(f, 78, 80);
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
    let area = overlay_area(f, 78, 80);
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
    let area = overlay_area(f, 82, 80);
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
    let area = overlay_area(f, 70, 60);
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
    let area = overlay_area(f, 60, 50);
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
    let area = overlay_area(f, 80, 80);
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
// shows the selected URL for manual opening; PRISM never launches a browser.

fn draw_link_picker(f: &mut Frame, app: &App) {
    let t = app.theme();
    let lp = &app.link_picker;

    // Confirm dialog: "do you want to go to this website?"
    if lp.confirm {
        let area = overlay_area(f, 64, 28);
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
                Span::styled("Show URL for manual opening", Style::default().fg(t.ok)),
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
    let area = overlay_area(f, 72, 60);
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
    // as broken. Cropped to the region an overlay may claim, like every
    // other overlay, so it cannot land on the prompt box or the sidebar.
    let screen = f.area();
    let full = overlay_bounds(screen);
    let height = (form.fields.len() as u16 + 5).min(full.height);
    let width = (screen.width * 64 / 100).max(40).min(full.width);
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
    let area = overlay_area(f, 72, 70);
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
                    format!("   (.{})", crate::knowledge::ingest_extensions().join(" .")),
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
    let area = overlay_area(f, 82, 82);
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
    // Figure strip: the newest cell's image, DRAWN, in the pane the reader is
    // already looking at.
    //
    // The history above still names the path — a path is useful, it is just not
    // a picture, and reporting one instead of showing the figure was the whole
    // defect. Only the newest figure is drawn: interleaving every cell's image
    // with a scrolling text history means tracking each one's wrapped offset as
    // the reader moves, and a stale offset would paint a plot over the wrong
    // cell. One correct figure beats several that drift.
    let newest_figure = pane
        .cells
        .iter()
        .rev()
        .find_map(|cell| cell.image_paths.last());
    // Below this the strip is too short to read, so the history keeps the room.
    const MIN_ROWS_FOR_A_FIGURE: u16 = 12;
    let (history_area, figure_area) = match newest_figure {
        Some(_) if rows[1].height >= MIN_ROWS_FOR_A_FIGURE => {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(rows[1].height / 2)])
                .split(rows[1]);
            (split[0], Some(split[1]))
        }
        _ => (rows[1], None),
    };
    f.render_widget(history, history_area);
    if let (Some(figure_area), Some(path)) = (figure_area, newest_figure)
        && let Err(err) = app.image_view().draw(f, figure_area, path)
    {
        // Never a blank rectangle: a figure that could not be drawn says so and
        // names the file, because blank and broken look identical otherwise.
        f.render_widget(
            Paragraph::new(err.line()).style(Style::default().fg(Color::Red)),
            figure_area,
        );
    }

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
        let area = overlay_area(f, 72, 70);
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

    let area = overlay_area(f, 60, 20);
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

/// Hard-wrap plain text to `width` DISPLAY COLUMNS per line (no word-splitting
/// smarts — code must never be reflowed in a way that hides content). Wraps on
/// unicode display width, not char count, so a CJK/emoji-heavy line (each such
/// glyph is 2 columns wide) is not right-clipped past the panel edge where the
/// popup has no horizontal scroll. Every input line yields at least one output
/// line, so nothing is dropped.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut col = 0usize;
        for ch in line.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            // Break before this glyph would spill past the edge (but never on
            // an empty line, so a lone wide char wider than `width` still
            // emits rather than looping).
            if col + w > width && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                col = 0;
            }
            current.push(ch);
            col += w;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendHandle, FakeScenario};
    use unicode_width::UnicodeWidthStr;

    /// The workspace tab strip must never be wider than the panel it sits in.
    /// The panel paragraph wraps, so a single column of overflow does not
    /// truncate the strip — it pushes the trailing tabs onto the next row and
    /// costs a line of panel content.
    ///
    /// Checked at every width because width alone selects the ladder rung.
    /// The shipped layout only ever hands this a 32–41 column interior (the
    /// sidebar is hidden below 100 terminal columns and is then at least 33
    /// wide), so the three-letter rung always wins and the narrower rungs are
    /// currently UNREACHABLE through `draw`. They are tested as a contract for
    /// a future tab count, not as a live defect.
    ///
    /// The arithmetic, since an earlier version of this comment guessed it
    /// wrong and said "a seventh tab": `width_of` is
    /// `1 + labels + (n - 1) + 2`, against a 32-column floor. Seven tabs at
    /// three letters is 30 columns, which still fits. Reaching the MIN rung
    /// needs `4n + 2 > 32`, i.e. **n ≥ 8**; reaching the elision rung below it
    /// needs `3n + 2 > 32`, i.e. **n ≥ 11**. Eleven tabs, not seven.
    #[test]
    fn workspace_tab_strip_never_exceeds_the_width_it_is_given() {
        let mut app = App::new(BackendHandle::fake(FakeScenario::BasicChat));
        let t = app.theme();
        for tab in [
            WorkspaceTab::Activity,
            WorkspaceTab::Tools,
            WorkspaceTab::Files,
            WorkspaceTab::Objects,
            WorkspaceTab::Structures,
            WorkspaceTab::Artifacts,
        ] {
            app.workspace_tab = tab;
            for w in 0..=48usize {
                let (line, _) = workspace_tabs_line(&app, t, w);
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    line.width() <= w,
                    "{tab:?} strip is {} cols in a {w}-col panel — it wraps: {text:?}",
                    line.width()
                );
            }
        }
    }

    /// Elision must be visible and must never hide the tab you are on.
    #[test]
    fn a_narrowed_tab_strip_marks_the_tabs_it_dropped() {
        let mut app = App::new(BackendHandle::fake(FakeScenario::BasicChat));
        let t = app.theme();
        for (tab, label) in [
            (WorkspaceTab::Activity, "Ac"),
            (WorkspaceTab::Artifacts, "Ar"),
        ] {
            app.workspace_tab = tab;
            // 12 columns: the two-letter rung needs 20, so tabs must drop.
            let (line, _) = workspace_tabs_line(&app, t, 12);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.contains(&format!("[{label}]")),
                "the active tab vanished from the strip: {text:?}"
            );
            assert!(
                text.contains('‹') || text.contains('›'),
                "tabs were dropped with nothing to say so: {text:?}"
            );
        }
    }

    #[test]
    fn clip_truncates_wide_text_by_display_columns() {
        let input = "\u{754c}\u{754c}\u{754c}";
        let clipped = clip(input, 5);

        assert_eq!(clipped, "\u{754c}\u{754c}…");
        assert_eq!(clipped.width(), 5);
    }

    #[test]
    fn clip_keeps_emoji_graphemes_intact() {
        let input = "\u{1f469}\u{200d}\u{1f52c}science";
        let clipped = clip(input, 4);

        assert!(clipped.width() <= 4, "{clipped:?} overflowed");
        assert!(!clipped.ends_with('\u{200d}'), "split a joined emoji");
    }

    #[test]
    fn wrap_plain_bounds_wide_chars_by_display_width() {
        // Each `中` is 2 columns wide: at width 4 only two fit per line, so a
        // char-count wrap (old behavior) would overflow the panel and clip.
        let wrapped = wrap_plain("中中中中中", 4);
        for line in &wrapped {
            assert!(
                line.width() <= 4,
                "line {line:?} is {} cols, over the 4-col width",
                line.width()
            );
        }
        assert_eq!(wrapped.concat(), "中中中中中", "no glyph may be dropped");
    }

    #[test]
    fn wrap_plain_ascii_still_wraps_by_column() {
        let wrapped = wrap_plain("abcdefgh", 3);
        assert_eq!(wrapped, vec!["abc", "def", "gh"]);
    }

    #[test]
    fn wrap_plain_preserves_blank_lines() {
        assert_eq!(wrap_plain("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn wrap_plain_emits_lone_overwide_glyph() {
        // A single glyph wider than the whole width still emits (no infinite
        // loop, nothing dropped).
        assert_eq!(wrap_plain("🚀", 1), vec!["🚀"]);
    }
}
