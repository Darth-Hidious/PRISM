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

/// Hard char cap (head + tail) on the crash log text we splice into the error
/// message. Keeps the bail! string bounded regardless of `--tail` line sizes.
/// (The line-count bound itself is `LOG_TAIL_LINE_STR`, applied by `logs_args`
/// on the single `docker logs` invocation both paths now share.)
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

    /// Re-adopt a job submitted by a PREVIOUS process.
    ///
    /// `active` is in-process state, so a fresh `prism job-status` starts with
    /// an empty map and would honestly refuse ("no such local compute job")
    /// even though the container is still right there. The container name is a
    /// pure function of the job id, so a durable job record is enough to
    /// re-address it. This does not assert the container exists — `status()`
    /// still asks Docker, and an inspect failure is still a loud error.
    pub async fn adopt(&self, job_id: Uuid) {
        self.active
            .write()
            .await
            .insert(job_id, Self::container_name(job_id));
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

        interpret_inspect(
            job_id,
            output.status.success(),
            &output.stdout,
            &output.stderr,
        )
    }

    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value> {
        let container_name = Self::container_name(job_id);
        let tmp_dir = std::env::temp_dir().join(format!("prism-{}", job_id.as_simple()));
        let result_path = tmp_dir.join("result.json");

        // The image's structured result.json is the authoritative contract when
        // present, and is independent of any log-collection glitch.
        if let Ok(parsed) = read_result_json(&result_path).await {
            self.cleanup(job_id).await;
            return Ok(parsed);
        }

        // No usable result.json. Fetch logs + exit code BEFORE cleanup() —
        // cleanup runs `docker rm -f`, which destroys the logs.
        let logs_result = self.collect_logs(&container_name).await;
        let exit_code = self.inspect_exit_code(&container_name).await;

        // Exit 0 without a result.json is NOT a crash — it is the normal shape
        // of an arbitrary image (`alpine echo hello`). Returning the captured
        // stdout/stderr as the result is what makes `prism run <any-image>`
        // useful. Calling that a crash (the pre-existing behaviour, which
        // bailed for ANY missing result.json) was a lie for exit-0 jobs.
        if exit_code == Some(0) {
            let value = match logs_result {
                Ok((stdout, stderr)) => build_logs_result(&stdout, &stderr, exit_code),
                // With no result.json the logs ARE the result, so a failed
                // collection must surface as an error rather than an empty
                // `{"stdout":"","stderr":""}` that looks like a silent success.
                Err(error) => {
                    self.cleanup(job_id).await;
                    return Err(error);
                }
            };
            self.cleanup(job_id).await;
            return Ok(value);
        }

        // Non-zero (or unknown) exit and no result.json: a genuine crash. Keep
        // the rich, bounded crash report — logs + exit code — rather than a
        // bare "no result file".
        let logs = match logs_result {
            Ok((stdout, stderr)) => format!("{stdout}{stderr}"),
            Err(_) => String::new(),
        };
        self.cleanup(job_id).await;
        bail!("{}", crash_error_message(job_id, exit_code, &logs));
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

    /// Collect a container's stdout/stderr via `docker logs`. Mirrors the
    /// single-invocation form used by `prism-node/src/executor.rs:collect_output`.
    ///
    /// `docker logs` (and `podman logs`) demux the container's output to the
    /// child process's fds, so from the `Output`: container stdout = `o.stdout`
    /// and container stderr = `o.stderr`. (Do NOT use the non-existent
    /// `--stdout`/`--stderr`/`--no-stdout`/`--no-stderr` flags — those cause
    /// exit 125 "unknown flag" on real Docker/Podman, silently yielding empty
    /// output. See `logs_args`.)
    ///
    /// `--tail` bounds how much the kernel must buffer: without it,
    /// `Command::output()` buffers the ENTIRE child stdout into a `Vec` before
    /// our `MAX_LOG_BYTES` truncation runs — a chatty container could buffer
    /// gigabytes first.
    ///
    /// Honest failure handling: if `docker logs` itself fails to run OR exits
    /// non-zero (e.g. the container was already removed), we return `Err` so
    /// `results()` can decide whether to fall back. We must NOT mask a real
    /// collection failure as empty-as-success.
    async fn collect_logs(&self, container_name: &str) -> Result<(String, String)> {
        let output = Command::new(&self.runtime)
            .args(logs_args(container_name))
            .output()
            .await
            .context("failed to run `docker logs`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "`{} logs` failed (status {}): {}",
                self.runtime,
                output.status,
                stderr.trim()
            );
        }

        Ok((
            truncate_to_string(&output.stdout, MAX_LOG_BYTES),
            truncate_to_string(&output.stderr, MAX_LOG_BYTES),
        ))
    }

    /// Read the container's exit code via `docker inspect`. Returns `None` on
    /// any failure so callers can omit it instead of guessing.
    async fn inspect_exit_code(&self, container_name: &str) -> Option<i32> {
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
}

/// Per-stream cap for captured `docker logs` output. Mirrors the preview cap
/// used by prism-node's executor so a chatty container can't blow up memory or
/// the JSON result.
const MAX_LOG_BYTES: usize = 256 * 1024;

/// Number of trailing log lines to request from `docker logs --tail`. Bounds
/// how much the runtime buffers before our `MAX_LOG_BYTES` truncation runs.
///
/// `LOG_TAIL_LINE_STR` is the canonical form used by [`logs_args`]; the u64
/// mirror exists for the round-trip test so the two can never drift.
#[cfg(test)]
const LOG_TAIL_LINES: u64 = 10_000;
const LOG_TAIL_LINE_STR: &str = "10000";

/// Build the argv (after the runtime binary) for a `docker logs`/`podman logs`
/// invocation. Pure so the arg construction is unit-testable without Docker.
///
/// Deliberately uses ONE invocation with NO per-stream flags: the
/// `--stdout`/`--stderr`/`--no-stdout`/`--no-stderr` flags do not exist on
/// real Docker (exit 125 "unknown flag") or Podman, so a per-stream form
/// silently yields empty output.
fn logs_args(container_name: &str) -> [&str; 4] {
    ["logs", "--tail", LOG_TAIL_LINE_STR, container_name]
}

/// Truncate a byte buffer to `limit` bytes and decode lossily as UTF-8.
fn truncate_to_string(bytes: &[u8], limit: usize) -> String {
    let bounded = if bytes.len() > limit {
        &bytes[..limit]
    } else {
        bytes
    };
    String::from_utf8_lossy(bounded).into_owned()
}

/// Interpret a `docker inspect` result into a [`JobStatus`]. Pure so the
/// transient-vs-terminal decision is unit-testable without Docker.
///
/// A FAILED `inspect` command (`inspect_ok == false`) is treated as a
/// TRANSIENT error (`Err`), not a failed job: it can be a momentary dockerd
/// restart, a load spike, or a genuinely missing container, and the poll loop
/// (poll.rs) must be free to retry it to its deadline. Returning `Ok(Failed)`
/// here — the old behaviour — made the poll loop's terminal-state arm bail the
/// whole run on a single hiccup during an otherwise-healthy job. A genuine
/// non-zero container exit is only ever reported via the `exited` arm, where
/// `inspect` succeeded and returned the real exit code.
fn interpret_inspect(
    job_id: Uuid,
    inspect_ok: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<JobStatus> {
    if !inspect_ok {
        let stderr = String::from_utf8_lossy(stderr);
        bail!("container inspect for {job_id} failed: {}", stderr.trim());
    }

    let raw = String::from_utf8_lossy(stdout);
    let raw = raw.trim();
    let (status, exit_code) = raw.split_once(':').unwrap_or((raw, "1"));

    Ok(match status {
        "running" | "created" => JobStatus::Running { progress: 0.5 },
        "exited" | "dead" | "stopped" => {
            let code: i32 = exit_code.parse().unwrap_or(1);
            if code == 0 {
                JobStatus::Completed
            } else {
                JobStatus::Failed {
                    error: format!("exited with code {code}"),
                }
            }
        }
        _ => JobStatus::Running { progress: 0.0 },
    })
}

/// Read and parse the job's `result.json` (host-side path under the tmp mount).
/// Returns `Err` for a missing file or invalid JSON so the caller can fall back
/// to the logs-based result.
async fn read_result_json(result_path: &std::path::Path) -> Result<serde_json::Value> {
    if !result_path.exists() {
        bail!("no result file at {}", result_path.display());
    }
    let content = tokio::fs::read_to_string(result_path).await?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("invalid JSON in {}", result_path.display()))?;
    Ok(value)
}

/// Build a JSON result object from captured logs when the image did not write
/// a `result.json`. Kept as a pure module fn so the "logs → result" mapping is
/// unit-testable without touching Docker.
fn build_logs_result(stdout: &str, stderr: &str, exit_code: Option<i32>) -> serde_json::Value {
    serde_json::json!({
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": exit_code,
    })
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

    // --- Pure helpers for the results() output contract (no Docker needed) ---

    #[test]
    fn build_logs_result_wraps_streams_and_exit_code() {
        let value = build_logs_result("hello\n", "warn\n", Some(0));
        let obj = value.as_object().expect("logs result is a JSON object");
        assert_eq!(obj["stdout"], serde_json::json!("hello\n"));
        assert_eq!(obj["stderr"], serde_json::json!("warn\n"));
        assert_eq!(obj["exit_code"], serde_json::json!(0));
    }

    #[test]
    fn build_logs_result_supports_missing_exit_code() {
        // A vanished container yields `None` for exit_code — must serialize as
        // JSON null, never panic, and never be mistaken for exit 0.
        let value = build_logs_result("", "", None);
        let obj = value.as_object().expect("logs result is a JSON object");
        assert!(obj["exit_code"].is_null(), "missing exit code must be null");
        assert_ne!(
            obj["exit_code"],
            serde_json::json!(0),
            "null must not equal exit code 0"
        );
    }

    #[tokio::test]
    async fn read_result_json_errors_when_file_missing() {
        let dir =
            std::env::temp_dir().join(format!("prism-test-nofile-{}", Uuid::new_v4().as_simple()));
        // Intentionally do NOT create the file.
        let path = dir.join("result.json");
        let result = read_result_json(&path).await;
        assert!(result.is_err(), "missing result.json must be an error");
        assert!(
            result.unwrap_err().to_string().contains("no result file"),
            "error should explain the file is absent"
        );
    }

    #[tokio::test]
    async fn read_result_json_errors_on_invalid_json() {
        let dir =
            std::env::temp_dir().join(format!("prism-test-badjson-{}", Uuid::new_v4().as_simple()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("result.json");
        tokio::fs::write(&path, b"not json {{{").await.unwrap();
        let result = read_result_json(&path).await;
        assert!(result.is_err(), "invalid JSON must be an error");
    }

    // --- Hermetic docker-logs arg construction (catches F1/F2 without Docker) ---

    #[test]
    fn logs_args_uses_single_invocation_with_no_per_stream_flags() {
        // Regression guard for the ship-blocker where log collection used the
        // non-existent `--stdout/--stderr/--no-stdout/--no-stderr` flags
        // (Docker exits 125 "unknown flag"), silently yielding empty output.
        // Plain `logs --tail <N> <c>` is correct for both docker and podman.
        let args = logs_args("prism-compute-deadbeef");
        assert_eq!(args[0], "logs", "first arg must be the logs subcommand");
        assert_eq!(args[1], "--tail", "must bound output with --tail");
        assert_eq!(
            args[2].parse::<u64>().unwrap(),
            LOG_TAIL_LINES,
            "--tail value must equal LOG_TAIL_LINES"
        );
        assert_eq!(
            args[3], "prism-compute-deadbeef",
            "container name must be last"
        );
    }

    #[test]
    fn logs_args_must_not_contain_per_stream_flags() {
        // The banned flags do not exist on real Docker/Podman and cause exit 125.
        for banned in ["--stdout", "--stderr", "--no-stdout", "--no-stderr"] {
            let args = logs_args("c");
            assert!(
                !args.contains(&banned),
                "`{banned}` must never appear in logs args (it is not a real docker flag): {args:?}"
            );
        }
    }

    // --- F3: a failed `docker inspect` must be a transient Err, not Ok(Failed) ---

    #[test]
    fn interpret_inspect_failed_command_is_transient_error_not_failed_status() {
        // A failed `docker inspect` (dockerd restart, load spike, missing
        // container) must be Err so the poll loop retries to its deadline.
        // Returning Ok(Failed{..}) made the loop's terminal arm abort the run
        // on a single hiccup — the bug this guards against.
        let job = Uuid::new_v4();
        let result = interpret_inspect(job, false, b"", b"No such container: prism-compute-x");
        assert!(
            result.is_err(),
            "a failed inspect must be Err (transient), not Ok(Failed)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("inspect"),
            "error should mention inspect: {msg}"
        );
    }

    #[test]
    fn interpret_inspect_exited_zero_is_completed() {
        let job = Uuid::new_v4();
        let status = interpret_inspect(job, true, b"exited:0", b"").unwrap();
        assert!(matches!(status, JobStatus::Completed));
    }

    #[test]
    fn interpret_inspect_exited_nonzero_is_failed() {
        let job = Uuid::new_v4();
        let status = interpret_inspect(job, true, b"exited:137", b"").unwrap();
        match status {
            JobStatus::Failed { error } => assert!(error.contains("137"), "got: {error}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn interpret_inspect_running_is_running() {
        let job = Uuid::new_v4();
        let status = interpret_inspect(job, true, b"running:0", b"").unwrap();
        assert!(matches!(status, JobStatus::Running { .. }));
    }
}
