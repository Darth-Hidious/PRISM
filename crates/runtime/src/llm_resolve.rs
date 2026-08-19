// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Shared LLM-endpoint + tool-server resolution for NATIVE frontends
//! (TUI native backend, PRISM Desktop). Mirrors the CLI `backend` arm's
//! precedence (CLI flags aside): `~/.prism/config.toml [chat]` target >
//! prism.toml `[llm]` > env overrides, with the same credential policy
//! per target (provider keys never on the marc27 arm, JWT never exported
//! as MARC27_API_KEY).
//!
//! Model limits from the platform catalog are NOT resolved here — the
//! catalog fetch is fail-open in the CLI and native frontends run with
//! `context_window = None` (turn-count compaction), exactly the CLI's
//! offline behavior.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use prism_core::{chat_config, config as core_config, providers};

use crate::auth::PlatformAuth;
use crate::platform_env::PlatformVar;
use crate::{PlatformEndpoints, PrismPaths};

/// The built-in local default, refused for cloud targets when not
/// signed in (see [`resolve_unauth_llm_url`]).
pub const DEFAULT_LLM_URL: &str = "http://localhost:8080";

/// Outcome of [`resolve_llm`]: everything a frontend needs to build an
/// `LlmConfig` without re-implementing target policy.
#[derive(Clone)]
pub struct ResolvedLlm {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Explicit platform credential semantics. `None` is reserved for raw
    /// provider/legacy inputs whose caller did not declare a kind.
    pub credential_kind: Option<ResolvedCredentialKind>,
    pub embedding_model: Option<String>,
    /// Always `None` from this resolver (no catalog fetch); frontends
    /// fall back to turn-count compaction, same as the offline CLI.
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// Whether the resolved endpoint serves SSE streaming, per the provider
    /// registry. Resolved HERE because every frontend that built its own
    /// `LlmConfig` with `..Default::default()` silently got `true`, and a
    /// provider that answers `stream: true` with 200 and then sends nothing
    /// (mlx-lm) hangs the caller forever. One answer, at the point the
    /// endpoint is chosen.
    pub streaming: bool,
}

impl std::fmt::Debug for ResolvedLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedLlm")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("credential_kind", &self.credential_kind)
            .field("embedding_model", &self.embedding_model)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("streaming", &self.streaming)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedCredentialKind {
    ApiKey,
    Bearer,
}

fn resolved_platform_credential(
    raw_override: Option<String>,
    api_key: Option<String>,
    token: Option<String>,
    stored_token: Option<String>,
) -> (Option<String>, Option<ResolvedCredentialKind>) {
    if let Some(value) = raw_override {
        return (Some(value), None);
    }
    if let Some(value) = api_key {
        return (Some(value), Some(ResolvedCredentialKind::ApiKey));
    }
    if let Some(value) = token.or(stored_token) {
        return (Some(value), Some(ResolvedCredentialKind::Bearer));
    }
    (None, None)
}

/// `{api_base}/projects/{id}/llm` — the MARC27 native LLM proxy endpoint.
pub fn marc27_llm_url_for_project(api_base: &str, project_id: &str) -> String {
    format!(
        "{}/projects/{}/llm",
        api_base.trim_end_matches('/'),
        project_id
    )
}

/// Unauthenticated-case policy: an explicitly-set `[llm].url` is a
/// deliberate local-mode choice and is honored; the untouched default is
/// refused with an honest error rather than silently pointing "cloud"
/// chat at localhost.
pub fn resolve_unauth_llm_url(fallback_url: &str) -> Result<String> {
    if fallback_url != DEFAULT_LLM_URL {
        return Ok(fallback_url.to_string());
    }
    anyhow::bail!(
        "Not signed in and no LLM endpoint configured. Sign in to use the hosted \
         platform (run sign-in from the palette), or set `[llm].url` in prism.toml \
         (or LLM_BASE_URL) to use a local model explicitly."
    )
}

/// LLM_BASE_URL env → signed-in project `/llm` endpoint → explicit
/// `[llm].url` (refusing the localhost default).
pub fn marc27_llm_base_url(
    paths: &PrismPaths,
    api_base: &str,
    fallback_url: &str,
) -> Result<String> {
    marc27_llm_base_url_with_source(paths, api_base, fallback_url).map(|(url, _)| url)
}

/// Resolve the hosted target URL together with whether it was derived from
/// the bound platform endpoint. Stored platform bearers are permitted only
/// when the second value is `true`.
fn marc27_llm_base_url_with_source(
    paths: &PrismPaths,
    api_base: &str,
    fallback_url: &str,
) -> Result<(String, bool)> {
    if let Ok(explicit) = std::env::var("LLM_BASE_URL") {
        return Ok((explicit, false));
    }
    if let Some(project_id) = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .and_then(|c| c.project_id)
    {
        return Ok((marc27_llm_url_for_project(api_base, &project_id), true));
    }
    resolve_unauth_llm_url(fallback_url).map(|url| (url, false))
}

pub fn provider_endpoint(registry: &providers::Registry, provider: &str) -> String {
    providers::base_url_for(registry, provider)
        .unwrap_or_else(|| providers::legacy_guess_base_url(provider))
}

/// Resolve the LLM endpoint + credential for a native frontend session,
/// with the same per-target policy as the CLI backend.
pub fn resolve_llm(project_root: &Path, paths: &PrismPaths) -> Result<ResolvedLlm> {
    resolve_llm_with(project_root, paths, None)
}

/// Like [`resolve_llm`] but with an explicit target chosen by the user
/// (provider picker). `None` falls back to the persisted config — which
/// is only honored when the user actually chose one; a missing `[chat]`
/// table is NOT a choice and errors honestly instead of presetting the
/// hosted platform.
pub fn resolve_llm_with(
    project_root: &Path,
    paths: &PrismPaths,
    target: Option<chat_config::ChatTarget>,
) -> Result<ResolvedLlm> {
    let node_config = core_config::NodeConfig::load(Some(project_root));
    let cfg_llm = &node_config.llm;
    let chat_target = match target {
        Some(t) => t,
        None => {
            if !chat_config::chat_target_is_configured() {
                anyhow::bail!(
                    "No LLM provider selected. Choose a chat target with `prism use marc27 --model <model>` for the hosted platform or `prism use local --url <url> --model <model>` for a local model."
                );
            }
            chat_config::load().unwrap_or_default().chat
        }
    };
    let stored_credentials = paths
        .load_cli_state()
        .ok()
        .and_then(|state| state.credentials);
    let endpoints = PlatformEndpoints::resolve_for_paths(
        node_config.platform.url.as_deref(),
        node_config.platform.provider.as_deref(),
        stored_credentials.as_ref(),
        paths,
    );

    // Generic key chain for the local/direct-provider targets. Provider
    // keys belong ONLY here — never on the marc27 arm (a project `.env`
    // ANTHROPIC_API_KEY would otherwise shadow the platform JWT and 401
    // every platform LLM call).
    let api_key = std::env::var("LLM_API_KEY")
        .or_else(|_| {
            PlatformVar::get_preferred_then_alias(&[PlatformVar::TOKEN, PlatformVar::API_TOKEN])
                .ok_or(std::env::VarError::NotPresent)
        })
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .ok()
        .or_else(|| cfg_llm.resolve_api_key());

    let (base_url, model, api_key, credential_kind) = match &chat_target {
        chat_config::ChatTarget::Local {
            url,
            model,
            api_key: local_key,
        } => (
            url.clone(),
            model.clone(),
            local_key.clone().or(api_key),
            None,
        ),
        chat_config::ChatTarget::Provider {
            provider,
            model,
            api_key_env,
        } => {
            let registry = providers::Registry::load();
            let env_name = api_key_env
                .clone()
                .unwrap_or_else(|| providers::default_api_key_env(&registry, provider));
            let provider_key = std::env::var(&env_name).ok();
            (
                provider_endpoint(&registry, provider),
                model.clone(),
                provider_key.or(api_key),
                None,
            )
        }
        chat_config::ChatTarget::Marc27 {
            model: target_model,
        } => {
            let endpoints = endpoints
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!(crate::auth::PLATFORM_NOT_CONFIGURED))?;
            let (base_url, platform_derived) =
                marc27_llm_base_url_with_source(paths, &endpoints.api_base, &cfg_llm.url)?;
            let platform_token = if platform_derived {
                stored_credentials
                    .as_ref()
                    .map(|credentials| {
                        crate::auth::stored_bearer_for_endpoints(endpoints, credentials)
                    })
                    .transpose()?
                    .flatten()
                    .map(|credential| credential.secret().to_string())
            } else {
                None
            };
            // LLM_MODEL env → target model → [llm].model; with none, the
            // literal `default` alias — the platform resolves it
            // server-side. (No catalog fetch in native frontends.)
            let model = std::env::var("LLM_MODEL")
                .ok()
                .or_else(|| target_model.clone())
                .or_else(|| cfg_llm.model.clone())
                .unwrap_or_else(|| "default".to_string());
            // Explicit LLM_API_KEY → PRISM_API_KEY → PRISM_TOKEN → session
            // JWT. Historical MARC27 spellings remain deprecated aliases.
            let (environment_api_key, environment_token) = if platform_derived {
                match endpoints.environment_credential() {
                    Some(PlatformAuth::ApiKey(value)) => (Some(value), None),
                    Some(PlatformAuth::Bearer(value)) => (None, Some(value)),
                    None => (None, None),
                }
            } else {
                (None, None)
            };
            let raw_override = std::env::var("LLM_API_KEY").ok().or_else(|| {
                if platform_derived {
                    None
                } else {
                    cfg_llm.resolve_api_key()
                }
            });
            let (marc27_key, credential_kind) = resolved_platform_credential(
                raw_override,
                environment_api_key,
                environment_token,
                platform_token,
            );
            (base_url, model, marc27_key, credential_kind)
        }
    };

    let base_url_for_capabilities = base_url.clone();
    Ok(ResolvedLlm {
        base_url,
        model,
        api_key,
        credential_kind,
        embedding_model: cfg_llm.embedding_model.clone(),
        context_window: None,
        max_output_tokens: None,
        streaming: prism_core::providers::streams_for_url(
            &prism_core::providers::Registry::load(),
            &base_url_for_capabilities,
        ),
    })
}

/// Tool-server env with the same credential policy as the CLI backend:
/// the session JWT is NEVER exported as MARC27_API_KEY (the Python side
/// reads ~/.prism/credentials.json itself); a genuine user-set
/// MARC27_API_KEY still passes through normal env inheritance.
pub fn tool_server_env() -> BTreeMap<String, String> {
    let endpoints = PlatformEndpoints::from_env();
    tool_server_env_with_endpoints(endpoints.as_ref())
}

/// Build tool-server environment from the endpoint already resolved for the
/// native session. This keeps config/stored-login sessions from splitting:
/// hosted chat and Python tools always see the same provider endpoint.
pub fn tool_server_env_with_endpoints(
    endpoints: Option<&PlatformEndpoints>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PRISM_ENABLE_MCP".to_string(), "1".to_string());
    if let Some(endpoints) = endpoints {
        // Native name is authoritative. The historical spelling is exported
        // too so older Python sidecars keep working during the migration.
        env.insert("PRISM_API_URL".to_string(), endpoints.api_base.clone());
        env.insert("MARC27_API_URL".to_string(), endpoints.api_base.clone());
    }
    for key in &[
        "MP_API_KEY",
        "LENS_API_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "FIRECRAWL_API_KEY",
    ] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env
}

/// Convenience: the full native session inputs (resolved LLM + tool
/// server config) for a project root and python binary.
pub struct NativeSessionInputs {
    pub llm: ResolvedLlm,
    pub python_bin: PathBuf,
    pub project_root: PathBuf,
    pub env: BTreeMap<String, String>,
}

pub fn native_session_inputs(
    project_root: &Path,
    python_bin: PathBuf,
    paths: &PrismPaths,
) -> Result<NativeSessionInputs> {
    native_session_inputs_with(project_root, python_bin, paths, None)
}

/// With an explicit chat target (provider picker path).
pub fn native_session_inputs_with(
    project_root: &Path,
    python_bin: PathBuf,
    paths: &PrismPaths,
    target: Option<chat_config::ChatTarget>,
) -> Result<NativeSessionInputs> {
    let llm = resolve_llm_with(project_root, paths, target)?;
    let node_config = core_config::NodeConfig::load(Some(project_root));
    let stored_credentials = paths
        .load_cli_state()
        .ok()
        .and_then(|state| state.credentials);
    let endpoints = PlatformEndpoints::resolve_for_paths(
        node_config.platform.url.as_deref(),
        node_config.platform.provider.as_deref(),
        stored_credentials.as_ref(),
        paths,
    );
    Ok(NativeSessionInputs {
        llm,
        python_bin,
        project_root: project_root.to_path_buf(),
        env: tool_server_env_with_endpoints(endpoints.as_ref()),
    })
}

// ── Native session spawning ────────────────────────────────────────

/// Resolve the Python interpreter with the same managed-venv preference
/// as the CLI: PRISM_PYTHON → ~/.prism/venv/bin/python3 → `python3`.
pub fn resolve_python_bin() -> PathBuf {
    if let Ok(p) = std::env::var("PRISM_PYTHON") {
        return PathBuf::from(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        let venv = PathBuf::from(home)
            .join(".prism")
            .join("venv")
            .join("bin")
            .join("python3");
        if venv.exists() {
            return venv;
        }
    }
    PathBuf::from("python3")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PlatformEnvironmentGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl PlatformEnvironmentGuard {
        fn clear() -> Self {
            let names = [
                "PRISM_API_KEY",
                "PRISM_TOKEN",
                "PRISM_API_TOKEN",
                "MARC27_API_KEY",
                "MARC27_TOKEN",
                "MARC27_API_TOKEN",
                "PRISM_API_URL",
                "MARC27_API_URL",
                "PRISM_PLATFORM_URL",
                "MARC27_PLATFORM_URL",
                "PRISM_PLATFORM_PROVIDER",
                "MARC27_PLATFORM_PROVIDER",
                "LLM_API_KEY",
                "LLM_BASE_URL",
                "LLM_MODEL",
                "TEST_DIRECT_PROVIDER_KEY",
            ];
            let previous = names
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect::<Vec<_>>();
            for (name, _) in &previous {
                unsafe { std::env::remove_var(name) };
            }
            Self(previous)
        }
    }

    impl Drop for PlatformEnvironmentGuard {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    #[test]
    fn prism_api_key_preserves_api_key_wire_kind_without_prefix() {
        let (value, kind) = resolved_platform_credential(
            None,
            Some("provider-defined-key".into()),
            Some("shadowed-token".into()),
            Some("shadowed-session".into()),
        );
        assert_eq!(value.as_deref(), Some("provider-defined-key"));
        assert_eq!(kind, Some(ResolvedCredentialKind::ApiKey));
    }

    #[test]
    fn platform_token_preserves_bearer_kind_even_with_legacy_prefix() {
        let (value, kind) =
            resolved_platform_credential(None, None, Some("m27_session-shaped".into()), None);
        assert_eq!(value.as_deref(), Some("m27_session-shaped"));
        assert_eq!(kind, Some(ResolvedCredentialKind::Bearer));
    }

    #[test]
    fn raw_llm_override_remains_legacy_auto_classified() {
        let (value, kind) = resolved_platform_credential(
            Some("m27_old-raw-caller".into()),
            Some("shadowed-key".into()),
            None,
            None,
        );
        assert_eq!(value.as_deref(), Some("m27_old-raw-caller"));
        assert_eq!(kind, None);
    }

    #[test]
    fn resolved_llm_debug_redacts_every_api_key_family() {
        let resolved = ResolvedLlm {
            base_url: "https://provider.example/v1".to_string(),
            model: "model".to_string(),
            api_key: Some("llm-secret-must-not-leak".to_string()),
            credential_kind: Some(ResolvedCredentialKind::Bearer),
            embedding_model: None,
            context_window: None,
            max_output_tokens: None,
            streaming: true,
        };

        let debug = format!("{resolved:?}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
        assert!(!debug.contains("llm-secret-must-not-leak"), "{debug}");
    }

    #[test]
    fn local_and_direct_provider_targets_never_inherit_a_stored_platform_bearer() {
        let _lock = crate::tests::ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _environment = PlatformEnvironmentGuard::clear();
        let directory = tempfile::tempdir().expect("isolated runtime paths");
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        paths
            .save_cli_state(&crate::PrismCliState {
                credentials: Some(crate::StoredCredentials {
                    access_token: "stored-platform-bearer-must-not-leak".into(),
                    platform_url: "https://trusted.example".into(),
                    platform_provider: Some(crate::SUPABASE_PROVIDER.into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("store test login");
        unsafe {
            std::env::set_var("PRISM_API_URL", "https://unrelated.example");
            std::env::set_var("PRISM_PLATFORM_PROVIDER", "supabase");
        }

        let targets = [
            chat_config::ChatTarget::Local {
                url: "https://local-or-operator.example/v1".to_string(),
                model: "local-model".to_string(),
                api_key: None,
            },
            chat_config::ChatTarget::Provider {
                provider: "openai".to_string(),
                model: "direct-model".to_string(),
                api_key_env: Some("TEST_DIRECT_PROVIDER_KEY".to_string()),
            },
        ];
        for target in targets {
            let resolved = resolve_llm_with(directory.path(), &paths, Some(target))
                .expect("non-platform target resolution");
            assert_eq!(
                resolved.api_key, None,
                "stored platform bearer reached a non-platform LLM target"
            );
        }
    }

    #[test]
    fn hosted_target_never_sends_a_stored_bearer_to_an_explicit_llm_url() {
        let _lock = crate::tests::ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _environment = PlatformEnvironmentGuard::clear();
        let directory = tempfile::tempdir().expect("isolated runtime paths");
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        paths
            .save_cli_state(&crate::PrismCliState {
                credentials: Some(crate::StoredCredentials {
                    access_token: "stored-platform-bearer-must-not-leak".into(),
                    platform_url: "https://trusted.example".into(),
                    platform_provider: Some(crate::SUPABASE_PROVIDER.into()),
                    project_id: Some("project-123".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("store test login");
        unsafe {
            std::env::set_var("LLM_BASE_URL", "https://arbitrary-llm.example/v1");
        }

        let resolved = resolve_llm_with(
            directory.path(),
            &paths,
            Some(chat_config::ChatTarget::Marc27 { model: None }),
        )
        .expect("explicit LLM endpoint resolution");
        assert_eq!(resolved.base_url, "https://arbitrary-llm.example/v1");
        assert_eq!(resolved.api_key, None);

        unsafe { std::env::set_var("LLM_API_KEY", "explicit-llm-key") };
        let resolved = resolve_llm_with(
            directory.path(),
            &paths,
            Some(chat_config::ChatTarget::Marc27 { model: None }),
        )
        .expect("explicit LLM credential resolution");
        assert_eq!(resolved.api_key.as_deref(), Some("explicit-llm-key"));
        assert_eq!(resolved.credential_kind, None);
    }

    #[test]
    fn real_supabase_llm_resolution_never_uses_the_public_anon_key_as_bearer() {
        let _lock = crate::tests::ENV_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _environment = PlatformEnvironmentGuard::clear();
        let directory = tempfile::tempdir().expect("isolated runtime paths");
        let paths = PrismPaths {
            config_dir: directory.path().join("config"),
            cache_dir: directory.path().join("cache"),
            data_dir: directory.path().join("data"),
            state_dir: directory.path().join("state"),
        };
        paths
            .save_cli_state(&crate::PrismCliState {
                credentials: Some(crate::StoredCredentials {
                    access_token: "verified-user-session".into(),
                    platform_url: "https://project.supabase.co".into(),
                    platform_provider: Some(crate::SUPABASE_PROVIDER.into()),
                    identity_provider_url: Some("https://project.supabase.co".into()),
                    identity_provider_key: Some("public-anon-key".into()),
                    project_id: Some("project-123".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .expect("store test login");
        unsafe { std::env::set_var("PRISM_API_KEY", "public-anon-key") };

        let resolved = resolve_llm_with(
            directory.path(),
            &paths,
            Some(chat_config::ChatTarget::Marc27 { model: None }),
        )
        .expect("resolve hosted LLM through the real seam");
        assert_eq!(resolved.api_key.as_deref(), Some("verified-user-session"));
        assert_eq!(
            resolved.credential_kind,
            Some(ResolvedCredentialKind::Bearer)
        );

        unsafe { std::env::set_var("PRISM_TOKEN", "explicit-user-token") };
        let resolved = resolve_llm_with(
            directory.path(),
            &paths,
            Some(chat_config::ChatTarget::Marc27 { model: None }),
        )
        .expect("explicit user token remains valid");
        assert_eq!(resolved.api_key.as_deref(), Some("explicit-user-token"));
        assert_eq!(
            resolved.credential_kind,
            Some(ResolvedCredentialKind::Bearer)
        );
    }

    #[test]
    fn resolved_session_endpoint_is_forwarded_to_python_tools() {
        let endpoints = PlatformEndpoints::from_url("https://configured.example");
        let env = tool_server_env_with_endpoints(Some(&endpoints));
        assert_eq!(
            env.get("PRISM_API_URL").map(String::as_str),
            Some("https://configured.example/api/v1")
        );
        assert_eq!(
            env.get("MARC27_API_URL").map(String::as_str),
            Some("https://configured.example/api/v1")
        );
    }
}
