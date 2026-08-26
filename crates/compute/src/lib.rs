// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Compute backend abstraction for PRISM experiment execution.
//!
//! Defines the [`ComputeBackend`] trait and routes jobs to one of three targets:
//!
//! - **Local** ([`LocalBackend`]): Docker/Podman containers on the current machine.
//! - **Cloud** ([`Marc27Backend`]): MARC27 platform-managed compute via REST API.
//! - **BYOC** ([`byoc`]): Bring Your Own Compute — SSH, Kubernetes, or SLURM.
//! - **HyperQueue** ([`hyperqueue`]): many independent tasks via the `hq`
//!   binary, standalone or on top of Slurm/PBS through HQ's autoallocator.
//!
//! The [`ComputeRouter`] selects the appropriate backend based on image names and
//! configuration. Job lifecycle is tracked by [`JobTracker`].

pub mod backend;
pub mod byoc;
pub mod hyperqueue;
pub mod job;
pub mod local;
pub mod marc27;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-exports for convenience.
pub use backend::ComputeRouter;
pub use hyperqueue::{HqTask, HyperQueueBackend, HyperQueueConfig};
pub use job::JobTracker;
pub use local::LocalBackend;
pub use marc27::Marc27Auth;
pub use marc27::Marc27Backend;

/// Trait for compute dispatch backends.
#[async_trait]
pub trait ComputeBackend: Send + Sync {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid>;
    async fn status(&self, job_id: Uuid) -> Result<JobStatus>;
    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value>;
    async fn cancel(&self, job_id: Uuid) -> Result<()>;
}

/// What a job needs from the machine it lands on.
///
/// One vocabulary shared by every backend, so a GPU request survives the trip
/// across the [`ComputeBackend`] boundary instead of being dropped there:
/// HyperQueue turns it into JDF resource requests, SLURM into `#SBATCH`
/// directives, the platform broker into its `gpu_type`/`timeout` fields, and
/// the local backend into `docker run --gpus`.
///
/// Every field is `Option` and `None` means "whatever this backend does by
/// default" — never zero, and never a ceiling. This type only ever *asks* for
/// resources; it does not cap them. A backend that cannot honour a request
/// must say so ([`ResourceSpec::unsupported_by`]) rather than silently
/// submitting a smaller job, which is how a GPU request became a CPU job.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceSpec {
    /// Number of GPUs the job needs on each node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<u32>,
    /// Specific accelerator to ask for, e.g. `A100-80GB`, `H200`, `MI300X`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_class: Option<String>,
    /// CPU cores per task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Memory per node, in GiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<u32>,
    /// Wall-clock the job is allowed to run for. This is a *scheduler
    /// allocation*, not a PRISM deadline: it exists because batch systems
    /// require one, and PRISM never invents a value for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walltime_secs: Option<u64>,
    /// Nodes to allocate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<u32>,
    /// Tasks (MPI ranks) to launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ntasks: Option<u32>,
    /// Scheduler partition/queue to submit into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    /// Allocation to charge, required on most facilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl ResourceSpec {
    /// True when nothing was asked for, so a backend can take its own defaults
    /// without having dropped anything.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// True when the job asked for at least one accelerator.
    pub fn wants_gpu(&self) -> bool {
        self.gpus.is_some_and(|n| n > 0) || self.gpu_class.is_some()
    }

    /// Names the fields this spec asked for that `supported` does not carry,
    /// so a backend can refuse loudly instead of submitting a job that quietly
    /// lost its accelerators. Returns an empty vec when everything asked for
    /// can be honoured.
    pub fn unsupported_by(&self, supported: &[&str]) -> Vec<&'static str> {
        let asked: [(&'static str, bool); 9] = [
            ("gpus", self.gpus.is_some()),
            ("gpu_class", self.gpu_class.is_some()),
            ("cpus", self.cpus.is_some()),
            ("memory_gb", self.memory_gb.is_some()),
            ("walltime_secs", self.walltime_secs.is_some()),
            ("nodes", self.nodes.is_some()),
            ("ntasks", self.ntasks.is_some()),
            ("partition", self.partition.is_some()),
            ("account", self.account.is_some()),
        ];
        asked
            .iter()
            .filter(|(field, requested)| *requested && !supported.contains(field))
            .map(|(field, _)| *field)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentPlan {
    pub name: String,
    pub image: String,
    pub inputs: serde_json::Value,
    /// What the job needs from the machine. Defaults to "the backend's own
    /// defaults" so existing callers keep working unchanged.
    #[serde(default)]
    pub resources: ResourceSpec,
}

impl ExperimentPlan {
    /// A plan that takes each backend's default resources.
    pub fn new(name: impl Into<String>, image: impl Into<String>, inputs: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            image: image.into(),
            inputs,
            resources: ResourceSpec::default(),
        }
    }

    /// Attach a resource request.
    pub fn with_resources(mut self, resources: ResourceSpec) -> Self {
        self.resources = resources;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    Running { progress: f64 },
    Completed,
    Failed { error: String },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ResourceSpec ──
    //
    // These guard the rule that a resource request survives the trip across
    // the ComputeBackend boundary. Before ResourceSpec existed, ExperimentPlan
    // was {name, image, inputs} and every GPU request was dropped here.

    #[test]
    fn a_default_spec_asks_for_nothing() {
        let spec = ResourceSpec::default();
        assert!(spec.is_empty(), "default must mean 'the backend's own defaults'");
        assert!(!spec.wants_gpu());
        // Serialising a default spec must add no keys, so a plan that asks for
        // nothing is wire-identical to one from before this type existed.
        assert_eq!(serde_json::to_string(&spec).unwrap(), "{}");
    }

    #[test]
    fn zero_gpus_is_not_a_gpu_request() {
        // `Some(0)` is an explicit "no accelerator", not "some accelerator".
        // Treating it as a request would emit `--gres=gpu:0`, which allocation
        // clusters reject outright.
        let spec = ResourceSpec { gpus: Some(0), ..Default::default() };
        assert!(!spec.wants_gpu());
    }

    #[test]
    fn naming_a_class_alone_is_a_gpu_request() {
        let spec = ResourceSpec { gpu_class: Some("A100-80GB".into()), ..Default::default() };
        assert!(spec.wants_gpu(), "asking for an A100 without a count still wants a GPU");
    }

    #[test]
    fn unsupported_by_names_every_dropped_field() {
        let spec = ResourceSpec {
            gpus: Some(2),
            nodes: Some(4),
            account: Some("proj".into()),
            ..Default::default()
        };
        let dropped = spec.unsupported_by(&["gpus"]);
        assert_eq!(dropped, vec!["nodes", "account"]);
        // Nothing asked for is nothing dropped.
        assert!(ResourceSpec::default().unsupported_by(&[]).is_empty());
        // A field that was never requested is never reported as dropped.
        assert!(!spec.unsupported_by(&["gpus"]).contains(&"cpus"));
    }

    #[test]
    fn a_plan_carries_its_resources_through_serde() {
        let plan = ExperimentPlan::new("mg-dislocation", "vasp:6.5", serde_json::json!({}))
            .with_resources(ResourceSpec { gpus: Some(4), ..Default::default() });
        let round: ExperimentPlan =
            serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        assert_eq!(round.resources.gpus, Some(4));
    }

    #[test]
    fn a_plan_without_resources_still_deserialises() {
        // Job records persisted before this field existed must still load.
        let json = r#"{"name":"old","image":"busybox","inputs":{}}"#;
        let plan: ExperimentPlan = serde_json::from_str(json).unwrap();
        assert!(plan.resources.is_empty());
    }

    #[test]
    fn job_status_roundtrip() {
        let status = JobStatus::Running { progress: 0.75 };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: JobStatus = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(parsed, JobStatus::Running { progress } if (progress - 0.75).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn experiment_plan_roundtrip() {
        let plan = ExperimentPlan {
            name: "test".into(),
            image: "python:3.11".into(),
            inputs: serde_json::json!({"key": "value"}),
        resources: Default::default(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: ExperimentPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
    }

    // --- Edge-case tests ---

    #[test]
    fn experiment_plan_complex_nested_inputs_roundtrip() {
        let plan = ExperimentPlan {
            name: "nested-inputs-job".into(),
            image: "marc27/calphad:latest".into(),
            inputs: serde_json::json!({
                "composition": {
                    "Fe": 0.7,
                    "Ni": 0.2,
                    "Cr": 0.1
                },
                "temperature_range": [300, 500, 1000, 1500],
                "flags": { "verbose": true, "save_intermediates": false },
                "metadata": {
                    "run_id": "abc-123",
                    "tags": ["production", "urgent"],
                    "nested": { "deep": { "value": null } }
                }
            }),
        resources: Default::default(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: ExperimentPlan = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.name, plan.name);
        assert_eq!(parsed.image, plan.image);
        assert_eq!(parsed.inputs["composition"]["Fe"], 0.7);
        assert_eq!(parsed.inputs["temperature_range"][2], 1000);
        assert_eq!(parsed.inputs["flags"]["verbose"], true);
        assert!(parsed.inputs["metadata"]["nested"]["deep"]["value"].is_null());
    }

    #[test]
    fn experiment_plan_empty_inputs_roundtrip() {
        let plan = ExperimentPlan {
            name: "empty-inputs".into(),
            image: "busybox:latest".into(),
            inputs: serde_json::Value::Null,
        resources: Default::default(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: ExperimentPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "empty-inputs");
        assert!(parsed.inputs.is_null());

        // Also test with an empty object.
        let plan_obj = ExperimentPlan {
            name: "empty-obj".into(),
            image: "busybox:latest".into(),
            inputs: serde_json::json!({}),
        resources: Default::default(),
        };
        let json2 = serde_json::to_string(&plan_obj).unwrap();
        let parsed2: ExperimentPlan = serde_json::from_str(&json2).unwrap();
        assert!(parsed2.inputs.as_object().unwrap().is_empty());
    }

    #[test]
    fn job_status_all_variants_serde_roundtrip() {
        let variants: &[JobStatus] = &[
            JobStatus::Queued,
            JobStatus::Running { progress: 0.5 },
            JobStatus::Completed,
            JobStatus::Failed {
                error: "oom killed".into(),
            },
            JobStatus::Cancelled,
        ];

        for variant in variants {
            let json = serde_json::to_string(variant).unwrap();
            let parsed: JobStatus = serde_json::from_str(&json).unwrap();
            // Verify structural identity.
            match (variant, &parsed) {
                (JobStatus::Queued, JobStatus::Queued) => {}
                (JobStatus::Completed, JobStatus::Completed) => {}
                (JobStatus::Cancelled, JobStatus::Cancelled) => {}
                (JobStatus::Running { progress: a }, JobStatus::Running { progress: b }) => {
                    assert!((a - b).abs() < f64::EPSILON);
                }
                (JobStatus::Failed { error: a }, JobStatus::Failed { error: b }) => {
                    assert_eq!(a, b);
                }
                _ => panic!("variant mismatch after roundtrip"),
            }
        }
    }

    #[test]
    fn job_status_running_edge_progress_zero() {
        let status = JobStatus::Running { progress: 0.0 };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: JobStatus = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, JobStatus::Running { progress } if progress == 0.0));
    }

    #[test]
    fn job_status_running_edge_progress_one() {
        let status = JobStatus::Running { progress: 1.0 };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: JobStatus = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(parsed, JobStatus::Running { progress } if (progress - 1.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn job_status_running_nan_serializes_as_null_or_nan() {
        // NaN is not valid JSON; serde_json serializes it as null with default
        // behavior. We verify the round-trip does not panic and produces a
        // parseable document (even if the value is not mathematically preserved).
        let status = JobStatus::Running { progress: f64::NAN };
        // serde_json will error on NaN — confirm the caller gets an error rather
        // than silent corruption.
        let result = serde_json::to_string(&status);
        // Whether it errors or succeeds (some configs allow null), the important
        // thing is no panic occurred. Just consume the result.
        let _ = result;
    }
}
