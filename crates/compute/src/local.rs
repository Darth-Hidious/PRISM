//! Local compute backend — dispatches jobs to Docker/Podman on the local machine.
//!
//! Wraps the container executor from `prism-node` (when wired) or shells out
//! to `docker run` / `podman run` directly. This is the default backend for
//! single-node PRISM deployments.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// Cap on the number of log lines fetched from a crashed container (VS1/F3).
/// A chatty container can emit MBs/GBs before crashing; without `--tail`, the
/// whole buffer is read into memory and then the downstream 30k cliff keeps
/// only the verbose HEAD — the actual crash line at the END would be lost.
/// `--tail` bounds the fetch; `crash_error_message` then head+tail-caps the
/// text so the crash line survives the cliff too.
const CRASH_LOG_TAIL_LINES: usize = 5_000;
/// Hard char cap (head + tail) on the crash log text we splice into the error
/// message. Keeps the bail! string bounded regardless of `--tail` line sizes.
const CRASH_LOG_TEXT_CHARS: usize = 20_000;

/// Local Docker/Podman compute backend.
pub struct LocalBackend {
    /// Container runtime binary ("docker" or "podman").
    runtime: String,
    /// Active container handles: job_id → container_name.
    active: Arc<RwLock<HashMap<Uuid, String>>>,
}

impl LocalBackend {
    pub fn new() -> Self {
        let runtime = detect_runtime().unwrap_or_else(|| "docker".into());
        Self {
            runtime,
            active: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_runtime(runtime: &str) -> Self {
        Self {
            runtime: runtime.to_string(),
            active: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn container_name(job_id: Uuid) -> String {
        format!("prism-compute-{}", job_id.as_simple())
    }
}

impl Default for LocalBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ComputeBackend for LocalBackend {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        let job_id = Uuid::new_v4();
        let container_name = Self::container_name(job_id);

        // Write inputs to a temp file that gets mounted into the container.
        let inputs_json = serde_json::to_string(&plan.inputs)?;
        let tmp_dir = std::env::temp_dir().join(format!("prism-{}", job_id.as_simple()));
        tokio::fs::create_dir_all(&tmp_dir).await?;
        tokio::fs::write(tmp_dir.join("inputs.json"), &inputs_json).await?;

        let mount = format!("{}:/workspace", tmp_dir.display());

        let output = Command::new(&self.runtime)
            .args([
                "run",
                "-d",
                "--name",
                &container_name,
                "--network",
                "none",
                "-v",
                &mount,
                "-e",
                &format!("PRISM_JOB_ID={job_id}"),
                "-e",
                "PRISM_INPUTS_PATH=/workspace/inputs.json",
                "-e",
                "PRISM_OUTPUT_PATH=/workspace/result.json",
                &plan.image,
            ])
            .output()
            .await
            .with_context(|| format!("failed to start {} container", self.runtime))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("{} run failed: {err}", self.runtime);
        }

        self.active.write().await.insert(job_id, container_name);

        tracing::info!(%job_id, image = %plan.image, "local compute job submitted");
        Ok(job_id)
    }

    async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        let active = self.active.read().await;
        let container_name = match active.get(&job_id) {
            Some(name) => name.clone(),
            // Unknown to this backend: never submitted, already cancelled, or
            // already collected via `results()` (which cleans up `active`).
            // Reporting `Completed` here would be a lie for jobs that never
            // existed — the caller has no way to tell "done" from "made up".
            None => bail!("no such local compute job: {job_id}"),
        };
        drop(active);

        let output = Command::new(&self.runtime)
            .args([
                "inspect",
                "--format",
                "{{.State.Status}}:{{.State.ExitCode}}",
                &container_name,
            ])
            .output()
            .await
            .context("container inspect failed")?;

        if !output.status.success() {
            return Ok(JobStatus::Failed {
                error: "container disappeared".into(),
            });
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let raw = raw.trim();
        let (status, exit_code) = raw.split_once(':').unwrap_or((raw, "1"));

        match status {
            "running" | "created" => Ok(JobStatus::Running { progress: 0.5 }),
            "exited" | "dead" | "stopped" => {
                let code: i32 = exit_code.parse().unwrap_or(1);
                if code == 0 {
                    Ok(JobStatus::Completed)
                } else {
                    Ok(JobStatus::Failed {
                        error: format!("exited with code {code}"),
                    })
                }
            }
            _ => Ok(JobStatus::Running { progress: 0.0 }),
        }
    }

    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value> {
        let tmp_dir = std::env::temp_dir().join(format!("prism-{}", job_id.as_simple()));
        let result_path = tmp_dir.join("result.json");

        if !result_path.exists() {
            // The container crashed before writing result.json. The OLD code
            // bailed with a bare "no result file for job {job_id}", throwing
            // away the real traceback in the container's logs. Surface those
            // logs + the exit code instead (mirroring byoc.rs::results(), which
            // already does `docker logs`). MUST fetch before cleanup() —
            // cleanup() runs `docker rm -f`, which destroys the logs.
            let container_name = Self::container_name(job_id);
            let exit_code = self.fetch_exit_code(&container_name).await;
            let logs = self.fetch_logs(&container_name).await;
            // Still clean up on the error path so we don't leak the container.
            self.cleanup(job_id).await;
            bail!("{}", crash_error_message(job_id, exit_code, &logs));
        }

        let content = tokio::fs::read_to_string(&result_path).await?;
        let value: serde_json::Value = serde_json::from_str(&content)?;

        // Cleanup container.
        self.cleanup(job_id).await;

        Ok(value)
    }

    async fn cancel(&self, job_id: Uuid) -> Result<()> {
        let active = self.active.read().await;
        if let Some(name) = active.get(&job_id) {
            let name = name.clone();
            drop(active);

            Command::new(&self.runtime)
                .args(["kill", &name])
                .output()
                .await
                .ok();

            Command::new(&self.runtime)
                .args(["rm", "-f", &name])
                .output()
                .await
                .ok();

            self.active.write().await.remove(&job_id);
            tracing::info!(%job_id, "local compute job cancelled");
        }
        Ok(())
    }
}

impl LocalBackend {
    async fn cleanup(&self, job_id: Uuid) {
        let mut active = self.active.write().await;
        if let Some(name) = active.remove(&job_id) {
            Command::new(&self.runtime)
                .args(["rm", "-f", &name])
                .output()
                .await
                .ok();
        }
    }

    /// Fetch the container's exit code via `inspect`. Returns None if the
    /// container is gone or `inspect` fails for any reason — callers must
    /// tolerate "unknown" rather than propagating a hard error (the logs are
    /// the load-bearing signal on a crash path).
    async fn fetch_exit_code(&self, container_name: &str) -> Option<i32> {
        let output = Command::new(&self.runtime)
            .args(["inspect", "--format", "{{.State.ExitCode}}", container_name])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        raw.trim().parse::<i32>().ok()
    }

    /// Fetch the container's combined stdout+stderr logs. Returns an empty
    /// string on any failure (no `--stdout`/`--no-stderr` flags — bare
    /// `docker logs` already merges both streams, matching byoc.rs). Capped
    /// to the last `CRASH_LOG_TAIL_LINES` lines so a chatty container cannot
    /// OOM us or push the actual crash line past the downstream 30k cliff.
    async fn fetch_logs(&self, container_name: &str) -> String {
        let output = Command::new(&self.runtime)
            .args([
                "logs",
                "--tail",
                &CRASH_LOG_TAIL_LINES.to_string(),
                container_name,
            ])
            .output()
            .await;
        match output {
            Ok(o) => {
                let mut combined = String::new();
                combined.push_str(&String::from_utf8_lossy(&o.stdout));
                if !o.stderr.is_empty() {
                    combined.push_str(&String::from_utf8_lossy(&o.stderr));
                }
                combined
            }
            Err(_) => String::new(),
        }
    }
}

/// Build the error message for a crashed container (no result.json). Pure so
/// it can be unit-tested without docker. The container's logs + exit code are
/// the honest "what actually went wrong" signal — far better than a bare
/// "no result file" that tells the agent nothing it can act on.
///
/// The crash line lives at the END of the log, so when the log exceeds
/// `CRASH_LOG_TEXT_CHARS` we keep a head (context) + tail (the crash line)
/// with an explicit elided-middle marker — otherwise the downstream 30k cliff
/// would keep only the verbose head and drop the actual error.
fn crash_error_message(job_id: Uuid, exit_code: Option<i32>, logs: &str) -> String {
    let code_part = match exit_code {
        Some(c) => format!("exited with code {c}"),
        None => "exit code unknown (container already removed)".to_string(),
    };
    let logs = logs.trim();
    if logs.is_empty() {
        return format!(
            "compute job {job_id} crashed before writing a result ({code_part}); \
             no container logs were available"
        );
    }
    let bounded = cap_log_head_tail(logs, CRASH_LOG_TEXT_CHARS);
    format!(
        "compute job {job_id} crashed before writing a result ({code_part}); \
         container logs:\n{bounded}"
    )
}

/// Cap a log string to `max` chars keeping a head and a tail with an explicit
/// elided-middle marker. The tail is load-bearing (crash lines live there).
/// Pure and whole-char-safe.
fn cap_log_head_tail(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    // Favor the tail (crash line) over the head (context): 1/4 head, 3/4 tail.
    let head = max / 4;
    let tail = max - head;
    let head_str: String = s.chars().take(head).collect();
    let skip = total - tail;
    let tail_byte_start = s
        .char_indices()
        .nth(skip)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(s.len());
    let tail_str = &s[tail_byte_start..];
    let elided = total - head - tail;
    format!(
        "{head_str}\n[…{elided} chars elided — showing head + tail; the crash line is at the end…]\n{tail_str}"
    )
}

/// Detect available container runtime.
fn detect_runtime() -> Option<String> {
    for bin in ["docker", "podman"] {
        if which(bin) {
            return Some(bin.to_string());
        }
    }
    None
}

fn which(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .any(|dir| dir.join(binary).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_name_is_deterministic() {
        let id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let name = LocalBackend::container_name(id);
        assert!(name.starts_with("prism-compute-"));
        assert!(name.contains("00000000"));
    }

    #[test]
    fn detect_runtime_returns_something_or_none() {
        // Just verifies it doesn't panic.
        let _ = detect_runtime();
    }

    #[tokio::test]
    async fn status_of_unknown_job_id_is_an_honest_error_not_completed() {
        // A job ID that was never submitted (or was already cleaned up) must
        // never be reported as `Completed` — that would be indistinguishable
        // from an actual success and would mislead callers billing or acting
        // on the result.
        let backend = LocalBackend::with_runtime("docker");
        let unknown_job = Uuid::new_v4();

        let result = backend.status(unknown_job).await;

        assert!(
            result.is_err(),
            "status of an untracked job must be an error, not Ok(Completed)"
        );
        let message = result.unwrap_err().to_string();
        assert!(
            message.contains(&unknown_job.to_string()),
            "error should identify which job id was not found: {message}"
        );
    }

    // --- Edge-case tests ---

    #[test]
    fn container_name_uniqueness_different_uuids_produce_different_names() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        // Extremely unlikely to collide; two random v4 UUIDs must be different.
        assert_ne!(id_a, id_b);
        let name_a = LocalBackend::container_name(id_a);
        let name_b = LocalBackend::container_name(id_b);
        assert_ne!(
            name_a, name_b,
            "container names for different UUIDs must differ"
        );
    }

    #[test]
    fn container_name_always_starts_with_prism_compute() {
        // Verify the prefix invariant holds for several random UUIDs.
        for _ in 0..10 {
            let id = Uuid::new_v4();
            let name = LocalBackend::container_name(id);
            assert!(
                name.starts_with("prism-compute-"),
                "container name '{name}' does not start with 'prism-compute-'"
            );
        }
    }

    #[test]
    fn container_name_embeds_simple_uuid_without_hyphens() {
        // as_simple() formats the UUID without hyphens.
        let id = Uuid::parse_str("12345678-1234-4000-8000-000000000abc").unwrap();
        let name = LocalBackend::container_name(id);
        // The simple form contains no hyphens inside the UUID portion.
        let suffix = name.strip_prefix("prism-compute-").unwrap();
        assert!(
            !suffix.contains('-'),
            "UUID suffix should use simple (no-hyphen) format"
        );
    }

    #[test]
    fn local_backend_with_runtime_stores_runtime_name() {
        let backend = LocalBackend::with_runtime("podman");
        assert_eq!(backend.runtime, "podman");

        let backend2 = LocalBackend::with_runtime("docker");
        assert_eq!(backend2.runtime, "docker");

        // Arbitrary custom runtime name is stored verbatim.
        let backend3 = LocalBackend::with_runtime("nerdctl");
        assert_eq!(backend3.runtime, "nerdctl");
    }

    // ── VS1 / F3: surface container logs on crash ──────────────────────

    #[test]
    fn f3_crash_message_names_job_exit_code_and_logs() {
        // The pure message builder — the load-bearing signal is that the
        // container's traceback and exit code reach the caller, not a bare
        // "no result file" they can't act on.
        let id = Uuid::parse_str("00000000-0000-4000-8000-00000000000f").unwrap();
        let msg = crash_error_message(
            id,
            Some(137),
            "Traceback (most recent call last):\nRuntimeError: OOM\n",
        );
        assert!(
            msg.contains(&id.to_string()),
            "message names the job: {msg}"
        );
        assert!(msg.contains("137"), "message includes the exit code: {msg}");
        assert!(
            msg.contains("RuntimeError: OOM"),
            "message includes the container logs: {msg}"
        );
        assert!(
            !msg.starts_with("no result file"),
            "must not be the opaque old message: {msg}"
        );
    }

    #[test]
    fn f3_crash_message_keeps_crash_line_when_log_is_huge() {
        // The crash line lives at the END. A head-only cap (or the downstream
        // 30k cliff) would keep the verbose head and drop the crash line.
        // crash_error_message must head+tail-cap so the crash survives.
        let id = Uuid::new_v4();
        let mut log = String::new();
        // ~50k chars of noise, then the real crash line at the end.
        for _ in 0..5000 {
            log.push_str("verbose build line blah blah blah\n");
        }
        log.push_str("RuntimeError: out of memory in material simulation\n");
        let msg = crash_error_message(id, Some(137), &log);

        assert!(
            msg.contains("RuntimeError: out of memory in material simulation"),
            "the crash line (at the end) must survive head+tail cap: {}",
            &msg[msg.len().saturating_sub(200)..]
        );
        assert!(
            msg.contains("chars elided"),
            "elision of the huge log must be marked: {}",
            &msg[..msg.len().min(160)]
        );
        // The message itself stays bounded — no multi-MB bail! string.
        assert!(
            msg.len() < 60_000,
            "crash message must be bounded, got {} bytes",
            msg.len()
        );
    }

    #[test]
    fn f3_crash_message_handles_unknown_exit_code() {
        // Container already removed by the time we inspect -> exit code is
        // None. Must still produce an honest, non-empty message.
        let id = Uuid::new_v4();
        let msg = crash_error_message(id, None, "partial log\n");
        assert!(msg.contains(&id.to_string()));
        assert!(
            msg.contains("unknown") || msg.contains("partial log"),
            "tolerates unknown exit code: {msg}"
        );
    }

    #[test]
    fn f3_crash_message_handles_empty_logs() {
        // No logs recoverable — still honest about what happened.
        let id = Uuid::new_v4();
        let msg = crash_error_message(id, Some(1), "");
        assert!(msg.contains(&id.to_string()));
        assert!(msg.contains("1"));
        assert!(
            msg.contains("no container logs were available"),
            "honest about missing logs rather than faking silence: {msg}"
        );
    }

    #[tokio::test]
    async fn f3_results_on_unknown_job_surfaces_error_with_job_id_not_silent_ok() {
        // A job id that was never submitted has no result.json. results()
        // must surface a real error naming the job — and must NOT return
        // Ok (which would be indistinguishable from a successful empty run).
        // The docker logs/inspect calls fail on a nonexistent container and
        // are handled gracefully, so this test does not require real docker.
        let backend = LocalBackend::with_runtime("docker");
        let unknown = Uuid::new_v4();

        let result = backend.results(unknown).await;

        assert!(
            result.is_err(),
            "missing result.json must be an error, not Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains(&unknown.to_string()),
            "error must name the job id: {msg}"
        );
        assert!(
            !msg.starts_with("no result file"),
            "must be richer than the old opaque message: {msg}"
        );
    }
}
