//! Container executor for node jobs.
//!
//! The initial production baseline is container-runtime oriented rather than
//! Docker-only. The node can execute jobs on Docker or Podman, writes
//! workspace-local manifests, and supports explicit cancellation/cleanup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tokio::process::Command;
use uuid::Uuid;

use crate::state;

/// Maximum output collected into memory (10 MB).
const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;
/// Maximum preview forwarded back to the platform.
const MAX_PREVIEW_BYTES: usize = 256 * 1024;
/// Maximum number of environment variables forwarded to a container job.
const MAX_ENV_VARS: usize = 128;
/// Maximum total environment payload size.
const MAX_ENV_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
}

impl ContainerRuntime {
    pub fn binary(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }

    fn mount_spec(self, workspace_dir: &Path) -> String {
        let base = format!("{}:/workspace", workspace_dir.display());
        if cfg!(target_os = "linux") && matches!(self, Self::Podman) {
            format!("{base}:Z")
        } else {
            base
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContainerJobSpec {
    pub job_id: Uuid,
    pub image: String,
    pub env_vars: BTreeMap<String, String>,
    pub gpu_type: Option<String>,
    pub timeout_secs: u64,
    pub allow_network: bool,
    pub workspace_dir: PathBuf,
    /// Memory limit for the container (e.g. "8g", "16g"). If None, uses 75% of system RAM.
    pub memory_limit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContainerDeploymentSpec {
    pub deployment_id: Uuid,
    pub image: String,
    pub env_vars: BTreeMap<String, String>,
    pub gpu_type: Option<String>,
    pub port: u16,
    pub command: Option<Vec<String>>,
    /// Memory limit for the container (e.g. "8g", "16g"). If None, uses 75% of system RAM.
    pub memory_limit: Option<String>,
}

/// Result of a completed job.
#[derive(Debug, Clone)]
pub struct JobOutput {
    pub stdout_preview: String,
    pub stderr_preview: String,
    pub exit_code: i32,
    pub duration_secs: u64,
    pub output_path: PathBuf,
    pub log_lines: Vec<String>,
}

#[derive(Debug, Serialize)]
struct JobManifest<'a> {
    job_id: Uuid,
    image: &'a str,
    runtime: &'a str,
    exit_code: i32,
    duration_secs: u64,
    stdout_log: String,
    stderr_log: String,
    stdout_preview: &'a str,
    stderr_preview: &'a str,
}

pub fn resolve_container_runtime(preferred: Option<&str>) -> Option<ContainerRuntime> {
    if let Some(runtime) = preferred {
        let runtime = runtime.trim().to_ascii_lowercase();
        match runtime.as_str() {
            "docker" if binary_exists("docker") => return Some(ContainerRuntime::Docker),
            "podman" if binary_exists("podman") => return Some(ContainerRuntime::Podman),
            _ => {}
        }
    }

    if binary_exists("docker") {
        Some(ContainerRuntime::Docker)
    } else if binary_exists("podman") {
        Some(ContainerRuntime::Podman)
    } else {
        None
    }
}

pub fn runtime_handle(job_id: Uuid) -> String {
    format!("prism-job-{}", job_id.as_simple())
}

pub fn deployment_handle(deployment_id: Uuid) -> String {
    format!("prism-deploy-{}", deployment_id.as_simple())
}

pub fn sanitize_env_vars(env_vars: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let mut safe = BTreeMap::new();
    let mut total_bytes = 0usize;

    for (key, value) in env_vars.iter().take(MAX_ENV_VARS) {
        if !valid_env_key(key) {
            continue;
        }
        let pair_bytes = key.len() + value.len();
        if total_bytes + pair_bytes > MAX_ENV_BYTES {
            break;
        }
        safe.insert(key.clone(), value.clone());
        total_bytes += pair_bytes;
    }

    Ok(safe)
}

pub async fn execute_container_job(
    runtime: ContainerRuntime,
    spec: &ContainerJobSpec,
    on_progress: impl Fn(f64, &str),
) -> Result<JobOutput> {
    verify_runtime_available(runtime).await?;

    on_progress(0.0, "Pulling image...");
    let pull = Command::new(runtime.binary())
        .args(["pull", &spec.image])
        .output()
        .await
        .with_context(|| format!("failed to pull image with {}", runtime.binary()))?;

    if !pull.status.success() {
        let err = String::from_utf8_lossy(&pull.stderr);
        bail!("{} pull failed: {err}", runtime.binary());
    }

    on_progress(0.1, "Starting container...");

    let container_name = runtime_handle(spec.job_id);
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.clone(),
        "--label".to_string(),
        format!("prism.job_id={}", spec.job_id),
        "--workdir".to_string(),
        "/workspace".to_string(),
        "-v".to_string(),
        runtime.mount_spec(&spec.workspace_dir),
    ];

    if !spec.allow_network {
        args.push("--network".to_string());
        args.push("none".to_string());
    }

    if spec.gpu_type.is_some() {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    let mem_limit = spec.memory_limit.clone().unwrap_or_else(|| {
        let sys = sysinfo::System::new_all();
        let total_gb = sys.total_memory() / 1024 / 1024 / 1024;
        let limit_gb = (total_gb * 3 / 4).max(2); // 75% of system RAM, minimum 2 GB
        format!("{limit_gb}g")
    });
    args.push("--memory".to_string());
    args.push(mem_limit);

    let env_vars = sanitize_env_vars(&spec.env_vars)?;
    let prism_env = [
        ("PRISM_JOB_ID", spec.job_id.to_string()),
        ("PRISM_WORKSPACE", "/workspace".to_string()),
        ("PRISM_INPUTS_PATH", "/workspace/inputs.json".to_string()),
        ("PRISM_OUTPUT_PATH", "/workspace/result.json".to_string()),
    ];

    for (key, value) in &env_vars {
        args.push("-e".to_string());
        args.push(format!("{key}={value}"));
    }
    for (key, value) in prism_env {
        args.push("-e".to_string());
        args.push(format!("{key}={value}"));
    }

    args.push(spec.image.clone());

    let start = std::time::Instant::now();

    let run = Command::new(runtime.binary())
        .args(&args)
        .output()
        .await
        .with_context(|| format!("failed to start {} container", runtime.binary()))?;

    if !run.status.success() {
        let err = String::from_utf8_lossy(&run.stderr);
        bail!("{} run failed: {err}", runtime.binary());
    }

    let container_id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    tracing::info!(job_id = %spec.job_id, %container_id, runtime = runtime.as_str(), "container started");

    let result = tokio::time::timeout(
        Duration::from_secs(spec.timeout_secs),
        poll_container(runtime, &container_id, &on_progress),
    )
    .await;

    let duration_secs = start.elapsed().as_secs();

    match result {
        Ok(Ok(exit_code)) => {
            on_progress(0.9, "Collecting output...");
            let (stdout_full, stderr_full) = collect_output(runtime, &container_id).await?;
            let stdout_preview = truncate_output(&stdout_full, MAX_PREVIEW_BYTES);
            let stderr_preview = truncate_output(&stderr_full, MAX_PREVIEW_BYTES);
            write_logs(&spec.workspace_dir, &stdout_full, &stderr_full)?;
            let output_path = write_manifest(
                &spec.workspace_dir,
                spec.job_id,
                &spec.image,
                runtime,
                exit_code,
                duration_secs,
                &stdout_preview,
                &stderr_preview,
            )?;
            cleanup_container(runtime, &container_name).await;
            Ok(JobOutput {
                stdout_preview,
                stderr_preview,
                exit_code,
                duration_secs,
                output_path,
                log_lines: extract_log_lines(&stdout_full, &stderr_full),
            })
        }
        Ok(Err(e)) => {
            cleanup_container(runtime, &container_name).await;
            Err(e)
        }
        Err(_) => {
            tracing::warn!(job_id = %spec.job_id, timeout_secs = spec.timeout_secs, "job timed out");
            cancel_container_job(runtime, &container_name).await;
            bail!("job timed out after {}s", spec.timeout_secs);
        }
    }
}

pub async fn cancel_container_job(runtime: ContainerRuntime, handle: &str) {
    Command::new(runtime.binary())
        .args(["kill", handle])
        .output()
        .await
        .ok();
    cleanup_container(runtime, handle).await;
}

pub async fn start_container_deployment(
    runtime: ContainerRuntime,
    spec: &ContainerDeploymentSpec,
) -> Result<String> {
    verify_runtime_available(runtime).await?;

    let handle = deployment_handle(spec.deployment_id);

    let pull = Command::new(runtime.binary())
        .args(["pull", &spec.image])
        .output()
        .await
        .with_context(|| format!("failed to pull image with {}", runtime.binary()))?;

    if !pull.status.success() {
        let err = String::from_utf8_lossy(&pull.stderr);
        bail!("{} pull failed: {err}", runtime.binary());
    }

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        handle.clone(),
        "--label".to_string(),
        format!("prism.deployment_id={}", spec.deployment_id),
        "--restart".to_string(),
        "unless-stopped".to_string(),
        "-p".to_string(),
        format!("{}:{}", spec.port, spec.port),
    ];

    if spec.gpu_type.is_some() {
        args.push("--gpus".to_string());
        args.push("all".to_string());
    }

    let mem_limit = spec.memory_limit.clone().unwrap_or_else(|| {
        let sys = sysinfo::System::new_all();
        let total_gb = sys.total_memory() / 1024 / 1024 / 1024;
        let limit_gb = (total_gb * 3 / 4).max(2);
        format!("{limit_gb}g")
    });
    args.push("--memory".to_string());
    args.push(mem_limit);

    let env_vars = sanitize_env_vars(&spec.env_vars)?;
    for (key, value) in &env_vars {
        args.push("-e".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push("-e".to_string());
    args.push(format!("PORT={}", spec.port));

    args.push(spec.image.clone());
    if let Some(command) = &spec.command {
        args.extend(command.iter().cloned());
    }

    let run = Command::new(runtime.binary())
        .args(&args)
        .output()
        .await
        .with_context(|| format!("failed to start {} container", runtime.binary()))?;

    if !run.status.success() {
        let err = String::from_utf8_lossy(&run.stderr);
        bail!("{} run failed: {err}", runtime.binary());
    }

    Ok(handle)
}

pub async fn inspect_container_handle(
    runtime: ContainerRuntime,
    handle: &str,
) -> Result<(String, i32)> {
    let inspect = Command::new(runtime.binary())
        .args([
            "inspect",
            "--format",
            "{{.State.Status}}:{{.State.ExitCode}}",
            handle,
        ])
        .output()
        .await
        .with_context(|| format!("{} inspect failed", runtime.binary()))?;

    if !inspect.status.success() {
        bail!("container handle {handle} not found");
    }

    let output = String::from_utf8_lossy(&inspect.stdout);
    let output = output.trim();
    let (status, exit_code) = output.split_once(':').unwrap_or((output, "1"));
    Ok((status.to_string(), exit_code.parse().unwrap_or(1)))
}

pub async fn stop_container_handle(runtime: ContainerRuntime, handle: &str) {
    Command::new(runtime.binary())
        .args(["stop", handle])
        .output()
        .await
        .ok();
    cleanup_container(runtime, handle).await;
}

pub async fn cleanup_container(runtime: ContainerRuntime, handle: &str) {
    Command::new(runtime.binary())
        .args(["rm", "-f", handle])
        .output()
        .await
        .ok();
}

pub async fn cleanup_orphaned_job(runtime: &str, handle: &str) {
    if let Some(runtime) = resolve_container_runtime(Some(runtime)) {
        cancel_container_job(runtime, handle).await;
    }
}

async fn verify_runtime_available(runtime: ContainerRuntime) -> Result<()> {
    let output = Command::new(runtime.binary())
        .arg("--version")
        .output()
        .await
        .with_context(|| format!("{} not found", runtime.binary()))?;

    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "{} is installed but not responding correctly",
            runtime.binary()
        )
    }
}

async fn poll_container(
    runtime: ContainerRuntime,
    container_id: &str,
    on_progress: &impl Fn(f64, &str),
) -> Result<i32> {
    let mut polls = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        polls += 1;

        let inspect = Command::new(runtime.binary())
            .args([
                "inspect",
                "--format",
                "{{.State.Status}}:{{.State.ExitCode}}",
                container_id,
            ])
            .output()
            .await
            .with_context(|| format!("{} inspect failed", runtime.binary()))?;

        if !inspect.status.success() {
            bail!("container disappeared during execution");
        }

        let output = String::from_utf8_lossy(&inspect.stdout);
        let output = output.trim();
        let (status, exit_code_str) = output.split_once(':').unwrap_or((output, "1"));

        let progress = (0.15 + (polls as f64 * 0.05)).min(0.85);
        on_progress(progress, &format!("Running... ({status})"));

        if status == "exited" || status == "dead" || status == "stopped" {
            let exit_code: i32 = exit_code_str.parse().unwrap_or(1);
            return Ok(exit_code);
        }
    }
}

async fn collect_output(
    runtime: ContainerRuntime,
    container_id: &str,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let stdout = Command::new(runtime.binary())
        .args(["logs", "--stdout", "--no-stderr", container_id])
        .output()
        .await
        .context("failed to collect stdout")?;

    let stderr = Command::new(runtime.binary())
        .args(["logs", "--stderr", "--no-stdout", container_id])
        .output()
        .await
        .context("failed to collect stderr")?;

    Ok((
        truncate_bytes(&stdout.stdout, MAX_OUTPUT_BYTES),
        truncate_bytes(&stderr.stdout, MAX_OUTPUT_BYTES),
    ))
}

fn truncate_bytes(bytes: &[u8], limit: usize) -> Vec<u8> {
    if bytes.len() > limit {
        bytes[..limit].to_vec()
    } else {
        bytes.to_vec()
    }
}

fn truncate_output(bytes: &[u8], limit: usize) -> String {
    let truncated = if bytes.len() > limit {
        &bytes[..limit]
    } else {
        bytes
    };
    String::from_utf8_lossy(truncated).into_owned()
}

fn write_logs(workspace_dir: &Path, stdout: &[u8], stderr: &[u8]) -> Result<()> {
    std::fs::write(state::stdout_log_path(workspace_dir), stdout)
        .context("failed to write stdout log")?;
    std::fs::write(state::stderr_log_path(workspace_dir), stderr)
        .context("failed to write stderr log")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_manifest(
    workspace_dir: &Path,
    job_id: Uuid,
    image: &str,
    runtime: ContainerRuntime,
    exit_code: i32,
    duration_secs: u64,
    stdout_preview: &str,
    stderr_preview: &str,
) -> Result<PathBuf> {
    let manifest = JobManifest {
        job_id,
        image,
        runtime: runtime.as_str(),
        exit_code,
        duration_secs,
        stdout_log: state::stdout_log_path(workspace_dir).display().to_string(),
        stderr_log: state::stderr_log_path(workspace_dir).display().to_string(),
        stdout_preview,
        stderr_preview,
    };
    let path = state::result_manifest_path(workspace_dir);
    let body = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&path, format!("{body}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn extract_log_lines(stdout: &[u8], stderr: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    for content in [stdout, stderr] {
        for line in String::from_utf8_lossy(content).lines().rev().take(25) {
            lines.push(line.to_string());
        }
    }
    lines.reverse();
    lines
}

fn valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn binary_exists(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .any(|dir| {
            let plain = dir.join(binary);
            if plain.is_file() {
                return true;
            }
            if cfg!(windows) {
                for ext in ["exe", "cmd", "bat"] {
                    if dir.join(format!("{binary}.{ext}")).is_file() {
                        return true;
                    }
                }
            }
            false
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_environment_keys_and_caps_count() {
        let mut env = BTreeMap::new();
        env.insert("GOOD_KEY".to_string(), "value".to_string());
        env.insert("bad-key".to_string(), "nope".to_string());
        env.insert("lower".to_string(), "nope".to_string());

        let safe = sanitize_env_vars(&env).unwrap();
        assert_eq!(safe.len(), 1);
        assert_eq!(safe.get("GOOD_KEY").unwrap(), "value");
    }

    #[test]
    fn runtime_handle_is_stable() {
        let job_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        assert!(runtime_handle(job_id).starts_with("prism-job-"));
    }
}
