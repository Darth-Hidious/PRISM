//! HyperQueue many-task backend.
//!
//! [HyperQueue](https://github.com/it4innovations/hyperqueue) (MIT) executes
//! large SETS of independent tasks and load-balances them across workers —
//! either standalone workers on this machine, or workers that HQ's automatic
//! allocator spawns inside Slurm/PBS allocations. PRISM talks to it by
//! shelling out to the `hq` binary (`--output-mode json`), the same pattern
//! `byoc.rs` uses for ssh/sbatch/squeue and the workspace uses for `gh`,
//! `git`, `hf`, `ollama`, and friends. HQ's crate library is deliberately
//! NOT linked: it is single-threaded (`Rc<RefCell>` core), not on crates.io,
//! and its `pub` surface exists for HQ's own binary, not for embedding.
//!
//! # Division of labour with `byoc.rs`
//!
//! `byoc.rs` stays the path for ONE long job that needs checkpoint/requeue
//! (exit code 140 → `scontrol requeue`): HQ has no checkpoint/resume for a
//! monolithic job. This module is the path for MANY independent tasks —
//! a thousand CALPHAD evaluations, or a 91-paper ingest that previously ran
//! as a hand-written bash concurrency loop. Submitting that workload through
//! `byoc.rs` would mean one `sbatch` per task and fall over queue limits;
//! here it is one `hq job submit-file` regardless of task count.
//!
//! # Failure semantics
//!
//! - If the `hq` binary is missing, every verb fails with the install
//!   command. There is no silent fallback to another backend.
//! - A submitted task set whose server cannot be polled is an `Err` from
//!   `status`, never a fabricated `Running`.
//! - A job id that vanished from the server (e.g. the server was restarted
//!   without a journal) reports `Failed` with that explanation.
//!
//! # Offline mode
//!
//! `Standalone` is allowed under `PRISM_OFFLINE=1`: the server this backend
//! starts binds 127.0.0.1 and the workers are local processes, so no egress
//! is created. `AutoAlloc` is refused: work leaves this machine through the
//! cluster scheduler, and the scheduler target cannot be verified from here
//! (same fail-closed reasoning as `byoc.rs` applies to kubectl contexts).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// How long to wait for a freshly started HQ server to accept commands.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between server-readiness probes while waiting for startup.
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// Which scheduler HQ's automatic allocator asks for resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HqScheduler {
    Slurm,
    Pbs,
}

impl HqScheduler {
    fn as_str(self) -> &'static str {
        match self {
            HqScheduler::Slurm => "slurm",
            HqScheduler::Pbs => "pbs",
        }
    }
}

/// How this backend sources HQ workers.
#[derive(Debug, Clone, PartialEq)]
pub enum HqMode {
    /// HQ standalone: PRISM starts the server and `workers` local worker
    /// processes. Needs no scheduler anywhere — the 91-paper-ingest shape.
    Standalone { workers: u32 },
    /// HQ automatic allocation: `hq alloc add <scheduler>` makes HQ ask
    /// Slurm/PBS for allocations and spawn workers inside them.
    AutoAlloc {
        scheduler: HqScheduler,
        /// Walltime HQ passes to sbatch/qsub for each allocation, e.g. `1h`.
        time_limit: String,
        /// Trailing arguments passed verbatim to sbatch/qsub after `--`
        /// (partition, account, etc.).
        extra_args: Vec<String>,
    },
}

/// Configuration for [`HyperQueueBackend`].
#[derive(Debug, Clone)]
pub struct HyperQueueConfig {
    /// The `hq` executable; `hq` on PATH unless overridden (tests point this
    /// at a fake binary).
    pub binary: String,
    /// HQ server directory this backend OWNS. Server record, access files,
    /// submitted job definitions and daemon logs live here. Use a
    /// PRISM-owned path (e.g. `<data_dir>/hyperqueue`), never a server
    /// directory a user runs interactively.
    pub server_dir: PathBuf,
    pub mode: HqMode,
    /// How long to wait for a freshly started server to accept commands.
    pub startup_timeout: Duration,
}

impl HyperQueueConfig {
    /// Standalone mode: local server plus `workers` local worker processes.
    pub fn standalone(server_dir: impl Into<PathBuf>, workers: u32) -> Self {
        Self {
            binary: "hq".into(),
            server_dir: server_dir.into(),
            mode: HqMode::Standalone {
                workers: workers.max(1),
            },
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        }
    }

    /// Automatic allocation against a Slurm/PBS cluster.
    pub fn autoalloc(
        server_dir: impl Into<PathBuf>,
        scheduler: HqScheduler,
        time_limit: impl Into<String>,
        extra_args: Vec<String>,
    ) -> Self {
        Self {
            binary: "hq".into(),
            server_dir: server_dir.into(),
            mode: HqMode::AutoAlloc {
                scheduler,
                time_limit: time_limit.into(),
                extra_args,
            },
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        }
    }
}

/// One task in a HyperQueue task set.
///
/// `command` is an argv vector: HQ execs it directly with no shell, so
/// metacharacters in arguments are literal — there is no quoting layer to
/// escape and no injection surface.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HqTask {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
}

impl HqTask {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            ..Default::default()
        }
    }

    fn validate(&self) -> Result<()> {
        if self.command.is_empty() || self.command[0].is_empty() {
            bail!("HyperQueue task has an empty command: every task needs a non-empty argv");
        }
        Ok(())
    }
}

/// Per-task status in PRISM's existing [`JobStatus`] vocabulary — no
/// parallel status enum.
#[derive(Debug, Clone, PartialEq)]
pub struct HqTaskStatus {
    pub task_id: u32,
    pub status: JobStatus,
}

/// HyperQueue backend: submits task SETS, one `hq job submit-file` per set.
pub struct HyperQueueBackend {
    config: HyperQueueConfig,
    hq_job_ids: Arc<RwLock<HashMap<Uuid, u32>>>,
    /// Memoised successful `ensure_ready` — avoids a probe storm on every
    /// submit. If the server dies later, verbs fail with honest errors.
    ready: AtomicBool,
}

impl HyperQueueBackend {
    pub fn new(config: HyperQueueConfig) -> Self {
        Self {
            config,
            hq_job_ids: Arc::new(RwLock::new(HashMap::new())),
            ready: AtomicBool::new(false),
        }
    }

    /// Restore the HQ job id persisted for a previously submitted PRISM job
    /// (cross-process status), mirroring `ByocBackend::resume`.
    pub fn resume(config: HyperQueueConfig, job_id: Uuid, hq_job_id: Option<u32>) -> Self {
        let mut hq_job_ids = HashMap::new();
        if let Some(hq_job_id) = hq_job_id {
            hq_job_ids.insert(job_id, hq_job_id);
        }
        Self {
            config,
            hq_job_ids: Arc::new(RwLock::new(hq_job_ids)),
            ready: AtomicBool::new(false),
        }
    }

    /// The HQ job id a PRISM job was submitted as, if known to this instance.
    pub async fn hq_job_id(&self, job_id: Uuid) -> Option<u32> {
        self.hq_job_ids.read().await.get(&job_id).copied()
    }

    /// The server directory this backend owns.
    pub fn server_dir(&self) -> &Path {
        &self.config.server_dir
    }

    /// Refuse under hard offline mode, BEFORE any process is spawned.
    ///
    /// Same wiring discipline as `byoc.rs`: called at the top of all four
    /// `ComputeBackend` methods, and pinned by a test that drives all four.
    fn check_offline(&self) -> Result<()> {
        if !prism_runtime::offline::enabled() {
            return Ok(());
        }
        match &self.config.mode {
            HqMode::Standalone { .. } => Ok(()),
            HqMode::AutoAlloc { scheduler, .. } => bail!(
                "offline mode: HyperQueue auto-allocation via {} sends work to a cluster \
                 scheduler — the compute leaves this machine, so hard offline refuses it",
                scheduler.as_str()
            ),
        }
    }

    /// Submit a SET of tasks as one HQ job; returns the PRISM job id.
    ///
    /// This is the native shape of this backend. `ComputeBackend::submit`
    /// delegates here after extracting tasks from the plan inputs.
    pub async fn submit_tasks(&self, name: &str, tasks: &[HqTask]) -> Result<Uuid> {
        self.check_offline()?;
        if tasks.is_empty() {
            bail!("refusing to submit an empty HyperQueue task set");
        }
        for task in tasks {
            task.validate()?;
        }
        self.ensure_ready().await?;

        let job_id = Uuid::new_v4();
        let jdf_path = self
            .config
            .server_dir
            .join("prism-jobs")
            .join(format!("prism-{job_id}.toml"));
        let jdf = build_jdf(name, tasks)?;
        tokio::fs::create_dir_all(jdf_path.parent().expect("jdf path has a parent"))
            .await
            .with_context(|| {
                format!("failed to create {}", jdf_path.parent().unwrap().display())
            })?;
        tokio::fs::write(&jdf_path, &jdf).await.with_context(|| {
            format!("failed to write job definition file {}", jdf_path.display())
        })?;

        let output = self
            .hq_output(&submit_file_args(&jdf_path))
            .await
            .context("HyperQueue submission failed")?;
        if !output.status.success() {
            bail!(
                "hq job submit-file failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout).context("non-UTF-8 hq submit output")?;
        let hq_job_id = parse_hq_job_id(&stdout).with_context(|| {
            format!("HyperQueue accepted the task set but its job id could not be parsed from {stdout:?}")
        })?;
        self.hq_job_ids.write().await.insert(job_id, hq_job_id);
        tracing::info!(
            %job_id,
            hq_job_id,
            tasks = tasks.len(),
            server_dir = %self.config.server_dir.display(),
            "HyperQueue task set submitted"
        );
        Ok(job_id)
    }

    /// Per-task statuses in the shared [`JobStatus`] vocabulary.
    pub async fn task_statuses(&self, job_id: Uuid) -> Result<Vec<HqTaskStatus>> {
        self.check_offline()?;
        let detail = self.job_detail(job_id).await?;
        detail
            .tasks
            .iter()
            .map(|task| {
                Ok(HqTaskStatus {
                    task_id: task.id,
                    status: map_task_state(&task.state, task.error.as_deref())?,
                })
            })
            .collect()
    }

    /// Stop the HQ server this backend owns (workers follow the server).
    /// Not called automatically: in-flight tasks would be lost.
    pub async fn shutdown(&self) -> Result<()> {
        let output = self.hq_output(&server_stop_args()).await?;
        if !output.status.success() {
            bail!(
                "hq server stop failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// The HQ job id for a PRISM job, or an honest error naming the gap.
    async fn known_hq_job_id(&self, job_id: Uuid) -> Result<u32> {
        self.hq_job_id(job_id).await.context(format!(
            "no HyperQueue job id recorded for PRISM job {job_id} in this backend instance; \
             cross-process, restore it with HyperQueueBackend::resume from the job tracker record"
        ))
    }

    /// `hq job list --output-mode json`, parsed.
    async fn job_list(&self) -> Result<Vec<HqJobSummary>> {
        let output = self.hq_output(&job_list_args()).await?;
        if !output.status.success() {
            bail!(
                "cannot poll the HyperQueue server at {}: {}",
                self.config.server_dir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout).context("non-UTF-8 hq job list output")?;
        serde_json::from_str(&stdout).with_context(|| {
            format!("hq job list did not return the expected JSON; stdout was {stdout:?}")
        })
    }

    /// `hq job info <id> --output-mode json` for one job, parsed.
    async fn job_detail(&self, job_id: Uuid) -> Result<HqJobDetailDoc> {
        let hq_job_id = self.known_hq_job_id(job_id).await?;
        let output = self
            .hq_output(&job_info_args(hq_job_id))
            .await
            .with_context(|| format!("cannot poll HyperQueue job {hq_job_id}"))?;
        if !output.status.success() {
            bail!(
                "hq job info {hq_job_id} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout).context("non-UTF-8 hq job info output")?;
        let docs: Vec<HqJobDetailDoc> = serde_json::from_str(&stdout).with_context(|| {
            format!("hq job info did not return the expected JSON; stdout was {stdout:?}")
        })?;
        docs.into_iter().next().with_context(|| {
            format!("HyperQueue job {hq_job_id} for PRISM job {job_id} no longer exists on the server at {}", self.config.server_dir.display())
        })
    }

    /// Server up, and a worker source in place (local workers or an
    /// allocation queue). Idempotent.
    async fn ensure_ready(&self) -> Result<()> {
        if self.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        self.check_binary()?;
        self.ensure_server().await?;
        match &self.config.mode {
            HqMode::Standalone { workers } => self.ensure_workers(*workers).await?,
            HqMode::AutoAlloc {
                scheduler,
                time_limit,
                extra_args,
            } => {
                self.ensure_alloc_queue(*scheduler, time_limit, extra_args)
                    .await?
            }
        }
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    /// Probe the server; if it is down, start it and wait for the probe.
    /// Callers must run [`Self::check_binary`] first so a missing `hq`
    /// reports the install command instead of a raw spawn failure.
    async fn ensure_server(&self) -> Result<()> {
        if self.probe().await.is_ok() {
            return Ok(());
        }
        tokio::fs::create_dir_all(&self.config.server_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to create HyperQueue server dir {}",
                    self.config.server_dir.display()
                )
            })?;
        let log = self.config.server_dir.join("prism-server.out");
        let log_file = std::fs::File::create(&log)
            .with_context(|| format!("failed to create server log {}", log.display()))?;
        let mut cmd = self.hq_command(&server_start_args());
        cmd.stdout(Stdio::from(
            log_file.try_clone().context("dup server log handle")?,
        ))
        .stderr(Stdio::from(log_file));
        cmd.spawn().with_context(|| {
            format!(
                "failed to start the HyperQueue server ({} server start)",
                self.config.binary
            )
        })?;
        // The daemon outlives this call on purpose; `shutdown` stops it.
        let deadline = tokio::time::Instant::now() + self.config.startup_timeout;
        loop {
            if self.probe().await.is_ok() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "HyperQueue server did not come up at {} within {}s; see {}",
                    self.config.server_dir.display(),
                    self.config.startup_timeout.as_secs(),
                    log.display()
                );
            }
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
    }

    /// Cheap server-health probe: `hq job list` round-trips through the
    /// client connection. Nonzero exit = server down (or not yet up).
    async fn probe(&self) -> Result<()> {
        let output = self.hq_output(&job_list_args()).await?;
        if output.status.success() {
            Ok(())
        } else {
            bail!(
                "HQ server probe failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        }
    }

    /// Start local workers when the server has none.
    async fn ensure_workers(&self, workers: u32) -> Result<()> {
        let output = self.hq_output(&worker_list_args()).await?;
        if !output.status.success() {
            bail!(
                "hq worker list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout).context("non-UTF-8 hq worker list output")?;
        let list: Vec<Value> = serde_json::from_str(&stdout).with_context(|| {
            format!("hq worker list did not return a JSON array; stdout was {stdout:?}")
        })?;
        if !list.is_empty() {
            return Ok(());
        }
        let log = self.config.server_dir.join("prism-workers.out");
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .with_context(|| format!("failed to open worker log {}", log.display()))?;
        for _ in 0..workers {
            let mut cmd = self.hq_command(&worker_start_args());
            cmd.stdout(Stdio::from(
                log_file.try_clone().context("dup worker log handle")?,
            ))
            .stderr(Stdio::from(
                log_file.try_clone().context("dup worker log handle")?,
            ));
            cmd.spawn().context("failed to start a HyperQueue worker")?;
        }
        Ok(())
    }

    /// Register an allocation queue when HQ has none.
    async fn ensure_alloc_queue(
        &self,
        scheduler: HqScheduler,
        time_limit: &str,
        extra_args: &[String],
    ) -> Result<()> {
        let output = self.hq_output(&alloc_list_args()).await?;
        if !output.status.success() {
            bail!(
                "hq alloc list failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let stdout = String::from_utf8(output.stdout).context("non-UTF-8 hq alloc list output")?;
        let list: Vec<Value> = serde_json::from_str(&stdout).with_context(|| {
            format!("hq alloc list did not return a JSON array; stdout was {stdout:?}")
        })?;
        if !list.is_empty() {
            return Ok(());
        }
        let output = self
            .hq_output(&alloc_add_args(scheduler, time_limit, extra_args))
            .await?;
        if !output.status.success() {
            bail!(
                "hq alloc add {} failed: {}",
                scheduler.as_str(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// Refuse early, with the install command, when the `hq` binary cannot
    /// be found. Pure PATH lookup — no process is spawned.
    fn check_binary(&self) -> Result<()> {
        if find_hq_binary(&self.config.binary).is_some() {
            return Ok(());
        }
        bail!("{}", missing_binary_message(&self.config.binary))
    }

    fn hq_command(&self, args: &[String]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.config.binary);
        cmd.args(["--server-dir"])
            .arg(&self.config.server_dir)
            .args(args);
        cmd
    }

    /// Run one `hq` invocation. A missing binary becomes an honest error
    /// carrying the install command — never a silent fallback.
    async fn hq_output(&self, args: &[String]) -> Result<std::process::Output> {
        match self.hq_command(args).output().await {
            Ok(output) => Ok(output),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("{}", missing_binary_message(&self.config.binary))
            }
            Err(e) => Err(e).with_context(|| format!("failed to run {:?} hq", self.config.binary)),
        }
    }
}

/// Locate the `hq` binary: an explicit path is checked directly, a bare
/// name is searched on PATH (same shape as `local.rs`'s `which`).
fn find_hq_binary(binary: &str) -> Option<PathBuf> {
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        return path.is_file().then_some(path);
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

fn missing_binary_message(binary: &str) -> String {
    format!(
        "the `hq` binary {binary:?} was not found; PRISM does not fall back to another \
         backend. Install HyperQueue with `cargo install hyperqueue` or download a \
         release binary from https://github.com/it4innovations/hyperqueue/releases"
    )
}

#[async_trait]
impl ComputeBackend for HyperQueueBackend {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        let tasks = parse_plan_tasks(&plan.inputs)?;
        // `plan.image` is deliberately unused: HQ tasks are plain commands,
        // not containers. A containerised task can still wrap its command.
        self.submit_tasks(&plan.name, &tasks).await
    }

    async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        self.check_offline()?;
        let hq_job_id = self.known_hq_job_id(job_id).await?;
        let jobs = self.job_list().await?;
        match jobs.iter().find(|job| job.id == hq_job_id) {
            None => Ok(JobStatus::Failed {
                error: format!(
                    "HyperQueue job {hq_job_id} for PRISM job {job_id} no longer exists on the \
                     server at {}; the server may have restarted without a journal",
                    self.config.server_dir.display()
                ),
            }),
            Some(job) => fold_task_counters(&job.task_stats),
        }
    }

    async fn results(&self, job_id: Uuid) -> Result<Value> {
        self.check_offline()?;
        let detail = self.job_detail(job_id).await?;
        let stats = &detail.info.task_stats;
        let total = stats.total();
        let terminal = stats.finished + stats.failed + stats.canceled + stats.aborted;
        if terminal < total {
            bail!(
                "HyperQueue job {} is not finished: {terminal} of {total} tasks are terminal; \
                 poll status until it completes before fetching results",
                detail.info.id
            );
        }
        let mut tasks_out = Vec::with_capacity(detail.tasks.len());
        for task in &detail.tasks {
            let mut entry = serde_json::json!({ "id": task.id, "state": task.state });
            match task.state.as_str() {
                "finished" => match stdio_path(&task.stdout) {
                    Some(path) => match tokio::fs::read_to_string(&path).await {
                        Ok(text) => match serde_json::from_str::<Value>(&text) {
                            Ok(value) => entry["result"] = value,
                            Err(_) => entry["result"] = serde_json::json!({ "output": text }),
                        },
                        Err(e) => {
                            entry["result_error"] = serde_json::json!(format!(
                                "stdout file {} could not be read: {e}",
                                path.display()
                            ));
                        }
                    },
                    None => {
                        entry["result_error"] = serde_json::json!(
                            "task finished but has no stdout file (streaming or `none` stdout?)"
                        );
                    }
                },
                "failed" | "aborted" => {
                    entry["result_error"] = serde_json::json!(
                        task.error
                            .clone()
                            .unwrap_or_else(|| "task failed without an error message".into())
                    );
                }
                _ => {
                    entry["result_error"] =
                        serde_json::json!(format!("task ended in state {}", task.state));
                }
            }
            tasks_out.push(entry);
        }
        Ok(serde_json::json!({ "job": detail.info.id, "tasks": tasks_out }))
    }

    async fn cancel(&self, job_id: Uuid) -> Result<()> {
        self.check_offline()?;
        let hq_job_id = self.known_hq_job_id(job_id).await?;
        // Cancelling an already-terminal job is an error in HQ; check first.
        if let Ok(status) = self.status(job_id).await
            && matches!(
                status,
                JobStatus::Completed | JobStatus::Failed { .. } | JobStatus::Cancelled
            )
        {
            return Ok(());
        }
        let output = self.hq_output(&cancel_args(hq_job_id)).await?;
        if !output.status.success() {
            bail!(
                "hq job cancel {hq_job_id} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

// ── Plan-shape helpers (pure, unit-tested) ───────────────────────────────

/// Whether a plan has the many-task shape HQ exists for: `inputs.tasks`
/// (array of task objects) or `inputs.command` (single argv array).
///
/// The router uses this to pick between this backend and `byoc.rs`: a task
/// set goes to HyperQueue when one is configured; anything else — notably a
/// single long job that wants checkpoint/requeue — stays on the caller's
/// default backend, because HQ has no checkpoint/resume for monolithic jobs.
pub fn plan_is_task_set(plan: &ExperimentPlan) -> bool {
    plan.inputs.get("tasks").is_some_and(Value::is_array)
        || plan.inputs.get("command").is_some_and(Value::is_array)
}

/// Extract the task set from plan inputs: `tasks` array, or a single
/// `command` argv array as a one-task set. Anything else is an honest error.
fn parse_plan_tasks(inputs: &Value) -> Result<Vec<HqTask>> {
    if let Some(tasks) = inputs.get("tasks") {
        let tasks: Vec<HqTask> = serde_json::from_value(tasks.clone()).context(
            "inputs.tasks is not a valid HyperQueue task list; expected an array of \
             {command: [...], cwd?: string, env?: {...}, stdin?: string}",
        )?;
        if tasks.is_empty() {
            bail!("inputs.tasks is empty — submit at least one task");
        }
        return Ok(tasks);
    }
    if let Some(command) = inputs.get("command") {
        let command: Vec<String> = serde_json::from_value(command.clone())
            .context("inputs.command must be an array of argv strings")?;
        return Ok(vec![HqTask::new(command)]);
    }
    bail!(
        "HyperQueue plans need inputs.tasks (array of task objects) or inputs.command \
         (single argv array); this plan has neither"
    )
}

// ── HQ command argument builders (pure, unit-tested) ─────────────────────

fn server_start_args() -> Vec<String> {
    // Bind loopback only: workers are local or spawned by HQ's own
    // allocator, and this keeps offline-mode reasoning honest.
    ["server", "start", "--host", "127.0.0.1"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn server_stop_args() -> Vec<String> {
    ["server", "stop"].iter().map(|s| s.to_string()).collect()
}

fn worker_start_args() -> Vec<String> {
    ["worker", "start"].iter().map(|s| s.to_string()).collect()
}

fn worker_list_args() -> Vec<String> {
    ["worker", "list", "--output-mode", "json"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn alloc_list_args() -> Vec<String> {
    ["alloc", "list", "--output-mode", "json"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn alloc_add_args(scheduler: HqScheduler, time_limit: &str, extra_args: &[String]) -> Vec<String> {
    let mut args: Vec<String> = [
        "alloc",
        "add",
        scheduler.as_str(),
        "--time-limit",
        time_limit,
        "--",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(extra_args.iter().cloned());
    args
}

fn job_list_args() -> Vec<String> {
    ["job", "list", "--output-mode", "json"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn job_info_args(hq_job_id: u32) -> Vec<String> {
    [
        "job",
        "info",
        &hq_job_id.to_string(),
        "--output-mode",
        "json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn cancel_args(hq_job_id: u32) -> Vec<String> {
    ["job", "cancel", &hq_job_id.to_string()]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn submit_file_args(jdf_path: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["job", "submit-file"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push(jdf_path.display().to_string());
    args.extend(["--output-mode", "json"].iter().map(|s| s.to_string()));
    args
}

// ── Job Definition File (TOML) generation ────────────────────────────────

#[derive(Serialize)]
struct JdfFile {
    name: String,
    task: Vec<JdfTask>,
}

#[derive(Serialize)]
struct JdfTask {
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdin: Option<String>,
}

/// Render the task set as an HQ Job Definition File. Task ids are left to
/// HQ's automatic numbering (0..n), so they line up with the task order.
fn build_jdf(name: &str, tasks: &[HqTask]) -> Result<String> {
    let file = JdfFile {
        name: name.to_string(),
        task: tasks
            .iter()
            .map(|task| JdfTask {
                command: task.command.clone(),
                cwd: task.cwd.clone(),
                env: task.env.clone(),
                stdin: task.stdin.clone(),
            })
            .collect(),
    };
    toml::to_string(&file).context("failed to serialise the HyperQueue job definition file")
}

// ── HQ JSON parsing (shapes verified against hyperqueue 0.26.x) ──────────

/// Per-task counters from `hq job list` (`info.task_stats`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HqTaskStats {
    pub running: u32,
    pub finished: u32,
    pub failed: u32,
    pub canceled: u32,
    pub aborted: u32,
    pub waiting: u32,
}

impl HqTaskStats {
    fn total(&self) -> u32 {
        self.running + self.finished + self.failed + self.canceled + self.aborted + self.waiting
    }
}

#[derive(Debug, Deserialize)]
struct HqJobSummary {
    id: u32,
    task_stats: HqTaskStats,
}

#[derive(Debug, Deserialize)]
struct HqJobDetailDoc {
    info: HqJobInfoBlock,
    #[serde(default)]
    tasks: Vec<HqTaskDoc>,
}

#[derive(Debug, Deserialize)]
struct HqJobInfoBlock {
    id: u32,
    task_stats: HqTaskStats,
}

#[derive(Debug, Deserialize)]
struct HqTaskDoc {
    id: u32,
    state: String,
    #[serde(default)]
    error: Option<String>,
    /// HQ emits `null`, a `"<pipe>"` marker, or `Some(path)` — the last
    /// serialises as a one-element array, so accept both string forms.
    #[serde(default)]
    stdout: Option<Value>,
}

/// `{"id": N}` — what `hq job submit-file --output-mode json` prints.
fn parse_hq_job_id(stdout: &str) -> Result<u32> {
    let value: Value = serde_json::from_str(stdout)?;
    let id = value
        .get("id")
        .and_then(Value::as_u64)
        .context("submission JSON had no numeric `id`")?;
    let id = u32::try_from(id).context("HyperQueue job id does not fit in u32")?;
    if id == 0 {
        bail!("HyperQueue job ids start at 1; got 0");
    }
    Ok(id)
}

/// Fold HQ task counters into PRISM's [`JobStatus`] — the single status
/// vocabulary the tracker understands. `progress` is the finished fraction.
fn fold_task_counters(stats: &HqTaskStats) -> Result<JobStatus> {
    let total = stats.total();
    if total == 0 {
        bail!("HyperQueue reported a job with zero tasks");
    }
    let failed = stats.failed + stats.aborted;
    let active = stats.running + stats.waiting;
    let progress = f64::from(stats.finished) / f64::from(total);
    if failed > 0 && active == 0 {
        return Ok(JobStatus::Failed {
            error: format!("{failed} of {total} tasks failed"),
        });
    }
    if active == 0 {
        return Ok(if stats.canceled > 0 {
            JobStatus::Cancelled
        } else {
            JobStatus::Completed
        });
    }
    if stats.running > 0 || stats.finished + stats.failed + stats.canceled + stats.aborted > 0 {
        return Ok(JobStatus::Running { progress });
    }
    Ok(JobStatus::Queued)
}

/// Map one HQ task state into the shared [`JobStatus`] vocabulary.
fn map_task_state(state: &str, error: Option<&str>) -> Result<JobStatus> {
    Ok(match state {
        "waiting" => JobStatus::Queued,
        "running" => JobStatus::Running { progress: 0.0 },
        "finished" => JobStatus::Completed,
        "failed" => JobStatus::Failed {
            error: error
                .map(str::to_string)
                .unwrap_or_else(|| "task failed without an error message".into()),
        },
        "aborted" => JobStatus::Failed {
            error: error.map(str::to_string).unwrap_or_else(|| {
                "task aborted (a dependency failed or the crash limit was reached)".into()
            }),
        },
        "canceled" => JobStatus::Cancelled,
        other => bail!("unknown HyperQueue task state {other:?} — hq version drift?"),
    })
}

/// Extract a filesystem path from HQ's polymorphic stdio JSON field.
fn stdio_path(value: &Option<Value>) -> Option<PathBuf> {
    match value {
        Some(Value::String(s)) if s != "<pipe>" => Some(PathBuf::from(s)),
        Some(Value::Array(items)) => items.iter().find_map(Value::as_str).map(PathBuf::from),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_runtime::offline::test_support::{OfflineEnvGuard, env_lock};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn local_backend(dir: &Path) -> HyperQueueBackend {
        HyperQueueBackend::new(HyperQueueConfig::standalone(dir, 1))
    }

    fn task_plan(name: &str, tasks: usize) -> ExperimentPlan {
        let tasks: Vec<Value> = (0..tasks)
            .map(|i| serde_json::json!({ "command": ["echo", i.to_string()] }))
            .collect();
        ExperimentPlan {
            name: name.into(),
            image: "ignored-by-hyperqueue".into(),
            inputs: serde_json::json!({ "tasks": tasks }),
        }
    }

    // ── plan shape ──

    #[test]
    fn parse_plan_tasks_accepts_tasks_array() {
        let plan = task_plan("corpus", 3);
        let tasks = parse_plan_tasks(&plan.inputs).unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].command, ["echo", "0"]);
    }

    #[test]
    fn parse_plan_tasks_accepts_single_command_as_one_task_set() {
        let inputs = serde_json::json!({ "command": ["python3", "ingest.py"] });
        let tasks = parse_plan_tasks(&inputs).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, ["python3", "ingest.py"]);
    }

    #[test]
    fn parse_plan_tasks_rejects_missing_shape() {
        let err = parse_plan_tasks(&serde_json::json!({"other": 1})).unwrap_err();
        assert!(format!("{err:#}").contains("inputs.tasks"), "{err:#}");
    }

    #[test]
    fn parse_plan_tasks_rejects_empty_tasks() {
        let err = parse_plan_tasks(&serde_json::json!({ "tasks": [] })).unwrap_err();
        assert!(format!("{err}").contains("at least one task"), "{err}");
    }

    #[test]
    fn parse_plan_tasks_rejects_malformed_task() {
        let err =
            parse_plan_tasks(&serde_json::json!({ "tasks": [{ "command": "not-an-array" }] }))
                .unwrap_err();
        assert!(format!("{err:#}").contains("inputs.tasks"), "{err:#}");
    }

    #[test]
    fn plan_is_task_set_shapes() {
        assert!(plan_is_task_set(&task_plan("t", 2)));
        let single = ExperimentPlan {
            name: "s".into(),
            image: "i".into(),
            inputs: serde_json::json!({ "command": ["true"] }),
        };
        assert!(plan_is_task_set(&single));
        let byoc_shaped = ExperimentPlan {
            name: "b".into(),
            image: "img.sif".into(),
            inputs: serde_json::json!({}),
        };
        assert!(!plan_is_task_set(&byoc_shaped));
        let tasks_not_array = ExperimentPlan {
            name: "x".into(),
            image: "i".into(),
            inputs: serde_json::json!({ "tasks": "oops" }),
        };
        assert!(!plan_is_task_set(&tasks_not_array));
    }

    // ── status folding ──

    fn stats(
        running: u32,
        finished: u32,
        failed: u32,
        canceled: u32,
        aborted: u32,
        waiting: u32,
    ) -> HqTaskStats {
        HqTaskStats {
            running,
            finished,
            failed,
            canceled,
            aborted,
            waiting,
        }
    }

    #[test]
    fn fold_counters_all_finished_is_completed() {
        let status = fold_task_counters(&stats(0, 91, 0, 0, 0, 0)).unwrap();
        assert!(matches!(status, JobStatus::Completed));
    }

    #[test]
    fn fold_counters_terminal_failures_are_failed() {
        let status = fold_task_counters(&stats(0, 89, 2, 0, 0, 0)).unwrap();
        assert!(matches!(&status, JobStatus::Failed { error } if error.contains("2 of 91")));
    }

    #[test]
    fn fold_counters_aborted_counts_as_failure() {
        let status = fold_task_counters(&stats(0, 1, 0, 0, 1, 0)).unwrap();
        assert!(matches!(status, JobStatus::Failed { .. }));
    }

    #[test]
    fn fold_counters_running_reports_finished_fraction() {
        let status = fold_task_counters(&stats(2, 1, 0, 0, 0, 1)).unwrap();
        assert!(
            matches!(status, JobStatus::Running { progress } if (progress - 0.25).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn fold_counters_failure_in_flight_still_runs() {
        // Some tasks failed but HQ is still working through the rest.
        let status = fold_task_counters(&stats(1, 1, 1, 0, 0, 1)).unwrap();
        assert!(matches!(status, JobStatus::Running { .. }));
    }

    #[test]
    fn fold_counters_all_waiting_is_queued() {
        let status = fold_task_counters(&stats(0, 0, 0, 0, 0, 5)).unwrap();
        assert!(matches!(status, JobStatus::Queued));
    }

    #[test]
    fn fold_counters_canceled_terminal_is_cancelled() {
        let status = fold_task_counters(&stats(0, 3, 0, 2, 0, 0)).unwrap();
        assert!(matches!(status, JobStatus::Cancelled));
    }

    #[test]
    fn fold_counters_zero_tasks_is_an_error() {
        assert!(fold_task_counters(&stats(0, 0, 0, 0, 0, 0)).is_err());
    }

    // ── task state mapping ──

    #[test]
    fn task_states_map_to_the_shared_vocabulary() {
        assert_eq!(map_task_state("waiting", None).unwrap(), JobStatus::Queued);
        assert_eq!(
            map_task_state("running", None).unwrap(),
            JobStatus::Running { progress: 0.0 }
        );
        assert_eq!(
            map_task_state("finished", None).unwrap(),
            JobStatus::Completed
        );
        assert_eq!(
            map_task_state("canceled", None).unwrap(),
            JobStatus::Cancelled
        );
        assert!(matches!(
            map_task_state("failed", Some("exit 1")).unwrap(),
            JobStatus::Failed { error } if error == "exit 1"
        ));
        assert!(matches!(
            map_task_state("aborted", None).unwrap(),
            JobStatus::Failed { .. }
        ));
        assert!(map_task_state("teleporting", None).is_err());
    }

    // ── JDF generation ──

    #[test]
    fn jdf_roundtrips_through_toml() {
        let mut env = BTreeMap::new();
        env.insert("PAPER".into(), "47".into());
        let tasks = [
            HqTask {
                command: vec!["python3".into(), "ingest.py".into()],
                cwd: Some("/work/corpus".into()),
                env,
                stdin: Some("paper-47".into()),
            },
            HqTask::new(vec!["true".into()]),
        ];
        let jdf = build_jdf("corpus-ingest", &tasks).unwrap();
        let parsed: toml::Value = toml::from_str(&jdf).unwrap();
        assert_eq!(parsed["name"].as_str(), Some("corpus-ingest"));
        let tasks = parsed["task"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks[0]["command"].as_array().unwrap()[1].as_str(),
            Some("ingest.py")
        );
        assert_eq!(tasks[0]["cwd"].as_str(), Some("/work/corpus"));
        assert_eq!(tasks[0]["env"]["PAPER"].as_str(), Some("47"));
        assert_eq!(tasks[0]["stdin"].as_str(), Some("paper-47"));
        assert!(tasks[1].get("cwd").is_none());
        assert!(tasks[1].get("env").is_none());
    }

    #[test]
    fn jdf_escapes_hostile_strings() {
        // TOML, not a shell: quotes and newlines are data, not syntax.
        let tasks = [HqTask::new(vec![
            "echo".into(),
            "she said \"hi\"\nand left".into(),
        ])];
        let jdf = build_jdf("quote's job", &tasks).unwrap();
        let parsed: toml::Value = toml::from_str(&jdf).unwrap();
        assert_eq!(parsed["name"].as_str(), Some("quote's job"));
        assert_eq!(
            parsed["task"][0]["command"][1].as_str(),
            Some("she said \"hi\"\nand left")
        );
    }

    // ── HQ JSON shapes ──

    #[test]
    fn parse_submit_output_id() {
        assert_eq!(parse_hq_job_id("{\"id\": 7}").unwrap(), 7);
        assert!(parse_hq_job_id("{\"id\": 0}").is_err());
        assert!(parse_hq_job_id("no json").is_err());
        assert!(parse_hq_job_id("{}").is_err());
    }

    #[test]
    fn stdio_path_accepts_all_hq_shapes() {
        assert_eq!(
            stdio_path(&Some(Value::String("/tmp/out".into()))).unwrap(),
            Path::new("/tmp/out")
        );
        // HQ serialises Some(path) as a one-element array.
        assert_eq!(
            stdio_path(&Some(serde_json::json!(["/tmp/out"]))).unwrap(),
            Path::new("/tmp/out")
        );
        assert_eq!(stdio_path(&Some(Value::String("<pipe>".into()))), None);
        assert_eq!(stdio_path(&Some(Value::Null)), None);
        assert_eq!(stdio_path(&None), None);
    }

    #[test]
    fn job_list_json_parses() {
        let raw = r#"[{"id": 3, "name": "corpus", "task_count": 91, "is_open": false,
                       "task_stats": {"running": 2, "finished": 40, "failed": 1,
                                      "canceled": 0, "aborted": 0, "waiting": 48},
                       "cancel_reason": null}]"#;
        let jobs: Vec<HqJobSummary> = serde_json::from_str(raw).unwrap();
        assert_eq!(jobs[0].id, 3);
        assert_eq!(jobs[0].task_stats.waiting, 48);
    }

    #[test]
    fn job_detail_json_parses_tasks_and_paths() {
        let raw = r#"[{"info": {"id": 3, "task_stats": {"running": 0, "finished": 1,
                          "failed": 1, "canceled": 0, "aborted": 0, "waiting": 0}},
                       "tasks": [
                         {"id": 0, "state": "finished", "cwd": "/w",
                          "stdout": ["/w/job-3/0-0.stdout"], "stderr": null},
                         {"id": 1, "state": "failed", "error": "exit code 1",
                          "stdout": null}
                       ]}]"#;
        let docs: Vec<HqJobDetailDoc> = serde_json::from_str(raw).unwrap();
        assert_eq!(docs[0].tasks.len(), 2);
        assert_eq!(
            stdio_path(&docs[0].tasks[0].stdout).unwrap(),
            Path::new("/w/job-3/0-0.stdout")
        );
        assert_eq!(docs[0].tasks[1].error.as_deref(), Some("exit code 1"));
    }

    // ── argument builders ──

    #[test]
    fn arg_builders_match_the_hq_cli() {
        assert_eq!(
            server_start_args(),
            ["server", "start", "--host", "127.0.0.1"]
        );
        assert_eq!(job_list_args(), ["job", "list", "--output-mode", "json"]);
        assert_eq!(
            job_info_args(3),
            ["job", "info", "3", "--output-mode", "json"]
        );
        assert_eq!(cancel_args(3), ["job", "cancel", "3"]);
        assert_eq!(
            submit_file_args(Path::new("/d/j.toml")),
            ["job", "submit-file", "/d/j.toml", "--output-mode", "json"]
        );
        assert_eq!(
            alloc_add_args(
                HqScheduler::Slurm,
                "1h",
                &[
                    "--partition=main".to_string(),
                    "--account=alloc".to_string()
                ]
            ),
            [
                "alloc",
                "add",
                "slurm",
                "--time-limit",
                "1h",
                "--",
                "--partition=main",
                "--account=alloc"
            ]
        );
        assert_eq!(
            alloc_add_args(HqScheduler::Pbs, "30m", &[]),
            ["alloc", "add", "pbs", "--time-limit", "30m", "--"]
        );
    }

    #[test]
    fn global_server_dir_comes_before_the_subcommand() {
        let backend = local_backend(Path::new("/data/prism/hyperqueue"));
        let cmd = backend.hq_command(&job_list_args());
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--server-dir",
                "/data/prism/hyperqueue",
                "job",
                "list",
                "--output-mode",
                "json"
            ]
        );
    }

    // ── offline policy ──

    #[test]
    #[allow(clippy::await_holding_lock)]
    fn standalone_is_allowed_offline_autoalloc_is_refused() {
        let _lock = env_lock();
        let _guard = OfflineEnvGuard::set("1");

        let local = HyperQueueBackend::new(HyperQueueConfig::standalone("/tmp/hq", 1));
        assert!(local.check_offline().is_ok());

        let cluster = HyperQueueBackend::new(HyperQueueConfig::autoalloc(
            "/tmp/hq",
            HqScheduler::Slurm,
            "1h",
            vec![],
        ));
        let err = format!("{:#}", cluster.check_offline().unwrap_err());
        assert!(err.contains("offline"), "{err}");
        assert!(err.contains("slurm"), "{err}");
    }

    /// Wiring test, mirroring `byoc.rs`: all four verbs call `check_offline`
    /// and refuse offline autoalloc BEFORE any process is spawned.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn no_verb_spawns_hq_offline_in_autoalloc_mode() {
        let _lock = env_lock();
        let _guard = OfflineEnvGuard::set("1");

        let backend = HyperQueueBackend::new(HyperQueueConfig::autoalloc(
            "/tmp/prism-hq-offline-test",
            HqScheduler::Pbs,
            "1h",
            vec![],
        ));
        let plan = task_plan("corpus", 2);

        let refusals = [
            format!("{:#}", backend.submit(&plan).await.unwrap_err()),
            format!("{:#}", backend.status(Uuid::nil()).await.unwrap_err()),
            format!("{:#}", backend.results(Uuid::nil()).await.unwrap_err()),
            format!("{:#}", backend.cancel(Uuid::nil()).await.unwrap_err()),
        ];
        for refusal in &refusals {
            assert!(refusal.contains("offline"), "{refusal}");
        }
        let _restore = OfflineEnvGuard::clear();
    }

    // ── honest failure modes ──

    #[tokio::test]
    async fn missing_binary_names_the_install_command() {
        let dir = std::env::temp_dir().join(format!("prism-hq-missing-{}", Uuid::new_v4()));
        let mut config = HyperQueueConfig::standalone(&dir, 1);
        config.binary = "/nonexistent/hq-does-not-exist".into();
        let backend = HyperQueueBackend::new(config);

        let err = format!(
            "{:#}",
            backend.submit(&task_plan("t", 1)).await.unwrap_err()
        );
        assert!(err.contains("cargo install hyperqueue"), "{err}");
        assert!(err.contains("does not fall back"), "{err}");
    }

    #[tokio::test]
    async fn status_without_a_recorded_job_id_fails_honestly() {
        let backend = local_backend(Path::new("/tmp/never-used"));
        let err = format!("{:#}", backend.status(Uuid::new_v4()).await.unwrap_err());
        assert!(err.contains("no HyperQueue job id recorded"), "{err}");
    }

    #[tokio::test]
    async fn empty_task_set_is_refused() {
        let backend = local_backend(Path::new("/tmp/never-used"));
        let err = format!("{:#}", backend.submit_tasks("t", &[]).await.unwrap_err());
        assert!(err.contains("empty"), "{err}");
    }

    #[tokio::test]
    async fn task_with_empty_command_is_refused() {
        let backend = local_backend(Path::new("/tmp/never-used"));
        let err = format!(
            "{:#}",
            backend
                .submit_tasks("t", &[HqTask::new(vec![])])
                .await
                .unwrap_err()
        );
        assert!(err.contains("empty command"), "{err}");
    }

    // ── end-to-end against a fake `hq` binary ──

    /// A shell script impersonating `hq`: proves the full subprocess wiring
    /// (global `--server-dir` placement, JSON parsing, status folding,
    /// results reading) without HyperQueue installed.
    #[cfg(unix)]
    fn write_fake_hq(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-hq.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn submit_status_results_cancel_against_fake_hq() {
        let dir = std::env::temp_dir().join(format!("prism-hq-fake-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let stdout_file = dir.join("task0.stdout");
        std::fs::write(&stdout_file, "{\"papers\": 91}").unwrap();

        let script = format!(
            r#"case "$*" in
              # Real `hq` is clap-parsed: an unknown global flag is exit 2,
              # not a warning. This double used to glob on the SUBCOMMAND
              # only, so it happily answered `--output-type json` — a flag
              # HyperQueue does not have (the real one is `--output-mode`).
              # Every JSON call would have failed against a real server while
              # every test stayed green. A double that accepts anything
              # proves nothing, so it now rejects what clap would reject.
              *"--output-type"*) echo "error: unexpected argument '--output-type' found" >&2; exit 2 ;;
              *"worker list"*) echo '[{{"id": 1}}]' ;;
              *"job list"*) echo '[{{"id": 7, "task_stats": {{"running": 0, "finished": 2, "failed": 0, "canceled": 0, "aborted": 0, "waiting": 0}}}}]' ;;
              *"job submit-file"*) echo '{{"id": 7}}' ;;
              *"job info 7"*) echo '[{{"info": {{"id": 7, "task_stats": {{"running": 0, "finished": 2, "failed": 0, "canceled": 0, "aborted": 0, "waiting": 0}}}}, "tasks": [{{"id": 0, "state": "finished", "stdout": ["{out}"]}}, {{"id": 1, "state": "finished", "stdout": ["{out}"]}}]}}]' ;;
              *"job cancel 7"*) echo "cancelled" ;;
              *) echo 'unexpected hq call: '"$*" >&2; exit 1 ;;
            esac"#,
            out = stdout_file.display()
        );
        let binary = write_fake_hq(&dir, &script);

        let mut config = HyperQueueConfig::standalone(&dir, 1);
        config.binary = binary.display().to_string();
        let backend = HyperQueueBackend::new(config);

        // Submit: probe passes, fake worker exists, submit-file returns id 7.
        let job_id = backend.submit(&task_plan("corpus", 2)).await.unwrap();
        assert_eq!(backend.hq_job_id(job_id).await, Some(7));
        assert!(
            dir.join("prism-jobs")
                .join(format!("prism-{job_id}.toml"))
                .exists()
        );

        // Status folds counters into the shared vocabulary.
        assert!(matches!(
            backend.status(job_id).await.unwrap(),
            JobStatus::Completed
        ));

        // Per-task statuses reuse JobStatus.
        let statuses = backend.task_statuses(job_id).await.unwrap();
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].task_id, 0);
        assert_eq!(statuses[0].status, JobStatus::Completed);

        // Results parse each task's stdout as JSON.
        let results = backend.results(job_id).await.unwrap();
        assert_eq!(results["job"], 7);
        assert_eq!(results["tasks"][0]["result"]["papers"], 91);

        // Cancel of a terminal job short-circuits to Ok without calling hq.
        backend.cancel(job_id).await.unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn unpollable_task_set_reports_the_failure_not_running() {
        let dir = std::env::temp_dir().join(format!("prism-hq-down-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // job list fails (server down); worker list fine; submit works.
        let script = r#"case "$*" in
          *"worker list"*) echo '[{"id": 1}]' ;;
          *"job submit-file"*) echo '{"id": 9}' ;;
          *"job list"*) echo 'server not found' >&2; exit 1 ;;
          *) exit 1 ;;
        esac"#;
        let binary = write_fake_hq(&dir, script);
        let mut config = HyperQueueConfig::standalone(&dir, 1);
        config.binary = binary.display().to_string();
        config.startup_timeout = Duration::from_secs(1);
        let backend = HyperQueueBackend::new(config);

        // ensure_ready: probe fails -> attempts server start (fake hq exits 1
        // for `server start`) -> probe keeps failing -> honest startup error.
        let err = format!(
            "{:#}",
            backend.submit(&task_plan("t", 1)).await.unwrap_err()
        );
        assert!(err.contains("did not come up"), "{err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn vanished_job_reports_failed_not_running() {
        let dir = std::env::temp_dir().join(format!("prism-hq-vanish-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Server answers, but the job list is empty: the job vanished.
        let script = r#"case "$*" in
          *"worker list"*) echo '[{"id": 1}]' ;;
          *"job submit-file"*) echo '{"id": 11}' ;;
          *"job list"*) echo '[]' ;;
          *) exit 1 ;;
        esac"#;
        let binary = write_fake_hq(&dir, script);
        let mut config = HyperQueueConfig::standalone(&dir, 1);
        config.binary = binary.display().to_string();
        let backend = HyperQueueBackend::new(config);

        let job_id = backend.submit(&task_plan("t", 1)).await.unwrap();
        let status = backend.status(job_id).await.unwrap();
        assert!(
            matches!(&status, JobStatus::Failed { error } if error.contains("no longer exists")),
            "{status:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
