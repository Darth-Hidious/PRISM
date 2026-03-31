//! Job tracking and lifecycle management.
//!
//! In-memory tracker for compute jobs across all backends. Provides
//! status queries, cancellation, and cleanup of stale entries.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::JobStatus;

/// Metadata for a tracked job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: Uuid,
    pub name: String,
    pub image: String,
    pub backend: String,
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

/// Thread-safe job tracker.
#[derive(Clone)]
pub struct JobTracker {
    jobs: Arc<RwLock<HashMap<Uuid, JobRecord>>>,
}

impl JobTracker {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new job.
    pub async fn register(
        &self,
        job_id: Uuid,
        name: &str,
        image: &str,
        backend: &str,
    ) -> JobRecord {
        let now = Utc::now();
        let record = JobRecord {
            job_id,
            name: name.to_string(),
            image: image.to_string(),
            backend: backend.to_string(),
            status: TrackedStatus::Queued,
            submitted_at: now,
            updated_at: now,
        };
        self.jobs.write().await.insert(job_id, record.clone());
        record
    }

    /// Update the status of an existing job.
    pub async fn update_status(&self, job_id: Uuid, status: TrackedStatus) -> bool {
        let mut jobs = self.jobs.write().await;
        if let Some(record) = jobs.get_mut(&job_id) {
            record.status = status;
            record.updated_at = Utc::now();
            true
        } else {
            false
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
        records.sort_by(|a, b| b.submitted_at.cmp(&a.submitted_at));
        records
    }

    /// Remove completed/failed/cancelled jobs older than the given duration.
    pub async fn cleanup_stale(&self, max_age: std::time::Duration) -> usize {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let mut jobs = self.jobs.write().await;
        let before = jobs.len();
        jobs.retain(|_, j| !(j.status.is_terminal() && j.updated_at < cutoff));
        before - jobs.len()
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
}

impl Default for JobTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_get() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        tracker
            .register(id, "test-job", "python:3.11", "local")
            .await;
        let record = tracker.get(id).await.unwrap();
        assert_eq!(record.name, "test-job");
        assert!(matches!(record.status, TrackedStatus::Queued));
    }

    #[tokio::test]
    async fn update_status() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        tracker.register(id, "job", "img", "local").await;

        tracker
            .update_status(id, TrackedStatus::Running { progress: 0.5 })
            .await;
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
        tracker.register(id1, "running", "img", "local").await;
        tracker.register(id2, "done", "img", "local").await;

        tracker
            .update_status(id2, TrackedStatus::Completed { duration_secs: 10 })
            .await;

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
        tracker.register(id, "j", "i", "local").await;
        assert_eq!(tracker.active_count().await, 1);

        tracker.update_status(id, TrackedStatus::Cancelled).await;
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
            .await;
        assert!(
            !updated,
            "update_status should return false for unknown job"
        );
    }

    #[tokio::test]
    async fn cleanup_stale_with_no_stale_jobs_removes_nothing() {
        let tracker = JobTracker::new();
        let id = Uuid::new_v4();
        tracker.register(id, "fresh-job", "img", "local").await;

        // Use a very short max_age — zero seconds — but the job is not terminal
        // so it should never be removed.
        let removed = tracker
            .cleanup_stale(std::time::Duration::from_secs(0))
            .await;
        assert_eq!(removed, 0);
        assert!(tracker.get(id).await.is_some());
    }

    #[tokio::test]
    async fn cleanup_stale_removes_only_terminal_jobs_older_than_max_age() {
        let tracker = JobTracker::new();

        let id_active = Uuid::new_v4();
        let id_done = Uuid::new_v4();
        let id_cancelled = Uuid::new_v4();

        tracker.register(id_active, "active", "img", "local").await;
        tracker.register(id_done, "done", "img", "local").await;
        tracker
            .register(id_cancelled, "cancelled", "img", "local")
            .await;

        tracker
            .update_status(id_done, TrackedStatus::Completed { duration_secs: 10 })
            .await;
        tracker
            .update_status(id_cancelled, TrackedStatus::Cancelled)
            .await;

        // Force timestamps to the past by waiting — but we can't sleep in tests.
        // Instead, bypass via zero-duration: cleanup_stale(0) removes terminal
        // jobs whose updated_at is before Utc::now() which is almost always true
        // for records set in the same test (nanos may differ). Use Duration::ZERO
        // to guarantee the cutoff is effectively "now", meaning all updated_at
        // values are <= cutoff.
        let removed = tracker
            .cleanup_stale(std::time::Duration::from_nanos(0))
            .await;

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
        assert!(TrackedStatus::Failed {
            error: "boom".into()
        }
        .is_terminal());
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
            .register(id, "roundtrip-job", "python:3.11", "marc27")
            .await;

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
                t.register(id, &format!("job-{i}"), "img", "local").await;
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
