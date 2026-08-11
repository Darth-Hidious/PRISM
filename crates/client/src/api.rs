use anyhow::{Context, Result};
use prism_runtime::{auth::PlatformAuth, retry};
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

// Platform failures are translated inside `send_retrying`, which needs the
// typed error before the response body is consumed.
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

/// Typed HTTP client for a configured PRISM-compatible provider API.
///
/// The base URL should include the API version prefix,
/// e.g. `https://provider.example/api/v1`.
#[derive(Debug, Clone)]
pub struct PlatformClient {
    base_url: String,
    client: reqwest::Client,
    credential: Option<PlatformAuth>,
}

impl PlatformClient {
    /// Create a new client pointing at the given API base URL.
    ///
    /// The URL should include the version prefix (e.g. `https://provider.example/api/v1`).
    pub fn new(base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");

        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
            credential: None,
        }
    }

    /// Attach a raw legacy credential for authenticated requests.
    ///
    /// New callers that know whether they hold an API key or a bearer token
    /// should use [`Self::with_auth`]. This method retains the frozen `m27_`
    /// shape heuristic for source compatibility with older callers.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.credential = Some(PlatformAuth::classify(&token.into()));
        self
    }

    /// Attach a credential with its PRISM-defined wire semantics intact.
    pub fn with_auth(mut self, credential: PlatformAuth) -> Self {
        self.credential = Some(credential);
        self
    }

    /// Return the base URL this client targets.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Return the credential (API key or access token) this client authenticates
    /// with, if any. Used by node-up to assert the stored client carries the
    /// refreshed token after a 401-retry (a previous bug stored the stale,
    /// 401'd client and the daemon then ran all REST calls on the dead token).
    pub fn access_token(&self) -> Option<&str> {
        self.credential.as_ref().map(PlatformAuth::secret)
    }

    /// Return a reference to the inner reqwest client.
    pub fn inner(&self) -> &reqwest::Client {
        &self.client
    }

    /// Build authorization headers if a credential is set.
    ///
    /// Explicit credential kind is authoritative. The raw [`Self::with_token`]
    /// compatibility constructor classifies the frozen `m27_` prefix, while
    /// [`Self::with_auth`] allows provider-defined API-key shapes.
    pub(crate) fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(credential) = &self.credential {
            match credential {
                PlatformAuth::ApiKey(key) => {
                    let val =
                        HeaderValue::from_str(key).context("invalid characters in API key")?;
                    headers.insert(HeaderName::from_static("x-api-key"), val);
                }
                PlatformAuth::Bearer(token) => {
                    let val = HeaderValue::from_str(&format!("Bearer {token}"))
                        .context("invalid characters in access token")?;
                    headers.insert(AUTHORIZATION, val);
                }
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
        // One rule, one home: `offline::enabled()` also trims, so a
        // `PRISM_OFFLINE=" 1"` from a shell or CI template is honoured here
        // exactly as it is everywhere else. This re-derived the check without
        // the trim.
        if prism_runtime::offline::enabled() {
            anyhow::bail!(
                "offline mode: {method} {path} blocked by --offline \
                 (remove the flag to reach the configured provider)"
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
            .headers(self.auth_headers().map_err(|e| ApiError {
                status: StatusCode::UNAUTHORIZED,
                body_text: format!("invalid token in authorization header: {e}"),
                code: Some("bad_credentials".to_string()),
            })?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_api_key_kind_does_not_depend_on_marc27_prefix() {
        let headers = PlatformClient::new("https://provider.example/api/v1")
            .with_auth(PlatformAuth::ApiKey("provider-defined-key".into()))
            .auth_headers()
            .unwrap();

        assert_eq!(headers.get("x-api-key").unwrap(), "provider-defined-key");
        assert!(!headers.contains_key(AUTHORIZATION));
    }

    #[test]
    fn explicit_bearer_kind_overrides_legacy_prefix_heuristic() {
        let headers = PlatformClient::new("https://provider.example/api/v1")
            .with_auth(PlatformAuth::Bearer("m27_session-shaped".into()))
            .auth_headers()
            .unwrap();

        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer m27_session-shaped"
        );
        assert!(!headers.contains_key("x-api-key"));
    }

    #[test]
    fn raw_token_constructor_retains_legacy_marc27_prefix_heuristic() {
        let headers = PlatformClient::new("https://provider.example/api/v1")
            .with_token("m27_legacy_key")
            .auth_headers()
            .unwrap();

        assert_eq!(headers.get("x-api-key").unwrap(), "m27_legacy_key");
        assert!(!headers.contains_key(AUTHORIZATION));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn explicit_provider_api_key_reaches_http_as_x_api_key() {
        // PRISM_OFFLINE is process-global, so a test that needs it UNSET depends
        // on it exactly as much as the test that sets it. Without this lock the
        // offline test's `PRISM_OFFLINE=1` refuses this request mid-flight.
        let _guard = prism_runtime::offline::test_support::env_lock();
        let _restore = prism_runtime::offline::test_support::OfflineEnvGuard::capture();

        let mut server = mockito::Server::new_async().await;
        let request = server
            .mock("GET", "/api/v1/probe")
            .match_header("x-api-key", "provider-defined-key")
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;

        let response: serde_json::Value = PlatformClient::new(format!("{}/api/v1", server.url()))
            .with_auth(PlatformAuth::ApiKey("provider-defined-key".into()))
            .get("/probe")
            .await
            .unwrap();

        assert_eq!(response, serde_json::json!({}));
        request.assert_async().await;
    }

    /// The highest-blast-radius guard on the branch, and it had NO test.
    ///
    /// `offline_guard` backs `get`/`post`/`post_inspect`/`patch`/`delete` —
    /// essentially every authenticated platform call from cli, agent, node,
    /// server, tui and mesh. This branch also switched it from a hand-rolled
    /// untrimmed `== "1"` to `offline::enabled()`; nothing proved either the
    /// blocking or the trim.
    ///
    /// Base URL is `0.0.0.0:1`, chosen deliberately: `is_loopback_url` treats
    /// it as NON-loopback (pinned in offline.rs's own tests), so the policy
    /// must refuse it — while the OS refuses a connection to it in ~7 ms, so
    /// the not-offline half does not spend a connect timeout per value. The
    /// first version used TEST-NET-3 and took 75 s.
    ///
    /// A guard failure therefore surfaces as a TRANSPORT error, not a policy
    /// one, and the assertions distinguish the two rather than merely checking
    /// that an error occurred.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn every_verb_is_blocked_offline_and_the_value_is_trimmed() {
        let _guard = prism_runtime::offline::test_support::env_lock();
        let _restore = prism_runtime::offline::test_support::OfflineEnvGuard::capture();

        let client = PlatformClient::new("http://0.0.0.0:1/api/v1");

        for value in ["1", " 1", "1 ", "\t1\n"] {
            unsafe { std::env::set_var("PRISM_OFFLINE", value) };

            let get: Result<serde_json::Value> = client.get("/x").await;
            let msg = format!("{:#}", get.expect_err("GET must be refused"));
            assert!(
                msg.contains("offline mode"),
                "PRISM_OFFLINE={value:?}: {msg}"
            );

            let post: Result<serde_json::Value> = client.post("/x", &serde_json::json!({})).await;
            assert!(
                format!("{:#}", post.expect_err("POST must be refused")).contains("offline mode"),
                "POST not refused for {value:?}"
            );

            let patch: Result<serde_json::Value> = client.patch("/x", &serde_json::json!({})).await;
            assert!(
                format!("{:#}", patch.expect_err("PATCH must be refused")).contains("offline mode"),
                "PATCH not refused for {value:?}"
            );

            let delete: Result<()> = client.delete("/x").await;
            assert!(
                format!("{:#}", delete.expect_err("DELETE must be refused"))
                    .contains("offline mode"),
                "DELETE not refused for {value:?}"
            );
        }

        // Values that are NOT offline must fall through to a real attempt —
        // otherwise every assertion above would pass against a guard that
        // refused unconditionally.
        for value in ["0", "", "true", "yes"] {
            unsafe { std::env::set_var("PRISM_OFFLINE", value) };
            let got: Result<serde_json::Value> = client.get("/x").await;
            let msg = format!("{:#}", got.expect_err("nothing is listening"));
            assert!(
                !msg.contains("offline mode"),
                "PRISM_OFFLINE={value:?} must not enable offline: {msg}"
            );
        }
    }

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
