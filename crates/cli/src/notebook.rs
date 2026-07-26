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
