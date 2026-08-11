//! Shared platform-auth seam.
//!
//! Every PRISM surface that needs the hosted platform must use this module's
//! credential resolver. The seam is deliberately independent of the device
//! flow implementation so the future `marc27` CLI can replace that
//! implementation without changing callers.
//!
//! Contract:
//! - `PRISM_API_KEY` is the preferred headless credential and is sent as
//!   `X-API-Key`; `MARC27_API_KEY` remains a deprecated compatibility alias.
//!   For an explicitly selected Supabase identity provider, `PRISM_API_KEY`
//!   configures the public anon key and is not a platform credential.
//! - `PRISM_TOKEN` / `PRISM_API_TOKEN` and stored credentials are Bearer
//!   credentials; their `MARC27_*` spellings remain deprecated aliases.
//! - Stored credentials are read from `cli-state.json`, with the legacy SDK
//!   mirror as a compatibility fallback.
//! - Resolution never starts a device flow, opens a browser, reads stdin, or
//!   waits for approval. Missing credentials return an actionable failure.
//! - Interactive auth is allowed only for the CLI when an explicit flag or
//!   `PRISM_ALLOW_INTERACTIVE_AUTH=1` is present and both standard streams are
//!   TTYs. Agent, TUI, and IPC callers are always refused.

use std::env;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::platform_env::PlatformVar;
use crate::{MARC27_PROVIDER, PlatformEndpoints, PrismPaths, StoredCredentials, StoredNodeToken};

/// JSON-RPC error code used for a missing platform credential.
pub const AUTH_REQUIRED_RPC_CODE: i64 = -32001;
/// Stable machine-readable auth failure label.
pub const AUTH_REQUIRED_CODE: &str = "AUTH_REQUIRED";
/// Explicit opt-in for the retained human device flow.
pub const INTERACTIVE_AUTH_ENV: &str = "PRISM_ALLOW_INTERACTIVE_AUTH";
/// Stable refusal when no provider endpoint was explicitly configured.
pub const PLATFORM_NOT_CONFIGURED: &str =
    "No platform configured. Set PRISM_API_URL to the provider API endpoint.";

/// The surface requesting auth. Only the CLI may opt into interactive auth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSurface {
    Cli,
    AgentProtocol,
    Tui,
    Ipc,
}

impl AuthSurface {
    fn label(self) -> &'static str {
        match self {
            Self::Cli => "CLI",
            Self::AgentProtocol => "agent protocol",
            Self::Tui => "TUI",
            Self::Ipc => "IPC",
        }
    }
}

/// Credential type and its wire-header semantics.
#[derive(Clone, PartialEq, Eq)]
pub enum PlatformAuth {
    /// Stable provider API key, sent as `X-API-Key`.
    ApiKey(String),
    /// Rotating login/session credential, sent as a Bearer token.
    Bearer(String),
}

impl std::fmt::Debug for PlatformAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey(_) => f.debug_tuple("ApiKey").field(&"[REDACTED]").finish(),
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&"[REDACTED]").finish(),
        }
    }
}

impl PlatformAuth {
    /// Attach this credential to a request without exposing credential
    /// resolution to individual callers.
    pub fn apply(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::ApiKey(key) => request.header("X-API-Key", key),
            Self::Bearer(token) => request.header("Authorization", format!("Bearer {token}")),
        }
    }

    /// Return the secret for clients that must hand it to an existing typed
    /// client. Callers must never log the returned value.
    pub fn secret(&self) -> &str {
        match self {
            Self::ApiKey(value) | Self::Bearer(value) => value,
        }
    }

    /// Whether this is a stable, non-expiring API key.
    pub fn is_api_key(&self) -> bool {
        matches!(self, Self::ApiKey(_))
    }

    /// Classify a raw credential by shape: the frozen `m27_` prefix marks a
    /// stable API key (`X-API-Key`); anything else is a rotating session
    /// credential (`Bearer`).
    ///
    /// Public so callers that hold a credential but not a whole
    /// [`ResolvedPlatformAuth`] — the boot checks, for one — get the same
    /// answer as the resolver instead of re-deriving the prefix rule.
    pub fn classify(value: &str) -> Self {
        if value.starts_with("m27_") {
            Self::ApiKey(value.to_string())
        } else {
            Self::Bearer(value.to_string())
        }
    }
}

/// Resolve the process environment's platform credential as one typed family.
/// Every PRISM-native spelling is considered before any MARC27 alias, even
/// across key/token names, so endpoint and transport layers cannot disagree.
pub fn resolve_environment_credential() -> Option<PlatformAuth> {
    let (value, source) = PlatformVar::get_with_source_preferred_then_alias(&[
        PlatformVar::API_KEY,
        PlatformVar::TOKEN,
        PlatformVar::API_TOKEN,
    ])?;
    if source == PlatformVar::API_KEY.preferred || source == PlatformVar::API_KEY.alias {
        Some(PlatformAuth::ApiKey(value))
    } else {
        // Retain the raw-token compatibility rule for old installs that put a
        // frozen `m27_` API key in a token variable before typed surfaces.
        Some(PlatformAuth::classify(&value))
    }
}

/// Auth result returned by the seam.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedPlatformAuth {
    pub api_base: String,
    pub credential: PlatformAuth,
}

impl std::fmt::Debug for ResolvedPlatformAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedPlatformAuth")
            .field("api_base", &self.api_base)
            .field("credential", &self.credential)
            .finish()
    }
}

/// Structured, actionable auth failure for protocol and UI callers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthFailure {
    pub code: &'static str,
    pub action: &'static str,
    pub message: String,
}

impl AuthFailure {
    pub fn missing(surface: &str) -> Self {
        Self {
            code: AUTH_REQUIRED_CODE,
            action: "export PRISM_API_KEY=<key> or run `prism login --token <PAT>`",
            message: format!(
                "Authentication required for {surface}. Run `export PRISM_API_KEY=<key>` or `prism login --token <PAT>`. Interactive authentication is disabled by default."
            ),
        }
    }

    pub fn interactive(surface: AuthSurface) -> Self {
        Self {
            code: AUTH_REQUIRED_CODE,
            action: "export PRISM_API_KEY=<key> or run `prism login --token <PAT>`",
            message: format!(
                "Interactive authentication is unavailable from the {}. Run `export PRISM_API_KEY=<key>` or `prism login --token <PAT>`. The retained device flow requires `prism login --interactive-auth` from a TTY.",
                surface.label()
            ),
        }
    }

    pub fn as_anyhow(&self) -> anyhow::Error {
        anyhow::anyhow!(self.message.clone())
    }
}

impl std::fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AuthFailure {}

/// Resolve platform auth from explicit inputs. This pure seam is used by the
/// environment resolver and makes precedence testable without mutating the
/// process environment.
pub fn resolve_platform_auth(
    endpoints: &PlatformEndpoints,
    api_key: Option<&str>,
    token: Option<&str>,
    node_token: Option<&StoredNodeToken>,
    stored: Option<&StoredCredentials>,
) -> Result<ResolvedPlatformAuth> {
    if let Some(value) = non_empty(api_key) {
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(&endpoints.api_base),
            credential: PlatformAuth::ApiKey(value.to_string()),
        });
    }

    if let Some(value) = non_empty(token) {
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(&endpoints.api_base),
            credential: classify_token(value),
        });
    }

    if let Some(credential) = node_token
        .map(|token| stored_node_bearer_for_endpoints(endpoints, token))
        .transpose()?
        .flatten()
    {
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(&endpoints.api_base),
            credential,
        });
    }

    if let Some(credential) = stored
        .map(|credentials| stored_bearer_for_endpoints(endpoints, credentials))
        .transpose()?
        .flatten()
    {
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(&endpoints.api_base),
            credential,
        });
    }

    Err(AuthFailure::missing("this command").into())
}

/// Select a stored session only when it is still paired with the exact
/// provider and normalized platform URL recorded at login.
///
/// Environment/config endpoint overrides are valid for explicit API keys and
/// token variables. They are not authority to redirect a durable stored
/// bearer: doing so would disclose the user's access token to an unrelated
/// host before that host had to prove anything.
pub fn stored_bearer_for_endpoints(
    endpoints: &PlatformEndpoints,
    credentials: &StoredCredentials,
) -> Result<Option<PlatformAuth>> {
    let Some(access_token) = non_empty(Some(&credentials.access_token)) else {
        return Ok(None);
    };

    validate_stored_session_binding(endpoints, credentials)?;
    Ok(Some(PlatformAuth::Bearer(access_token.to_string())))
}

/// Validate the endpoint/provider half of a stored-session binding without
/// selecting or exposing its bearer. Refresh paths use this before contacting
/// an identity provider and again before returning the rotated access token.
pub fn validate_stored_session_binding(
    endpoints: &PlatformEndpoints,
    credentials: &StoredCredentials,
) -> Result<()> {
    let stored_provider = credentials
        .platform_provider
        .as_deref()
        .and_then(|value| non_empty(Some(value)));
    let selected_provider = endpoints
        .provider
        .as_deref()
        .and_then(|value| non_empty(Some(value)));
    if stored_provider != selected_provider {
        anyhow::bail!(
            "stored session binding refused: selected platform provider does not match the stored identity"
        );
    }

    let stored_url = non_empty(Some(&credentials.platform_url)).ok_or_else(|| {
        anyhow::anyhow!("stored session binding refused: stored platform URL is missing")
    })?;
    let selected_url = non_empty(Some(&endpoints.api_base)).ok_or_else(|| {
        anyhow::anyhow!("stored session binding refused: selected platform URL is missing")
    })?;
    let stored_api_base = PlatformEndpoints::from_url(stored_url).api_base;
    let selected_api_base = PlatformEndpoints::from_url(selected_url).api_base;
    if stored_api_base != selected_api_base {
        anyhow::bail!(
            "stored session binding refused: selected platform URL does not match the stored login"
        );
    }

    Ok(())
}

/// Select a durable node credential only for the endpoint/provider it was
/// minted against.
///
/// Legacy `m27_` files predate binding metadata. They remain usable only for
/// the canonical MARC27 endpoint inferred by that provider-specific key
/// shape; they are refused for every configured override.
pub fn stored_node_bearer_for_endpoints(
    endpoints: &PlatformEndpoints,
    token: &StoredNodeToken,
) -> Result<Option<PlatformAuth>> {
    let Some(key) = non_empty(Some(&token.key)) else {
        return Ok(None);
    };
    validate_stored_node_binding(endpoints, token)?;
    Ok(Some(PlatformAuth::classify(key)))
}

/// Validate a durable node credential's destination without exposing it.
pub fn validate_stored_node_binding(
    endpoints: &PlatformEndpoints,
    token: &StoredNodeToken,
) -> Result<()> {
    let selected_provider = endpoints
        .provider
        .as_deref()
        .and_then(|value| non_empty(Some(value)));
    let selected_api_base = PlatformEndpoints::from_url(&endpoints.api_base).api_base;

    let Some(stored_url) = non_empty(Some(&token.platform_url)) else {
        let legacy_marc27 = token.key.starts_with("m27_")
            && selected_provider == Some(MARC27_PROVIDER)
            && selected_api_base == PlatformEndpoints::marc27().api_base;
        if legacy_marc27 {
            return Ok(());
        }
        anyhow::bail!(
            "stored node credential binding refused: legacy token has no recorded platform URL"
        );
    };

    let stored_provider = token
        .platform_provider
        .as_deref()
        .and_then(|value| non_empty(Some(value)));
    if stored_provider != selected_provider {
        anyhow::bail!(
            "stored node credential binding refused: selected platform provider does not match"
        );
    }
    if PlatformEndpoints::from_url(stored_url).api_base != selected_api_base {
        anyhow::bail!(
            "stored node credential binding refused: selected platform URL does not match"
        );
    }
    Ok(())
}

/// Resolve auth using the PRISM-native environment variables, their frozen
/// compatibility aliases, and local state.
/// Resolution is local-only and always fails fast when no credential exists.
pub fn resolve_from_environment(
    paths: Option<&PrismPaths>,
    configured_api_base: Option<&str>,
) -> Result<ResolvedPlatformAuth> {
    resolve_from_environment_with_provider(paths, configured_api_base, None)
}

/// Provider-aware form of [`resolve_from_environment`]. The provider selects
/// an external adapter; it does not define PRISM's authorization model.
pub fn resolve_from_environment_with_provider(
    paths: Option<&PrismPaths>,
    configured_api_base: Option<&str>,
    configured_provider: Option<&str>,
) -> Result<ResolvedPlatformAuth> {
    let node_token = paths.and_then(PrismPaths::load_node_token);
    let stored = paths
        .and_then(|value| value.load_cli_state().ok())
        .and_then(|state| state.credentials)
        .or_else(load_legacy_sdk_credentials);
    let endpoints = match paths {
        Some(paths) => PlatformEndpoints::resolve_for_paths(
            configured_api_base,
            configured_provider,
            stored.as_ref(),
            paths,
        ),
        None => PlatformEndpoints::resolve_with_provider(
            configured_api_base,
            configured_provider,
            stored.as_ref(),
        ),
    }
    .ok_or_else(|| anyhow::anyhow!(PLATFORM_NOT_CONFIGURED))?;
    // Provider identity must be known before reading the credential family.
    // For Supabase, PRISM_API_KEY may be the project's public anon key; the
    // endpoint-owned resolver therefore excludes it while retaining explicit
    // token precedence and typed API-key handling for every other provider.
    let (api_key, token) = match endpoints.environment_credential() {
        Some(PlatformAuth::ApiKey(value)) => (Some(value), None),
        Some(PlatformAuth::Bearer(value)) => (None, Some(value)),
        None => (None, None),
    };

    resolve_platform_auth(
        &endpoints,
        api_key.as_deref(),
        token.as_deref(),
        node_token.as_ref(),
        stored.as_ref(),
    )
}

fn load_legacy_sdk_credentials() -> Option<StoredCredentials> {
    let path = legacy_sdk_credentials_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Guard the retained device-flow implementation. This function performs no
/// I/O besides the caller-supplied TTY facts and is therefore safe to call
/// before any auth/network work.
pub fn require_interactive_auth(
    surface: AuthSurface,
    explicit_flag: bool,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
) -> Result<()> {
    if !matches!(surface, AuthSurface::Cli)
        || (!explicit_flag && !interactive_auth_env_enabled())
        || !stdin_is_tty
        || !stdout_is_tty
    {
        return Err(AuthFailure::interactive(surface).into());
    }
    Ok(())
}

/// Return the standard missing-credential failure as a JSON object for an
/// agent/TUI/IPC protocol event.
pub fn auth_failure_json(failure: &AuthFailure) -> serde_json::Value {
    serde_json::json!({
        "code": failure.code,
        "action": failure.action,
        "message": failure.message,
    })
}

fn interactive_auth_env_enabled() -> bool {
    matches!(
        env::var(INTERACTIVE_AUTH_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn classify_token(value: &str) -> PlatformAuth {
    PlatformAuth::classify(value)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_api_base(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/v1")
    }
}

fn legacy_sdk_credentials_path() -> Result<PathBuf> {
    let home = env::var("HOME").map_err(|_| anyhow::anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".prism/credentials.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PlatformEnvironmentGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl PlatformEnvironmentGuard {
        fn clear() -> Self {
            let mut previous = Vec::new();
            unsafe {
                for variable in PlatformVar::ALL {
                    for name in [variable.preferred, variable.alias] {
                        previous.push((name, env::var_os(name)));
                        env::remove_var(name);
                    }
                }
            }
            Self(previous)
        }
    }

    impl Drop for PlatformEnvironmentGuard {
        fn drop(&mut self) {
            unsafe {
                for (name, value) in self.0.drain(..) {
                    match value {
                        Some(value) => env::set_var(name, value),
                        None => env::remove_var(name),
                    }
                }
            }
        }
    }

    fn stored(token: &str) -> StoredCredentials {
        StoredCredentials {
            access_token: token.to_string(),
            platform_url: "https://stored.example".to_string(),
            platform_provider: Some(crate::MARC27_PROVIDER.to_string()),
            ..Default::default()
        }
    }

    fn endpoints(url: &str, provider: Option<&str>) -> PlatformEndpoints {
        PlatformEndpoints::from_url_with_provider(url, provider.map(str::to_string))
    }

    #[test]
    fn platform_auth_debug_redacts_bearer_and_api_key_secrets() {
        for (credential, kind, secret) in [
            (
                PlatformAuth::ApiKey("api-key-secret-marker".into()),
                "ApiKey",
                "api-key-secret-marker",
            ),
            (
                PlatformAuth::Bearer("bearer-secret-marker".into()),
                "Bearer",
                "bearer-secret-marker",
            ),
        ] {
            let rendered = format!("{credential:?}");
            assert!(rendered.contains(kind), "{rendered}");
            assert!(rendered.contains("[REDACTED]"), "{rendered}");
            assert!(!rendered.contains(secret), "secret leaked: {rendered}");
        }
    }

    #[test]
    fn resolved_platform_auth_debug_redacts_every_nested_credential_family() {
        for (credential, kind, secret) in [
            (
                PlatformAuth::ApiKey("nested-api-key-secret-marker".into()),
                "ApiKey",
                "nested-api-key-secret-marker",
            ),
            (
                PlatformAuth::Bearer("nested-bearer-secret-marker".into()),
                "Bearer",
                "nested-bearer-secret-marker",
            ),
        ] {
            let resolved = ResolvedPlatformAuth {
                api_base: "https://provider.example/api/v1".into(),
                credential,
            };
            let rendered = format!("{resolved:?}");

            assert!(rendered.contains("https://provider.example/api/v1"));
            assert!(rendered.contains(kind), "{rendered}");
            assert!(rendered.contains("[REDACTED]"), "{rendered}");
            assert!(!rendered.contains(secret), "secret leaked: {rendered}");
        }
    }

    #[test]
    fn api_key_only_resolution_is_headless_and_uses_x_api_key() {
        let resolved = resolve_platform_auth(
            &endpoints("https://api.example/api/v1", None),
            Some("m27_test"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(resolved.api_base, "https://api.example/api/v1");
        assert_eq!(resolved.credential, PlatformAuth::ApiKey("m27_test".into()));
    }

    #[test]
    fn api_key_precedes_stored_session() {
        let resolved = resolve_platform_auth(
            &endpoints("https://api.example", None),
            Some("m27_test"),
            None,
            None,
            Some(&stored("jwt")),
        )
        .unwrap();
        assert!(resolved.credential.is_api_key());
    }

    #[test]
    fn unrelated_endpoint_is_refused_for_a_stored_session() {
        let error = resolve_platform_auth(
            &endpoints("https://native.example/api/v1", Some("marc27")),
            None,
            None,
            None,
            Some(&stored("jwt")),
        )
        .expect_err("a stored bearer must remain bound to its login endpoint")
        .to_string();
        assert!(error.contains("does not match"), "{error}");
        assert!(!error.contains("jwt"), "stored token leaked: {error}");
    }

    #[test]
    fn matching_endpoint_and_provider_select_the_stored_session() {
        let resolved = resolve_platform_auth(
            &endpoints("https://stored.example/api/v1", Some("marc27")),
            None,
            None,
            None,
            Some(&stored("jwt")),
        )
        .expect("the recorded endpoint may use its stored session");
        assert_eq!(resolved.api_base, "https://stored.example/api/v1");
        assert_eq!(resolved.credential, PlatformAuth::Bearer("jwt".into()));
    }

    #[test]
    fn provider_neutral_and_matching_unknown_sessions_preserve_pat_compatibility() {
        for provider in [None, Some("custom-idp")] {
            let credentials = StoredCredentials {
                access_token: "provider-pat".into(),
                platform_url: "https://provider.example".into(),
                platform_provider: provider.map(str::to_string),
                ..Default::default()
            };
            let selected = endpoints("https://provider.example/api/v1", provider);
            let auth = stored_bearer_for_endpoints(&selected, &credentials)
                .expect("equal provider options and URLs are a valid binding");
            assert_eq!(auth, Some(PlatformAuth::Bearer("provider-pat".into())));
        }
    }

    #[test]
    fn durable_node_key_is_refused_for_an_unrelated_endpoint() {
        let token = StoredNodeToken {
            key: "m27_node-secret-marker".into(),
            id: "node-key-id".into(),
            prefix: "m27_node".into(),
            platform_url: "https://stored.example".into(),
            platform_provider: Some("marc27".into()),
        };
        let error = stored_node_bearer_for_endpoints(
            &endpoints("https://unrelated.example", Some("marc27")),
            &token,
        )
        .expect_err("a durable node key must stay bound to its mint endpoint")
        .to_string();
        assert!(error.contains("does not match"), "{error}");
        assert!(
            !error.contains("node-secret-marker"),
            "secret leaked: {error}"
        );
    }

    #[test]
    fn legacy_node_key_is_usable_only_at_the_canonical_marc27_endpoint() {
        let token = StoredNodeToken {
            key: "m27_legacy-node".into(),
            ..Default::default()
        };
        assert!(
            stored_node_bearer_for_endpoints(&PlatformEndpoints::marc27(), &token)
                .unwrap()
                .is_some()
        );
        assert!(
            stored_node_bearer_for_endpoints(
                &endpoints("https://override.example", Some("marc27")),
                &token,
            )
            .is_err()
        );
    }

    #[test]
    fn missing_credentials_are_actionable() {
        let error = resolve_platform_auth(
            &endpoints("https://api.example", None),
            None,
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("export PRISM_API_KEY=<key>"));
        assert!(error.contains("prism login --token <PAT>"));
    }

    #[test]
    fn prism_api_key_is_provider_neutral() {
        let resolved = resolve_platform_auth(
            &endpoints("https://provider.example", None),
            Some("provider-defined-key-shape"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            resolved.credential,
            PlatformAuth::ApiKey("provider-defined-key-shape".into())
        );
    }

    #[test]
    fn supabase_anon_key_does_not_override_a_verified_stored_session() {
        let _lock = crate::tests::ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _environment = PlatformEnvironmentGuard::clear();
        let directory = tempfile::tempdir().unwrap();
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        let credentials = StoredCredentials {
            access_token: "verified-user-session".into(),
            platform_url: "https://project.supabase.co".into(),
            platform_provider: Some(crate::SUPABASE_PROVIDER.into()),
            identity_provider_url: Some("https://project.supabase.co".into()),
            identity_provider_key: Some("public-anon-key".into()),
            ..Default::default()
        };
        paths
            .save_cli_state(&crate::PrismCliState {
                credentials: Some(credentials),
                ..Default::default()
            })
            .unwrap();
        unsafe {
            env::set_var("PRISM_API_URL", "https://project.supabase.co");
            env::set_var("PRISM_API_KEY", "public-anon-key");
        }

        let resolved = resolve_from_environment_with_provider(Some(&paths), None, None).unwrap();
        assert_eq!(
            resolved.credential,
            PlatformAuth::Bearer("verified-user-session".into())
        );

        unsafe {
            env::set_var("PRISM_TOKEN", "explicit-user-session");
        }
        let resolved = resolve_from_environment_with_provider(Some(&paths), None, None).unwrap();
        assert_eq!(
            resolved.credential,
            PlatformAuth::Bearer("explicit-user-session".into())
        );
    }

    #[test]
    fn unrelated_environment_url_cannot_select_a_stored_supabase_bearer() {
        let _lock = crate::tests::ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _environment = PlatformEnvironmentGuard::clear();
        let directory = tempfile::tempdir().unwrap();
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        let credentials = StoredCredentials {
            access_token: "stored-supabase-access-secret".into(),
            platform_url: "https://trusted.example".into(),
            platform_provider: Some(crate::SUPABASE_PROVIDER.into()),
            identity_provider_url: Some("https://identity.supabase.co".into()),
            identity_provider_key: Some("public-anon-key".into()),
            ..Default::default()
        };
        paths
            .save_cli_state(&crate::PrismCliState {
                credentials: Some(credentials),
                ..Default::default()
            })
            .unwrap();
        unsafe {
            env::set_var("PRISM_API_URL", "https://unrelated.example");
            env::set_var("PRISM_PLATFORM_PROVIDER", "supabase");
            env::set_var("PRISM_API_KEY", "public-anon-key");
        }

        let error = resolve_from_environment_with_provider(Some(&paths), None, None)
            .expect_err("an endpoint override cannot redirect a stored bearer")
            .to_string();
        assert!(error.contains("stored session binding refused"), "{error}");
        assert!(error.contains("does not match"), "{error}");
        assert!(
            !error.contains("stored-supabase-access-secret"),
            "token leaked: {error}"
        );

        unsafe { env::set_var("PRISM_TOKEN", "explicit-override-token") };
        let resolved = resolve_from_environment_with_provider(Some(&paths), None, None)
            .expect("an explicit token may target an explicit endpoint");
        assert_eq!(resolved.api_base, "https://unrelated.example/api/v1");
        assert_eq!(
            resolved.credential,
            PlatformAuth::Bearer("explicit-override-token".into())
        );
    }

    #[test]
    fn provider_aware_resolution_keeps_non_supabase_api_key_typing() {
        let _lock = crate::tests::ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _environment = PlatformEnvironmentGuard::clear();
        let directory = tempfile::tempdir().unwrap();
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        paths
            .save_cli_state(&crate::PrismCliState {
                credentials: Some(stored("stored-session")),
                ..Default::default()
            })
            .unwrap();
        unsafe {
            env::set_var("PRISM_API_URL", "https://independent.example");
            env::set_var("PRISM_API_KEY", "provider-defined-key-shape");
        }

        let resolved = resolve_from_environment_with_provider(Some(&paths), None, None).unwrap();
        assert_eq!(
            resolved.credential,
            PlatformAuth::ApiKey("provider-defined-key-shape".into())
        );
    }

    #[test]
    fn non_tty_interactive_auth_is_rejected_without_browser_or_stdin() {
        let error = require_interactive_auth(AuthSurface::Cli, true, false, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("prism login --token <PAT>"));
        assert!(error.contains("TTY"));
    }

    #[test]
    fn agent_protocol_cannot_enable_interactive_auth_even_with_opt_in() {
        assert!(require_interactive_auth(AuthSurface::AgentProtocol, true, true, true).is_err());
        assert!(require_interactive_auth(AuthSurface::Ipc, true, true, true).is_err());
    }

    #[test]
    fn normalizes_platform_api_base() {
        assert_eq!(
            normalize_api_base("https://api.example/"),
            "https://api.example/api/v1"
        );
        assert_eq!(
            normalize_api_base("https://api.example/api/v1"),
            "https://api.example/api/v1"
        );
    }
}
