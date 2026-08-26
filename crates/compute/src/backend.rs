//! Backend selection and routing logic.
//!
//! Routes experiment plans to the appropriate compute backend based on
//! configuration, resource requirements, and availability.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::byoc::{ByocBackend, ByocTarget};
use crate::hyperqueue::{HyperQueueBackend, HyperQueueConfig, plan_is_task_set};
use crate::job::{JobTarget, JobTracker};
use crate::local::LocalBackend;
use crate::marc27::{Marc27Auth, Marc27Backend};
use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// Which backend to use for a given job.
#[derive(Debug, Clone)]
pub enum BackendKind {
    Local,
    Marc27 { api_base: String, auth: Marc27Auth },
    Byoc(ByocTarget),
    HyperQueue { server_dir: PathBuf },
}

/// Compute router — selects and dispatches to the right backend.
pub struct ComputeRouter {
    local: LocalBackend,
    marc27: Option<Marc27Backend>,
    byoc: Option<ByocBackend>,
    hyperqueue: Option<HyperQueueBackend>,
    tracker: JobTracker,
    default_backend: BackendKind,
}

impl ComputeRouter {
    /// Create a router with only the local backend.
    pub fn local_only() -> Self {
        Self::local_with_tracker(JobTracker::new())
    }

    /// Create a local router backed by the persistent job tracker.
    pub fn local_only_persistent(data_dir: &Path) -> Result<Self> {
        Ok(Self::local_with_tracker(JobTracker::persistent(data_dir)?))
    }

    fn local_with_tracker(tracker: JobTracker) -> Self {
        Self {
            local: LocalBackend::new(),
            marc27: None,
            byoc: None,
            hyperqueue: None,
            tracker,
            default_backend: BackendKind::Local,
        }
    }

    /// Create a router with local + MARC27 platform backends.
    ///
    /// `api_base` is normalised by [`Marc27Backend`] so the `/api/v1` prefix
    /// appears exactly once (a bare host or a prefixed base both work).
    pub fn with_marc27(api_base: &str, auth: Marc27Auth) -> Self {
        Self::marc27_with_tracker(api_base, auth, JobTracker::new())
    }

    /// Create a MARC27 router backed by the persistent job tracker.
    pub fn with_marc27_persistent(
        api_base: &str,
        auth: Marc27Auth,
        data_dir: &Path,
    ) -> Result<Self> {
        Ok(Self::marc27_with_tracker(
            api_base,
            auth,
            JobTracker::persistent(data_dir)?,
        ))
    }

    fn marc27_with_tracker(api_base: &str, auth: Marc27Auth, tracker: JobTracker) -> Self {
        Self {
            local: LocalBackend::new(),
            marc27: Some(Marc27Backend::new(api_base, auth.clone())),
            byoc: None,
            hyperqueue: None,
            tracker,
            default_backend: BackendKind::Marc27 {
                api_base: api_base.to_string(),
                auth,
            },
        }
    }

    /// Add a BYOC backend and make it the default.
    pub fn with_byoc(mut self, target: ByocTarget) -> Self {
        self.default_backend = BackendKind::Byoc(target.clone());
        self.byoc = Some(ByocBackend::new(target));
        self
    }

    /// Add a HyperQueue backend and make it the default.
    ///
    /// HyperQueue is the many-task path: independent task sets submitted as
    /// one HQ job. Even when it is NOT the default, the router sends
    /// task-set-shaped plans (`inputs.tasks` / `inputs.command`) here, and
    /// everything else — a single long job that wants checkpoint/requeue —
    /// stays on the default backend.
    pub fn with_hyperqueue(mut self, config: HyperQueueConfig) -> Self {
        self.default_backend = BackendKind::HyperQueue {
            server_dir: config.server_dir.clone(),
        };
        self.hyperqueue = Some(HyperQueueBackend::new(config));
        self
    }

    /// Get the job tracker for status queries.
    pub fn tracker(&self) -> &JobTracker {
        &self.tracker
    }

    /// Resolve which backend to use for a plan.
    fn resolve_backend(&self, plan: &ExperimentPlan) -> &dyn ComputeBackend {
        // Simple heuristic: if image contains "marc27", route to platform.
        // Otherwise use default.
        if (plan.image.contains("marc27") || plan.image.contains("platform"))
            && let Some(ref m) = self.marc27
        {
            return m;
        }

        // Many-task plans go to HyperQueue when one is configured: N
        // independent tasks are one HQ job, where byoc would be N sbatch
        // submissions. Single plans keep the caller's chosen default —
        // notably byoc's checkpoint/requeue path, which HQ does not replace.
        if plan_is_task_set(plan)
            && let Some(ref hq) = self.hyperqueue
        {
            return hq;
        }

        match &self.default_backend {
            BackendKind::Local => &self.local,
            BackendKind::Marc27 { .. } => self
                .marc27
                .as_ref()
                .map(|m| m as &dyn ComputeBackend)
                .unwrap_or(&self.local),
            BackendKind::Byoc(_) => self
                .byoc
                .as_ref()
                .map(|b| b as &dyn ComputeBackend)
                .unwrap_or(&self.local),
            BackendKind::HyperQueue { .. } => self
                .hyperqueue
                .as_ref()
                .map(|h| h as &dyn ComputeBackend)
                .unwrap_or(&self.local),
        }
    }

    fn backend_name(&self, plan: &ExperimentPlan) -> &str {
        if (plan.image.contains("marc27") || plan.image.contains("platform"))
            && self.marc27.is_some()
        {
            return "marc27";
        }
        if plan_is_task_set(plan) && self.hyperqueue.is_some() {
            return "hyperqueue";
        }
        match &self.default_backend {
            BackendKind::Local => "local",
            BackendKind::Marc27 { .. } => {
                if self.marc27.is_some() {
                    "marc27"
                } else {
                    "local"
                }
            }
            BackendKind::Byoc(_) => {
                if self.byoc.is_some() {
                    "byoc"
                } else {
                    "local"
                }
            }
            BackendKind::HyperQueue { .. } => {
                if self.hyperqueue.is_some() {
                    "hyperqueue"
                } else {
                    "local"
                }
            }
        }
    }

    fn job_target(&self, plan: &ExperimentPlan) -> JobTarget {
        if (plan.image.contains("marc27") || plan.image.contains("platform"))
            && let BackendKind::Marc27 { api_base, .. } = &self.default_backend
        {
            return JobTarget::Marc27 {
                api_base: api_base.clone(),
            };
        }
        if plan_is_task_set(plan)
            && let BackendKind::HyperQueue { server_dir } = &self.default_backend
        {
            return JobTarget::HyperQueue {
                server_dir: server_dir.clone(),
            };
        }
        match &self.default_backend {
            BackendKind::Local => JobTarget::Local,
            BackendKind::Marc27 { api_base, .. } => JobTarget::Marc27 {
                api_base: api_base.clone(),
            },
            BackendKind::Byoc(target) => JobTarget::Byoc(target.clone()),
            BackendKind::HyperQueue { server_dir } => JobTarget::HyperQueue {
                server_dir: server_dir.clone(),
            },
        }
    }

    /// Submit a job through the router.
    pub async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        let backend = self.resolve_backend(plan);
        let backend_name = self.backend_name(plan);

        let job_id = backend.submit(plan).await?;
        let slurm_job_id = if backend_name == "byoc" {
            match &self.byoc {
                Some(byoc) => byoc.slurm_job_id(job_id).await,
                None => None,
            }
        } else {
            None
        };
        let hyperqueue_job_id = if backend_name == "hyperqueue" {
            match &self.hyperqueue {
                Some(hq) => hq.hq_job_id(job_id).await,
                None => None,
            }
        } else {
            None
        };

        let registration = match hyperqueue_job_id {
            Some(hq_id) => {
                self.tracker
                    .register_with_hyperqueue_job_id(
                        job_id,
                        &plan.name,
                        &plan.image,
                        backend_name,
                        self.job_target(plan),
                        Some(hq_id),
                    )
                    .await
            }
            None => {
                self.tracker
                    .register_with_slurm_job_id(
                        job_id,
                        &plan.name,
                        &plan.image,
                        backend_name,
                        self.job_target(plan),
                        slurm_job_id,
                    )
                    .await
            }
        };
        registration.with_context(|| {
            let scheduler = slurm_job_id
                .map(|id| format!(" (SLURM scheduler id {id})"))
                .or_else(|| hyperqueue_job_id.map(|id| format!(" (HyperQueue job id {id})")))
                .unwrap_or_default();
            format!(
                "job {job_id}{scheduler} was submitted via {backend_name} but its tracking record could not be persisted"
            )
        })?;

        tracing::info!(%job_id, backend = backend_name, "job routed");
        Ok(job_id)
    }

    /// Query job status.
    pub async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        // Check tracker first for backend routing.
        if let Some(record) = self.tracker.get(job_id).await {
            let backend: &dyn ComputeBackend = match record.backend.as_str() {
                "marc27" => self
                    .marc27
                    .as_ref()
                    .map(|m| m as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                "byoc" => self
                    .byoc
                    .as_ref()
                    .map(|b| b as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                "hyperqueue" => self
                    .hyperqueue
                    .as_ref()
                    .map(|h| h as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                _ => &self.local,
            };
            return backend.status(job_id).await;
        }

        // Fallback: try local.
        self.local.status(job_id).await
    }

    /// Fetch job results.
    pub async fn results(&self, job_id: Uuid) -> Result<serde_json::Value> {
        if let Some(record) = self.tracker.get(job_id).await {
            let backend: &dyn ComputeBackend = match record.backend.as_str() {
                "marc27" => self
                    .marc27
                    .as_ref()
                    .map(|m| m as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                "byoc" => self
                    .byoc
                    .as_ref()
                    .map(|b| b as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                "hyperqueue" => self
                    .hyperqueue
                    .as_ref()
                    .map(|h| h as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                _ => &self.local,
            };
            return backend.results(job_id).await;
        }
        self.local.results(job_id).await
    }

    /// Cancel a job.
    pub async fn cancel(&self, job_id: Uuid) -> Result<()> {
        if let Some(record) = self.tracker.get(job_id).await {
            let backend: &dyn ComputeBackend = match record.backend.as_str() {
                "marc27" => self
                    .marc27
                    .as_ref()
                    .map(|m| m as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                "byoc" => self
                    .byoc
                    .as_ref()
                    .map(|b| b as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                "hyperqueue" => self
                    .hyperqueue
                    .as_ref()
                    .map(|h| h as &dyn ComputeBackend)
                    .unwrap_or(&self.local),
                _ => &self.local,
            };
            backend.cancel(job_id).await?;

            use crate::job::TrackedStatus;
            self.tracker
                .update_status(job_id, TrackedStatus::Cancelled)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_only_router() {
        let router = ComputeRouter::local_only();
        let plan = ExperimentPlan {
            name: "test".into(),
            image: "python:3.11".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        assert_eq!(router.backend_name(&plan), "local");
    }

    #[test]
    fn marc27_image_routes_to_platform() {
        let router = ComputeRouter::with_marc27(
            "https://api.marc27.com/api/v1",
            Marc27Auth::Bearer("tok".into()),
        );
        let plan = ExperimentPlan {
            name: "test".into(),
            image: "marc27/calphad-runner:latest".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        assert_eq!(router.backend_name(&plan), "marc27");
    }

    // --- Edge-case tests ---

    #[test]
    fn backend_name_local_only_default_is_local() {
        // local_only() has no marc27 backend — any image routes to "local".
        let router = ComputeRouter::local_only();
        let plan = ExperimentPlan {
            name: "t".into(),
            image: "alpine:latest".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        assert_eq!(router.backend_name(&plan), "local");
    }

    #[test]
    fn backend_name_platform_image_without_marc27_falls_back_to_local() {
        // local_only() has no marc27 backend. Even a "platform" image falls
        // back to local because marc27 is None.
        let router = ComputeRouter::local_only();
        let plan = ExperimentPlan {
            name: "t".into(),
            image: "platform-runner:latest".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        // "platform" in image name triggers the heuristic, but since marc27 is
        // None the router returns the default backend which is "local".
        assert_eq!(router.backend_name(&plan), "local");
    }

    #[test]
    fn backend_name_platform_in_image_routes_to_marc27_when_backend_present() {
        let router = ComputeRouter::with_marc27(
            "https://api.marc27.com/api/v1",
            Marc27Auth::Bearer("tok".into()),
        );
        let plan = ExperimentPlan {
            name: "t".into(),
            image: "platform/experiment:v1".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        assert_eq!(router.backend_name(&plan), "marc27");
    }

    #[test]
    fn compute_router_local_only_tracker_is_accessible() {
        let router = ComputeRouter::local_only();
        // tracker() should return a reference without panicking.
        let tracker = router.tracker();
        // The tracker is initially empty — we can verify this by checking the
        // pointer is non-null by using it (it's a reference, always valid).
        // A trivial round-trip: the tracker must exist and be the same instance.
        let _ = tracker;
    }

    // --- HyperQueue routing tests ---

    fn hq_router() -> ComputeRouter {
        ComputeRouter::local_only()
            .with_hyperqueue(HyperQueueConfig::standalone("/tmp/prism-hq-router-test", 1))
    }

    fn task_set_plan() -> ExperimentPlan {
        ExperimentPlan {
            name: "corpus-ingest".into(),
            image: "ignored".into(),
            inputs: serde_json::json!({"tasks": [{"command": ["true"]}]}),
            resources: Default::default(),
        }
    }

    #[test]
    fn task_set_routes_to_hyperqueue_when_configured() {
        assert_eq!(hq_router().backend_name(&task_set_plan()), "hyperqueue");
    }

    #[test]
    fn task_set_stays_on_default_without_hyperqueue() {
        let router = ComputeRouter::local_only();
        assert_eq!(router.backend_name(&task_set_plan()), "local");
    }

    #[test]
    fn hyperqueue_default_receives_non_task_set_plans_too() {
        // When the caller chose HQ as the DEFAULT, even a plan without the
        // task-set shape routes there and fails honestly at submit time.
        // Silent fallback to local would hide the misconfiguration.
        let plan = ExperimentPlan {
            name: "long-job".into(),
            image: "worker.sif".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        assert_eq!(hq_router().backend_name(&plan), "hyperqueue");
    }

    #[test]
    fn task_set_overrides_a_byoc_default() {
        let router = ComputeRouter::local_only()
            .with_hyperqueue(HyperQueueConfig::standalone("/tmp/prism-hq-router-test", 1))
            .with_byoc(ByocTarget::default());
        // Default is byoc now, but the many-task shape still goes to HQ.
        assert_eq!(router.backend_name(&task_set_plan()), "hyperqueue");
        let single = ExperimentPlan {
            name: "long-job".into(),
            image: "worker.sif".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        assert_eq!(router.backend_name(&single), "byoc");
    }

    #[test]
    fn hyperqueue_default_accepts_single_command_plans() {
        let plan = ExperimentPlan {
            name: "one-task".into(),
            image: "ignored".into(),
            inputs: serde_json::json!({"command": ["echo", "hi"]}),
        resources: Default::default(),
        };
        assert_eq!(hq_router().backend_name(&plan), "hyperqueue");
    }

    #[test]
    fn job_target_carries_the_hyperqueue_server_dir() {
        let router = hq_router();
        match router.job_target(&task_set_plan()) {
            JobTarget::HyperQueue { server_dir } => {
                assert_eq!(server_dir, Path::new("/tmp/prism-hq-router-test"))
            }
            other => panic!("expected HyperQueue target, got {other:?}"),
        }
    }
}
