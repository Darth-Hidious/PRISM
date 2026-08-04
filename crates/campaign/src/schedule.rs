// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Durable schedules and watchers — what wakes a long-running goal back up.
//!
//! # The gap this closes
//!
//! The campaign engine already makes a goal *durable*: it checkpoints to
//! `~/.prism/campaigns/{id}.json`, resumes cold via
//! [`Campaign::from_checkpoint`], caps iterations and spend, and pauses at
//! approval gates. What it never had is anything that **wakes it up**. A goal
//! whose worker was killed (reboot, OOM, `kill -9`) stays stopped until a
//! human types `prism campaign continue`. For a goal that is supposed to run
//! for months, that human *is* the babysitter.
//!
//! This module is the missing half: a durable registry of triggers, and a
//! `tick` that turns due triggers into resumes.
//!
//! # Why the registry is durable but the ticking is delegated
//!
//! An in-process scheduler (a `tokio::time::interval` inside `prism node`)
//! dies with the process — which is the exact failure it is supposed to
//! recover from. So the heartbeat is delegated to whatever already supervises
//! processes on the host: launchd, a systemd timer, cron, or a container
//! runtime's restart policy. All of them do one thing: run `prism schedule
//! tick` periodically.
//!
//! Crucially the OS owns *only* the heartbeat — **one** unit, installed once.
//! Every individual schedule is a row in this store, so the agent creates,
//! lists and cancels schedules with plain database writes and never needs
//! `launchctl`, root, or a human. That is what makes scheduling a first-class
//! agent capability rather than an operator chore.
//!
//! # Storage
//!
//! Pod-local embedded libSQL (`turso`) at `~/.prism/schedules.db` — a file,
//! not a co-located database server. A **separate** file from
//! `~/.prism/provenance.db` on purpose: the provenance path is high-rate and
//! append-only, and holding a write lock on it once a minute for scheduler
//! bookkeeping would contend with it for no benefit.
//!
//! # Watchers
//!
//! A watcher fires when a condition *becomes* true (edge, not level). There is
//! no event bus in this workspace to hook into — the only `broadcast::channel`
//! is the server's in-process websocket fan-out (`crates/server/src/lib.rs`),
//! which dies with the process and so cannot carry a months-long watch, and no
//! filesystem-notify dependency exists at all. Rather than introduce a second
//! event system, watchers are evaluated on the same `tick` heartbeat that
//! drives clock schedules: one wake-up path, one place to reason about.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use turso::Value;

use crate::{Campaign, GoalStatus};

/// A watcher condition that cannot be evaluated *yet* but plausibly will be —
/// the classic case being `watch_goal` on a goal that has not written its
/// first checkpoint. Distinguished from a hard error so a tick can wait a
/// bounded number of rounds, instead of either retiring the schedule on the
/// first tick or waiting forever on a typo'd id.
#[derive(Debug)]
struct WatcherPending;

impl std::fmt::Display for WatcherPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the watched target does not exist yet")
    }
}

impl std::error::Error for WatcherPending {}

/// What causes a schedule to fire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Fire every `seconds`. The simplest recurring wake-up.
    Every { seconds: u64 },
    /// Fire on a cron expression. Accepts standard 5-field cron
    /// (`min hour dom mon dow`) — normalized to the 6-field seconds-first
    /// form the parser wants — or 6/7-field directly.
    Cron { expr: String },
    /// One-shot: fire once at this unix timestamp, then the schedule is done.
    At { unix_secs: i64 },
    /// Watcher: fire when `path` appears.
    WatchFile { path: PathBuf },
    /// Watcher: fire when another goal reaches `status` (a job finishing).
    WatchGoal { goal_id: String, status: String },
    /// Watcher: fire when the local knowledge graph in `db` holds at least
    /// `at_least` entities (a corpus growing).
    ///
    /// `tenant` scopes the count. `None` counts every tenant's rows
    /// together, which is only ever what you want on a single-tenant node —
    /// an unscoped count is the same landmine the `:Chunk` merge keys have.
    WatchCorpus {
        db: PathBuf,
        at_least: i64,
        #[serde(default)]
        tenant: Option<String>,
    },
}

impl Trigger {
    fn is_watch(&self) -> bool {
        matches!(
            self,
            Self::WatchFile { .. } | Self::WatchGoal { .. } | Self::WatchCorpus { .. }
        )
    }

    /// Next fire time after `after` (unix secs) for clock triggers.
    /// Watchers have no clock due time — they are polled every tick.
    fn next_due(&self, after: i64) -> Result<Option<i64>> {
        match self {
            Self::Every { seconds } => {
                if *seconds == 0 {
                    bail!("interval schedules need a non-zero period");
                }
                Ok(Some(after + *seconds as i64))
            }
            Self::At { unix_secs } => Ok(Some(*unix_secs)),
            Self::Cron { expr } => {
                let schedule = cron::Schedule::from_str(&normalize_cron(expr))
                    .with_context(|| format!("invalid cron expression: {expr}"))?;
                let from = chrono::DateTime::from_timestamp(after, 0)
                    .ok_or_else(|| anyhow::anyhow!("cron base timestamp {after} out of range"))?;
                schedule
                    .after(&from)
                    .next()
                    .map(|dt| Some(dt.timestamp()))
                    // A cron expression with no future occurrence (e.g. a
                    // past-only year field) is a defect to surface, not a
                    // schedule that quietly never runs.
                    .ok_or_else(|| {
                        anyhow::anyhow!("cron expression '{expr}' has no next fire time")
                    })
            }
            _ => Ok(None),
        }
    }

    /// Evaluate a watcher's condition right now. Errors (unreadable file
    /// metadata, missing graph table) propagate — an unevaluable condition is
    /// a defect to report, never a silent `false`.
    async fn condition_met(&self) -> Result<bool> {
        match self {
            Self::WatchFile { path } => Ok(path.exists()),
            Self::WatchGoal { goal_id, status } => {
                let path = campaigns_dir().join(format!("{goal_id}.json"));
                if !path.exists() {
                    // "Not started yet" and "you typo'd the id" look the
                    // same from here, and chaining a goal onto one that is
                    // still coming up is the normal case. Report it as a
                    // condition that is not met YET; the caller's
                    // no-progress budget bounds how long we wait before
                    // calling it a mistake.
                    return Err(WatcherPending.into());
                }
                let campaign = Campaign::from_checkpoint(&path)?;
                Ok(campaign
                    .state()
                    .status
                    .as_str()
                    .eq_ignore_ascii_case(status))
            }
            Self::WatchCorpus {
                db,
                at_least,
                tenant,
            } => {
                let db_str = db
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("corpus db path is not UTF-8: {db:?}"))?;
                let conn = turso::Builder::new_local(db_str)
                    .build()
                    .await
                    .with_context(|| format!("failed to open corpus db {db_str}"))?
                    .connect()?;
                let (sql, params): (&str, Vec<Value>) = match tenant {
                    Some(t) => (
                        "SELECT count(*) FROM emmo_entity WHERE tenant = ?1",
                        vec![Value::Text(t.clone())],
                    ),
                    None => ("SELECT count(*) FROM emmo_entity", Vec::new()),
                };
                let mut rows = conn
                    .query(sql, params)
                    .await
                    .context("corpus watcher: emmo_entity is not queryable in that database")?;
                let row = rows
                    .next()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("corpus count returned no row"))?;
                let count = match row.get_value(0)? {
                    Value::Integer(i) => i,
                    other => bail!("corpus count returned a non-integer: {other:?}"),
                };
                Ok(count >= *at_least)
            }
            _ => Ok(false),
        }
    }
}

/// `cron` parses 6- or 7-field expressions (seconds first). Operators write
/// 5-field crontab lines. Accept both by prefixing `0 ` (fire on the minute).
fn normalize_cron(expr: &str) -> String {
    if expr.split_whitespace().count() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

/// Parse `30s` / `15m` / `6h` / `2d` into seconds. Bare digits are seconds.
pub fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3_600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        _ => (s, 1),
    };
    let n: u64 = digits
        .parse()
        .with_context(|| format!("bad duration '{s}' — use e.g. 30s, 15m, 6h, 2d"))?;
    if n == 0 {
        bail!("duration must be greater than zero");
    }
    Ok(n * mult)
}

/// Lifecycle of a schedule row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    /// Firing normally.
    Active,
    /// Stopped because the goal made no progress across repeated wake-ups.
    Wedged,
    /// Stopped for a benign terminal reason (goal completed, ceiling reached).
    Done,
    /// Stopped by a human or the agent.
    Cancelled,
}

impl ScheduleState {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Wedged => "wedged",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "wedged" => Self::Wedged,
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            _ => Self::Active,
        }
    }
}

/// One durable schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    /// The goal this schedule wakes up.
    pub goal_id: String,
    pub trigger: Trigger,
    pub state: ScheduleState,
    /// Next clock due time (unix secs). `None` for watchers.
    pub next_due_at: Option<i64>,
    pub last_fired_at: Option<i64>,
    pub fires: u32,
    /// Hard ceiling on how many times this schedule may resume the goal.
    /// This is the always-enforceable spend guard: unlike the USD ceiling it
    /// does not depend on any step reporting a cost.
    pub max_fires: u32,
    /// Progress fingerprint recorded at the last fire.
    pub progress_fp: String,
    pub no_progress: u32,
    /// Stop and report after this many consecutive no-progress wake-ups.
    pub max_no_progress: u32,
    /// Whether the watcher condition was true at the previous tick (edge
    /// detection — a watcher fires on false→true, not on every tick while
    /// the condition holds).
    pub cond_was_true: bool,
    /// Human-readable record of what the last tick decided. Never empty
    /// once a tick has looked at this row.
    pub last_outcome: String,
    pub created_at: String,
}

/// What a tick decided about one schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The goal was resumed; the worker's pid.
    Fired(u32),
    /// Not fired this tick, schedule stays active. Carries the reason.
    Skipped(String),
    /// Schedule moved to a terminal state. Carries the reason.
    Stopped(String),
}

impl Decision {
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Fired(pid) => format!("resumed goal (worker pid {pid})"),
            Self::Skipped(r) | Self::Stopped(r) => r.clone(),
        }
    }
}

/// The state of a goal, as far as the scheduler needs to know it.
#[derive(Debug, Clone)]
pub struct GoalSnapshot {
    pub status: GoalStatus,
    pub iteration: usize,
    /// Units of work done (candidates evaluated, or research steps).
    pub work_done: usize,
    pub spent_usd: f64,
    pub budget_usd: Option<f64>,
    /// True when the goal is paused and therefore waiting on a person.
    ///
    /// Every pause the campaign engine can produce is an approval gate — the
    /// materials loop pauses only at `approval_gate_at`, and so does
    /// `run_research`. So "paused" *is* "waiting for a human", and this is
    /// derived from the pause itself rather than from `gates_hit`.
    ///
    /// Deriving it from `gates_hit` looked more precise and was wrong:
    /// `run_research` pauses without recording a gate, so a research goal
    /// stopped at an approval gate would have read as "not a gate pause" and
    /// been resumed by the scheduler. A wake-up must never stand in for an
    /// approval. If a non-approval pause cause is ever introduced, this is
    /// the line that has to change with it.
    pub paused_for_approval: bool,
    pub completion_reason: String,
}

impl GoalSnapshot {
    /// The progress fingerprint. Two wake-ups that produce the same
    /// fingerprint did no work in between.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!(
            "{}:{}:{}:{:.6}",
            self.iteration,
            self.work_done,
            self.status.as_str(),
            self.spent_usd
        )
    }
}

/// How the scheduler reads and restarts goals. A trait so the tick engine is
/// testable without spawning processes.
pub trait GoalResumer: Send + Sync {
    /// Read the goal's durable state. `Err` when the checkpoint is missing or
    /// unreadable — the scheduler surfaces that, it does not assume.
    fn snapshot(&self, goal_id: &str) -> Result<GoalSnapshot>;
    /// True when a worker process for this goal is still alive.
    fn worker_alive(&self, goal_id: &str) -> bool;
    /// Start a worker that continues the goal. Returns its pid.
    ///
    /// Contract: return `Err` **only** when no process was started. Once a
    /// worker exists it is spending money, and the scheduler retires a
    /// schedule whose resume errored — reporting a failure after a
    /// successful spawn would leave that worker running untracked behind a
    /// schedule that never fires again.
    fn resume(&self, goal_id: &str) -> Result<u32>;
}

// ── The real resumer: campaign checkpoints + a detached worker ───────

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.prism/campaigns`, or `$PRISM_CAMPAIGNS_DIR` when set (tests, pods).
#[must_use]
pub fn campaigns_dir() -> PathBuf {
    std::env::var_os("PRISM_CAMPAIGNS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".prism").join("campaigns"))
}

/// `~/.prism/schedules.db`, or `$PRISM_SCHEDULES_DB` when set.
#[must_use]
pub fn default_db_path() -> PathBuf {
    std::env::var_os("PRISM_SCHEDULES_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".prism").join("schedules.db"))
}

/// Resumes goals by spawning `<exe> campaign continue <id>` — the *same*
/// detached-worker path `prism campaign start --detach` already uses, so a
/// scheduled resume and a human resume are one code path and one audit trail.
pub struct WorkerResumer {
    /// The `prism` executable to re-invoke.
    pub exe: PathBuf,
}

/// How long after a spawn the parent's pid record still counts as "a worker
/// is coming up", before the worker's own lock has to be the proof. Covers
/// the milliseconds between `spawn` returning and the child taking its lock.
const WORKER_HANDOFF_SECS: u64 = 30;

/// A heartbeat older than this means the host stopped ticking. Generous
/// enough for an hourly timer, tight enough to notice a dead supervisor.
const HEARTBEAT_STALE_SECS: i64 = 2 * 3600;

impl WorkerResumer {
    /// Path of the lock file a worker for `goal_id` holds for its lifetime.
    #[must_use]
    pub fn worker_pid_path(goal_id: &str) -> PathBuf {
        campaigns_dir().join(format!("{goal_id}.worker"))
    }

    /// Record `pid` as the worker just spawned for `goal_id`. This is the
    /// short-lived handoff record; the durable liveness signal is the lock
    /// the worker itself takes via [`WorkerLock::acquire`].
    pub fn write_worker_pid(goal_id: &str, pid: u32) -> Result<()> {
        let path = Self::worker_pid_path(goal_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, pid.to_string())
            .with_context(|| format!("failed to write worker pid file {}", path.display()))
    }
}

/// The liveness token a campaign worker holds for as long as it is running.
///
/// This replaced "read a pid file and `kill(pid, 0)`", which had two ways to
/// lie over a months-long goal: a `kill -9`'d worker leaves its pid file
/// behind, and once the OS recycles that pid every future tick reads "still
/// running" forever — a crashed goal silently never resumed, which is the
/// exact failure this feature exists to remove. An `flock` cannot lie: the
/// kernel drops it when the process dies, SIGKILL included.
pub struct WorkerLock {
    #[allow(dead_code)]
    file: std::fs::File,
    path: PathBuf,
}

impl WorkerLock {
    /// Take the lock for `goal_id` and record this process's pid in it.
    /// `Ok(None)` means another worker already holds it — the caller must not
    /// start a second loop over the same goal.
    pub fn acquire(goal_id: &str) -> Result<Option<Self>> {
        let path = WorkerResumer::worker_pid_path(goal_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open worker lock {}", path.display()))?;
        if !try_lock_exclusive(&file)? {
            return Ok(None);
        }
        use std::io::{Seek, Write};
        file.set_len(0)?;
        file.rewind()?;
        write!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Some(Self { file, path }))
    }
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        // Truncate rather than delete: another worker may already be blocked
        // on this exact inode, and removing the file would let it lock a new
        // one and run alongside. The kernel releases our flock when `file`
        // closes, which is the signal that matters.
        if let Err(e) = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
        {
            warn!(goal = %self.path.display(), error = %e, "could not clear worker lock file");
        }
    }
}

/// `Ok(false)` when another process holds the lock. Any other failure is an
/// error: we cannot prove exclusivity, and guessing risks a duplicate worker.
fn try_lock_exclusive(file: &std::fs::File) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `file` owns the fd for the duration of the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let err = std::io::Error::last_os_error();
            return match err.kind() {
                std::io::ErrorKind::WouldBlock => Ok(false),
                _ => Err(anyhow::anyhow!("lock unavailable: {err}")),
            };
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        // No advisory lock wired up on this platform yet. Say so instead of
        // reporting a liveness answer we cannot back up.
        Err(anyhow::anyhow!(
            "worker liveness locking is not implemented on this platform"
        ))
    }
}

impl GoalResumer for WorkerResumer {
    fn snapshot(&self, goal_id: &str) -> Result<GoalSnapshot> {
        let path = campaigns_dir().join(format!("{goal_id}.json"));
        let campaign = Campaign::from_checkpoint(&path)?;
        let s = campaign.state();
        Ok(GoalSnapshot {
            status: s.status,
            iteration: s.current_iteration,
            work_done: s.total_evaluated() + s.research_outcomes.len(),
            spent_usd: s.total_cost_usd,
            budget_usd: s.config.budget_usd,
            // The legacy `paused` flag is checked alongside `status` because
            // `run_research` sets only the flag: reading `status` alone would
            // let a research goal stopped at an approval gate look resumable.
            paused_for_approval: s.status == GoalStatus::Paused || s.paused,
            completion_reason: s.completion_reason.clone(),
        })
    }

    fn worker_alive(&self, goal_id: &str) -> bool {
        let path = Self::worker_pid_path(goal_id);
        // Primary signal: can we take the worker's lock? If not, a worker
        // holds it and is by definition alive. If we can, we drop it again
        // immediately — probing must never leave the lock held.
        match std::fs::OpenOptions::new().write(true).open(&path) {
            Ok(file) => match try_lock_exclusive(&file) {
                Ok(false) => return true,
                Ok(true) => {}
                // We could not determine exclusivity. Reporting "dead" would
                // spawn a second worker over a possibly-live one; reporting
                // "alive" only costs a delayed resume. Take the cheap error.
                Err(e) => {
                    warn!(goal = goal_id, error = %e, "worker liveness undetermined — assuming alive");
                    return true;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
            Err(e) => {
                warn!(goal = goal_id, error = %e, "worker lock unreadable — assuming alive");
                return true;
            }
        }
        // The lock is free, so no worker holds it. One case remains: we
        // spawned a worker moments ago and it has not reached its lock yet.
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age.as_secs() < WORKER_HANDOFF_SECS);
        fresh
            && std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
                .is_some_and(|pid| {
                    #[cfg(unix)]
                    // SAFETY: signal 0 performs error checking only, it sends
                    // nothing. Only consulted inside the seconds-wide handoff
                    // window, so pid recycling is not a concern here.
                    unsafe {
                        libc::kill(pid as libc::pid_t, 0) == 0
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = pid;
                        false
                    }
                })
    }

    fn resume(&self, goal_id: &str) -> Result<u32> {
        let mut cmd = std::process::Command::new(&self.exe);
        cmd.args(["campaign", "continue", goal_id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn worker for goal '{goal_id}'"))?;
        let pid = child.id();
        // The spawn already happened and is already spending. Failing here
        // would retire the schedule and orphan a live, billing worker behind
        // it, so this is a warning: the worker's own lock is the durable
        // signal anyway, and it takes that within milliseconds.
        if let Err(e) = Self::write_worker_pid(goal_id, pid) {
            warn!(goal = goal_id, pid, error = %e, "worker spawned but its pid could not be recorded");
        }
        Ok(pid)
    }
}

// ── Store ───────────────────────────────────────────────────────────

/// Durable schedule registry — embedded libSQL, one file, no server.
pub struct ScheduleStore {
    conn: turso::Connection,
    /// Sibling lock file guarding [`tick_once`]. Two overlapping ticks would
    /// each read the same due schedule and each start a worker for it; the
    /// `worker_alive` check narrows that to a race, and this closes it.
    /// launchd and systemd will not run two copies of one unit, but plain
    /// `cron` will, and so will a human running `prism schedule tick` while
    /// the timer fires.
    lock_path: PathBuf,
}

impl ScheduleStore {
    pub async fn open(path: &Path) -> Result<Self> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("schedules database path is not UTF-8: {path:?}"))?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = turso::Builder::new_local(path_str)
            .build()
            .await
            .context("failed to open schedules database")?
            .connect()?;
        conn.execute(
            r#"CREATE TABLE IF NOT EXISTS schedules (
                id TEXT PRIMARY KEY,
                goal_id TEXT NOT NULL,
                trigger_json TEXT NOT NULL,
                state TEXT NOT NULL,
                next_due_at INTEGER,
                last_fired_at INTEGER,
                fires INTEGER NOT NULL DEFAULT 0,
                max_fires INTEGER NOT NULL,
                progress_fp TEXT NOT NULL DEFAULT '',
                no_progress INTEGER NOT NULL DEFAULT 0,
                max_no_progress INTEGER NOT NULL,
                cond_was_true INTEGER NOT NULL DEFAULT 0,
                last_outcome TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL
            )"#,
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sched_goal ON schedules(goal_id)",
            (),
        )
        .await?;
        // One row: when a tick last actually ran. This is the ONLY honest
        // answer to "is anything on this host driving these schedules?" —
        // the unit file existing on disk proves nothing (it may never have
        // been loaded), and it cannot see a `schedule daemon` running in
        // another process at all.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schedule_meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)",
            (),
        )
        .await?;
        // An in-memory store has no file to sit a lock next to (and two of
        // them are genuinely independent), so give it a private one.
        let lock_path = if path_str == ":memory:" {
            std::env::temp_dir().join(format!("prism-sched-{}.tick.lock", uuid::Uuid::new_v4()))
        } else {
            path.with_extension("tick.lock")
        };
        Ok(Self { conn, lock_path })
    }

    /// Open the default pod-local store.
    pub async fn open_default() -> Result<Self> {
        Self::open(&default_db_path()).await
    }

    /// Unix seconds of the last completed tick, or `None` when nothing has
    /// ever ticked this store. `None` means every schedule here is inert.
    pub async fn last_tick_at(&self) -> Result<Option<i64>> {
        let mut rows = self
            .conn
            .query("SELECT v FROM schedule_meta WHERE k = 'last_tick_at'", ())
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        match row.get_value(0)? {
            Value::Text(t) => Ok(t.parse::<i64>().ok()),
            Value::Integer(i) => Ok(Some(i)),
            _ => Ok(None),
        }
    }

    async fn record_tick(&self, now: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO schedule_meta (k, v) VALUES ('last_tick_at', ?1) \
                 ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                [Value::Text(now.to_string())],
            )
            .await?;
        Ok(())
    }

    /// One line saying whether anything is actually driving this store, for
    /// humans and for the agent. Never claims health it cannot demonstrate.
    pub async fn heartbeat_status(&self, now: i64) -> String {
        match self.last_tick_at().await {
            Ok(Some(last)) => {
                let age = (now - last).max(0);
                if age <= HEARTBEAT_STALE_SECS {
                    format!("heartbeat OK — last tick {age}s ago")
                } else {
                    format!(
                        "heartbeat STALE — last tick was {age}s ago. Nothing has run \
                         `prism schedule tick` since then, so these schedules are not firing."
                    )
                }
            }
            Ok(None) => "heartbeat MISSING — nothing has ever run `prism schedule tick` on this \
                         host, so no schedule here can fire. Install it with `prism schedule \
                         install --write` (then run the command it prints), or run `prism \
                         schedule daemon` inside a supervised container."
                .to_string(),
            Err(e) => format!("heartbeat UNKNOWN — could not read the tick record: {e:#}"),
        }
    }

    /// Register a schedule. `max_fires` is required and finite on purpose:
    /// an autonomous loop with no wake-up ceiling is unbounded spend.
    pub async fn create(
        &self,
        goal_id: &str,
        trigger: Trigger,
        max_fires: u32,
        max_no_progress: u32,
    ) -> Result<Schedule> {
        if goal_id.trim().is_empty() {
            bail!("a schedule needs a goal id to wake up");
        }
        if max_fires == 0 {
            bail!(
                "max_fires must be at least 1 — a schedule that can never fire is not a schedule"
            );
        }
        if max_no_progress == 0 {
            bail!("max_no_progress must be at least 1");
        }
        let now = Utc::now().timestamp();
        let next_due_at = trigger.next_due(now)?;
        let sched = Schedule {
            id: format!("sched-{}", uuid::Uuid::new_v4().simple()),
            goal_id: goal_id.to_string(),
            trigger,
            state: ScheduleState::Active,
            next_due_at,
            last_fired_at: None,
            fires: 0,
            max_fires,
            progress_fp: String::new(),
            no_progress: 0,
            max_no_progress,
            cond_was_true: false,
            last_outcome: String::new(),
            created_at: Utc::now().to_rfc3339(),
        };
        self.conn
            .execute(
                "INSERT INTO schedules (id, goal_id, trigger_json, state, next_due_at, \
                 last_fired_at, fires, max_fires, progress_fp, no_progress, max_no_progress, \
                 cond_was_true, last_outcome, created_at) \
                 VALUES (?1,?2,?3,?4,?5,NULL,0,?6,'',0,?7,0,'',?8)",
                (
                    sched.id.clone(),
                    sched.goal_id.clone(),
                    serde_json::to_string(&sched.trigger)?,
                    sched.state.as_str().to_string(),
                    sched.next_due_at.map_or(Value::Null, Value::Integer),
                    i64::from(sched.max_fires),
                    i64::from(sched.max_no_progress),
                    sched.created_at.clone(),
                ),
            )
            .await?;
        info!(schedule = %sched.id, goal = %sched.goal_id, "schedule created");
        Ok(sched)
    }

    pub async fn list(&self) -> Result<Vec<Schedule>> {
        let mut rows = self
            .conn
            .query("SELECT * FROM schedules ORDER BY created_at", ())
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(row_to_schedule(&row)?);
        }
        Ok(out)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        let mut rows = self
            .conn
            .query(
                "SELECT * FROM schedules WHERE id = ?1",
                [Value::Text(id.to_string())],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(row_to_schedule(&row)?)),
            None => Ok(None),
        }
    }

    /// Cancel a schedule. Returns `false` when no such schedule exists — an
    /// honest "nothing to cancel", not a silent success.
    pub async fn cancel(&self, id: &str) -> Result<bool> {
        if self.get(id).await?.is_none() {
            return Ok(false);
        }
        self.conn
            .execute(
                "UPDATE schedules SET state = 'cancelled', last_outcome = 'cancelled' \
                 WHERE id = ?1",
                [Value::Text(id.to_string())],
            )
            .await?;
        Ok(true)
    }

    async fn persist(&self, s: &Schedule) -> Result<()> {
        self.conn
            .execute(
                "UPDATE schedules SET state=?2, next_due_at=?3, last_fired_at=?4, fires=?5, \
                 progress_fp=?6, no_progress=?7, cond_was_true=?8, last_outcome=?9 WHERE id=?1",
                (
                    s.id.clone(),
                    s.state.as_str().to_string(),
                    s.next_due_at.map_or(Value::Null, Value::Integer),
                    s.last_fired_at.map_or(Value::Null, Value::Integer),
                    i64::from(s.fires),
                    s.progress_fp.clone(),
                    i64::from(s.no_progress),
                    i64::from(s.cond_was_true),
                    s.last_outcome.clone(),
                ),
            )
            .await?;
        Ok(())
    }
}

fn row_to_schedule(row: &turso::Row) -> Result<Schedule> {
    fn text(row: &turso::Row, i: usize) -> Result<String> {
        match row.get_value(i)? {
            Value::Text(t) => Ok(t),
            Value::Null => Ok(String::new()),
            other => bail!("schedules column {i}: expected text, got {other:?}"),
        }
    }
    fn int(row: &turso::Row, i: usize) -> Result<Option<i64>> {
        match row.get_value(i)? {
            Value::Integer(v) => Ok(Some(v)),
            Value::Null => Ok(None),
            other => bail!("schedules column {i}: expected integer, got {other:?}"),
        }
    }
    Ok(Schedule {
        id: text(row, 0)?,
        goal_id: text(row, 1)?,
        trigger: serde_json::from_str(&text(row, 2)?)
            .context("schedules.trigger_json is not a valid trigger")?,
        state: ScheduleState::parse(&text(row, 3)?),
        next_due_at: int(row, 4)?,
        last_fired_at: int(row, 5)?,
        fires: int(row, 6)?.unwrap_or(0) as u32,
        max_fires: int(row, 7)?.unwrap_or(0) as u32,
        progress_fp: text(row, 8)?,
        no_progress: int(row, 9)?.unwrap_or(0) as u32,
        max_no_progress: int(row, 10)?.unwrap_or(1) as u32,
        cond_was_true: int(row, 11)?.unwrap_or(0) != 0,
        last_outcome: text(row, 12)?,
        created_at: text(row, 13)?,
    })
}

// ── The tick engine ─────────────────────────────────────────────────

/// Held for the duration of a tick so two ticks cannot both resume the same
/// goal. The kernel releases the `flock` when the process exits, including on
/// SIGKILL, so this can never go stale.
struct TickLock(
    /// The lock is the open descriptor itself — the value is never read, it
    /// is held. Dropping the file releases the `flock`.
    #[allow(dead_code)]
    std::fs::File,
);

impl TickLock {
    /// `None` when another tick already holds it — the caller reports that
    /// and does nothing, which is the correct outcome for a duplicate tick.
    fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .with_context(|| format!("failed to open tick lock {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: `file` owns the fd for the whole call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                return match err.kind() {
                    std::io::ErrorKind::WouldBlock => Ok(None),
                    // Any other failure means we cannot prove exclusivity;
                    // refusing to tick is safer than risking a double resume.
                    _ => Err(anyhow::anyhow!("tick lock unavailable: {err}")),
                };
            }
        }
        Ok(Some(Self(file)))
    }
}

impl Drop for TickLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: the descriptor remains owned by `self` until this drop
            // finishes. Closing also unlocks, but making LOCK_UN explicit
            // ensures a completed async tick cannot retain the lock until a
            // delayed descriptor close.
            if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) } != 0 {
                warn!(
                    error = %std::io::Error::last_os_error(),
                    "could not explicitly release schedule tick lock"
                );
            }
        }
    }
}

/// Evaluate every active schedule once and act on the due ones.
///
/// This is what `prism schedule tick` runs, and what the daemon loop calls on
/// each pass. Pure with respect to the clock (`now` is injected) and to
/// process spawning (via [`GoalResumer`]), so the full gate ladder is
/// unit-testable.
///
/// Returns `Err` when another tick is already in flight, so the caller says
/// so rather than reporting an empty, successful-looking pass.
pub async fn tick_once(
    store: &ScheduleStore,
    resumer: &dyn GoalResumer,
    now: i64,
) -> Result<Vec<(String, Decision)>> {
    let Some(lock) = TickLock::try_acquire(&store.lock_path)? else {
        bail!(
            "another `prism schedule tick` is already running — skipping this one so a goal \
             is not resumed twice"
        );
    };
    // Record the heartbeat FIRST: a tick that dies partway through still
    // proves something is driving this store, and `heartbeat_status` must
    // reflect reality rather than only successful passes.
    store.record_tick(now).await?;
    let mut out = Vec::new();
    for mut sched in store.list().await? {
        if sched.state != ScheduleState::Active {
            continue;
        }
        let Some(decision) = evaluate(&mut sched, resumer, now).await else {
            continue; // not due / condition still false — nothing to record
        };
        sched.last_outcome = decision.reason();
        store.persist(&sched).await?;
        match &decision {
            Decision::Fired(pid) => {
                info!(schedule = %sched.id, goal = %sched.goal_id, pid, "schedule fired")
            }
            Decision::Stopped(r) => {
                warn!(schedule = %sched.id, goal = %sched.goal_id, reason = %r, "schedule stopped")
            }
            Decision::Skipped(r) => {
                info!(schedule = %sched.id, goal = %sched.goal_id, reason = %r, "schedule skipped")
            }
        }
        out.push((sched.id.clone(), decision));
    }
    // Release before returning the completed future. Relying only on async
    // temporary destruction left the next tick in the same process seeing
    // this completed tick's flock under parallel test execution.
    drop(lock);
    Ok(out)
}

/// The gate ladder for one schedule. `None` = not due, nothing happened.
///
/// Order matters and is the safety contract:
/// approval gate → budget → wake-up ceiling → no-progress → already-running →
/// fire.
async fn evaluate(sched: &mut Schedule, resumer: &dyn GoalResumer, now: i64) -> Option<Decision> {
    // ── Is it due? ────────────────────────────────────────────────
    if sched.trigger.is_watch() {
        match sched.trigger.condition_met().await {
            Ok(true) => {
                if sched.cond_was_true {
                    return None; // level, not edge — already fired for this
                }
                sched.cond_was_true = true;
            }
            Ok(false) => {
                sched.cond_was_true = false;
                return None;
            }
            // The target isn't there YET — normal when a watcher is created
            // moments after the goal it chains onto. Wait, but on a budget:
            // an id that never appears is a typo, and waiting forever on it
            // silently is the failure this codebase keeps repeating.
            Err(e) if e.downcast_ref::<WatcherPending>().is_some() => {
                sched.no_progress += 1;
                if sched.no_progress >= sched.max_no_progress {
                    sched.state = ScheduleState::Wedged;
                    return Some(Decision::Stopped(format!(
                        "{e} after {} checks — it is not coming; check the id",
                        sched.no_progress
                    )));
                }
                return Some(Decision::Skipped(format!(
                    "{e} — waiting ({} of {} checks used)",
                    sched.no_progress, sched.max_no_progress
                )));
            }
            // Anything else is a real defect: stop and say so rather than
            // treat it as "not yet" forever.
            Err(e) => {
                sched.state = ScheduleState::Wedged;
                return Some(Decision::Stopped(format!(
                    "watcher condition could not be evaluated: {e:#}"
                )));
            }
        }
    } else {
        match sched.next_due_at {
            Some(due) if due <= now => {}
            Some(_) => return None,
            None => {
                sched.state = ScheduleState::Wedged;
                return Some(Decision::Stopped(
                    "clock schedule has no due time — it would never fire".into(),
                ));
            }
        }
    }

    // ── Read the goal. An unreadable goal is reported, not retried blind. ──
    let snap = match resumer.snapshot(&sched.goal_id) {
        Ok(s) => s,
        Err(e) => {
            sched.state = ScheduleState::Wedged;
            return Some(Decision::Stopped(format!(
                "goal '{}' checkpoint unreadable: {e:#}",
                sched.goal_id
            )));
        }
    };

    // ── Terminal goal ─────────────────────────────────────────────
    if snap.status == GoalStatus::Completed {
        sched.state = ScheduleState::Done;
        return Some(Decision::Stopped(format!(
            "goal completed ({}) — nothing left to wake",
            if snap.completion_reason.is_empty() {
                "no reason recorded"
            } else {
                &snap.completion_reason
            }
        )));
    }

    // ── Approval gate: the one thing a schedule must never launder ──
    if snap.paused_for_approval {
        // Stay active: once a human approves, the goal leaves Paused and the
        // next tick picks it up normally.
        advance_clock(sched, now);
        return Some(Decision::Skipped(format!(
            "goal is paused at the approval gate on iteration {} — a human must approve it; \
             a scheduled wake-up is not approval",
            snap.iteration
        )));
    }

    // ── Budget ceiling ────────────────────────────────────────────
    if let Some(ceiling) = snap.budget_usd
        && snap.spent_usd >= ceiling
    {
        sched.state = ScheduleState::Done;
        return Some(Decision::Stopped(format!(
            "budget ceiling reached: ${:.4} spent of ${:.4} — stopping, not continuing silently",
            snap.spent_usd, ceiling
        )));
    }

    // ── Wake-up ceiling: the always-enforceable spend guard ───────
    if sched.fires >= sched.max_fires {
        sched.state = ScheduleState::Done;
        return Some(Decision::Stopped(format!(
            "wake-up ceiling reached ({} of {}) — stopping rather than resuming indefinitely",
            sched.fires, sched.max_fires
        )));
    }

    // ── Already running? Then there is nothing to wake. ───────────
    //
    // This MUST come before the no-progress check. An unchanged checkpoint
    // while the worker is alive means the current iteration has not finished
    // yet — a 20-minute iteration under a 5-minute schedule is healthy, and
    // counting those ticks as "no progress" retired the schedule for a goal
    // that was working fine. Worse than useless: the schedule is then gone
    // when that worker really does die later.
    if resumer.worker_alive(&sched.goal_id) {
        advance_clock(sched, now);
        return Some(Decision::Skipped(
            "goal worker is still running — no resume needed".into(),
        ));
    }

    // ── No-progress detection ─────────────────────────────────────
    //
    // Scope, stated plainly: this compares the checkpoint between wake-ups,
    // so it catches a goal that is not advancing at all (crash-looping,
    // stuck before its first step). It does NOT judge whether the work is
    // any good — a goal that dutifully burns iterations proposing junk has a
    // changing fingerprint and will run to its iteration cap. The wake-up
    // ceiling is the bound on that case.
    let fp = snap.fingerprint();
    if sched.fires > 0 && fp == sched.progress_fp {
        sched.no_progress += 1;
        if sched.no_progress >= sched.max_no_progress {
            sched.state = ScheduleState::Wedged;
            return Some(Decision::Stopped(format!(
                "no progress across {} wake-ups with no worker running (still at {fp}) — goal \
                 is wedged, stopping rather than burning credits on a loop that is not advancing",
                sched.no_progress
            )));
        }
    } else {
        sched.no_progress = 0;
    }

    // ── Fire ──────────────────────────────────────────────────────
    match resumer.resume(&sched.goal_id) {
        Ok(pid) => {
            sched.fires += 1;
            sched.last_fired_at = Some(now);
            sched.progress_fp = fp;
            advance_clock(sched, now);
            if matches!(sched.trigger, Trigger::At { .. }) {
                sched.state = ScheduleState::Done;
            }
            Some(Decision::Fired(pid))
        }
        // `resume` returns Err ONLY when no process was started (see the
        // trait contract) — otherwise retiring the schedule here would
        // orphan a running, billing worker behind a schedule that will
        // never fire again.
        Err(e) => {
            sched.state = ScheduleState::Wedged;
            Some(Decision::Stopped(format!("resume failed: {e:#}")))
        }
    }
}

fn advance_clock(sched: &mut Schedule, now: i64) {
    if sched.trigger.is_watch() {
        return;
    }
    match sched.trigger.next_due(now) {
        Ok(next) => sched.next_due_at = next,
        Err(e) => {
            warn!(schedule = %sched.id, error = %e, "cannot compute next due time");
            sched.state = ScheduleState::Wedged;
        }
    }
}

// ── Operator units ──────────────────────────────────────────────────

/// The launchd agent that owns the heartbeat on macOS. One unit, forever;
/// every individual schedule lives in the database, not here.
#[must_use]
pub fn launchd_plist(exe: &Path, interval_secs: u64) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.mirdyne.prism.schedule</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>schedule</string>
    <string>tick</string>
  </array>
  <key>StartInterval</key><integer>{interval_secs}</integer>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        log = home()
            .join(".prism")
            .join("logs")
            .join("schedule.log")
            .display(),
    )
}

/// The systemd user service + timer that owns the heartbeat on Linux.
#[must_use]
pub fn systemd_units(exe: &Path, interval_secs: u64) -> (String, String) {
    let service = format!(
        "[Unit]\nDescription=PRISM schedule tick\n\n[Service]\nType=oneshot\n\
         ExecStart={exe} schedule tick\n",
        exe = exe.display()
    );
    let timer = format!(
        "[Unit]\nDescription=PRISM schedule heartbeat\n\n[Timer]\n\
         OnBootSec={interval_secs}\nOnUnitActiveSec={interval_secs}\n\
         Unit=prism-schedule.service\n\n[Install]\nWantedBy=timers.target\n"
    );
    (service, timer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A resumer whose goal state the test controls and whose "resume" only
    /// counts calls — no processes, no LLM, no spend.
    struct FakeResumer {
        snap: Mutex<GoalSnapshot>,
        alive: Mutex<bool>,
        resumes: Mutex<Vec<String>>,
        fail: bool,
    }

    fn snapshot(status: GoalStatus, iteration: usize, work: usize) -> GoalSnapshot {
        GoalSnapshot {
            status,
            iteration,
            work_done: work,
            spent_usd: 0.0,
            budget_usd: None,
            paused_for_approval: false,
            completion_reason: String::new(),
        }
    }

    impl FakeResumer {
        fn new(snap: GoalSnapshot) -> Self {
            Self {
                snap: Mutex::new(snap),
                alive: Mutex::new(false),
                resumes: Mutex::new(Vec::new()),
                fail: false,
            }
        }
        fn resume_count(&self) -> usize {
            self.resumes.lock().unwrap().len()
        }
    }

    impl GoalResumer for FakeResumer {
        fn snapshot(&self, _goal_id: &str) -> Result<GoalSnapshot> {
            Ok(self.snap.lock().unwrap().clone())
        }
        fn worker_alive(&self, _goal_id: &str) -> bool {
            *self.alive.lock().unwrap()
        }
        fn resume(&self, goal_id: &str) -> Result<u32> {
            if self.fail {
                bail!("spawn refused");
            }
            self.resumes.lock().unwrap().push(goal_id.to_string());
            Ok(4242)
        }
    }

    async fn store() -> ScheduleStore {
        ScheduleStore::open(Path::new(":memory:")).await.unwrap()
    }

    #[test]
    fn duration_parses_units() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("15m").unwrap(), 900);
        assert_eq!(parse_duration("6h").unwrap(), 21_600);
        assert_eq!(parse_duration("2d").unwrap(), 172_800);
        assert_eq!(parse_duration("45").unwrap(), 45);
        assert!(parse_duration("0h").is_err());
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn five_field_cron_is_accepted() {
        // Operators write crontab lines; the parser wants seconds first.
        let t = Trigger::Cron {
            expr: "0 */6 * * *".into(),
        };
        let next = t.next_due(0).unwrap().unwrap();
        assert!(next > 0, "5-field cron must resolve to a real next fire");
        // 6-field passes straight through.
        let t6 = Trigger::Cron {
            expr: "0 0 */6 * * *".into(),
        };
        assert!(t6.next_due(0).unwrap().is_some());
        // Garbage is an error, never a schedule that silently never fires.
        assert!(
            Trigger::Cron {
                expr: "not a cron".into()
            }
            .next_due(0)
            .is_err()
        );
    }

    #[tokio::test]
    async fn schedule_fires_when_due_and_not_before() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 3, 30));
        let s = store
            .create("goal-1", Trigger::Every { seconds: 60 }, 10, 3)
            .await
            .unwrap();
        let created_due = s.next_due_at.unwrap();

        // Before the due time: nothing happens at all.
        let out = tick_once(&store, &r, created_due - 1).await.unwrap();
        assert!(out.is_empty(), "must not fire before it is due: {out:?}");
        assert_eq!(r.resume_count(), 0);

        // At the due time: it fires.
        let out = tick_once(&store, &r, created_due).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Decision::Fired(4242));
        assert_eq!(r.resume_count(), 1);

        let after = store.get(&s.id).await.unwrap().unwrap();
        assert_eq!(after.fires, 1);
        assert_eq!(after.next_due_at, Some(created_due + 60));
        assert!(!after.last_outcome.is_empty());
    }

    #[tokio::test]
    async fn approval_gate_is_never_laundered_by_a_wakeup() {
        let store = store().await;
        let mut snap = snapshot(GoalStatus::Paused, 10, 100);
        snap.paused_for_approval = true;
        let r = FakeResumer::new(snap);
        let s = store
            .create("goal-gate", Trigger::Every { seconds: 60 }, 10, 3)
            .await
            .unwrap();

        let out = tick_once(&store, &r, s.next_due_at.unwrap()).await.unwrap();
        assert_eq!(r.resume_count(), 0, "a gate pause must never be resumed");
        let Decision::Skipped(reason) = &out[0].1 else {
            panic!("expected a skip, got {:?}", out[0].1);
        };
        assert!(
            reason.contains("approval"),
            "reason must name the gate: {reason}"
        );
        // Still active — a human approving it later must be picked up.
        let after = store.get(&s.id).await.unwrap().unwrap();
        assert_eq!(after.state, ScheduleState::Active);
    }

    /// The gate guard reads the real checkpoint, not a hand-built snapshot.
    /// A research checkpoint pauses by setting the legacy `paused` flag, so a
    /// guard keyed on `gates_hit` (or on `status` alone) would have read that
    /// goal as resumable and walked through the approval.
    #[tokio::test]
    async fn a_paused_checkpoint_on_disk_is_read_as_needing_a_human() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test setup before any campaign dir read.
        unsafe { std::env::set_var("PRISM_CAMPAIGNS_DIR", dir.path()) };

        for (id, extra) in [
            // Materials shape: status + gates_hit both recorded.
            (
                "mat-gate",
                serde_json::json!({"status": "paused", "gates_hit": [3]}),
            ),
            // Research shape: only the legacy flag, empty gates_hit.
            (
                "res-gate",
                serde_json::json!({"status": "submitted", "gates_hit": []}),
            ),
        ] {
            let mut checkpoint = serde_json::json!({
                "campaign_id": id,
                "goal": {"description": "gated", "elements": [], "objective": "",
                         "constraints": [], "seeds": []},
                "config": {"max_iterations": 50, "batch_size": 2, "checkpoint_every": 1,
                           "approval_gate_at": [3], "llm_model": "", "llm_temperature": 0.7,
                           "reward_weights": {}},
                "candidates": [], "current_iteration": 3, "total_cost_usd": 0.0,
                "paused": true, "completed": false, "completion_reason": "",
                "started_at": "2026-07-27T00:00:00Z", "last_checkpoint_at": ""
            });
            let obj = checkpoint.as_object_mut().unwrap();
            for (k, v) in extra.as_object().unwrap() {
                obj.insert(k.clone(), v.clone());
            }
            std::fs::write(
                dir.path().join(format!("{id}.json")),
                serde_json::to_string(&checkpoint).unwrap(),
            )
            .unwrap();

            let resumer = WorkerResumer {
                exe: PathBuf::from("/nonexistent"),
            };
            let snap = resumer.snapshot(id).expect("checkpoint reads");
            assert!(
                snap.paused_for_approval,
                "'{id}' is paused at an approval gate — the scheduler must see that"
            );
        }
        unsafe { std::env::remove_var("PRISM_CAMPAIGNS_DIR") };
    }

    #[tokio::test]
    async fn budget_ceiling_stops_the_schedule() {
        let store = store().await;
        let mut snap = snapshot(GoalStatus::Running, 5, 50);
        snap.spent_usd = 12.5;
        snap.budget_usd = Some(10.0);
        let r = FakeResumer::new(snap);
        let s = store
            .create("goal-broke", Trigger::Every { seconds: 60 }, 10, 3)
            .await
            .unwrap();

        let out = tick_once(&store, &r, s.next_due_at.unwrap()).await.unwrap();
        assert_eq!(r.resume_count(), 0);
        let Decision::Stopped(reason) = &out[0].1 else {
            panic!("expected a stop, got {:?}", out[0].1);
        };
        assert!(reason.contains("budget ceiling"), "{reason}");
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Done
        );
    }

    #[tokio::test]
    async fn wakeup_ceiling_stops_the_schedule() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 0, 0));
        let s = store
            .create("goal-cap", Trigger::Every { seconds: 10 }, 2, 99)
            .await
            .unwrap();
        let mut now = s.next_due_at.unwrap();
        // Two fires allowed…
        for i in 0..2 {
            // Move the goal forward so the no-progress detector stays quiet.
            self_advance(&r, i + 1);
            let out = tick_once(&store, &r, now).await.unwrap();
            assert!(matches!(out[0].1, Decision::Fired(_)), "fire {i}: {out:?}");
            now += 10;
        }
        // …the third is refused.
        self_advance(&r, 3);
        let out = tick_once(&store, &r, now).await.unwrap();
        let Decision::Stopped(reason) = &out[0].1 else {
            panic!("expected a stop, got {:?}", out[0].1);
        };
        assert!(reason.contains("wake-up ceiling"), "{reason}");
        assert_eq!(r.resume_count(), 2);
    }

    fn self_advance(r: &FakeResumer, iteration: usize) {
        let mut s = r.snap.lock().unwrap();
        s.iteration = iteration;
        s.work_done = iteration * 10;
    }

    #[tokio::test]
    async fn no_progress_across_wakeups_is_detected() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 7, 70));
        let s = store
            .create("goal-wedged", Trigger::Every { seconds: 10 }, 50, 2)
            .await
            .unwrap();
        let mut now = s.next_due_at.unwrap();

        // Fire 1 records the fingerprint.
        assert!(matches!(
            tick_once(&store, &r, now).await.unwrap()[0].1,
            Decision::Fired(_)
        ));
        now += 10;
        // Fire 2: identical fingerprint → streak 1, still fires.
        assert!(matches!(
            tick_once(&store, &r, now).await.unwrap()[0].1,
            Decision::Fired(_)
        ));
        now += 10;
        // Fire 3: streak hits max → wedged, refuses to keep spending.
        let out = tick_once(&store, &r, now).await.unwrap();
        let Decision::Stopped(reason) = &out[0].1 else {
            panic!("expected a stop, got {:?}", out[0].1);
        };
        assert!(reason.contains("no progress"), "{reason}");
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Wedged
        );
        assert_eq!(r.resume_count(), 2, "the wedged wake-up must not resume");
    }

    /// A goal whose iterations are slower than its schedule is HEALTHY, not
    /// wedged. Checking no-progress before liveness retired the schedule for
    /// a working goal — and then nothing was left to wake it when its worker
    /// really did die.
    #[tokio::test]
    async fn a_slow_but_running_goal_is_never_declared_wedged() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 4, 40));
        let s = store
            .create("goal-slow", Trigger::Every { seconds: 10 }, 50, 2)
            .await
            .unwrap();

        // First tick finds a dead worker and resumes it. This is what puts
        // `fires` above zero and arms the no-progress comparison.
        let mut now = s.next_due_at.unwrap();
        assert!(matches!(
            tick_once(&store, &r, now).await.unwrap()[0].1,
            Decision::Fired(_)
        ));
        now += 10;

        // That worker is now alive and grinding through one long iteration,
        // so the checkpoint does not move for several ticks.
        *r.alive.lock().unwrap() = true;
        for tick in 0..6 {
            let out = tick_once(&store, &r, now).await.unwrap();
            assert!(
                matches!(out[0].1, Decision::Skipped(_)),
                "tick {tick} must skip a live worker, got {:?}",
                out[0].1
            );
            now += 10;
        }
        let after = store.get(&s.id).await.unwrap().unwrap();
        assert_eq!(
            after.state,
            ScheduleState::Active,
            "a working goal must keep its schedule: {}",
            after.last_outcome
        );
        assert_eq!(after.no_progress, 0);
        assert_eq!(r.resume_count(), 1, "no second worker over a live one");

        // The moment its worker actually dies, the same schedule resumes it.
        *r.alive.lock().unwrap() = false;
        self_advance(&r, 5);
        let out = tick_once(&store, &r, now).await.unwrap();
        assert!(matches!(out[0].1, Decision::Fired(_)), "{out:?}");
    }

    #[tokio::test]
    async fn heartbeat_is_missing_until_something_actually_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScheduleStore::open(&dir.path().join("s.db")).await.unwrap();
        assert!(store.last_tick_at().await.unwrap().is_none());
        let status = store.heartbeat_status(1_000).await;
        assert!(
            status.contains("MISSING"),
            "an unticked store must say so, not look healthy: {status}"
        );

        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        tick_once(&store, &r, 5_000).await.unwrap();
        assert_eq!(store.last_tick_at().await.unwrap(), Some(5_000));
        assert!(store.heartbeat_status(5_010).await.contains("OK"));
        // A host that stopped ticking is reported as stale, not as healthy.
        let stale = store
            .heartbeat_status(5_000 + HEARTBEAT_STALE_SECS + 1)
            .await;
        assert!(stale.contains("STALE"), "{stale}");
    }

    #[tokio::test]
    async fn a_live_worker_is_not_resumed_twice() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        *r.alive.lock().unwrap() = true;
        let s = store
            .create("goal-live", Trigger::Every { seconds: 60 }, 10, 3)
            .await
            .unwrap();
        let out = tick_once(&store, &r, s.next_due_at.unwrap()).await.unwrap();
        assert_eq!(r.resume_count(), 0);
        assert!(matches!(out[0].1, Decision::Skipped(_)));
        // Clock still advances so it re-checks next period.
        assert!(
            store
                .get(&s.id)
                .await
                .unwrap()
                .unwrap()
                .next_due_at
                .unwrap()
                > s.next_due_at.unwrap()
        );
    }

    #[tokio::test]
    async fn completed_goal_retires_the_schedule() {
        let store = store().await;
        let mut snap = snapshot(GoalStatus::Completed, 50, 500);
        snap.completion_reason = "iteration_limit".into();
        let r = FakeResumer::new(snap);
        let s = store
            .create("goal-done", Trigger::Every { seconds: 60 }, 10, 3)
            .await
            .unwrap();
        let out = tick_once(&store, &r, s.next_due_at.unwrap()).await.unwrap();
        assert!(matches!(out[0].1, Decision::Stopped(_)));
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Done
        );
        assert_eq!(r.resume_count(), 0);
    }

    #[tokio::test]
    async fn watcher_fires_on_the_condition_becoming_true_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("job.done");
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        store
            .create(
                "goal-watch",
                Trigger::WatchFile { path: flag.clone() },
                10,
                3,
            )
            .await
            .unwrap();

        // Condition false: nothing happens, no matter how many ticks.
        assert!(tick_once(&store, &r, 1_000).await.unwrap().is_empty());
        assert!(tick_once(&store, &r, 2_000).await.unwrap().is_empty());

        // Condition becomes true → fires once.
        std::fs::write(&flag, "done").unwrap();
        let out = tick_once(&store, &r, 3_000).await.unwrap();
        assert!(matches!(out[0].1, Decision::Fired(_)), "{out:?}");
        assert_eq!(r.resume_count(), 1);

        // Still true → does NOT fire again (edge, not level).
        assert!(tick_once(&store, &r, 4_000).await.unwrap().is_empty());
        assert_eq!(r.resume_count(), 1);
    }

    /// A watcher chained onto a goal that has not started yet must wait —
    /// chaining B onto A right after launching A is the normal case — but on
    /// a budget, so a typo'd id is reported rather than waited on forever.
    #[tokio::test]
    async fn a_watcher_waits_for_a_missing_target_then_reports_it() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        let s = store
            .create(
                "goal-x",
                Trigger::WatchGoal {
                    goal_id: "no-such-goal".into(),
                    status: "completed".into(),
                },
                10,
                3, // three checks before calling it a mistake
            )
            .await
            .unwrap();

        for check in 1..=2 {
            let out = tick_once(&store, &r, 1_000 + check).await.unwrap();
            let Decision::Skipped(reason) = &out[0].1 else {
                panic!("check {check} must wait, got {:?}", out[0].1);
            };
            assert!(reason.contains("does not exist yet"), "{reason}");
            assert_eq!(
                store.get(&s.id).await.unwrap().unwrap().state,
                ScheduleState::Active
            );
        }

        // Third check exhausts the budget: reported, not waited on forever.
        let out = tick_once(&store, &r, 2_000).await.unwrap();
        let Decision::Stopped(reason) = &out[0].1 else {
            panic!("the budget must run out, got {:?}", out[0].1);
        };
        assert!(reason.contains("check the id"), "{reason}");
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Wedged
        );
        assert_eq!(r.resume_count(), 0);
    }

    /// A watcher whose condition is genuinely broken (not merely early) is
    /// still surfaced immediately — an unreadable corpus is a defect.
    #[tokio::test]
    async fn a_broken_watcher_condition_is_reported_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        let s = store
            .create(
                "goal-y",
                Trigger::WatchCorpus {
                    // A real, openable database with no `emmo_entity` table:
                    // the count cannot be answered, which is a defect.
                    db: dir.path().join("not-a-graph.db"),
                    at_least: 10,
                    tenant: None,
                },
                10,
                3,
            )
            .await
            .unwrap();
        let out = tick_once(&store, &r, 1_000).await.unwrap();
        let Decision::Stopped(reason) = &out[0].1 else {
            panic!("a broken condition must be surfaced, got {:?}", out[0].1);
        };
        assert!(reason.contains("not queryable"), "{reason}");
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Wedged
        );
    }

    #[tokio::test]
    async fn cancel_is_honest_about_a_missing_schedule() {
        let store = store().await;
        assert!(!store.cancel("sched-nope").await.unwrap());
        let s = store
            .create("g", Trigger::Every { seconds: 60 }, 1, 1)
            .await
            .unwrap();
        assert!(store.cancel(&s.id).await.unwrap());
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Cancelled
        );
        // A cancelled schedule never fires again.
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 1));
        assert!(
            tick_once(&store, &r, i64::MAX / 2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn create_refuses_an_unbounded_schedule() {
        let store = store().await;
        assert!(
            store
                .create("g", Trigger::Every { seconds: 60 }, 0, 3)
                .await
                .is_err(),
            "max_fires = 0 must be refused"
        );
        assert!(
            store
                .create("", Trigger::Every { seconds: 60 }, 5, 3)
                .await
                .is_err(),
            "a schedule with no goal must be refused"
        );
    }

    #[tokio::test]
    async fn one_shot_at_retires_after_firing() {
        let store = store().await;
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        let s = store
            .create("goal-once", Trigger::At { unix_secs: 5_000 }, 10, 3)
            .await
            .unwrap();
        assert!(tick_once(&store, &r, 4_999).await.unwrap().is_empty());
        assert!(matches!(
            tick_once(&store, &r, 5_000).await.unwrap()[0].1,
            Decision::Fired(_)
        ));
        assert_eq!(
            store.get(&s.id).await.unwrap().unwrap().state,
            ScheduleState::Done
        );
        assert_eq!(r.resume_count(), 1);
    }

    #[tokio::test]
    async fn schedules_survive_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("schedules.db");
        let id = {
            let store = ScheduleStore::open(&db).await.unwrap();
            store
                .create("goal-persist", Trigger::Every { seconds: 3_600 }, 7, 4)
                .await
                .unwrap()
                .id
        };
        // Fresh process would do exactly this: reopen the file.
        let store = ScheduleStore::open(&db).await.unwrap();
        let back = store
            .get(&id)
            .await
            .unwrap()
            .expect("schedule must persist");
        assert_eq!(back.goal_id, "goal-persist");
        assert_eq!(back.max_fires, 7);
        assert_eq!(back.max_no_progress, 4);
        assert_eq!(back.trigger, Trigger::Every { seconds: 3_600 });
    }

    #[tokio::test]
    async fn overlapping_ticks_refuse_rather_than_double_resume() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("schedules.db");
        let store = ScheduleStore::open(&db).await.unwrap();
        let r = FakeResumer::new(snapshot(GoalStatus::Running, 1, 10));
        let s = store
            .create("goal-race", Trigger::Every { seconds: 60 }, 10, 3)
            .await
            .unwrap();

        // Stand in for a tick that is still in flight (cron overlapping
        // itself, or a human running `tick` while the timer fires).
        let held = TickLock::try_acquire(&store.lock_path).unwrap();
        assert!(held.is_some(), "first holder must get the lock");

        let err = tick_once(&store, &r, s.next_due_at.unwrap())
            .await
            .expect_err("a second tick must refuse, not silently do nothing");
        assert!(
            err.to_string().contains("already running"),
            "the refusal must say why: {err}"
        );
        assert_eq!(r.resume_count(), 0, "no goal may be resumed twice");

        // Once the first tick finishes, the next one proceeds normally.
        drop(held);
        let out = tick_once(&store, &r, s.next_due_at.unwrap()).await.unwrap();
        assert!(matches!(out[0].1, Decision::Fired(_)), "{out:?}");
    }

    #[test]
    fn operator_units_name_the_tick_command() {
        let plist = launchd_plist(Path::new("/usr/local/bin/prism"), 60);
        assert!(plist.contains("<string>com.mirdyne.prism.schedule</string>"));
        assert!(plist.contains("<string>schedule</string>"));
        assert!(plist.contains("<string>tick</string>"));
        assert!(plist.contains("<integer>60</integer>"));
        let (service, timer) = systemd_units(Path::new("/usr/local/bin/prism"), 300);
        assert!(service.contains("/usr/local/bin/prism schedule tick"));
        assert!(timer.contains("OnUnitActiveSec=300"));
    }
}
