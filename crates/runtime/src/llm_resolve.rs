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

use crate::{PlatformEndpoints, PrismPaths};

/// The built-in local default, refused for cloud targets when not
/// signed in (see [`resolve_unauth_llm_url`]).
pub const DEFAULT_LLM_URL: &str = "http://localhost:8080";

/// Outcome of [`resolve_llm`]: everything a frontend needs to build an
/// `LlmConfig` without re-implementing target policy.
#[derive(Debug, Clone)]
pub struct ResolvedLlm {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub embedding_model: Option<String>,
    /// Always `None` from this resolver (no catalog fetch); frontends
    /// fall back to turn-count compaction, same as the offline CLI.
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
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
pub fn marc27_llm_base_url(paths: &PrismPaths, api_base: &str, fallback_url: &str) -> Result<String> {
    if let Ok(explicit) = std::env::var("LLM_BASE_URL") {
        return Ok(explicit);
    }
    if let Some(project_id) = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .and_then(|c| c.project_id)
    {
        return Ok(marc27_llm_url_for_project(api_base, &project_id));
    }
    resolve_unauth_llm_url(fallback_url)
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
                    "No LLM provider selected. PRISM is provider-neutral: choose the hosted                      platform, a local endpoint, or a direct provider (prism use / the app's                      picker)."
                );
            }
            chat_config::load().unwrap_or_default().chat
        }
    };
    let endpoints = PlatformEndpoints::from_env();

    // The session's platform JWT — the credential the MARC27 LLM proxy
    // authenticates.
    let platform_token = paths
        .load_cli_state()
        .ok()
        .and_then(|s| s.credentials)
        .map(|c| c.access_token);

    // Generic key chain for the local/direct-provider targets. Provider
    // keys belong ONLY here — never on the marc27 arm (a project `.env`
    // ANTHROPIC_API_KEY would otherwise shadow the platform JWT and 401
    // every platform LLM call).
    let api_key = std::env::var("LLM_API_KEY")
        .or_else(|_| std::env::var("MARC27_TOKEN"))
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .ok()
        .or_else(|| cfg_llm.resolve_api_key())
        .or_else(|| platform_token.clone());

    let (base_url, model, api_key) = match &chat_target {
        chat_config::ChatTarget::Local {
            url,
            model,
            api_key: local_key,
        } => (url.clone(), model.clone(), local_key.clone().or(api_key)),
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
            )
        }
        chat_config::ChatTarget::Marc27 {
            model: target_model,
        } => {
            // LLM_MODEL env → target model → [llm].model; with none, the
            // literal `default` alias — the platform resolves it
            // server-side. (No catalog fetch in native frontends.)
            let model = std::env::var("LLM_MODEL")
                .ok()
                .or_else(|| target_model.clone())
                .or_else(|| cfg_llm.model.clone())
                .unwrap_or_else(|| "default".to_string());
            // Explicit LLM_API_KEY → stable m27_* key → MARC27_TOKEN →
            // session JWT. Provider keys are NOT platform credentials.
            let marc27_key = std::env::var("LLM_API_KEY")
                .or_else(|_| std::env::var("MARC27_API_KEY"))
                .or_else(|_| std::env::var("MARC27_TOKEN"))
                .ok()
                .or_else(|| platform_token.clone());
            (
                marc27_llm_base_url(paths, &endpoints.api_base, &cfg_llm.url)?,
                model,
                marc27_key,
            )
        }
    };

    Ok(ResolvedLlm {
        base_url,
        model,
        api_key,
        embedding_model: cfg_llm.embedding_model.clone(),
        context_window: None,
        max_output_tokens: None,
    })
}

/// Tool-server env with the same credential policy as the CLI backend:
/// the session JWT is NEVER exported as MARC27_API_KEY (the Python side
/// reads ~/.prism/credentials.json itself); a genuine user-set
/// MARC27_API_KEY still passes through normal env inheritance.
pub fn tool_server_env() -> BTreeMap<String, String> {
    let endpoints = PlatformEndpoints::from_env();
    let mut env = BTreeMap::new();
    env.insert("PRISM_ENABLE_MCP".to_string(), "1".to_string());
    env.insert("MARC27_API_URL".to_string(), endpoints.api_base.clone());
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
    Ok(NativeSessionInputs {
        llm,
        python_bin,
        project_root: project_root.to_path_buf(),
        env: tool_server_env(),
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
        let venv = PathBuf::from(home).join(".prism").join("venv").join("bin").join("python3");
        if venv.exists() {
            return venv;
        }
    }
    PathBuf::from("python3")
}
