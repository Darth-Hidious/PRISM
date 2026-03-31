use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

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

    /// Build authorization headers if a token is set.
    pub(crate) fn auth_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(ref token) = self.access_token {
            let val = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid characters in access token")?;
            headers.insert(AUTHORIZATION, val);
        }
        headers.try_reserve(0).ok(); // no-op, keeps borrow checker happy
        Ok(headers)
    }

    // ── generic helpers ────────────────────────────────────────────

    /// Perform an authenticated GET request and deserialise the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
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

    /// Perform an authenticated DELETE request. Returns `Ok(())` on success.
    pub async fn delete(&self, path: &str) -> Result<()> {
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
