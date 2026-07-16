//! In-app supervision of the local node daemon.
//!
//! `node up` used to be a CLI-only, foreground verb: the TUI could list nodes
//! but could not bring one up, stop it, or see why it died without dropping to
//! a shell. This module makes the node lifecycle a first-class in-app
//! capability: the backend spawns `prism node up` as a *managed* child (the
//! same current-exe re-invocation pattern the command tools use), remembers
//! the handle, and can stop and report on it later. Both front-ends share it —
//! the TUI palette (`node.up` / `node.stop` / `node.status` → `/node …` slash
//! commands) and the agent's `node` command tool route through here.
//!
//! Supervision is two-layered on purpose:
//! - the **child handle** (this module) — reaping, kill-escalation, startup
//!   log parsing for the session that launched the daemon;
//! - the daemon's own **pid file** — so a node brought up by a previous
//!   session (or `prism node up --background`) is still stoppable through the
//!   same `node_stop` path instead of being orphaned.
//!
//! The child is deliberately *not* `kill_on_drop`: closing the chat UI should
//! not take the user's compute node down with it. The pid-file layer keeps the
//! daemon stoppable from any later session.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use prism_runtime::PrismPaths;
use tokio::process::{Child, Command};

use crate::command_tools::CommandToolRuntime;

/// How long `node_up` watches the daemon log for a startup outcome before
/// reporting "started, unconfirmed". Platform registration is one REST call
/// made before the daemon loop, so it lands well inside this window.
const STARTUP_WAIT: Duration = Duration::from_secs(25);
const STARTUP_POLL: Duration = Duration::from_millis(250);

/// A node daemon child this backend session started and still tracks.
struct SupervisedNode {
    child: Child,
    pid: u32,
    node_id: Option<String>,
    log_path: PathBuf,
    started_at: Instant,
}

/// Read-only view of the supervised daemon for status displays.
#[derive(Debug, Clone)]
pub struct SupervisedSnapshot {
    pub pid: u32,
    pub node_id: Option<String>,
    pub log_path: PathBuf,
    pub uptime: Duration,
}

/// One supervised node per backend process — the same invariant the pid file
/// enforces machine-wide.
static SUPERVISED: Mutex<Option<SupervisedNode>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<SupervisedNode>> {
    SUPERVISED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Bring the local node daemon up as a supervised child of this process.
///
/// `extra_args` is appended verbatim to `prism node up` (e.g. `--name`,
/// `--broadcast`, `--visibility`), so the in-app surface accepts exactly what
/// the CLI accepts — except `--background`, which would detach the daemon out
/// from under supervision and is rejected.
///
/// Returns a human-readable startup report (pid, platform node id when
/// registration is confirmed, dashboard URL, log path). The report never
/// overclaims: if registration failed or wasn't confirmed inside the startup
/// window, it says so.
pub async fn node_up(runtime: &CommandToolRuntime, extra_args: &[String]) -> Result<String> {
    if extra_args.iter().any(|arg| arg == "--background") {
        bail!(
            "--background is not needed here: the node already runs as a supervised \
             background child. Drop the flag and retry."
        );
    }

    // One supervised node per session; reap a child that already exited so a
    // crashed daemon doesn't block a restart.
    {
        let mut guard = lock();
        if let Some(sup) = guard.as_mut() {
            match sup.child.try_wait() {
                Ok(Some(_)) => *guard = None,
                _ => bail!(
                    "a supervised node is already running (pid {}) — stop it first with node.stop",
                    sup.pid
                ),
            }
        }
    }

    let paths = PrismPaths::discover().context("failed to locate PRISM state directories")?;
    if let Some(pid) = prism_node::daemon::running_daemon_pid(&paths) {
        bail!(
            "a node daemon is already running on this machine (pid {pid}) — \
             stop it first with node.stop"
        );
    }

    std::fs::create_dir_all(&paths.state_dir)?;
    let log_path = paths.state_dir.join("node.log");
    // Truncate per launch — same policy as `prism node up --background`.
    let log_file = std::fs::File::create(&log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;

    let mut cmd = Command::new(&runtime.current_exe);
    cmd.arg("--project-root")
        .arg(&runtime.project_root)
        .arg("--python")
        .arg(&runtime.python_bin)
        .arg("node")
        .arg("up")
        .args(extra_args)
        .current_dir(&runtime.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));
    // No kill_on_drop: the node must outlive the chat session (see module
    // docs); the pid file keeps it stoppable from any later session.
    let mut child = cmd.spawn().context("failed to spawn the node daemon")?;
    let pid = child
        .id()
        .context("node daemon spawned without a pid (already reaped?)")?;

    // Watch the daemon log until the startup outcome is known.
    let deadline = Instant::now() + STARTUP_WAIT;
    let mut node_id: Option<String>;
    let mut note: Option<String> = None;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!(
                "node daemon exited during startup ({status}).\n--- log tail ---\n{}",
                log_tail(&log_path, 30)
            );
        }
        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        node_id = parse_registered_node_id(&log_text);
        if node_id.is_some() {
            break;
        }
        if log_text.contains("Platform registration failed") || log_text.contains("No credentials")
        {
            note = Some(
                "started, but platform registration failed — running unregistered \
                 (see the log for the cause)"
                    .to_string(),
            );
            break;
        }
        if Instant::now() >= deadline {
            note = Some(format!(
                "started; registration not confirmed within {}s — check node.status or the log",
                STARTUP_WAIT.as_secs()
            ));
            break;
        }
        tokio::time::sleep(STARTUP_POLL).await;
    }

    let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
    let dashboard = parse_dashboard_url(&log_text);

    let mut lines = vec![format!("Node daemon started (pid {pid}).")];
    match (&node_id, &note) {
        (Some(id), _) => lines.push(format!("  Registered with platform — node_id {id}")),
        (None, Some(note)) => lines.push(format!("  {note}")),
        (None, None) => {}
    }
    if let Some(url) = dashboard {
        lines.push(format!("  Dashboard: {url}"));
    }
    lines.push(format!("  Log: {}", log_path.display()));
    lines.push("Stop it anytime with node.stop.".to_string());

    *lock() = Some(SupervisedNode {
        child,
        pid,
        node_id,
        log_path,
        started_at: Instant::now(),
    });

    Ok(lines.join("\n"))
}

/// Stop the running node daemon — supervised child or not.
///
/// Runs the same graceful path as `prism node down` (shutdown-request file →
/// daemon deregisters from the platform and exits → SIGTERM only as
/// escalation), then reaps the supervised child if this session owns one so
/// no zombie is left behind.
pub async fn node_stop() -> Result<String> {
    let supervised = lock().take();
    let paths = PrismPaths::discover().context("failed to locate PRISM state directories")?;

    if supervised.is_none() && prism_node::daemon::running_daemon_pid(&paths).is_none() {
        bail!("no running node found — nothing supervised in this session and no live pid file");
    }

    // stop_daemon blocks (bounded waits) — keep it off the async runtime.
    let stop_result = tokio::task::spawn_blocking(move || prism_node::daemon::stop_daemon(&paths))
        .await
        .context("node stop task panicked")?;

    let Some(mut sup) = supervised else {
        return stop_result;
    };

    let mut lines = match stop_result {
        Ok(message) => vec![message],
        // Still reap the child: e.g. the daemon crashed and removed nothing.
        Err(error) => vec![format!("Graceful stop path reported: {error}")],
    };
    match tokio::time::timeout(Duration::from_secs(10), sup.child.wait()).await {
        Ok(Ok(status)) => lines.push(format!(
            "Supervised daemon (pid {}) exited ({status}).",
            sup.pid
        )),
        Ok(Err(error)) => lines.push(format!(
            "Supervised daemon (pid {}) could not be reaped: {error}",
            sup.pid
        )),
        Err(_) => {
            let _ = sup.child.start_kill();
            let _ = sup.child.wait().await;
            lines.push(format!(
                "Supervised daemon (pid {}) killed after the grace period.",
                sup.pid
            ));
        }
    }
    Ok(lines.join("\n"))
}

/// The supervised daemon, if this session has a live one. Reaps an exited
/// child on the way (so status never reports a dead pid as running).
pub fn supervised_snapshot() -> Option<SupervisedSnapshot> {
    let mut guard = lock();
    let sup = guard.as_mut()?;
    if let Ok(Some(_)) = sup.child.try_wait() {
        *guard = None;
        return None;
    }
    Some(SupervisedSnapshot {
        pid: sup.pid,
        node_id: sup.node_id.clone(),
        log_path: sup.log_path.clone(),
        uptime: sup.started_at.elapsed(),
    })
}

/// Pid of a live node daemon on this machine (supervised by us or not).
pub fn machine_daemon_pid() -> Option<u32> {
    PrismPaths::discover()
        .ok()
        .and_then(|paths| prism_node::daemon::running_daemon_pid(&paths))
}

/// Extract the platform node id from the daemon startup log. The CLI prints
/// `✓ Registered with platform (node_id: <uuid>)` on successful registration
/// (crates/cli/src/main.rs keeps that shape stable for this parser).
fn parse_registered_node_id(log: &str) -> Option<String> {
    let marker = "Registered with platform (node_id: ";
    let start = log.find(marker)? + marker.len();
    let rest = &log[start..];
    let end = rest.find(')')?;
    let id = rest[..end].trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Extract the dashboard URL from the daemon startup log
/// (`✓ Dashboard    http://localhost:<port>`).
fn parse_dashboard_url(log: &str) -> Option<String> {
    log.lines()
        .find(|line| line.contains("Dashboard"))
        .and_then(|line| line.split_whitespace().find(|tok| tok.starts_with("http")))
        .map(str::to_string)
}

fn log_tail(path: &std::path::Path, max_lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_id_from_registration_line() {
        let log = "  Starting services...\n  \u{2713} Registered with platform (node_id: 0f8c2a44-1111-4222-b333-abcdefabcdef)\n  \u{2713} Dashboard    http://localhost:7327\n";
        assert_eq!(
            parse_registered_node_id(log).as_deref(),
            Some("0f8c2a44-1111-4222-b333-abcdefabcdef")
        );
    }

    #[test]
    fn no_node_id_when_registration_absent_or_malformed() {
        assert_eq!(parse_registered_node_id("  Warning: No credentials"), None);
        assert_eq!(
            parse_registered_node_id("Registered with platform (node_id: "),
            None
        );
    }

    #[test]
    fn parses_dashboard_url() {
        let log =
            "  \u{2713} Dashboard    http://localhost:7327\n  \u{2713} Mesh: passive discovery\n";
        assert_eq!(
            parse_dashboard_url(log).as_deref(),
            Some("http://localhost:7327")
        );
        assert_eq!(parse_dashboard_url("no dashboard line"), None);
    }
}
