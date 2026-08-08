use anyhow::{Context, Result, bail};
use prism_runtime::retry;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

use crate::platform_error::{PlatformError, PlatformResponseExt};

/// Response from the device-code initiation endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// Server-provided config (returned on login).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub mp_api_key: Option<String>,
    #[serde(default)]
    pub firecrawl_api_key: Option<String>,
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
    #[serde(default)]
    pub config: Option<ServerConfig>,
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
    #[serde(default)]
    config: Option<ServerConfig>,
}

/// Device-code authorisation flow (GitHub CLI-style).
///
/// This is a stateless helper — all state lives in the returned structs.
pub struct DeviceFlowAuth;

impl DeviceFlowAuth {
    /// Refuse a device-flow call under hard offline mode.
    ///
    /// These three methods take a raw `&reqwest::Client`, normally obtained via
    /// `PlatformClient::inner()` — which hands out the inner client and so
    /// bypasses `PlatformClient::offline_guard` entirely. Every verb on
    /// `PlatformClient` is guarded; the device flow escaped through that one
    /// accessor, so `PRISM_OFFLINE=1 prism setup` still ran a device login and
    /// `prism resume` still POSTed the REFRESH TOKEN to the remote host.
    ///
    /// Guarding here rather than at the three call sites covers every current
    /// and future `inner()` consumer of this flow.
    fn offline_guard(path: &str) -> Result<()> {
        if prism_runtime::offline::enabled() {
            anyhow::bail!(
                "offline mode: POST {path} blocked by --offline \
                 (remove the flag to reach the platform)"
            );
        }
        Ok(())
    }

    /// Start the device authorisation flow.
    ///
    /// Calls `POST {base_url}/auth/device/start` with `client_id=prism-cli`.
    pub async fn start_device_flow(
        client: &reqwest::Client,
        base_url: &str,
    ) -> Result<DeviceCodeResponse> {
        let url = format!("{base_url}/auth/device/start");
        Self::offline_guard(&url)?;
        debug!(%url, "starting device flow");

        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "client_id": "prism-cli" }))
            .send()
            .await
            .context("failed to start device flow")?
            .platform_error_for_status()
            .await?;

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
        // Before the sleep loop: refusing after a wait would be indistinguishable
        // from a slow network to anyone watching.
        Self::offline_guard(&url)?;
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
                    config: payload.config,
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
    ///
    /// Retried on transient failure. This runs unattended whenever a token
    /// ages out, so one dropped packet used to present to the user as "PRISM
    /// logged me out". A *rejected* refresh token (401) still fails on the
    /// first attempt — that one really does mean log in again.
    pub async fn refresh_token(
        client: &reqwest::Client,
        base_url: &str,
        refresh_token: &str,
    ) -> Result<TokenResponse> {
        let url = format!("{base_url}/auth/refresh");
        Self::offline_guard(&url)?;
        debug!(%url, "refreshing token");

        // Billable in the "must not be duplicated" sense rather than the
        // money sense: the platform rotates the refresh token, so replaying a
        // request that may already have landed burns a rotation and logs the
        // user out for real.
        let resp = retry::retrying("auth.refresh", retry::Idempotency::Billable, || async {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "refresh_token": refresh_token }))
                .send()
                .await
                .context("failed to refresh token")?;
            if !resp.status().is_success() {
                // Classify off the borrow before the body read consumes the
                // response, then keep the platform's own reason on top: a
                // rejected refresh token has to say `prism login`, not
                // "returned error status 401". Same shape, and the same
                // reasoning, as `PlatformClient::send_retrying`.
                let classified = retry::HttpStatus::from_response(&resp);
                let reason = PlatformError::from_response(resp).await;
                return Err(classified).context(reason);
            }
            Ok(resp)
        })
        .await?;

        resp.json::<TokenResponse>()
            .await
            .context("failed to parse refresh response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that would take the full connect timeout if a request were
    /// actually issued. The guard must refuse before that, so these tests are
    /// fast — a slow one means the guard did not fire.
    const UNROUTABLE: &str = "http://127.0.0.1:1";

    /// The refresh path is the one that matters most: it POSTs the REFRESH
    /// TOKEN, and it runs unattended whenever a session nears expiry. It takes
    /// a raw `&reqwest::Client` obtained via `PlatformClient::inner()`, which
    /// bypasses `PlatformClient`'s own guard — so before this it ran under
    /// hard offline mode and sent the token anyway.
    /// Serializing an env mutation around an await REQUIRES holding the guard
    /// across it — dropping it early is exactly the race the lock prevents,
    /// since `PRISM_OFFLINE` is process-global. Matches the existing precedent
    /// in `agent/src/protocol.rs` and `agent/src/meta_tools.rs`.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn refresh_token_is_refused_offline() {
        let _guard = prism_runtime::offline::test_support::env_lock();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };
        let client = reqwest::Client::new();
        let result = DeviceFlowAuth::refresh_token(&client, UNROUTABLE, "refresh-abc").await;
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let err = result
            .expect_err("offline must refuse the refresh")
            .to_string();
        assert!(err.contains("offline mode"), "{err}");
        assert!(
            err.contains("/auth/refresh"),
            "must name what it blocked: {err}"
        );
        // The secret must never appear in the refusal.
        assert!(
            !err.contains("refresh-abc"),
            "credential leaked into the error: {err}"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn device_flow_start_is_refused_offline() {
        let _guard = prism_runtime::offline::test_support::env_lock();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };
        let client = reqwest::Client::new();
        let result = DeviceFlowAuth::start_device_flow(&client, UNROUTABLE).await;
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let err = result
            .expect_err("offline must refuse device login")
            .to_string();
        assert!(err.contains("offline mode"), "{err}");
        assert!(err.contains("/auth/device/start"), "{err}");
    }

    /// The poll refuses BEFORE its sleep loop; refusing after would look
    /// identical to a slow network.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn device_flow_poll_is_refused_offline_without_sleeping() {
        let _guard = prism_runtime::offline::test_support::env_lock();
        unsafe { std::env::set_var(prism_runtime::offline::ENV, "1") };
        let client = reqwest::Client::new();
        let started = std::time::Instant::now();
        let result = DeviceFlowAuth::poll_for_token(&client, UNROUTABLE, "code", 30).await;
        let elapsed = started.elapsed();
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };

        let err = result.expect_err("offline must refuse polling").to_string();
        assert!(err.contains("offline mode"), "{err}");
        assert!(
            elapsed < Duration::from_secs(5),
            "refused only after sleeping {elapsed:?} — guard is after the sleep"
        );
    }

    /// Offline unset means the guard is inert: the call proceeds and fails for
    /// a network reason, not a policy one. Without this the tests above would
    /// pass even if the guard refused unconditionally.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn the_guard_is_inert_when_offline_is_unset() {
        let _guard = prism_runtime::offline::test_support::env_lock();
        unsafe { std::env::remove_var(prism_runtime::offline::ENV) };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(400))
            .build()
            .expect("client");
        let err = DeviceFlowAuth::start_device_flow(&client, UNROUTABLE)
            .await
            .expect_err("unroutable host still fails")
            .to_string();
        assert!(
            !err.contains("offline mode"),
            "guard fired with offline unset: {err}"
        );
    }
}
