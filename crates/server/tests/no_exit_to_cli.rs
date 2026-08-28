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
///
/// "with: prism " joined the list after a live TUI transcript showed "(the
/// local semantic index is empty — ingest data first with: prism ingest
/// <path>)" — an instruction to exit and type a command, phrased without
/// "run" and therefore invisible to the original needles. The backtick idiom
/// ("with `prism X`", "use `prism X`") is deliberately NOT banned here: it is
/// the CLI's standing remediation style at ~25 call sites, and widening the
/// needle would turn this guard into a rewrite mandate rather than a
/// regression tripwire. That residue is a policy decision, not an oversight.
fn is_instruction(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    [
        "run `prism ",
        "runs `prism ",
        "run: prism ",
        "run prism ",
        "with: prism ",
    ]
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

/// Splice `\`-continued string literals back into one logical line.
///
/// Rust's `\`-before-newline continuation drops the newline and the next
/// line's leading whitespace, so the SOURCE never shows the phrase the USER
/// reads. The escaped live string was exactly that: `"…ingest data first
/// with: \` / `prism ingest <path>)"` splits between "with: " and "prism",
/// so a physical-line scan could not match ANY needle across the seam — the
/// guard was green while the TUI showed the violation verbatim. Comments are
/// not joined (a trailing `\` in a comment is text, not a continuation), and
/// a line ending in `\\` ends in an escaped backslash, so only an odd run of
/// trailing backslashes continues. Reported line numbers are the logical
/// line's first physical line.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (idx, raw) in text.lines().enumerate() {
        let (start, line) = match pending.take() {
            Some((start, prefix)) => (start, prefix + raw.trim_start()),
            None => (idx + 1, raw.to_string()),
        };
        let trimmed = line.trim_end();
        let trailing_backslashes = trimmed.chars().rev().take_while(|&c| c == '\\').count();
        if trailing_backslashes % 2 == 1 && !is_comment(&line) {
            pending = Some((start, trimmed[..trimmed.len() - 1].to_string()));
        } else {
            out.push((start, line));
        }
    }
    if let Some(unterminated) = pending {
        out.push(unterminated);
    }
    out
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
            for (lineno, line) in logical_lines(&text) {
                if is_comment(&line) || is_guard_assertion(&line) {
                    continue;
                }
                // Only string literals reach a user.
                if line.contains('"') && is_instruction(&line) {
                    violations.push(format!("{}:{}: {}", file.display(), lineno, line.trim()));
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

/// The live escape, byte for byte: `cli/src/main.rs` shipped this literal and
/// the guard stayed green, because "with: " and "prism" only meet after the
/// continuation splice AND no needle covered the "with: prism" phrasing. The
/// repo scan above passes vacuously once sources are clean, so this pins both
/// mechanisms against regressing independently of the tree's current state.
#[test]
fn continuation_split_instruction_is_caught() {
    let src = "println!(\n    \"  (the local semantic index is empty — ingest data first with: \\\n     prism ingest <path>)\"\n);\n";
    let lines = logical_lines(src);
    let hit = lines
        .iter()
        .find(|(_, line)| line.contains('"') && is_instruction(line));
    assert_eq!(
        hit.map(|(lineno, _)| *lineno),
        Some(2),
        "the spliced literal must match and report its first physical line: {lines:?}"
    );
}

/// A trailing `\` in a comment is text, not a continuation — joining there
/// would splice the NEXT code line into a skipped comment and hide it.
#[test]
fn comment_backslash_does_not_swallow_the_next_line() {
    let src = "// quoted evidence ends with: \\\nlet x = \"run prism doctor now\";\n";
    let lines = logical_lines(src);
    assert!(
        lines
            .iter()
            .any(|(_, line)| !is_comment(line) && line.contains('"') && is_instruction(line)),
        "the code line after a backslash-terminated comment must still be scanned: {lines:?}"
    );
}
