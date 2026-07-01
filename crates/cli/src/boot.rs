//! PRISM boot sequence — inline ANSI checklist printed before the chat UI.
//!
//! Pure stdout, no ratatui / crossterm. Extracted from the deprecated
//! `tui` module so we can delete the rest of that code without losing the
//! branded startup checklist users see at the top of every `prism tui`.
//!
//! Also provides the "section" / "status_line" helpers so every PRISM-side
//! piece of UI (model download, tool router boot, MCP wire-up) renders in
//! the same visual language as the boot checklist instead of dumping raw
//! stderr that breaks the eye's flow.

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

/// A single boot check result.
pub struct BootCheck {
    pub name: String,
    pub result: String,
    pub ok: bool,
    pub dots: u32,
    pub delay_ms: u64,
}

/// Run the boot sequence (raw stdout, ANSI colors only).
///
/// Prints the "Initializing..." banner, every check line, and the
/// "Igniting core..." trailer. Used at `prism` startup.
pub fn boot_sequence(checks: &[BootCheck]) {
    let print_colored = |text: &str| {
        print!("{text}");
        let _ = io::stdout().flush();
    };

    print_colored(
        "\x1b[38;2;0;255;255m[PRISM]\x1b[0m \x1b[38;2;200;200;200mInitializing Materials Discovery Node...\x1b[0m\n",
    );
    thread::sleep(Duration::from_millis(200));

    print_check_lines(checks);

    print_colored(
        "\n\x1b[38;2;0;255;255m[PRISM]\x1b[0m \x1b[38;2;200;200;200mIgniting core...\x1b[0m\n",
    );
    thread::sleep(Duration::from_millis(300));
}

/// Print just the check lines without the "Initializing..." / "Igniting core..."
/// frame. Used by `prism doctor` so each section under its own [PRISM] header
/// reads as one continuous tree instead of repeating the boot banner.
pub fn print_check_lines(checks: &[BootCheck]) {
    let print_colored = |text: &str| {
        print!("{text}");
        let _ = io::stdout().flush();
    };

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
}

// ── Shared styling for section-style output ──────────────────────────

/// One styled status line in the same visual language as `boot_sequence`.
///
/// Status: ok=true → green [OK], ok=false → red [--], pending=Some(label) →
/// cyan label (e.g. `[..]`). Detail is shown dimmed in parens at the end.
pub fn status_line(name: &str, ok: bool, detail: &str) {
    let marker = if ok {
        "\x1b[38;2;0;255;0m[OK]\x1b[0m"
    } else {
        "\x1b[38;2;255;80;80m[--]\x1b[0m"
    };
    println!(
        "\x1b[38;2;100;100;255m \u{251c}\u{2500}\u{2500} \x1b[38;2;255;255;255m{name} \x1b[38;2;100;100;100m\u{2026}\u{2026}\u{2026}\u{2026}\u{2026}\x1b[0m {marker} \x1b[38;2;100;100;100m({detail})\x1b[0m"
    );
}

/// "Pending" status — something is in progress. Returns a closure that, when
/// called with (ok, final_detail), updates the line in place. Falls back to
/// a fresh line on terminals that don't support carriage-return overwrites.
pub fn section(title: &str) {
    println!("\n\x1b[38;2;0;255;255m[PRISM]\x1b[0m \x1b[38;2;200;200;200m{title}\x1b[0m");
}

/// Print a single bullet without a status marker — for plain progress text.
#[allow(dead_code)]
pub fn bullet(text: &str) {
    println!("\x1b[38;2;100;100;255m \u{251c}\u{2500}\u{2500} \x1b[38;2;200;200;200m{text}\x1b[0m");
}

/// A nicely-coloured warning the user should notice but isn't fatal.
pub fn warn(text: &str) {
    println!("\x1b[38;2;100;100;255m \u{2502}   \x1b[38;2;255;200;100m\u{26A0}  {text}\x1b[0m");
}
