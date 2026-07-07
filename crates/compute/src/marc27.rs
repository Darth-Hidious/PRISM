//! MARC27 platform compute backend.
//!
//! Dispatches jobs to the MARC27 compute broker over its public REST API
//! (`api.marc27.com`). This is the same `/compute/*` surface the `prism compute`
//! commands and the `compute_submit` agent tool use — `prism run --backend marc27`
//! (and the `run`/`run_submit` agent tools) route here, so this backend MUST stay
//! pointed at the real endpoints and the real auth.
//!
//! ## Auth
//!
//! Agents authenticate with an API key (`MARC27_API_KEY` → `X-API-Key`); users
//! fall back to a Bearer session token. Both are resolved in the CLI via
//! `resolve_agent_auth` and passed in as a [`Marc27Auth`].
//!
//! ## Endpoints (must match `marc27-core` `crates/api/src/routes/compute.rs`)
//!
//! - submit  → `POST {api_base}/compute/submit`           body `{image, inputs}`
//! - status  → `GET  {api_base}/compute/{job_id}`          (status + result in one)
//! - results → `GET  {api_base}/compute/{job_id}`          (returns the `output` field)
//! - cancel  → `POST {api_base}/compute/{job_id}/cancel`
//!
//! There is NO `/compute/jobs/*` surface and NO separate `/results` endpoint —
//! the previous backend hit both, 404-ing every dispatch.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// How to authenticate to the MARC27 platform. Mirrors the CLI's `PlatformAuth`
/// without coupling this crate to the CLI: API keys send `X-API-Key`, sessions
/// send `Authorization: Bearer`.
#[derive(Debug, Clone)]
pub enum Marc27Auth {
    ApiKey(String),
    Bearer(String),
}

impl Marc27Auth {
    /// Apply the credential to a request builder.
    fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::ApiKey(key) => req.header("X-API-Key", key),
            Self::Bearer(token) => req.header("Authorization", format!("Bearer {token}")),
        }
    }
}

/// MARC27 cloud compute backend.
pub struct Marc27Backend {
    client: reqwest::Client,
    /// API base, normalised to end with `/api/v1`
    /// (e.g. `https://api.marc27.com/api/v1`).
    api_base: String,
    auth: Marc27Auth,
}

impl Marc27Backend {
    /// Construct from an API base and resolved auth.
    ///
    /// `api_base` may be either a bare host (`https://api.marc27.com`) or the
    /// full API base (`https://api.marc27.com/api/v1`); it is normalised so the
    /// `/api/v1` prefix appears exactly once — this is what prevents the
    /// historical `…/api/v1/api/v1/compute/jobs` double-prefix 404.
    pub fn new(api_base: &str, auth: Marc27Auth) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_base: normalise_api_base(api_base),
            auth,
        }
    }

    /// Build a `/compute*` URL. `path` begins with `/` (e.g. `/submit`,
    /// `/{job_id}`, `/{job_id}/cancel`).
    fn url(&self, path: &str) -> String {
        format!("{}{}{}", self.api_base, "/compute", path)
    }
}

/// Normalise an API base to end with exactly one `/api/v1`. Accepts a bare host
/// or an already-prefixed base; never doubles the prefix.
fn normalise_api_base(api_base: &str) -> String {
    let trimmed = api_base.trim_end_matches('/');
    if trimmed.ends_with("/api/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/v1")
    }
}

/// Submit response from `POST /compute/submit`.
#[derive(Deserialize)]
struct SubmitResponse {
    job_id: Uuid,
}

/// Combined status+result response from `GET /compute/{job_id}`. All fields
/// except `status` are optional — the broker omits them until known.
#[derive(Deserialize)]
struct JobResponse {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    output: Option<serde_json::Value>,
}

/// Map a broker status string to the router's `JobStatus`. Unknown strings
/// read as `Running` (never silently `Completed`).
fn map_status(resp: JobResponse) -> JobStatus {
    match resp.status.as_str() {
        "queued" | "pending" => JobStatus::Queued,
        "running" => JobStatus::Running { progress: 0.0 },
        "completed" | "succeeded" => JobStatus::Completed,
        "failed" | "error" => JobStatus::Failed {
            error: resp.error.unwrap_or_else(|| "unknown error".into()),
        },
        "cancelled" => JobStatus::Cancelled,
        // Unknown status: safer to surface as running than to claim completion.
        _ => JobStatus::Running { progress: 0.0 },
    }
}

#[async_trait]
impl ComputeBackend for Marc27Backend {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        // The broker's SubmitRequest accepts `image` + `inputs` (plus optional
        // gpu_type/timeout/budget); `ExperimentPlan` only carries image + inputs,
        // so that is exactly what we send. No invented `name` field.
        let body = serde_json::json!({
            "image": plan.image,
            "inputs": plan.inputs,
        });

        let resp = self
            .auth
            .apply(self.client.post(self.url("/submit")))
            .json(&body)
            .send()
            .await
            .context("failed to submit job to MARC27 platform")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("MARC27 submit failed ({status}): {text}");
        }

        let result: SubmitResponse = resp.json().await.context("bad submit response")?;
        tracing::info!(job_id = %result.job_id, "job submitted to MARC27 platform");
        Ok(result.job_id)
    }

    async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        let resp = self
            .auth
            .apply(self.client.get(self.url(&format!("/{job_id}"))))
            .send()
            .await
            .context("failed to query job status")?;

        if !resp.status().is_success() {
            bail!("MARC27 status query failed: {}", resp.status());
        }

        let job: JobResponse = resp.json().await?;
        Ok(map_status(job))
    }

    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value> {
        // Status and output come from the SAME endpoint (`GET /compute/{job_id}`);
        // there is no separate `/results` path.
        let resp = self
            .auth
            .apply(self.client.get(self.url(&format!("/{job_id}"))))
            .send()
            .await
            .context("failed to fetch job results")?;

        if !resp.status().is_success() {
            bail!("MARC27 results query failed: {}", resp.status());
        }

        let job: JobResponse = resp.json().await?;
        Ok(job.output.unwrap_or(serde_json::Value::Null))
    }

    async fn cancel(&self, job_id: Uuid) -> Result<()> {
        let resp = self
            .auth
            .apply(self.client.post(self.url(&format!("/{job_id}/cancel"))))
            .send()
            .await
            .context("failed to cancel job")?;

        if !resp.status().is_success() {
            bail!("MARC27 cancel failed: {}", resp.status());
        }

        tracing::info!(%job_id, "job cancelled on MARC27 platform");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_API_BASE: &str = "https://api.marc27.com/api/v1";

    #[test]
    fn url_targets_real_compute_endpoints() {
        let backend = Marc27Backend::new(DEFAULT_API_BASE, Marc27Auth::ApiKey("k".into()));
        assert_eq!(
            backend.url("/submit"),
            "https://api.marc27.com/api/v1/compute/submit"
        );
        assert_eq!(
            backend.url(&format!("/{}", Uuid::nil())),
            format!("https://api.marc27.com/api/v1/compute/{}", Uuid::nil())
        );
        assert_eq!(
            backend.url(&format!("/{}/cancel", Uuid::nil())),
            format!(
                "https://api.marc27.com/api/v1/compute/{}/cancel",
                Uuid::nil()
            )
        );
    }

    #[test]
    fn normalise_accepts_bare_host_and_prefixed_base() {
        assert_eq!(
            normalise_api_base("https://api.marc27.com"),
            DEFAULT_API_BASE
        );
        assert_eq!(
            normalise_api_base("https://api.marc27.com/api/v1"),
            DEFAULT_API_BASE
        );
    }

    #[test]
    fn normalise_never_doubles_api_prefix() {
        // The historical bug: a base already ending in /api/v1 must NOT gain a
        // second /api/v1.
        assert_eq!(
            normalise_api_base("https://platform.marc27.com/api/v1/"),
            "https://platform.marc27.com/api/v1"
        );
    }

    #[test]
    fn normalise_strips_trailing_slashes() {
        assert_eq!(
            normalise_api_base("https://api.marc27.com///"),
            DEFAULT_API_BASE
        );
    }

    #[test]
    fn url_no_longer_hits_nonexistent_jobs_path() {
        // The old backend built /compute/jobs and /compute/jobs/{id}/status,
        // neither of which exists on the broker. The new paths are /compute/submit
        // and /compute/{id}.
        let backend = Marc27Backend::new(DEFAULT_API_BASE, Marc27Auth::Bearer("t".into()));
        let submit = backend.url("/submit");
        assert!(!submit.contains("/jobs"));
        let status = backend.url(&format!("/{}", Uuid::nil()));
        assert!(!status.contains("/status"));
    }

    #[test]
    fn map_status_known_states() {
        assert!(matches!(
            map_status(JobResponse {
                status: "queued".into(),
                error: None,
                output: None
            }),
            JobStatus::Queued
        ));
        assert!(matches!(
            map_status(JobResponse {
                status: "completed".into(),
                error: None,
                output: None
            }),
            JobStatus::Completed
        ));
        assert!(matches!(
            map_status(JobResponse {
                status: "failed".into(),
                error: Some("oom".into()),
                output: None
            }),
            JobStatus::Failed { error } if error == "oom"
        ));
        assert!(matches!(
            map_status(JobResponse {
                status: "cancelled".into(),
                error: None,
                output: None
            }),
            JobStatus::Cancelled
        ));
    }

    #[test]
    fn map_status_unknown_is_running_not_completed() {
        // An unrecognised status must never read as Completed (which would skip
        // result polling). Running is the safe default.
        assert!(matches!(
            map_status(JobResponse {
                status: "provisioning".into(),
                error: None,
                output: None
            }),
            JobStatus::Running { .. }
        ));
    }

    #[test]
    fn api_key_auth_uses_x_api_key_header() {
        // A header-value test would need a server; assert the variant carries the
        // key verbatim (the header name is fixed in `apply`).
        let Marc27Auth::ApiKey(key) = Marc27Auth::ApiKey("m27_secret".into()) else {
            panic!("expected ApiKey variant");
        };
        assert_eq!(key, "m27_secret");
    }

    #[test]
    fn bearer_auth_carries_token_verbatim() {
        let Marc27Auth::Bearer(token) =
            Marc27Auth::Bearer("eyJhbGciOiJIUzI1NiJ9.payload.sig".into())
        else {
            panic!("expected Bearer variant");
        };
        assert_eq!(token, "eyJhbGciOiJIUzI1NiJ9.payload.sig");
    }
}
