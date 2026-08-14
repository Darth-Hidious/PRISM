use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use prism_runtime::retry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::debug;

use crate::platform_error::{PlatformError, PlatformResponseExt};
use crate::supabase_auth::{SupabaseAuth, SupabaseAuthPolicy, SupabaseClaims};

/// Stable identity-provider id for the retained MARC27 device flow.
pub const MARC27_IDENTITY_PROVIDER: &str = "marc27";
/// Stable identity-provider id for Supabase Auth.
pub const SUPABASE_IDENTITY_PROVIDER: &str = "supabase";
/// Stable identity-provider id for Mirdyne.
///
/// Mirdyne is a SEPARATE identity domain from MARC27 — different issuer,
/// different principal namespace, different RBAC subject. That separation is
/// the point: an account in one is not an account in the other, and the
/// corporate boundary between them is auditable because the provider id is
/// recorded on every verified identity.
///
/// It currently authenticates against a Mirdyne-owned JWT issuer using the
/// same signature/`exp`/`iss`/`aud` verification Supabase gets, because that
/// path is proven and a second hand-rolled verifier would be a second place
/// to get token validation wrong. It is NOT an alias: a Supabase token is not
/// a Mirdyne token, because the issuer it is checked against differs, and a
/// token minted for one fails `iss` verification for the other.
///
/// When Mirdyne gains its own OIDC service, only this adapter's arms change —
/// callers select providers by id and never branch on the protocol.
pub const MIRDYNE_IDENTITY_PROVIDER: &str = "mirdyne";

/// Provider-specific authentication adapter selected explicitly at login and
/// refresh boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityProviderAdapter {
    Marc27,
    Supabase,
    Mirdyne,
}

/// Provider-neutral identity established only after the configured provider
/// has verified the presented access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub provider: IdentityProviderAdapter,
    /// Provider-native subject. It is never accepted directly as a PRISM id.
    pub subject_id: String,
    /// Signature-verified namespace used to canonicalize provider subjects.
    /// Supabase carries its exact verified issuer; MARC27's legacy account IDs
    /// are already canonical and therefore need no separate scope.
    pub provider_scope: Option<String>,
    /// Canonical identity used by PRISM sessions and RBAC.
    pub principal_id: String,
    /// Signed provider role claim, retained for mapping at the RBAC boundary.
    pub role_claim: Option<String>,
}

/// Complete configuration for verifying access tokens presented to a node.
///
/// Fields are private so an unknown provider or an incomplete Supabase
/// configuration cannot be installed in server state. The custom `Debug`
/// implementation never exposes the Supabase anon key.
#[derive(Clone)]
pub struct IdentityVerifierConfig {
    adapter: IdentityProviderAdapter,
    provider_url: String,
    provider_key: Option<String>,
    supabase_policy: SupabaseAuthPolicy,
}

impl std::fmt::Debug for IdentityVerifierConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityVerifierConfig")
            .field("adapter", &self.adapter)
            .field("provider_url", &self.provider_url)
            .field(
                "provider_key",
                &self.provider_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("supabase_policy", &self.supabase_policy)
            .finish()
    }
}

impl IdentityVerifierConfig {
    /// Build a verifier using the provider's documented default policy.
    pub fn new(
        provider: Option<&str>,
        provider_url: &str,
        provider_key: Option<&str>,
    ) -> Result<Self> {
        Self::with_supabase_policy(
            provider,
            provider_url,
            provider_key,
            SupabaseAuthPolicy::default(),
        )
    }

    /// Build a verifier with an explicit Supabase verification policy.
    ///
    /// MARC27 does not use the Supabase policy; it continues to verify tokens
    /// against its authenticated `/users/me` endpoint.
    pub fn with_supabase_policy(
        provider: Option<&str>,
        provider_url: &str,
        provider_key: Option<&str>,
        supabase_policy: SupabaseAuthPolicy,
    ) -> Result<Self> {
        let adapter = identity_provider_for(provider).with_context(|| match provider {
            Some(provider) => {
                format!("unknown identity provider `{provider}`; refusing token verification")
            }
            None => "identity provider is missing; refusing token verification".to_string(),
        })?;
        let provider_url = provider_url.trim();
        ensure!(
            !provider_url.is_empty(),
            "identity provider URL is not configured"
        );

        let provider_key = match adapter {
            IdentityProviderAdapter::Marc27 => {
                ensure!(
                    provider_key.is_none(),
                    "MARC27 identity verification does not accept a provider key"
                );
                None
            }
            IdentityProviderAdapter::Supabase => {
                let provider_key = provider_key
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .context("Supabase is not configured: missing identity provider anon key")?;
                // Validate the project URL and key now, before this config can
                // be installed in a long-lived node state.
                SupabaseAuth::new(
                    reqwest::Client::new(),
                    provider_url,
                    provider_key,
                    supabase_policy.clone(),
                )?;
                Some(provider_key.to_string())
            }
            // Mirdyne fails closed on exactly the same terms as Supabase: an
            // unconfigured issuer must never yield a verifier that accepts
            // tokens, because a verifier that cannot check `iss` would accept
            // ANY provider's token as a Mirdyne identity.
            IdentityProviderAdapter::Mirdyne => {
                let provider_key = provider_key
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .context(
                        "Mirdyne is not configured: missing identity provider key. Set the \
                         Mirdyne issuer URL and key, or log in with `--provider marc27`.",
                    )?;
                SupabaseAuth::new(
                    reqwest::Client::new(),
                    provider_url,
                    provider_key,
                    supabase_policy.clone(),
                )?;
                Some(provider_key.to_string())
            }
        };

        Ok(Self {
            adapter,
            provider_url: provider_url.to_string(),
            provider_key,
            supabase_policy,
        })
    }

    pub fn provider(&self) -> IdentityProviderAdapter {
        self.adapter
    }

    /// Verify one bearer token through the explicitly selected provider.
    pub async fn verify_access_token(&self, token: &str) -> Result<VerifiedIdentity> {
        self.adapter
            .verify_access_token(
                &self.provider_url,
                self.provider_key.as_deref(),
                token,
                &self.supabase_policy,
            )
            .await
    }
}

/// Provider refresh output. Supabase carries verified claims so callers can
/// update PRISM's provider-scoped role assignment before persisting tokens.
#[derive(Debug, Clone)]
pub struct ProviderTokenResponse {
    pub tokens: TokenResponse,
    pub supabase_claims: Option<SupabaseClaims>,
}

impl IdentityProviderAdapter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Marc27 => MARC27_IDENTITY_PROVIDER,
            Self::Supabase => SUPABASE_IDENTITY_PROVIDER,
            Self::Mirdyne => MIRDYNE_IDENTITY_PROVIDER,
        }
    }

    /// Refresh through this provider's real protocol.
    ///
    /// `provider_url` is MARC27's API base for `Marc27` and the Supabase
    /// project root for `Supabase`. `provider_key` is required only by
    /// Supabase and is never forwarded to MARC27.
    pub async fn refresh_token(
        self,
        client: &reqwest::Client,
        provider_url: &str,
        provider_key: Option<&str>,
        refresh_token: &str,
    ) -> Result<ProviderTokenResponse> {
        match self {
            Self::Marc27 => Ok(ProviderTokenResponse {
                tokens: DeviceFlowAuth::refresh_token(client, provider_url, refresh_token).await?,
                supabase_claims: None,
            }),
            Self::Supabase => {
                let provider_key = provider_key
                    .context("Supabase is not configured: missing identity provider anon key")?;
                let auth = SupabaseAuth::new(
                    client.clone(),
                    provider_url,
                    provider_key,
                    SupabaseAuthPolicy::default(),
                )?;
                let session = auth.refresh_session(refresh_token).await?;
                Ok(ProviderTokenResponse {
                    tokens: session.tokens,
                    supabase_claims: Some(session.claims),
                })
            }
            // Mirdyne refreshes against ITS OWN issuer: `provider_url` is the
            // Mirdyne project root, never MARC27's. The claims come back under
            // Mirdyne's `iss`, which is what keeps the two identity domains
            // from collapsing into one on refresh.
            Self::Mirdyne => {
                let provider_key = provider_key
                    .context("Mirdyne is not configured: missing identity provider key")?;
                let auth = SupabaseAuth::new(
                    client.clone(),
                    provider_url,
                    provider_key,
                    SupabaseAuthPolicy::default(),
                )?;
                let session = auth.refresh_session(refresh_token).await?;
                Ok(ProviderTokenResponse {
                    tokens: session.tokens,
                    supabase_claims: Some(session.claims),
                })
            }
        }
    }

    /// Verify an access token and normalize the provider identity for PRISM.
    ///
    /// MARC27 retains its authenticated `/users/me` check. Supabase verifies
    /// the JWT signature against project JWKS and enforces `exp`, `iss`, and
    /// `aud` before any claim is returned or canonicalized.
    pub async fn verify_access_token(
        self,
        provider_url: &str,
        provider_key: Option<&str>,
        token: &str,
        supabase_policy: &SupabaseAuthPolicy,
    ) -> Result<VerifiedIdentity> {
        ensure!(!token.trim().is_empty(), "access token is empty");

        match self {
            Self::Marc27 => {
                ensure!(
                    provider_key.is_none(),
                    "MARC27 identity verification does not accept a provider key"
                );
                let user = crate::PlatformClient::new(provider_url)
                    .with_token(token)
                    .fetch_current_user()
                    .await
                    .context("MARC27 access token verification failed")?;
                ensure!(
                    !user.id.trim().is_empty(),
                    "MARC27 returned an empty verified subject"
                );
                Ok(VerifiedIdentity {
                    provider: self,
                    subject_id: user.id.clone(),
                    provider_scope: None,
                    principal_id: user.id,
                    role_claim: None,
                })
            }
            Self::Supabase => {
                let provider_key = provider_key
                    .context("Supabase is not configured: missing identity provider anon key")?;
                let auth = SupabaseAuth::new(
                    reqwest::Client::new(),
                    provider_url,
                    provider_key,
                    supabase_policy.clone(),
                )?;
                let claims = auth.verify_access_token(token).await?;
                let principal_id = canonical_supabase_principal(&claims.iss, &claims.sub)
                    .context("verified Supabase token has no canonical PRISM principal")?;
                Ok(VerifiedIdentity {
                    provider: self,
                    subject_id: claims.sub,
                    provider_scope: Some(claims.iss),
                    principal_id,
                    role_claim: claims.role,
                })
            }
            // Mirdyne. The separation between identity domains is not enforced
            // by this arm being different code — it is enforced by `iss`:
            //
            //   * the token's signature is checked against MIRDYNE's JWKS, so
            //     a token minted elsewhere fails verification outright, and
            //   * `canonical_supabase_principal` hashes the VERIFIED issuer
            //     into the principal, so even identical `sub` values under two
            //     issuers produce two different PRISM principals.
            //
            // That is why this is a real provider and not an alias: pointing
            // it at another provider's URL cannot import that provider's
            // users, it just fails to verify.
            Self::Mirdyne => {
                let provider_key = provider_key
                    .context("Mirdyne is not configured: missing identity provider key")?;
                let auth = SupabaseAuth::new(
                    reqwest::Client::new(),
                    provider_url,
                    provider_key,
                    supabase_policy.clone(),
                )?;
                let claims = auth.verify_access_token(token).await?;
                let principal_id = canonical_supabase_principal(&claims.iss, &claims.sub)
                    .context("verified Mirdyne token has no canonical PRISM principal")?;
                Ok(VerifiedIdentity {
                    provider: self,
                    subject_id: claims.sub,
                    provider_scope: Some(claims.iss),
                    principal_id,
                    role_claim: claims.role,
                })
            }
        }
    }
}

/// Select only an identity provider PRISM recognizes explicitly.
///
/// Unknown and absent provider names fail closed instead of inheriting the
/// MARC27 device or refresh protocol.
pub fn identity_provider_for(provider: Option<&str>) -> Option<IdentityProviderAdapter> {
    match provider {
        Some(MARC27_IDENTITY_PROVIDER) => Some(IdentityProviderAdapter::Marc27),
        Some(SUPABASE_IDENTITY_PROVIDER) => Some(IdentityProviderAdapter::Supabase),
        Some(MIRDYNE_IDENTITY_PROVIDER) => Some(IdentityProviderAdapter::Mirdyne),
        _ => None,
    }
}

/// Build PRISM's project-scoped canonical identity for a Supabase subject.
///
/// Supabase `sub` values are unique only within a project. The signature-
/// verified canonical issuer is hashed into a fixed-size namespace, avoiding
/// both cross-project collisions and ambiguous URL separators in persisted
/// principal identifiers.
pub fn canonical_supabase_principal(project_scope: &str, subject_id: &str) -> Option<String> {
    let project_scope = project_scope.trim().trim_end_matches('/');
    if project_scope.is_empty() || subject_id.trim().is_empty() || subject_id.trim() != subject_id {
        return None;
    }

    let project_digest = Sha256::digest(project_scope.as_bytes());
    let encoded_scope = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(project_digest);
    Some(format!("supabase:{encoded_scope}:{subject_id}"))
}

/// Polling and timeout policy for MARC27's retained device flow.
#[derive(Debug, Clone, Copy)]
pub struct DeviceFlowPolicy {
    /// Smallest interval accepted from the device-code response.
    pub minimum_poll_interval: Duration,
    /// Extra delay applied when the provider returns `slow_down`.
    pub slow_down_increment: Duration,
    /// Maximum time one device login may wait for approval.
    pub timeout: Duration,
}

impl Default for DeviceFlowPolicy {
    fn default() -> Self {
        Self {
            minimum_poll_interval: Duration::from_secs(1),
            slow_down_increment: Duration::from_secs(5),
            timeout: Duration::from_secs(15 * 60),
        }
    }
}

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
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub mp_api_key: Option<String>,
    #[serde(default)]
    pub firecrawl_api_key: Option<String>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("default_model", &self.default_model)
            .field(
                "mp_api_key",
                &self.mp_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "firecrawl_api_key",
                &self.firecrawl_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
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
#[derive(Deserialize)]
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

impl std::fmt::Debug for PollPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollPayload")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("error", &self.error)
            .field("config", &self.config)
            .finish()
    }
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
        Self::poll_for_token_with_policy(
            client,
            base_url,
            device_code,
            Duration::from_secs(interval),
            DeviceFlowPolicy::default(),
        )
        .await
    }

    /// Poll using an explicit, documented policy.
    pub async fn poll_for_token_with_policy(
        client: &reqwest::Client,
        base_url: &str,
        device_code: &str,
        interval: Duration,
        policy: DeviceFlowPolicy,
    ) -> Result<TokenResponse> {
        let url = format!("{base_url}/auth/device/poll");
        // Before the sleep loop: refusing after a wait would be indistinguishable
        // from a slow network to anyone watching.
        Self::offline_guard(&url)?;
        let mut poll_interval = interval.max(policy.minimum_poll_interval);
        let started = tokio::time::Instant::now();

        loop {
            if started.elapsed().saturating_add(poll_interval) > policy.timeout {
                bail!("device login timed out before approval");
            }
            tokio::time::sleep(poll_interval).await;
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
                    poll_interval = poll_interval.saturating_add(policy.slow_down_increment);
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

    #[test]
    fn poll_payload_debug_redacts_tokens_and_nested_api_keys() {
        let payload = PollPayload {
            access_token: Some("poll-access-secret-marker".into()),
            refresh_token: Some("poll-refresh-secret-marker".into()),
            token_type: Some("bearer".into()),
            expires_in: Some(3600),
            error: None,
            config: Some(ServerConfig {
                default_model: Some("model-name".into()),
                mp_api_key: Some("materials-api-key-secret-marker".into()),
                firecrawl_api_key: Some("firecrawl-api-key-secret-marker".into()),
            }),
        };

        let rendered = format!("{payload:?}");

        for secret in [
            "poll-access-secret-marker",
            "poll-refresh-secret-marker",
            "materials-api-key-secret-marker",
            "firecrawl-api-key-secret-marker",
        ] {
            assert!(!rendered.contains(secret), "secret leaked: {rendered}");
        }
        assert_eq!(rendered.matches("[REDACTED]").count(), 4, "{rendered}");
        assert!(rendered.contains("model-name"), "{rendered}");
    }

    #[test]
    fn identity_provider_dispatch_is_explicit_and_fails_closed() {
        assert_eq!(
            identity_provider_for(Some("marc27")),
            Some(IdentityProviderAdapter::Marc27)
        );
        assert_eq!(
            identity_provider_for(Some("supabase")),
            Some(IdentityProviderAdapter::Supabase)
        );
        assert_eq!(
            identity_provider_for(Some("mirdyne")),
            Some(IdentityProviderAdapter::Mirdyne)
        );
        assert_eq!(identity_provider_for(None), None);
        assert_eq!(identity_provider_for(Some("unknown")), None);
        assert_eq!(identity_provider_for(Some("Supabase")), None);
        assert_eq!(identity_provider_for(Some("Mirdyne")), None);
        assert_eq!(identity_provider_for(Some("")), None);
    }

    /// Mirdyne and Supabase share an implementation arm. They must NOT share
    /// identities. The separation rests entirely on the verified issuer, so
    /// this pins it: the same provider-native subject under two issuers must
    /// canonicalize to two different PRISM principals.
    ///
    /// If someone "simplifies" the adapter by making Mirdyne reuse Supabase's
    /// configured issuer, this fails — which is the point. Merging the code is
    /// fine; merging the accounts is a security defect.
    #[test]
    fn mirdyne_and_supabase_are_separate_identity_domains() {
        let supabase = canonical_supabase_principal("https://acme.supabase.co/auth/v1", "user-1")
            .expect("canonical principal");
        let mirdyne = canonical_supabase_principal("https://auth.mirdyne.com/auth/v1", "user-1")
            .expect("canonical principal");
        assert_ne!(
            supabase, mirdyne,
            "identical subjects under different issuers must not collapse into one principal"
        );

        // The provider ids are distinct and round-trip through the registry,
        // so a stored credential can always name which domain minted it.
        assert_ne!(MIRDYNE_IDENTITY_PROVIDER, SUPABASE_IDENTITY_PROVIDER);
        assert_ne!(MIRDYNE_IDENTITY_PROVIDER, MARC27_IDENTITY_PROVIDER);
        for id in [
            MARC27_IDENTITY_PROVIDER,
            SUPABASE_IDENTITY_PROVIDER,
            MIRDYNE_IDENTITY_PROVIDER,
        ] {
            assert_eq!(
                identity_provider_for(Some(id))
                    .expect("registered")
                    .as_str(),
                id,
                "provider id must round-trip"
            );
        }
    }

    /// An unconfigured Mirdyne must not produce a usable verifier. A verifier
    /// without an issuer key cannot check `iss`, and one that cannot check
    /// `iss` would accept any provider's token as a Mirdyne identity.
    #[test]
    fn mirdyne_without_a_key_fails_closed() {
        let missing = IdentityVerifierConfig::new(
            Some(MIRDYNE_IDENTITY_PROVIDER),
            "https://auth.mirdyne.com",
            None,
        );
        assert!(missing.is_err(), "unconfigured Mirdyne must be refused");
        let blank = IdentityVerifierConfig::new(
            Some(MIRDYNE_IDENTITY_PROVIDER),
            "https://auth.mirdyne.com",
            Some("   "),
        );
        assert!(blank.is_err(), "a blank key is not a configured key");
        // …and an empty issuer URL is refused for the same reason.
        let no_url =
            IdentityVerifierConfig::new(Some(MIRDYNE_IDENTITY_PROVIDER), "  ", Some("anon-key"));
        assert!(no_url.is_err(), "Mirdyne without an issuer URL must fail");
    }

    #[test]
    fn supabase_principal_is_deterministic_and_project_scoped() {
        let first =
            canonical_supabase_principal("https://first.supabase.co/auth/v1", "subject-123")
                .expect("canonical principal");
        let equivalent =
            canonical_supabase_principal("https://first.supabase.co/auth/v1/", "subject-123")
                .expect("canonical principal");
        let second =
            canonical_supabase_principal("https://second.supabase.co/auth/v1", "subject-123")
                .expect("canonical principal");

        assert_eq!(first, equivalent);
        assert_ne!(first, second);
        assert!(first.starts_with("supabase:"));
        assert!(!first.contains("https://"));
    }

    #[test]
    fn verifier_configuration_fails_closed_and_redacts_provider_key() {
        let config = IdentityVerifierConfig::new(
            Some(SUPABASE_IDENTITY_PROVIDER),
            "https://project.supabase.co",
            Some("public-but-sensitive-anon-key"),
        )
        .expect("valid Supabase verifier");
        let debug = format!("{config:?}");
        assert!(!debug.contains("public-but-sensitive-anon-key"));
        assert!(debug.contains("[REDACTED]"));

        let missing = IdentityVerifierConfig::new(None, "https://provider.invalid", None)
            .expect_err("missing provider must fail closed")
            .to_string();
        assert!(
            missing.contains("identity provider is missing"),
            "{missing}"
        );

        let unknown = IdentityVerifierConfig::new(
            Some("unknown"),
            "https://provider.invalid",
            Some("must-not-leak"),
        )
        .expect_err("unknown provider must fail closed")
        .to_string();
        assert!(unknown.contains("unknown identity provider"), "{unknown}");
        assert!(!unknown.contains("must-not-leak"), "{unknown}");
    }

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
