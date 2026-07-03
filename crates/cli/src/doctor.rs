//! `prism doctor` — diagnostic snapshot of everything PRISM needs to run.
//!
//! Runs in two clearly-labeled sections:
//!
//! 1. **Local Setup** — binaries, files, project root, python interpreter.
//!    The "is your laptop ready?" pass.
//! 2. **Platform Connectivity** — auth, KG, models, compute, marketplace,
//!    local node, policy engine. The same checks `prism` runs on startup,
//!    so a green doctor means a green boot.
//!
//! Lists each check with [OK] / [--] markers. Designed to be the first
//! thing a user runs when something feels off — single screen, full picture.

use std::path::PathBuf;

use anyhow::Result;
use prism_runtime::{PlatformEndpoints, PrismPaths};

use crate::boot::{self, BootCheck, print_check_lines};
use crate::boot_checks;

pub async fn run(project_root: &std::path::Path, python_bin: &std::path::Path) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let prism_dir = PathBuf::from(&home).join(".prism");

    // ── Section 1: local setup ────────────────────────────────────────
    boot::section("Local Setup");

    let mut checks: Vec<BootCheck> = Vec::new();

    // 1. llama-server (homebrew or PATH)
    checks.push(check_binary(
        "llama-server",
        &[
            "/opt/homebrew/bin/llama-server",
            "/usr/local/bin/llama-server",
        ],
    ));

    // 1b. Check if llama-server is running (async check)
    // We use a simple TCP connect instead of reqwest to avoid async/blocking mismatch.
    if std::net::TcpStream::connect("127.0.0.1:8081").is_ok() {
        checks.push(BootCheck {
            name: "llama-server running".into(),
            result: "OK".into(),
            ok: true,
            dots: 0,
            delay_ms: 0,
        });
    }

    // 2. Embedder GGUF
    let embed_gguf = prism_dir.join("models/embeddinggemma-300m.gguf");
    checks.push(check_file(
        "EmbeddingGemma model",
        &embed_gguf,
        "auto-downloads on first `prism`",
    ));

    // 3. FunctionGemma model — DEPRECATED. The Stage 2.2 local-routing
    //    path was removed because it caused silent failures (it picked a
    //    tool the chat LLM never got to summarise). The doctor still
    //    reports the file's presence as informational so users with a
    //    cached copy don't get a confusing "missing" warning, but the
    //    model is no longer required and no longer downloaded.
    let fn_gguf = prism_dir.join("models/functiongemma-270m.gguf");
    if fn_gguf.exists() {
        checks.push(check_file(
            "FunctionGemma model",
            &fn_gguf,
            "deprecated — kept on disk but no longer used by PRISM",
        ));
    }

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
        "auto-generated on first `prism`",
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

    print_check_lines(&checks);

    // ── Section 2: platform connectivity (same checks as `prism` boot) ─
    boot::section("Platform Connectivity");

    let paths = PrismPaths::discover()?;
    let state = paths.load_cli_state().unwrap_or_default();
    let endpoints = PlatformEndpoints::from_env();
    let platform_checks =
        boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
    print_check_lines(&platform_checks);

    println!();
    println!("Anything marked [--] means: not yet present, but PRISM will set it up");
    println!("on demand or via the documented one-liner.");
    println!();
    println!("If chat is misbehaving in unexpected ways, also try:");
    println!("  rm -rf ~/.prism/tool_router && prism    # rebuilds tool index");
    println!("  rm  ~/.forge/.mcp.json && prism          # rewrites MCP config");
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
