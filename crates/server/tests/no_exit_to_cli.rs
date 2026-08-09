//! Guard: no user-facing message may instruct a human to leave and run a command.
//!
//! The owner's rule, stated precisely: *showing* a CLI error in the UI or TUI is
//! fine and useful — the defect is when the **remedy** requires exiting. So this
//! test bans the imperative form ("run `prism …`"), not every mention of the
//! binary. Doc comments are exempt: they are read by developers, not users. Tool
//! *descriptions* aimed at the agent are also legitimate — the agent invokes
//! those itself; only strings a human reads are in scope, which is why this scans
//! crates that render to people rather than the agent's tool catalog.
//!
//! Added after a commit message claimed a guard existed here when it only existed
//! in the separate `prism-app` repo, and an adversarial reviewer caught three
//! live user-facing violations that claim had implied were gone.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

use std::fs;
use std::path::{Path, PathBuf};

/// Crates whose string literals reach a human directly.
const HUMAN_FACING_CRATES: &[&str] = &["server", "cli", "tui", "mesh"];

/// The imperative form we ban. A bare mention ("obtained via the device flow")
/// is fine; telling the reader to go and type something is not.
fn is_instruction(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    ["run `prism ", "runs `prism ", "run: prism ", "run prism "]
        .iter()
        .any(|needle| lowered.contains(needle))
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("*") || t.starts_with("#!")
}

/// A line asserting *against* the pattern is the guard, not a violation.
fn is_guard_assertion(line: &str) -> bool {
    line.contains("!body.contains") || line.contains("assert") || line.contains("is_instruction")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_user_facing_string_tells_a_human_to_run_a_cli_command() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .to_path_buf();

    let mut violations = Vec::new();
    for name in HUMAN_FACING_CRATES {
        let src = crates_dir.join(name).join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_sources(&src, &mut files);
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            for (idx, line) in text.lines().enumerate() {
                if is_comment(line) || is_guard_assertion(line) {
                    continue;
                }
                // Only string literals reach a user.
                if line.contains('"') && is_instruction(line) {
                    violations.push(format!("{}:{}: {}", file.display(), idx + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "user-facing strings must state the fact, never instruct the reader to exit and run a \
         command. The remedy belongs in the surface the user is already in.\n{}",
        violations.join("\n")
    );
}
