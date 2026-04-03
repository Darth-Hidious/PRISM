//! Backend selection and routing logic.
//!
//! Routes experiment plans to the appropriate compute backend based on
//! configuration, resource requirements, and availability.

use anyhow::Result;
use uuid::Uuid;

use crate::byoc::{ByocBackend, ByocTarget};
use crate::job::JobTracker;
use crate::local::LocalBackend;
use crate::marc27::Marc27Backend;
use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// Which backend to use for a given job.
#[derive(Debug, Clone)]
pub enum BackendKind {
    Local,
    Marc27 { base_url: String, api_token: String },
    Byoc(ByocTarget),
}

/// Compute router — selects and dispatches to the right backend.
pub struct ComputeRouter {
    local: LocalBackend,
    marc27: Option<Marc27Backend>,
    byoc: Option<ByocBackend>,
    tracker: JobTracker,
    default_backend: BackendKind,
}

impl ComputeRouter {
    /// Create a router with only the local backend.
    pub fn local_only() -> Self {
        Self {
            local: LocalBackend::new(),
            marc27: None,
            byoc: None,
            tracker: JobTracker::new(),
            default_backend: BackendKind::Local,
        }
    }

    /// Create a router with local + MARC27 platform backends.
    pub fn with_marc27(base_url: &str, api_token: &str) -> Self {
        Self {
            local: LocalBackend::new(),
            marc27: Some(Marc27Backend::new(base_url, api_token)),
            byoc: None,
            tracker: JobTracker::new(),
            default_backend: BackendKind::Marc27 {
                base_url: base_url.to_string(),
                api_token: api_token.to_string(),
            },
        }
    }

    /// Add a BYOC backend and make it the default.
    pub fn with_byoc(mut self, target: ByocTarget) -> Self {
        self.default_backend = BackendKind::Byoc(target.clone());
        self.byoc = Some(ByocBackend::new(target));
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
        if plan.image.contains("marc27") || plan.image.contains("platform") {
            if let Some(ref m) = self.marc27 {
                return m;
            }
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
        }
    }

    fn backend_name(&self, plan: &ExperimentPlan) -> &str {
        if (plan.image.contains("marc27") || plan.image.contains("platform"))
            && self.marc27.is_some()
        {
            return "marc27";
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
        }
    }

    /// Submit a job through the router.
    pub async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        let backend = self.resolve_backend(plan);
        let backend_name = self.backend_name(plan);

        let job_id = backend.submit(plan).await?;

        self.tracker
            .register(job_id, &plan.name, &plan.image, backend_name)
            .await;

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
                _ => &self.local,
            };
            backend.cancel(job_id).await?;

            use crate::job::TrackedStatus;
            self.tracker
                .update_status(job_id, TrackedStatus::Cancelled)
                .await;
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
        };
        assert_eq!(router.backend_name(&plan), "local");
    }

    #[test]
    fn marc27_image_routes_to_platform() {
        let router = ComputeRouter::with_marc27("https://platform.marc27.com", "tok");
        let plan = ExperimentPlan {
            name: "test".into(),
            image: "marc27/calphad-runner:latest".into(),
            inputs: serde_json::json!({}),
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
        };
        // "platform" in image name triggers the heuristic, but since marc27 is
        // None the router returns the default backend which is "local".
        assert_eq!(router.backend_name(&plan), "local");
    }

    #[test]
    fn backend_name_platform_in_image_routes_to_marc27_when_backend_present() {
        let router = ComputeRouter::with_marc27("https://platform.marc27.com", "tok");
        let plan = ExperimentPlan {
            name: "t".into(),
            image: "platform/experiment:v1".into(),
            inputs: serde_json::json!({}),
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
}
