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
//!
//! With `--fix` a third section runs: everything mechanically repairable gets
//! repaired, everything else prints the exact command to run. The one rule in
//! that section is that a row goes green **only** when the original check is
//! re-run and passes — see [`settle`]. Doctor has a history of reporting on
//! files nothing ever writes; `--fix` must not extend it by claiming work it
//! did not do.

use std::path::{Path, PathBuf};

use anyhow::Result;
use prism_core::chat_config;
use prism_runtime::{PlatformEndpoints, PrismPaths};

use crate::boot::{self, BootCheck, print_check_lines};
use crate::boot_checks;

pub async fn run(project_root: &Path, python_bin: &Path, fix: bool) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let prism_dir = PathBuf::from(&home).join(".prism");

    // ── Section 1: local setup ────────────────────────────────────────
    boot::section("Local Setup");

    let mut checks: Vec<BootCheck> = Vec::new();

    // 1. llama-server — OPTIONAL. Nothing in PRISM spawns it; it only
    //    matters if the user has pointed chat at a local server with
    //    `prism use local`. Reporting it as a plain missing dependency told
    //    every fresh install to go and `brew install llama.cpp` for no
    //    reason.
    checks.push(check_binary(
        "llama-server (optional)",
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

    // 1c. WHICH MODEL ANSWERS, and whether it bills.
    //
    // Neither `doctor` nor `status` reported this, and the default target is
    // the PAID platform (`ChatTarget::default()` is `Marc27`). So a
    // `config.toml` that is missing, empty, or unparseable silently moves
    // chat off the local server the user set up and onto billed hosting,
    // with every other check still reading green. Measured on a live
    // machine: the file was truncated to zero bytes, `llama-server running`
    // said OK beside it, and nothing anywhere said the next question would
    // be billed.
    //
    // A local server that is configured but DOWN is the same class of
    // problem from the other end, so the row reports reachability too.
    checks.push(chat_route_check());

    // 2. Embedding model. This used to look for
    //    `models/embeddinggemma-300m.gguf` and claim it "auto-downloads on
    //    first `prism`" — nothing in the tree has ever written that file, so
    //    the row was permanently red with a hint that was simply untrue.
    //    The model PRISM actually uses is the pinned BGE-small-en-v1.5
    //    snapshot under `models/embed/`. Normal inference never downloads it.
    let embed_dir = prism_embed::default_cache_dir()?;
    let embed_status = prism_embed::configured_embedding_status();
    checks.push(embedding_check(&embed_status, &embed_dir));

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

    // 4. Python venv (used by prism-python MCP server). A venv directory
    //    alone proves nothing, and neither does `import app` — [OK] means
    //    every distribution declared in `[project] dependencies` is actually
    //    installed. (Fresh boxes used to get an empty venv that a bare
    //    exists() check happily blessed; later, a venv with no `python-ulid`
    //    passed the `import app` check while the MACE tool code could not be
    //    imported at all.)
    //    Path comes from the same helper `ensure_venv` uses, so this cannot
    //    drift from where the venv is actually created (Windows puts the
    //    interpreter in `Scripts\python.exe`, not `bin/python3`).
    let (venv_python, _) = prism_python_bridge::venv::venv_layout(&prism_dir.join("venv"));
    checks.push(check_venv_tools(&venv_python));

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
        "not authenticated",
    ));

    // 6. (removed) "forge MCP config" checked `~/.forge/.mcp.json` and, when
    //    absent, claimed it was "auto-generated on first `prism`". Nothing in
    //    the tree has ever written that path — it was read here and named in
    //    the footer hint below, and nowhere else — so the row was red on
    //    every machine forever, pointing at a file that does not exist and a
    //    self-heal that never happens. PRISM's own MCP client config is
    //    `~/.prism/mcp.json` (see `prism_agent::mcp`), where *missing* is a
    //    documented, working state meaning "no MCP servers", not a fault.
    //    Same class of defect as items 2 and 7; removed rather than restated.

    // 7. (removed) "Tool router index" checked
    //    `~/.prism/tool_router/index/catalog.jsonl`. Nothing in the codebase
    //    writes that path — it was read here and mentioned in the hint text
    //    below, and nowhere else — so the row was red on every machine
    //    forever and told users a cache would appear "on first chat" that
    //    never does. Tool selection works off the in-memory catalog built by
    //    `ToolCatalog` each run; there is no on-disk index to report.

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
    let config = prism_core::config::NodeConfig::load(Some(project_root));
    let endpoints = PlatformEndpoints::resolve_for_paths(
        config.platform.url.as_deref(),
        config.platform.provider.as_deref(),
        state.credentials.as_ref(),
        &paths,
    );
    let node_token = paths.load_node_token();
    let platform_checks = boot_checks::run_boot_checks_with_node_token(
        state.credentials.as_ref(),
        endpoints.as_ref(),
        node_token.as_ref(),
    )
    .await;
    print_check_lines(&platform_checks);

    // ── Section 3: repairs (`--fix` only) ─────────────────────────────
    if fix {
        boot::section("Repairs");
        let mut repairs = vec![
            fix_venv(&prism_dir, project_root).await,
            fix_embedding_model(&embed_dir).await,
        ];
        repairs.extend(manual_only(creds_path.exists(), &platform_checks));
        print_check_lines(&repairs);
        println!();
        println!("[OK] rows above were re-checked after the repair — nothing is");
        println!("reported as fixed unless the original check now passes.");
        return Ok(());
    }

    println!();
    println!("[--] means not present yet. Each row above says whether that is");
    println!("something PRISM fills in on demand or something you need to do.");
    if !creds_path.exists() {
        println!();
        println!("Next step:  authenticate this node");
    }
    println!();
    println!("To repair what can be repaired automatically:");
    println!("  prism doctor --fix");
    Ok(())
}

// ── Repairs ───────────────────────────────────────────────────────────

/// Command we print when the venv cannot be rebuilt here.
const VENV_MANUAL: &str = "install Python 3.11+ (macOS: `brew install python@3.12`, \
     Debian/Ubuntu: `sudo apt-get install -y python3-venv`), then repair again";

/// Turn a repair attempt into a row whose verdict comes from **re-running the
/// check**, never from what the repair claims about itself.
///
/// `attempted` is what the repair did (or why it gave up); `verify` is the
/// same predicate the diagnostic section used. `ok` is `verify()` and nothing
/// else — that is the whole point of this function.
fn settle(
    name: &str,
    attempted: Result<String, String>,
    verify: impl Fn() -> bool,
    manual: &str,
) -> BootCheck {
    let healthy = verify();
    let result = match (&attempted, healthy) {
        (Ok(note), true) => note.clone(),
        (Err(why), true) => format!("healthy, though the repair reported: {why}"),
        (Ok(note), false) => format!("{note}, but the check still fails — {manual}"),
        (Err(why), false) => format!("could not repair: {why} — {manual}"),
    };
    BootCheck {
        name: name.to_string(),
        result,
        ok: healthy,
        dots: 4,
        delay_ms: 0,
    }
}

/// Rebuild the managed Python venv.
///
/// `ensure_venv` self-heals a venv that is merely missing its tools or a
/// declared dependency, but it never deletes one it cannot repair in place —
/// the documented escape hatch is `rm -rf ~/.prism/venv`, which users are
/// expected to know. `--fix` does that second pass for them, then proves the
/// result by re-running the completeness check rather than trusting
/// `ensure_venv`'s return value.
async fn fix_venv(prism_dir: &Path, project_root: &Path) -> BootCheck {
    let venv_dir = prism_dir.join("venv");
    let (venv_python, _) = prism_python_bridge::venv::venv_layout(&venv_dir);
    let complete = || prism_python_bridge::venv::missing_requirements(&venv_python).is_empty();

    if complete() {
        return settle(
            "PRISM Python venv",
            Ok("already healthy".to_string()),
            complete,
            VENV_MANUAL,
        );
    }

    let mut attempted = provision_venv(prism_dir, project_root).await;
    if !complete() && venv_dir.exists() {
        // `remove_dir_all` does not follow symlinks — a `~/.prism/venv`
        // symlinked onto another disk loses the link, not the target's
        // contents. Nothing outside `~/.prism/venv` is ever touched.
        match std::fs::remove_dir_all(&venv_dir) {
            Ok(()) => {
                attempted = provision_venv(prism_dir, project_root)
                    .await
                    .map(|note| format!("removed the unusable venv, then {note}"))
                    .map_err(|why| format!("removed the unusable venv, but {why}"));
            }
            Err(err) => {
                attempted = Err(format!("cannot remove {}: {err}", venv_dir.display()));
            }
        }
    }
    settle("PRISM Python venv", attempted, complete, VENV_MANUAL)
}

async fn provision_venv(prism_dir: &Path, project_root: &Path) -> Result<String, String> {
    prism_python_bridge::venv::ensure_venv(prism_dir, project_root)
        .await
        .map(|_| "provisioned".to_string())
        .map_err(|err| err.to_string())
}

/// Hint for the case where no embedding backend can be built at all.
const EMBED_MANUAL: &str = "explicit acquisition: `prism models install bge-small-en-v1.5`; \
     hosted alternative: PRISM_EMBED_BACKEND=openai with PRISM_EMBED_ENDPOINT_URL";

/// Re-check the configured embedding backend without acquiring model files.
///
/// A hosted (OpenAI-compatible) embedder needs no local weights at all. That
/// case gets its own verdict rather than being pushed through the cache
/// check, which would otherwise print the self-contradiction "no local model
/// is needed" next to a red mark — the exact species of nonsense row this
/// file has spent three deletions getting rid of.
async fn fix_embedding_model(embed_dir: &Path) -> BootCheck {
    match tokio::task::spawn_blocking(prism_embed::configured_embedding_status).await {
        Ok(status) => embedding_check(&status, embed_dir),
        Err(err) => BootCheck {
            name: "Embedding backend".to_string(),
            result: format!("embedding status task failed: {err} — {EMBED_MANUAL}"),
            ok: false,
            dots: 4,
            delay_ms: 0,
        },
    }
}

fn embedding_check(
    status: &prism_embed::EmbeddingConfigurationStatus,
    embed_dir: &Path,
) -> BootCheck {
    use prism_embed::{ConfiguredBackendStatus, NativeModelStatus};

    let snapshot = match &status.native_snapshot {
        NativeModelStatus::Ready { snapshot_dir, .. } => {
            format!(
                "local snapshot integrity verified at {}",
                snapshot_dir.display()
            )
        }
        NativeModelStatus::Unavailable(reason) => {
            format!("local snapshot integrity unavailable ({:?})", reason.code)
        }
    };
    let (ok, result) = match &status.backend {
        ConfiguredBackendStatus::NativeSupported => match &status.native_snapshot {
            NativeModelStatus::Ready { .. } => (true, format!("native configured; {snapshot}")),
            NativeModelStatus::Unavailable(reason) => (
                false,
                format!(
                    "native configured but {snapshot} — {reason}; cache: {}",
                    embed_dir.display()
                ),
            ),
        },
        ConfiguredBackendStatus::NativeUnsupported { reason } => (
            false,
            format!("native configured but platform unusable: {reason}; {snapshot}"),
        ),
        ConfiguredBackendStatus::HostedReady { backend_id } => (
            true,
            format!(
                "hosted configured ({backend_id}); {snapshot} (not required by hosted backend)"
            ),
        ),
        ConfiguredBackendStatus::HostedUnavailable { reason } => (
            false,
            format!("hosted configured but unusable: {reason}; {snapshot} (not selected)"),
        ),
        ConfiguredBackendStatus::Disabled => (
            true,
            format!("disabled by configuration; {snapshot} (not required)"),
        ),
    };
    BootCheck {
        name: "Embedding backend".to_string(),
        result,
        ok,
        dots: 4,
        delay_ms: 0,
    }
}

/// Platform rows worth repeating under Repairs.
///
/// Only the two that actually gate PRISM. Everything else in the boot-check
/// list is downstream of them (Knowledge Graph / Models / Compute /
/// Marketplace all read "unavailable" when auth is missing, so repeating them
/// is noise) or describes an opt-in feature — "Local Node offline" is the
/// normal state for anyone who has not run `prism node up`, and listing it
/// under Repairs frames a feature nobody has to use as an unsolved problem.
/// Same reasoning that already keeps `llama-server` labelled "(optional)".
const REPAIR_RELEVANT: &[&str] = &["Platform", "Auth"];

/// Rows for the things `--fix` deliberately does not touch, each carrying the
/// exact command. Credentials are supplied outside this diagnostic; platform
/// reachability needs the platform. Pretending otherwise is the failure mode
/// this section exists to avoid.
fn manual_only(creds_present: bool, platform: &[BootCheck]) -> Vec<BootCheck> {
    let mut rows = Vec::new();
    if !creds_present {
        rows.push(BootCheck {
            name: "PRISM credentials".to_string(),
            result: "not repairable here — requires authentication".to_string(),
            ok: false,
            dots: 4,
            delay_ms: 0,
        });
    }
    for check in platform
        .iter()
        .filter(|c| !c.ok && REPAIR_RELEVANT.contains(&c.name.as_str()))
    {
        rows.push(BootCheck {
            name: check.name.clone(),
            result: format!("not repairable here — {}", check.result),
            ok: false,
            dots: 4,
            delay_ms: 0,
        });
    }
    rows
}

/// Which model answers a chat turn, and whether that costs money.
///
/// The distinction this row exists to draw is CHOSE-the-platform versus
/// FELL-BACK-to-it. [`chat_config::ChatTarget::default()`] is `Marc27`, the
/// billed platform, so a config that is missing, empty, or unparseable reads
/// identically to one that names it — and the user who set up a local server
/// gets billed with no signal. Only the second case is a problem, so only the
/// second case is reported as one.
fn chat_route_check() -> BootCheck {
    // BOTH halves must read the SAME file. An earlier version took the prism
    // directory as an argument and inspected `<that>/config.toml` while
    // `load()` resolved its own path — so the row could report on one file and
    // route by another, which is precisely the class of failure it exists to
    // catch.
    let path = chat_config::config_path().unwrap_or_else(|_| PathBuf::from("config.toml"));
    // Did the FILE actually name a target? `load()` cannot answer this: it
    // returns the default for "absent", "empty" and "unparseable" alike,
    // which is exactly how the fallback stays invisible.
    // DECLARED means "this file names a target `load()` could actually use",
    // not merely "a [chat] table exists". Adversarial review found the weaker
    // check reporting the same silent fallback this row exists to catch: a
    // `[chat]` block missing a required field is valid TOML, so the key is
    // present, but the structured parse fails and `load()` returns the billed
    // default — and the row called that a deliberate choice.
    let declared = std::fs::read_to_string(&path).is_ok_and(|raw| {
        toml::from_str::<toml::Value>(&raw)
            .ok()
            .and_then(|value| value.get("chat").cloned())
            .is_some_and(|chat| chat.try_into::<chat_config::ChatTarget>().is_ok())
    });

    chat_route_row(
        declared,
        chat_config::load().unwrap_or_default().chat,
        &path,
    )
}

/// The row itself, as a pure decision over what was found.
///
/// Split out so the verdict can be tested without standing up a HOME: all the
/// behaviour worth guarding is the mapping from (was a target declared?, which
/// target) to a verdict.
fn chat_route_row(declared: bool, target: chat_config::ChatTarget, path: &Path) -> BootCheck {
    let (result, ok) = match target {
        chat_config::ChatTarget::Local { url, model, .. } => {
            let reachable = tcp_reachable(&url);
            (
                format!(
                    "local — {model} at {url}{}",
                    if reachable {
                        ""
                    } else {
                        " — NOT REACHABLE; start the server or `prism use marc27`"
                    }
                ),
                reachable,
            )
        }
        chat_config::ChatTarget::Provider {
            provider, model, ..
        } => (
            format!("direct provider — {provider}, {model} (billed by {provider})"),
            true,
        ),
        chat_config::ChatTarget::Marc27 { model } => {
            let named = model.unwrap_or_else(|| "platform default".to_string());
            if declared {
                (format!("{} — {named} (BILLED)", brand_name()), true)
            } else {
                (
                    format!(
                        "no [chat] target in {} — defaulting to {} ({named}), which is BILLED. \
                         `prism use local --url <url> --model <name>` to route to your own server",
                        path.display(),
                        brand_name(),
                    ),
                    false,
                )
            }
        }
    };
    BootCheck {
        name: "Chat route".to_string(),
        result,
        ok,
        dots: 0,
        delay_ms: 0,
    }
}

/// Platform name for user-facing text, from `brand.toml` rather than a
/// hardcoded "MARC27" — the same rule the rest of the CLI follows.
fn brand_name() -> String {
    prism_core::brand::brand().platform_name.clone()
}

/// Whether an OpenAI-compatible base URL answers a TCP connect.
///
/// Deliberately a connect and not a request: this runs on every `doctor`,
/// a model server can take seconds to answer `/v1/models` while loading
/// weights, and "the port is open" is the fact the row needs.
fn tcp_reachable(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    let default_port = if url.starts_with("https://") { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(default_port)),
        None => (authority, default_port),
    };
    std::net::TcpStream::connect_timeout(
        &match format!("{host}:{port}").parse() {
            Ok(addr) => addr,
            // A hostname needs resolving; fall back to the resolving connect.
            Err(_) => {
                return std::net::TcpStream::connect((host, port)).is_ok();
            }
        },
        std::time::Duration::from_millis(800),
    )
    .is_ok()
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
        result: "not installed — only needed for `prism use local`".to_string(),
        ok: false,
        dots: 4,
        delay_ms: 0,
    }
}

/// Venv check that verifies every **declared** dependency is present, not
/// merely that a directory exists or that `import app` happens to work.
///
/// `import app` was the old test, and it passed on a venv with `python-ulid`
/// absent — so this row read [OK] while the MACE tool code and seven test
/// modules were unimportable. The predicate is now the one `ensure_venv` uses,
/// imported rather than restated, so the two cannot disagree.
fn check_venv_tools(venv_python: &Path) -> BootCheck {
    let name = "PRISM Python venv";
    if !venv_python.exists() {
        return BootCheck {
            name: name.to_string(),
            result: "missing — provisioned on next `prism` launch".to_string(),
            ok: false,
            dots: 4,
            delay_ms: 0,
        };
    }
    let missing = prism_python_bridge::venv::missing_requirements(venv_python);
    let result = if missing.is_empty() {
        format!(
            "{} (all declared dependencies present)",
            venv_python.display()
        )
    } else {
        // Not "next launch self-heals": relaunching does now re-sync a venv
        // that is merely out of date, but a venv whose interpreter or pip is
        // broken loops on the same error. `--fix` removes it and rebuilds,
        // which is the step that works in both cases.
        format!(
            "missing {}: {} — repairable",
            if missing.len() == 1 {
                "1 declared dependency".to_string()
            } else {
                format!("{} declared dependencies", missing.len())
            },
            missing.join(", ")
        )
    };
    BootCheck {
        name: name.to_string(),
        result,
        ok: missing.is_empty(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The anti-lying invariant. A repair that reports success but leaves the
    /// check failing must be rendered as NOT fixed. This is the guard against
    /// adding a fourth doctor row that reports on something nobody wrote.
    #[test]
    fn a_repair_that_did_not_work_is_never_reported_as_fixed() {
        let row = settle(
            "thing",
            Ok("rebuilt it".to_string()),
            || false,
            "repairable",
        );
        assert!(
            !row.ok,
            "verdict must come from the re-check, not the repair"
        );
        assert!(row.result.contains("still fails"), "{}", row.result);
        // The repair's own note survives, so the reader can see what was tried
        // and why believing it would have been wrong.
        assert!(row.result.contains("rebuilt it"), "{}", row.result);
        // The caller's guidance is carried through verbatim. This assertion
        // used to require the literal string "run: prism fixit"; d8667e14's
        // exit-to-CLI sweep removed that instruction from the product, and
        // afterwards this test was the ONLY occurrence of it left anywhere in
        // the tree — it pinned a string nothing produced. Same stale-assertion
        // class as ef514082's fix to unfixable_rows_*.
        assert!(row.result.contains("repairable"), "{}", row.result);
        // The row states the condition; it never tells the reader to quit and
        // run something. Guarded repo-wide by
        // crates/server/tests/no_exit_to_cli.rs.
        assert!(!row.result.contains("prism fixit"), "{}", row.result);
        assert!(!row.result.contains("prism login"), "{}", row.result);
    }

    #[test]
    fn a_repair_that_worked_is_reported_with_what_it_did() {
        let row = settle("thing", Ok("rebuilt it".to_string()), || true, "manual");
        assert!(row.ok);
        assert_eq!(row.result, "rebuilt it");
    }

    #[test]
    fn a_failed_repair_prints_the_reason_and_the_exact_command() {
        let row = settle(
            "thing",
            Err("no python".to_string()),
            || false,
            "run: brew install python@3.12",
        );
        assert!(!row.ok);
        assert!(row.result.contains("no python"), "{}", row.result);
        assert!(
            row.result.contains("brew install python@3.12"),
            "{}",
            row.result
        );
    }

    /// A repair can error while the thing turns out to be healthy anyway
    /// (something else provisioned it). Report the truth, not the error.
    #[test]
    fn health_wins_over_a_noisy_repair() {
        let row = settle("thing", Err("timed out".to_string()), || true, "manual");
        assert!(row.ok);
        assert!(row.result.contains("timed out"), "{}", row.result);
    }

    /// An interpreter that is not there accounts for nothing, so the row must
    /// be red and must name what is unaccounted for rather than say "OK".
    #[test]
    fn venv_row_is_red_and_specific_when_the_interpreter_is_absent() {
        let row = check_venv_tools(Path::new("/nonexistent/prism-doctor-test/bin/python3"));
        assert!(!row.ok);
        assert!(row.result.contains("missing"), "{}", row.result);
    }

    /// The row must report a venv that is missing a declared dependency as
    /// broken *and name it*. This is the defect the file is being repaired
    /// for: `import app` succeeded on exactly such a venv, so the row was
    /// green while the MACE code paths were unimportable.
    #[test]
    fn venv_row_names_the_dependencies_a_real_interpreter_lacks() {
        // A stock interpreter: runs, but has no PRISM platform and none of
        // the declared distributions installed.
        let Some(python) = ["/usr/bin/python3", "/opt/homebrew/bin/python3"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists())
        else {
            return; // no system python on this box; nothing to assert against
        };
        let row = check_venv_tools(python);
        // Hard assert, deliberately: the probe runs the interpreter with `-I`
        // (venv.rs:135), so it is isolated from the working directory and from
        // user site-packages. A system interpreter therefore cannot satisfy
        // these requirements by accident — not even in this repo, where a bare
        // `python3 -c "import app"` DOES succeed by picking up ./app. That
        // difference is what made this test look environmental when it was not.
        //
        // An earlier version of this fix replaced this with `if row.ok { return }`,
        // which turned a loud failure into a silent skip and executed zero
        // assertions. It was never needed: row.ok was already false here.
        assert!(!row.ok, "a venv without the declared set is not healthy");
        assert!(
            row.result.contains("prism-platform"),
            "must name what is missing: {}",
            row.result
        );
        // `check_venv_tools` ends the row with "— repairable". This assertion
        // used to require "prism doctor --fix"; d8667e14's exit-to-CLI sweep
        // replaced that guidance, and the string this test demanded survives
        // only in an unrelated `println!` at line ~199 — so the test pinned a
        // sentence this code path never emits, and failed on every machine,
        // not just ones with an unusual interpreter. Same stale-assertion
        // class as ef514082's fix to unfixable_rows_*.
        assert!(row.result.contains("repairable"), "{}", row.result);
        // The row names the condition; it never sends the reader out to a
        // command. Guarded repo-wide by crates/server/tests/no_exit_to_cli.rs.
        assert!(!row.result.contains("prism doctor"), "{}", row.result);
    }

    #[test]
    fn embed_cache_predicate_rejects_arbitrary_content() {
        let dir = std::env::temp_dir().join(format!("prism-doctor-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !prism_embed::native_model_status_at(&dir).is_ready(),
            "missing dir is not a cache"
        );
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            !prism_embed::native_model_status_at(&dir).is_ready(),
            "empty dir is not a cache"
        );
        std::fs::write(dir.join("model.onnx"), b"weights").unwrap();
        assert!(
            !prism_embed::native_model_status_at(&dir).is_ready(),
            "unversioned, unverified content is not an installed snapshot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn missing_native_snapshot() -> prism_embed::NativeModelStatus {
        prism_embed::NativeModelStatus::Unavailable(prism_embed::NativeModelUnavailable {
            code: prism_embed::NativeModelUnavailableCode::MissingReference,
            detail: "missing in test".to_string(),
            install_command: prism_embed::BGE_INSTALL_COMMAND,
        })
    }

    #[test]
    fn hosted_configuration_does_not_fail_doctor_for_missing_local_weights() {
        let status = prism_embed::EmbeddingConfigurationStatus {
            backend: prism_embed::ConfiguredBackendStatus::HostedReady {
                backend_id: "openai:test-embedding".to_string(),
            },
            native_snapshot: missing_native_snapshot(),
        };
        let row = embedding_check(&status, Path::new("/unused/embed-cache"));
        assert!(row.ok, "hosted backend does not require local weights");
        assert!(row.result.contains("hosted configured"), "{}", row.result);
        assert!(row.result.contains("not required"), "{}", row.result);
        assert!(
            row.result.contains("integrity unavailable"),
            "{}",
            row.result
        );
    }

    #[test]
    fn platform_unusable_native_backend_never_reports_as_ready() {
        let status = prism_embed::EmbeddingConfigurationStatus {
            backend: prism_embed::ConfiguredBackendStatus::NativeUnsupported {
                reason: "unsupported test platform".to_string(),
            },
            native_snapshot: prism_embed::NativeModelStatus::Ready {
                snapshot_dir: PathBuf::from("/verified/snapshot"),
                revision: prism_embed::BGE_SMALL_EN_V15_MANIFEST.revision,
            },
        };
        let row = embedding_check(&status, Path::new("/unused/embed-cache"));
        assert!(!row.ok, "verified bytes cannot make the platform usable");
        assert!(row.result.contains("platform unusable"), "{}", row.result);
        assert!(row.result.contains("integrity verified"), "{}", row.result);
    }

    /// A venv path that cannot possibly host a venv (there is a regular file
    /// sitting where the directory belongs). `--fix` must say so, name the
    /// command, and not claim a repair.
    #[tokio::test]
    async fn fix_venv_honestly_reports_what_it_cannot_repair() {
        let root = std::env::temp_dir().join(format!("prism-doctor-venv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A regular file where `~/.prism/venv/` should be.
        std::fs::write(root.join("venv"), b"not a directory").unwrap();

        let row = fix_venv(&root, &root).await;

        assert!(!row.ok, "must not claim a venv it does not have");
        assert!(
            !row.result.contains("provisioned"),
            "must not claim work it did not do: {}",
            row.result
        );
        assert!(
            row.result.contains("brew install python@3.12"),
            "must print the exact command: {}",
            row.result
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn row(name: &str, result: &str, ok: bool) -> BootCheck {
        BootCheck {
            name: name.into(),
            result: result.into(),
            ok,
            dots: 4,
            delay_ms: 0,
        }
    }

    #[test]
    fn unfixable_rows_name_the_condition_without_sending_the_user_away() {
        let platform = vec![
            row("Platform", "api.marc27.com unreachable", false),
            row("Policy Engine", "OPA/Rego loaded", true),
        ];
        let rows = manual_only(false, &platform);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["PRISM credentials", "Platform"]);
        assert!(rows.iter().all(|r| !r.ok));
        // The row must state what is wrong, not instruct the reader to quit and
        // run something — the remedy belongs in the surface they are already in.
        // Guarded repo-wide by crates/server/tests/no_exit_to_cli.rs.
        assert!(rows[0].result.contains("requires authentication"));
        assert!(!rows[0].result.contains("prism login"));
        assert!(rows[1].result.contains("unreachable"));
    }

    /// An offline local node is the normal state for anyone who never opted
    /// into `prism node up`, and the downstream rows are all just echoes of a
    /// missing login. Listing them under "Repairs" would invent problems.
    #[test]
    fn optional_and_downstream_failures_are_not_dressed_up_as_repairs() {
        let platform = vec![
            row("Local Node", "offline — node not started", false),
            row("Knowledge Graph", "unavailable", false),
            row("Marketplace", "unavailable", false),
            row("Compute", "unavailable", false),
        ];
        assert!(
            manual_only(true, &platform).is_empty(),
            "opt-in and downstream rows must not appear under Repairs",
        );
    }

    /// THE ROW THAT SAYS WHETHER THE NEXT QUESTION IS BILLED.
    ///
    /// [`chat_config::ChatTarget::default`] is the paid platform, so a
    /// `config.toml` that is missing, empty or unparseable silently moves chat
    /// off a local server onto billed hosting. Measured on a live machine: the
    /// file was truncated to zero bytes, every other doctor row still read
    /// green, `llama-server running [OK]` sat directly above it, and nothing
    /// said the next question would cost money. The distinction the row exists
    /// to draw is CHOSE-the-platform versus FELL-BACK-to-it, so both
    /// directions are pinned.
    #[test]
    fn chat_route_separates_a_chosen_platform_from_a_silent_fallback() {
        let path = Path::new("/nonexistent/config.toml");

        // Undeclared: absent, empty and unparseable all arrive here, because
        // `load()` returns the same default for every one of them.
        let fell_back = chat_route_row(false, chat_config::ChatTarget::default(), path);
        assert!(!fell_back.ok, "a silent fallback must not read as healthy");
        assert!(fell_back.result.contains("BILLED"), "{}", fell_back.result);
        assert!(
            fell_back.result.contains("no [chat] target"),
            "name the cause: {}",
            fell_back.result
        );
        assert!(
            fell_back.result.contains("prism use local"),
            "an actionable way out belongs in the row: {}",
            fell_back.result
        );

        // The SAME target, deliberately chosen, is not a fault. The row states
        // the cost without crying wolf, or operators learn to ignore it.
        let chosen = chat_route_row(true, chat_config::ChatTarget::default(), path);
        assert!(chosen.ok, "an explicit choice is not a misconfiguration");
        assert!(chosen.result.contains("BILLED"), "{}", chosen.result);
    }

    /// A [chat] TABLE THAT DOES NOT PARSE IS NOT A CHOICE.
    ///
    /// Adversarial review found this row reporting the very fallback it exists
    /// to catch: `[chat] mode="local" url="..."` with no `model` is valid TOML,
    /// so a bare key-presence check called it declared, while `load()` failed
    /// the structured parse and returned the BILLED default. Green row, silent
    /// billing — the original bug, one layer down.
    #[test]
    fn a_chat_table_that_fails_to_parse_is_not_a_deliberate_choice() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        // Valid TOML, invalid ChatTarget: `local` requires a model.
        std::fs::write(&path, "[chat]\nmode = \"local\"\nurl = \"http://x/v1\"\n").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let declared = toml::from_str::<toml::Value>(&raw)
            .ok()
            .and_then(|value| value.get("chat").cloned())
            .is_some_and(|chat| chat.try_into::<chat_config::ChatTarget>().is_ok());
        assert!(
            !declared,
            "a [chat] table that cannot become a ChatTarget must not count as declared"
        );
        let row = chat_route_row(declared, chat_config::ChatTarget::default(), &path);
        assert!(!row.ok, "the row must flag it, not bless it");
        assert!(row.result.contains("BILLED"), "{}", row.result);
    }

    /// A configured-but-down local server is the same problem from the other
    /// end, and was equally invisible before this row existed.
    #[test]
    fn chat_route_reports_a_configured_but_unreachable_local_server() {
        // Port 9 is discard; nothing listens on a normal host.
        let down = chat_route_row(
            true,
            chat_config::ChatTarget::Local {
                url: "http://127.0.0.1:9/v1".to_string(),
                model: "some-model".to_string(),
                api_key: None,
            },
            Path::new("/nonexistent/config.toml"),
        );
        assert!(
            !down.ok,
            "an unreachable local server must not read as healthy"
        );
        assert!(
            down.result.contains("NOT REACHABLE"),
            "say so plainly: {}",
            down.result
        );
        assert!(
            down.result.contains("some-model"),
            "name the model it would have used: {}",
            down.result
        );
    }

    /// Malformed URLs must return a verdict, never panic the whole diagnostic.
    #[test]
    fn reachability_never_panics_on_a_malformed_url() {
        for url in [
            "http://127.0.0.1:8081/v1",
            "http://127.0.0.1/v1",
            "https://example.invalid/v1",
            "http://[::1]:8081/v1",
            "not-a-url",
            "",
        ] {
            let _ = tcp_reachable(url);
        }
    }
}
