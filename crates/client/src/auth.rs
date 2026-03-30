use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

/// Response from the device-code initiation endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// Successful token response (initial or refresh).
#[derive(Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Internal poll response — may carry tokens OR an error string.
#[derive(Debug, Deserialize)]
struct PollPayload {
    access_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

/// Device-code authorisation flow (GitHub CLI-style).
///
/// This is a stateless helper — all state lives in the returned structs.
pub struct DeviceFlowAuth;

impl DeviceFlowAuth {
    /// Start the device authorisation flow.
    ///
    /// Calls `POST {base_url}/auth/device/start` with `client_id=prism-cli`.
    pub async fn start_device_flow(
        client: &reqwest::Client,
        base_url: &str,
    ) -> Result<DeviceCodeResponse> {
        let url = format!("{base_url}/auth/device/start");
        debug!(%url, "starting device flow");

        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "client_id": "prism-cli" }))
            .send()
            .await
            .context("failed to start device flow")?
            .error_for_status()
            .context("device flow start returned error status")?;

        resp.json::<DeviceCodeResponse>()
            .await
            .context("failed to parse device-code response")
    }

    /// Poll the platform until the user approves (or the code expires).
    ///
    /// Calls `POST {base_url}/auth/device/poll` with the device code,
    /// sleeping for `interval` seconds between attempts.
    pub async fn poll_for_token(
        client: &reqwest::Client,
        base_url: &str,
        device_code: &str,
        interval: u64,
    ) -> Result<TokenResponse> {
        let url = format!("{base_url}/auth/device/poll");
        let mut sleep_secs = interval.max(1);

        loop {
            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
            debug!(%url, "polling for token");

            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "device_code": device_code }))
                .send()
                .await
                .context("failed to poll device flow")?;

            let status = resp.status();
            let payload: PollPayload = resp
                .json()
                .await
                .context("failed to parse device poll response")?;

            // Success case: both tokens present, no error
            if payload.error.is_none()
                && payload.access_token.is_some()
                && payload.refresh_token.is_some()
            {
                return Ok(TokenResponse {
                    access_token: payload.access_token.unwrap_or_default(),
                    refresh_token: payload.refresh_token.unwrap_or_default(),
                    token_type: payload.token_type,
                    expires_in: payload.expires_in,
                });
            }

            match payload.error.as_deref() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    sleep_secs += 5;
                    continue;
                }
                Some("access_denied") => bail!("device login denied by user"),
                Some("expired_token") => bail!("device login expired before approval"),
                Some(other) => bail!("device login failed: {other} (http {status})"),
                None => bail!("device login returned unexpected payload"),
            }
        }
    }

    /// Refresh an access token using a refresh token.
    ///
    /// Calls `POST {base_url}/auth/refresh`.
    pub async fn refresh_token(
        client: &reqwest::Client,
        base_url: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse> {
        let url = format!("{base_url}/auth/refresh");
        debug!(%url, "refreshing token");

        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "refresh_token": refresh_token }))
            .send()
            .await
            .context("failed to refresh token")?
            .error_for_status()
            .context("token refresh returned error status")?;

        resp.json::<TokenResponse>()
            .await
            .context("failed to parse refresh response")
    }
}
