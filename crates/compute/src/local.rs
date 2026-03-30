//! Local compute backend — dispatches jobs to Docker/Podman on the local machine.
//!
//! Wraps the container executor from `prism-node` (when wired) or shells out
//! to `docker run` / `podman run` directly. This is the default backend for
//! single-node PRISM deployments.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tokio::process::Command;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{ComputeBackend, ExperimentPlan, JobStatus};

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

        self.active
            .write()
            .await
            .insert(job_id, container_name);

        tracing::info!(%job_id, image = %plan.image, "local compute job submitted");
        Ok(job_id)
    }

    async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        let active = self.active.read().await;
        let container_name = match active.get(&job_id) {
            Some(name) => name.clone(),
            None => return Ok(JobStatus::Completed), // already cleaned up
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
            bail!("no result file for job {job_id}");
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

    // --- Edge-case tests ---

    #[test]
    fn container_name_uniqueness_different_uuids_produce_different_names() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        // Extremely unlikely to collide; two random v4 UUIDs must be different.
        assert_ne!(id_a, id_b);
        let name_a = LocalBackend::container_name(id_a);
        let name_b = LocalBackend::container_name(id_b);
        assert_ne!(name_a, name_b, "container names for different UUIDs must differ");
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
        assert!(!suffix.contains('-'), "UUID suffix should use simple (no-hyphen) format");
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
}
