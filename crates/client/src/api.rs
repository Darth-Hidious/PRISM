use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use tracing::debug;

/// A non-2xx platform response, carrying the HTTP status and (when present)
/// the parsed JSON error body so callers can react to specific error codes
/// (e.g. a `401` with `{"code":"token_expired"}` → refresh + retry).
///
/// Built by [`PlatformClient::post_inspect`], which — unlike [`PlatformClient::post`]
/// — reads the response body on failure instead of discarding it via
/// `error_for_status()`. The opaque status-only error path was the reason a
/// stale-token `401` surfaced as an undiagnosable "returned error status" and
/// dropped `node up` to silent offline mode.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    /// Best-effort raw body text. Empty if the response had no body or the
    /// read failed; never used to gate logic on its own.
    pub body_text: String,
    /// Parsed `{"code": ...}` from the body, when the platform used its
    /// standard error envelope. `None` for unstructured/empty bodies.
    pub code: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{} ({code})", self.status),
            None => write!(f, "{}", self.status),
        }
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// True when this failure indicates the request's credential was rejected
    /// as expired/invalid and a refresh + retry may recover it.
    ///
    /// Matches a `401 Unauthorized` whose body carried the platform's
    /// `token_expired` code, OR any `401` (some paths return a bare 401). A
    /// 403 (revoked) is intentionally NOT included — a revoked token won't be
    /// fixed by a refresh.
    pub fn is_token_expired(&self) -> bool {
        if self.status != StatusCode::UNAUTHORIZED {
            return false;
        }
        match &self.code {
            // Explicit server signal.
            Some(c) => c == "token_expired",
            // Bare 401 with no parseable envelope — treat as retryable too,
            // since the only 401 the node-register endpoint emits on a stale
            // JWT is the expired-token case. A refresh attempt is cheap and
            // self-bounds to one retry at the call site.
            None => true,
        }
    }
}

/// The platform's standard JSON error envelope: `{"error":{"code","message"}}`.
#[derive(Debug, Clone, Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: Option<String>,
}

/// Response type for the current user endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

/// A project within an organisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub org_id: String,
}

/// An organisation the user belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgInfo {
    pub id: String,
    pub name: String,
    pub slug: String,
}

/// Typed HTTP client for the MARC27 platform API.
///
/// The base URL should include the API version prefix,
/// e.g. `https://api.marc27.com/api/v1`.
#[derive(Debug, Clone)]
pub struct PlatformClient {
    base_url: String,
    client: reqwest::Client,
    access_token: Option<String>,
}

impl PlatformClient {
    /// Create a new client pointing at the given API base URL.
    ///
    /// The URL should include the version prefix (e.g. `https://api.marc27.com/api/v1`).
    pub fn new(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");

        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
            access_token: None,
        }
    }

    /// Attach an access token for authenticated requests.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.access_token = Some(token.into());
        self
    }

    /// Return the base URL this client targets.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Return a reference to the inner reqwest client.
    pub fn inner(&self) -> &reqwest::Client {
        &self.client
    }

    /// Build authorization headers if a credential is set.
    ///
    /// Routes by credential shape: non-expiring `m27_*` API keys authenticate
    /// on the `X-API-Key` header, while rotating session JWTs use
    /// `Authorization: Bearer`. The platform rejects each on the other's
    /// channel, so a headless server or agent configured with an API key
    /// (no login, no device flow, no refresh) authenticates correctly here.
    pub(crate) fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(ref token) = self.access_token {
            if token.starts_with("m27_") {
                let val = HeaderValue::from_str(token).context("invalid characters in API key")?;
                headers.insert(HeaderName::from_static("x-api-key"), val);
            } else {
                let val = HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("invalid characters in access token")?;
                headers.insert(AUTHORIZATION, val);
            }
        }
        Ok(headers)
    }

    // ── generic helpers ────────────────────────────────────────────

    /// Bail with a clean error when `--offline` is active.
    ///
    /// `prism --offline …` sets `PRISM_OFFLINE=1`; every platform HTTP
    /// helper checks it here so offline mode actually blocks network
    /// calls instead of being silently ignored (break-test defect H-3).
    fn offline_guard(&self, method: &str, path: &str) -> Result<()> {
        if std::env::var("PRISM_OFFLINE").is_ok_and(|v| v == "1") {
            anyhow::bail!(
                "offline mode: {method} {path} blocked by --offline \
                 (remove the flag to reach the MARC27 platform)"
            );
        }
        Ok(())
    }

    /// Perform an authenticated GET request and deserialise the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.offline_guard("GET", path)?;
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "GET");

        let resp = self
            .client
            .get(&url)
            .headers(self.auth_headers()?)
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?
            .error_for_status()
            .with_context(|| format!("GET {url} returned error status"))?;

        resp.json::<T>()
            .await
            .with_context(|| format!("failed to parse JSON from GET {url}"))
    }

    /// Perform an authenticated POST request with a JSON body and deserialise the response.
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        self.offline_guard("POST", path)?;
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "POST");

        let resp = self
            .client
            .post(&url)
            .headers(self.auth_headers()?)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?
            .error_for_status()
            .with_context(|| format!("POST {url} returned error status"))?;

        resp.json::<T>()
            .await
            .with_context(|| format!("failed to parse JSON from POST {url}"))
    }

    /// POST with body, returning either the deserialised success body or an
    /// [`ApiError`] carrying the HTTP status + parsed error `code`.
    ///
    /// Unlike [`PlatformClient::post`], this never calls `error_for_status()`
    /// (which discards the body) — it inspects the status itself and reads the
    /// body on the failure path so callers can branch on specific codes such
    /// as `token_expired`. Used by the node-registration refresh+retry path so
    /// a stale session token can be detected and recovered instead of dropping
    /// `node up` to silent offline mode.
    pub async fn post_inspect<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> std::result::Result<T, ApiError> {
        // offline_guard returns anyhow::Result<()>; surface a 403-shaped ApiError
        // so callers treating any Err as "registration blocked" still get a
        // consistent type. Reachable only under PRISM_OFFLINE=1.
        if let Err(e) = self.offline_guard("POST", path) {
            return Err(ApiError {
                status: StatusCode::FORBIDDEN,
                body_text: format!("{e:#}"),
                code: Some("offline".to_string()),
            });
        }
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "POST (inspect)");

        let resp = self
            .client
            .post(&url)
            .headers(self.auth_headers().expect("auth headers valid"))
            .json(body)
            .send()
            .await
            .map_err(|e| ApiError {
                status: StatusCode::BAD_GATEWAY,
                body_text: format!("POST {url} failed: {e}"),
                code: Some("send_failed".to_string()),
            })?;

        let status = resp.status();
        if status.is_success() {
            resp.json::<T>().await.map_err(|e| ApiError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                body_text: format!("failed to parse JSON from POST {url}: {e}"),
                code: Some("parse_failed".to_string()),
            })
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            let code = serde_json::from_str::<ErrorEnvelope>(&body_text)
                .ok()
                .and_then(|env| env.error)
                .and_then(|b| b.code);
            Err(ApiError {
                status,
                body_text,
                code,
            })
        }
    }

    /// Perform an authenticated DELETE request. Returns `Ok(())` on success.
    pub async fn delete(&self, path: &str) -> Result<()> {
        self.offline_guard("DELETE", path)?;
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "DELETE");

        self.client
            .delete(&url)
            .headers(self.auth_headers()?)
            .send()
            .await
            .with_context(|| format!("DELETE {url} failed"))?
            .error_for_status()
            .with_context(|| format!("DELETE {url} returned error status"))?;

        Ok(())
    }

    // ── convenience endpoints ──────────────────────────────────────

    /// Fetch the currently authenticated user's profile.
    pub async fn fetch_current_user(&self) -> Result<UserInfo> {
        self.get("/users/me").await
    }

    /// List projects, optionally filtered by organisation.
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>> {
        self.get("/projects").await
    }

    /// List organisations the current user belongs to.
    pub async fn list_orgs(&self) -> Result<Vec<OrgInfo>> {
        self.get("/orgs").await
    }

    /// List projects filtered by organisation.
    pub async fn list_projects_for_org(&self, org_id: &str) -> Result<Vec<ProjectInfo>> {
        let url = format!("{}/projects", self.base_url);
        debug!(%url, org_id, "GET (filtered)");

        let resp = self
            .client
            .get(&url)
            .query(&[("org_id", org_id)])
            .headers(self.auth_headers()?)
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?
            .error_for_status()
            .with_context(|| format!("GET {url} returned error status"))?;

        resp.json::<Vec<ProjectInfo>>()
            .await
            .with_context(|| format!("failed to parse JSON from GET {url}"))
    }

    /// Get a project by ID.
    pub async fn get_project(&self, project_id: &str) -> Result<ProjectInfo> {
        self.get(&format!("/projects/{project_id}")).await
    }

    /// Create a new project within an organisation.
    pub async fn create_project(
        &self,
        org_id: &str,
        name: &str,
        slug: &str,
    ) -> Result<ProjectInfo> {
        self.post(
            "/projects",
            &serde_json::json!({
                "name": name,
                "slug": slug,
                "org_id": org_id,
            }),
        )
        .await
    }

    // ── role sync ───────────────────────────────────────────────────

    /// Fetch the roles of all members in the current user's organisation.
    ///
    /// Expected response: `[{ "user_id": "...", "role": "owner|admin|member|viewer" }, ...]`
    ///
    /// **Note:** This endpoint may not be deployed yet on the live API.
    /// Callers should treat errors as non-fatal.
    pub async fn fetch_org_roles(&self, org_id: &str) -> Result<Vec<OrgMemberRole>> {
        self.get(&format!("/orgs/{org_id}/members")).await
    }

    // ── LLM key provisioning ────────────────────────────────────────

    /// Fetch managed LLM API keys provisioned for this organisation.
    ///
    /// **Note:** This endpoint may not be deployed yet on the live API.
    /// Callers should treat errors as non-fatal.
    pub async fn fetch_llm_keys(&self, org_id: &str) -> Result<Vec<LlmKeyEntry>> {
        self.get(&format!("/orgs/{org_id}/keys/llm")).await
    }
}

/// A member's role within an organisation, as returned by the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgMemberRole {
    pub user_id: String,
    pub role: String,
}

/// A managed LLM API key entry provisioned by the platform.
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmKeyEntry {
    pub provider: String,
    pub api_key: String,
    #[serde(default)]
    pub model_filter: Option<String>,
}

impl std::fmt::Debug for LlmKeyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmKeyEntry")
            .field("provider", &self.provider)
            .field("api_key", &"[REDACTED]")
            .field("model_filter", &self.model_filter)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_is_token_expired_matches_401_with_code() {
        // The exact server signal: 401 + {"error":{"code":"token_expired"}}.
        let err = ApiError {
            status: StatusCode::UNAUTHORIZED,
            body_text: r#"{"error":{"code":"token_expired","message":"token expired"}}"#
                .to_string(),
            code: Some("token_expired".to_string()),
        };
        assert!(err.is_token_expired());
    }

    #[test]
    fn api_error_is_token_expired_matches_bare_401() {
        // A bare 401 with no parseable envelope is still treated as retryable:
        // the node-register endpoint only emits 401 on an expired JWT.
        let err = ApiError {
            status: StatusCode::UNAUTHORIZED,
            body_text: String::new(),
            code: None,
        };
        assert!(err.is_token_expired());
    }

    #[test]
    fn api_error_is_token_expired_rejects_revoked_403() {
        // A revoked token (403) is NOT recoverable by a refresh — must not be
        // flagged retryable, or the daemon would loop refresh-fail forever.
        let err = ApiError {
            status: StatusCode::FORBIDDEN,
            body_text: r#"{"error":{"code":"token_revoked"}}"#.to_string(),
            code: Some("token_revoked".to_string()),
        };
        assert!(!err.is_token_expired());
    }

    #[test]
    fn api_error_is_token_expired_rejects_4xx_other_than_401() {
        // 400/404/409 etc. are client-side problems, not auth-expiry.
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            let err = ApiError {
                status,
                body_text: String::new(),
                code: None,
            };
            assert!(!err.is_token_expired(), "{status} should not be retryable");
        }
    }

    #[test]
    fn error_envelope_parses_platform_code() {
        // The platform's standard envelope: {"error":{"code","message"}}.
        let body = r#"{"error":{"code":"token_expired","message":"token expired — refresh it and retry"}}"#;
        let env: ErrorEnvelope = serde_json::from_str(body).unwrap();
        assert_eq!(env.error.unwrap().code.as_deref(), Some("token_expired"));
    }

    #[test]
    fn error_envelope_handles_missing_error_field() {
        // Defensive: a non-envelope body must not panic on parse.
        let env: ErrorEnvelope = serde_json::from_str(r#"{"unrelated":true}"#).unwrap();
        assert!(env.error.is_none());
    }
}
