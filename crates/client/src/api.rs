use anyhow::{Context, Result};
use prism_runtime::retry;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;
use tracing::debug;

// Platform failures are translated inside `send_retrying`, which needs the
// typed error rather than the `platform_error_for_status` convenience: it has
// to take the retry verdict off the response *before* the body read consumes
// it, and it treats any non-2xx as a failure.
use crate::platform_error::PlatformError;

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

    /// Send a request, retrying only transient failures.
    ///
    /// Every platform call in PRISM funnels through here, so this is the one
    /// place that has to get "worth another attempt?" right. The request is
    /// rebuilt per attempt (`send` consumes the builder) and
    /// [`prism_runtime::retry`] owns the verdict: a 503 or a reset socket
    /// comes back, a 401 or a 402 does not.
    ///
    /// A failed response has to answer two different questions, and ownership
    /// forces the order:
    ///
    /// 1. **Is it worth another attempt?** [`retry::HttpStatus::from_response`]
    ///    borrows, so it must run *first* — reading the body consumes the
    ///    response, and the status and `Retry-After` go with it.
    /// 2. **What does the user need to know?** Only the body carries the
    ///    platform's own `code`, `message` and `help`, so answering this
    ///    consumes the response.
    ///
    /// The two are then combined the way [`prism_runtime::retry`] documents:
    /// the human-readable [`PlatformError`] on top, [`retry::HttpStatus`]
    /// attached as its cause. That ordering is load-bearing in both
    /// directions. `retry::is_retryable` walks the cause chain and takes the
    /// verdict of the first link it recognises; `PlatformError` is not one of
    /// the types it knows, so classification still reaches the `HttpStatus`
    /// underneath and a 401 keeps failing on the first attempt. Meanwhile the
    /// message the user sees is the platform's own words instead of a bare
    /// "returned error status 401".
    async fn send_retrying(
        &self,
        method: &str,
        url: &str,
        idem: retry::Idempotency,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        retry::retrying(&format!("platform.{method}"), idem, || async {
            let resp = build()
                .headers(self.auth_headers()?)
                .send()
                .await
                .with_context(|| format!("{method} {url} failed"))?;
            if !resp.status().is_success() {
                // Borrow for the retry verdict…
                let classified = retry::HttpStatus::from_response(&resp);
                // …then consume for the platform's own reason.
                let reason = PlatformError::from_response(resp).await;
                return Err(classified).context(reason);
            }
            Ok(resp)
        })
        .await
    }

    /// Perform an authenticated GET request and deserialise the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.offline_guard("GET", path)?;
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "GET");

        let resp = self
            .send_retrying("GET", &url, retry::Idempotency::Safe, || {
                self.client.get(&url)
            })
            .await?;

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
            .send_retrying(
                "POST",
                &url,
                // A platform POST creates something (a project, a node
                // registration, a key exchange). Replaying one that may
                // already have landed is how you get two of everything.
                retry::Idempotency::Billable,
                || self.client.post(&url).json(body),
            )
            .await?;

        resp.json::<T>()
            .await
            .with_context(|| format!("failed to parse JSON from POST {url}"))
    }

    /// Perform an authenticated PATCH request with a JSON body and
    /// deserialise the response.
    ///
    /// `Safe` idempotency: PATCH bodies here carry the full desired value of
    /// each field they set, so replaying one lands the same state.
    pub async fn patch<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.offline_guard("PATCH", path)?;
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "PATCH");

        let resp = self
            .send_retrying("PATCH", &url, retry::Idempotency::Safe, || {
                self.client.patch(&url).json(body)
            })
            .await?;

        resp.json::<T>()
            .await
            .with_context(|| format!("failed to parse JSON from PATCH {url}"))
    }

    /// Perform an authenticated DELETE request. Returns `Ok(())` on success.
    pub async fn delete(&self, path: &str) -> Result<()> {
        self.offline_guard("DELETE", path)?;
        let url = format!("{}{path}", self.base_url);
        debug!(%url, "DELETE");

        self.send_retrying("DELETE", &url, retry::Idempotency::Safe, || {
            self.client.delete(&url)
        })
        .await?;

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
    ///
    /// Goes through [`Self::get`] rather than hand-rolling the request, so it
    /// inherits the same offline guard and the same retry policy as every
    /// other platform call.
    pub async fn list_projects_for_org(&self, org_id: &str) -> Result<Vec<ProjectInfo>> {
        self.get(&format!("/projects?org_id={}", urlencoding::encode(org_id)))
            .await
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
