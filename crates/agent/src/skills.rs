//! Self-authored skills — the Voyager loop.
//!
//! The agent writes a small skill (a named, described snippet of shell or
//! Python), it is **verified by executing it once**, and only then stored under
//! `~/.prism/skills/` where `list_skills` / `find_tools` can surface it and
//! `run_skill` can re-execute it on later turns. Authored skills are
//! **untrusted by default** (design `docs/CAPABILITY_REGISTRY_DESIGN.md` §5a);
//! execution here is a plain subprocess (no container sandbox yet — that is the
//! next hardening slice), which also sidesteps the `execute_python` sidecar
//! (`preexec_fn=os.setsid`) implicated in the macOS SIGSEGV.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Longest allowed skill name (also the filename stem).
const SKILL_NAME_MAX: usize = 64;

/// One agent-authored skill: a named, described, executable code artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthoredSkill {
    /// Safe single-segment slug; also the `<name>.json` filename stem.
    pub name: String,
    /// One line the retrieval layer embeds and the model reads.
    pub description: String,
    /// `"shell"` or `"python"` — selects the interpreter for verify/run.
    pub language: String,
    /// The skill body.
    pub code: String,
    /// Whether `code` executed cleanly (exit 0) at write time.
    pub verified: bool,
    /// Trust tag. Authored skills are `"untrusted"` by default.
    pub trust: String,
    /// Unix-seconds creation time (0 if the clock was unavailable).
    pub created_at: u64,
}

impl AuthoredSkill {
    /// Build an untrusted authored skill, stamping `created_at` to now.
    pub fn new(name: &str, description: &str, language: &str, code: &str, verified: bool) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            language: language.to_string(),
            code: code.to_string(),
            verified,
            trust: "untrusted".to_string(),
            created_at: now_unix(),
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The skills directory: `$PRISM_SKILLS_DIR` if set (tests / sandboxes), else
/// `~/.prism/skills`. Falls back to `./.prism/skills` if `$HOME` is unset.
pub fn skills_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PRISM_SKILLS_DIR") {
        return PathBuf::from(dir);
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".prism").join("skills"),
        None => PathBuf::from(".prism").join("skills"),
    }
}

/// A name is valid iff it is a non-empty, bounded, single-segment slug of
/// `[A-Za-z0-9_-]` — so it can never traverse out of [`skills_dir`].
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SKILL_NAME_MAX
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Path of the `<name>.json` manifest for `name` (caller must [`valid_name`]).
pub fn path_for(name: &str) -> PathBuf {
    skills_dir().join(format!("{name}.json"))
}

/// Persist a skill (overwrites a same-named one). Creates the directory.
pub fn store(skill: &AuthoredSkill) -> Result<PathBuf> {
    anyhow::ensure!(
        valid_name(&skill.name),
        "invalid skill name '{}'",
        skill.name
    );
    let dir = skills_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create skills dir {dir:?}"))?;
    let path = path_for(&skill.name);
    let json = serde_json::to_string_pretty(skill).context("serialize skill")?;
    std::fs::write(&path, json).with_context(|| format!("write skill {path:?}"))?;
    Ok(path)
}

/// Load one authored skill by name.
pub fn load(name: &str) -> Result<AuthoredSkill> {
    anyhow::ensure!(valid_name(name), "invalid skill name '{name}'");
    let path = path_for(name);
    let bytes =
        std::fs::read(&path).with_context(|| format!("no authored skill '{name}' at {path:?}"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse skill {path:?}"))
}

/// Load every authored skill. Malformed / unreadable files are skipped so one
/// bad file never blinds the agent to the rest.
pub fn load_all() -> Vec<AuthoredSkill> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(skills_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(skill) = serde_json::from_slice::<AuthoredSkill>(&bytes)
        {
            out.push(skill);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `(name, "name: description")` pairs for the retrieval / progressive-
/// disclosure layer, matching the tool-catalog entry shape.
pub fn retrieval_entries() -> Vec<(String, String)> {
    load_all()
        .into_iter()
        .map(|s| (s.name.clone(), format!("{}: {}", s.name, s.description)))
        .collect()
}

/// Whether an authored skill with this name exists.
pub fn exists(name: &str) -> bool {
    valid_name(name) && path_for(name).is_file()
}

/// L1 progressive-disclosure block for authored skills: a compact list the
/// model is passively AWARE of, with the correct call instruction (`run_skill`,
/// not `find_tools`). `None` when no skills exist. Bounded by `max_entries`.
pub fn skills_menu(max_entries: usize) -> Option<String> {
    let skills = load_all();
    if skills.is_empty() {
        return None;
    }
    let lines: Vec<String> = skills
        .iter()
        .take(max_entries)
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect();
    Some(format!(
        "You have previously authored these reusable skills. Execute one with \
         run_skill(name=\"…\") when it fits the task (write_skill to add more):\n{}",
        lines.join("\n")
    ))
}

/// Outcome of executing a skill body.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Process exited 0.
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    /// Exit code, or `None` if killed by a signal.
    pub code: Option<i32>,
}

/// Interpreter path for `python`: the PRISM venv python if present, else
/// `python3` from `PATH`.
fn python_interpreter() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let venv = PathBuf::from(home).join(".prism/venv/bin/python3");
        if venv.exists() {
            return venv.to_string_lossy().into_owned();
        }
    }
    "python3".to_string()
}

/// Default wall-clock limit for a skill run.
const SKILL_EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Run a skill body once with the default timeout. `language` is `"shell"`
/// (default for anything unrecognized) or `"python"`. Blocks the calling
/// thread; callers run it via `spawn_blocking`.
pub fn execute(language: &str, code: &str) -> Result<ExecOutput> {
    execute_with_timeout(language, code, SKILL_EXEC_TIMEOUT)
}

/// Run a skill body once, capturing output, with hardening for untrusted code:
/// the environment is **scrubbed** (only a minimal `PATH`/`HOME` pass through,
/// so a skill can't read the parent's secrets) and the process is **killed if
/// it exceeds `timeout`**. Still a plain subprocess, not a container — full
/// isolation (namespaces / seccomp) is a later slice — so treat authored code
/// as untrusted-but-local.
pub fn execute_with_timeout(
    language: &str,
    code: &str,
    timeout: std::time::Duration,
) -> Result<ExecOutput> {
    use std::process::{Command, Stdio};
    let mut cmd = match language {
        "python" | "py" => {
            let mut c = Command::new(python_interpreter());
            c.arg("-c").arg(code);
            c
        }
        _ => {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(code);
            c
        }
    };
    cmd.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        cmd.env("HOME", home);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_with_timeout(cmd, timeout)
}

/// Spawn `cmd`, draining stdout/stderr on threads (so a full pipe buffer can't
/// deadlock against our own wait), and enforce `timeout` by polling — killing
/// the child if it overruns.
fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> Result<ExecOutput> {
    use std::io::Read;
    use std::time::Instant;

    let mut child = cmd.spawn().context("spawn skill interpreter")?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().context("wait on skill process")? {
            Some(s) => break Some(s),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };

    let stdout = String::from_utf8_lossy(&out_handle.join().unwrap_or_default()).into_owned();
    let mut stderr = String::from_utf8_lossy(&err_handle.join().unwrap_or_default()).into_owned();
    match status {
        Some(s) => Ok(ExecOutput {
            ok: s.success(),
            stdout,
            stderr,
            code: s.code(),
        }),
        None => {
            stderr.push_str(&format!(
                "\n[skill exceeded {}s and was killed]",
                timeout.as_secs()
            ));
            Ok(ExecOutput {
                ok: false,
                stdout,
                stderr,
                code: None,
            })
        }
    }
}

/// `PRISM_SKILLS_DIR` is process-global; serialize every test (in any module)
/// that mutates it so parallel runs can't read each other's temp dir.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Lock the env, point [`skills_dir`] at a fresh temp dir, and return the guard
/// (keep it alive for the whole test) plus the dir. Shared by `skills` and
/// `meta_tools` tests.
#[cfg(test)]
pub(crate) fn test_env_guard(tag: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
    let guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("prism-skills-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    unsafe { std::env::set_var("PRISM_SKILLS_DIR", &base) };
    (guard, base)
}

#[cfg(test)]
mod tests {
    use super::test_env_guard as env_guard;
    use super::*;

    #[test]
    fn valid_name_rejects_traversal_and_bad_chars() {
        assert!(valid_name("compute_density"));
        assert!(valid_name("skill-1"));
        assert!(!valid_name(""));
        assert!(!valid_name("../evil"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("has space"));
        assert!(!valid_name(&"x".repeat(SKILL_NAME_MAX + 1)));
    }

    #[test]
    fn store_then_load_roundtrips() {
        let (_g, _dir) = env_guard("roundtrip");
        let skill = AuthoredSkill::new("greet", "print a greeting", "shell", "echo hi", true);
        let path = store(&skill).expect("store");
        assert!(path.exists());
        let loaded = load("greet").expect("load");
        assert_eq!(loaded, skill);
    }

    #[test]
    fn store_rejects_invalid_name() {
        let (_g, _dir) = env_guard("badname");
        let skill = AuthoredSkill::new("../evil", "x", "shell", "echo x", true);
        assert!(store(&skill).is_err());
    }

    #[test]
    fn load_all_skips_malformed_and_sorts() {
        let (_g, dir) = env_guard("loadall");
        std::fs::create_dir_all(&dir).unwrap();
        store(&AuthoredSkill::new("bravo", "b", "shell", "echo b", true)).unwrap();
        store(&AuthoredSkill::new("alpha", "a", "shell", "echo a", true)).unwrap();
        // A malformed file must not abort the whole load.
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        let all = load_all();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "bravo"],
            "malformed skipped, rest sorted"
        );
    }

    #[test]
    fn retrieval_entries_match_catalog_shape() {
        let (_g, _dir) = env_guard("entries");
        store(&AuthoredSkill::new(
            "lattice_a",
            "estimate the lattice parameter",
            "python",
            "print(3.6)",
            true,
        ))
        .unwrap();
        let entries = retrieval_entries();
        assert_eq!(
            entries,
            vec![(
                "lattice_a".to_string(),
                "lattice_a: estimate the lattice parameter".to_string()
            )]
        );
    }

    #[test]
    fn execute_shell_captures_success_and_failure() {
        let ok = execute("shell", "echo hello").expect("run");
        assert!(ok.ok);
        assert_eq!(ok.stdout.trim(), "hello");
        let bad = execute("shell", "exit 3").expect("run");
        assert!(!bad.ok);
        assert_eq!(bad.code, Some(3));
    }

    #[test]
    fn execute_times_out_and_is_killed() {
        let out = execute_with_timeout("shell", "sleep 5", std::time::Duration::from_millis(200))
            .unwrap();
        assert!(!out.ok, "a timed-out skill must not report success");
        assert_eq!(out.code, None, "killed process has no exit code");
        assert!(
            out.stderr.contains("exceeded"),
            "stderr notes the timeout: {out:?}"
        );
    }

    #[test]
    fn execute_scrubs_parent_env() {
        // Serialize env mutation with the shared lock (also sets a temp skills dir).
        let (_g, _dir) = env_guard("scrub");
        unsafe { std::env::set_var("PRISM_SKILL_SECRET_XYZ", "leaked") };
        let out = execute("shell", "echo secret=[$PRISM_SKILL_SECRET_XYZ]").unwrap();
        unsafe { std::env::remove_var("PRISM_SKILL_SECRET_XYZ") };
        assert!(out.ok);
        assert_eq!(
            out.stdout.trim(),
            "secret=[]",
            "the child must not inherit the parent's env secrets"
        );
    }

    #[test]
    fn exists_and_skills_menu() {
        let (_g, _dir) = env_guard("menu");
        assert!(
            skills_menu(10).is_none(),
            "no menu when there are no skills"
        );
        assert!(!exists("nope"));
        store(&AuthoredSkill::new(
            "density_calc",
            "compute density from mass and volume",
            "python",
            "print(1.0)",
            true,
        ))
        .unwrap();
        assert!(exists("density_calc"));
        let menu = skills_menu(10).expect("menu with one skill");
        assert!(
            menu.contains("run_skill"),
            "menu carries the call instruction"
        );
        assert!(menu.contains("- density_calc: compute density from mass and volume"));
    }
}
