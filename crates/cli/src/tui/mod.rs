//! PRISM TUI — Ratatui-based terminal interface.
//!
//! Boot sequence animation → interactive agent shell.
//! Design ported from the Gemini-generated HTML/JS prototype.
#![allow(
    clippy::manual_range_contains,
    clippy::approx_constant,
    clippy::redundant_pattern_matching
)]

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Alignment,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::{
    io::{self, Write},
    thread,
    time::{Duration, Instant},
};

pub mod backend_client;
pub mod components;
pub mod markdown;
pub mod protocol;
pub mod state;

// 80-column base scene mask — the prism + PRISM text
const SCENE: [&str; 9] = [
    "                                                                                ",
    "             \u{25b2}                                                                  ",
    "            / \\                \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2588}\u{2557}           ",
    "           /   \\               \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2551}           ",
    "          /  \u{2b21}  \\              \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2551}           ",
    "         /       \\             \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2550}\u{255d} \u{2588}\u{2588}\u{2554}\u{2550}\u{2550}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551}\u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2554}\u{255d}\u{2588}\u{2588}\u{2551}           ",
    "        /_________\\            \u{2588}\u{2588}\u{2551}     \u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551}\u{2588}\u{2588}\u{2551} \u{255a}\u{2550}\u{255d} \u{2588}\u{2588}\u{2551}           ",
    "                               \u{255a}\u{2550}\u{255d}     \u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}\u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}\u{255a}\u{2550}\u{255d}     \u{255a}\u{2550}\u{255d}           ",
    "                                                                                ",
];

/// HSV to RGB conversion
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);

    let (r, g, b) = match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };

    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Core render engine: determines character + RGB for position (x,y) at time t
fn get_char_and_color(x: usize, y: usize, t: f64, row_chars: &[char]) -> (char, (u8, u8, u8)) {
    let mut c = if x < row_chars.len() {
        row_chars[x]
    } else {
        ' '
    };
    let mut r: f64 = 25.0;
    let mut g: f64 = 25.0;
    let mut b: f64 = 25.0;

    let wave_x = t * 1.5 - 10.0;
    let xf = x as f64;
    let yf = y as f64;

    // 1. Incoming data streams
    if x < 10 && c == ' ' {
        let tx = xf - (t * 4.0).floor();
        let tx2 = xf - (t * 2.0).floor();
        let tx3 = xf - (t * 3.0).floor();

        if y == 4 && tx.rem_euclid(15.0) < 3.0 {
            c = '\u{2500}';
            r = 255.0;
            g = 255.0;
            b = 255.0;
        } else if y == 3 && tx2.rem_euclid(25.0) < 1.0 {
            c = '-';
            r = 0.0;
            g = 150.0;
            b = 255.0;
        } else if y == 5 && tx3.rem_euclid(18.0) < 2.0 {
            c = '\u{2504}';
            r = 100.0;
            g = 200.0;
            b = 255.0;
        }
    }

    // 2. Prism core
    let is_prism = ['\u{25b2}', '/', '\\', '_', '\u{2b21}'].contains(&c) && x < 20;
    if is_prism {
        if xf <= wave_x {
            let dist = wave_x - xf;
            let intensity = (1.0 - dist * 0.08).max(0.0);
            r = intensity * 255.0;
            g = 100.0 + intensity * 155.0;
            b = 180.0 + intensity * 75.0;

            if c == '\u{2b21}' {
                let pulse = ((t * 0.3).sin() + 1.0) / 2.0;
                r = 100.0 + pulse * 155.0;
                g = 255.0;
                b = 255.0;
            }
        } else {
            r = 0.0;
            g = 30.0;
            b = 60.0;
        }
    }

    // 3. Refraction beams
    if x > 18 && x < 31 && c == ' ' && xf <= wave_x {
        let dist = wave_x - xf;
        let intensity = (1.0 - dist * 0.05).max(0.0);
        if intensity > 0.1 {
            let hue = (yf - 2.0) / 6.0;
            let (cr, cg, cb) = hsv_to_rgb(hue, 1.0, 1.0);
            let cr = cr as f64 * intensity;
            let cg = cg as f64 * intensity;
            let cb = cb as f64 * intensity;

            let check =
                |val: f64, mod_val: f64, limit: f64| -> bool { val.rem_euclid(mod_val) < limit };

            if y == 2 && check(xf - (t * 2.0).floor(), 6.0, 2.0) {
                c = '\u{2802}';
                r = cr;
                g = cg;
                b = cb;
            } else if y == 3 && check(xf - (t * 3.0).floor(), 4.0, 1.0) {
                c = '\u{2500}';
                r = cr;
                g = cg;
                b = cb;
            } else if y == 4 && check(xf - (t * 4.0).floor(), 5.0, 2.0) {
                c = '\u{2501}';
                r = cr;
                g = cg;
                b = cb;
            } else if y == 5 && check(xf - (t * 3.5).floor(), 4.0, 2.0) {
                c = '\u{2500}';
                r = cr;
                g = cg;
                b = cb;
            } else if y == 6 && check(xf - (t * 2.5).floor(), 7.0, 1.0) {
                c = '\u{2804}';
                r = cr;
                g = cg;
                b = cb;
            }
        }
    }

    // 4. Text render (PRISM block letters)
    let is_text = x >= 31 && c != ' ';
    if is_text {
        if xf <= wave_x {
            let dist = wave_x - xf;
            let hue = (xf * 0.02 - yf * 0.05 - t * 0.03).rem_euclid(1.0);
            let (tr, tg, tb) = hsv_to_rgb(hue, 0.9, 1.0);

            let flash = (1.0 - dist * 0.15).max(0.0);
            r = (tr as f64 + flash * 255.0).min(255.0);
            g = (tg as f64 + flash * 255.0).min(255.0);
            b = (tb as f64 + flash * 255.0).min(255.0);
        } else {
            r = 30.0;
            g = 30.0;
            b = 30.0;
        }
    }

    // 5. Knowledge graph background nodes
    if c == ' ' && (x > 18 || y < 3 || y > 5) && !((19..31).contains(&x) && (2..=6).contains(&y)) {
        let noise = ((xf * 12.34 + yf * 3.14 + t * 0.05).sin()
            + (xf * 7.1 + yf * 5.2 - t * 0.02).cos())
            / 2.0;
        if noise > 0.85 {
            if xf <= wave_x {
                c = if noise < 0.95 { '\u{00b7}' } else { '+' };
                r = 60.0;
                g = 60.0;
                b = 100.0;
            } else {
                c = '.';
                r = 20.0;
                g = 20.0;
                b = 20.0;
            }
        }
    }

    (c, (r as u8, g as u8, b as u8))
}

/// A single boot check result
pub struct BootCheck {
    pub name: String,
    pub result: String,
    pub ok: bool,
    pub dots: u32,
    pub delay_ms: u64,
}

/// Run the boot sequence (raw stdout, before ratatui takes over).
/// Takes real API ping results for maximum honesty.
pub fn boot_sequence(checks: &[BootCheck]) {
    let print_colored = |text: &str| {
        print!("{text}");
        let _ = io::stdout().flush();
    };

    print_colored(
        "\x1b[38;2;0;255;255m[PRISM]\x1b[0m \x1b[38;2;200;200;200mInitializing Materials Discovery Node...\x1b[0m\n",
    );
    thread::sleep(Duration::from_millis(200));

    for check in checks {
        print_colored(&format!(
            "\x1b[38;2;100;100;255m \u{251c}\u{2500}\u{2500} \x1b[38;2;255;255;255m{} \x1b[0m",
            check.name
        ));
        for _ in 0..check.dots {
            print_colored("\x1b[38;2;0;255;255m.\x1b[0m");
            thread::sleep(Duration::from_millis(check.delay_ms));
        }
        if check.ok {
            print_colored(&format!(
                " \x1b[38;2;0;255;0m[OK]\x1b[0m \x1b[38;2;100;100;100m({})\x1b[0m\n",
                check.result
            ));
        } else {
            print_colored(&format!(
                " \x1b[38;2;255;80;80m[--]\x1b[0m \x1b[38;2;100;100;100m({})\x1b[0m\n",
                check.result
            ));
        }
    }

    print_colored(
        "\n\x1b[38;2;0;255;255m[PRISM]\x1b[0m \x1b[38;2;200;200;200mIgniting core...\x1b[0m\n",
    );
    thread::sleep(Duration::from_millis(300));
}

/// Run the animated splash screen. Returns when user presses any key.
pub fn run_splash() -> io::Result<()> {
    // Pre-compute SCENE as char vectors for indexing
    let scene_chars: Vec<Vec<char>> = SCENE.iter().map(|row| row.chars().collect()).collect();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut t: f64 = 0.0;
    let tick_rate = Duration::from_millis(40); // ~25 FPS
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| {
            let mut lines = Vec::new();

            for (y, row_chars) in scene_chars.iter().enumerate() {
                let mut spans = Vec::new();
                for x in 0..80 {
                    let (ch, (r, g, b)) = get_char_and_color(x, y, t, row_chars);
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(Color::Rgb(r, g, b)),
                    ));
                }
                lines.push(Line::from(spans));
            }

            // Status footer
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "   >> System: ",
                    Style::default().fg(Color::Rgb(100, 100, 100)),
                ),
                Span::styled("ONLINE", Style::default().fg(Color::Rgb(0, 255, 0))),
                Span::styled(
                    " | Entities: ",
                    Style::default().fg(Color::Rgb(100, 100, 100)),
                ),
                Span::styled("211K+", Style::default().fg(Color::Rgb(0, 255, 255))),
                Span::styled(" | v", Style::default().fg(Color::Rgb(100, 100, 100))),
                Span::styled(
                    env!("CARGO_PKG_VERSION"),
                    Style::default().fg(Color::Rgb(255, 100, 255)),
                ),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "   Press any key to continue...",
                Style::default().fg(Color::Rgb(80, 80, 80)),
            )]));

            let paragraph = Paragraph::new(lines)
                .block(
                    Block::default()
                        .title(" PRISM // Materials Discovery ")
                        .title_alignment(Alignment::Center)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Rgb(50, 50, 50))),
                )
                .alignment(Alignment::Left);

            let area = f.size();
            let centered = ratatui::layout::Rect {
                x: area.x + area.width.saturating_sub(84) / 2,
                y: area.y + area.height.saturating_sub(15) / 2,
                width: 84.min(area.width),
                height: 15.min(area.height),
            };

            f.render_widget(paragraph, centered);
        })?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO);

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('q') => break,
                    _ => break, // Any key continues
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            t += 0.5;
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

pub async fn run_tui_app(
    project_root: &std::path::Path,
    python_bin: &std::path::Path,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = state::App::new(project_root.to_path_buf());

    // Pre-load model catalog via direct HTTP (don't wait for backend)
    {
        if let Ok(paths) = prism_runtime::PrismPaths::discover() {
            if let Ok(cli_state) = paths.load_cli_state() {
                if let Some(creds) = &cli_state.credentials {
                    if let Some(project_id) = &creds.project_id {
                        let url = format!(
                            "https://api.marc27.com/api/v1/projects/{project_id}/llm/models"
                        );
                        if let Ok(resp) = reqwest::Client::new()
                            .get(&url)
                            .header("Authorization", format!("Bearer {}", creds.access_token))
                            .timeout(Duration::from_secs(10))
                            .send()
                            .await
                        {
                            if let Ok(data) = resp.json::<serde_json::Value>().await {
                                let models = components::model_picker::parse_models(&data);
                                if !models.is_empty() {
                                    app.cached_providers =
                                        components::model_picker::providers(&models);
                                    app.model_count = Some(models.len());
                                    app.cached_models = models;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Check local node
    if let Ok(resp) = reqwest::Client::new()
        .get("http://127.0.0.1:7327/api/health")
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        if resp.status().is_success() {
            app.node_count = Some(1);
            // Try to get peer count
            if let Ok(mesh) = reqwest::Client::new()
                .get("http://127.0.0.1:7327/api/mesh/nodes")
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                if let Ok(data) = mesh.json::<serde_json::Value>().await {
                    app.peer_count = data
                        .get("peer_count")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize);
                }
            }
        }
    }

    // Spawn backend process
    let current_exe = std::env::current_exe()?;
    let mut client =
        backend_client::BackendClient::spawn(&current_exe, project_root, python_bin).await?;

    // Send init request
    let init_req = protocol::InitRequest {
        jsonrpc: "2.0".to_string(),
        method: "init".to_string(),
        id: 1,
        params: protocol::InitParams {
            auto_approve: false,
            resume: "default".to_string(),
        },
    };
    client
        .tx_requests
        .send(serde_json::to_string(&init_req)?)
        .await?;

    // Input thread bridging crossterm events
    let (tx_events, mut rx_events) = tokio::sync::mpsc::unbounded_channel();
    thread::spawn(move || loop {
        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(e) = event::read() {
                let _ = tx_events.send(e);
            }
        }
    });

    loop {
        terminal.draw(|f| components::layout::draw(f, &app))?;

        tokio::select! {
            notif = client.rx_notifications.recv() => {
                let Some(notif) = notif else { break; };
                match notif.notification {
                    protocol::ProtocolNotification::Status(status) => {
                        app.status = Some(status);
                    }
                    protocol::ProtocolNotification::TextDelta(delta) => {
                        app.streaming_text.push_str(&delta.text);
                    }
                    protocol::ProtocolNotification::TextFlush(_) => {
                        let text = app.streaming_text.clone();
                        app.streaming_text.clear();
                        if !text.is_empty() {
                            app.chat_history.push(state::ChatElement::Text(text));
                        }
                    }
                    protocol::ProtocolNotification::ToolStart(ts) => {
                        app.chat_history.push(state::ChatElement::ToolStart(ts));
                    }
                    protocol::ProtocolNotification::Card(card) => {
                        app.chat_history.push(state::ChatElement::Card(card));
                    }
                    protocol::ProtocolNotification::Prompt(prompt) => {
                        app.active_prompt = Some(prompt);
                    }
                    protocol::ProtocolNotification::Cost(cost) => {
                        app.total_cost += cost.turn_cost;
                        app.chat_history.push(state::ChatElement::Cost(cost));
                    }
                    protocol::ProtocolNotification::TurnComplete(_) => {
                        // Flush any remaining streaming text
                        if !app.streaming_text.is_empty() {
                            let text = app.streaming_text.clone();
                            app.streaming_text.clear();
                            app.chat_history.push(state::ChatElement::Text(text));
                        }
                    }
                    protocol::ProtocolNotification::Welcome(w) => {
                        app.tool_count = w.tool_count;
                    }
                    protocol::ProtocolNotification::View(view) => {
                        app.active_view = Some(view);
                    }
                    _ => {}
                }
            }
            ev = rx_events.recv() => {
                let Some(Event::Key(key)) = ev else { continue; };
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.sidebar_visible = !app.sidebar_visible;
                    }
                    // Tab switching: 1-9 when NOT typing in input
                    KeyCode::Char(c @ '1'..='9') if app.focus != state::FocusZone::Input => {
                        let idx = (c as usize) - ('1' as usize);
                        app.select_activity(idx);
                    }
                    // Ctrl+1-9 always works for tab switching
                    KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let idx = (c as usize) - ('1' as usize);
                        app.select_activity(idx);
                    }
                    // Left/Right arrows switch tabs when sidebar or chat is focused
                    KeyCode::Left if app.focus == state::FocusZone::Sidebar || app.focus == state::FocusZone::Chat => {
                        app.activity_up();
                    }
                    KeyCode::Right if app.focus == state::FocusZone::Sidebar || app.focus == state::FocusZone::Chat => {
                        app.activity_down();
                    }
                    // Alt+Left/Right always works
                    KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                        app.activity_up();
                    }
                    KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                        app.activity_down();
                    }
                    // Tab cycles focus zones (when palette/picker not open)
                    KeyCode::Tab if !app.palette_visible && !app.model_picker_visible => {
                        app.focus = match app.focus {
                            state::FocusZone::Input => {
                                if app.sidebar_visible {
                                    state::FocusZone::Sidebar
                                } else {
                                    state::FocusZone::Chat
                                }
                            }
                            state::FocusZone::Sidebar => state::FocusZone::Chat,
                            state::FocusZone::Chat => state::FocusZone::Input,
                        };
                    }
                    // Up/Down when chat is focused → scroll chat
                    KeyCode::Up if app.focus == state::FocusZone::Chat => {
                        if app.chat_scroll > 0 {
                            app.chat_scroll -= 1;
                        }
                    }
                    KeyCode::Down if app.focus == state::FocusZone::Chat => {
                        app.chat_scroll += 1;
                    }
                    KeyCode::PageUp if app.focus == state::FocusZone::Chat => {
                        app.chat_scroll = app.chat_scroll.saturating_sub(10);
                    }
                    KeyCode::PageDown if app.focus == state::FocusZone::Chat => {
                        app.chat_scroll += 10;
                    }
                    // Home goes to top, End goes to bottom
                    KeyCode::Home if app.focus == state::FocusZone::Chat => {
                        app.chat_scroll = 0;
                    }
                    KeyCode::End if app.focus == state::FocusZone::Chat => {
                        app.chat_scroll = u16::MAX; // will be clamped by Paragraph
                    }
                    // Up/Down when sidebar is focused → move selection
                    KeyCode::Up if app.focus == state::FocusZone::Sidebar => {
                        if app.sidebar_scroll > 0 {
                            app.sidebar_scroll -= 1;
                        }
                    }
                    KeyCode::Down if app.focus == state::FocusZone::Sidebar => {
                        let items = components::sidebar::build_items(&app);
                        let max = components::sidebar::selectable_count(&items);
                        if (app.sidebar_scroll as usize) < max.saturating_sub(1) {
                            app.sidebar_scroll += 1;
                        }
                    }
                    // Enter on sidebar → execute selected action
                    KeyCode::Enter if app.focus == state::FocusZone::Sidebar => {
                        let items = components::sidebar::build_items(&app);
                        if let Some(action) = components::sidebar::get_selected_action(&items, app.sidebar_scroll as usize) {
                            // Execute the slash command
                            let req = protocol::InputCommandRequest {
                                jsonrpc: "2.0".to_string(),
                                method: "input.command".to_string(),
                                id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                params: protocol::InputCommandParams {
                                    command: action,
                                    silent: false,
                                },
                            };
                            let _ = client.tx_requests.send(serde_json::to_string(&req)?).await;
                            // Switch to chat to see the result
                            app.focus = state::FocusZone::Chat;
                        }
                    }
                    KeyCode::Esc => {
                        if app.model_picker_visible {
                            app.model_picker_visible = false;
                        } else if app.palette_visible {
                            app.palette_visible = false;
                        } else {
                            app.active_view = None;
                            app.active_prompt = None;
                        }
                    }
                    // ── Model picker keys ────────────────────────
                    KeyCode::Up if app.model_picker_visible => {
                        if app.model_picker_selected > 0 {
                            app.model_picker_selected -= 1;
                        }
                    }
                    KeyCode::Down if app.model_picker_visible => {
                        app.model_picker_selected += 1;
                    }
                    KeyCode::Tab if app.model_picker_visible => {
                        // Cycle provider filter
                        let max = app.cached_providers.len() + 1; // +1 for "all"
                        app.model_picker_provider_idx = (app.model_picker_provider_idx + 1) % max;
                        app.model_picker_selected = 0;
                    }
                    KeyCode::Char(c) if app.model_picker_visible => {
                        app.model_picker_search.push(c);
                        app.model_picker_selected = 0;
                    }
                    KeyCode::Backspace if app.model_picker_visible => {
                        app.model_picker_search.pop();
                        app.model_picker_selected = 0;
                    }
                    KeyCode::Enter if app.model_picker_visible => {
                        // Select the model
                        let provider_filter = if app.model_picker_provider_idx == 0 {
                            None
                        } else {
                            app.cached_providers.get(app.model_picker_provider_idx - 1).map(|s| s.as_str())
                        };
                        let filtered = components::model_picker::filter_models(
                            &app.cached_models,
                            &app.model_picker_search,
                            provider_filter,
                        );
                        if let Some(model) = filtered.get(app.model_picker_selected) {
                            // Send /model <id> command to backend
                            let cmd = format!("/model {}", model.model_id);
                            let req = protocol::InputCommandRequest {
                                jsonrpc: "2.0".to_string(),
                                method: "input.command".to_string(),
                                id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                params: protocol::InputCommandParams {
                                    command: cmd,
                                    silent: true,
                                },
                            };
                            let _ = client.tx_requests.send(serde_json::to_string(&req)?).await;
                        }
                        app.model_picker_visible = false;
                        app.model_picker_search.clear();
                        app.model_picker_selected = 0;
                        app.model_picker_provider_idx = 0;
                        continue;
                    }
                    // ── Command palette keys ─────────────────────
                    KeyCode::Up if app.palette_visible => {
                        if app.palette_selected > 0 {
                            app.palette_selected -= 1;
                        }
                    }
                    KeyCode::Down if app.palette_visible => {
                        app.palette_selected += 1;
                    }
                    KeyCode::Tab if app.palette_visible => {
                        let commands = components::command_palette::all_commands();
                        let query = app.input_buffer.strip_prefix('/').unwrap_or(&app.input_buffer);
                        let filtered = components::command_palette::filter_commands(&commands, query);
                        if let Some(entry) = filtered.get(app.palette_selected) {
                            app.input_buffer = entry.command.clone();
                            app.palette_visible = false;
                            app.palette_selected = 0;
                        }
                    }
                    // Left/Right arrow in input → move cursor
                    KeyCode::Left if app.focus == state::FocusZone::Input && !app.palette_visible && !app.model_picker_visible => {
                        if app.input_cursor > 0 {
                            // Move to previous char boundary
                            let mut pos = app.input_cursor - 1;
                            while pos > 0 && !app.input_buffer.is_char_boundary(pos) {
                                pos -= 1;
                            }
                            app.input_cursor = pos;
                        }
                    }
                    KeyCode::Right if app.focus == state::FocusZone::Input && !app.palette_visible && !app.model_picker_visible => {
                        if app.input_cursor < app.input_buffer.len() {
                            let mut pos = app.input_cursor + 1;
                            while pos < app.input_buffer.len() && !app.input_buffer.is_char_boundary(pos) {
                                pos += 1;
                            }
                            app.input_cursor = pos;
                        }
                    }
                    KeyCode::Char(c) => {
                        if app.active_view.is_none() && app.active_prompt.is_none() && app.focus == state::FocusZone::Input {
                            app.input_buffer.insert(app.input_cursor, c);
                            app.input_cursor += c.len_utf8();
                            // Show palette when typing /
                            if app.input_buffer.starts_with('/') {
                                app.palette_visible = true;
                                app.palette_selected = 0;
                            } else {
                                app.palette_visible = false;
                            }
                        } else if let Some(_) = &app.active_prompt {
                            // Quick response bindings y/n/a/b
                            match c {
                                'y' | 'n' | 'a' | 'b' => {
                                    app.active_prompt = None;
                                    let resp = protocol::ApprovalRespondRequest {
                                        jsonrpc: "2.0".to_string(),
                                        method: "input.approval".to_string(),
                                        id: 2,
                                        params: protocol::ApprovalRespondParams {
                                            response: c.to_string(),
                                        },
                                    };
                                    let _ = client.tx_requests.send(serde_json::to_string(&resp)?).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if app.active_view.is_none() && app.active_prompt.is_none() && app.focus == state::FocusZone::Input {
                            if app.input_cursor > 0 {
                                // Find previous char boundary
                                let mut prev = app.input_cursor - 1;
                                while prev > 0 && !app.input_buffer.is_char_boundary(prev) {
                                    prev -= 1;
                                }
                                app.input_buffer.drain(prev..app.input_cursor);
                                app.input_cursor = prev;
                            }
                            if app.input_buffer.starts_with('/') {
                                app.palette_visible = true;
                                app.palette_selected = 0;
                            } else {
                                app.palette_visible = false;
                            }
                        }
                    }
                    KeyCode::Enter if app.focus == state::FocusZone::Input || app.palette_visible || app.model_picker_visible => {
                        // If palette is visible, execute the selected command
                        if app.palette_visible {
                            let commands = components::command_palette::all_commands();
                            let query = app.input_buffer.strip_prefix('/').unwrap_or(&app.input_buffer);
                            let filtered = components::command_palette::filter_commands(&commands, query);
                            if let Some(entry) = filtered.get(app.palette_selected) {
                                let cmd = entry.command.clone();
                                app.input_buffer.clear();
                                app.input_cursor = 0;
                                app.palette_visible = false;
                                app.palette_selected = 0;

                                // Special: /model opens picker
                                if (cmd == "/model" || cmd == "/models") && !app.cached_models.is_empty() {
                                    app.model_picker_visible = true;
                                    continue;
                                }

                                // Execute the command
                                let req = protocol::InputCommandRequest {
                                    jsonrpc: "2.0".to_string(),
                                    method: "input.command".to_string(),
                                    id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                    params: protocol::InputCommandParams {
                                        command: cmd,
                                        silent: false,
                                    },
                                };
                                let _ = client.tx_requests.send(serde_json::to_string(&req)?).await;
                                continue;
                            }
                            app.palette_visible = false;
                            app.palette_selected = 0;
                            continue;
                        }
                        if app.active_view.is_none() && app.active_prompt.is_none() && !app.input_buffer.is_empty() {
                            let text = app.input_buffer.clone();
                            app.input_buffer.clear();
                                app.input_cursor = 0;

                            app.chat_history.push(state::ChatElement::UserMessage(text.clone()));

                            // Special handling for /model — open model picker
                            if text == "/model" || text == "/models" {
                                if !app.cached_models.is_empty() {
                                    app.model_picker_visible = true;
                                    app.model_picker_search.clear();
                                    app.model_picker_selected = 0;
                                    app.model_picker_provider_idx = 0;
                                } else {
                                    // No cached models — send as regular command
                                    let req = protocol::InputCommandRequest {
                                        jsonrpc: "2.0".to_string(),
                                        method: "input.command".to_string(),
                                        id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                        params: protocol::InputCommandParams {
                                            command: text,
                                            silent: false,
                                        },
                                    };
                                    let _ = client.tx_requests.send(serde_json::to_string(&req)?).await;
                                }
                                continue;
                            }

                            if text.starts_with('/') {
                                let req = protocol::InputCommandRequest {
                                    jsonrpc: "2.0".to_string(),
                                    method: "input.command".to_string(),
                                    id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                    params: protocol::InputCommandParams {
                                        command: text,
                                        silent: false,
                                    },
                                };
                                let _ = client.tx_requests.send(serde_json::to_string(&req)?).await;
                            } else {
                                let req = protocol::InputMessageRequest {
                                    jsonrpc: "2.0".to_string(),
                                    method: "input.message".to_string(),
                                    id: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                                    params: protocol::InputMessageParams {
                                        text,
                                    },
                                };
                                let _ = client.tx_requests.send(serde_json::to_string(&req)?).await;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
