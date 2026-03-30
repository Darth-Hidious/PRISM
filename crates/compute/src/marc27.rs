//! MARC27 platform compute backend.
//!
//! Dispatches jobs to platform.marc27.com cloud infrastructure.
//! Jobs run on managed GPU/CPU clusters with automatic scaling.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ComputeBackend, ExperimentPlan, JobStatus};

/// MARC27 cloud compute backend.
pub struct Marc27Backend {
    client: reqwest::Client,
    base_url: String,
    api_token: String,
}

#[derive(Serialize)]
struct SubmitRequest<'a> {
    name: &'a str,
    image: &'a str,
    inputs: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct SubmitResponse {
    job_id: Uuid,
}

#[derive(Deserialize)]
struct StatusResponse {
    status: String,
    progress: Option<f64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ResultsResponse {
    output: serde_json::Value,
}

impl Marc27Backend {
    pub fn new(base_url: &str, api_token: &str) -> Self {
        let client = reqwest::Client::new();
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_token: api_token.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/compute{}", self.base_url, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_token)
    }
}

#[async_trait]
impl ComputeBackend for Marc27Backend {
    async fn submit(&self, plan: &ExperimentPlan) -> Result<Uuid> {
        let body = SubmitRequest {
            name: &plan.name,
            image: &plan.image,
            inputs: &plan.inputs,
        };

        let resp = self
            .client
            .post(&self.url("/jobs"))
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .context("failed to submit job to MARC27 platform")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("MARC27 submit failed ({}): {}", status, text);
        }

        let result: SubmitResponse = resp.json().await.context("bad submit response")?;
        tracing::info!(job_id = %result.job_id, "job submitted to MARC27 platform");
        Ok(result.job_id)
    }

    async fn status(&self, job_id: Uuid) -> Result<JobStatus> {
        let resp = self
            .client
            .get(&self.url(&format!("/jobs/{job_id}/status")))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("failed to query job status")?;

        if !resp.status().is_success() {
            bail!("MARC27 status query failed: {}", resp.status());
        }

        let status: StatusResponse = resp.json().await?;

        match status.status.as_str() {
            "queued" | "pending" => Ok(JobStatus::Queued),
            "running" => Ok(JobStatus::Running {
                progress: status.progress.unwrap_or(0.0),
            }),
            "completed" | "succeeded" => Ok(JobStatus::Completed),
            "failed" | "error" => Ok(JobStatus::Failed {
                error: status.error.unwrap_or_else(|| "unknown error".into()),
            }),
            "cancelled" => Ok(JobStatus::Cancelled),
            _other => Ok(JobStatus::Running {
                progress: status.progress.unwrap_or(0.0),
            }),
        }
    }

    async fn results(&self, job_id: Uuid) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(&self.url(&format!("/jobs/{job_id}/results")))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .context("failed to fetch job results")?;

        if !resp.status().is_success() {
            bail!("MARC27 results query failed: {}", resp.status());
        }

        let results: ResultsResponse = resp.json().await?;
        Ok(results.output)
    }

    async fn cancel(&self, job_id: Uuid) -> Result<()> {
        let resp = self
            .client
            .post(&self.url(&format!("/jobs/{job_id}/cancel")))
            .header("Authorization", self.auth_header())
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

    #[test]
    fn url_construction() {
        let backend = Marc27Backend::new("https://platform.marc27.com", "tok");
        assert_eq!(
            backend.url("/jobs"),
            "https://platform.marc27.com/api/v1/compute/jobs"
        );
    }

    #[test]
    fn url_strips_trailing_slash() {
        let backend = Marc27Backend::new("https://platform.marc27.com/", "tok");
        assert_eq!(
            backend.url("/jobs"),
            "https://platform.marc27.com/api/v1/compute/jobs"
        );
    }

    // --- Edge-case tests ---

    #[test]
    fn auth_header_format_is_bearer_token() {
        let backend = Marc27Backend::new("https://platform.marc27.com", "my-secret-token");
        assert_eq!(backend.auth_header(), "Bearer my-secret-token");
    }

    #[test]
    fn auth_header_preserves_token_verbatim() {
        // Tokens can contain dots, dashes, underscores, etc.
        let token = "eyJhbGciOiJIUzI1NiJ9.payload.sig";
        let backend = Marc27Backend::new("https://platform.marc27.com", token);
        assert_eq!(backend.auth_header(), format!("Bearer {token}"));
    }

    #[test]
    fn url_with_different_path_suffixes() {
        let backend = Marc27Backend::new("https://platform.marc27.com", "tok");

        assert_eq!(
            backend.url("/jobs/123/status"),
            "https://platform.marc27.com/api/v1/compute/jobs/123/status"
        );
        assert_eq!(
            backend.url("/jobs/123/results"),
            "https://platform.marc27.com/api/v1/compute/jobs/123/results"
        );
        assert_eq!(
            backend.url("/jobs/123/cancel"),
            "https://platform.marc27.com/api/v1/compute/jobs/123/cancel"
        );
        assert_eq!(
            backend.url(""),
            "https://platform.marc27.com/api/v1/compute"
        );
    }

    #[test]
    fn marc27_backend_stores_correct_base_url_and_token() {
        let backend = Marc27Backend::new("https://custom.host.example.com", "tok-abc-123");
        // Verify via public surface (url() and auth_header()).
        assert!(backend.url("/x").starts_with("https://custom.host.example.com"));
        assert!(backend.auth_header().ends_with("tok-abc-123"));
    }

    #[test]
    fn marc27_backend_trims_multiple_trailing_slashes() {
        // Only one trailing slash is stripped by trim_end_matches('/').
        // This test documents the actual behavior: all trailing slashes stripped.
        let backend = Marc27Backend::new("https://platform.marc27.com///", "tok");
        let url = backend.url("/jobs");
        // trim_end_matches strips all occurrences, so the result should be clean.
        assert_eq!(url, "https://platform.marc27.com/api/v1/compute/jobs");
    }
}
