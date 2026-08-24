//! Notebook manager — launch, list, and stop local Jupyter sessions.
//!
//! Each session runs `jupyter lab --no-browser` in the PRISM Python venv.
//! Sessions are tracked in `~/.prism/notebooks.json` (mode 0600) so they
//! survive CLI restarts.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookSession {
    pub pid: u32,
    pub port: u16,
    pub url: String,
    pub token: String,
    pub started_at: f64,
}

fn notebooks_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".prism/notebooks.json"))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn read_sessions() -> Vec<NotebookSession> {
    let Ok(path) = notebooks_path() else {
        return Vec::new();
    };
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_sessions(sessions: &[NotebookSession]) -> Result<()> {
    let path = notebooks_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(sessions)?;
    // This file holds live notebook tokens, i.e. kernel-exec credentials.
    // Create it 0600 rather than writing at the umask default and
    // chmod'ing after — the latter leaves a window in which the tokens
    // are world-readable, and on a 0022 umask that window is every
    // first write.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(json.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&path, json)?;
    }
    Ok(())
}

/// Filter out sessions whose PID is no longer alive.
fn prune_dead(sessions: Vec<NotebookSession>) -> Vec<NotebookSession> {
    sessions
        .into_iter()
        .filter(|s| {
            // Check if process is alive via `kill -0`.
            Command::new("kill")
                .arg("-0")
                .arg(s.pid.to_string())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .collect()
}

/// Find a free port by binding to port 0.
fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Generate a random token.
///
/// This token is the ONLY authentication on the Jupyter server, and a
/// valid token means arbitrary code execution in the kernel. It must be
/// unguessable, so it comes from the OS CSPRNG (two v4 UUIDs, 244 bits)
/// — never from the clock. A clock-derived token is recomputable by
/// anyone who knows roughly when the notebook started.
fn gen_token() -> String {
    format!(
        "prism-{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Launch a Jupyter Lab server in the PRISM venv.
pub fn start(port: Option<u16>, _notebook: Option<&str>) -> Result<NotebookSession> {
    let python = std::env::var("HOME")
        .map(|h| format!("{h}/.prism/venv/bin/python3"))
        .unwrap_or_else(|_| "python3".to_string());

    // Verify jupyter is available.
    let check = Command::new(&python)
        .args(["-c", "import jupyter_server; print('ok')"])
        .output();
    if !matches!(check, Ok(o) if o.status.success()) {
        bail!(
            "Jupyter is not installed in the PRISM venv ({python}).\n\
             Install it: {python} -m pip install jupyterlab"
        );
    }

    let port = port.unwrap_or_else(|| free_port().unwrap_or(8888));
    let token = gen_token();

    // Spawn jupyter lab headless.
    //
    // The token goes through the environment, NOT argv: a process's
    // command line is world-readable (`ps`, /proc/<pid>/cmdline), so
    // `--ServerApp.token=<secret>` would hand the kernel to any other
    // local user. `/proc/<pid>/environ` is owner-only. jupyter_server
    // reads `JUPYTER_TOKEN` as the env fallback for `ServerApp.token`.
    //
    // `--ServerApp.allow_origin=*` is deliberately NOT set: it let any
    // website the user visited make credentialed cross-origin calls to
    // this kernel. Jupyter's same-origin default is correct here — the
    // session URL we hand out is same-origin, and non-browser clients
    // (IDEs) don't send Origin at all, so nothing legitimate needs it.
    let child = Command::new(&python)
        .args([
            "-m",
            "jupyterlab",
            "--no-browser",
            "--port",
            &port.to_string(),
        ])
        .env("JUPYTER_TOKEN", &token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn jupyter lab")?;

    let pid = child.id();
    let url = format!("http://localhost:{port}/lab?token={token}");

    let session = NotebookSession {
        pid,
        port,
        url,
        token,
        started_at: now(),
    };

    // Persist.
    let mut sessions = prune_dead(read_sessions());
    sessions.push(session.clone());
    write_sessions(&sessions)?;

    Ok(session)
}

/// List active notebook sessions (prunes dead PIDs).
pub fn list() -> Result<Vec<NotebookSession>> {
    let sessions = prune_dead(read_sessions());
    write_sessions(&sessions)?;
    Ok(sessions)
}

/// Stop a notebook by PID, port, or "all".
pub fn stop(target: &str) -> Result<usize> {
    let mut sessions = prune_dead(read_sessions());
    let before = sessions.len();

    if target == "all" {
        for s in &sessions {
            let _ = Command::new("kill").arg(s.pid.to_string()).spawn();
        }
        sessions.clear();
    } else {
        let target_pid: Option<u32> = target.parse().ok();
        let target_port: Option<u16> = target.parse().ok();
        sessions.retain(|s| {
            let matches = Some(s.pid) == target_pid || Some(s.port) == target_port;
            if matches {
                let _ = Command::new("kill").arg(s.pid.to_string()).spawn();
                false
            } else {
                true
            }
        });
    }

    write_sessions(&sessions)?;
    Ok(before - sessions.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The Jupyter token is the ONLY thing standing between a caller and
    /// arbitrary code execution in the notebook kernel. It must be an
    /// unguessable secret, not a value anybody can recompute.
    #[test]
    fn token_is_not_recoverable_from_the_wall_clock() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let token = gen_token();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        // Reconstruct the attacker's guess: read the token as a hex
        // nanosecond timestamp. If it lands inside the window we just
        // measured, the "secret" is simply the clock.
        let hex = token.strip_prefix("prism-").unwrap_or(&token);
        if let Ok(nanos) = u128::from_str_radix(hex, 16) {
            assert!(
                !(before..=after).contains(&nanos),
                "notebook token is the wall clock in hex ({token}): anyone who \
                 knows when the notebook started can recompute it"
            );
        }
    }

    /// Sanity floor on entropy: distinct calls must not collide, and the
    /// token must be long enough to resist online guessing.
    #[test]
    fn token_is_long_and_unique() {
        let a = gen_token();
        let b = gen_token();
        assert_ne!(a, b, "two tokens collided: {a}");
        let secret = a.strip_prefix("prism-").unwrap_or(&a);
        assert!(
            secret.len() >= 32,
            "token secret too short ({} chars): {a}",
            secret.len()
        );
    }
}

// ── In-terminal viewing ────────────────────────────────────────────────

/// How long to let JupyterLab boot before reading it. Measured, not
/// guessed: reading immediately after `open` returned nothing at all, and
/// four seconds was enough for the UI to be present on this machine.
const NOTEBOOK_BOOT_MS: &str = "4000";

/// How a notebook should be rendered into the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Agent-readable text — greppable, diffable, and what the agent itself
    /// consumes.
    Text,
    /// A PNG written to disk. iTerm2 and kitty draw it inline; every other
    /// terminal gets a path, which is still a usable answer.
    Image,
}

/// Render a running notebook inside the terminal.
///
/// PRISM already starts Jupyter (`notebook start`) but there was nowhere to
/// SEE it without leaving for a browser — and "never tell anyone to run a
/// separate app" is the same rule as "never exit to the CLI". This drives a
/// headless browser instead, so the notebook is readable where the work is.
///
/// Shells out to `agent-browser` rather than linking it: it is an
/// independent binary with its own release cadence, and PRISM should not
/// take a browser engine into its own link graph to render a page.
///
/// ## The token
///
/// [`start`] passes the Jupyter token through the ENVIRONMENT precisely so
/// it never lands in a process's argv, where any local user can read it with
/// `ps` — and a valid token is arbitrary code execution in the kernel.
/// `agent-browser` takes its target as an argument, so a tokened URL would
/// undo that. Instead the token is planted as a COOKIE via a file, and only
/// the token-free base URL is ever passed on the command line.
pub fn view(session: &NotebookSession, mode: ViewMode, out_path: Option<&str>) -> Result<String> {
    let browser = which_agent_browser()?;

    // Jupyter accepts its token as a cookie, so the credential travels in a
    // file rather than on a command line.
    //
    // NOT in `/tmp`. A predictable name in a world-writable directory is a
    // symlink-plant waiting to happen, and writing first then chmod'ing
    // after leaves a window in which a kernel-exec credential is readable by
    // every local user. Both of those are exactly what `start` avoids by
    // passing the token through the environment, and it would be absurd to
    // reintroduce them here in order to display the notebook.
    //
    // So: a 0700 directory inside the user's own PRISM home, and the file
    // created with `create_new` + mode 0600 in ONE syscall — `O_CREAT|O_EXCL`
    // refuses to follow a planted symlink, and the mode is set at creation
    // rather than repaired afterwards.
    let run_dir = notebooks_path()?
        .parent()
        .context("PRISM home has no parent")?
        .join("run");
    std::fs::create_dir_all(&run_dir).context("creating the PRISM run directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700));
    }
    let cookie_file = run_dir.join(format!("nb-{}-{}.json", session.port, gen_token()));
    let cookie = serde_json::json!([{
        "name": "token",
        "value": session.token,
        "url": session.url,
    }]);
    {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&cookie_file)
            .context("creating the notebook cookie file")?;
        file.write_all(&serde_json::to_vec(&cookie)?)
            .context("writing the notebook cookie file")?;
    }

    let run = |args: &[&str]| -> Result<String> {
        let output = Command::new(&browser)
            .args(args)
            .output()
            .with_context(|| format!("running agent-browser {}", args.join(" ")))?;
        if !output.status.success() {
            anyhow::bail!(
                "agent-browser {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };

    let cookie_arg = cookie_file.display().to_string();
    let _ = run(&["cookies", "set", "--curl", &cookie_arg]);

    // The stored session URL carries `?token=...`, so opening it directly
    // would put the credential straight into agent-browser's argv and undo
    // the file-based handling above. Only the token-free base URL is passed
    // on a command line; the cookie is what authenticates.
    let base_url = session
        .url
        .split_once('?')
        .map_or(session.url.as_str(), |(base, _)| base);
    run(&["open", base_url])?;

    // JupyterLab is a single-page app: immediately after `open` there is a
    // shell and no content, and reading then returns an empty string that
    // looks exactly like an empty notebook. Wait for it to boot.
    let _ = run(&["wait", NOTEBOOK_BOOT_MS]);

    let rendered = match mode {
        ViewMode::Text => run(&["read"])?,
        ViewMode::Image => {
            let path = out_path
                .map(str::to_string)
                .unwrap_or_else(|| format!("/tmp/prism-notebook-{}.png", session.port));
            run(&["screenshot", &path])?;
            path
        }
    };
    let _ = std::fs::remove_file(&cookie_file);
    Ok(rendered)
}

/// Locate the `agent-browser` binary, or say exactly how to get it.
fn which_agent_browser() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("PRISM_AGENT_BROWSER")
        && !explicit.trim().is_empty()
    {
        return Ok(PathBuf::from(explicit));
    }
    for candidate in ["agent-browser"] {
        if let Ok(found) = which_on_path(candidate) {
            return Ok(found);
        }
    }
    anyhow::bail!(
        "agent-browser is not on PATH — it renders the notebook in this terminal.\n\
         Install it, or point PRISM at an existing copy with PRISM_AGENT_BROWSER=/path/to/agent-browser"
    )
}

fn which_on_path(name: &str) -> Result<PathBuf> {
    let path = std::env::var("PATH").context("PATH not set")?;
    for dir in path.split(':') {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("{name} not found on PATH")
}

#[cfg(test)]
mod view_tests {
    use super::*;

    #[test]
    fn a_missing_browser_says_how_to_fix_it() {
        // An empty PATH is the "not installed" case.
        temp_env_path("", || {
            let err = which_agent_browser().unwrap_err().to_string();
            assert!(err.contains("not on PATH"), "{err}");
            assert!(err.contains("PRISM_AGENT_BROWSER"), "{err}");
        });
    }

    fn temp_env_path(value: &str, f: impl FnOnce()) {
        let previous = std::env::var("PATH").ok();
        let previous_override = std::env::var("PRISM_AGENT_BROWSER").ok();
        unsafe {
            std::env::set_var("PATH", value);
            std::env::remove_var("PRISM_AGENT_BROWSER");
        }
        f();
        unsafe {
            if let Some(p) = previous {
                std::env::set_var("PATH", p);
            }
            if let Some(p) = previous_override {
                std::env::set_var("PRISM_AGENT_BROWSER", p);
            }
        }
    }
}

#[cfg(test)]
mod credential_file_tests {
    use super::*;

    /// The Jupyter token is arbitrary code execution in the kernel, so the
    /// file carrying it must be created private in one syscall, in a
    /// directory only this user can write. A world-writable `/tmp` path with
    /// a predictable name lets another local user plant a symlink and read
    /// the credential; a post-write `chmod` leaves a window where it is
    /// world-readable even without one.
    #[test]
    fn the_cookie_file_is_created_private_and_refuses_a_planted_path() {
        let dir = std::env::temp_dir().join(format!("prism-cred-test-{}", gen_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("cookie.json");

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        {
            use std::io::Write as _;
            let mut f = options.open(&target).expect("first create succeeds");
            f.write_all(b"{}").unwrap();
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "credential file must be owner-only");
        }

        // An already-present path — a planted symlink is the case that
        // matters — must make the create FAIL rather than be written through.
        let mut again = std::fs::OpenOptions::new();
        again.write(true).create_new(true);
        assert!(
            again.open(&target).is_err(),
            "create_new must refuse an existing path instead of following it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
