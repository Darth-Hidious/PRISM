//! Shared platform-auth seam.
//!
//! Every PRISM surface that needs the hosted platform must use this module's
//! credential resolver. The seam is deliberately independent of the device
//! flow implementation so the future `marc27` CLI can replace that
//! implementation without changing callers.
//!
//! Contract:
//! - `MARC27_API_KEY` is the preferred headless credential and is sent as
//!   `X-API-Key`; valid keys retain the frozen `m27_` prefix.
//! - `MARC27_TOKEN` / `MARC27_API_TOKEN` and stored credentials are Bearer
//!   credentials.
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
use crate::{PrismPaths, StoredCredentials};

/// JSON-RPC error code used for a missing platform credential.
pub const AUTH_REQUIRED_RPC_CODE: i64 = -32001;
/// Stable machine-readable auth failure label.
pub const AUTH_REQUIRED_CODE: &str = "AUTH_REQUIRED";
/// Explicit opt-in for the retained human device flow.
pub const INTERACTIVE_AUTH_ENV: &str = "PRISM_ALLOW_INTERACTIVE_AUTH";

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
    /// Stable MARC27 API key, sent as `X-API-Key`.
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
            action: "export MARC27_API_KEY=m27_your_key_here or run `prism login --token <PAT>`",
            message: format!(
                "Authentication required for {surface}. Run `export MARC27_API_KEY=m27_your_key_here` or `prism login --token <PAT>`. Interactive authentication is disabled by default."
            ),
        }
    }

    pub fn interactive(surface: AuthSurface) -> Self {
        Self {
            code: AUTH_REQUIRED_CODE,
            action: "export MARC27_API_KEY=m27_your_key_here or run `prism login --token <PAT>`",
            message: format!(
                "Interactive authentication is unavailable from the {}. Run `export MARC27_API_KEY=m27_your_key_here` or `prism login --token <PAT>`. The retained device flow requires `prism login --interactive-auth` from a TTY.",
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
        if !value.starts_with("m27_") {
            return Err(anyhow::anyhow!(
                "MARC27_API_KEY must use the frozen m27_ key prefix; run `export MARC27_API_KEY=m27_your_key_here` or `prism login --token <PAT>`"
            ));
        }
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
        let stored_base = if credentials.platform_url.trim().is_empty() {
            api_base
        } else {
            credentials.platform_url.as_str()
        };
        return Ok(ResolvedPlatformAuth {
            api_base: normalize_api_base(stored_base),
            credential: PlatformAuth::Bearer(value.to_string()),
        });
    }

    Err(AuthFailure::missing("this command").into())
}

/// Resolve auth using the frozen environment variables and local state.
/// Resolution is local-only and always fails fast when no credential exists.
pub fn resolve_from_environment(
    paths: Option<&PrismPaths>,
    default_api_base: &str,
) -> Result<ResolvedPlatformAuth> {
    let api_base = PlatformVar::API_URL
        .get()
        .unwrap_or_else(|| default_api_base.to_string());
    let api_key = PlatformVar::API_KEY.get();
    let token = PlatformVar::TOKEN
        .get()
        .or_else(|| PlatformVar::API_TOKEN.get());
    let node_token = paths
        .and_then(PrismPaths::load_node_token)
        .map(|token| token.key);
    let stored = paths.and_then(|value| value.load_cli_state().ok());

    if let Some(stored) = stored.as_ref()
        && stored.credentials.is_some()
    {
        return resolve_platform_auth(
            &api_base,
            api_key.as_deref(),
            token.as_deref(),
            node_token.as_deref(),
            stored.credentials.as_ref(),
        );
    }

    if let Ok(path) = legacy_sdk_credentials_path()
        && let Ok(text) = std::fs::read_to_string(path)
        && let Ok(credentials) = serde_json::from_str::<StoredCredentials>(&text)
    {
        return resolve_platform_auth(
            &api_base,
            api_key.as_deref(),
            token.as_deref(),
            node_token.as_deref(),
            Some(&credentials),
        );
    }

    resolve_platform_auth(
        &api_base,
        api_key.as_deref(),
        token.as_deref(),
        node_token.as_deref(),
        None,
    )
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
    fn missing_credentials_are_actionable() {
        let error = resolve_platform_auth("https://api.example", None, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("export MARC27_API_KEY=m27_your_key_here"));
        assert!(error.contains("prism login --token <PAT>"));
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
