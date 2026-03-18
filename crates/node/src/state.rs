//! Crash-safe local state for prism-node.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const ACTIVE_JOBS_FILE: &str = "node-active-jobs.json";
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ActiveJobState {
    #[serde(default)]
    jobs: BTreeMap<Uuid, ActiveJobRecord>,
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

pub fn remove_active_job(state_dir: &Path, job_id: Uuid) -> Result<Option<ActiveJobRecord>> {
    let mut state = load_active_jobs(state_dir)?;
    let removed = state.jobs.remove(&job_id);
    save_active_jobs(state_dir, &state)?;
    Ok(removed)
}

pub fn active_jobs(state_dir: &Path) -> Result<Vec<ActiveJobRecord>> {
    Ok(load_active_jobs(state_dir)?.jobs.into_values().collect())
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

fn load_active_jobs(state_dir: &Path) -> Result<ActiveJobState> {
    let path = active_jobs_path(state_dir);
    if !path.exists() {
        return Ok(ActiveJobState::default());
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
    fn shutdown_request_round_trip() {
        let tmp = TempDir::new().unwrap();
        assert!(!shutdown_requested(tmp.path()));
        write_shutdown_request(tmp.path()).unwrap();
        assert!(shutdown_requested(tmp.path()));
        clear_shutdown_request(tmp.path());
        assert!(!shutdown_requested(tmp.path()));
    }
}
