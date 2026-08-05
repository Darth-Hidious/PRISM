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

    // 2. Embedding model. This used to look for
    //    `models/embeddinggemma-300m.gguf` and claim it "auto-downloads on
    //    first `prism`" — nothing in the tree has ever written that file, so
    //    the row was permanently red with a hint that was simply untrue.
    //    The model PRISM actually uses is BGE-small-en-v1.5, cached by
    //    `prism-embed` under `models/embed/` on first semantic search.
    let embed_dir = prism_dir.join("models/embed");
    checks.push(BootCheck {
        name: "Embedding model".to_string(),
        result: if embed_model_cached(&embed_dir) {
            format!("cached at {}", embed_dir.display())
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            "unavailable on Intel macOS — set PRISM_EMBED_BACKEND=openai".to_string()
        } else {
            "downloads on first semantic search (~90 MB)".to_string()
        },
        ok: embed_model_cached(&embed_dir),
        dots: 4,
        delay_ms: 0,
    });

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
    let endpoints = PlatformEndpoints::from_env();
    let platform_checks =
        boot_checks::run_boot_checks(state.credentials.as_ref(), &endpoints).await;
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
const EMBED_MANUAL: &str =
    "retry with network access, or set PRISM_EMBED_BACKEND=openai with PRISM_EMBED_ENDPOINT_URL";

/// Warm the local embedding model cache.
///
/// Building the configured backend is what downloads the weights, so that is
/// the repair — and it only counts as one if the cache directory has content
/// afterwards.
///
/// A hosted (OpenAI-compatible) embedder needs no local weights at all. That
/// case gets its own verdict rather than being pushed through the cache
/// check, which would otherwise print the self-contradiction "no local model
/// is needed" next to a red mark — the exact species of nonsense row this
/// file has spent three deletions getting rid of.
async fn fix_embedding_model(embed_dir: &Path) -> BootCheck {
    let dir = embed_dir.to_path_buf();
    let cached = move || embed_model_cached(&dir);
    if cached() {
        return settle("Embedding model", Ok("already cached".into()), cached, "");
    }
    println!("  building the embedding backend (native weights are ~90 MB, once)…");
    // `from_config` blocks on the download; keep it off the async workers.
    // The backend id tells us which one we got: `native:…` or `openai:…`.
    let id = tokio::task::spawn_blocking(prism_embed::from_config)
        .await
        .ok()
        .flatten()
        .map(|backend| backend.id().to_string());

    match id {
        Some(id) if !id.starts_with("native:") => {
            // Nothing to download and nothing wrong. The re-check here is
            // "did a usable backend come back?", which it did.
            settle(
                "Embedding model",
                Ok(format!(
                    "hosted embedder configured ({id}) — no local model needed"
                )),
                || true,
                "",
            )
        }
        Some(_) => settle(
            "Embedding model",
            Ok(format!("downloaded to {}", embed_dir.display())),
            cached,
            EMBED_MANUAL,
        ),
        None => settle(
            "Embedding model",
            Err("no embedding backend could be built".to_string()),
            cached,
            EMBED_MANUAL,
        ),
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

/// Does the native embedding model cache hold anything? Shared by the check
/// and `--fix` so the diagnostic and the repair cannot drift apart about what
/// "cached" means — the same anti-drift reason the venv rows both go through
/// `prism_python_bridge::venv::missing_requirements`.
fn embed_model_cached(embed_dir: &Path) -> bool {
    embed_dir.exists() && std::fs::read_dir(embed_dir).is_ok_and(|mut d| d.next().is_some())
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
        assert!(row.result.contains("run: prism fixit"), "{}", row.result);
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
        assert!(!row.ok, "a venv without the declared set is not healthy");
        assert!(
            row.result.contains("prism-platform"),
            "must name what is missing: {}",
            row.result
        );
        assert!(row.result.contains("prism doctor --fix"), "{}", row.result);
    }

    #[test]
    fn embed_cache_predicate_needs_actual_content() {
        let dir = std::env::temp_dir().join(format!("prism-doctor-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!embed_model_cached(&dir), "missing dir is not a cache");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!embed_model_cached(&dir), "empty dir is not a cache");
        std::fs::write(dir.join("model.onnx"), b"weights").unwrap();
        assert!(embed_model_cached(&dir));
        let _ = std::fs::remove_dir_all(&dir);
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
    fn unfixable_rows_carry_the_command() {
        let platform = vec![
            row("Platform", "api.marc27.com unreachable", false),
            row("Policy Engine", "OPA/Rego loaded", true),
        ];
        let rows = manual_only(false, &platform);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["PRISM credentials", "Platform"]);
        assert!(rows.iter().all(|r| !r.ok));
        assert!(rows[0].result.contains("prism login"));
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
}
