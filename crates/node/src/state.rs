//! Crash-safe local state for prism-node.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const ACTIVE_JOBS_FILE: &str = "node-active-jobs.json";
const ACTIVE_DEPLOYMENTS_FILE: &str = "node-active-deployments.json";
const JOBS_DIR: &str = "node-jobs";
const SHUTDOWN_FILE: &str = "node.shutdown";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveJobRecord {
    pub job_id: Uuid,
    pub runtime: String,
    pub handle: String,
    pub workspace_dir: String,
    pub image: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveDeploymentRecord {
    pub deployment_id: Uuid,
    /// "runtime" for marc27-runtime deployments, or a container runtime like "docker".
    pub backend: String,
    /// Deployment id for runtime-backed deploys, container name for container-backed deploys.
    pub handle: String,
    #[serde(default)]
    pub runtime_url: Option<String>,
    pub endpoint_url: String,
    pub local_health_url: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ActiveJobState {
    #[serde(default)]
    jobs: BTreeMap<Uuid, ActiveJobRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ActiveDeploymentState {
    #[serde(default)]
    deployments: BTreeMap<Uuid, ActiveDeploymentRecord>,
}

pub fn jobs_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(JOBS_DIR)
}

pub fn workspace_dir(state_dir: &Path, job_id: Uuid) -> PathBuf {
    jobs_dir(state_dir).join(job_id.to_string())
}

pub fn result_manifest_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("result.json")
}

pub fn stdout_log_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("stdout.log")
}

pub fn stderr_log_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("stderr.log")
}

pub fn inputs_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("inputs.json")
}

pub fn metadata_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("metadata.json")
}

pub fn ensure_workspace(state_dir: &Path, job_id: Uuid) -> Result<PathBuf> {
    let workspace = workspace_dir(state_dir, job_id);
    fs::create_dir_all(&workspace)
        .with_context(|| format!("failed to create job workspace {}", workspace.display()))?;
    Ok(workspace)
}

pub fn register_active_job(state_dir: &Path, record: ActiveJobRecord) -> Result<()> {
    let mut state = load_active_jobs(state_dir)?;
    state.jobs.insert(record.job_id, record);
    save_active_jobs(state_dir, &state)
}

pub fn register_active_deployment(state_dir: &Path, record: ActiveDeploymentRecord) -> Result<()> {
    let mut state = load_active_deployments(state_dir)?;
    state.deployments.insert(record.deployment_id, record);
    save_active_deployments(state_dir, &state)
}

pub fn remove_active_job(state_dir: &Path, job_id: Uuid) -> Result<Option<ActiveJobRecord>> {
    let mut state = load_active_jobs(state_dir)?;
    let removed = state.jobs.remove(&job_id);
    save_active_jobs(state_dir, &state)?;
    Ok(removed)
}

pub fn remove_active_deployment(
    state_dir: &Path,
    deployment_id: Uuid,
) -> Result<Option<ActiveDeploymentRecord>> {
    let mut state = load_active_deployments(state_dir)?;
    let removed = state.deployments.remove(&deployment_id);
    save_active_deployments(state_dir, &state)?;
    Ok(removed)
}

pub fn active_jobs(state_dir: &Path) -> Result<Vec<ActiveJobRecord>> {
    Ok(load_active_jobs(state_dir)?.jobs.into_values().collect())
}

pub fn active_deployments(state_dir: &Path) -> Result<Vec<ActiveDeploymentRecord>> {
    Ok(load_active_deployments(state_dir)?
        .deployments
        .into_values()
        .collect())
}

pub fn write_shutdown_request(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    fs::write(shutdown_file_path(state_dir), b"shutdown\n")
        .context("failed to write node shutdown request")
}

pub fn shutdown_requested(state_dir: &Path) -> bool {
    shutdown_file_path(state_dir).exists()
}

pub fn clear_shutdown_request(state_dir: &Path) {
    let _ = fs::remove_file(shutdown_file_path(state_dir));
}

fn shutdown_file_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SHUTDOWN_FILE)
}

fn active_jobs_path(state_dir: &Path) -> PathBuf {
    state_dir.join(ACTIVE_JOBS_FILE)
}

fn active_deployments_path(state_dir: &Path) -> PathBuf {
    state_dir.join(ACTIVE_DEPLOYMENTS_FILE)
}

fn load_active_jobs(state_dir: &Path) -> Result<ActiveJobState> {
    let path = active_jobs_path(state_dir);
    if !path.exists() {
        return Ok(ActiveJobState::default());
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn load_active_deployments(state_dir: &Path) -> Result<ActiveDeploymentState> {
    let path = active_deployments_path(state_dir);
    if !path.exists() {
        return Ok(ActiveDeploymentState::default());
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_active_jobs(state_dir: &Path, state: &ActiveJobState) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = active_jobs_path(state_dir);
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(state)?;
    fs::write(&tmp, format!("{body}\n"))
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("failed to persist {}", path.display()))
}

fn save_active_deployments(state_dir: &Path, state: &ActiveDeploymentState) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = active_deployments_path(state_dir);
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(state)?;
    fs::write(&tmp, format!("{body}\n"))
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("failed to persist {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn active_job_round_trip() {
        let tmp = TempDir::new().unwrap();
        let record = ActiveJobRecord {
            job_id: Uuid::new_v4(),
            runtime: "docker".to_string(),
            handle: "prism-job-1".to_string(),
            workspace_dir: "/tmp/prism-job-1".to_string(),
            image: "marc27/test:latest".to_string(),
            started_at: Utc::now(),
        };

        register_active_job(tmp.path(), record.clone()).unwrap();
        let jobs = active_jobs(tmp.path()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, record.job_id);

        let removed = remove_active_job(tmp.path(), record.job_id)
            .unwrap()
            .unwrap();
        assert_eq!(removed.handle, "prism-job-1");
        assert!(active_jobs(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn active_deployment_round_trip() {
        let tmp = TempDir::new().unwrap();
        let record = ActiveDeploymentRecord {
            deployment_id: Uuid::new_v4(),
            backend: "runtime".to_string(),
            handle: "dep-1".to_string(),
            runtime_url: Some("http://127.0.0.1:8090".to_string()),
            endpoint_url: "http://192.168.1.50:9001".to_string(),
            local_health_url: "http://127.0.0.1:9001/health".to_string(),
            started_at: Utc::now(),
        };

        register_active_deployment(tmp.path(), record.clone()).unwrap();
        let deployments = active_deployments(tmp.path()).unwrap();
        assert_eq!(deployments.len(), 1);
        assert_eq!(deployments[0].deployment_id, record.deployment_id);

        let removed = remove_active_deployment(tmp.path(), record.deployment_id)
            .unwrap()
            .unwrap();
        assert_eq!(removed.handle, "dep-1");
        assert!(active_deployments(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn shutdown_request_round_trip() {
        let tmp = TempDir::new().unwrap();
        assert!(!shutdown_requested(tmp.path()));
        write_shutdown_request(tmp.path()).unwrap();
        assert!(shutdown_requested(tmp.path()));
        clear_shutdown_request(tmp.path());
        assert!(!shutdown_requested(tmp.path()));
    }

    // -- Edge cases ---

    #[test]
    fn active_jobs_empty_state_dir() {
        let tmp = TempDir::new().unwrap();
        let jobs = active_jobs(tmp.path()).unwrap();
        assert!(jobs.is_empty());
    }

    #[test]
    fn remove_nonexistent_job_returns_none() {
        let tmp = TempDir::new().unwrap();
        let result = remove_active_job(tmp.path(), Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn multiple_active_jobs() {
        let tmp = TempDir::new().unwrap();
        for i in 0..5 {
            register_active_job(
                tmp.path(),
                ActiveJobRecord {
                    job_id: Uuid::new_v4(),
                    runtime: "docker".into(),
                    handle: format!("container-{i}"),
                    workspace_dir: format!("/tmp/job-{i}"),
                    image: "test:latest".into(),
                    started_at: Utc::now(),
                },
            )
            .unwrap();
        }
        assert_eq!(active_jobs(tmp.path()).unwrap().len(), 5);
    }

    #[test]
    fn register_same_job_id_overwrites() {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::new_v4();
        let rec1 = ActiveJobRecord {
            job_id: id,
            runtime: "docker".into(),
            handle: "old-handle".into(),
            workspace_dir: "/tmp/old".into(),
            image: "img:v1".into(),
            started_at: Utc::now(),
        };
        let rec2 = ActiveJobRecord {
            job_id: id,
            runtime: "podman".into(),
            handle: "new-handle".into(),
            workspace_dir: "/tmp/new".into(),
            image: "img:v2".into(),
            started_at: Utc::now(),
        };

        register_active_job(tmp.path(), rec1).unwrap();
        register_active_job(tmp.path(), rec2).unwrap();

        let jobs = active_jobs(tmp.path()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].handle, "new-handle");
    }

    #[test]
    fn workspace_paths_are_correct() {
        let state = Path::new("/var/prism/state");
        let id = Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap();

        let ws = workspace_dir(state, id);
        assert!(ws
            .to_str()
            .unwrap()
            .contains("12345678-1234-1234-1234-123456789abc"));

        let result = result_manifest_path(&ws);
        assert!(result.ends_with("result.json"));

        let stdout = stdout_log_path(&ws);
        assert!(stdout.ends_with("stdout.log"));

        let stderr = stderr_log_path(&ws);
        assert!(stderr.ends_with("stderr.log"));

        let inputs = inputs_path(&ws);
        assert!(inputs.ends_with("inputs.json"));

        let meta = metadata_path(&ws);
        assert!(meta.ends_with("metadata.json"));
    }

    #[test]
    fn ensure_workspace_creates_dirs() {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::new_v4();
        let ws = ensure_workspace(tmp.path(), id).unwrap();
        assert!(ws.is_dir());
    }

    #[test]
    fn active_job_record_serde_roundtrip() {
        let record = ActiveJobRecord {
            job_id: Uuid::new_v4(),
            runtime: "docker".into(),
            handle: "prism-job-test".into(),
            workspace_dir: "/tmp/test".into(),
            image: "marc27/calphad:v1".into(),
            started_at: Utc::now(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: ActiveJobRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.job_id, record.job_id);
        assert_eq!(parsed.runtime, record.runtime);
    }

    #[test]
    fn clear_shutdown_idempotent() {
        let tmp = TempDir::new().unwrap();
        // Clearing when no shutdown file exists should not error.
        clear_shutdown_request(tmp.path());
        clear_shutdown_request(tmp.path());
        assert!(!shutdown_requested(tmp.path()));
    }

    #[test]
    fn state_persists_across_loads() {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::new_v4();
        register_active_job(
            tmp.path(),
            ActiveJobRecord {
                job_id: id,
                runtime: "docker".into(),
                handle: "persistent".into(),
                workspace_dir: "/tmp/persist".into(),
                image: "test:latest".into(),
                started_at: Utc::now(),
            },
        )
        .unwrap();

        // Load again from same directory.
        let jobs = active_jobs(tmp.path()).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, id);
    }
}
