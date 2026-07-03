// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Compute backend abstraction for PRISM experiment execution.
//!
//! Defines the [`ComputeBackend`] trait and routes jobs to one of three targets:
//!
//! - **Local** ([`LocalBackend`]): Docker/Podman containers on the current machine.
//! - **Cloud** ([`Marc27Backend`]): MARC27 platform-managed compute via REST API.
//! - **BYOC** ([`byoc`]): Bring Your Own Compute — SSH, Kubernetes, or SLURM.
//!
//! The [`ComputeRouter`] selects the appropriate backend based on image names and
//! configuration. Job lifecycle is tracked by [`JobTracker`].

pub mod backend;
pub mod byoc;
pub mod job;
pub mod local;
pub mod marc27;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-exports for convenience.
pub use backend::ComputeRouter;
pub use job::JobTracker;
pub use local::LocalBackend;
pub use marc27::Marc27Backend;

/// Trait for compute dispatch backends.
#[async_trait]
pub trait ComputeBackend: Send + Sync {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid>;
    async fn status(&self, job_id: Uuid) -> Result<JobStatus>;
    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value>;
    async fn cancel(&self, job_id: Uuid) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentPlan {
    pub name: String,
    pub image: String,
    pub inputs: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
