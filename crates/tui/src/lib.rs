// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! PRISM TUI — full-screen Ratatui terminal UI for the AI agent.
//!
//! Architecture: The Elm Architecture (TEA)
//!
//! NOTE: the TUI is still being wired up — several enum-variant fields
//! (tool_name, elapsed_ms, content, etc.) and the `ChatLine` import in
//! render.rs are retained for the upcoming render passes but not yet
//! read.  Silence the dead-code warnings crate-wide until they are.
#![allow(dead_code, unused_imports, unused_variables)]

//!   - App state holds all model data (messages, input, scroll, status)
//!   - Msg enum: every input (key, agent event, tick) becomes a Msg
//!   - update(app, msg) → applies transition, returns nothing
//!   - render(app, frame) → pure render from state
//!
//! The agent backend runs as a background tokio task. It sends Msg
//! events through an mpsc channel. The main loop uses tokio::select!
//! to multiplex crossterm key events and agent messages.
//!
//! ```text
//! ┌──────────────────────────────────────────┐
//! │  Chat Scrollback (scrollable viewport)    │
//! │  ┌────────────────────────────────────┐   │
//! │  │ > user message                     │   │
//! │  │ ◆ assistant streaming text...      │   │
//! │  │ ⚙ sample_material [✓ 292ms]       │   │
//! │  │   results: W45 Mo13 Ta9...        │   │
//! │  └────────────────────────────────────┘   │
//! ├──────────────────────────────────────────┤
//! │  Status bar: model | cost | tools | mode  │
//! ├──────────────────────────────────────────┤
//! │  Input (tui-textarea, multi-line)         │
//! │  Type a message... (Enter=send, Esc=blur) │
//! └──────────────────────────────────────────┘
//! ```

pub mod app;
pub mod backend;
pub mod command;
pub mod form;
pub mod gh;
pub mod keymap;
pub mod knowledge;
pub mod latex;
pub mod markdown;
pub mod msg;
/// Render module — public for integration snapshot tests only.
///
/// This module is NOT stable API. It exists as `pub` so that the
/// `tests/render_snapshots.rs` integration tests can call
/// `render::draw` via `TestBackend`. Do not depend on it from
/// external crates.
#[doc(hidden)]
pub mod render;
pub mod sanitize;
pub mod theme;
pub mod toast;

use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

// ── Run configuration ───────────────────────────────────────────────

/// Selects which backend the TUI should drive.
#[derive(Debug, Clone)]
pub enum BackendMode {
    /// Spawn the real `prism backend` subprocess.
    Real {
        prism_binary: String,
        project_root: String,
        python_bin: String,
    },
    /// Use a deterministic fake backend (no subprocess, no network).
    Fake { scenario: backend::FakeScenario },
}

/// Platform base URL (`…/api/v1`) + bearer token for cheap billing/credits
/// polling at turn boundaries. Absent for offline / fake backends, in which
/// case no credits are shown.
#[derive(Debug, Clone)]
pub struct PlatformAuth {
    pub base_url: String,
    pub token: String,
}

/// Configuration for [`run_with_config`].
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub backend_mode: BackendMode,
    /// When set, the status bar polls the org credit balance.
    pub platform: Option<PlatformAuth>,
    /// Launch-time conversation resume. `None` = start fresh; `Some("")` =
    /// open the session picker (`prism resume`); `Some(id)` = jump straight
    /// into that conversation (`prism resume <id>`).
    pub resume: Option<String>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            backend_mode: BackendMode::Real {
                prism_binary: "prism".into(),
                project_root: ".".into(),
                python_bin: "python3".into(),
            },
            platform: None,
            resume: None,
        }
    }
}

/// Entry point — called by `prism` CLI (bare `prism` or `prism tui`).
///
/// Preserved for backward compatibility.  Delegates to
/// [`run_with_config`] with a real backend mode.
pub async fn run(prism_binary: &str, project_root: &str, python_bin: &str) -> Result<()> {
    let config = RunConfig {
        backend_mode: BackendMode::Real {
            prism_binary: prism_binary.to_string(),
            project_root: project_root.to_string(),
            python_bin: python_bin.to_string(),
        },
        platform: None,
        resume: None,
    };
    run_with_config(config).await
}

/// Entry point with explicit backend mode.  Used by
/// `prism tui --fake-backend --scenario <name>`.
pub async fn run_with_config(config: RunConfig) -> Result<()> {
    // Check that we're running in a real terminal — raw mode requires a TTY.
    if !std::io::IsTerminal::is_terminal(&io::stdin()) {
        anyhow::bail!(
            "PRISM TUI requires a real terminal (TTY).\n\
             You're running in a pipe or non-interactive shell.\n\
             Try running `prism` directly in your terminal,\n\
             or use `prism backend` for the JSON-RPC protocol."
        );
    }

    // Panic hook: restore terminal on crash so we don't brick the user's terminal.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
        original_hook(info);
    }));

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, Hide)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    // Terminal::new() queries the cursor position via `\x1b[6n` (DSR).
    // Some PTY environments (pexpect, CI runners, non-interactive pipes)
    // never respond to this query, causing a timeout.  Fall back to a
    // fixed viewport so the TUI can still render in those environments.
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            // Restore terminal before bailing so we don't leave the user
            // in raw mode / alt screen.
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                LeaveAlternateScreen,
                DisableMouseCapture,
                Show
            );
            anyhow::bail!(
                "Failed to initialise terminal: {e}.\n\
                 This can happen in non-interactive shells or PTY environments\n\
                 that don't respond to cursor-position queries. Try running\n\
                 `prism` in a real terminal."
            );
        }
    };
    // NOTE: we deliberately skip `terminal.clear()` here.  Ratatui's
    // `clear()` calls `backend.get_cursor_position()` which sends a
    // `\x1b[6n` DSR query to the terminal.  Some PTY environments
    // (pexpect, certain CI runners) never respond, causing a timeout.
    // The first `terminal.draw()` will render the full frame anyway,
    // so the explicit clear is unnecessary.

    // Spawn backend — real subprocess or fake deterministic player.
    let mut backend_handle = match &config.backend_mode {
        BackendMode::Real {
            prism_binary,
            project_root,
            python_bin,
        } => backend::BackendHandle::spawn(prism_binary, project_root, python_bin)?,
        BackendMode::Fake { scenario } => backend::BackendHandle::fake(*scenario),
    };
    backend_handle.init().await?;

    // Build app state
    let mut app = app::App::new(backend_handle);

    // Launch-time resume (`prism resume` / `prism resume <id>`): an empty
    // string opens the session picker; a concrete id jumps straight into
    // that conversation. Both reuse the same `/sessions` + `/resume`
    // backend commands the in-TUI picker already drives.
    match config.resume.as_deref() {
        Some("") => app.open_sessions(),
        Some(id) => app.resume_session(id),
        None => {}
    }

    // Credits polling: a spawned task fetches the org balance and publishes it
    // via an atomic the loop reads (no extra select! branch needed). Fired at
    // startup and after each completed turn. Stays None when unauthed.
    let credits_cell = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN));
    let platform_auth = config.platform.clone();

    // Main event loop — tokio::select! between crossterm events,
    // agent messages, and a periodic render tick (100ms) for animations.
    //
    // SIGINT handling: we install a raw SIGINT handler that sets an
    // atomic flag.  The loop checks this flag on every iteration (at
    // least every 100ms via the tick).  This is more reliable than
    // tokio::signal::ctrl_c() which can be starved by the select!
    // in release mode.
    use crossterm::event::{Event, EventStream};
    use futures::StreamExt;
    use tokio::time::{Duration, interval};
    let mut events = EventStream::new();
    let mut tick = interval(Duration::from_millis(100));

    // Install SIGINT handler — sets a flag the loop checks each iteration.
    // On Unix, we use a raw libc::sigaction handler for reliable signal
    // delivery in raw terminal mode.  On non-Unix (Windows), we fall
    // back to tokio::signal::ctrl_c() which is polled in the select!
    // loop below.  The raw handler is preferred on Unix because it
    // cannot be starved by the select! branch ordering.
    static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

    #[cfg(unix)]
    {
        extern "C" fn handle_sigint(_: i32) {
            SIGINT_RECEIVED.store(true, Ordering::SeqCst);
        }
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigint as *const () as usize;
            sa.sa_flags = 0;
            libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        }
    }

    // On non-Unix (Windows), ctrl_c is polled inline in the select! below.
    // We call tokio::signal::ctrl_c() *fresh inside* the select! rather than
    // storing the future and polling `&mut fut` across iterations: that future
    // is !Unpin on Windows, so `&mut fut` fails to satisfy select!'s Future
    // bound (E0277) and breaks the Windows build. Calling it inline lets the
    // macro pin the temporary internally — the same pattern forge_main uses.

    // Tracks the terminal's mouse-capture state so copy mode can toggle it.
    let mut mouse_captured = true;
    loop {
        // Render every frame
        terminal.draw(|f| render::draw(f, &app))?;

        // Credits: (re)fetch at startup and after each completed turn, then
        // publish the latest known balance into the app for the status bar.
        if app.needs_credits_refresh {
            app.needs_credits_refresh = false;
            if let Some(auth) = &platform_auth {
                let (base, token, cell) = (
                    auth.base_url.clone(),
                    auth.token.clone(),
                    credits_cell.clone(),
                );
                tokio::spawn(async move {
                    let client = prism_client::PlatformClient::new(base).with_token(token);
                    match prism_client::billing::get_balance(&client).await {
                        Ok(b) => {
                            cell.store(b.balance_millicredits, std::sync::atomic::Ordering::SeqCst);
                        }
                        Err(e) => tracing::debug!(error = %e, "credits fetch failed"),
                    }
                });
            }
        }
        let fetched = credits_cell.load(std::sync::atomic::Ordering::SeqCst);
        if fetched != i64::MIN {
            app.credits = Some(fetched);
        }

        // Check quit conditions: should_quit (from key handler) or
        // SIGINT received (from the signal handler).
        if app.should_quit || SIGINT_RECEIVED.load(Ordering::SeqCst) {
            break;
        }

        // Select between key events, agent messages, and render ticks.
        // A 200ms timeout ensures the loop checks SIGINT at least
        // 5 times per second even if all other branches are idle.
        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = tick.tick() => { app.prune_toasts(); }
                Some(Ok(ev)) = events.next() => {
                    match ev {
                        Event::Key(key) => app.handle_key(key),
                        Event::Mouse(m) => app.handle_mouse(m),
                        _ => {}
                    }
                }
                Some(msg) = app.backend.recv() => {
                    app.handle_backend_message(&msg);
                }
                _ = tokio::signal::ctrl_c() => {
                    SIGINT_RECEIVED.store(true, Ordering::SeqCst);
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
        #[cfg(unix)]
        {
            tokio::select! {
                // Render tick — fires every 100ms for animations + toast expiry.
                _ = tick.tick() => { app.prune_toasts(); }
                // Terminal events (keyboard, resize, etc.)
                Some(Ok(ev)) = events.next() => {
                    match ev {
                        Event::Key(key) => app.handle_key(key),
                        Event::Mouse(m) => app.handle_mouse(m),
                        _ => {}
                    }
                }
                // Agent backend messages
                Some(msg) = app.backend.recv() => {
                    app.handle_backend_message(&msg);
                }
                // Fallback timeout — ensures SIGINT flag is checked
                // even if tick and events are both stalled.
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }

        // Reconcile terminal mouse capture with copy mode: capture ON blocks
        // the terminal's native drag-to-select, so copy mode turns it off and
        // exiting restores it. Done here because the loop owns stdout.
        if app.copy_mode == mouse_captured {
            let _ = if app.copy_mode {
                execute!(io::stdout(), DisableMouseCapture)
            } else {
                execute!(io::stdout(), EnableMouseCapture)
            };
            mouse_captured = !app.copy_mode;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    )?;

    Ok(())
}
