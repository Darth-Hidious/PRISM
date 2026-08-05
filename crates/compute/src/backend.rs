//! Backend selection and routing logic.
//!
//! Routes experiment plans to the appropriate compute backend based on
//! configuration, resource requirements, and availability.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::byoc::{ByocBackend, ByocTarget};
use crate::job::{JobTarget, JobTracker};
use crate::licence::{LicenceManager, LicenceRegistry, parse_slurm_walltime};
use crate::local::LocalBackend;
use crate::marc27::{Marc27Auth, Marc27Backend};
use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// Which backend to use for a given job.
#[derive(Debug, Clone)]
pub enum BackendKind {
    Local,
    Marc27 { api_base: String, auth: Marc27Auth },
    Byoc(ByocTarget),
}

/// Compute router — selects and dispatches to the right backend.
pub struct ComputeRouter {
    local: LocalBackend,
    marc27: Option<Marc27Backend>,
    byoc: Option<ByocBackend>,
    tracker: JobTracker,
    default_backend: BackendKind,
    licences: LicenceManager,
    licence_keys_dir: Option<PathBuf>,
}

impl ComputeRouter {
    /// Create a router with only the local backend.
    pub fn local_only() -> Self {
        Self::local_with_tracker(JobTracker::new())
    }

    /// Create a local router backed by the persistent job tracker.
    pub fn local_only_persistent(data_dir: &Path) -> Result<Self> {
        let tracker = JobTracker::persistent(data_dir)?;
        let licences = LicenceManager::new(
            LicenceRegistry::load_default(),
            tracker.clone(),
            Some(licence_keys_dir(data_dir)),
        );
        Ok(Self::local_with_parts(
            tracker,
            licences,
            Some(licence_keys_dir(data_dir)),
        ))
    }

    fn local_with_parts(
        tracker: JobTracker,
        licences: LicenceManager,
        licence_keys_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            local: LocalBackend::new(),
            marc27: None,
            byoc: None,
            tracker,
            default_backend: BackendKind::Local,
            licences,
            licence_keys_dir,
        }
    }

    fn local_with_tracker(tracker: JobTracker) -> Self {
        let licences = LicenceManager::new(LicenceRegistry::load_default(), tracker.clone(), None);
        Self {
            local: LocalBackend::new(),
            marc27: None,
            byoc: None,
            tracker,
            default_backend: BackendKind::Local,
            licences,
            licence_keys_dir: None,
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
        let tracker = JobTracker::persistent(data_dir)?;
        let licences = LicenceManager::new(
            LicenceRegistry::load_default(),
            tracker.clone(),
            Some(licence_keys_dir(data_dir)),
        );
        Ok(Self::marc27_with_parts(
            api_base,
            auth,
            tracker,
            licences,
            Some(licence_keys_dir(data_dir)),
        ))
    }

    fn marc27_with_parts(
        api_base: &str,
        auth: Marc27Auth,
        tracker: JobTracker,
        licences: LicenceManager,
        licence_keys_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            local: LocalBackend::new(),
            marc27: Some(Marc27Backend::new(api_base, auth.clone())),
            byoc: None,
            tracker,
            default_backend: BackendKind::Marc27 {
                api_base: api_base.to_string(),
                auth,
            },
            licences,
            licence_keys_dir,
        }
    }

    fn marc27_with_tracker(api_base: &str, auth: Marc27Auth, tracker: JobTracker) -> Self {
        let licences = LicenceManager::new(LicenceRegistry::load_default(), tracker.clone(), None);
        Self::marc27_with_parts(api_base, auth, tracker, licences, None)
    }

    /// Add a BYOC backend and make it the default.
    pub fn with_byoc(mut self, target: ByocTarget) -> Self {
        self.default_backend = BackendKind::Byoc(target.clone());
        self.byoc = Some(ByocBackend::new(target));
        self
    }

    /// Replace the licence declarations (tests, embedded callers). The
    /// job tracker and key directory are preserved.
    pub fn with_licence_registry(self, registry: LicenceRegistry) -> Self {
        let Self {
            local,
            marc27,
            byoc,
            tracker,
            default_backend,
            licence_keys_dir,
            ..
        } = self;
        let licences = LicenceManager::new(Ok(registry), tracker.clone(), licence_keys_dir.clone());
        Self {
            local,
            marc27,
            byoc,
            tracker,
            default_backend,
            licences,
            licence_keys_dir,
        }
    }

    /// Get the job tracker for status queries.
    pub fn tracker(&self) -> &JobTracker {
        &self.tracker
    }

    /// The licence manager: seat queries, reclamation.
    pub fn licences(&self) -> &LicenceManager {
        &self.licences
    }

    /// Walltime of the resolved target, when one is declared. A licensed
    /// job with an unparseable walltime is refused rather than minted an
    /// unbounded lease.
    fn lease_walltime(&self, plan: &ExperimentPlan) -> Result<Option<Duration>> {
        if self.backend_name(plan) != "byoc" {
            return Ok(None);
        }
        let Some(byoc) = &self.byoc else {
            return Ok(None);
        };
        let ByocTarget::Slurm { config, .. } = byoc.target() else {
            return Ok(None);
        };
        match config.time.as_deref() {
            Some(spec) => Ok(Some(parse_slurm_walltime(spec)?)),
            None => Ok(None),
        }
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

    fn job_target(&self, plan: &ExperimentPlan) -> JobTarget {
        if (plan.image.contains("marc27") || plan.image.contains("platform"))
            && let BackendKind::Marc27 { api_base, .. } = &self.default_backend
        {
            return JobTarget::Marc27 {
                api_base: api_base.clone(),
            };
        }
        match &self.default_backend {
            BackendKind::Local => JobTarget::Local,
            BackendKind::Marc27 { api_base, .. } => JobTarget::Marc27 {
                api_base: api_base.clone(),
            },
            BackendKind::Byoc(target) => JobTarget::Byoc(target.clone()),
        }
    }

    /// Submit a job through the router.
    ///
    /// If the plan requests a licence, the seat is acquired **before**
    /// anything is dispatched; a refusal aborts the submission with an
    /// actionable message and the scheduler never sees the job. On
    /// dispatch failure the hold is released; on success the signed lease
    /// is bound to the tracking record, and the seat then follows the
    /// job's lifecycle (terminal state or lease expiry frees it).
    pub async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        let backend = self.resolve_backend(plan);
        let backend_name = self.backend_name(plan);

        // Stale leases (terminal or expired jobs) stop counting against
        // the seat total; sweep before every dispatch so the accounting
        // we gate on is honest.
        self.licences.reclaim_expired().await?;

        // ── Licence gate: checkout before dispatch ─────────────────────
        let hold = match &plan.licence {
            Some(request) => Some(
                self.licences
                    .checkout(request, self.lease_walltime(plan)?)
                    .await?,
            ),
            None => None,
        };

        // Platforms that assign their own job id (MARC27) get the lease
        // minted after submission; every other backend receives the
        // signed lease up front so it can travel with the job.
        let platform_assigns_ids = backend_name == "marc27";
        let proposed_job_id = Uuid::new_v4();
        let pre_lease = match &hold {
            Some(hold) if !platform_assigns_ids => {
                Some(self.licences.mint(hold, proposed_job_id).await?)
            }
            _ => None,
        };

        let submitted = backend
            .submit(proposed_job_id, plan, pre_lease.as_ref())
            .await;
        let job_id = match submitted {
            Ok(job_id) => job_id,
            Err(error) => {
                if let Some(hold) = &hold {
                    self.licences.drop_hold(hold.lease_id).await;
                }
                return Err(error);
            }
        };

        let lease = match (&hold, pre_lease) {
            (Some(hold), None) => Some(self.licences.mint(hold, job_id).await?),
            (_, lease) => lease,
        };

        let slurm_job_id = if backend_name == "byoc" {
            match &self.byoc {
                Some(byoc) => byoc.slurm_job_id(job_id).await,
                None => None,
            }
        } else {
            None
        };

        let registered = self
            .tracker
            .register_with_slurm_job_id(
                job_id,
                &plan.name,
                &plan.image,
                backend_name,
                self.job_target(plan),
                slurm_job_id,
            )
            .await
            .map(|_| ());
        let accounted = match registered {
            Ok(()) => match &lease {
                Some(lease) => self
                    .tracker
                    .attach_licence(job_id, lease.clone())
                    .await
                    .map(|_| ()),
                None => Ok(()),
            },
            Err(error) => Err(error),
        };
        let accounted = accounted.with_context(|| {
                let scheduler = slurm_job_id
                    .map(|id| format!(" (SLURM scheduler id {id})"))
                    .unwrap_or_default();
                format!(
                    "job {job_id}{scheduler} was submitted via {backend_name} but its tracking record could not be persisted"
                )
            });

        match accounted {
            Ok(()) => {
                // The lease now holds the seat through the job record;
                // the pre-dispatch hold must go so the seat is not
                // double-counted.
                if let Some(hold) = &hold {
                    self.licences.drop_hold(hold.lease_id).await;
                }
            }
            Err(error) => {
                // Submitted but not accounted: release the seat and do
                // not leave an untracked licensed job running.
                if let Some(hold) = &hold {
                    self.licences.drop_hold(hold.lease_id).await;
                }
                let _ = backend.cancel(job_id).await;
                return Err(error);
            }
        }

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
                .await?;
        }
        Ok(())
    }
}

fn licence_keys_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("licences")
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
            licence: None,
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
            licence: None,
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
            licence: None,
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
            licence: None,
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
            licence: None,
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

    // ── Licence gate wiring ──────────────────────────────────────────

    use crate::byoc::{ByocTarget, SlurmCheckpoint, SlurmJobConfig};
    use crate::licence::{LicenceError, LicenceRequest};

    fn slurm_target(time: Option<&str>, sif_path: &str) -> ByocTarget {
        ByocTarget::Slurm {
            head_node: "127.0.0.1".into(),
            user: "prism-test".into(),
            partition: "gpu".into(),
            config: Box::new(SlurmJobConfig {
                time: time.map(str::to_string),
                checkpoint: SlurmCheckpoint::disabled(),
                sif_path: sif_path.into(),
                ..SlurmJobConfig::default()
            }),
        }
    }

    fn licensed_plan(licence_id: &str) -> ExperimentPlan {
        ExperimentPlan {
            name: "licensed-experiment".into(),
            image: "/shared/prism-worker.sif".into(),
            inputs: serde_json::json!({}),
            licence: Some(LicenceRequest {
                id: licence_id.into(),
                seats: 1,
            }),
        }
    }

    const VASP_DECL: &str = r#"
[[licence]]
id = "vasp-6"
name = "VASP 6 (ESA pool)"
seats = 1
expires = "2126-12-31"
"#;

    #[tokio::test]
    async fn licensed_request_is_refused_before_any_backend_dispatch() {
        // One seat, undeclared licence id: the gate must refuse before
        // the backend is ever touched (otherwise the error would be
        // about sbatch/ssh, not licences).
        let router = ComputeRouter::local_only()
            .with_byoc(slurm_target(Some("01:00:00"), "/shared/prism-worker.sif"))
            .with_licence_registry(LicenceRegistry::from_str(VASP_DECL).unwrap());
        let error = router.submit(&licensed_plan("ansys")).await.unwrap_err();
        let licence_error = error
            .downcast_ref::<LicenceError>()
            .expect("gate refusal must be a LicenceError");
        assert!(
            matches!(licence_error, LicenceError::NotDeclared { .. }),
            "{licence_error}"
        );
        assert!(licence_error.to_string().contains("ansys"));
        assert!(router.tracker().list(false).await.is_empty());
    }

    #[tokio::test]
    async fn zero_config_unlicensed_plan_passes_the_gate_untouched() {
        // Empty registry (zero config) and no licence request: the gate
        // is skipped entirely and the plan reaches the backend — here
        // the backend's own validation fails the submission, proving
        // the free path is not gated on licence configuration.
        let router = ComputeRouter::local_only()
            .with_byoc(slurm_target(None, "")) // invalid sif path fails in the backend
            .with_licence_registry(LicenceRegistry::default());
        let plan = ExperimentPlan {
            name: "free-experiment".into(),
            image: "quantum-espresso.sif".into(),
            inputs: serde_json::json!({}),
            licence: None,
        };
        let error = router.submit(&plan).await.unwrap_err();
        assert!(
            error.downcast_ref::<LicenceError>().is_none(),
            "unlicensed zero-config plan must not hit the licence gate: {error}"
        );
        assert!(
            error.to_string().contains("pre-staged .sif"),
            "plan should have reached the backend: {error}"
        );
    }

    #[tokio::test]
    async fn failed_dispatch_releases_the_held_seat() {
        // Declared licence, one seat, unreachable head node (loopback,
        // BatchMode ssh — fails instantly, no external network): the
        // checkout holds the seat, dispatch fails, and the seat must be
        // released again.
        let router = ComputeRouter::local_only()
            .with_byoc(slurm_target(Some("01:00:00"), "/shared/prism-worker.sif"))
            .with_licence_registry(LicenceRegistry::from_str(VASP_DECL).unwrap());

        let error = router.submit(&licensed_plan("vasp-6")).await.unwrap_err();
        assert!(
            error.downcast_ref::<LicenceError>().is_none(),
            "expected a dispatch failure, not a gate refusal: {error}"
        );

        // Seat is back: held count is zero and the single seat can be
        // checked out again.
        assert_eq!(router.licences().held_summary("vasp-6").await.seats_held, 0);
        let hold = router
            .licences()
            .checkout(
                &LicenceRequest {
                    id: "vasp-6".into(),
                    seats: 1,
                },
                None,
            )
            .await
            .unwrap();
        router.licences().drop_hold(hold.lease_id).await;
        assert!(router.tracker().list(false).await.is_empty());
    }

    #[test]
    fn lease_walltime_comes_from_the_slurm_target() {
        let router = ComputeRouter::local_only()
            .with_byoc(slurm_target(Some("02:00:00"), "/shared/prism-worker.sif"))
            .with_licence_registry(LicenceRegistry::default());
        let plan = licensed_plan("vasp-6");
        assert_eq!(
            router.lease_walltime(&plan).unwrap(),
            Some(std::time::Duration::from_secs(2 * 3600))
        );

        let no_time = ComputeRouter::local_only()
            .with_byoc(slurm_target(None, "/shared/prism-worker.sif"))
            .with_licence_registry(LicenceRegistry::default());
        assert_eq!(no_time.lease_walltime(&plan).unwrap(), None);

        let local = ComputeRouter::local_only().with_licence_registry(LicenceRegistry::default());
        assert_eq!(local.lease_walltime(&plan).unwrap(), None);

        let garbage = ComputeRouter::local_only()
            .with_byoc(slurm_target(Some("soon"), "/shared/prism-worker.sif"))
            .with_licence_registry(LicenceRegistry::default());
        assert!(garbage.lease_walltime(&plan).is_err());
    }

    #[tokio::test]
    async fn licensed_dispatch_binds_lease_to_the_tracking_record() {
        // Mirrors the submit path's bind step against the router's own
        // manager and tracker: checkout → mint → register → attach →
        // drop hold. The seat then follows the job's lifecycle.
        let router = ComputeRouter::local_only()
            .with_byoc(slurm_target(Some("01:00:00"), "/shared/prism-worker.sif"))
            .with_licence_registry(LicenceRegistry::from_str(VASP_DECL).unwrap());

        let request = LicenceRequest {
            id: "vasp-6".into(),
            seats: 1,
        };
        let hold = router
            .licences()
            .checkout(
                &request,
                router.lease_walltime(&licensed_plan("vasp-6")).unwrap(),
            )
            .await
            .unwrap();
        let job_id = Uuid::new_v4();
        let lease = router.licences().mint(&hold, job_id).await.unwrap();
        router
            .tracker()
            .register(
                job_id,
                "licensed",
                "/shared/prism-worker.sif",
                "byoc",
                crate::job::JobTarget::Local,
            )
            .await
            .unwrap();
        router
            .tracker()
            .attach_licence(job_id, lease.clone())
            .await
            .unwrap();
        router.licences().drop_hold(hold.lease_id).await;

        // Exactly one seat held, by exactly the bound lease.
        assert_eq!(router.licences().held_summary("vasp-6").await.seats_held, 1);
        let record = router.tracker().get(job_id).await.unwrap();
        assert_eq!(record.licence.as_ref(), Some(&lease));
        // The bound lease is bounded by the walltime (1h), not the
        // 2126 licence expiry.
        assert!(
            lease.expires_at
                <= chrono::Utc::now() + chrono::Duration::hours(1) + chrono::Duration::seconds(5)
        );

        // Terminal state frees the seat — the release path.
        router
            .tracker()
            .update_status(
                job_id,
                crate::job::TrackedStatus::Completed { duration_secs: 42 },
            )
            .await
            .unwrap();
        assert_eq!(router.licences().held_summary("vasp-6").await.seats_held, 0);
    }
}
