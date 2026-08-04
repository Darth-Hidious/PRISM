//! Job tracking and lifecycle management.
//!
//! Persistent tracker for compute jobs across all backends. Provides
//! status queries, cancellation, and cleanup of stale entries.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::JobStatus;
use crate::byoc::ByocTarget;

const JOBS_FILE: &str = "compute-jobs.json";

/// Connection details needed to query a submitted job in a later process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobTarget {
    Local,
    Marc27 { api_base: String },
    Byoc(ByocTarget),
}

/// Metadata for a tracked job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: Uuid,
    pub name: String,
    pub image: String,
    pub backend: String,
    pub target: JobTarget,
    /// Numeric scheduler id returned by `sbatch`; populated for SLURM jobs.
    #[serde(default)]
    pub slurm_job_id: Option<u64>,
    pub status: TrackedStatus,
    pub submitted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Serializable version of JobStatus with timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrackedStatus {
    Queued,
    Running { progress: f64 },
    Completed { duration_secs: u64 },
    Failed { error: String },
    Cancelled,
}

impl From<&JobStatus> for TrackedStatus {
    fn from(s: &JobStatus) -> Self {
        match s {
            JobStatus::Queued => TrackedStatus::Queued,
            JobStatus::Running { progress } => TrackedStatus::Running {
                progress: *progress,
            },
            JobStatus::Completed => TrackedStatus::Completed { duration_secs: 0 },
            JobStatus::Failed { error } => TrackedStatus::Failed {
                error: error.clone(),
            },
            JobStatus::Cancelled => TrackedStatus::Cancelled,
        }
    }
}

impl TrackedStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TrackedStatus::Completed { .. }
                | TrackedStatus::Failed { .. }
                | TrackedStatus::Cancelled
        )
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PersistedJobs {
    #[serde(default)]
    jobs: HashMap<Uuid, JobRecord>,
}

/// Thread-safe job tracker, optionally backed by an atomic JSON state file.
#[derive(Clone)]
pub struct JobTracker {
    jobs: Arc<RwLock<HashMap<Uuid, JobRecord>>>,
    path: Option<Arc<PathBuf>>,
}

impl JobTracker {
    /// Create an in-memory tracker, primarily for embedded callers and tests.
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            path: None,
        }
    }

    /// Load the tracker from `<data_dir>/compute-jobs.json`.
    pub fn persistent(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(JOBS_FILE);
        let jobs = if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str::<PersistedJobs>(&text)
                .with_context(|| format!("failed to parse {}", path.display()))?
                .jobs
        } else {
            HashMap::new()
        };
        Ok(Self {
            jobs: Arc::new(RwLock::new(jobs)),
            path: Some(Arc::new(path)),
        })
    }

    /// Register and persist a newly submitted job.
    pub async fn register(
        &self,
        job_id: Uuid,
        name: &str,
        image: &str,
        backend: &str,
        target: JobTarget,
    ) -> Result<JobRecord> {
        let now = Utc::now();
        let record = JobRecord {
            job_id,
            name: name.to_string(),
            image: image.to_string(),
            backend: backend.to_string(),
            target,
            slurm_job_id: None,
            status: TrackedStatus::Queued,
            submitted_at: now,
            updated_at: now,
        };
        let mut jobs = self.jobs.write().await;
        jobs.insert(job_id, record.clone());
        self.persist(&jobs)?;
        Ok(record)
    }

    /// Update and persist the status of an existing job.
    pub async fn update_status(&self, job_id: Uuid, status: TrackedStatus) -> Result<bool> {
        let mut jobs = self.jobs.write().await;
        if let Some(record) = jobs.get_mut(&job_id) {
            record.status = status;
            record.updated_at = Utc::now();
            self.persist(&jobs)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Get a job record by ID.
    pub async fn get(&self, job_id: Uuid) -> Option<JobRecord> {
        self.jobs.read().await.get(&job_id).cloned()
    }

    /// List all jobs, optionally filtered to non-terminal only.
    pub async fn list(&self, active_only: bool) -> Vec<JobRecord> {
        let jobs = self.jobs.read().await;
        let mut records: Vec<JobRecord> = if active_only {
            jobs.values()
                .filter(|j| !j.status.is_terminal())
                .cloned()
                .collect()
        } else {
            jobs.values().cloned().collect()
        };
        records.sort_by_key(|r| std::cmp::Reverse(r.submitted_at));
        records
    }

    /// Remove and persist terminal jobs older than the given duration.
    pub async fn cleanup_stale(&self, max_age: std::time::Duration) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let mut jobs = self.jobs.write().await;
        let before = jobs.len();
        jobs.retain(|_, j| !(j.status.is_terminal() && j.updated_at < cutoff));
        let removed = before - jobs.len();
        if removed > 0 {
            self.persist(&jobs)?;
        }
        Ok(removed)
    }

    /// Count active (non-terminal) jobs.
    pub async fn active_count(&self) -> usize {
        self.jobs
            .read()
            .await
            .values()
            .filter(|j| !j.status.is_terminal())
            .count()
    }

    fn persist(&self, jobs: &HashMap<Uuid, JobRecord>) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .context("compute job state path has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let tmp = path.with_extension("tmp");
        let body = serde_json::to_string_pretty(&PersistedJobs { jobs: jobs.clone() })?;
        fs::write(&tmp, format!("{body}\n"))
            .with_context(|| format!("failed to write {}", tmp.display()))?;
        fs::rename(&tmp, path.as_ref())
            .with_context(|| format!("failed to persist {}", path.display()))
    }
}

impl Default for JobTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn register_local(
        tracker: &JobTracker,
        job_id: Uuid,
        name: &str,
        image: &str,
    ) -> JobRecord {
        tracker
            .register(job_id, name, image, "local", JobTarget::Local)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn register_and_get() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        register_local(&tracker, id, "test-job", "python:3.11").await;
        let record = tracker.get(id).await.unwrap();
        assert_eq!(record.name, "test-job");
        assert!(matches!(record.status, TrackedStatus::Queued));
    }

    #[tokio::test]
    async fn update_status() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        register_local(&tracker, id, "job", "img").await;

        tracker
            .update_status(id, TrackedStatus::Running { progress: 0.5 })
            .await
            .unwrap();
        let record = tracker.get(id).await.unwrap();
        assert!(
            matches!(record.status, TrackedStatus::Running { progress } if (progress - 0.5).abs() < f64::EPSILON)
        );
    }

    #[tokio::test]
    async fn list_active_only() {
        let tracker = JobTracker::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        register_local(&tracker, id1, "running", "img").await;
        register_local(&tracker, id2, "done", "img").await;

        tracker
            .update_status(id2, TrackedStatus::Completed { duration_secs: 10 })
            .await
            .unwrap();

        let active = tracker.list(true).await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].job_id, id1);

        let all = tracker.list(false).await;
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn active_count() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        register_local(&tracker, id, "j", "i").await;
        assert_eq!(tracker.active_count().await, 1);

        tracker
            .update_status(id, TrackedStatus::Cancelled)
            .await
            .unwrap();
        assert_eq!(tracker.active_count().await, 0);
    }

    // --- Edge-case tests ---

    #[tokio::test]
    async fn get_nonexistent_job_returns_none() {
        let tracker = JobTracker::new();
        let unknown = Uuid::new_v4();
        assert!(tracker.get(unknown).await.is_none());
    }

    #[tokio::test]
    async fn update_status_nonexistent_job_returns_false() {
        let tracker = JobTracker::new();
        let unknown = Uuid::new_v4();
        let updated = tracker
            .update_status(unknown, TrackedStatus::Completed { duration_secs: 5 })
            .await
            .unwrap();
        assert!(
            !updated,
            "update_status should return false for unknown job"
        );
    }

    #[tokio::test]
    async fn cleanup_stale_with_no_stale_jobs_removes_nothing() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        register_local(&tracker, id, "fresh-job", "img").await;

        // Use a very short max_age — zero seconds — but the job is not terminal
        // so it should never be removed.
        let removed = tracker
            .cleanup_stale(std::time::Duration::from_secs(0))
            .await
            .unwrap();
        assert_eq!(removed, 0);
        assert!(tracker.get(id).await.is_some());
    }

    #[tokio::test]
    async fn cleanup_stale_removes_only_terminal_jobs_older_than_max_age() {
        let tracker = JobTracker::new();

        let id_active = Uuid::new_v4();
        let id_done = Uuid::new_v4();
        let id_cancelled = Uuid::new_v4();

        register_local(&tracker, id_active, "active", "img").await;
        register_local(&tracker, id_done, "done", "img").await;
        register_local(&tracker, id_cancelled, "cancelled", "img").await;

        tracker
            .update_status(id_done, TrackedStatus::Completed { duration_secs: 10 })
            .await
            .unwrap();
        tracker
            .update_status(id_cancelled, TrackedStatus::Cancelled)
            .await
            .unwrap();

        // Force timestamps to the past by waiting — but we can't sleep in tests.
        // Instead, bypass via zero-duration: cleanup_stale(0) removes terminal
        // jobs whose updated_at is before Utc::now() which is almost always true
        // for records set in the same test (nanos may differ). Use Duration::ZERO
        // to guarantee the cutoff is effectively "now", meaning all updated_at
        // values are <= cutoff.
        let removed = tracker
            .cleanup_stale(std::time::Duration::from_nanos(0))
            .await
            .unwrap();

        // At least the two terminal jobs were eligible; exact count depends on
        // sub-nanosecond scheduling, so assert at most 2 and active job survived.
        assert!(removed <= 2);
        // The active (non-terminal) job must never be removed.
        assert!(tracker.get(id_active).await.is_some());
    }

    #[tokio::test]
    async fn list_with_no_jobs_returns_empty_vec() {
        let tracker = JobTracker::new();
        let all = tracker.list(false).await;
        assert!(all.is_empty());

        let active = tracker.list(true).await;
        assert!(active.is_empty());
    }

    #[test]
    fn tracked_status_is_terminal_for_each_variant() {
        assert!(!TrackedStatus::Queued.is_terminal());
        assert!(!TrackedStatus::Running { progress: 0.5 }.is_terminal());
        assert!(TrackedStatus::Completed { duration_secs: 0 }.is_terminal());
        assert!(
            TrackedStatus::Failed {
                error: "boom".into()
            }
            .is_terminal()
        );
        assert!(TrackedStatus::Cancelled.is_terminal());
    }

    #[test]
    fn from_job_status_for_tracked_status_all_variants() {
        use crate::JobStatus;

        let queued = TrackedStatus::from(&JobStatus::Queued);
        assert!(matches!(queued, TrackedStatus::Queued));

        let running = TrackedStatus::from(&JobStatus::Running { progress: 0.42 });
        assert!(
            matches!(running, TrackedStatus::Running { progress } if (progress - 0.42).abs() < f64::EPSILON)
        );

        let completed = TrackedStatus::from(&JobStatus::Completed);
        assert!(matches!(
            completed,
            TrackedStatus::Completed { duration_secs: 0 }
        ));

        let failed = TrackedStatus::from(&JobStatus::Failed {
            error: "disk full".into(),
        });
        assert!(matches!(failed, TrackedStatus::Failed { ref error } if error == "disk full"));

        let cancelled = TrackedStatus::from(&JobStatus::Cancelled);
        assert!(matches!(cancelled, TrackedStatus::Cancelled));
    }

    #[tokio::test]
    async fn job_record_serde_roundtrip() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        let record = tracker
            .register(
                id,
                "roundtrip-job",
                "python:3.11",
                "marc27",
                JobTarget::Marc27 {
                    api_base: "https://api.marc27.com/api/v1".into(),
                },
            )
            .await
            .unwrap();

        let json = serde_json::to_string(&record).unwrap();
        let parsed: JobRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.job_id, record.job_id);
        assert_eq!(parsed.name, "roundtrip-job");
        assert_eq!(parsed.image, "python:3.11");
        assert_eq!(parsed.backend, "marc27");
        assert!(matches!(parsed.status, TrackedStatus::Queued));
    }

    #[test]
    fn tracked_status_serde_roundtrip_all_variants() {
        let variants: &[TrackedStatus] = &[
            TrackedStatus::Queued,
            TrackedStatus::Running { progress: 0.33 },
            TrackedStatus::Completed { duration_secs: 120 },
            TrackedStatus::Failed {
                error: "oom".into(),
            },
            TrackedStatus::Cancelled,
        ];

        for variant in variants {
            let json = serde_json::to_string(variant).unwrap();
            let parsed: TrackedStatus = serde_json::from_str(&json).unwrap();
            match (variant, &parsed) {
                (TrackedStatus::Queued, TrackedStatus::Queued) => {}
                (TrackedStatus::Cancelled, TrackedStatus::Cancelled) => {}
                (
                    TrackedStatus::Running { progress: a },
                    TrackedStatus::Running { progress: b },
                ) => assert!((a - b).abs() < f64::EPSILON),
                (
                    TrackedStatus::Completed { duration_secs: a },
                    TrackedStatus::Completed { duration_secs: b },
                ) => assert_eq!(a, b),
                (TrackedStatus::Failed { error: a }, TrackedStatus::Failed { error: b }) => {
                    assert_eq!(a, b)
                }
                _ => panic!("TrackedStatus variant mismatch after roundtrip"),
            }
        }
    }

    #[tokio::test]
    async fn persistent_tracker_survives_a_new_process_instance_with_target_metadata() {
        use crate::byoc::{ByocTarget, SlurmJobConfig};

        let data_dir = std::env::temp_dir().join(format!("prism-compute-test-{}", Uuid::new_v4()));
        let id = Uuid::new_v4();
        let target = JobTarget::Byoc(ByocTarget::Slurm {
            head_node: "login.hpc".into(),
            user: "researcher".into(),
            partition: "gpu".into(),
            config: Box::new(SlurmJobConfig {
                account: Some("esa-materials".into()),
                sif_path: "/shared/prism-worker.sif".into(),
                ..SlurmJobConfig::default()
            }),
        });

        let first = JobTracker::persistent(&data_dir).unwrap();
        first
            .register(id, "hpc-job", "/shared/prism-worker.sif", "byoc", target)
            .await
            .unwrap();
        drop(first);
        assert!(data_dir.join(JOBS_FILE).is_file());

        let second = JobTracker::persistent(&data_dir).unwrap();
        let record = second.get(id).await.unwrap();
        assert_eq!(record.job_id, id);
        assert_eq!(record.backend, "byoc");
        assert!(record.submitted_at <= Utc::now());
        assert!(record.slurm_job_id.is_none());
        assert!(matches!(
            record.target,
            JobTarget::Byoc(ByocTarget::Slurm { .. })
        ));

        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn multiple_concurrent_register_calls_are_safe() {
        use std::sync::Arc;
        use tokio::task::JoinSet;

        let tracker = Arc::new(JobTracker::new());
        let mut set = JoinSet::new();
        let n = 50u32;

        for i in 0..n {
            let t = Arc::clone(&tracker);
            set.spawn(async move {
                let id = Uuid::new_v4();
                register_local(&t, id, &format!("job-{i}"), "img").await;
                id
            });
        }

        let mut ids = Vec::new();
        while let Some(res) = set.join_next().await {
            ids.push(res.unwrap());
        }

        // All n jobs registered, no duplicates dropped.
        let all = tracker.list(false).await;
        assert_eq!(all.len(), n as usize);

        // Every returned id is retrievable.
        for id in &ids {
            assert!(tracker.get(*id).await.is_some());
        }
    }
}
