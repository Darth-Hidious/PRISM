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
    fs::write(&path, json)?;
    // Restrict permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
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
fn gen_token() -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("prism-{:x}", seed)
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
    let child = Command::new(&python)
        .args([
            "-m",
            "jupyterlab",
            "--no-browser",
            "--port",
            &port.to_string(),
            &format!("--ServerApp.token={token}"),
            "--ServerApp.allow_origin=*",
        ])
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
