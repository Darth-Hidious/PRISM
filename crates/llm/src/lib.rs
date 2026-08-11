// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! LLM client — OpenAI-compatible + MARC27 platform proxy.
//!
//! Wire formats:
//! - OpenAI: `/v1/chat/completions`, `/v1/embeddings`
//! - MARC27: `/stream` (SSE), text-based tool calling
//!
//! Works with: llama.cpp, Ollama, vLLM, LiteLLM, OpenAI, Anthropic,
//! MARC27 platform, and any OpenAI-compatible endpoint.

use anyhow::{Context, Result, bail};
use prism_runtime::retry;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

mod local;
mod minja;
mod model_artifact;
pub use local::{LOCAL_GGUF_URL, default_model_dir, is_local_gguf_url, resolve_model_path};
pub use minja::render as render_minja_template;
pub use model_artifact::{BUNDLED_GEMMA, ModelArtifactManifest, sha256_hex, verify_model_artifact};

/// Canonical text and identity produced by the embedded GGUF's own template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedLocalPrompt {
    pub text: String,
    pub token_count: u64,
    pub template_sha256: String,
}

/// Why prompt-intervention scoring is unavailable without attempting a
/// different backend or acquiring a model implicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalPromptInfluenceUnavailableCode {
    /// Hosted providers own their final prompt template and logits, so the
    /// inference-context intervention cannot be observed locally.
    HostedBackend,
    /// The caller selected `gguf://local`, but this binary has no embedded
    /// llama.cpp support.
    LocalInferenceFeatureDisabled,
    /// This target cannot make llama.cpp load through a stable descriptor path,
    /// so a digest cannot be proven to describe the bytes actually loaded.
    DescriptorBackedIdentityUnavailable,
}

/// Content identity of the exact GGUF file bound to one local client.
///
/// On supported Unix targets, this receipt is computed from the descriptor
/// retained during model initialization. It is never inferred from a
/// configured path or model name. Targets without descriptor-backed loading
/// return an explicit unavailable outcome instead of this type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalModelIdentity {
    pub sha256: String,
    pub size_bytes: u64,
}

/// Result of requesting the identity of this client's loaded local model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LocalModelIdentityOutcome {
    Verified {
        identity: LocalModelIdentity,
    },
    Unavailable {
        code: LocalPromptInfluenceUnavailableCode,
        detail: String,
    },
}

/// Result of requesting local prompt-intervention scores.
///
/// The tagged status prevents a hosted or feature-disabled run from being
/// mistaken for a successfully primed local run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LocalPromptInfluenceOutcome {
    Scored {
        report: LocalPromptInfluenceReport,
    },
    Unavailable {
        code: LocalPromptInfluenceUnavailableCode,
        detail: String,
    },
}

/// Influence scores for one baseline prompt and an ordered candidate list.
///
/// Each candidate is appended to the supplied baseline tools and evaluated in
/// its own fresh KV context. The divergence is measured over the complete
/// next-token distribution before grammar filtering or sampling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalPromptInfluenceReport {
    /// SHA-256 of the exact retained file handle used to initialize the model
    /// that produced these logits.
    pub model_sha256: String,
    pub model_size_bytes: u64,
    pub template_sha256: String,
    pub baseline_prompt_tokens: u64,
    pub baseline_scoring_wall_time_micros: u64,
    pub total_scoring_wall_time_micros: u64,
    pub candidates: Vec<LocalToolInfluenceScore>,
}

/// Causal prompt-intervention score for one full tool definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalToolInfluenceScore {
    /// Stable position in the caller-provided candidate ordering.
    pub candidate_index: usize,
    pub tool_name: String,
    /// Jensen-Shannon divergence in natural-log units, bounded by `ln(2)`.
    pub raw_js_divergence_nats: f64,
    /// Raw divergence divided by the number of final GGUF prompt tokens added
    /// by this intervention. `None` means the template added no tokens (or
    /// produced a shorter prompt), so normalization would be misleading.
    pub normalized_js_divergence_per_added_prompt_token: Option<f64>,
    pub baseline_prompt_tokens: u64,
    pub candidate_prompt_tokens: u64,
    pub added_prompt_tokens: i64,
    /// Rendering, tokenization, fresh-context prefill, logits extraction, and
    /// divergence calculation for this candidate. Model loading is excluded.
    pub scoring_wall_time_micros: u64,
}

// ── Configuration ────────────────────────────────────────────────────

/// Wire semantics for a configured LLM credential.
///
/// `None` on [`LlmConfig::credential_kind`] is the compatibility mode for
/// callers that only supplied a raw string: the frozen `m27_` prefix is then
/// treated as an API key and every other value as a bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCredentialKind {
    ApiKey,
    Bearer,
}

/// Configuration for connecting to an LLM backend.
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Base URL of the LLM API.
    pub base_url: String,
    /// Model name (e.g. "gemma-3-27b", "gpt-4o", "claude-sonnet-4-6").
    pub model: String,
    /// API key for authenticated providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Explicit wire semantics for `api_key`. PRISM's native platform
    /// credential resolver sets this so provider-defined API-key shapes are
    /// never reclassified by their contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<LlmCredentialKind>,
    /// Separate embedding model. If not set, uses `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    /// Maximum sample rows for extraction prompts.
    #[serde(default = "default_max_sample_rows")]
    pub max_sample_rows: usize,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// The model's context window in tokens. Hosted values come from the
    /// platform catalog; an embedded GGUF client replaces them with the
    /// active context derived from model metadata. `None` means unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// The model's max output tokens, from the platform catalog. Used to
    /// reserve room for the response when budgeting input context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("credential_kind", &self.credential_kind)
            .field("embedding_model", &self.embedding_model)
            .field("max_sample_rows", &self.max_sample_rows)
            .field("timeout_secs", &self.timeout_secs)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
}

#[cfg(test)]
mod credential_debug_tests {
    use super::*;

    #[test]
    fn llm_config_debug_redacts_bearer_or_api_key() {
        let config = LlmConfig {
            base_url: "https://provider.example/api/v1/projects/p/llm".into(),
            model: "model".into(),
            api_key: Some("llm-access-secret-marker".into()),
            credential_kind: Some(LlmCredentialKind::Bearer),
            ..LlmConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        assert!(!rendered.contains("llm-access-secret-marker"));
    }
}

fn default_max_sample_rows() -> usize {
    10
}
fn default_timeout_secs() -> u64 {
    // 0 = no read deadline. Research runs are long by nature; the operator may
    // impose a deadline, PRISM does not impose one on them.
    0
}

/// Tokens kept free between the estimated prompt and the context window, so a
/// requested `max_tokens` can never overrun the input. Feeds the client-side
/// output clamp ([`LlmClient::effective_max_tokens`]).
const CONTEXT_MARGIN_TOKENS: u64 = 1024;

/// Sent as `max_tokens` when the operator has set no ceiling. Not a policy
/// limit — it is large enough to be irrelevant next to any real context
/// window, so the effective bound is `context_window - prompt - margin` and,
/// beyond that, the server's own clamp. Output is metered and billed per
/// token; counting is the control, not truncation.
const UNCAPPED_OUTPUT_TOKENS: u64 = 1_000_000;

impl Default for LlmConfig {
    fn default() -> Self {
        // These are fallback defaults only — real values come from prism.toml
        // or server config on login. Don't hardcode provider-specific values here.
        Self {
            base_url: String::new(), // Must be set from config
            model: String::new(),    // Must be set from config or server default
            api_key: None,
            credential_kind: None,
            embedding_model: None,
            max_sample_rows: 10,
            timeout_secs: 300,
            context_window: None,
            max_output_tokens: None,
        }
    }
}

// ── API-key hydration ────────────────────────────────────────────────

/// Hydrate provider API keys from `~/.prism/api_keys.json` (written by the
/// TUI's API-key window, 0600) into the process environment.
///
/// Env vars that are ALREADY set win — the file is a fallback, never an
/// override. Call this at process start (CLI) and again before switching
/// providers (backend), so a key saved mid-session takes effect without a
/// restart. Never fails: a missing or malformed file is a no-op.
pub fn hydrate_env_from_api_keys() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::PathBuf::from(home)
        .join(".prism")
        .join("api_keys.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw) else {
        return;
    };
    hydrate_env_from_map(&map);
    // Bridge the Google naming split: the TUI saves GOOGLE_API_KEY, while
    // some consumers default to GEMINI_API_KEY. Whichever exists serves both.
    for (have, want) in [
        ("GOOGLE_API_KEY", "GEMINI_API_KEY"),
        ("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    ] {
        if let Ok(v) = std::env::var(have)
            && !v.is_empty()
            && std::env::var_os(want).is_none()
        {
            // SAFETY: called at process start / in the backend's
            // single-threaded command loop, before any concurrent
            // env reads for these provider vars.
            unsafe { std::env::set_var(want, v) };
        }
    }
}

/// File-independent core of [`hydrate_env_from_api_keys`] (unit-testable).
fn hydrate_env_from_map(map: &serde_json::Map<String, serde_json::Value>) {
    for (name, value) in map {
        let Some(v) = value.as_str().filter(|v| !v.is_empty()) else {
            continue;
        };
        if std::env::var_os(name).is_none() {
            // SAFETY: see hydrate_env_from_api_keys — single-threaded
            // call sites only.
            unsafe { std::env::set_var(name, v) };
        }
    }
}

// ── Client ───────────────────────────────────────────────────────────

/// A message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallResponse>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Option<UsageInfo>,
    /// Exact embedded-GGUF generation phases. Hosted providers do not expose
    /// this split and therefore return `None` rather than an estimate.
    pub generation_metrics: Option<GenerationPhaseMetrics>,
}

/// Wall-clock split for one embedded-GGUF generation after the model is warm.
///
/// Prefill includes canonical template rendering, tokenization, grammar setup,
/// KV-context creation, and prompt evaluation. Decode begins at the first
/// greedy sample and ends after the final decoder flush. Model loading is
/// deliberately excluded from both phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationPhaseMetrics {
    pub prefill_wall_time_micros: u64,
    pub decode_wall_time_micros: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Assembles OpenAI-style streamed `tool_calls` deltas into whole calls.
///
/// Providers split one call across many chunks: the first carries `id` and
/// `function.name`, later ones append `function.arguments` fragments, all
/// keyed by `index`. BOTH streaming paths feed this — the OpenAI
/// `/v1/chat/completions` stream and the MARC27 `/stream` SSE (which forwards
/// the upstream provider's deltas verbatim on `tool_calls`). Sharing it is
/// what keeps a tool call assembled identically whichever backend answered;
/// the two paths having separate parsers is how the MARC27 path went years
/// without native tool calling at all.
#[derive(Default)]
struct ToolCallAccumulator {
    /// index -> (id, name, arguments-so-far)
    by_index: std::collections::HashMap<u32, (String, String, String)>,
}

impl ToolCallAccumulator {
    /// Fold one chunk's `tool_calls` array into the accumulator.
    fn push_deltas(&mut self, deltas: &[serde_json::Value]) {
        for tc in deltas {
            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let entry = self.by_index.entry(idx).or_default();
            // `id` / `name` may arrive on any chunk (not always the first) —
            // take the first non-empty value seen and never overwrite it.
            if entry.0.is_empty()
                && let Some(id) = tc.get("id").and_then(|i| i.as_str())
            {
                entry.0.push_str(id);
            }
            if entry.1.is_empty()
                && let Some(name) = tc.pointer("/function/name").and_then(|n| n.as_str())
            {
                entry.1.push_str(name);
            }
            if let Some(args) = tc.pointer("/function/arguments").and_then(|a| a.as_str()) {
                entry.2.push_str(args);
            }
        }
    }

    /// The assembled calls in `index` order, or `None` when the turn carried
    /// no native tool calls.
    ///
    /// An entry whose name never arrived is KEPT, not dropped: the dispatcher
    /// answers it with "unknown tool", which the model can see and recover
    /// from. Dropping it would turn a diagnosable error into an empty turn.
    fn finish(self) -> Option<Vec<ToolCallResponse>> {
        let mut calls: Vec<(u32, ToolCallResponse)> = self
            .by_index
            .into_iter()
            .map(|(idx, (id, name, args))| {
                (
                    idx,
                    ToolCallResponse {
                        id,
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name,
                            arguments: if args.is_empty() {
                                "{}".to_string()
                            } else {
                                args
                            },
                        },
                    },
                )
            })
            .collect();
        if calls.is_empty() {
            return None;
        }
        calls.sort_by_key(|(idx, _)| *idx);
        Some(calls.into_iter().map(|(_, tc)| tc).collect())
    }
}

/// Which adapter a configured client selects. Selection is exact and never
/// changes in response to an adapter failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Http,
    LocalGguf,
}

/// Pure adapter selection. Only the explicit `gguf://local` sentinel selects
/// embedded inference; every HTTP/provider URL remains on the existing path.
#[must_use]
pub fn choose_backend(base_url: &str) -> BackendChoice {
    if is_local_gguf_url(base_url) {
        BackendChoice::LocalGguf
    } else {
        BackendChoice::Http
    }
}

enum LlmBackend {
    Http(reqwest::Client),
    LocalGguf(local::LocalGguf),
}

/// Unified LLM client. HTTP and embedded GGUF adapters satisfy the same public
/// chat/streaming surface; unsupported local capabilities fail explicitly.
pub struct LlmClient {
    backend: LlmBackend,
    config: LlmConfig,
    /// Monotonic source of unique ids for local GGUF tool calls. A constant
    /// id (`local_call_0` for every call) corrupted multi-step sessions: with
    /// two pending results sharing one id, the prompt renderer could not tell
    /// which result belonged to which call, so earlier results were
    /// misattributed or dropped when the next turn was rendered.
    local_call_counter: std::sync::atomic::AtomicU64,
}

/// The chat-completions endpoint for an OpenAI-compatible base URL.
///
/// Appends `/chat/completions`, and nothing else. This used to synthesise a
/// `/v1` segment whenever the base did not already end in one, which broke
/// every vendor whose OpenAI-compatible surface is not mounted at `/v1`:
///
/// ```text
/// google, gemini   https://generativelanguage.googleapis.com/v1beta/openai
///                    → …/v1beta/openai/v1/chat/completions   404
/// zai              https://api.z.ai/api/paas/v4
///                    → …/api/paas/v4/v1/chat/completions     404
/// ```
///
/// Those are each vendor's documented endpoint, so the registry was right
/// and the guess was the bug — and because Google is offered as a headline
/// bring-your-own-key option in the onboarding wizard, it was a new user's
/// very first chat that 404'd.
///
/// A base URL is now taken at face value. Whoever supplied it — the shipped
/// `providers.toml`, `prism use local --url`, a `~/.prism/providers.toml`
/// gateway override — already said where the API lives, and a client that
/// edits that string can only be wrong in ways they cannot correct.
/// A hint for a 404 on the chat-completions URL, or nothing.
///
/// `chat_completions_url` takes a base URL at face value and appends only
/// `/chat/completions` — deliberately, see its doc comment: synthesising `/v1`
/// broke every vendor not mounted there. The cost of that correctness is that
/// a base URL missing its own `/v1` now 404s, and the raw upstream body for
/// that is Ollama's `404 page not found`, which names nothing.
///
/// Ollama is the commonest local setup and serves `/v1/chat/completions`, so
/// `--llm-url http://127.0.0.1:11434` fails and `.../v1` works. Measured
/// against a live daemon: `/v1/chat/completions` -> 400 (reached, bad body),
/// `/chat/completions` -> 404.
///
/// Only fires on 404, and only when the URL does not already carry a version
/// segment — so it cannot mislead someone whose base is correct and whose 404
/// is a wrong model or a dead route.
fn base_url_hint(url: &str, status: reqwest::StatusCode) -> String {
    if status != reqwest::StatusCode::NOT_FOUND {
        return String::new();
    }
    let base = url.trim_end_matches("/chat/completions");
    if base.contains("/v1") || base.contains("/v1beta") || base.contains("/v4") {
        return String::new();
    }
    format!(
        "\n  hint: {base} has no API version segment. Most OpenAI-compatible \
         servers — Ollama and llama.cpp included — mount at `/v1`, so the base \
         URL is usually `{base}/v1`. PRISM appends only `/chat/completions` and \
         never guesses a version, because vendors mount it at `/v1beta/openai` \
         and `/api/paas/v4` too."
    )
}

pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

impl LlmClient {
    pub fn new(mut config: LlmConfig) -> Self {
        let backend = match choose_backend(&config.base_url) {
            BackendChoice::Http => LlmBackend::Http({
                // No read deadline unless the operator sets one.
                //
                // PRISM is a materials-research harness, not a web service.
                // Extracting facts from a paper with a reasoning model takes
                // minutes; a 12B model on consumer hardware takes minutes on a
                // single CSV. A default deadline does not make the science
                // faster, it just fails the run partway through and throws the
                // work away — and it got worse the moment output stopped being
                // capped, because a model that thinks longer is now allowed to.
                //
                // The CONNECT timeout stays: refusing to hang on an endpoint
                // that is not there is different from refusing to wait for one
                // that is working.
                //
                // `timeout_secs = 0` means "no deadline" and is the default.
                let mut builder =
                    reqwest::Client::builder().connect_timeout(Duration::from_secs(30));
                if config.timeout_secs > 0 {
                    builder = builder.timeout(Duration::from_secs(config.timeout_secs));
                }
                builder.build().expect("failed to build HTTP client")
            }),
            BackendChoice::LocalGguf => {
                let local = local::LocalGguf::new(config.model.clone());
                if let Some(context_window) = local.context_window() {
                    // A registry fallback describes a model id, not the local
                    // runtime. GGUF metadata (plus an explicit smaller runtime
                    // cap) is authoritative for what this client can accept.
                    config.context_window = Some(context_window);
                }
                LlmBackend::LocalGguf(local)
            }
        };
        Self {
            backend,
            config,
            local_call_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The next unique tool-call id for this client's local session
    /// (`local_call_1`, `local_call_2`, …).
    fn next_local_call_id(counter: &std::sync::atomic::AtomicU64) -> String {
        format!(
            "local_call_{}",
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        )
    }

    fn local_backend(&self) -> Option<&local::LocalGguf> {
        match &self.backend {
            LlmBackend::LocalGguf(local) => Some(local),
            LlmBackend::Http(_) => None,
        }
    }

    fn http_client(&self) -> Result<&reqwest::Client> {
        match &self.backend {
            LlmBackend::Http(client) => Ok(client),
            LlmBackend::LocalGguf(_) => bail!(
                "internal adapter error: an HTTP operation was attempted for {LOCAL_GGUF_URL}; no remote request was sent"
            ),
        }
    }

    fn effective_local_max_tokens(&self, estimated_prompt_tokens: u64) -> u64 {
        let configured = self.config.max_output_tokens.unwrap_or(512);
        match self.config.context_window {
            Some(context) => configured.min(
                context
                    .saturating_sub(estimated_prompt_tokens)
                    .saturating_sub(CONTEXT_MARGIN_TOKENS),
            ),
            None => configured,
        }
    }

    fn local_tool_call_response(
        name: &str,
        arguments: &serde_json::Value,
        tools: &[ToolDefinition],
        model: &str,
        usage: UsageInfo,
        generation_metrics: GenerationPhaseMetrics,
        call_id: String,
    ) -> Result<ChatResponse> {
        let tool = tools.iter().find(|tool| tool.function.name == name).ok_or_else(|| {
            anyhow::anyhow!(
                "local GGUF model {model:?} requested unknown tool {name:?}; the call was rejected and no remote endpoint was tried."
            )
        })?;
        if !arguments.is_object() {
            bail!(
                "local GGUF model {model:?} returned non-object arguments for tool {name:?}; the call was rejected and no remote endpoint was tried."
            );
        }
        validate_json_schema(arguments, &tool.function.parameters, "$arguments").map_err(
            |error| {
                anyhow::anyhow!(
                    "local GGUF model {model:?} returned invalid arguments for tool {name:?}: {error}; the call was rejected and no remote endpoint was tried."
                )
            },
        )?;
        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCallResponse {
                    id: call_id,
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.to_string(),
                        arguments: serde_json::to_string(arguments)?,
                    },
                }]),
                tool_call_id: None,
            },
            usage: Some(usage),
            generation_metrics: Some(generation_metrics),
        })
    }

    fn local_chat_response(
        generation: local::LocalGeneration,
        tools: &[ToolDefinition],
        model: &str,
        has_tool_result: bool,
        counter: &std::sync::atomic::AtomicU64,
    ) -> Result<ChatResponse> {
        let usage = UsageInfo {
            prompt_tokens: generation.prompt_tokens,
            completion_tokens: generation.completion_tokens,
            total_tokens: generation.prompt_tokens + generation.completion_tokens,
        };
        let generation_metrics = GenerationPhaseMetrics {
            prefill_wall_time_micros: generation.prefill_wall_time_micros,
            decode_wall_time_micros: generation.decode_wall_time_micros,
        };
        if tools.is_empty() {
            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: (!generation.text.is_empty()).then_some(generation.text),
                    tool_calls: None,
                    tool_call_id: None,
                },
                usage: Some(usage),
                generation_metrics: Some(generation_metrics),
            });
        }

        let text = generation.text.trim();
        if text.starts_with("<start_function_call>")
            || text.starts_with("<|tool_call_start|>")
            || text.starts_with("<|tool_call>")
        {
            let (name, arguments) = parse_native_tool_call(text).map_err(|error| {
                anyhow::anyhow!(
                    "local GGUF model {model:?} produced an invalid embedded-template tool call {text:?}: {error}; the call was rejected and no remote endpoint was tried."
                )
            })?;
            return Self::local_tool_call_response(
                &name,
                &arguments,
                tools,
                model,
                usage,
                generation_metrics,
                Self::next_local_call_id(counter),
            );
        }
        if has_tool_result && !text.starts_with('{') {
            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(text.to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                usage: Some(usage),
                generation_metrics: Some(generation_metrics),
            });
        }
        let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
            anyhow::anyhow!(
                "local GGUF model {model:?} produced an invalid tool response {text:?}: {error}; expected one strict JSON object. No tool call was guessed and no remote endpoint was tried."
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            anyhow::anyhow!(
                "local GGUF model {model:?} produced a non-object tool response; no tool call was guessed and no remote endpoint was tried."
            )
        })?;
        let kind = object.get("kind").and_then(serde_json::Value::as_str);
        match kind {
            Some("final") => {
                if object.len() != 2 || !object.contains_key("content") {
                    bail!(
                        "local GGUF model {model:?} produced an invalid final response shape {text:?}; no tool call was guessed and no remote endpoint was tried."
                    );
                }
                let content = object
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "local GGUF model {model:?} produced non-string final content; no tool call was guessed and no remote endpoint was tried."
                        )
                    })?;
                Ok(ChatResponse {
                    message: ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(content.to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    usage: Some(usage),
                    generation_metrics: Some(generation_metrics),
                })
            }
            Some("tool_call") => {
                if object.len() != 3
                    || !object.contains_key("name")
                    || !object.contains_key("arguments")
                {
                    bail!(
                        "local GGUF model {model:?} produced an invalid tool-call shape {text:?}; no tool call was guessed and no remote endpoint was tried."
                    );
                }
                let name = object
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "local GGUF model {model:?} produced an invalid tool name; no tool call was guessed and no remote endpoint was tried."
                        )
                    })?;
                let arguments = object.get("arguments").ok_or_else(|| {
                    anyhow::anyhow!(
                        "local GGUF model {model:?} omitted arguments for tool {name:?}; the call was rejected and no remote endpoint was tried."
                    )
                })?;
                Self::local_tool_call_response(
                    name,
                    arguments,
                    tools,
                    model,
                    usage,
                    generation_metrics,
                    Self::next_local_call_id(counter),
                )
            }
            _ => bail!(
                "local GGUF model {model:?} produced an invalid tool-response kind; no tool call was guessed and no remote endpoint was tried."
            ),
        }
    }

    /// The configuration this client was built with. Lets callers derive a
    /// sibling client (same endpoint/credentials, different model) — e.g. the
    /// agent's `spawn_subagent`, which runs a nested turn on another model.
    #[must_use]
    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Generate text from a prompt.
    pub async fn generate(&self, prompt: &str) -> Result<String> {
        self.chat("You are a helpful assistant.", prompt).await
    }

    /// Whether this client targets the MARC27 platform LLM proxy
    /// (which uses `/stream` + SSE instead of OpenAI `/v1/chat/completions`).
    fn is_marc27(&self) -> bool {
        self.config.base_url.contains("marc27.com") || self.config.base_url.contains("/llm")
    }

    /// This client's chat-completions endpoint. See the free
    /// [`chat_completions_url`] for why it appends nothing but the path.
    fn chat_completions_url(&self) -> String {
        chat_completions_url(&self.config.base_url)
    }

    /// Extract the assistant's text from an OpenAI-compatible response.
    /// Falls back to `reasoning_content` when `content` is empty (e.g.
    /// Gemma 4 thinking mode puts all text in `reasoning_content`).
    fn extract_content(data: &serde_json::Value) -> String {
        let msg = &data["choices"][0]["message"];
        let content = msg["content"].as_str().unwrap_or_default();
        if !content.is_empty() {
            return content.to_string();
        }
        msg["reasoning_content"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Generate text with a system + user message.
    pub async fn chat(&self, system: &str, user: &str) -> Result<String> {
        let local_messages = [
            ChatMessage {
                role: "system".to_string(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some(user.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        if let Some(local) = self.local_backend() {
            let serialized = serde_json::to_value(&local_messages)?;
            let generation = local
                .generate_streaming(
                    &local_messages,
                    &[],
                    self.effective_local_max_tokens(Self::estimate_tokens(&serialized)),
                    |_| {},
                )
                .await?;
            return Ok(generation.text);
        }
        let messages = serde_json::json!([
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]);
        if self.is_marc27() {
            return self.chat_marc27_simple(&messages).await;
        }
        let url = self.chat_completions_url();
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
        });
        let body = self.with_operator_output_cap(body, Self::estimate_tokens(&messages));
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;
        Ok(Self::extract_content(&data))
    }

    /// Ask a vision model about an image.
    ///
    /// `image_png` is raw PNG bytes; they are base64'd into the OpenAI-style
    /// `image_url` content part that llama.cpp, vLLM and OpenAI all accept.
    ///
    /// The two paths that CANNOT carry an image refuse rather than quietly
    /// dropping it. That failure is not hypothetical: sending a page image to
    /// a text-only build returns a fluent, confident description of a
    /// document the model never saw — the caller has no way to tell that
    /// answer from a real one, and every fact extracted from it is fabricated.
    /// An error is recoverable; a hallucinated page is not.
    /// `max_output_tokens` bounds this ONE call. It is not an operator
    /// policy cap on what the model may say — it is the size of the thing
    /// being asked about. A caller transcribing a region of a page knows how
    /// much text can physically be printed there, and without that bound a
    /// model that falls into a repetition loop (small VLMs do, on repetitive
    /// imagery) generates until the context is exhausted: measured at over
    /// five minutes for a single tile of micrographs before the request even
    /// returned to be judged degenerate.
    pub async fn describe_image(
        &self,
        prompt: &str,
        image_png: &[u8],
        max_output_tokens: u64,
    ) -> Result<String> {
        if self.local_backend().is_some() {
            bail!(
                "the in-process local backend cannot accept images; point PRISM at a \
                 vision-capable server (a llama-server started with --mmproj, for \
                 example) to read pages with a model"
            );
        }
        if self.is_marc27() {
            bail!(
                "the hosted platform LLM endpoint does not accept images; configure a \
                 vision-capable endpoint for document reading"
            );
        }

        let body = self.vision_request_body(prompt, image_png, max_output_tokens);
        let resp = self.post(&self.chat_completions_url(), &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad vision response")?;
        Ok(Self::extract_content(&data))
    }

    /// Build the vision request. Separated from the send so the parts that
    /// are easy to get silently wrong — that the image is actually attached,
    /// and that the caller's output bound actually reaches the wire — are
    /// checkable without a server. Both have been wrong here before.
    fn vision_request_body(
        &self,
        prompt: &str,
        image_png: &[u8],
        max_output_tokens: u64,
    ) -> serde_json::Value {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(image_png);
        let messages = serde_json::json!([{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url",
                 "image_url": {"url": format!("data:image/png;base64,{encoded}")}},
            ],
        }]);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            // Deterministic: the same page must read the same way twice, or
            // two ingests of one document disagree about what it says.
            "temperature": 0.0,
        });
        // The base64 payload is ~4/3 of the image and would dominate a
        // token estimate, but it does NOT cost text tokens — a vision
        // encoder charges a fixed budget per image (256 on Gemma 4,
        // measured). Estimate from the prompt alone so the output cap is
        // not throttled by the size of the picture.
        let mut body =
            self.with_operator_output_cap(body, Self::estimate_tokens(&serde_json::json!(prompt)));
        // The caller's bound and the operator's cap both apply; the smaller
        // wins, so neither can silently widen the other.
        if let Some(object) = body.as_object_mut() {
            let bounded = object
                .get("max_tokens")
                .and_then(serde_json::Value::as_u64)
                .map_or(max_output_tokens, |cap| cap.min(max_output_tokens));
            object.insert("max_tokens".to_string(), serde_json::json!(bounded));
        }
        body
    }

    /// MARC27 platform LLM: POST /stream with SSE response.
    async fn chat_marc27_simple(&self, messages: &serde_json::Value) -> Result<String> {
        let url = format!("{}/stream", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            // Was previously omitted entirely, letting the platform generate
            // unbounded output (thousands of tokens observed) on every call —
            // real, billed credits with no cap. Send the same context-clamped
            // budget every other chat path uses.
        });
        let body = self.with_operator_output_cap(body, Self::estimate_tokens(messages));
        let resp = self.post(&url, &body).await?;
        let text = resp
            .text()
            .await
            .context("failed to read platform stream")?;
        let mut result = String::new();
        for line in text.lines() {
            let line = line.strip_prefix("data: ").unwrap_or(line).trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                    result.push_str(delta);
                }
                if chunk.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                    break;
                }
            }
        }
        if result.is_empty() {
            bail!("platform LLM returned empty response");
        }
        Ok(result)
    }

    /// Chat with tool-calling support, non-streaming.
    ///
    /// Sends full message history + tool definitions and returns a response
    /// that may contain tool_calls — on the OpenAI path. On the MARC27 path it
    /// drops `tools` (see below). Nothing in the workspace calls this today;
    /// the agent loop uses [`Self::chat_with_tools_streaming`], which sends
    /// tools on both.
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatResponse> {
        if let Some(local) = self.local_backend() {
            let messages_estimate = Self::estimate_tokens(&serde_json::to_value(messages)?);
            let tools_estimate = Self::estimate_tokens(&serde_json::to_value(tools)?);
            let generation = local
                .generate_streaming(
                    messages,
                    tools,
                    self.effective_local_max_tokens(messages_estimate + tools_estimate),
                    |_| {},
                )
                .await?;
            return Self::local_chat_response(
                generation,
                tools,
                &self.config.model,
                messages.iter().any(|message| message.role == "tool"),
                &self.local_call_counter,
            );
        }
        // MARC27 platform proxy: use /stream, collect text. This branch DROPS
        // `tools` — unlike `chat_with_tools_streaming`, which sends them
        // natively. Nothing in the workspace calls this method today; use the
        // streaming variant, which is the agent loop's only entry point.
        if self.is_marc27() {
            let msgs = serde_json::to_value(messages)?;
            let text = self.chat_marc27_simple(&msgs).await?;
            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                },
                usage: None,
                generation_metrics: None,
            });
        }
        let url = self.chat_completions_url();

        let est = Self::estimate_tokens(&serde_json::to_value(messages).unwrap_or_default())
            + Self::estimate_tokens(&serde_json::to_value(tools).unwrap_or_default());
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
        });
        let mut body = self.with_operator_output_cap(body, est);

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;

        let choice = &data["choices"][0];
        let msg_val = &choice["message"];

        let tool_calls: Option<Vec<ToolCallResponse>> = msg_val
            .get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok());

        let content_str = Self::extract_content(&data);
        let content = if content_str.is_empty() {
            None
        } else {
            Some(content_str)
        };

        let usage = data
            .get("usage")
            .and_then(|u| serde_json::from_value::<UsageInfo>(u.clone()).ok());

        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".to_string(),
                content,
                tool_calls,
                tool_call_id: None,
            },
            usage,
            generation_metrics: None,
        })
    }

    /// Token budget shared by every chat/completion request this client
    /// sends. Honors `LlmConfig::max_output_tokens` (fetched from the
    /// platform model catalog) instead of a fixed guess — a reasoning/
    /// "thinking" model can burn its entire budget on `reasoning_content`
    /// before writing any JSON or visible text, and a caller may know the
    /// model needs more (or less) room than a hardcoded 4096. Falls back to
    /// 4096 only when the config doesn't carry a value (e.g. local llama.cpp
    /// with no catalog entry).
    /// Attach `max_tokens` ONLY when the operator asked for a ceiling.
    ///
    /// PRISM sends no output limit of its own. There are millions of models
    /// and more arriving; deciding how many tokens any of them may emit is not
    /// PRISM's call. Output is metered and billed per token — on the platform
    /// side that is exactly how a user is charged against prepaid credits — so
    /// counting is the control. Truncating just breaks models that reason
    /// before answering and saves nobody anything.
    ///
    /// When `max_output_tokens` is unset the key is absent from the request
    /// and the server applies its own context-derived bound.
    fn with_operator_output_cap(
        &self,
        mut body: serde_json::Value,
        est_prompt_tokens: u64,
    ) -> serde_json::Value {
        if self.config.max_output_tokens.is_some()
            && let Some(object) = body.as_object_mut()
        {
            object.insert(
                "max_tokens".to_string(),
                serde_json::json!(self.effective_max_tokens(est_prompt_tokens)),
            );
        }
        body
    }

    fn effective_max_tokens(&self, est_prompt_tokens: u64) -> u64 {
        const FLOOR: u64 = 256;
        // PRISM does NOT cap output on the operator's behalf.
        //
        // The previous 4096 default was a cost guard, added after unbounded
        // platform output burned real credits. But output is METERED and
        // BILLED per token — counting it is the control, not truncating it.
        // Capping does not save anyone money; it just decides for the user,
        // and it silently breaks any model that reasons before it answers.
        // Gemma 4 12B spent ~2.7k tokens of `reasoning_content` against that
        // default, hit the ceiling, and returned no JSON at all. PRISM's bug,
        // not the model's — and the next model will reason more, not less.
        //
        // What genuinely bounds output: the CONTEXT WINDOW (enforced below and
        // again by the server), per-token metering, and the solvency check
        // that fails closed when credits run out. An explicit
        // `max_output_tokens` is still honoured — the operator may cap.
        let model_max = self
            .config
            .max_output_tokens
            .unwrap_or(UNCAPPED_OUTPUT_TOKENS);
        // Clamp the requested output so it can never collide with the input:
        // context_window − estimated prompt − margin. When the context window is
        // unknown, only the configured max applies. Embedded local models now
        // carry their GGUF-derived context but still have no catalog
        // max_output_tokens, so their output ceiling naturally remains 4096 — the
        // same cap the compact profile would select.
        let by_context = match self.config.context_window {
            Some(cw) => cw
                .saturating_sub(est_prompt_tokens)
                .saturating_sub(CONTEXT_MARGIN_TOKENS),
            None => model_max,
        };
        model_max.min(by_context).max(FLOOR)
    }

    /// Rough prompt-token estimate for a serialized request value (~4 chars per
    /// token). Only feeds the [`Self::effective_max_tokens`] safety clamp, so a
    /// slight under-count is harmless — the margin and the server-side clamp
    /// absorb the slack.
    fn estimate_tokens(value: &serde_json::Value) -> u64 {
        value.to_string().len() as u64 / 4
    }

    /// Extract strict JSON output from a chat-completions choice.
    ///
    /// Unlike [`Self::extract_content`] (used for human-readable chat, where
    /// falling back to `reasoning_content` is a reasonable best-effort),
    /// JSON extraction must NEVER return reasoning text: it is never valid
    /// JSON, so silently returning it only turns this into an opaque
    /// "invalid JSON" parse error one layer up. When `content` is empty we
    /// diagnose *why* and bail with an actionable message instead
    /// (AUDIT_BACKLOG #6 / INGESTION_AUDIT #6 — a thinking model hitting
    /// `finish_reason: "length"` while reasoning silently broke every
    /// extraction on that hardware).
    fn extract_json_content(choice: &serde_json::Value) -> Result<String> {
        let msg = &choice["message"];
        let content = msg["content"].as_str().unwrap_or_default();
        if !content.is_empty() {
            return Ok(content.to_string());
        }
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("unknown");
        let reasoning_len = msg["reasoning_content"].as_str().unwrap_or_default().len();
        if finish_reason == "length" && reasoning_len > 0 {
            bail!(
                "LLM produced no JSON output: it hit max_tokens before finishing, having \
                 spent the whole budget on {reasoning_len} chars of reasoning_content \
                 (finish_reason=length). This model is running in 'thinking' mode — raise \
                 max_output_tokens in the LLM config, or disable thinking for extraction \
                 requests if the backend supports it."
            );
        }
        bail!("LLM returned empty content for JSON extraction (finish_reason={finish_reason})");
    }

    /// Generate text and parse as JSON (uses response_format).
    pub async fn generate_json(&self, prompt: &str) -> Result<String> {
        self.generate_json_with_usage(prompt)
            .await
            .map(|(text, _usage)| text)
    }

    /// [`Self::generate_json`], also returning the provider-reported token
    /// usage when the wire carries one. Output is metered and billed per
    /// token — counting is the control — so callers that fan a document or
    /// dataset out over many extraction calls accumulate these to report
    /// what the run actually cost. `None` means the backend reported
    /// nothing (the MARC27 `/stream` path), never that usage was zero.
    pub async fn generate_json_with_usage(
        &self,
        prompt: &str,
    ) -> Result<(String, Option<UsageInfo>)> {
        if let Some(local) = self.local_backend() {
            let messages = [ChatMessage {
                role: "user".to_string(),
                content: Some(format!(
                    "Return one valid JSON object and no Markdown fences.\n\n{prompt}"
                )),
                tool_calls: None,
                tool_call_id: None,
            }];
            let serialized = serde_json::to_value(&messages)?;
            let generation = local
                .generate_streaming(
                    &messages,
                    &[],
                    self.effective_local_max_tokens(Self::estimate_tokens(&serialized)),
                    |_| {},
                )
                .await?;
            let usage = UsageInfo {
                prompt_tokens: generation.prompt_tokens,
                completion_tokens: generation.completion_tokens,
                total_tokens: generation.prompt_tokens + generation.completion_tokens,
            };
            return Ok((
                Self::strip_json_fences(&generation.text).to_string(),
                Some(usage),
            ));
        }
        // MARC27 platform: this method used to skip the is_marc27() branch
        // that chat()/chat_with_tools() have, so ingest ontology extraction
        // against a platform URL hit `{base}/v1/chat/completions` → 404
        // (the platform speaks `/stream`). The stream path has no
        // response_format, so fenced output is tolerated instead.
        if self.is_marc27() {
            let msgs = serde_json::json!([{ "role": "user", "content": prompt }]);
            let text = self.chat_marc27_simple(&msgs).await?;
            return Ok((Self::strip_json_fences(&text).to_string(), None));
        }
        let url = self.chat_completions_url();
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.1,
            "response_format": {"type": "json_object"},
        });
        // The reasoning kill-switch, honoured PROACTIVELY when the operator
        // asked for it: without this, a thinking-mode model burns one whole
        // call per request on reasoning_content before the self-heal below
        // retries with thinking disabled — at minutes per call on a local
        // 12B model, every chunk of a windowed document would pay twice.
        // Opt-in via env because the kwarg is a llama-server/vLLM extension
        // OpenAI rejects with 400 (same contract as the schema path's
        // caller-opt-in `no_think`).
        if no_think_requested() {
            body["chat_template_kwargs"] = serde_json::json!({"enable_thinking": false});
        }
        let body = self.with_operator_output_cap(body, prompt.len() as u64 / 4);
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;

        // Self-heal for thinking-mode budget burn (measured live with Gemma 4
        // 12B on llama-server: the model spent its entire max_tokens on
        // `reasoning_content` and produced no JSON, so EVERY extraction
        // failed). Retry ONCE with thinking disabled via
        // `chat_template_kwargs` — llama-server honors it, Ollama ignores it
        // (both verified against live servers), and the field is only ever
        // sent to a backend that has already exhibited thinking-mode burn,
        // so providers that reject unknown parameters never see it. If the
        // retry does not produce usable content either, the ORIGINAL
        // diagnosis below is what the caller gets.
        if Self::burned_budget_on_reasoning(&data["choices"][0]) {
            let mut retry_body = body.clone();
            retry_body["chat_template_kwargs"] = serde_json::json!({"enable_thinking": false});
            match self.post(&url, &retry_body).await {
                Ok(retry_resp) => {
                    if let Ok(retry_data) = retry_resp.json::<serde_json::Value>().await
                        && let Ok(text) = Self::extract_json_content(&retry_data["choices"][0])
                    {
                        return Ok((text, Self::usage_of(&retry_data)));
                    }
                    tracing::warn!(
                        "thinking-disabled retry still produced no JSON; reporting the \
                         original thinking-mode diagnosis"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "thinking-disabled retry failed ({e:#}); reporting the original \
                         thinking-mode diagnosis"
                    );
                }
            }
        }
        let text = Self::extract_json_content(&data["choices"][0])?;
        Ok((text, Self::usage_of(&data)))
    }

    /// The `usage` object of a chat-completions response, when present.
    fn usage_of(data: &serde_json::Value) -> Option<UsageInfo> {
        data.get("usage")
            .and_then(|u| serde_json::from_value::<UsageInfo>(u.clone()).ok())
    }

    /// Generate JSON under a server-enforced schema (`response_format:
    /// json_schema`, supported by llama-server, vLLM and OpenAI), with the
    /// determinism knobs sent explicitly: `temperature: 0` and the caller's
    /// `seed`. Constrained decoding guarantees FORM (the output parses and
    /// every enum-locked field holds a declared value), never TRUTH — the
    /// caller's validation still owns semantic checks.
    ///
    /// Degradation is honest, never silent: an endpoint that REJECTS the
    /// schema (see [`error_rejects_json_schema`]) gets ONE fallback request
    /// identical except for `response_format: json_object` — same seed, same
    /// temperature — and the returned [`JsonDecodingTrace`] carries the
    /// server's rejection so the caller can surface it. Backends with no
    /// `response_format` at all (embedded GGUF, the MARC27 `/stream` path)
    /// take the existing prompt-only path and say so the same way. What this
    /// method CANNOT detect is a server that accepts `json_schema` with 200
    /// and silently ignores it (old Ollama builds did) — that class is only
    /// caught downstream, by validating the output against the same
    /// declarations the schema was built from.
    ///
    /// Any other failure propagates as an error — a 401/404/429 is not a
    /// capability signal, and retrying it without the schema would bury the
    /// real problem under a second, less informative failure.
    ///
    /// `no_think` adds `chat_template_kwargs: {"enable_thinking": false}`
    /// (llama-server / vLLM) — the reasoning kill-switch for models whose
    /// thinking mode otherwise consumes the output budget before any JSON
    /// appears (measured 2026-08-10, Gemma-4-12B on the tabular extraction
    /// prompt: ~85% of ANY budget went to reasoning_content — 11.1k chars
    /// at 4096 tokens, 46.7k at 16384 — and the constrained JSON never
    /// started). It is caller-opt-in and off by default because OpenAI
    /// rejects unknown request fields with 400. Recorded in the trace so a
    /// run difference stays attributable.
    pub async fn generate_json_with_schema(
        &self,
        prompt: &str,
        schema: &JsonSchemaSpec,
        seed: i64,
        no_think: bool,
    ) -> Result<ConstrainedJson> {
        // Embedded GGUF: the local adapter exposes no grammar surface and no
        // per-request seed/temperature, so nothing deterministic or
        // constrained can be claimed. Take the existing prompt-only path.
        if self.local_backend().is_some() {
            let (text, usage) = self.generate_json_with_usage(prompt).await?;
            return Ok(ConstrainedJson {
                text,
                trace: JsonDecodingTrace {
                    mode: JsonDecodingMode::PromptOnly,
                    degraded: Some(
                        "embedded GGUF inference has no schema-constrained decoding; \
                         prompt-only JSON extraction was used and no seed or temperature \
                         was sent"
                            .to_string(),
                    ),
                    seed: None,
                    temperature: None,
                    no_think: false,
                },
                usage,
            });
        }
        // MARC27 platform: /stream carries no response_format and no sampling
        // controls — the schema cannot be applied there.
        if self.is_marc27() {
            let (text, usage) = self.generate_json_with_usage(prompt).await?;
            return Ok(ConstrainedJson {
                text,
                trace: JsonDecodingTrace {
                    mode: JsonDecodingMode::PromptOnly,
                    degraded: Some(
                        "the MARC27 /stream path has no response_format or sampling \
                         controls; prompt-only JSON extraction was used and no seed or \
                         temperature was sent"
                            .to_string(),
                    ),
                    seed: None,
                    temperature: None,
                    no_think: false,
                },
                usage,
            });
        }
        let url = self.chat_completions_url();
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.0,
            "seed": seed,
            "max_tokens": self.effective_max_tokens(prompt.len() as u64 / 4),
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": schema.name,
                    "strict": true,
                    "schema": schema.schema,
                },
            },
        });
        if no_think {
            body["chat_template_kwargs"] = serde_json::json!({"enable_thinking": false});
        }
        match self.post(&url, &body).await {
            Ok(resp) => {
                let data: serde_json::Value = resp.json().await.context("bad chat response")?;
                let text = Self::extract_json_content(&data["choices"][0])?;
                Ok(ConstrainedJson {
                    text,
                    trace: JsonDecodingTrace {
                        mode: JsonDecodingMode::JsonSchema,
                        degraded: None,
                        seed: Some(seed),
                        temperature: Some(0.0),
                        no_think,
                    },
                    usage: Self::usage_of(&data),
                })
            }
            Err(err) if error_rejects_json_schema(&err) => {
                // The endpoint rejected the SCHEMA, not the request: fall
                // back once, identical except for response_format, and carry
                // the rejection into the trace so the caller can report it.
                body["response_format"] = serde_json::json!({"type": "json_object"});
                let resp = self.post(&url, &body).await.with_context(|| {
                    format!(
                        "the endpoint rejected response_format json_schema ({err:#}) \
                         and the json_object fallback request failed too"
                    )
                })?;
                let data: serde_json::Value = resp.json().await.context("bad chat response")?;
                let text = Self::extract_json_content(&data["choices"][0])?;
                Ok(ConstrainedJson {
                    text,
                    trace: JsonDecodingTrace {
                        mode: JsonDecodingMode::JsonObject,
                        degraded: Some(format!(
                            "the endpoint rejected schema-constrained decoding \
                             (response_format json_schema): {err:#}. Extraction fell \
                             back to prompt-guided JSON, so the model was NOT \
                             structurally prevented from emitting out-of-vocabulary \
                             types, relationships, or units — validation still gates \
                             what is stored"
                        )),
                        seed: Some(seed),
                        temperature: Some(0.0),
                        no_think,
                    },
                    usage: Self::usage_of(&data),
                })
            }
            Err(err) => Err(err.context(
                "schema-constrained JSON generation failed; the endpoint did not reject \
                 the schema itself, so this is a request failure, not a capability \
                 degradation",
            )),
        }
    }

    /// True when a chat choice shows the thinking-mode failure signature:
    /// no visible content, `finish_reason: "length"`, and a non-empty
    /// `reasoning_content` — i.e. the whole output budget went to reasoning.
    fn burned_budget_on_reasoning(choice: &serde_json::Value) -> bool {
        let msg = &choice["message"];
        msg["content"].as_str().unwrap_or_default().is_empty()
            && choice["finish_reason"].as_str() == Some("length")
            && !msg["reasoning_content"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
    }

    /// Strip a Markdown code fence (```json … ``` or ``` … ```) from around
    /// a JSON payload. Providers without a JSON response mode (the MARC27
    /// `/stream` path) often fence their JSON; the parser downstream wants
    /// the bare object. Text without a fence is returned unchanged.
    fn strip_json_fences(text: &str) -> &str {
        let trimmed = text.trim();
        let Some(rest) = trimmed.strip_prefix("```") else {
            return trimmed;
        };
        // Drop an optional language tag (e.g. `json`) up to the first newline.
        let body = match rest.split_once('\n') {
            Some((_lang, body)) => body,
            None => rest,
        };
        body.strip_suffix("```").map_or(body, str::trim).trim()
    }

    /// Embed a single text string. Returns the embedding vector.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let vecs = self.embed(vec![text.to_string()]).await?;
        vecs.into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("LLM returned no embedding"))
    }

    /// Batch embedding. Returns one vector per input text.
    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if self.local_backend().is_some() {
            bail!(
                "embedded GGUF inference provides text generation, not embeddings. Configure prism-embed's native ONNX or OpenAI-compatible adapter. No remote endpoint was tried."
            );
        }
        let base = self.config.base_url.trim_end_matches('/');
        let url = if base.ends_with("/v1") {
            format!("{base}/embeddings")
        } else {
            format!("{base}/v1/embeddings")
        };
        let body = serde_json::json!({
            "model": self.embed_model(),
            "input": texts,
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad embedding response")?;
        let arr = data["data"]
            .as_array()
            .context("expected data array in embeddings response")?;
        let mut embeddings = Vec::with_capacity(arr.len());
        for item in arr {
            let vec: Vec<f32> = serde_json::from_value(item["embedding"].clone())
                .context("bad embedding vector")?;
            embeddings.push(vec);
        }
        Ok(embeddings)
    }

    /// Render and tokenize the exact prompt the embedded GGUF path would
    /// prefill. This loads no remote resource and is unavailable for hosted
    /// backends, whose provider-owned templates are not observable here.
    pub async fn render_local_prompt(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<RenderedLocalPrompt> {
        let Some(local) = self.local_backend() else {
            bail!(
                "exact prompt rendering is unavailable for hosted backends because their final chat template is provider-owned"
            );
        };
        local.render_prompt(messages, tools).await
    }

    /// Return a content receipt for the exact GGUF bound to this client.
    ///
    /// The first call initializes the model, hashes its retained source file,
    /// and caches the receipt with that model. Later calls and influence
    /// scoring reuse the same per-client receipt. No model is downloaded and
    /// no hosted fallback is attempted. Targets that cannot pass a stable
    /// descriptor path to llama.cpp return an explicit unavailable outcome;
    /// path-only checks are never presented as a verified receipt.
    pub async fn local_model_identity(&self) -> Result<LocalModelIdentityOutcome> {
        let Some(local) = self.local_backend() else {
            return Ok(LocalModelIdentityOutcome::Unavailable {
                code: LocalPromptInfluenceUnavailableCode::HostedBackend,
                detail: "loaded-model identity is only observable for a local GGUF backend; no hosted request or fallback was attempted"
                    .to_string(),
            });
        };
        local.model_identity().await
    }

    /// Score how much each candidate tool changes the local model's complete
    /// next-token distribution when appended to `baseline_tools`.
    ///
    /// This is inference-context influence, not training-data or weight
    /// influence. Every arm uses this client's one cached GGUF, its embedded
    /// Minja template, and a fresh prefill context. No grammar or sampler is
    /// applied. Each full definition is passed to the production renderer
    /// unchanged; the measured intervention is the representation that the
    /// model's template actually emits. Candidate order is preserved exactly.
    /// Scoring is explicitly unavailable when the target cannot prove a
    /// descriptor-backed loaded-model identity.
    pub async fn score_local_tool_influence(
        &self,
        messages: &[ChatMessage],
        baseline_tools: &[ToolDefinition],
        candidates: &[ToolDefinition],
    ) -> Result<LocalPromptInfluenceOutcome> {
        let Some(local) = self.local_backend() else {
            return Ok(LocalPromptInfluenceOutcome::Unavailable {
                code: LocalPromptInfluenceUnavailableCode::HostedBackend,
                detail: "prompt-intervention influence requires local GGUF logits; hosted providers own the final template and logits, and no fallback was attempted"
                    .to_string(),
            });
        };
        local
            .score_tool_influence(messages, baseline_tools, candidates)
            .await
    }

    /// Chat with tool-calling support and SSE streaming.
    /// Calls `on_delta` for each text chunk as it arrives.
    /// Returns the final assembled response (same as `chat_with_tools`).
    pub async fn chat_with_tools_streaming(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        mut on_delta: impl FnMut(&str, bool),
    ) -> Result<ChatResponse> {
        if let Some(local) = self.local_backend() {
            let messages_estimate = Self::estimate_tokens(&serde_json::to_value(messages)?);
            let tools_estimate = Self::estimate_tokens(&serde_json::to_value(tools)?);
            let has_tool_result = messages.iter().any(|message| message.role == "tool");
            if tools.is_empty() {
                let generation = local
                    .generate_streaming(
                        messages,
                        tools,
                        self.effective_local_max_tokens(messages_estimate + tools_estimate),
                        |delta| on_delta(delta, false),
                    )
                    .await?;
                return Self::local_chat_response(
                    generation,
                    tools,
                    &self.config.model,
                    false,
                    &self.local_call_counter,
                );
            }

            // Tool-call syntax is buffered so control markers never leak into
            // visible assistant text. Once a result is present, ordinary
            // final prose streams immediately; only a possible protocol
            // prefix remains buffered until the response is classified.
            let mut pending = String::new();
            let mut visible_started = false;
            let generation = local
                .generate_streaming(
                    messages,
                    tools,
                    self.effective_local_max_tokens(messages_estimate + tools_estimate),
                    |delta| {
                        if !has_tool_result {
                            return;
                        }
                        pending.push_str(delta);
                        let is_protocol = ["{", "<start_function_call>", "<|tool_call_start|>"]
                            .iter()
                            .any(|marker| {
                                marker.starts_with(&pending) || pending.starts_with(marker)
                            });
                        if !visible_started && is_protocol {
                            return;
                        }
                        visible_started = true;
                        on_delta(&pending, false);
                        pending.clear();
                    },
                )
                .await?;
            let response = Self::local_chat_response(
                generation,
                tools,
                &self.config.model,
                has_tool_result,
                &self.local_call_counter,
            )?;
            if !visible_started && let Some(content) = response.message.content.as_deref() {
                on_delta(content, false);
            }
            return Ok(response);
        }
        // MARC27 platform: use /stream with SSE.
        // The platform forwards `tools` verbatim to the upstream provider and
        // streams OpenAI-style `tool_calls` deltas back, so this path sends the
        // SAME tool definitions as the OpenAI path (see the module docs). What
        // is injected as text here is behavioural guidance ONLY — never a tool
        // inventory.
        if self.is_marc27() {
            let url = format!("{}/stream", self.config.base_url);

            let mut aug_messages: Vec<serde_json::Value> = serde_json::to_value(messages)?
                .as_array()
                .cloned()
                .unwrap_or_default();

            // Where a synthetic system message goes: after the caller's own
            // system prompt, if there is one.
            let inject_idx = usize::from(
                aug_messages
                    .first()
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("system"),
            );

            // Convert OpenAI-format messages to what MARC27 accepts.
            // MARC27 only understands system/user/assistant with string content.
            for msg in &mut aug_messages {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                if role == "tool" {
                    // Convert tool results to user messages
                    let tool_id = msg
                        .get("tool_call_id")
                        .and_then(|t| t.as_str())
                        .unwrap_or("tool");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    *msg = serde_json::json!({
                        "role": "user",
                        "content": format!("[Tool result from {tool_id}]\n{content}"),
                    });
                } else if role == "assistant" {
                    // Strip tool_calls and ensure content is a string (not null)
                    if let Some(obj) = msg.as_object_mut() {
                        obj.remove("tool_calls");
                        obj.remove("tool_call_id");
                        // Ensure content is always a string
                        if obj.get("content").is_none()
                            || obj.get("content") == Some(&serde_json::Value::Null)
                        {
                            obj.insert(
                                "content".to_string(),
                                serde_json::Value::String(String::new()),
                            );
                        }
                    }
                }
            }

            // Build the request for a given tool mode. `native` = real `tools`
            // array (what every OpenAI-shaped upstream provider accepts);
            // otherwise the selected tools are rendered as text and no `tools`
            // key is sent at all.
            let build = |native: bool| -> Result<serde_json::Value> {
                let mut msgs = aug_messages.clone();
                if !tools.is_empty() {
                    let mut block = TOOL_GUIDANCE_BLOCK.to_string();
                    if !native {
                        block.push_str(&render_tools_as_text(tools));
                    }
                    msgs.insert(
                        inject_idx,
                        serde_json::json!({"role": "system", "content": block}),
                    );
                }
                let est = msgs.iter().map(|m| m.to_string().len() as u64).sum::<u64>() / 4
                    + if native {
                        Self::estimate_tokens(&serde_json::to_value(tools)?)
                    } else {
                        0
                    };
                let body = serde_json::json!({
                    "model": self.config.model,
                    "messages": msgs,
                    // Same fix as chat_marc27_simple: this path previously sent
                    // no cap at all, so a tool-calling turn could generate an
                    // unbounded (and unbounded-billed) response.
                });
                let mut body = self.with_operator_output_cap(body, est);
                // The tool surface, identical to the OpenAI path below: the
                // caller's already-token-bounded selection, with FULL schemas.
                if native && !tools.is_empty() {
                    body["tools"] = serde_json::to_value(tools)?;
                }
                Ok(body)
            };

            // Retry only the *establishment* of the stream. Once a byte has
            // been handed to the caller, a retry would replay visible output
            // and bill the turn twice — so everything below this line stays
            // fatal on first failure.
            //
            // One extra establishment attempt exists for tool schemas: the
            // platform forwards `tools` verbatim, so an upstream provider that
            // does not speak the OpenAI tool shape rejects the request outright
            // (measured: its direct Anthropic provider answers `tools.0: Input
            // tag 'function' … does not match any of the expected tags`). A
            // rejected request bills nothing, so fall back to the text protocol
            // for that turn rather than failing it. Any other error is returned
            // as-is — this must never mask a 401/402/429.
            let resp = match self
                .send_retrying("llm.stream.marc27", &url, &build(true)?, true)
                .await
            {
                Ok(resp) => resp,
                Err(e) if !tools.is_empty() && error_rejects_tool_schemas(&e) => {
                    tracing::warn!(
                        "MARC27 upstream rejected OpenAI-shaped tool schemas ({e:#}) — \
                         retrying this turn with the text tool-call protocol"
                    );
                    match self
                        .send_retrying("llm.stream.marc27", &url, &build(false)?, true)
                        .await
                    {
                        Ok(resp) => resp,
                        // Surface the FIRST error — it says what the provider
                        // actually refused. The fallback's own failure rides
                        // along as context instead of replacing it.
                        Err(fallback_err) => {
                            return Err(e.context(format!(
                                "retry without tool schemas also failed: {fallback_err:#}"
                            )));
                        }
                    }
                }
                Err(e) => return Err(e),
            };
            debug!("MARC27 stream response received, reading chunks...");

            // Read SSE stream incrementally — don't use resp.text() which
            // blocks until the connection closes (SSE keeps it open).
            use futures_util::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut sse_buf = String::new();
            let mut full_text = String::new();
            let mut usage_info = None;
            let mut done = false;
            // Native tool_calls, assembled by the SAME accumulator the OpenAI
            // path uses — the platform forwards the provider's OpenAI-style
            // deltas verbatim on `StreamChunk.tool_calls`.
            let mut native_calls = ToolCallAccumulator::default();

            while let Some(chunk) = stream.next().await {
                let bytes = chunk.context("error reading SSE chunk")?;
                sse_buf.push_str(&String::from_utf8_lossy(&bytes));

                // Process complete lines from the buffer
                while let Some(nl) = sse_buf.find('\n') {
                    let line = sse_buf[..nl].trim().to_string();
                    sse_buf = sse_buf[nl + 1..].to_string();

                    let line = line.strip_prefix("data: ").unwrap_or(&line).trim();
                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str())
                            && !delta.is_empty()
                        {
                            full_text.push_str(delta);
                            // Don't call on_delta during streaming for MARC27 path.
                            // We collect full_text, strip tool calls, then emit clean
                            // content_text after the response completes. This prevents
                            // partial tool call JSON from leaking into visible text.
                        }
                        if let Some(tcs) = chunk.get("tool_calls").and_then(|t| t.as_array()) {
                            native_calls.push_deltas(tcs);
                        }
                        if let Some(u) = chunk.get("usage") {
                            let pt = u
                                .get("prompt_tokens")
                                .or_else(|| u.get("input_tokens"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            let ct = u
                                .get("completion_tokens")
                                .or_else(|| u.get("output_tokens"))
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            if pt > 0 || ct > 0 {
                                usage_info = Some(UsageInfo {
                                    prompt_tokens: pt,
                                    completion_tokens: ct,
                                    total_tokens: pt + ct,
                                });
                            }
                        }
                        if chunk.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                            done = true;
                            break;
                        }
                    }
                }
                if done {
                    break;
                }
            }

            // Native tool_calls win. The text parser stays as a fallback for
            // any upstream provider the platform can't yet forward tool_calls
            // for (today: the direct Anthropic provider) and for models that
            // narrate a call instead of emitting one.
            let native = native_calls.finish();
            let from_native = native.is_some();
            let tool_calls = match native {
                Some(calls) => calls,
                // Only take the FIRST batch (before any "Results:" hallucination),
                // deduped (the LLM sometimes repeats a call).
                None => dedup_tool_calls(parse_text_tool_calls(&full_text)),
            };
            let mut content_text = if from_native {
                full_text.trim().to_string()
            } else {
                strip_tool_call_blocks(&full_text)
            };

            // If we found tool calls IN THE TEXT, suppress any JSON/code
            // artifacts in content. Gemini often leaks partial tool call JSON
            // or closing ``` into the content when it outputs a fenced call.
            // Only keep content that looks like actual natural language prose.
            // Native tool_calls arrive on their own channel and cannot leak
            // into the text, so this heuristic must NOT run for them — it
            // would delete a legitimate answer that merely ends in a fence.
            if !from_native && !tool_calls.is_empty() && !content_text.is_empty() {
                let trimmed = content_text.trim();
                let looks_like_json = trimmed.contains("}}")
                    || trimmed.contains("\"name\":")
                    || trimmed.contains("\"arguments\":")
                    || trimmed.starts_with('{')
                    || trimmed.starts_with('"')
                    || trimmed.starts_with("```")
                    || trimmed.ends_with("```");
                if looks_like_json {
                    content_text.clear();
                }
            }

            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: if content_text.is_empty() {
                        None
                    } else {
                        Some(content_text)
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                },
                usage: usage_info,
                generation_metrics: None,
            });
        }
        let url = self.chat_completions_url();

        let est = Self::estimate_tokens(&serde_json::to_value(messages).unwrap_or_default())
            + Self::estimate_tokens(&serde_json::to_value(tools).unwrap_or_default());
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "stream": true,
        });
        let mut body = self.with_operator_output_cap(body, est);

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        // As above: retry only until the stream is open. A mid-stream failure
        // stays fatal, because replaying it would duplicate what the user has
        // already seen and pay for the turn twice.
        let resp = self.send_retrying("llm.stream", &url, &body, false).await?;

        // Parse SSE stream
        let mut full_content = String::new();
        let mut native_calls = ToolCallAccumulator::default();
        let mut usage_info: Option<UsageInfo> = None;

        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut sse_buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("error reading SSE chunk")?;
            sse_buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete SSE lines from the buffer
            while let Some(newline_pos) = sse_buffer.find('\n') {
                let line = sse_buffer[..newline_pos].trim().to_string();
                sse_buffer = sse_buffer[newline_pos + 1..].to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    continue;
                }
                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data)
                {
                    // Extract text delta — separate content from
                    // reasoning_content. Content is the actual response;
                    // reasoning_content is thinking/reasoning tokens that
                    // should be rendered dimmed/collapsed in the TUI.
                    let content_delta = chunk
                        .pointer("/choices/0/delta/content")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty());

                    let reasoning_delta = chunk
                        .pointer("/choices/0/delta/reasoning_content")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty());

                    if let Some(delta) = content_delta {
                        on_delta(delta, false);
                        full_content.push_str(delta);
                    } else if let Some(delta) = reasoning_delta {
                        // Reasoning tokens — is_reasoning=true so the
                        // agent loop can emit them as ui.thinking.delta
                        on_delta(delta, true);
                        full_content.push_str(delta);
                    }

                    // Extract streaming tool calls
                    if let Some(tcs) = chunk
                        .pointer("/choices/0/delta/tool_calls")
                        .and_then(|t| t.as_array())
                    {
                        native_calls.push_deltas(tcs);
                    }

                    // Extract usage from final chunk
                    if let Some(u) = chunk.get("usage") {
                        usage_info = serde_json::from_value::<UsageInfo>(u.clone()).ok();
                    }
                }
            }
        }

        let tool_calls = native_calls.finish();

        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: if full_content.is_empty() {
                    None
                } else {
                    Some(full_content)
                },
                tool_calls,
                tool_call_id: None,
            },
            usage: usage_info,
            generation_metrics: None,
        })
    }

    /// The model's context window in tokens, for callers that size work to
    /// it (ingest batching derives batch sizes from this — the window is the
    /// binding constraint, not a number someone picked).
    ///
    /// Resolution order: the configured value (platform catalog, or
    /// GGUF-derived for the embedded backend) when known; otherwise the
    /// serving runtime is asked. llama.cpp's `/props` reports the ACTUAL
    /// serving allocation (`default_generation_settings.n_ctx`), which is
    /// what binds a request — deliberately preferred over `/v1/models`'
    /// `n_ctx_train`, the trained maximum a server may not have allocated
    /// (measured live: n_ctx_train 262144 vs n_ctx 65536).
    ///
    /// Returns `None` when the backend reports nothing (plain OpenAI-shaped
    /// endpoints have no `/props`). That is a config gap the CALLER must
    /// surface and bridge with a documented fallback — never silently.
    pub async fn probe_context_window(&self) -> Option<u64> {
        if let Some(cw) = self.config.context_window {
            return Some(cw);
        }
        let LlmBackend::Http(client) = &self.backend else {
            // The embedded GGUF backend writes its window into the config at
            // construction; reaching here means it genuinely has none.
            return None;
        };
        // `/props` lives at the server root; the OpenAI surface is mounted
        // under `/v1`, so a `…/v1` base is peeled back to the root.
        let base = self.config.base_url.trim_end_matches('/');
        let root = base.strip_suffix("/v1").unwrap_or(base);
        let url = format!("{root}/props");
        prism_runtime::offline::check_url(&url).ok()?;
        // Per-request deadline: this is a metadata poke, not a generation —
        // an unanswered probe must degrade to "unknown", not hang the run.
        let mut req = client.get(&url).timeout(Duration::from_secs(10));
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        let resp = req.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let data: serde_json::Value = resp.json().await.ok()?;
        data.pointer("/default_generation_settings/n_ctx")
            .and_then(serde_json::Value::as_u64)
    }

    /// Health check — verify the LLM backend is reachable.
    pub async fn health_check(&self) -> Result<()> {
        if let Some(local) = self.local_backend() {
            return local.health_check().await;
        }
        let url = format!("{}/v1/models", self.config.base_url);
        prism_runtime::offline::check_url(&url).map_err(anyhow::Error::msg)?;
        let mut req = self.http_client()?.get(&url);
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
        }
        let resp = req.send().await.context("LLM not reachable")?;
        if !resp.status().is_success() {
            bail!("LLM health check returned {}", resp.status());
        }
        Ok(())
    }

    // ── Internal ──────────────────────────────────────────────────

    fn embed_model(&self) -> &str {
        self.config
            .embedding_model
            .as_deref()
            .unwrap_or(&self.config.model)
    }

    /// The auth header `(name, value)` for the configured credential.
    /// Explicit kind wins; raw-string callers retain the frozen `m27_`
    /// compatibility heuristic.
    fn auth_header(&self) -> Option<(&'static str, String)> {
        let key = self.config.api_key.as_ref().filter(|k| !k.is_empty())?;
        match self.config.credential_kind {
            Some(LlmCredentialKind::ApiKey) => Some(("X-API-Key", key.clone())),
            Some(LlmCredentialKind::Bearer) => Some(("Authorization", format!("Bearer {key}"))),
            None if key.starts_with("m27_") => Some(("X-API-Key", key.clone())),
            None => Some(("Authorization", format!("Bearer {key}"))),
        }
    }

    async fn post(&self, url: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        prism_runtime::offline::check_url(url).map_err(anyhow::Error::msg)?;
        debug!(%url, "LLM request");
        self.send_retrying("llm.post", url, body, false).await
    }

    /// Issue an LLM request, retrying only transient failures.
    ///
    /// This replaced a hand-rolled loop that retried 429 and *nothing else* —
    /// a reset socket or a 503 from the proxy was terminal mid-conversation.
    /// The shared policy keeps that loop's `Retry-After` handling (see
    /// [`retry::HttpStatus::from_response`]) and adds transport failures,
    /// while still failing a 400/401/402 on the first attempt.
    ///
    /// `sse` adds `Accept: text/event-stream` for the streaming callers.
    async fn send_retrying(
        &self,
        label: &str,
        url: &str,
        body: &serde_json::Value,
        sse: bool,
    ) -> Result<reqwest::Response> {
        // Hard offline mode is checked before the retry closure so a blocked
        // URL is never classified as transient and retried.
        prism_runtime::offline::check_url(url).map_err(anyhow::Error::msg)?;
        // Every call here bills tokens, so only an outright refusal (429,
        // 503) or a connection that never opened is replayed. A read timeout
        // is NOT — see `retry::Idempotency`.
        retry::retrying(label, retry::Idempotency::Billable, || async {
            let mut req = self.http_client()?.post(url).json(body);
            if sse {
                req = req.header("Accept", "text/event-stream");
            }
            if let Some((name, value)) = self.auth_header() {
                req = req.header(name, value);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("LLM request to {url} failed"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let http = retry::HttpStatus::from_response(&resp);
                let text = resp.text().await.unwrap_or_default();
                return Err(http).with_context(|| {
                    format!(
                        "LLM returned HTTP {status}: {text}{}",
                        base_url_hint(url, status)
                    )
                });
            }
            Ok(resp)
        })
        .await
    }
}

// ── MARC27 tool-calling helpers ─────────────────────────────────────

/// Whether an LLM error is the upstream provider refusing OpenAI-shaped tool
/// schemas, as opposed to anything else that can fail a request.
///
/// Deliberately narrow, on two axes. The fallback it gates costs one extra
/// (unbilled, already-failed) round-trip, but retrying a 401/402/429 without
/// tools would hide the real problem behind a second, less informative
/// failure — so the error must BOTH carry a request-shape status AND name the
/// tools field. A body that merely mentions credits or a plan does not match.
fn error_rejects_tool_schemas(err: &anyhow::Error) -> bool {
    // 400/422 = the provider rejected the request; 500 = the platform's own
    // wrapper around an upstream 400 (measured shape). Auth (401/403),
    // billing (402) and capacity (429/503) are never a schema problem.
    let request_shape = err
        .chain()
        .find_map(|c| c.downcast_ref::<retry::HttpStatus>())
        .is_some_and(|h| matches!(h.status, 400 | 422 | 500));
    if !request_shape {
        return false;
    }
    let text = format!("{err:#}").to_ascii_lowercase();
    ["tools.", "tools[", "\"tools\"", "tool_choice"]
        .iter()
        .any(|needle| text.contains(needle))
}

// ── Schema-constrained JSON generation ──────────────────────────────

/// Whether requests should carry the reasoning kill-switch
/// (`LLM_NO_THINK=1`/`true` in the environment — the same `LLM_*` surface
/// the other model knobs use). Opt-in because the kwarg it adds
/// (`chat_template_kwargs: {"enable_thinking": false}`) is a
/// llama-server/vLLM extension OpenAI rejects with 400; needed because a
/// thinking-mode model can burn ANY output budget on reasoning before the
/// JSON starts (measured 2026-08-10, Gemma-4-12B: ~85% of any budget went
/// to reasoning_content).
pub fn no_think_requested() -> bool {
    std::env::var("LLM_NO_THINK").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// The one extraction seed PRISM sends, deliberately a constant rather than
/// a config knob: reproducibility means every run of the same input against
/// the same model must sample identically, and a per-run seed would defeat
/// exactly that. It is recorded in the provenance activity alongside the
/// model id and temperature, so a difference between two runs is
/// attributable to input/model/server — never to an unrecorded knob.
pub const EXTRACTION_SEED: i64 = 42;

/// A named JSON schema for `response_format: {"type": "json_schema", …}`.
/// The caller owns deriving `schema` from its real declarations (PRISM: the
/// active ontology) — this crate only carries it to the wire.
#[derive(Debug, Clone)]
pub struct JsonSchemaSpec {
    /// Identifier sent as `json_schema.name` (OpenAI requires one).
    pub name: String,
    /// The JSON-Schema document itself.
    pub schema: serde_json::Value,
}

/// How one JSON generation was actually decoded on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonDecodingMode {
    /// The endpoint accepted `response_format: json_schema` — output is
    /// grammar-constrained to the supplied schema.
    JsonSchema,
    /// The endpoint rejected the schema; `response_format: json_object`
    /// was used instead (JSON syntax enforced, vocabulary NOT).
    JsonObject,
    /// No `response_format` at all (embedded GGUF, MARC27 `/stream`): the
    /// prompt text is the only thing shaping the output.
    PromptOnly,
}

impl JsonDecodingMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JsonSchema => "json_schema",
            Self::JsonObject => "json_object",
            Self::PromptOnly => "prompt_only",
        }
    }
}

/// The honest record of one JSON generation: which decoding mode actually
/// applied, why it degraded when it did, and the determinism knobs that were
/// really sent (None = the backend offered no such knob — never a guess).
/// Callers surface `degraded` to the user and record seed/temperature in
/// provenance; a capability that silently isn't applied is the defect class
/// this type exists to prevent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonDecodingTrace {
    pub mode: JsonDecodingMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Whether the request carried the reasoning kill-switch
    /// (`chat_template_kwargs: {"enable_thinking": false}`). Part of the
    /// reproducibility record: a thinking and a non-thinking run of the
    /// same seed are different computations.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_think: bool,
}

/// One schema-constrained generation: the raw JSON text plus its trace.
#[derive(Debug, Clone)]
pub struct ConstrainedJson {
    pub text: String,
    pub trace: JsonDecodingTrace,
    /// Token usage the backend reported for this call, if any. Output is
    /// metered and billed per token — counting is the control — so callers
    /// that fan a dataset out over many extraction calls sum these to
    /// report what the run actually cost. `None` means the backend
    /// reported nothing, never that usage was zero.
    pub usage: Option<UsageInfo>,
}

/// Whether an LLM error is the endpoint refusing `response_format:
/// json_schema`, as opposed to anything else that can fail a request.
///
/// Same two-axis discipline as [`error_rejects_tool_schemas`]: the error must
/// BOTH carry a request-shape status (400/422 = provider rejected the
/// request, 500 = a proxy's wrapper around an upstream 400, 501 = declared
/// unimplemented) AND name the schema mechanism. Auth (401/403), billing
/// (402), routing (404) and capacity (429/503) are never a schema problem —
/// falling back on those would bury the real failure under a second, less
/// informative one.
fn error_rejects_json_schema(err: &anyhow::Error) -> bool {
    let request_shape = err
        .chain()
        .find_map(|c| c.downcast_ref::<retry::HttpStatus>())
        .is_some_and(|h| matches!(h.status, 400 | 422 | 500 | 501));
    if !request_shape {
        return false;
    }
    let text = format!("{err:#}").to_ascii_lowercase();
    [
        "response_format",
        "json_schema",
        "json schema",
        "grammar",
        "structured output",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// Render the SELECTED tools as text, for the fallback path only.
///
/// Every tool the selection layer chose, with its description and its
/// parameter names — the same set the `tools` array would have carried, not a
/// sample of it. (The categorised summary this replaces showed 4 names per
/// category and no parameters at all.)
fn render_tools_as_text(tools: &[ToolDefinition]) -> String {
    let mut out = String::from("\n\n## Tools available to you\n\n");
    for t in tools {
        let params = t
            .function
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        out.push_str(&format!(
            "- `{}({})` — {}\n",
            t.function.name, params, t.function.description
        ));
    }
    out.push_str(
        "\n## How to call a tool\n\n\
         ```tool_call\n\
         {\"name\": \"tool_name\", \"arguments\": {\"arg\": \"value\"}}\n\
         ```\n\n\
         Output ONE ```tool_call block, then STOP — the system executes it and \
         returns the result in your next message. Call `find_tools` if you need \
         a capability that is not listed above.\n",
    );
    out
}

/// Behavioural guidance injected as a second system message alongside the
/// agent prompt on the MARC27 path.
///
/// This is NOT a tool surface. The tool surface is the real `tools` array on
/// the request — the caller's already-token-bounded selection, with FULL
/// schemas, identical to the OpenAI path. What used to stand in for it here
/// was a categorised NAME SUMMARY (4 names per category, no descriptions, no
/// parameters): on a 135-tool catalog the model saw ~40 bare names, so every
/// other tool was only reachable via a `find_tools` hop. That summary is gone.
///
/// What remains has no other code path: the retrieval-discipline carve-out and
/// the domain guidance from PRs #109/#111/#114/#115 — where materials data
/// actually lives, which hosts the `web` tool cannot reach, the composition
/// patterns, and long-horizon discipline.
const TOOL_GUIDANCE_BLOCK: &str = "\
        ## IMPORTANT: When NOT to call tools\n\n\
        For greetings, casual conversation, conceptual explanations, \
        or anything that does not need live data — respond with plain text. \
        Do NOT call tools for simple chat like \"hello\", \"what can you do?\", or \"explain X\".\n\
        This is NOT a licence to answer from memory: a question about a specific \
        MATERIAL, a source, or platform/job state always needs live data. Retrieve \
        it. If retrieval comes back empty, say it was not found — never fill the \
        gap from memory.\n\n\
        **When a tool fails (recovery rules — DO NOT GIVE UP):**\n\
        - A tool returning an error is NORMAL. It is NOT a signal to stop.\n\
        - If a tool returns a missing-API-key error (e.g. \"MP_API_KEY not set\"), \
        immediately try a keyless alternative: `materials_search` (OPTIMADE federation, \
        no key needed) or `prior_art_search` (literature) before giving up.\n\
        - If a tool returns \"unknown tool\", call `find_tools` to see real \
        names, then try the closest match. Do not give up.\n\
        - If two tools have failed for the same goal, call `find_tools` again, \
        then propose the next-best tool. The user expects multiple tool attempts on \
        failure — silence is the worst outcome.\n\
        - NEVER respond with empty content + no tool call after a tool error. Either \
        try a different tool, or explicitly tell the user which tools you tried and \
        why none of them worked.\n\n\
        ## Tool-composition patterns (USE THESE for the common tasks)\n\n\
        PRISM is a materials-discovery strategy engine, not just a chat model. \
        For non-trivial questions you should COMPOSE multiple tools instead of \
        relying on a single one.\n\n\
        **CRITICAL — where materials data actually lives:**\n\
        - Materials property data (creep, modulus, density, band gap, etc.) \
        lives in `materials_search` (federated DB across MP / OPTIMADE / 18 \
        others) and in academic papers via `prior_art_search`. NOT on vendor \
        websites.\n\
        - Vendor PDFs (specialmetals.com, haynesintl.com, nickelinstitute.org, \
        matweb.com, hightempmetals.com, …) are paywalled, robots-blocked, or \
        gated. The `web` tool WILL return 403 / 404 / robots.txt on them. \
        Do not chain guesses at vendor URLs — that loop never converges.\n\
        - **Search engines + government repos block the `web` tool's User-Agent.** \
        Do NOT call `web` GET on `google.com/search`, `bing.com/search`, \
        `duckduckgo.com`, `osti.gov/servlets/*`, `osti.gov/biblio/*` — every one \
        returns robots.txt or 403. Observed cost in real runs: ~15 wasted tool \
        calls per question. Use `prior_art_search` (Semantic Scholar / arXiv / \
        OpenAlex / PubMed) or `research` instead. The CrossRef API \
        (`api.crossref.org/works`) IS accessible and is the right place for \
        DOI-based citation lookups.\n\
        - For ANY question of the form \"compare property X of alloys A, B, C\" \
        or \"what is property Y of material Z\", your FIRST tool call should be \
        `materials_search` or `prior_art_search` — never a `web` GET against a \
        vendor domain.\n\
        - `research` (the server-side RLM) is the right call when the question \
        spans multiple alloy systems + multiple properties + needs synthesis. \
        It already searches Semantic Scholar / arXiv / OpenAlex / the KG \
        internally; you do not need to do that hop yourself.\n\n\
        The most common patterns:\n\n\
        - **Materials-discovery**: \
        `materials_search` (federated DB lookup) → `prior_art_search` (literature \
        cross-check on the candidates that came back) → `predict` (only if you \
        need a property the DB didn't return). Output candidates with BOTH a \
        DB id AND a paper citation. Never propose a composition without a \
        traceable source.\n\
        - **Property-prediction**: `predict` first, then validate with \
        `prior_art_search` on the predicted property to see if literature \
        agrees with the model output.\n\
        - **Use-case scoping** (\"can material X be used for Y?\"): \
        `prior_art_search` first (does anyone publish on this?), then \
        `materials_search` for compositional alternatives, then `web` only \
        for industry / regulatory context that isn't in academic papers.\n\
        - **Knowledge-graph queries**: `query_platform` (term or semantic \
        search) and `knowledge_entity` (one entity + its neighbours) for \
        platform-internal provenance. Use them BEFORE `materials_search` if the \
        user is asking about a specific project / dataset rather than a \
        general material.\n\n\
        For ANY recommendation you give the user: cite the source. \
        \"Composition X has property Y\" must come with a tool result reference \
        (DB id, paper DOI, predict() output id). \"It's a known refractory \
        alloy\" without a citation is hallucination, not strategy.\n\n\
        ## Long-horizon discipline (the difference between PRISM and a chatbot)\n\n\
        Real materials questions take MANY tool calls — typically 8 to 30 — \
        and span minutes, not seconds. The literature shows that LLMs at long \
        horizons fail in two predictable ways: they (a) terminate early after \
        2–3 tool calls, returning a thin answer, or (b) forget the original \
        constraint by turn 10. Both are unacceptable here. The user is paying \
        for a strategy engine; behave like one.\n\n\
        **For any non-trivial question (not a single-fact lookup), follow this \
        loop:**\n\n\
        1. **Plan first, in writing.** Before ANY tool call, emit a numbered \
        plan listing the sub-questions you need to answer and which tool you \
        will use for each. This is your scratchpad and your contract with \
        the user. Re-read it before every subsequent tool call.\n\
        2. **Use `research` for deep multi-hop questions.** `research(question=...)` \
        runs a server-side Recursive Language Model that does iterative \
        decomposition + literature search + KG traversal in ONE call. Prefer \
        ONE `research` call over five hand-rolled `prior_art_search` + `web` \
        calls when the question is open-ended (\"design an alloy for X\", \
        \"compare approaches to Y\"). It exists because of arxiv:2512.24601; \
        you are the one calling it.\n\
        3. **Persist past the urge to wrap up.** If you've made fewer than \
        five tool calls on a multi-part question, you are NOT done. Asking \
        yourself \"do I have enough?\" after two calls is the failure mode. \
        Instead ask: \"which sub-question on my plan is still un-answered?\" \
        and call the next tool.\n\
        4. **Re-anchor on the original goal every ~5 turns.** Quote the \
        user's original ask back to yourself in your reasoning. The most \
        common long-horizon failure is silently drifting from \"design an RHEA \
        for LPBF at 2200 °C\" to \"list some refractory metals\".\n\
        5. **Deliberate completion.** When you ARE done, emit the marker \
        `FINAL ANSWER:` followed by the synthesized answer with citations. \
        This is the only acceptable way to end a research turn. An empty \
        response, or a response that just summarizes one tool's output \
        without synthesis, is not completion — it is giving up.\n\n\
        Long horizon is the product. The compaction system, the research \
        tool, and the recovery rules above all exist so you can sustain \
        20+ tool calls on one question without losing the thread. Use them.\n\
    ";

/// Parse ```tool_call blocks from response text.
/// Return the byte index just past the `}` that closes the JSON object
/// starting at `start` (which must be a `{`), respecting string literals and
/// escapes. `None` if the object is unbalanced. Only ever returns indices at
/// ASCII `}` positions, so the result is a valid char boundary.
fn balanced_object_end(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    if b.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// If a loose tool-call marker token (`tool_call`, `_call`, `call`,
/// `function_call`, optionally back-ticked / colon-suffixed) immediately
/// precedes the JSON object at `obj_start`, return the marker's start offset so
/// callers can strip it too; otherwise return `obj_start`.
fn marker_start_before(text: &str, obj_start: usize) -> usize {
    const MARKERS: &[&str] = &["tool_call", "_call", "call", "function_call"];
    let trimmed = text[..obj_start].trim_end();
    let token_start = trimmed
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let norm = trimmed[token_start..]
        .trim_matches('`')
        .trim_end_matches(':')
        .to_ascii_lowercase();
    if MARKERS.contains(&norm.as_str()) {
        token_start
    } else {
        obj_start
    }
}

/// Recover a bare (un-fenced) JSON tool call: the first `{...}` object that
/// deserializes and carries a non-empty, whitespace-free `name` plus an
/// `arguments` **object**. Returns `(region_start, name, arguments_json)` where
/// `region_start` includes any immediately-preceding loose marker line.
///
/// This is the recovery path for models that drop the ```tool_call fence.
/// Observed live on the MARC27 `/stream` text-tool-calling path: after a
/// natural-language preamble, Claude emitted `_call\n{"name":...}` with no
/// backticks, so the fenced/XML parsers missed it and the call silently leaked
/// into visible text and ended the turn. The guards (name shape + arguments is
/// an object) keep incidental prose JSON from being mistaken for a call.
fn find_bare_json_tool_call(text: &str) -> Option<(usize, String, String)> {
    let mut from = 0;
    while let Some(rel) = text[from..].find('{') {
        let obj_start = from + rel;
        if let Some(obj_end) = balanced_object_end(text, obj_start)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[obj_start..obj_end])
        {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args_is_obj = v.get("arguments").map(|a| a.is_object()).unwrap_or(false);
            if !name.is_empty() && !name.contains(char::is_whitespace) && args_is_obj {
                let arguments = v
                    .get("arguments")
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let region_start = marker_start_before(text, obj_start);
                return Some((region_start, name.to_string(), arguments));
            }
        }
        from = obj_start + 1;
    }
    None
}

fn parse_native_tool_call(text: &str) -> Result<(String, serde_json::Value)> {
    if text.starts_with("<|tool_call_start|>") {
        return parse_lfm_tool_call(text);
    }
    if text.starts_with("<|tool_call>") {
        return parse_gemma_tool_call(text);
    }

    let prefix = "<start_function_call>call:";
    let suffix = "<end_function_call>";
    let body = text
        .strip_prefix(prefix)
        .context("missing native function-call prefix")?
        .strip_suffix(suffix)
        .context("missing native function-call terminator")?;
    let open = body.find('{').context("missing native argument object")?;
    let name = body[..open].trim();
    validate_native_tool_name(name)?;
    let mut parser = NativeValueParser::new(&body[open..]);
    let arguments = parser.object()?;
    parser.skip_whitespace();
    if parser.position != parser.input.len() {
        bail!("native argument object has trailing characters");
    }
    Ok((name.to_string(), arguments))
}

fn parse_gemma_tool_call(text: &str) -> Result<(String, serde_json::Value)> {
    let prefix = "<|tool_call>call:";
    let suffix = "<tool_call|>";
    let body = text
        .strip_prefix(prefix)
        .context("missing Gemma function-call prefix")?
        .strip_suffix(suffix)
        .context("missing Gemma function-call terminator")?;
    let open = body.find('{').context("missing Gemma argument object")?;
    let name = body[..open].trim();
    validate_native_tool_name(name)?;
    // Gemma's template uses <|"|> as its string delimiter. The legacy
    // parser already implements the otherwise identical recursive value
    // syntax, and its grammar excludes literal '<' inside a string, so this
    // delimiter substitution cannot collide with payload data.
    let normalized = body[open..].replace(r#"<|"|>"#, "<escape>");
    let mut parser = NativeValueParser::new(&normalized);
    let arguments = parser.object()?;
    parser.skip_whitespace();
    if parser.position != parser.input.len() {
        bail!("Gemma argument object has trailing characters");
    }
    Ok((name.to_string(), arguments))
}

fn parse_lfm_tool_call(text: &str) -> Result<(String, serde_json::Value)> {
    let body = text
        .strip_prefix("<|tool_call_start|>")
        .context("missing LFM function-call prefix")?
        .strip_suffix("<|tool_call_end|>")
        .context("missing LFM function-call terminator")?
        .strip_prefix('[')
        .and_then(|body| body.strip_suffix(']'))
        .context("LFM function call must contain exactly one bracketed call")?;
    let open = body.find('(').context("missing LFM argument list")?;
    let name = body[..open].trim();
    validate_native_tool_name(name)?;
    let arguments = body[open + 1..]
        .strip_suffix(')')
        .context("unterminated LFM argument list")?;
    let mut parser = LfmArgumentParser::new(arguments);
    let arguments = parser.object()?;
    parser.skip_whitespace();
    if parser.position != parser.input.len() {
        bail!("LFM argument list has trailing characters");
    }
    Ok((name.to_string(), arguments))
}

fn validate_native_tool_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        bail!("native function name is empty or contains whitespace");
    }
    Ok(())
}

struct LfmArgumentParser<'a> {
    input: &'a str,
    position: usize,
}

impl<'a> LfmArgumentParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, position: 0 }
    }

    fn skip_whitespace(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn object(&mut self) -> Result<serde_json::Value> {
        let mut result = serde_json::Map::new();
        self.skip_whitespace();
        if self.position == self.input.len() {
            return Ok(serde_json::Value::Object(result));
        }
        loop {
            self.skip_whitespace();
            let start = self.position;
            while let Some(byte) = self.input.as_bytes().get(self.position) {
                if *byte == b'=' || byte.is_ascii_whitespace() {
                    break;
                }
                self.position += 1;
            }
            let key = self.input[start..self.position].trim();
            if key.is_empty() || result.contains_key(key) {
                bail!("LFM tool call has an empty or duplicate argument key");
            }
            self.skip_whitespace();
            if self.input.as_bytes().get(self.position) != Some(&b'=') {
                bail!("LFM tool call expected '=' after argument key {key:?}");
            }
            self.position += 1;
            let value = self.value()?;
            result.insert(key.to_string(), value);
            self.skip_whitespace();
            match self.input.as_bytes().get(self.position) {
                Some(b',') => self.position += 1,
                None => break,
                _ => bail!("LFM tool call expected ',' after an argument"),
            }
        }
        Ok(serde_json::Value::Object(result))
    }

    fn value(&mut self) -> Result<serde_json::Value> {
        self.skip_whitespace();
        match self.input.as_bytes().get(self.position) {
            Some(b'\'') | Some(b'"') => Ok(serde_json::Value::String(self.quoted_string()?)),
            Some(b'{') | Some(b'[') => self.json_container(),
            Some(_) => {
                let start = self.position;
                while let Some(byte) = self.input.as_bytes().get(self.position) {
                    if *byte == b',' || byte.is_ascii_whitespace() {
                        break;
                    }
                    self.position += 1;
                }
                let raw = self.input[start..self.position].trim();
                if raw.is_empty() {
                    bail!("LFM tool call has an empty argument value");
                }
                serde_json::from_str(raw)
                    .map_err(|error| anyhow::anyhow!("LFM argument is not valid JSON: {error}"))
            }
            None => bail!("LFM tool call ended before an argument value"),
        }
    }

    fn quoted_string(&mut self) -> Result<String> {
        let quote = *self
            .input
            .as_bytes()
            .get(self.position)
            .context("LFM string is missing its opening quote")?;
        self.position += 1;
        let mut result = String::new();
        while let Some(byte) = self.input.as_bytes().get(self.position).copied() {
            self.position += 1;
            if byte == quote {
                return Ok(result);
            }
            if byte != b'\\' {
                let start = self.position - 1;
                let character = self.input[start..]
                    .chars()
                    .next()
                    .context("LFM string contains invalid UTF-8")?;
                self.position = start + character.len_utf8();
                result.push(character);
                continue;
            }
            let escaped = *self
                .input
                .as_bytes()
                .get(self.position)
                .context("LFM string ends in an escape")?;
            self.position += 1;
            match escaped {
                b'"' => result.push('"'),
                b'\'' => result.push('\''),
                b'\\' => result.push('\\'),
                b'/' => result.push('/'),
                b'b' => result.push('\u{0008}'),
                b'f' => result.push('\u{000C}'),
                b'n' => result.push('\n'),
                b'r' => result.push('\r'),
                b't' => result.push('\t'),
                b'u' => {
                    let hex_end = self.position + 4;
                    let hex = self
                        .input
                        .get(self.position..hex_end)
                        .context("LFM unicode escape is incomplete")?;
                    let codepoint = u32::from_str_radix(hex, 16)
                        .context("LFM unicode escape is not hexadecimal")?;
                    let character = char::from_u32(codepoint)
                        .context("LFM unicode escape is not a scalar value")?;
                    result.push(character);
                    self.position = hex_end;
                }
                _ => bail!("LFM string has an invalid escape"),
            }
        }
        bail!("LFM string is unterminated")
    }

    fn json_container(&mut self) -> Result<serde_json::Value> {
        let start = self.position;
        let mut nesting = Vec::new();
        let mut in_string = false;
        let mut escaped = false;
        while let Some(byte) = self.input.as_bytes().get(self.position).copied() {
            self.position += 1;
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => nesting.push(byte),
                // Both closers run the identical rule, so they share an arm.
                // NOT written as a match guard (`b'}' if nesting.pop() != …`)
                // even though clippy suggests it: `pop()` mutates, and a guard
                // that fails would fall through to `_ => {}` having already
                // consumed the stack entry. Same result today, a trap later.
                //
                // The merged-away branch version WAS that guard form, for
                // `b']'` specifically. Resolved toward this one deliberately:
                // the comment above is the reason it exists.
                b'}' | b']' => {
                    let opener = if byte == b'}' { b'{' } else { b'[' };
                    if nesting.pop() != Some(opener) {
                        bail!("LFM JSON argument has mismatched delimiters");
                    }
                }
                _ => {}
            }
            if nesting.is_empty() {
                return serde_json::from_str(&self.input[start..self.position])
                    .map_err(|error| anyhow::anyhow!("LFM argument is not valid JSON: {error}"));
            }
        }
        bail!("LFM JSON argument is unterminated")
    }
}

struct NativeValueParser<'a> {
    input: &'a str,
    position: usize,
}

impl<'a> NativeValueParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, position: 0 }
    }

    fn skip_whitespace(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn take(&mut self, expected: u8) -> Result<()> {
        self.skip_whitespace();
        if self.input.as_bytes().get(self.position) != Some(&expected) {
            bail!(
                "native tool call expected {:?} at byte {}",
                expected as char,
                self.position
            );
        }
        self.position += 1;
        Ok(())
    }

    fn object(&mut self) -> Result<serde_json::Value> {
        self.take(b'{')?;
        let mut result = serde_json::Map::new();
        self.skip_whitespace();
        if self.input.as_bytes().get(self.position) == Some(&b'}') {
            self.position += 1;
            return Ok(serde_json::Value::Object(result));
        }
        loop {
            self.skip_whitespace();
            let key_start = self.position;
            while let Some(byte) = self.input.as_bytes().get(self.position) {
                if *byte == b':' || byte.is_ascii_whitespace() {
                    break;
                }
                self.position += 1;
            }
            let key = self.input[key_start..self.position].trim();
            if key.is_empty() || result.contains_key(key) {
                bail!("native tool call has an empty or duplicate argument key");
            }
            self.take(b':')?;
            let value = self.value()?;
            result.insert(key.to_string(), value);
            self.skip_whitespace();
            match self.input.as_bytes().get(self.position) {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    break;
                }
                _ => bail!("native tool call expected ',' or '}}' after an argument"),
            }
        }
        Ok(serde_json::Value::Object(result))
    }

    fn array(&mut self) -> Result<serde_json::Value> {
        self.take(b'[')?;
        let mut result = Vec::new();
        self.skip_whitespace();
        if self.input.as_bytes().get(self.position) == Some(&b']') {
            self.position += 1;
            return Ok(serde_json::Value::Array(result));
        }
        loop {
            result.push(self.value()?);
            self.skip_whitespace();
            match self.input.as_bytes().get(self.position) {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    break;
                }
                _ => bail!("native tool call expected ',' or ']' after an array value"),
            }
        }
        Ok(serde_json::Value::Array(result))
    }

    fn value(&mut self) -> Result<serde_json::Value> {
        self.skip_whitespace();
        if self.input[self.position..].starts_with("<escape>") {
            self.position += "<escape>".len();
            let end = self.input[self.position..]
                .find("<escape>")
                .map(|offset| self.position + offset)
                .context("unterminated native escaped string")?;
            let encoded = &self.input[self.position..end];
            let value = serde_json::from_str(&format!("\"{encoded}\""))
                .context("native escaped string is not valid JSON")?;
            self.position = end + "<escape>".len();
            return Ok(serde_json::Value::String(value));
        }
        match self.input.as_bytes().get(self.position) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(_) => {
                let start = self.position;
                while let Some(byte) = self.input.as_bytes().get(self.position) {
                    if matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace() {
                        break;
                    }
                    self.position += 1;
                }
                let raw = self.input[start..self.position].trim();
                if raw.is_empty() {
                    bail!("native tool call has an empty argument value");
                }
                serde_json::from_str(raw)
                    .map_err(|error| anyhow::anyhow!("native argument is not valid JSON: {error}"))
            }
            None => bail!("native tool call ended before an argument value"),
        }
    }
}

/// Validate a model-produced argument object against the tool's JSON schema.
///
/// llama.cpp's grammar is the first line of defence. This second check is
/// deliberately strict about the parts of JSON Schema that describe the
/// shape of tool arguments, so a malformed or hallucinated call cannot be
/// repaired into a plausible one after generation.
fn validate_json_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<()> {
    if let Some(constant) = schema.get("const")
        && value != constant
    {
        bail!("{path} does not equal the schema const");
    }
    if let Some(enum_values) = schema.get("enum") {
        let values = enum_values
            .as_array()
            .context("tool schema enum must be an array")?;
        if !values.iter().any(|candidate| candidate == value) {
            bail!("{path} is not one of the allowed enum values");
        }
    }

    if let Some(any_of) = schema.get("anyOf").or_else(|| schema.get("oneOf")) {
        let variants = any_of
            .as_array()
            .context("tool schema anyOf/oneOf must be an array")?;
        if !variants
            .iter()
            .any(|variant| validate_json_schema(value, variant, path).is_ok())
        {
            bail!("{path} does not match any schema alternative");
        }
    }
    if let Some(all_of) = schema.get("allOf") {
        for variant in all_of
            .as_array()
            .context("tool schema allOf must be an array")?
        {
            validate_json_schema(value, variant, path)?;
        }
    }

    if let Some(type_value) = schema.get("type") {
        let type_matches = if let Some(type_name) = type_value.as_str() {
            json_type_matches(value, type_name)
        } else if let Some(type_names) = type_value.as_array() {
            type_names
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|type_name| json_type_matches(value, type_name))
        } else {
            bail!("tool schema type at {path} must be a string or array");
        };
        if !type_matches {
            bail!("{path} has the wrong JSON type");
        }
    }

    validate_json_constraints(value, schema, path)?;

    if let Some(properties) = schema.get("properties") {
        let properties = properties
            .as_object()
            .context("tool schema properties must be an object")?;
        let object = value
            .as_object()
            .context(format!("{path} must be an object"))?;
        if let Some(required) = schema.get("required") {
            for name in required
                .as_array()
                .context("tool schema required must be an array")?
            {
                let name = name
                    .as_str()
                    .context("tool schema required names must be strings")?;
                if !object.contains_key(name) {
                    bail!("{path} is missing required property {name:?}");
                }
            }
        }
        for (name, child_schema) in properties {
            if let Some(child) = object.get(name) {
                validate_json_schema(child, child_schema, &format!("{path}.{name}"))?;
            }
        }
        if schema
            .get("additionalProperties")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            for name in object.keys() {
                if !properties.contains_key(name) {
                    bail!("{path} contains unexpected property {name:?}");
                }
            }
        } else if let Some(additional_schema) = schema
            .get("additionalProperties")
            .filter(|value| value.is_object())
        {
            for (name, child) in object {
                if !properties.contains_key(name) {
                    validate_json_schema(child, additional_schema, &format!("{path}.{name}"))?;
                }
            }
        }
    } else if schema.get("required").is_some() {
        bail!("tool schema required is present without object properties at {path}");
    }

    if let Some(items) = schema.get("items") {
        let array = value
            .as_array()
            .context(format!("{path} must be an array"))?;
        for (index, child) in array.iter().enumerate() {
            validate_json_schema(child, items, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_json_constraints(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<()> {
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema_number(schema, "minimum")? {
            let exclusive = schema
                .get("exclusiveMinimum")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if (exclusive && number <= minimum) || (!exclusive && number < minimum) {
                bail!("{path} violates the schema minimum {minimum}");
            }
        }
        if let Some(minimum) = schema_number(schema, "exclusiveMinimum")?
            && number <= minimum
        {
            bail!("{path} violates the schema exclusiveMinimum {minimum}");
        }
        if let Some(maximum) = schema_number(schema, "maximum")? {
            let exclusive = schema
                .get("exclusiveMaximum")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if (exclusive && number >= maximum) || (!exclusive && number > maximum) {
                bail!("{path} violates the schema maximum {maximum}");
            }
        }
        if let Some(maximum) = schema_number(schema, "exclusiveMaximum")?
            && number >= maximum
        {
            bail!("{path} violates the schema exclusiveMaximum {maximum}");
        }
        if let Some(multiple) = schema_number(schema, "multipleOf")? {
            if multiple <= 0.0 {
                bail!("tool schema multipleOf at {path} must be positive");
            }
            let quotient = number / multiple;
            if (quotient - quotient.round()).abs() > f64::EPSILON * quotient.abs().max(1.0) {
                bail!("{path} violates the schema multipleOf {multiple}");
            }
        }
    }

    if let Some(string) = value.as_str() {
        validate_count_constraint(string.chars().count(), schema, "minLength", path)?;
        validate_max_count_constraint(string.chars().count(), schema, "maxLength", path)?;
    }
    if let Some(array) = value.as_array() {
        validate_count_constraint(array.len(), schema, "minItems", path)?;
        validate_max_count_constraint(array.len(), schema, "maxItems", path)?;
        if schema
            .get("uniqueItems")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && array
                .iter()
                .enumerate()
                .any(|(index, item)| array[..index].contains(item))
        {
            bail!("{path} violates the schema uniqueItems constraint");
        }
    }
    if let Some(object) = value.as_object() {
        validate_count_constraint(object.len(), schema, "minProperties", path)?;
        validate_max_count_constraint(object.len(), schema, "maxProperties", path)?;
    }
    Ok(())
}

fn schema_number(schema: &serde_json::Value, name: &str) -> Result<Option<f64>> {
    match schema.get(name) {
        None | Some(serde_json::Value::Bool(_)) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .context(format!("tool schema {name} must be a number")),
    }
}

fn validate_count_constraint(
    actual: usize,
    schema: &serde_json::Value,
    name: &str,
    path: &str,
) -> Result<()> {
    if let Some(minimum) = schema.get(name) {
        let minimum = minimum
            .as_u64()
            .context(format!("tool schema {name} must be a non-negative integer"))?;
        if actual < minimum as usize {
            bail!("{path} violates the schema {name} {minimum}");
        }
    }
    Ok(())
}

fn validate_max_count_constraint(
    actual: usize,
    schema: &serde_json::Value,
    name: &str,
    path: &str,
) -> Result<()> {
    if let Some(maximum) = schema.get(name) {
        let maximum = maximum
            .as_u64()
            .context(format!("tool schema {name} must be a non-negative integer"))?;
        if actual > maximum as usize {
            bail!("{path} violates the schema {name} {maximum}");
        }
    }
    Ok(())
}

fn json_type_matches(value: &serde_json::Value, type_name: &str) -> bool {
    match type_name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn parse_text_tool_calls(text: &str) -> Vec<ToolCallResponse> {
    let mut calls = Vec::new();
    let mut call_idx = 0;

    // Format 1: ```tool_call JSON blocks (Claude, Gemini)
    {
        let mut rest = text;
        while let Some(start) = rest.find("```tool_call") {
            let after = &rest[start + 12..];
            let after = after.trim_start_matches(|c: char| c != '\n');
            let after = after.strip_prefix('\n').unwrap_or(after);

            if let Some(end) = after.find("```") {
                let json_str = after[..end].trim();
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let name = parsed
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = parsed
                        .get("arguments")
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "{}".to_string());

                    calls.push(ToolCallResponse {
                        id: format!("tc_{call_idx}"),
                        call_type: "function".to_string(),
                        function: FunctionCall { name, arguments },
                    });
                    call_idx += 1;
                }
                rest = &after[end + 3..];
            } else {
                break;
            }
        }
    }

    // Format 2: <tool_call><function=name><parameter=key>value</parameter></function></tool_call>
    // Used by Nvidia, Llama, and some open models
    if calls.is_empty() {
        let mut rest = text;
        while let Some(start) = rest.find("<tool_call>") {
            let after = &rest[start + 11..];
            if let Some(end) = after.find("</tool_call>") {
                let block = &after[..end];
                // Parse <function=NAME>
                if let Some(fn_start) = block.find("<function=") {
                    let fn_after = &block[fn_start + 10..];
                    let fn_name_end = fn_after.find('>').unwrap_or(fn_after.len());
                    let fn_name = fn_after[..fn_name_end].to_string();

                    // Parse all <parameter=KEY>VALUE</parameter>
                    let mut args = serde_json::Map::new();
                    let mut param_rest = fn_after;
                    while let Some(p_start) = param_rest.find("<parameter=") {
                        let p_after = &param_rest[p_start + 11..];
                        if let Some(p_name_end) = p_after.find('>') {
                            let p_name = p_after[..p_name_end].to_string();
                            let p_value_start = &p_after[p_name_end + 1..];
                            let p_value_end = p_value_start
                                .find("</parameter>")
                                .unwrap_or(p_value_start.len());
                            let p_value = p_value_start[..p_value_end].trim().to_string();
                            args.insert(p_name, serde_json::Value::String(p_value));
                            param_rest = &p_value_start[p_value_end..];
                        } else {
                            break;
                        }
                    }

                    calls.push(ToolCallResponse {
                        id: format!("tc_{call_idx}"),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: fn_name,
                            arguments: serde_json::Value::Object(args).to_string(),
                        },
                    });
                    call_idx += 1;
                }
                rest = &after[end + 12..];
            } else {
                break;
            }
        }
    }

    // Format 3: bare JSON tool call — recovery for a dropped/mangled
    // ```tool_call fence. Observed on the MARC27 /stream path: after a prose
    // preamble the model emitted `_call\n{"name":...,"arguments":{...}}` with no
    // backticks, so Formats 1-2 missed it and the call silently leaked as text.
    if calls.is_empty()
        && let Some((_, name, arguments)) = find_bare_json_tool_call(text)
    {
        calls.push(ToolCallResponse {
            id: format!("tc_{call_idx}"),
            call_type: "function".to_string(),
            function: FunctionCall { name, arguments },
        });
    }

    calls
}

/// Deduplicate tool calls by name+arguments (LLM sometimes repeats the same call).
fn dedup_tool_calls(calls: Vec<ToolCallResponse>) -> Vec<ToolCallResponse> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for call in calls {
        let key = format!("{}:{}", call.function.name, call.function.arguments);
        if seen.insert(key) {
            result.push(call);
        }
    }
    result
}

/// Strip everything from the first ```tool_call block onwards.
/// The LLM outputs preamble text, then tool calls, then hallucinated results.
/// We only keep the preamble — tool results come from actual execution.
fn strip_tool_call_blocks(text: &str) -> String {
    // Truncate at first tool_call — everything after is hallucination
    // Handle both ```tool_call (Claude/Gemini) and <tool_call> (Nvidia/Llama)
    let fenced = text.find("```tool_call");
    let xml = text.find("<tool_call>");
    // Also strip a bare/mangled-fence tool call (Format 3), truncating at its
    // marker so `_call\n{...}` doesn't survive into visible content.
    let bare = find_bare_json_tool_call(text).map(|(start, _, _)| start);
    let first = [fenced, xml, bare].into_iter().flatten().min();
    if let Some(start) = first {
        return text[..start].trim().to_string();
    }

    // No tool calls — return as-is (dead code path kept for safety)
    let mut result = String::new();
    let mut rest = text;

    while let Some(start) = rest.find("```tool_call") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 12..];
        if let Some(end) = after.find("```") {
            rest = &after[end + 3..];
        } else {
            break;
        }
    }
    result.push_str(rest);
    result.trim().to_string()
}

#[cfg(test)]
mod tests {

    /// The BUILDER being right is not the same as the CALL SITE using it.
    /// This drives `describe_image` itself against a real socket and reads
    /// what actually went out — the check that would have caught the bound
    /// being built correctly and then never passed through.
    #[tokio::test]
    async fn describe_image_sends_the_bound_and_the_image_over_the_wire() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .match_request(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body().unwrap().to_vec()).unwrap();
                // The caller's bound, on the wire.
                body["max_tokens"].as_u64() == Some(1234)
                    // The image, on the wire — the exact bytes, not merely
                    // a well-formed data URI (an EMPTY image still produces
                    // one of those, which is the failure mode that matters).
                    && body["messages"][0]["content"][1]["image_url"]["url"]
                        .as_str()
                        .and_then(|u| u.strip_prefix("data:image/png;base64,"))
                        .and_then(|b| {
                            use base64::Engine as _;
                            base64::engine::general_purpose::STANDARD.decode(b).ok()
                        })
                        .is_some_and(|bytes| bytes == b"\x89PNG\r\n\x1a\nbytes")
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"choices":[{"message":{"content":"transcribed"}}]}"#)
            .create_async()
            .await;

        let client = LlmClient::new(LlmConfig {
            base_url: format!("{}/v1", server.url()),
            model: "gemma".into(),
            ..Default::default()
        });
        let text = client
            .describe_image("read this", b"\x89PNG\r\n\x1a\nbytes", 1234)
            .await
            .expect("the mock must match; a mismatch means the request was wrong");
        assert_eq!(text, "transcribed");
        mock.assert_async().await;
    }

    /// The two things that silently broke while this was being written: the
    /// image not actually being attached (a text-only request comes back with
    /// a fluent, wholly invented description of a page the model never saw),
    /// and the caller's output bound not reaching the wire (a looping model
    /// then generates until the context is exhausted — measured at five
    /// minutes for one tile before it could even be judged degenerate).
    #[test]
    fn vision_request_carries_the_image_and_the_caller_bound() {
        let client = LlmClient::new(LlmConfig {
            base_url: "http://localhost:8081/v1".into(),
            model: "gemma".into(),
            ..Default::default()
        });
        let png = b"\x89PNG\r\n\x1a\nfake image bytes";
        let body = client.vision_request_body("read this", png, 2048);

        // The image is attached, as a data URI, in the user turn.
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().expect("a url");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(url.trim_start_matches("data:image/png;base64,"))
            .expect("decodable");
        assert_eq!(decoded, png, "the exact bytes must survive to the wire");

        // The caller's bound reaches the request.
        assert_eq!(body["max_tokens"].as_u64(), Some(2048));
        // Determinism: the same page must read the same way twice.
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
    }

    /// The smaller of the caller's bound and the operator's cap wins, in
    /// BOTH directions — neither may silently widen the other.
    #[test]
    fn the_smaller_of_caller_bound_and_operator_cap_wins() {
        let client = LlmClient::new(LlmConfig {
            base_url: "http://localhost:8081/v1".into(),
            model: "gemma".into(),
            max_output_tokens: Some(256),
            ..Default::default()
        });
        // Operator cap (256) is tighter than the caller's 2048.
        let tight = client.vision_request_body("p", b"x", 2048);
        assert_eq!(tight["max_tokens"].as_u64(), Some(256));
        // Caller's bound (64) is tighter than the operator cap.
        let tighter = client.vision_request_body("p", b"x", 64);
        assert_eq!(tighter["max_tokens"].as_u64(), Some(64));
    }
    use super::*;

    #[test]
    fn llm_client_constructs_with_defaults() {
        let config = LlmConfig::default();
        let _client = LlmClient::new(config);
    }

    #[test]
    fn provider_selection_requires_the_explicit_gguf_sentinel() {
        assert_eq!(choose_backend(LOCAL_GGUF_URL), BackendChoice::LocalGguf);
        assert_eq!(choose_backend("gguf://local/"), BackendChoice::LocalGguf);
        assert_eq!(
            choose_backend("http://localhost:8080/v1"),
            BackendChoice::Http
        );
        assert_eq!(
            choose_backend("https://api.openai.com/v1"),
            BackendChoice::Http
        );
    }

    #[tokio::test]
    async fn hosted_prompt_influence_is_explicitly_unavailable_without_a_request() {
        let client = LlmClient::new(LlmConfig {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "fixture".to_string(),
            ..LlmConfig::default()
        });

        let outcome = client
            .score_local_tool_influence(&[], &[], &[])
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            LocalPromptInfluenceOutcome::Unavailable {
                code: LocalPromptInfluenceUnavailableCode::HostedBackend,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn hosted_model_identity_is_explicitly_unavailable_without_a_request() {
        let client = LlmClient::new(LlmConfig {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "fixture".to_string(),
            ..LlmConfig::default()
        });

        let outcome = client.local_model_identity().await.unwrap();
        assert!(matches!(
            outcome,
            LocalModelIdentityOutcome::Unavailable {
                code: LocalPromptInfluenceUnavailableCode::HostedBackend,
                ..
            }
        ));
    }

    #[cfg(feature = "local-inference")]
    fn write_context_only_gguf(path: &std::path::Path, context_window: u32) {
        use std::io::Write as _;

        fn write_string(file: &mut std::fs::File, value: &str) {
            file.write_all(&(value.len() as u64).to_le_bytes()).unwrap();
            file.write_all(value.as_bytes()).unwrap();
        }

        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"GGUF").unwrap();
        file.write_all(&3_u32.to_le_bytes()).unwrap();
        file.write_all(&0_u64.to_le_bytes()).unwrap(); // tensor count
        file.write_all(&2_u64.to_le_bytes()).unwrap(); // metadata count
        write_string(&mut file, "general.architecture");
        file.write_all(&8_u32.to_le_bytes()).unwrap(); // GGUF_TYPE_STRING
        write_string(&mut file, "prismtest");
        write_string(&mut file, "prismtest.context_length");
        file.write_all(&4_u32.to_le_bytes()).unwrap(); // GGUF_TYPE_UINT32
        file.write_all(&context_window.to_le_bytes()).unwrap();
    }

    /// The local client's effective context is the value every upstream tool
    /// budget must consume; a catalog fallback must never override GGUF truth.
    #[cfg(feature = "local-inference")]
    #[test]
    fn local_backend_budgets_tools_against_gguf_context_not_unknown_128k() {
        let temp = tempfile::tempdir().unwrap();
        let model = temp.path().join("context-only.gguf");
        write_context_only_gguf(&model, 8_192);

        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: model.display().to_string(),
            context_window: Some(128_000),
            ..LlmConfig::default()
        });

        assert_eq!(client.config().context_window, Some(8_192));
    }

    #[cfg(not(feature = "local-inference"))]
    #[tokio::test]
    async fn selected_gguf_backend_never_falls_back_when_feature_is_absent() {
        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: "missing.gguf".to_string(),
            ..LlmConfig::default()
        });
        let error = client.generate("hello").await.unwrap_err().to_string();
        assert!(error.contains("built without embedded inference"));
        assert!(error.contains("No remote endpoint was tried"));
    }

    #[cfg(not(feature = "local-inference"))]
    #[tokio::test]
    async fn feature_disabled_prompt_influence_is_explicitly_unavailable() {
        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: "missing.gguf".to_string(),
            ..LlmConfig::default()
        });

        let outcome = client
            .score_local_tool_influence(&[], &[], &[])
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            LocalPromptInfluenceOutcome::Unavailable {
                code: LocalPromptInfluenceUnavailableCode::LocalInferenceFeatureDisabled,
                ..
            }
        ));
    }

    #[cfg(not(feature = "local-inference"))]
    #[tokio::test]
    async fn feature_disabled_model_identity_is_explicitly_unavailable() {
        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: "missing.gguf".to_string(),
            ..LlmConfig::default()
        });

        let outcome = client.local_model_identity().await.unwrap();
        assert!(matches!(
            outcome,
            LocalModelIdentityOutcome::Unavailable {
                code: LocalPromptInfluenceUnavailableCode::LocalInferenceFeatureDisabled,
                ..
            }
        ));
    }

    #[cfg(not(feature = "local-inference"))]
    #[tokio::test]
    async fn selected_gguf_tool_turn_reports_local_capability_without_remote_fallback() {
        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: "missing.gguf".to_string(),
            ..LlmConfig::default()
        });
        let tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "lookup".to_string(),
                description: "Look something up".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        };
        let error = client
            .chat_with_tools(&[], &[tool])
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("does not currently support tool calling"));
        assert!(error.contains("built without embedded inference"));
        assert!(error.contains("No remote endpoint was tried"));
    }

    #[cfg(not(feature = "local-inference"))]
    #[tokio::test]
    async fn local_tool_turn_never_reaches_an_http_client() {
        let client = LlmClient::new(LlmConfig {
            base_url: LOCAL_GGUF_URL.to_string(),
            model: "missing.gguf".to_string(),
            ..LlmConfig::default()
        });
        assert!(client.http_client().is_err());
        let error = client
            .chat_with_tools(
                &[],
                &[ToolDefinition {
                    tool_type: "function".to_string(),
                    function: FunctionDef {
                        name: "lookup".to_string(),
                        description: "Look something up".to_string(),
                        parameters: serde_json::json!({"type": "object"}),
                    },
                }],
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("No remote endpoint was tried"));
    }

    #[test]
    fn local_tool_response_maps_only_a_strict_known_call() {
        let tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "lookup".to_string(),
                description: "Look something up".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
        };
        let response = LlmClient::local_chat_response(
            local::LocalGeneration {
                text: r#"{"kind":"tool_call","name":"lookup","arguments":{"query":"titanium"}}"#
                    .to_string(),
                prompt_tokens: 3,
                completion_tokens: 4,
                prefill_wall_time_micros: 5,
                decode_wall_time_micros: 7,
            },
            std::slice::from_ref(&tool),
            "test-model",
            false,
            &std::sync::atomic::AtomicU64::new(0),
        )
        .unwrap();
        let call = &response.message.tool_calls.unwrap()[0];
        assert_eq!(call.function.name, "lookup");
        assert_eq!(call.function.arguments, r#"{"query":"titanium"}"#);
        assert_eq!(
            response.generation_metrics,
            Some(GenerationPhaseMetrics {
                prefill_wall_time_micros: 5,
                decode_wall_time_micros: 7,
            })
        );

        let error = LlmClient::local_chat_response(
            local::LocalGeneration {
                text: r#"{"kind":"tool_call","name":"hallucinated","arguments":{}}"#.to_string(),
                prompt_tokens: 1,
                completion_tokens: 1,
                prefill_wall_time_micros: 0,
                decode_wall_time_micros: 0,
            },
            std::slice::from_ref(&tool),
            "test-model",
            false,
            &std::sync::atomic::AtomicU64::new(0),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown tool"));
        assert!(error.contains("no remote endpoint was tried"));
    }

    /// Regression H2: schema bounds are a rejection boundary, never a hint to
    /// clamp a model-controlled tool argument after parsing.
    #[test]
    fn local_tool_response_rejects_out_of_range_limit_instead_of_clamping() {
        let tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "find_tools".to_string(),
                description: "discover tools".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 25}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
        };
        let error = LlmClient::local_chat_response(
            local::LocalGeneration {
                text: r#"{"kind":"tool_call","name":"find_tools","arguments":{"query":"materials","limit":26}}"#.to_string(),
                prompt_tokens: 1,
                completion_tokens: 1,
                prefill_wall_time_micros: 0,
                decode_wall_time_micros: 0,
            },
            &[tool],
            "test-model",
            false,
            &std::sync::atomic::AtomicU64::new(0),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("maximum"), "{error}");
        assert!(error.contains("rejected"), "{error}");
        assert!(!error.contains("clamp"), "{error}");
    }

    #[test]
    fn native_tool_call_parser_is_strict_and_preserves_json_types() {
        let (name, arguments) = parse_native_tool_call(
            "<start_function_call>call:lookup{query:<escape>titanium<escape>,limit:2}<end_function_call>",
        )
        .unwrap();
        assert_eq!(name, "lookup");
        assert_eq!(arguments["query"], "titanium");
        assert_eq!(arguments["limit"], 2);
        assert!(parse_native_tool_call(
            "<start_function_call>call:lookup{query:<escape>titanium<escape>}<end_function_call>trailing"
        )
        .is_err());
    }

    #[test]
    fn lfm_native_tool_call_parser_preserves_json_escaped_text() {
        let (name, arguments) = parse_native_tool_call(
            "<|tool_call_start|>[find_tools(query=\"alpha, {beta} <gamma>\", limit=2)]<|tool_call_end|>",
        )
        .unwrap();
        assert_eq!(name, "find_tools");
        assert_eq!(arguments["query"], "alpha, {beta} <gamma>");
        assert_eq!(arguments["limit"], 2);
    }

    #[test]
    fn gemma_native_tool_call_parser_preserves_nested_types() {
        let (name, arguments) = parse_native_tool_call(
            r#"<|tool_call>call:lookup{query:<|"|>titanium<|"|>,filters:{limit:2,exact:true}}<tool_call|>"#,
        )
        .unwrap();
        assert_eq!(name, "lookup");
        assert_eq!(arguments["query"], "titanium");
        assert_eq!(arguments["filters"]["limit"], 2);
        assert_eq!(arguments["filters"]["exact"], true);
    }

    /// The close-delimiter arm had NO test — `grep "mismatched delimiters"`
    /// matched only the `bail!`. Two closers were merged into one arm to clear
    /// a clippy gate, so this pins what the merge must preserve: each closer
    /// derives the opener IT closes.
    ///
    /// Only the accept case is asserted, and that is deliberate. I wrote the
    /// obvious reject case first (`x=[1, 2}`) and mutation-checked it: with the
    /// kind-check replaced by `pop().is_none()` the test still PASSED, because
    /// any crossed-delimiter input is also invalid JSON and `serde_json` rejects
    /// it one line later. That assertion could not fail, so it is not here.
    /// The kind-check is defence in depth, not independently observable through
    /// this API. The accept case IS observable: swapping the opener derivation
    /// to `if byte == b'}' { b'[' } else { b'{' }` fails this test.
    #[test]
    fn lfm_argument_parser_accepts_correctly_nested_delimiters() {
        let (_, arguments) = parse_native_tool_call(
            "<|tool_call_start|>[f(x=[1, 2], y={\"k\": 3})]<|tool_call_end|>",
        )
        .expect("correctly nested delimiters must still parse");
        assert_eq!(arguments["x"][1], 2);
        assert_eq!(arguments["y"]["k"], 3);
    }

    #[test]
    fn local_tool_response_maps_a_strict_final_answer() {
        let response = LlmClient::local_chat_response(
            local::LocalGeneration {
                text: r#"{"kind":"final","content":"done"}"#.to_string(),
                prompt_tokens: 1,
                completion_tokens: 2,
                prefill_wall_time_micros: 0,
                decode_wall_time_micros: 0,
            },
            &[ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDef {
                    name: "lookup".to_string(),
                    description: "Look something up".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            }],
            "test-model",
            true,
            &std::sync::atomic::AtomicU64::new(0),
        )
        .unwrap();
        assert_eq!(response.message.content.as_deref(), Some("done"));
        assert!(response.message.tool_calls.is_none());
    }

    /// Regression C1: every local tool call in a session needs its own id, so
    /// pending results can be attributed to their own call when the next turn
    /// renders. A constant `local_call_0` collided and corrupted history.
    #[test]
    fn local_tool_calls_get_distinct_ids_within_one_session() {
        let tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "lookup".to_string(),
                description: "Look something up".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        };
        let counter = std::sync::atomic::AtomicU64::new(0);
        let ids = (0..3)
            .map(|_| {
                let response = LlmClient::local_chat_response(
                    local::LocalGeneration {
                        text: r#"{"kind":"tool_call","name":"lookup","arguments":{}}"#.to_string(),
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        prefill_wall_time_micros: 0,
                        decode_wall_time_micros: 0,
                    },
                    std::slice::from_ref(&tool),
                    "test-model",
                    false,
                    &counter,
                )
                .unwrap();
                response.message.tool_calls.unwrap()[0].id.clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, ["local_call_1", "local_call_2", "local_call_3"]);
    }

    /// The base URL is data, not a hint. Whatever path a vendor mounts its
    /// OpenAI-compatible surface at, only `/chat/completions` is appended.
    #[test]
    fn chat_completions_url_appends_only_the_path() {
        for (base, expected) in [
            // Mounted at /v1 — the case the old code got right.
            (
                "https://api.openai.com/v1",
                "https://api.openai.com/v1/chat/completions",
            ),
            // Trailing slash is trimmed, not doubled.
            (
                "https://api.openai.com/v1/",
                "https://api.openai.com/v1/chat/completions",
            ),
            // NOT mounted at /v1 — the 404s this function used to cause.
            (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            ),
            (
                "https://api.z.ai/api/paas/v4",
                "https://api.z.ai/api/paas/v4/chat/completions",
            ),
            // Mounted below /v1.
            (
                "https://api.groq.com/openai/v1",
                "https://api.groq.com/openai/v1/chat/completions",
            ),
        ] {
            assert_eq!(chat_completions_url(base), expected, "base {base}");
        }
    }

    /// Named regression pin: no `/v1` may ever be synthesised again. A base
    /// that already carries a version segment must not grow a second one.
    #[test]
    fn chat_completions_url_never_invents_a_version_segment() {
        for base in [
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://api.z.ai/api/paas/v4",
            "https://api.cohere.ai/compatibility/v1",
            "https://llm.corp.internal/openai",
        ] {
            let url = chat_completions_url(base);
            assert_eq!(
                url,
                format!("{base}/chat/completions"),
                "the client must not rewrite {base}"
            );
            assert!(
                !url.contains("/v1/chat/completions") || base.ends_with("/v1"),
                "a /v1 was synthesised into {url}"
            );
        }
    }

    /// The client and the LlmConfig it was built from must agree — the
    /// method is what the four request paths actually call.
    #[test]
    fn client_uses_its_configured_base_verbatim() {
        let client = LlmClient::new(LlmConfig {
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            model: "gemini-2.5-flash".into(),
            ..Default::default()
        });
        assert_eq!(
            client.chat_completions_url(),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
    }

    #[test]
    fn auth_header_present_when_key_set() {
        let config = LlmConfig {
            api_key: Some("sk-test123".into()),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        assert_eq!(
            client.auth_header(),
            Some(("Authorization", "Bearer sk-test123".to_string()))
        );
    }

    #[test]
    fn auth_header_routes_m27_api_key_to_x_api_key() {
        // Old raw callers keep the frozen prefix behavior.
        let config = LlmConfig {
            api_key: Some("m27_live_abc123".into()),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        assert_eq!(
            client.auth_header(),
            Some(("X-API-Key", "m27_live_abc123".to_string()))
        );
    }

    #[test]
    fn explicit_api_key_kind_accepts_provider_defined_shape() {
        let client = LlmClient::new(LlmConfig {
            api_key: Some("provider-defined-key".into()),
            credential_kind: Some(LlmCredentialKind::ApiKey),
            ..Default::default()
        });
        assert_eq!(
            client.auth_header(),
            Some(("X-API-Key", "provider-defined-key".to_string()))
        );
    }

    #[test]
    fn explicit_bearer_kind_overrides_legacy_prefix_heuristic() {
        let client = LlmClient::new(LlmConfig {
            api_key: Some("m27_session-shaped".into()),
            credential_kind: Some(LlmCredentialKind::Bearer),
            ..Default::default()
        });
        assert_eq!(
            client.auth_header(),
            Some(("Authorization", "Bearer m27_session-shaped".to_string()))
        );
    }

    #[tokio::test]
    async fn explicit_provider_api_key_reaches_llm_http_as_x_api_key() {
        let mut server = mockito::Server::new_async().await;
        let request = server
            .mock("GET", "/v1/models")
            .match_header("x-api-key", "provider-defined-key")
            .with_status(200)
            .with_body("{}")
            .create_async()
            .await;
        let client = LlmClient::new(LlmConfig {
            base_url: server.url(),
            api_key: Some("provider-defined-key".into()),
            credential_kind: Some(LlmCredentialKind::ApiKey),
            ..Default::default()
        });

        client.health_check().await.unwrap();
        request.assert_async().await;
    }

    #[test]
    fn auth_header_none_when_no_key() {
        let config = LlmConfig::default();
        let client = LlmClient::new(config);
        assert!(client.auth_header().is_none());
    }

    #[test]
    fn prism_imposes_no_output_ceiling_of_its_own() {
        // Was `effective_max_tokens_defaults_to_4096`, which pinned a cost
        // guard as if it were a model limit. Output is metered and billed per
        // token, so counting is the control — truncating is not. A 4096
        // default silently broke every model that reasons before answering:
        // Gemma 4 12B spent ~2.7k tokens of reasoning against it and returned
        // no JSON at all.
        let client = LlmClient::new(LlmConfig::default());
        assert!(
            client.effective_max_tokens(0) >= 100_000,
            "with no operator ceiling and no known context, PRISM must not \
             impose a limit of its own; got {}",
            client.effective_max_tokens(0)
        );
    }

    #[test]
    fn the_context_window_is_what_actually_bounds_output() {
        // The real bound, and the only one PRISM applies unasked: whatever the
        // context still has room for after the prompt and the margin.
        let config = LlmConfig {
            max_output_tokens: None,
            context_window: Some(32_768),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        assert_eq!(
            client.effective_max_tokens(8_000),
            32_768 - 8_000 - CONTEXT_MARGIN_TOKENS
        );
    }

    #[test]
    fn an_explicit_operator_ceiling_is_still_honoured() {
        // The operator may cap. PRISM may not cap on their behalf.
        let config = LlmConfig {
            max_output_tokens: Some(2_048),
            context_window: Some(200_000),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        assert_eq!(client.effective_max_tokens(1_000), 2_048);
    }

    #[test]
    fn effective_max_tokens_honors_config_when_context_roomy() {
        let config = LlmConfig {
            max_output_tokens: Some(16_384),
            context_window: Some(200_000),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        // Small prompt, huge context → the model max is the binding limit.
        assert_eq!(client.effective_max_tokens(1_000), 16_384);
    }

    #[test]
    fn effective_max_tokens_clamps_to_context_remaining() {
        // gpt-5-shaped: 128k max output but only 400k context. A 396k-token
        // prompt leaves far less than 128k of room — the clamp must bind, and
        // never exceed context − prompt − margin, nor drop below the floor.
        let config = LlmConfig {
            max_output_tokens: Some(128_000),
            context_window: Some(400_000),
            ..Default::default()
        };
        let client = LlmClient::new(config);

        // Roomy prompt: model max binds.
        assert_eq!(client.effective_max_tokens(10_000), 128_000);

        // Tight prompt: context binds. 400k − 396k − 1024 margin = 2976.
        assert_eq!(
            client.effective_max_tokens(396_000),
            400_000 - 396_000 - 1024
        );

        // Prompt bigger than the whole window: never underflows, floors at 256.
        assert_eq!(client.effective_max_tokens(500_000), 256);
    }

    #[test]
    fn extract_json_content_returns_real_content() {
        let choice = serde_json::json!({
            "finish_reason": "stop",
            "message": {"content": "{\"entities\": []}", "reasoning_content": ""}
        });
        assert_eq!(
            LlmClient::extract_json_content(&choice).unwrap(),
            "{\"entities\": []}"
        );
    }

    #[test]
    fn extract_json_content_never_falls_back_to_reasoning() {
        // Empty content + finish_reason "stop" (not length) with reasoning
        // text present must still fail rather than return the reasoning
        // text as if it were JSON.
        let choice = serde_json::json!({
            "finish_reason": "stop",
            "message": {"content": "", "reasoning_content": "I am thinking about it..."}
        });
        let err = LlmClient::extract_json_content(&choice).unwrap_err();
        assert!(err.to_string().contains("empty content"));
    }

    #[test]
    fn extract_json_content_diagnoses_thinking_model_length_cutoff() {
        // The live failure mode this fixes: a reasoning model burns the
        // whole max_tokens budget on reasoning_content and never emits
        // JSON content, ending with finish_reason=length.
        let choice = serde_json::json!({
            "finish_reason": "length",
            "message": {
                "content": "",
                "reasoning_content": "a very long chain of thought".repeat(100),
            }
        });
        let err = LlmClient::extract_json_content(&choice).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thinking"), "message was: {msg}");
        assert!(msg.contains("max_output_tokens"), "message was: {msg}");
    }

    #[test]
    fn extract_json_content_generic_empty_response_is_honest() {
        let choice = serde_json::json!({
            "finish_reason": "stop",
            "message": {"content": ""}
        });
        let err = LlmClient::extract_json_content(&choice).unwrap_err();
        assert!(err.to_string().contains("finish_reason=stop"));
    }

    /// Thinking-mode budget burn self-heals through the PRODUCTION
    /// `generate_json` dispatch: the first wire response burns the whole
    /// budget on `reasoning_content`, and the client must retry the same
    /// endpoint ONCE with `chat_template_kwargs.enable_thinking = false`
    /// (the mock for the retry only answers a request carrying that field).
    /// Removing the retry, or breaking the burn predicate, fails this test
    /// with the thinking-mode diagnosis.
    #[tokio::test]
    async fn generate_json_retries_thinking_burn_with_thinking_disabled() {
        let mut server = mockito::Server::new_async().await;
        let burn = serde_json::json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": "", "reasoning_content": "thinking… ".repeat(50)}
            }]
        });
        // Created FIRST so the retry mock (created second) is matched first;
        // the initial request lacks chat_template_kwargs and falls through
        // to this one.
        let first = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(burn.to_string())
            .expect(1)
            .create_async()
            .await;
        let retry = server
            .mock("POST", "/chat/completions")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "chat_template_kwargs": {"enable_thinking": false}
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "choices": [{
                        "finish_reason": "stop",
                        "message": {"content": "{\"facts\": []}"}
                    }]
                })
                .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        let client = LlmClient::new(LlmConfig {
            base_url: server.url(),
            model: "test-model".to_string(),
            ..LlmConfig::default()
        });
        let out = client
            .generate_json("extract facts")
            .await
            .expect("the thinking-disabled retry must recover the extraction");
        assert_eq!(out, "{\"facts\": []}");
        first.assert_async().await;
        retry.assert_async().await;
    }

    /// When the retry ALSO burns its budget on reasoning, the caller gets
    /// the original actionable diagnosis — not a success, not a generic
    /// empty-content error.
    #[tokio::test]
    async fn generate_json_reports_the_original_diagnosis_when_the_retry_fails_too() {
        let mut server = mockito::Server::new_async().await;
        let burn = serde_json::json!({
            "choices": [{
                "finish_reason": "length",
                "message": {"content": "", "reasoning_content": "thinking… ".repeat(50)}
            }]
        });
        server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(burn.to_string())
            .expect(2) // the original AND the failed retry
            .create_async()
            .await;

        let client = LlmClient::new(LlmConfig {
            base_url: server.url(),
            model: "test-model".to_string(),
            ..LlmConfig::default()
        });
        let err = client.generate_json("extract facts").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("thinking"), "message was: {msg}");
        assert!(msg.contains("max_output_tokens"), "message was: {msg}");
    }

    #[test]
    fn format1_fenced_tool_call_still_parses() {
        // Regression: the well-formed fence must keep working unchanged.
        let text = "Let me search.\n```tool_call\n{\"name\": \"web\", \"arguments\": {\"query\": \"x\"}}\n```";
        let calls = parse_text_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "web");
        assert_eq!(strip_tool_call_blocks(text), "Let me search.");
    }

    #[test]
    fn format3_recovers_mangled_underscore_call_marker() {
        // The exact live failure: after a prose preamble Claude dropped the
        // ```tool_call fence and emitted `_call\n{...}`. Must be recovered.
        let text = "The prior_art results were noisy — not useful. I'll cross-check \
                    the knowledge graph directly.\n\n_call\n{\"name\": \"knowledge\", \
                    \"arguments\": {\"action\": \"search\", \"query\": \"HfC-TaC\"}}";
        let calls = parse_text_tool_calls(text);
        assert_eq!(calls.len(), 1, "mangled-marker call must be recovered");
        assert_eq!(calls[0].function.name, "knowledge");
        assert!(calls[0].function.arguments.contains("\"action\""));
    }

    #[test]
    fn format3_recovers_bare_json_with_no_marker() {
        let text = "Let me look that up.\n{\"name\": \"web\", \"arguments\": \
                    {\"action\": \"search\", \"query\": \"x\"}}";
        let calls = parse_text_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "web");
    }

    #[test]
    fn format3_ignores_prose_json_that_is_not_a_tool_call() {
        // arguments is a string, not an object → not a call.
        assert!(
            parse_text_tool_calls("A record: {\"name\": \"Alice\", \"arguments\": \"none\"}.")
                .is_empty()
        );
        // no `arguments` key at all → not a call.
        assert!(parse_text_tool_calls("Config: {\"name\": \"widget\", \"value\": 3}").is_empty());
        // name contains whitespace → not a tool name.
        assert!(parse_text_tool_calls("{\"name\": \"John Smith\", \"arguments\": {}}").is_empty());
    }

    #[test]
    fn strip_removes_mangled_call_region_keeps_preamble() {
        let text = "Cross-checking the graph directly.\n\n_call\n\
                    {\"name\": \"knowledge\", \"arguments\": {\"action\": \"search\"}}";
        assert_eq!(
            strip_tool_call_blocks(text),
            "Cross-checking the graph directly."
        );
    }

    /// Pin the tool names the guidance block still names verbatim.
    ///
    /// Each name below MUST be a real tool registered in `app/tools/*.py`
    /// (`registry.register(Tool(name=...))`). When a tool is renamed,
    /// update both this list AND [`TOOL_GUIDANCE_BLOCK`] together —
    /// otherwise the LLM gets a stale name in its system prompt and
    /// hallucinates calls to it (we shipped this exact bug: the old
    /// `search_materials` line stayed in the prompt for ~2 rounds after
    /// the tool was renamed to `materials_search`, and gemini-3.1 dutifully
    /// called the dead name on every materials request).
    ///
    /// Shorter than it used to be: the "quick reference" list of 8 names was
    /// deleted along with the rest of the name-only surrogate — the request
    /// now carries real schemas, so a prose name list is both redundant and
    /// the only remaining way to ship a stale name.
    const GUIDANCE_TOOL_NAMES: &[&str] = &[
        "find_tools",
        "materials_search",
        "predict",
        "prior_art_search",
        "research",
        "query_platform",
        "knowledge_entity",
        "web",
    ];

    #[test]
    fn guidance_tool_names_appear_in_prompt() {
        for name in GUIDANCE_TOOL_NAMES {
            assert!(
                TOOL_GUIDANCE_BLOCK.contains(&format!("`{name}`")),
                "tool `{name}` missing from the guidance block — \
                 either restore it or remove it from GUIDANCE_TOOL_NAMES"
            );
        }
    }

    /// The guidance block must never re-grow a tool inventory: that summary
    /// (names only, 4 per category) is the defect this change removed.
    #[test]
    fn guidance_block_carries_no_tool_inventory() {
        for banned in [
            "tools available across these categories",
            "... and ",
            "```tool_call",
        ] {
            assert!(
                !TOOL_GUIDANCE_BLOCK.contains(banned),
                "`{banned}` is back in the guidance block — the tool surface is \
                 the request's `tools` array, not prose"
            );
        }
    }

    /// Pin the long-horizon orchestration patterns shipped in PRs #109 and #111.
    ///
    /// These markers exist because the BimoTech / Fraunhofer end-to-end test
    /// surfaced two real failure modes: (1) the LLM gave up after one tool
    /// error, and (2) the LLM wrapped up after 2-3 tool calls on a question
    /// that needed 8-30. The fixes are SYSTEM PROMPT TEXT — they have no
    /// other code path. If a future refactor silently drops these strings,
    /// the regression isn't visible until a customer hits it. This test
    /// catches the silent-drop case.
    ///
    /// Backed by the literature: arxiv 2604.11978 (Long-Horizon Mirage),
    /// arxiv 2603.29231 (Beyond pass@1), arxiv 2512.24601 (RLM `FINAL()`),
    /// arxiv 2605.02572 (empirical horizon-length study).
    #[test]
    fn long_horizon_orchestration_markers_present() {
        let block = TOOL_GUIDANCE_BLOCK;
        let required_markers: &[(&str, &str)] = &[
            ("DO NOT GIVE UP", "recovery-rules header from #109"),
            (
                "NEVER respond with empty content",
                "anti-early-termination rule from #109",
            ),
            (
                "Tool-composition patterns",
                "composition cookbook header from #109",
            ),
            (
                "Long-horizon discipline",
                "long-horizon section header from #111",
            ),
            ("Plan first, in writing", "plan-emission rule from #111"),
            ("FINAL ANSWER:", "deliberate-completion marker from #111"),
            // Tightened from a bare `research` substring (which would match
            // `research`, `prior_art_search`, `research_query`, and 12 other
            // unrelated occurrences — the previous form was effectively a
            // no-op). The new pin is the specific guidance string that
            // PR #111 added to direct the agent at the RLM tool for deep
            // multi-hop questions.
            (
                "Use `research` for deep multi-hop questions",
                "RLM-as-default rule from #111",
            ),
            // PR #114 — vendor-PDF clarifier. Without these pins, the
            // entire "where materials data actually lives" block can be
            // silently deleted with green tests. The end-to-end Test 3
            // trace (2026-05-10 ODS-alloy prompt) confirmed the agent
            // genuinely changes behaviour when this section is present.
            (
                "where materials data actually lives",
                "vendor-PDF clarifier section header from #114",
            ),
            ("Vendor PDFs", "vendor-PDF do-not-call rule from #114"),
            (
                "Do not chain guesses at vendor URLs",
                "anti-URL-enumeration rule from #114",
            ),
            // PR #115 — search engine + OSTI blacklist. Concrete domain
            // names are pinned because the rule's effectiveness depends on
            // the agent reading them verbatim.
            (
                "Search engines + government repos block",
                "search-engine blacklist section header from #115",
            ),
            (
                "google.com/search",
                "blacklisted Google search URL pattern from #115",
            ),
            ("osti.gov", "blacklisted OSTI repo pattern from #115"),
            (
                "CrossRef API",
                "allowed-fallback CrossRef pointer from #115",
            ),
        ];
        for (marker, why) in required_markers {
            assert!(
                block.contains(marker),
                "long-horizon marker `{marker}` missing from prompt block ({why}). \
                 If you intentionally removed it, update this test. If not, \
                 you've silently regressed PR #109, #111, #114, or #115."
            );
        }
    }

    /// The "when NOT to call tools" carve-out must not become a licence to
    /// answer materials questions from memory.
    ///
    /// This block is injected as a SECOND system message alongside the PRISM
    /// agent prompt (`prism_agent::prompts`), which says "You may not answer a
    /// scientific or platform question from memory". Before this pin the two
    /// messages contradicted each other in the same request — this block
    /// excused "general knowledge questions", and a materials question reads as
    /// one. Two contradictory system messages resolve toward the cheaper
    /// instruction, which is the fabrication.
    #[test]
    fn tool_carve_out_does_not_license_answering_from_memory() {
        let block = TOOL_GUIDANCE_BLOCK;
        assert!(
            !block.contains("general knowledge questions"),
            "the 'general knowledge questions' carve-out lets a materials \
             question be answered from memory — contradicts the agent prompt"
        );
        for marker in [
            "NOT a licence to answer from memory",
            "never fill the \
             gap from memory",
        ] {
            assert!(
                block.contains(marker),
                "retrieval-discipline marker `{marker}` missing from the tool block"
            );
        }
    }

    #[test]
    fn guidance_block_does_not_mention_renamed_tools() {
        // Belt-and-braces: explicit deny-list of names we've previously
        // renamed and don't want sneaking back into the prompt.
        let block = TOOL_GUIDANCE_BLOCK;
        for stale in &[
            "search_materials",
            "knowledge_search",
            "predict_property",
            "web_search",
            "web_read",
            "literature_search",
            "research_query",
            "semantic_search",
        ] {
            assert!(
                !block.contains(&format!("`{stale}`")),
                "stale tool name `{stale}` reappeared in the guidance block"
            );
        }
    }

    /// Cross-check the curated list against the actual Python tool registry.
    ///
    /// The two earlier tests catch *internal* drift (curated list vs prompt
    /// text). They DO NOT catch the worst case: someone renames a tool in
    /// `app/tools/*.py` and forgets to update the prompt — both sides of
    /// the internal check still agree, but the LLM gets a name that no
    /// longer matches reality. That's exactly how PR #91's `search_materials`
    /// bug shipped.
    ///
    /// This test reads `app/tools/*.py` directly and confirms every name in
    /// `GUIDANCE_TOOL_NAMES` appears as a `name="..."` registration.
    /// No Python subprocess, no runtime cost — just file IO at test time.
    ///
    /// If `app/tools/` is missing (e.g., someone runs the test outside a
    /// full PRISM checkout), the test soft-skips so it doesn't break
    /// downstream builds of the crate in isolation.
    #[test]
    fn guidance_tool_names_are_registered_in_python() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let tools_dir = std::path::Path::new(manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|p| p.join("app").join("tools"));

        let Some(tools_dir) = tools_dir else {
            eprintln!("skipping cross-check: cannot resolve workspace root");
            return;
        };
        if !tools_dir.is_dir() {
            eprintln!(
                "skipping cross-check: app/tools/ not found at {}",
                tools_dir.display()
            );
            return;
        }

        // Recursively scan every .py file under app/tools/ for
        // `name="..."` (or `name='...'`) tokens. The matcher is
        // intentionally simple — looking for the exact registration
        // pattern `name="<identifier>"` on its own line, which is
        // how every existing tool registers (see e.g.
        // `app/tools/research.py:6` "        name="research",").
        let mut registered = std::collections::BTreeSet::<String>::new();
        let mut stack = vec![tools_dir.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "py") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for line in text.lines() {
                    let trimmed = line.trim();
                    // Match either name="x" or name='x'.
                    for quote in ['"', '\''] {
                        let prefix = format!("name={quote}");
                        if let Some(rest) = trimmed.strip_prefix(&prefix)
                            && let Some(end) = rest.find(quote)
                        {
                            let name = &rest[..end];
                            if !name.is_empty()
                                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            {
                                registered.insert(name.to_string());
                            }
                        }
                    }
                }
            }
        }

        if registered.is_empty() {
            // Defensive: if our matcher ever stops finding any registrations
            // at all, prefer a clear failure to a silent green.
            panic!(
                "cross-check found ZERO tool registrations under {} — \
                 either the matcher is broken or the file layout changed. \
                 Update this test before continuing.",
                tools_dir.display()
            );
        }

        // Spine tools live in Rust, not app/tools/*.py: `find_tools` is an
        // always-on meta-tool (crates/agent/src/meta_tools.rs); `query_platform`
        // and `research` are Rust command-tools (crates/agent/src/command_tools.rs)
        // that replaced retired Python tools (knowledge.py / research.py). They
        // are real, just not Python-registered — exempt them from the Python
        // cross-check (the anti-dead-tool intent still covers the rest).
        const RUST_NATIVE: &[&str] = &[
            "find_tools",
            "query_platform",
            "knowledge_entity",
            "research",
        ];

        let missing: Vec<&str> = GUIDANCE_TOOL_NAMES
            .iter()
            .copied()
            .filter(|n| !RUST_NATIVE.contains(n))
            .filter(|n| !registered.contains(*n))
            .collect();

        assert!(
            missing.is_empty(),
            "tool name(s) in the guidance block are NOT registered in app/tools/ or RUST_NATIVE: {:?}\n\
             registered names found: {:?}\n\
             Either restore the registration in Python, add it to RUST_NATIVE if it is a \
             Rust command/meta tool, or remove the name from both \
             GUIDANCE_TOOL_NAMES and TOOL_GUIDANCE_BLOCK.",
            missing,
            registered.iter().take(20).collect::<Vec<_>>()
        );
    }

    #[test]
    fn strip_json_fences_variants() {
        // Fenced with language tag — the common Claude/marc27 shape.
        assert_eq!(
            LlmClient::strip_json_fences("```json\n{\"a\":1}\n```"),
            "{\"a\":1}"
        );
        // Fenced without a tag.
        assert_eq!(
            LlmClient::strip_json_fences("```\n{\"a\":1}\n```"),
            "{\"a\":1}"
        );
        // Bare JSON passes through untouched.
        assert_eq!(LlmClient::strip_json_fences("  {\"a\":1} "), "{\"a\":1}");
        // Unterminated fence: still yields the body rather than erroring.
        assert_eq!(
            LlmClient::strip_json_fences("```json\n{\"a\":1}"),
            "{\"a\":1}"
        );
    }

    // ── Schema-rejection classification ───────────────────────────────

    /// The fallback gate must be narrow on BOTH axes: a request-shape status
    /// alone is not enough (a 400 for a wrong model name must propagate),
    /// and naming the mechanism alone is not enough (a 402 whose body
    /// mentions response_format is still a billing failure). Only their
    /// conjunction may trigger the honest json_object fallback.
    #[test]
    fn json_schema_rejection_requires_status_and_mechanism_together() {
        let rejects = |status: u16, body: &str| {
            error_rejects_json_schema(
                &anyhow::Error::new(retry::HttpStatus::new(status))
                    .context(format!("LLM returned HTTP {status}: {body}")),
            )
        };
        // The real shapes: provider 400/422 naming the mechanism.
        assert!(rejects(
            400,
            "response_format 'json_schema' is not supported"
        ));
        assert!(rejects(422, "unknown field json_schema"));
        assert!(rejects(500, "Failed to convert json schema to grammar"));
        assert!(rejects(501, "structured output is not implemented"));
        // Request-shape status, unrelated body: NOT a capability signal.
        assert!(!rejects(400, "model 'nonexistent' not found"));
        // Mechanism named, wrong status class: auth/billing/capacity/routing.
        for status in [401, 402, 403, 404, 429, 503] {
            assert!(
                !rejects(status, "response_format json_schema"),
                "status {status} must never trigger the schema fallback"
            );
        }
        // No HttpStatus in the chain at all (transport error): propagate.
        assert!(!error_rejects_json_schema(&anyhow::anyhow!(
            "connection reset while sending response_format json_schema"
        )));
    }

    // ── Hard offline mode ─────────────────────────────────────────────
    //
    // Three separate `check_url` guards stand between a configured base URL
    // and a socket: `health_check`, `post`, and `send_retrying`. Each gets a
    // test here, and each asserts BOTH halves — offline refuses, not-offline
    // reaches the transport — because a one-sided test passes just as happily
    // against a function that refuses unconditionally.
    //
    // All of them take the shared lock from prism-runtime rather than
    // declaring one here: `PRISM_OFFLINE` is process-global, `cfg(test)` does
    // not cross crate boundaries, and two locks that do not exclude each other
    // serialize nothing.

    /// A target that is NOT loopback — so `PRISM_OFFLINE=1` must refuse it —
    /// and that refuses a connection immediately, so the not-offline halves
    /// cost milliseconds. A TEST-NET-3 address blackholes instead of refusing,
    /// which is how a test in this repo once took 75 seconds.
    const UNREACHABLE_BASE: &str = "http://0.0.0.0:1";

    /// A client on the HTTP adapter. `local_backend()` short-circuits
    /// `health_check` before the guard, so a `gguf://local` config would test
    /// nothing at all.
    fn unreachable_http_client() -> LlmClient {
        assert_eq!(
            choose_backend(UNREACHABLE_BASE),
            BackendChoice::Http,
            "the offline guards live on the HTTP path only"
        );
        LlmClient::new(LlmConfig {
            base_url: UNREACHABLE_BASE.to_string(),
            model: "offline-guard-probe".to_string(),
            timeout_secs: 5,
            ..LlmConfig::default()
        })
    }

    // Holding the lock across the awaits is the point — it is what stops a
    // concurrent test from flipping `PRISM_OFFLINE` mid-call. Same precedent
    // as `client/src/api.rs` and `node/src/daemon.rs`; applies to all three.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn health_check_is_refused_by_offline_mode_and_only_by_it() {
        use prism_runtime::offline::test_support::{OfflineEnvGuard, env_lock};

        let _lock = env_lock();
        let client = unreachable_http_client();

        // The guard restores `PRISM_OFFLINE` on drop, so an assertion panic —
        // exactly what this test exists to produce — cannot leak the variable
        // into every later test in the binary.
        let blocked = {
            let _offline = OfflineEnvGuard::set("1");
            format!("{:#}", client.health_check().await.unwrap_err())
        };
        assert!(
            blocked.contains("offline mode"),
            "PRISM_OFFLINE=1 must refuse {UNREACHABLE_BASE}/v1/models as policy, got: {blocked}"
        );

        let attempted = {
            let _online = OfflineEnvGuard::clear();
            format!("{:#}", client.health_check().await.unwrap_err())
        };
        assert!(
            !attempted.contains("offline mode"),
            "offline mode must not refuse with PRISM_OFFLINE unset, got: {attempted}"
        );
        assert!(
            attempted.contains("LLM not reachable"),
            "with offline mode off the check must reach the transport, got: {attempted}"
        );
    }

    /// `post` refuses a blocked URL before it builds a request.
    ///
    /// This guard is defence in depth, not the only thing standing there:
    /// `post` delegates to `send_retrying`, which repeats the identical check.
    /// Deleting either line on its own therefore leaves the other producing
    /// the same refusal, and no test can distinguish them — the two guards are
    /// only separable together. This test pins `post`'s observable contract;
    /// removing BOTH guards is what fails it.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn post_is_refused_by_offline_mode_and_only_by_it() {
        use prism_runtime::offline::test_support::{OfflineEnvGuard, env_lock};

        let _lock = env_lock();
        let client = unreachable_http_client();
        let url = chat_completions_url(UNREACHABLE_BASE);
        let body = serde_json::json!({});

        let blocked = {
            let _offline = OfflineEnvGuard::set("1");
            format!("{:#}", client.post(&url, &body).await.unwrap_err())
        };
        assert!(
            blocked.contains("offline mode"),
            "PRISM_OFFLINE=1 must refuse {url} as policy, got: {blocked}"
        );

        let attempted = {
            let _online = OfflineEnvGuard::clear();
            format!("{:#}", client.post(&url, &body).await.unwrap_err())
        };
        assert!(
            !attempted.contains("offline mode"),
            "offline mode must not refuse with PRISM_OFFLINE unset, got: {attempted}"
        );
        assert!(
            attempted.contains("LLM request to"),
            "with offline mode off the post must reach the transport, got: {attempted}"
        );
    }

    /// The guard sits BEFORE the retry closure on purpose, so a URL blocked by
    /// policy is never classified transient and replayed.
    ///
    /// The elapsed-time assertion is what pins that placement rather than
    /// merely the refusal: inside the closure, a refused connection to
    /// `0.0.0.0:1` is retryable even when billable, so the failure would come
    /// back only after the shared backoff's first 250 ms sleep.
    /// A 404 on a version-less base URL says what is almost certainly wrong.
    ///
    /// `chat_completions_url` never synthesises `/v1` — correct, and the reason
    /// is documented on it. The cost is that pointing at Ollama's bare base
    /// (`http://127.0.0.1:11434`, the commonest local setup) 404s with the
    /// upstream body `404 page not found`, which names nothing. Measured
    /// against a live daemon: `/v1/chat/completions` -> 400, `/chat/completions`
    /// -> 404.
    ///
    /// The hint must NOT fire when the base already carries a version, or it
    /// would send someone with a correct URL chasing the wrong thing — their
    /// 404 is a bad model or a dead route.
    #[test]
    fn a_versionless_404_hints_at_the_missing_v1_and_a_versioned_one_does_not() {
        use reqwest::StatusCode;

        let bare = base_url_hint(
            "http://127.0.0.1:11434/chat/completions",
            StatusCode::NOT_FOUND,
        );
        assert!(bare.contains("/v1"), "{bare}");
        assert!(bare.contains("127.0.0.1:11434"), "{bare}");

        // Already versioned: silent, for each shape the doc comment names.
        for versioned in [
            "http://127.0.0.1:11434/v1/chat/completions",
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            "https://api.z.ai/api/paas/v4/chat/completions",
        ] {
            assert!(
                base_url_hint(versioned, StatusCode::NOT_FOUND).is_empty(),
                "must not hint for {versioned}"
            );
        }

        // Only 404. A 401/429/500 on a version-less base is not this problem.
        for other in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(
                base_url_hint("http://127.0.0.1:11434/chat/completions", other).is_empty(),
                "must not hint for {other}"
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn send_retrying_refuses_before_the_retry_closure() {
        use prism_runtime::offline::test_support::{OfflineEnvGuard, env_lock};

        let _lock = env_lock();
        let client = unreachable_http_client();
        let url = chat_completions_url(UNREACHABLE_BASE);
        let body = serde_json::json!({});

        let (blocked, elapsed) = {
            let _offline = OfflineEnvGuard::set("1");
            let start = std::time::Instant::now();
            let error = client
                .send_retrying("test.offline", &url, &body, false)
                .await
                .unwrap_err();
            (format!("{error:#}"), start.elapsed())
        };
        assert!(
            blocked.contains("offline mode"),
            "PRISM_OFFLINE=1 must refuse {url} as policy, got: {blocked}"
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "a blocked URL must be refused before the retry closure; one retry \
             sleep alone is at least 250 ms, and this took {elapsed:?}"
        );

        let attempted = {
            let _online = OfflineEnvGuard::clear();
            format!(
                "{:#}",
                client
                    .send_retrying("test.online", &url, &body, false)
                    .await
                    .unwrap_err()
            )
        };
        assert!(
            !attempted.contains("offline mode"),
            "offline mode must not refuse with PRISM_OFFLINE unset, got: {attempted}"
        );
        assert!(
            attempted.contains("LLM request to"),
            "with offline mode off the send must reach the transport, got: {attempted}"
        );
    }
}

#[cfg(test)]
mod hydration_tests {
    use super::hydrate_env_from_map;

    /// Unique var names so parallel tests can't race on shared env state.
    #[test]
    fn file_fills_unset_env_but_never_overrides() {
        let mut map = serde_json::Map::new();
        map.insert(
            "PRISM_HYDRATE_TEST_UNSET_A".into(),
            serde_json::Value::String("from-file".into()),
        );
        map.insert(
            "PRISM_HYDRATE_TEST_PRESET_B".into(),
            serde_json::Value::String("from-file".into()),
        );
        map.insert(
            "PRISM_HYDRATE_TEST_EMPTY_C".into(),
            serde_json::Value::String(String::new()),
        );
        // SAFETY: test-only unique var names — no concurrent readers.
        unsafe { std::env::set_var("PRISM_HYDRATE_TEST_PRESET_B", "from-env") };

        hydrate_env_from_map(&map);

        assert_eq!(
            std::env::var("PRISM_HYDRATE_TEST_UNSET_A").as_deref(),
            Ok("from-file")
        );
        // Env wins over file — the file is a fallback, never an override.
        assert_eq!(
            std::env::var("PRISM_HYDRATE_TEST_PRESET_B").as_deref(),
            Ok("from-env")
        );
        // Empty strings are not exported.
        assert!(std::env::var_os("PRISM_HYDRATE_TEST_EMPTY_C").is_none());

        // SAFETY: test-only unique var names — no concurrent readers.
        unsafe {
            std::env::remove_var("PRISM_HYDRATE_TEST_UNSET_A");
            std::env::remove_var("PRISM_HYDRATE_TEST_PRESET_B");
        }
    }
}
