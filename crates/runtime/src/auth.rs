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
use crate::{PlatformEndpoints, PrismPaths, StoredCredentials};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformAuth {
    /// Stable provider API key, sent as `X-API-Key`.
    ApiKey(String),
    /// Rotating login/session credential, sent as a Bearer token.
    Bearer(String),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPlatformAuth {
    pub api_base: String,
    pub credential: PlatformAuth,
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
    api_base: &str,
    api_key: Option<&str>,
    token: Option<&str>,
    node_token: Option<&str>,
    stored: Option<&StoredCredentials>,
) -> Result<ResolvedPlatformAuth> {
    if let Some(value) = non_empty(api_key) {
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(api_base),
            credential: PlatformAuth::ApiKey(value.to_string()),
        });
    }

    if let Some(value) = non_empty(token).or_else(|| non_empty(node_token)) {
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(api_base),
            credential: classify_token(value),
        });
    }

    if let Some(credentials) = stored
        && let Some(value) = non_empty(Some(&credentials.access_token))
    {
        return Ok(ResolvedPlatformAuth {
            // Endpoint precedence is resolved before this pure credential
            // seam. Re-reading `credentials.platform_url` here would let a
            // stale login override an explicit PRISM_API_URL.
            api_base: normalize_api_base(api_base),
            credential: PlatformAuth::Bearer(value.to_string()),
        });
    }

    Err(AuthFailure::missing("this command").into())
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
    let (api_key, token) = match resolve_environment_credential() {
        Some(PlatformAuth::ApiKey(value)) => (Some(value), None),
        Some(PlatformAuth::Bearer(value)) => (None, Some(value)),
        None => (None, None),
    };
    let node_token = paths
        .and_then(PrismPaths::load_node_token)
        .map(|token| token.key);
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

    resolve_platform_auth(
        &endpoints.api_base,
        api_key.as_deref(),
        token.as_deref(),
        node_token.as_deref(),
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

    fn stored(token: &str) -> StoredCredentials {
        StoredCredentials {
            access_token: token.to_string(),
            platform_url: "https://stored.example".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn api_key_only_resolution_is_headless_and_uses_x_api_key() {
        let resolved = resolve_platform_auth(
            "https://api.example/api/v1",
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
            "https://api.example",
            Some("m27_test"),
            None,
            None,
            Some(&stored("jwt")),
        )
        .unwrap();
        assert!(resolved.credential.is_api_key());
    }

    #[test]
    fn explicitly_resolved_endpoint_precedes_stored_session_endpoint() {
        let resolved = resolve_platform_auth(
            "https://native.example/api/v1",
            None,
            None,
            None,
            Some(&stored("jwt")),
        )
        .unwrap();
        assert_eq!(resolved.api_base, "https://native.example/api/v1");
        assert_eq!(resolved.credential, PlatformAuth::Bearer("jwt".into()));
    }

    #[test]
    fn missing_credentials_are_actionable() {
        let error = resolve_platform_auth("https://api.example", None, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("export PRISM_API_KEY=<key>"));
        assert!(error.contains("prism login --token <PAT>"));
    }

    #[test]
    fn prism_api_key_is_provider_neutral() {
        let resolved = resolve_platform_auth(
            "https://provider.example",
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
