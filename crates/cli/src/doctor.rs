//! `prism doctor` — diagnostic snapshot of everything PRISM needs to run.
//!
//! Lists each runtime dependency with [OK] / [--] / [!] markers. Designed
//! to be the first thing a user runs when something feels off — gives them
//! a single screen of what's healthy and what's missing.

use std::path::PathBuf;

use anyhow::Result;

use crate::boot::{BootCheck, boot_sequence};

pub async fn run(project_root: &std::path::Path, python_bin: &std::path::Path) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let prism_dir = PathBuf::from(&home).join(".prism");
    let mut checks: Vec<BootCheck> = Vec::new();

    // 1. llama-server (homebrew or PATH)
    checks.push(check_binary(
        "llama-server",
        &[
            "/opt/homebrew/bin/llama-server",
            "/usr/local/bin/llama-server",
        ],
    ));

    // 2. Embedder GGUF
    let embed_gguf = prism_dir.join("models/embeddinggemma-300m.gguf");
    checks.push(check_file(
        "EmbeddingGemma model",
        &embed_gguf,
        "auto-downloads on first `prism tui`",
    ));

    // 3. Function GGUF (optional)
    let fn_gguf = prism_dir.join("models/functiongemma-270m.gguf");
    checks.push(check_file(
        "FunctionGemma model",
        &fn_gguf,
        "auto-downloads on first `prism tui` — local routing disabled without it",
    ));

    // 4. Python venv (used by prism-python MCP server)
    let venv_python = prism_dir.join("venv/bin/python3");
    checks.push(check_file(
        "PRISM Python venv",
        &venv_python,
        "run `prism setup` to provision",
    ));

    // 5. PRISM credentials (auth state) — newer prism uses cli-state.json,
    //    older builds wrote credentials.json. Either is fine.
    let cli_state = prism_dir.join("cli-state.json");
    let credentials_json = prism_dir.join("credentials.json");
    let creds_path = if cli_state.exists() {
        cli_state.clone()
    } else {
        credentials_json.clone()
    };
    checks.push(check_file(
        "PRISM credentials",
        &creds_path,
        "run `prism login`",
    ));

    // 6. Forge MCP config
    let forge_mcp = PathBuf::from(&home).join(".forge/.mcp.json");
    checks.push(check_file(
        "forge MCP config",
        &forge_mcp,
        "auto-generated on first `prism tui`",
    ));

    // 7. Tool router index (rebuilt automatically; informational)
    let index_dir = prism_dir.join("tool_router/index/catalog.jsonl");
    checks.push(BootCheck {
        name: "Tool router index".to_string(),
        result: if index_dir.exists() {
            format!("cached at {}", index_dir.display())
        } else {
            "no cache yet (built on first chat)".to_string()
        },
        ok: index_dir.exists(),
        dots: 4,
        delay_ms: 0,
    });

    // 8. Project root sanity (where prism is being run from)
    checks.push(BootCheck {
        name: "Project root".to_string(),
        result: project_root.display().to_string(),
        ok: project_root.exists(),
        dots: 2,
        delay_ms: 0,
    });

    // 9. Python bin resolved
    checks.push(BootCheck {
        name: "Python interpreter".to_string(),
        result: python_bin.display().to_string(),
        ok: python_bin.exists() || python_bin.as_os_str() == "python3",
        dots: 2,
        delay_ms: 0,
    });

    boot_sequence(&checks);

    println!();
    println!("Anything marked [--] means: not yet present, but PRISM will set it up");
    println!("on demand or via the documented one-liner.");
    println!();
    println!("If chat is misbehaving in unexpected ways, also try:");
    println!("  rm -rf ~/.prism/tool_router && prism tui    # rebuilds tool index");
    println!("  rm  ~/.forge/.mcp.json && prism tui          # rewrites MCP config");
    Ok(())
}

fn check_binary(name: &str, candidates: &[&str]) -> BootCheck {
    for c in candidates {
        if std::path::Path::new(c).exists() {
            return BootCheck {
                name: name.to_string(),
                result: c.to_string(),
                ok: true,
                dots: 4,
                delay_ms: 0,
            };
        }
    }
    BootCheck {
        name: name.to_string(),
        result: "missing — install via `brew install llama.cpp`".to_string(),
        ok: false,
        dots: 4,
        delay_ms: 0,
    }
}

fn check_file(name: &str, path: &std::path::Path, hint_if_missing: &str) -> BootCheck {
    if path.exists() {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let result = if size > 1_000_000 {
            format!("{} ({} MB)", path.display(), size / 1_048_576)
        } else {
            path.display().to_string()
        };
        BootCheck {
            name: name.to_string(),
            result,
            ok: true,
            dots: 4,
            delay_ms: 0,
        }
    } else {
        BootCheck {
            name: name.to_string(),
            result: hint_if_missing.to_string(),
            ok: false,
            dots: 4,
            delay_ms: 0,
        }
    }
}
