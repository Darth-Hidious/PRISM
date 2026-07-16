//! Data-driven model registry.
//!
//! Model metadata (pricing, context window, capability flags) is resolved
//! from three layered sources, highest precedence first:
//!
//! 1. **User-registered models** — `~/.prism/models.toml`, written by
//!    `prism models register` (also reachable in the TUI as
//!    `/models register ...`). Supports custom/self-hosted endpoints
//!    (`base_url` + `api_key_env`).
//! 2. **Platform catalog cache** — `~/.prism/model-catalog.json`, a local
//!    snapshot of `GET /projects/{id}/llm/models` (the MARC27
//!    discovery-service catalog). The CLI refreshes it with a TTL and the
//!    registry reads it at any age, so lookups keep working offline.
//! 3. **Static seed** — a small compiled-in list of well-known models,
//!    kept ONLY as an offline last resort (fresh install, no network yet).
//!
//! A model id that matches none of the three falls back to an honest
//! `UNKNOWN_MODEL_CONFIG` ($0 pricing, 128k context) and logs a warning
//! once per id so the miss is visible, not silent.
//!
//! The public API (`get_model_config`, `estimate_cost`,
//! `get_default_model`) is unchanged from the old compile-time registry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::types::UsageInfo;

// ---------------------------------------------------------------------------
// ModelConfig — immutable configuration for a specific LLM model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ModelConfig {
    pub id: &'static str,
    pub provider: &'static str,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub default_max_tokens: usize,
    pub input_price_per_mtok: f64,
    pub output_price_per_mtok: f64,
    pub supports_caching: bool,
    pub supports_thinking: bool,
    pub supports_tools: bool,
    /// Custom/self-hosted endpoint for user-registered models (z.ai, vLLM,
    /// Ollama, ...). `None` for platform-catalog and seed models, which are
    /// served through the configured chat target.
    pub base_url: Option<&'static str>,
    /// Name of the env var holding the API key for `base_url`. The key
    /// itself is never persisted.
    pub api_key_env: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Static seed — offline baseline ONLY
// ---------------------------------------------------------------------------
//
// The well-known models with their real, field-for-field values, so a fresh
// offline install — or a direct-provider user (`OPENAI_API_KEY` path) whose
// project-scoped catalog never lists these ids — still gets correct
// budget/cost accounting instead of the $0/128k UNKNOWN stub. The live
// catalog and `~/.prism/models.toml` AUGMENT this baseline (later source
// wins per id); they do not replace it. Add a new id here only when it is a
// stable default users hit offline — everything transient comes from the
// catalog.

#[allow(clippy::too_many_arguments)] // positional table constructor, used only for SEED below
const fn seed(
    id: &'static str,
    provider: &'static str,
    ctx: usize,
    max_out: usize,
    default_max: usize,
    pin: f64,
    pout: f64,
    cache: bool,
    think: bool,
) -> ModelConfig {
    ModelConfig {
        id,
        provider,
        context_window: ctx,
        max_output_tokens: max_out,
        default_max_tokens: default_max,
        input_price_per_mtok: pin,
        output_price_per_mtok: pout,
        supports_caching: cache,
        supports_thinking: think,
        supports_tools: true,
        base_url: None,
        api_key_env: None,
    }
}

const SEED: &[ModelConfig] = &[
    seed(
        "claude-fable-5",
        "anthropic",
        1_000_000,
        128_000,
        32_768,
        10.00,
        50.00,
        true,
        true,
    ),
    seed(
        "claude-opus-4-8",
        "anthropic",
        1_000_000,
        128_000,
        32_768,
        5.00,
        25.00,
        true,
        true,
    ),
    seed(
        "claude-opus-4-6",
        "anthropic",
        200_000,
        128_000,
        32_768,
        5.00,
        25.00,
        true,
        true,
    ),
    seed(
        "claude-sonnet-5",
        "anthropic",
        1_000_000,
        128_000,
        16_384,
        3.00,
        15.00,
        true,
        true,
    ),
    seed(
        "claude-sonnet-4-6",
        "anthropic",
        200_000,
        64_000,
        16_384,
        3.00,
        15.00,
        true,
        true,
    ),
    // `get_default_model("marc27")` — must resolve offline.
    seed(
        "claude-sonnet-4-20250514",
        "anthropic",
        200_000,
        64_000,
        16_384,
        3.00,
        15.00,
        true,
        true,
    ),
    seed(
        "claude-sonnet-4-20250318",
        "anthropic",
        200_000,
        64_000,
        16_384,
        3.00,
        15.00,
        true,
        true,
    ),
    seed(
        "claude-haiku-4-5",
        "anthropic",
        200_000,
        64_000,
        8_192,
        1.00,
        5.00,
        true,
        false,
    ),
    seed(
        "claude-haiku-4-5-20251001",
        "anthropic",
        200_000,
        64_000,
        8_192,
        1.00,
        5.00,
        true,
        false,
    ),
    // --- OpenAI ---
    // Legacy GPT-4 at its real list prices ($30/$60, 8k ctx; turbo $10/$30,
    // 128k). Seeded so bare `gpt-4` resolves EXACTLY instead of
    // family-aliasing to the ~15x-cheaper gpt-4.1.
    seed(
        "gpt-4", "openai", 8_192, 8_192, 4_096, 30.00, 60.00, false, false,
    ),
    seed(
        "gpt-4-turbo",
        "openai",
        128_000,
        4_096,
        4_096,
        10.00,
        30.00,
        false,
        false,
    ),
    seed(
        "gpt-4o", "openai", 128_000, 16_384, 8_192, 2.50, 10.00, false, false,
    ),
    seed(
        "gpt-4o-mini",
        "openai",
        128_000,
        16_384,
        4_096,
        0.15,
        0.60,
        false,
        false,
    ),
    seed(
        "gpt-4.1", "openai", 1_000_000, 32_768, 16_384, 2.00, 8.00, false, false,
    ),
    seed(
        "gpt-4.1-mini",
        "openai",
        1_000_000,
        32_768,
        8_192,
        0.40,
        1.60,
        false,
        false,
    ),
    seed(
        "gpt-5", "openai", 400_000, 128_000, 16_384, 1.25, 10.00, false, true,
    ),
    seed(
        "o3", "openai", 200_000, 100_000, 16_384, 2.00, 8.00, false, true,
    ),
    seed(
        "o3-mini", "openai", 200_000, 100_000, 8_192, 1.10, 4.40, false, true,
    ),
    // --- Google ---
    seed(
        "gemini-2.5-pro",
        "google",
        1_000_000,
        65_536,
        16_384,
        1.25,
        10.00,
        false,
        true,
    ),
    seed(
        "gemini-2.5-flash",
        "google",
        1_000_000,
        65_536,
        8_192,
        0.30,
        2.50,
        false,
        false,
    ),
    seed(
        "gemini-3.1-pro",
        "google",
        1_000_000,
        65_536,
        16_384,
        2.00,
        12.00,
        false,
        true,
    ),
    // --- Zhipu ---
    seed(
        "glm-5", "zhipu", 200_000, 128_000, 16_384, 1.00, 3.20, false, false,
    ),
    seed(
        "glm-4.7", "zhipu", 128_000, 16_384, 8_192, 0.38, 1.70, false, false,
    ),
    seed(
        "glm-4.5-air",
        "zhipu",
        128_000,
        16_384,
        4_096,
        0.10,
        0.50,
        false,
        false,
    ),
];

// ---------------------------------------------------------------------------
// User-registered models — ~/.prism/models.toml
// ---------------------------------------------------------------------------
//
// Schema (one table per model id):
//
//   [models."glm-5.2"]
//   provider          = "zhipu"
//   base_url          = "https://api.z.ai/api/anthropic"  # optional
//   api_key_env       = "ZAI_API_KEY"                     # optional (env var NAME, never the key)
//   input_price       = 1.00     # USD per 1M input tokens
//   output_price      = 3.20     # USD per 1M output tokens
//   context_window    = 200000
//   max_output_tokens = 128000   # optional, default 16384
//   supports_tools    = true     # optional, default true
//   supports_thinking = false    # optional, default false
//   supports_caching  = false    # optional, default false

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserModel {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// USD per 1M input tokens.
    pub input_price: f64,
    /// USD per 1M output tokens.
    pub output_price: f64,
    pub context_window: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_caching: Option<bool>,
}

impl UserModel {
    /// Reject values that would corrupt cost/limit accounting or leak a
    /// secret. Applied on register (hard error) AND on load (drop + warn),
    /// so a hand-edited `models.toml` can't inject a NaN price, a zero
    /// context window, or a pasted API key masquerading as an env-var name.
    fn validate(&self) -> Result<()> {
        if self.provider.trim().is_empty() {
            bail!("provider must not be empty");
        }
        if self.context_window == 0 {
            bail!("context_window must be > 0");
        }
        if let Some(0) = self.max_output_tokens {
            bail!("max_output_tokens must be > 0 when set");
        }
        // `< 0.0` is false for NaN, and clap/TOML accept `nan`/`inf` — a
        // non-finite price makes estimate_cost return NaN. Require finite ≥ 0.
        if !(self.input_price.is_finite() && self.input_price >= 0.0) {
            bail!("input_price must be a finite value >= 0 (USD per 1M tokens)");
        }
        if !(self.output_price.is_finite() && self.output_price >= 0.0) {
            bail!("output_price must be a finite value >= 0 (USD per 1M tokens)");
        }
        if let Some(env) = &self.api_key_env
            && !is_valid_env_var_name(env)
        {
            bail!(
                "api_key_env must be an environment variable NAME (e.g. ZAI_API_KEY), \
                 not the key value — got {env:?}"
            );
        }
        if let Some(url) = &self.base_url
            && !is_http_url(url)
        {
            bail!("base_url must be an http(s) URL — got {url:?}");
        }
        Ok(())
    }
}

/// A POSIX-ish env var name: `^[A-Za-z_][A-Za-z0-9_]*$`. Rejects a pasted
/// key (which contains `-`, `.`, `/`, or is empty) from being persisted.
fn is_valid_env_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Lightweight http(s) URL check — no new dependency. Enough to reject a
/// non-URL string; the reqwest client validates fully at request time.
fn is_http_url(s: &str) -> bool {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"));
    matches!(rest, Some(host) if !host.is_empty() && !host.starts_with('/'))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UserModelsFile {
    #[serde(default)]
    models: BTreeMap<String, UserModel>,
}

/// `~/.prism/models.toml`, overridable via `PRISM_USER_MODELS_PATH`
/// (used by tests and unusual setups).
pub fn user_models_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PRISM_USER_MODELS_PATH") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".prism/models.toml"))
}

/// Parse a `models.toml` document. Pure, so it is unit-testable.
fn parse_user_models(raw: &str) -> Result<Vec<(String, UserModel)>> {
    let file: UserModelsFile = toml::from_str(raw).context("parsing models.toml")?;
    Ok(file.models.into_iter().collect())
}

/// Load user models. Missing file → empty. Malformed file → empty plus one
/// warning (never fatal), same policy as `prompt_profiles.toml`.
fn load_user_models() -> Vec<(String, UserModel)> {
    let Some(path) = user_models_path() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match parse_user_models(&raw) {
        Ok(models) => models
            .into_iter()
            .filter(|(id, m)| match m.validate() {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("ignoring invalid model {id:?} in {}: {e}", path.display());
                    false
                }
            })
            .collect(),
        Err(e) => {
            tracing::warn!("ignoring malformed {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Register (or update) a user model in `models.toml`, then reload the
/// in-process registry so the model resolves immediately — no restart.
pub fn register_user_model(id: &str, model: UserModel) -> Result<PathBuf> {
    let path = user_models_path().context("cannot resolve home directory for models.toml")?;
    let result = register_user_model_at(&path, id, model);
    if result.is_ok() {
        reload();
    }
    result
}

fn register_user_model_at(path: &Path, id: &str, model: UserModel) -> Result<PathBuf> {
    let id = id.trim();
    if id.is_empty() {
        bail!("model id must not be empty");
    }
    model.validate()?;

    // A malformed existing file is a hard error here — registering must
    // never silently clobber hand-edited user config.
    let mut file: UserModelsFile = match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw).with_context(|| {
            format!(
                "{} is malformed — fix it before registering",
                path.display()
            )
        })?,
        Err(_) => UserModelsFile::default(),
    };
    file.models.insert(id.to_string(), model);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(&file).context("serialising models.toml")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, raw).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    Ok(path.to_path_buf())
}

/// User models rendered in the platform-catalog JSON shape (`model_id`,
/// `provider`, `input_price`, ...), tagged `"source": "user"`. Used by the
/// CLI to merge them into `prism models list` and the startup catalog.
#[must_use]
pub fn user_models_as_catalog_json() -> Vec<serde_json::Value> {
    load_user_models()
        .into_iter()
        .map(|(id, m)| {
            serde_json::json!({
                "model_id": id,
                "display_name": id,
                "provider": m.provider,
                "context_window": m.context_window,
                "max_output_tokens": m.max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
                "input_price": m.input_price,
                "output_price": m.output_price,
                "base_url": m.base_url,
                "api_key_env": m.api_key_env,
                "status": "user",
                "source": "user",
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Platform catalog cache — ~/.prism/model-catalog.json
// ---------------------------------------------------------------------------

/// How long a cached catalog counts as fresh. Within the TTL the CLI's
/// startup path skips the network; `prism models list` always tries live
/// first and only falls back to the cache.
pub const CATALOG_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Serialize, Deserialize)]
pub struct CatalogCache {
    /// Unix seconds at fetch time.
    pub fetched_at: u64,
    /// Raw platform catalog entries (`GET /projects/{id}/llm/models` shape).
    pub models: Vec<serde_json::Value>,
}

impl CatalogCache {
    #[must_use]
    pub fn age(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Duration::from_secs(now.saturating_sub(self.fetched_at))
    }

    #[must_use]
    pub fn is_fresh(&self) -> bool {
        self.age() < CATALOG_TTL
    }
}

/// `~/.prism/model-catalog.json`, overridable via `PRISM_MODEL_CATALOG_PATH`.
pub fn catalog_cache_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PRISM_MODEL_CATALOG_PATH") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".prism/model-catalog.json"))
}

/// Load the cached catalog at any age (offline lookups use stale caches on
/// purpose). Missing or malformed → `None`.
#[must_use]
pub fn load_catalog_cache() -> Option<CatalogCache> {
    let path = catalog_cache_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist a freshly fetched catalog and reload the in-process registry.
pub fn save_catalog_cache(models: &[serde_json::Value]) -> Result<PathBuf> {
    let path =
        catalog_cache_path().context("cannot resolve home directory for model-catalog.json")?;
    let cache = CatalogCache {
        fetched_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        models: models.to_vec(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let raw = serde_json::to_string(&cache).context("serialising model catalog cache")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, raw).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    reload();
    Ok(path)
}

// ---------------------------------------------------------------------------
// Registry assembly — seed, then catalog, then user (later wins)
// ---------------------------------------------------------------------------

const DEFAULT_MAX_OUTPUT_TOKENS: usize = 16_384;

/// Upper bound on token limits accepted from the catalog/cache. The largest
/// real context window today is ~10M tokens; 100M leaves headroom while
/// keeping a poisoned cache's `u64::MAX` out of live budget math.
const MAX_SANE_TOKEN_LIMIT: u64 = 100_000_000;

/// Intern a dynamic string. The registry is built at most once per
/// (re)load, so the leak is bounded: a few hundred bytes per model id,
/// re-leaked only on explicit `reload()` (i.e. after registering a model
/// or refreshing the catalog).
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// A price field from JSON: only a finite, non-negative value survives —
/// a poisoned/hand-edited cache with NaN, inf, or a negative price falls
/// back to $0 rather than poisoning `estimate_cost`.
fn sane_price(v: &serde_json::Value, key: &str) -> f64 {
    match v.get(key).and_then(serde_json::Value::as_f64) {
        Some(p) if p.is_finite() && p >= 0.0 => p,
        _ => 0.0,
    }
}

/// Map one raw platform-catalog entry to a `ModelConfig`. Entries without
/// a model id are skipped. Missing/absurd limits get the same conservative
/// defaults as `UNKNOWN_MODEL_CONFIG` (128k / 16k); missing/invalid prices
/// are $0, which is what the platform means by a free model.
///
/// `base_url` / `api_key_env` are deliberately NOT read from the catalog or
/// its on-disk cache: only the user's own `~/.prism/models.toml` may set a
/// custom endpoint or a secret env-var name. Importing them here would let a
/// poisoned catalog/cache pair an arbitrary env-secret name with an attacker
/// URL — a latent exfil primitive the moment endpoint routing wires up.
fn catalog_entry_to_config(v: &serde_json::Value) -> Option<ModelConfig> {
    let id = v
        .get("model_id")
        .or_else(|| v.get("id"))
        .and_then(|x| x.as_str())?;
    // A u64 within sane bounds; 0 or absurd (> [`MAX_SANE_TOKEN_LIMIT`],
    // e.g. a poisoned cache's u64::MAX) → treat as absent (use the default).
    let get_usize = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_u64()))
            .filter(|x| (1..=MAX_SANE_TOKEN_LIMIT).contains(x))
            .map(|x| x as usize)
    };
    let get_bool = |k: &str| v.get(k).and_then(serde_json::Value::as_bool);
    let context_window = get_usize(&["context_window", "max_input_tokens"]).unwrap_or(128_000);
    let max_output_tokens = get_usize(&["max_output_tokens"]).unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    Some(ModelConfig {
        id: leak(id),
        provider: leak(
            v.get("provider")
                .and_then(|x| x.as_str())
                .unwrap_or("unknown"),
        ),
        context_window,
        max_output_tokens,
        default_max_tokens: max_output_tokens.min(DEFAULT_MAX_OUTPUT_TOKENS),
        input_price_per_mtok: sane_price(v, "input_price"),
        output_price_per_mtok: sane_price(v, "output_price"),
        supports_caching: get_bool("supports_prompt_caching").unwrap_or(false),
        supports_thinking: get_bool("supports_reasoning").unwrap_or(false),
        supports_tools: get_bool("supports_function_calling").unwrap_or(true),
        // Never from the catalog — user config only (see fn doc).
        base_url: None,
        api_key_env: None,
    })
}

fn user_model_to_config(id: &str, m: &UserModel) -> ModelConfig {
    let max_output_tokens = m.max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    ModelConfig {
        id: leak(id),
        provider: leak(&m.provider),
        context_window: m.context_window,
        max_output_tokens,
        default_max_tokens: max_output_tokens.min(DEFAULT_MAX_OUTPUT_TOKENS),
        input_price_per_mtok: m.input_price,
        output_price_per_mtok: m.output_price,
        supports_caching: m.supports_caching.unwrap_or(false),
        supports_thinking: m.supports_thinking.unwrap_or(false),
        supports_tools: m.supports_tools.unwrap_or(true),
        base_url: m.base_url.as_deref().map(leak),
        api_key_env: m.api_key_env.as_deref().map(leak),
    }
}

/// Build a registry from explicit sources. Pure apart from string interning,
/// so tests can drive it with mock catalogs and configs directly.
fn build_registry_from(
    user: &[(String, UserModel)],
    catalog: &[serde_json::Value],
) -> HashMap<String, ModelConfig> {
    let mut m = HashMap::new();
    for cfg in SEED {
        m.insert(cfg.id.to_string(), *cfg);
    }
    for v in catalog {
        if let Some(cfg) = catalog_entry_to_config(v) {
            m.insert(cfg.id.to_string(), cfg);
        }
    }
    for (id, um) in user {
        m.insert(id.clone(), user_model_to_config(id, um));
    }
    m
}

type Registry = Arc<HashMap<String, ModelConfig>>;

static REGISTRY: RwLock<Option<Registry>> = RwLock::new(None);

fn registry() -> Registry {
    if let Some(reg) = REGISTRY.read().expect("registry lock").as_ref() {
        return reg.clone();
    }
    let catalog = load_catalog_cache().map(|c| c.models).unwrap_or_default();
    let built: Registry = Arc::new(build_registry_from(&load_user_models(), &catalog));
    let mut w = REGISTRY.write().expect("registry lock");
    if let Some(reg) = w.as_ref() {
        return reg.clone(); // another thread won the race
    }
    *w = Some(built.clone());
    built
}

/// Drop the in-process registry so the next lookup rebuilds it from disk.
/// Called after `models.toml` or the catalog cache changes. Also clears the
/// UNKNOWN-warned set so a newly-orphaned model id warns again.
pub fn reload() {
    *REGISTRY.write().expect("registry lock") = None;
    if let Some(seen) = WARNED_UNKNOWN.get() {
        seen.lock().expect("warned-models lock").clear();
    }
}

// ---------------------------------------------------------------------------
// estimate_cost — cost estimation from usage + model config
// ---------------------------------------------------------------------------

/// Estimate USD cost for a given usage and model configuration.
///
/// Cache read tokens get a 90% discount on the input price.
#[must_use]
pub fn estimate_cost(usage: &UsageInfo, config: &ModelConfig) -> f64 {
    let input_cost = usage.input_tokens as f64 * config.input_price_per_mtok / 1_000_000.0;
    let output_cost = usage.output_tokens as f64 * config.output_price_per_mtok / 1_000_000.0;
    let cache_read_cost =
        usage.cache_read_tokens as f64 * config.input_price_per_mtok * 0.1 / 1_000_000.0;
    input_cost + output_cost + cache_read_cost
}

// ---------------------------------------------------------------------------
// get_model_config — lookup with fallback chain
// ---------------------------------------------------------------------------

/// Minimum query length before the prefix step (step 4) engages. Shorter
/// queries (`""`, `"g"`, `"o3"`) are too ambiguous to family-alias safely —
/// they fall through to UNKNOWN rather than silently matching whatever
/// ranks top. The exact-tail suffix step (step 3) is exempt: it matches the
/// bare name EXACTLY, so a real short id present only router-style in the
/// catalog (`openai/o1`) still resolves.
const MIN_FUZZY_LEN: usize = 3;

/// Look up model configuration by ID.
///
/// Lookup order:
/// 1. Exact match
/// 2. Strip OpenRouter prefix (`provider/model-name`)
/// 3. Bare-name suffix — `glm-5.2` matches a router-style catalog id
///    `z-ai/glm-5.2` (same rule as the CLI's `resolve_catalog_model`)
/// 4. Prefix match — but only up to a token boundary (`-`/`.`/`/`), so
///    `o3` never grabs `o3-mini`; newest + most-canonical provider wins.
///
/// Step 4 requires a query of at least [`MIN_FUZZY_LEN`] chars (step 3 is
/// an exact-tail match, so real short ids like `o1` still resolve). Both
/// `tracing::debug` the resolved id so fuzzy hits are observable, not
/// silent. No match → honest UNKNOWN, logged once per id.
///
/// Sources per id: user `models.toml` > platform catalog cache > static seed.
#[must_use]
pub fn get_model_config(model_id: &str) -> ModelConfig {
    let reg = registry();
    if let Some(cfg) = lookup(&reg, model_id) {
        return cfg;
    }
    warn_unknown_once(model_id);
    UNKNOWN_MODEL_CONFIG
}

/// Preference when a fuzzy query is ambiguous across providers — lower =
/// more canonical. A direct vendor beats a router/cloud reseller, so bare
/// `o3` resolves to `openai/o3`, not `azure/o3`.
fn provider_rank(provider: &str) -> u8 {
    match provider {
        "anthropic" | "openai" | "google" | "zhipu" | "mistral" => 0,
        "vertexai" => 1,
        "openrouter" => 2,
        _ => 3,
    }
}

/// Pick the best of several fuzzy candidates: newest ([`recency_key`]),
/// then most-canonical provider ([`provider_rank`]), then lexicographically
/// first id. Fully deterministic — independent of `HashMap` order.
fn best_fuzzy<'a>(
    it: impl Iterator<Item = (&'a String, &'a ModelConfig)>,
) -> Option<(&'a String, ModelConfig)> {
    it.max_by(|(a_id, a), (b_id, b)| {
        recency_key(a_id)
            .cmp(&recency_key(b_id))
            .then_with(|| provider_rank(b.provider).cmp(&provider_rank(a.provider)))
            .then_with(|| b_id.cmp(a_id))
    })
    .map(|(id, cfg)| (id, *cfg))
}

/// `key` starts with `prefix` AND the char right after is a token boundary
/// (or the string ends) — so `claude-opus` matches `claude-opus-4-8` but
/// `o3` does not match `o3-mini` and `glm-4` does not match `glm-4o`.
fn prefix_to_boundary(key: &str, prefix: &str) -> bool {
    key.starts_with(prefix)
        && matches!(
            key.as_bytes().get(prefix.len()),
            None | Some(b'-' | b'.' | b'/')
        )
}

fn lookup(reg: &HashMap<String, ModelConfig>, model_id: &str) -> Option<ModelConfig> {
    // 1. Exact match
    if let Some(cfg) = reg.get(model_id) {
        return Some(*cfg);
    }
    // 2. Strip OpenRouter-style prefix
    if let Some((_, stripped)) = model_id.split_once('/')
        && let Some(cfg) = reg.get(stripped)
    {
        return Some(*cfg);
    }
    // 3. Bare-name suffix: `glm-5.2` → catalog id `z-ai/glm-5.2`. This is
    //    an EXACT-tail match, so it takes no length gate — its only
    //    ambiguity (cross-provider) is resolved by `best_fuzzy`'s
    //    provider_rank, and a real short id present only router-style
    //    (`openai/o1`) must still resolve.
    if !model_id.is_empty()
        && !model_id.contains('/')
        && let Some((id, cfg)) = best_fuzzy(
            reg.iter()
                .filter(|(key, _)| key.split_once('/').map(|(_, tail)| tail) == Some(model_id)),
        )
    {
        tracing::debug!(
            query = model_id,
            resolved = id.as_str(),
            "model resolved by suffix match"
        );
        return Some(cfg);
    }
    // Prefix matching only for queries long enough to be unambiguous.
    if model_id.len() < MIN_FUZZY_LEN {
        return None;
    }
    // 4. Prefix match — bounded at a token boundary, newest/canonical wins.
    if let Some((id, cfg)) = best_fuzzy(
        reg.iter()
            .filter(|(key, _)| prefix_to_boundary(key, model_id)),
    ) {
        tracing::debug!(
            query = model_id,
            resolved = id.as_str(),
            "model resolved by prefix match"
        );
        return Some(cfg);
    }
    None
}

/// Model ids already warned about, so the UNKNOWN warning fires once per id
/// per process instead of every turn. Cleared by [`reload`] so a model that
/// gets registered (and later re-orphaned) can warn again.
static WARNED_UNKNOWN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Warn once per unknown model id. Returns whether this call produced the
/// warning (first sighting).
fn warn_unknown_once(model_id: &str) -> bool {
    let mut seen = WARNED_UNKNOWN
        .get_or_init(Default::default)
        .lock()
        .expect("warned-models lock");
    let first = seen.insert(model_id.to_string());
    if first {
        tracing::warn!(
            model_id,
            "model not in registry (~/.prism/models.toml, platform catalog cache, or seed) — \
             using UNKNOWN fallback ($0 pricing, 128k context; cost estimates will be wrong). \
             Register it: /models register {model_id} --provider <p> --input-price <usd/mtok> \
             --output-price <usd/mtok> --context-window <tokens>"
        );
    }
    first
}

const UNKNOWN_MODEL_CONFIG: ModelConfig = ModelConfig {
    id: "unknown",
    provider: "unknown",
    context_window: 128_000,
    max_output_tokens: 16_384,
    default_max_tokens: 8_192,
    input_price_per_mtok: 0.0,
    output_price_per_mtok: 0.0,
    supports_caching: false,
    supports_thinking: false,
    supports_tools: true,
    base_url: None,
    api_key_env: None,
};

// ---------------------------------------------------------------------------
// Listing + ranking
// ---------------------------------------------------------------------------

/// Version-ish ranking key: the version-like numeric components of a model
/// id, compared lexicographically. `glm-5.2` → [5, 2] sorts above
/// `glm-4.7` → [4, 7]; `claude-sonnet-5` → [5] above `claude-sonnet-4-6`
/// → [4, 6]. A heuristic to put newer models first in listings.
///
/// Two kinds of digit run are *ignored* so they can't masquerade as a
/// higher version:
///  - date stamps — compact (≥ 6 digits, e.g. `-20250514`) or dashed
///    `YYYY-MM-DD` (e.g. `-2025-01-31`) — a dated snapshot must not
///    outrank the clean `-4-6` or the family's `-pro`;
///  - param-count / size tokens (a run followed by `b`/`m`/`k`, e.g. `34b`,
///    `72b`) — `yi-34b` must not outrank `claude-…-5` on "34 > 5".
#[must_use]
pub fn recency_key(id: &str) -> Vec<u64> {
    let bytes = id.as_bytes();
    // Pass 1: collect digit runs as byte ranges so pass 2 can inspect the
    // separators between them.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        runs.push((start, i));
    }
    // Pass 2: mark non-version runs. Compact dates and size tokens first…
    let mut skip = vec![false; runs.len()];
    for (idx, &(start, end)) in runs.iter().enumerate() {
        let followed_by_size = matches!(
            bytes.get(end),
            Some(b'b' | b'B' | b'm' | b'M' | b'k' | b'K')
        );
        if end - start >= 6 || followed_by_size {
            skip[idx] = true;
        }
    }
    // …then dashed date stamps: a plausible-`YYYY-MM-DD` triple of
    // consecutive '-'-joined runs is one date, not three version parts.
    let joined_by_dash =
        |prev_end: usize, next_start: usize| next_start == prev_end + 1 && bytes[prev_end] == b'-';
    let in_range = |range: std::ops::RangeInclusive<u64>, start: usize, end: usize| {
        id[start..end]
            .parse::<u64>()
            .is_ok_and(|n| range.contains(&n))
    };
    for (idx, w) in runs.windows(3).enumerate() {
        let ((ys, ye), (ms, me), (ds, de)) = (w[0], w[1], w[2]);
        if ye - ys == 4
            && me - ms <= 2
            && de - ds <= 2
            && joined_by_dash(ye, ms)
            && joined_by_dash(me, ds)
            && in_range(1900..=2099, ys, ye)
            && in_range(1..=12, ms, me)
            && in_range(1..=31, ds, de)
        {
            skip[idx] = true;
            skip[idx + 1] = true;
            skip[idx + 2] = true;
        }
    }
    runs.iter()
        .zip(skip.iter())
        .filter(|&(_, &skipped)| !skipped)
        .map(|(&(start, end), _)| id[start..end].parse().unwrap_or(0))
        .collect()
}

/// All registered models (user config + catalog cache + seed), newest
/// first by [`recency_key`], id as tiebreak.
#[must_use]
pub fn list_models() -> Vec<ModelConfig> {
    let reg = registry();
    let mut models: Vec<ModelConfig> = reg.values().copied().collect();
    models.sort_by(|a, b| {
        recency_key(b.id)
            .cmp(&recency_key(a.id))
            .then_with(|| a.id.cmp(b.id))
    });
    models
}

// ---------------------------------------------------------------------------
// get_default_model — default model ID per provider
// ---------------------------------------------------------------------------

/// Return the default model ID for a given provider name.
#[must_use]
pub fn get_default_model(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-sonnet-4-6",
        "openai" => "gpt-5",
        "google" | "vertexai" => "gemini-2.5-pro",
        "zhipu" => "glm-5",
        "marc27" => "claude-sonnet-4-20250514",
        "openrouter" => "anthropic/claude-sonnet-4-6",
        _ => "claude-sonnet-4-6",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that touch the process-global registry (or the env vars that
    /// point it at disk) serialize through this lock and get a fresh
    /// tempdir, so they never read the developer's real ~/.prism files.
    fn isolate() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        unsafe {
            std::env::set_var("PRISM_USER_MODELS_PATH", dir.path().join("models.toml"));
            std::env::set_var(
                "PRISM_MODEL_CATALOG_PATH",
                dir.path().join("model-catalog.json"),
            );
        }
        reload();
        (guard, dir)
    }

    fn mock_catalog() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "model_id": "glm-5.2",
                "display_name": "GLM 5.2",
                "provider": "zhipu",
                "context_window": 200_000,
                "max_output_tokens": 128_000,
                "input_price": 1.10,
                "output_price": 3.30,
                "supports_reasoning": true,
                "supports_prompt_caching": false,
                "status": "active",
            }),
            // Catalog updates an id that also exists in the seed.
            serde_json::json!({
                "model_id": "glm-5",
                "provider": "zhipu",
                "context_window": 256_000,
                "input_price": 0.90,
                "output_price": 3.00,
            }),
        ]
    }

    const GLM52_TOML: &str = r#"
[models."glm-5.2"]
provider = "zhipu"
base_url = "https://api.z.ai/api/anthropic"
api_key_env = "ZAI_API_KEY"
input_price = 1.0
output_price = 3.2
context_window = 200000
max_output_tokens = 128000
supports_thinking = true
"#;

    // ── Pure source/merge tests (no globals) ─────────────────────────

    #[test]
    fn seed_only_registry_resolves_defaults_offline() {
        let reg = build_registry_from(&[], &[]);
        let cfg = lookup(&reg, "claude-fable-5").expect("fable in seed");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.context_window, 1_000_000);
        assert!((cfg.input_price_per_mtok - 10.0).abs() < f64::EPSILON);
        // Every compiled-in provider default must resolve from the seed alone.
        for provider in [
            "anthropic",
            "openai",
            "google",
            "zhipu",
            "marc27",
            "openrouter",
        ] {
            let id = get_default_model(provider);
            assert!(
                lookup(&reg, id).is_some(),
                "default for {provider} ({id}) missing from seed"
            );
        }
    }

    #[test]
    fn models_toml_parses_glm52_with_real_pricing() {
        let models = parse_user_models(GLM52_TOML).unwrap();
        assert_eq!(models.len(), 1);
        let (id, m) = &models[0];
        assert_eq!(id, "glm-5.2");
        assert_eq!(m.provider, "zhipu");
        assert_eq!(
            m.base_url.as_deref(),
            Some("https://api.z.ai/api/anthropic")
        );
        assert_eq!(m.api_key_env.as_deref(), Some("ZAI_API_KEY"));
        assert_eq!(m.context_window, 200_000);
        assert!((m.input_price - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn glm52_resolves_from_catalog_not_unknown_stub() {
        // The whole point of the dynamic registry: a model that ships after
        // the binary (glm-5.2) resolves from the catalog with REAL pricing
        // and context — not the $0/128k UNKNOWN stub.
        let reg = build_registry_from(&[], &mock_catalog());
        let cfg = lookup(&reg, "glm-5.2").expect("glm-5.2 from catalog");
        assert_eq!(cfg.provider, "zhipu");
        assert_eq!(cfg.context_window, 200_000);
        assert_eq!(cfg.max_output_tokens, 128_000);
        assert!((cfg.input_price_per_mtok - 1.10).abs() < f64::EPSILON);
        assert!((cfg.output_price_per_mtok - 3.30).abs() < f64::EPSILON);
        assert!(cfg.supports_thinking);
        // OpenRouter-style spelling resolves to the same entry.
        assert_eq!(
            lookup(&reg, "zhipu/glm-5.2").unwrap().context_window,
            200_000
        );
    }

    #[test]
    fn merge_precedence_user_over_catalog_over_seed() {
        // Catalog overrides the seed entry for glm-5 …
        let reg = build_registry_from(&[], &mock_catalog());
        let cfg = lookup(&reg, "glm-5").unwrap();
        assert_eq!(cfg.context_window, 256_000, "catalog beats seed");
        assert!((cfg.input_price_per_mtok - 0.90).abs() < f64::EPSILON);

        // … and user config overrides the catalog for glm-5.2.
        let user = parse_user_models(GLM52_TOML).unwrap();
        let reg = build_registry_from(&user, &mock_catalog());
        let cfg = lookup(&reg, "glm-5.2").unwrap();
        assert!(
            (cfg.input_price_per_mtok - 1.0).abs() < f64::EPSILON,
            "user beats catalog"
        );
        assert_eq!(cfg.base_url, Some("https://api.z.ai/api/anthropic"));
        assert_eq!(cfg.api_key_env, Some("ZAI_API_KEY"));
    }

    #[test]
    fn catalog_entry_defaults_are_conservative() {
        let v = serde_json::json!({ "model_id": "mystery-1", "provider": "acme" });
        let cfg = catalog_entry_to_config(&v).unwrap();
        assert_eq!(cfg.context_window, 128_000);
        assert_eq!(cfg.max_output_tokens, 16_384);
        assert!((cfg.input_price_per_mtok).abs() < f64::EPSILON);
        assert!(cfg.supports_tools);
        assert!(!cfg.supports_thinking);
        // Entries without an id are skipped, not invented.
        assert!(catalog_entry_to_config(&serde_json::json!({"provider": "x"})).is_none());
    }

    #[test]
    fn catalog_never_imports_base_url_or_api_key_env() {
        // S1: a poisoned catalog/cache entry pairing an env-secret name with
        // an attacker URL must NOT populate ModelConfig.base_url/api_key_env —
        // only the user's own models.toml may set an endpoint/secret ref.
        let v = serde_json::json!({
            "model_id": "evil-1",
            "provider": "acme",
            "base_url": "https://attacker.example/exfil",
            "api_key_env": "OPENAI_API_KEY",
        });
        let cfg = catalog_entry_to_config(&v).unwrap();
        assert_eq!(cfg.base_url, None);
        assert_eq!(cfg.api_key_env, None);
    }

    #[test]
    fn catalog_prices_and_limits_are_sanitized() {
        // C4: NaN/inf/negative prices → $0, not a poisoned estimate_cost; a
        // zero/absent context window → the conservative 128k default.
        let v = serde_json::json!({
            "model_id": "junk-1",
            "provider": "acme",
            "input_price": f64::NAN,
            "output_price": -5.0,
            "context_window": 0,
            "max_output_tokens": 0,
        });
        let cfg = catalog_entry_to_config(&v).unwrap();
        assert_eq!(cfg.input_price_per_mtok, 0.0);
        assert_eq!(cfg.output_price_per_mtok, 0.0);
        assert_eq!(cfg.context_window, 128_000);
        assert_eq!(cfg.max_output_tokens, 16_384);
        // inf serializes as null in serde_json, so also test it explicitly.
        assert_eq!(
            sane_price(&serde_json::json!({ "p": f64::INFINITY }), "p"),
            0.0
        );
        // R2 hardening: absurdly large limits (a poisoned cache's u64::MAX)
        // fall back to the defaults too — "0 or absurd" means it.
        let v = serde_json::json!({
            "model_id": "junk-2",
            "provider": "acme",
            "context_window": u64::MAX,
            "max_output_tokens": u64::MAX,
        });
        let cfg = catalog_entry_to_config(&v).unwrap();
        assert_eq!(cfg.context_window, 128_000);
        assert_eq!(cfg.max_output_tokens, 16_384);
        assert_eq!(cfg.default_max_tokens, 16_384);
        // The bound is inclusive: a real 100M-token window would survive.
        let v = serde_json::json!({
            "model_id": "big-1", "provider": "acme",
            "context_window": MAX_SANE_TOKEN_LIMIT,
        });
        assert_eq!(
            catalog_entry_to_config(&v).unwrap().context_window,
            100_000_000
        );
    }

    #[test]
    fn ranking_is_newest_first() {
        assert!(recency_key("glm-5.2") > recency_key("glm-5"));
        assert!(recency_key("glm-5") > recency_key("glm-4.7"));
        assert!(recency_key("claude-sonnet-5") > recency_key("claude-sonnet-4-20250514"));
        assert!(recency_key("gpt-4.1") > recency_key("gpt-4o"));

        let user = parse_user_models(GLM52_TOML).unwrap();
        let reg = build_registry_from(&user, &mock_catalog());
        let mut models: Vec<ModelConfig> = reg.values().copied().collect();
        models.sort_by(|a, b| {
            recency_key(b.id)
                .cmp(&recency_key(a.id))
                .then_with(|| a.id.cmp(b.id))
        });
        let glm_ids: Vec<&str> = models
            .iter()
            .map(|m| m.id)
            .filter(|id| id.starts_with("glm"))
            .collect();
        // glm-5.2 (user/catalog) + the full seed's glm ids, newest first.
        assert_eq!(glm_ids, vec!["glm-5.2", "glm-5", "glm-4.7", "glm-4.5-air"]);
    }

    #[test]
    fn bare_id_matches_router_prefixed_catalog_entry() {
        // The live MARC27 catalog lists router models as `z-ai/glm-5.2`;
        // a user typing the bare id must still get real pricing/context.
        let catalog = vec![serde_json::json!({
            "model_id": "z-ai/glm-5.2",
            "provider": "openrouter",
            "context_window": 1_048_576,
            "input_price": 0.9212,
            "output_price": 2.8952,
        })];
        let reg = build_registry_from(&[], &catalog);
        let cfg = lookup(&reg, "glm-5.2").expect("bare id suffix-matches router entry");
        assert_eq!(cfg.id, "z-ai/glm-5.2");
        assert_eq!(cfg.context_window, 1_048_576);
        assert!((cfg.input_price_per_mtok - 0.9212).abs() < f64::EPSILON);
    }

    #[test]
    fn prefix_match_prefers_newest() {
        let reg = build_registry_from(&[], &mock_catalog());
        // "glm-5" is an exact hit; "glm" prefix-matches both → newest wins.
        assert_eq!(lookup(&reg, "glm").unwrap().id, "glm-5.2");
        // Seed-only prefix: claude-opus → opus-4-8 (newest), deterministic.
        let reg = build_registry_from(&[], &[]);
        assert_eq!(lookup(&reg, "claude-opus").unwrap().id, "claude-opus-4-8");
    }

    #[test]
    fn prefix_match_respects_token_boundary_and_min_length() {
        // C1: the probe-confirmed mis-resolutions. The catalog adds the
        // wrong-priced neighbours (`o3-mini`, `glm-4.7-air`) alongside the
        // seed's exact `o3` / `glm-4.7`.
        let catalog = vec![
            serde_json::json!({ "model_id": "o3-mini", "provider": "openai",
                "context_window": 200_000, "input_price": 1.10 }),
            serde_json::json!({ "model_id": "glm-4.7-air", "provider": "zhipu",
                "context_window": 128_000, "input_price": 0.10 }),
        ];
        let reg = build_registry_from(&[], &catalog);
        // `o3` / `glm-4.7` resolve to their EXACT seed entry — never the
        // cheaper neighbour that shares their prefix.
        let o3 = lookup(&reg, "o3").expect("exact seed o3");
        assert_eq!(o3.id, "o3");
        assert!(
            (o3.input_price_per_mtok - 2.00).abs() < f64::EPSILON,
            "not o3-mini's $1.10"
        );
        let glm47 = lookup(&reg, "glm-4.7").expect("exact seed glm-4.7");
        assert_eq!(glm47.id, "glm-4.7");
        assert!(
            (glm47.input_price_per_mtok - 0.38).abs() < f64::EPSILON,
            "not glm-4.7-air's $0.10"
        );
        // Mid-token prefixes (no boundary after the query) never match:
        // `gpt-4` must not grab `gpt-4o`, `o3-min` must not grab `o3-mini`.
        assert_ne!(lookup(&reg, "gpt-4").map(|c| c.id), Some("gpt-4o"));
        assert!(lookup(&reg, "o3-min").is_none(), "mid-token, no boundary");
        // Too-short queries never prefix-match (→ UNKNOWN).
        assert!(lookup(&reg, "o").is_none());
        assert!(lookup(&reg, "").is_none());
        // A real family prefix ending on a boundary still resolves.
        assert_eq!(lookup(&reg, "claude-sonnet").unwrap().provider, "anthropic");
        // `gpt-4` is seeded (real legacy GPT-4), so the exact match wins —
        // it never family-aliases to the much cheaper gpt-4.1.
        assert_eq!(lookup(&reg, "gpt-4").unwrap().id, "gpt-4");
    }

    #[test]
    fn bare_gpt4_resolves_to_seeded_real_gpt4_not_family_alias() {
        // R2 honesty fix: a user pointing at REAL OpenAI gpt-4 must get its
        // real 8k/$30/$60 config, not gpt-4.1's ~15x-lower pricing.
        let reg = build_registry_from(&[], &[]);
        let cfg = lookup(&reg, "gpt-4").expect("gpt-4 in seed");
        assert_eq!(cfg.id, "gpt-4");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.context_window, 8_192);
        assert!((cfg.input_price_per_mtok - 30.0).abs() < f64::EPSILON);
        assert!((cfg.output_price_per_mtok - 60.0).abs() < f64::EPSILON);
        let turbo = lookup(&reg, "gpt-4-turbo").expect("gpt-4-turbo in seed");
        assert_eq!(turbo.id, "gpt-4-turbo");
        assert_eq!(turbo.context_window, 128_000);
        assert!((turbo.input_price_per_mtok - 10.0).abs() < f64::EPSILON);
        assert!((turbo.output_price_per_mtok - 30.0).abs() < f64::EPSILON);
        // Family-prefix aliasing still works for genuinely-absent ids.
        assert_eq!(lookup(&reg, "gpt-4.1").unwrap().id, "gpt-4.1");
        assert_eq!(lookup(&reg, "claude-opus").unwrap().id, "claude-opus-4-8");
    }

    #[test]
    fn short_bare_id_resolves_via_exact_tail_suffix() {
        // R2 regression fix: a real short id present ONLY router-style in
        // the catalog (`openai/o1`) + bare query `o1` must resolve with its
        // real cost — the MIN_FUZZY_LEN gate applies to the prefix step
        // only, not the exact-tail suffix step.
        let catalog = vec![
            serde_json::json!({ "model_id": "openai/o1", "provider": "openai",
                "context_window": 200_000, "input_price": 15.0, "output_price": 60.0 }),
            serde_json::json!({ "model_id": "openai/o1-mini", "provider": "openai",
                "context_window": 128_000, "input_price": 1.10, "output_price": 4.40 }),
        ];
        let reg = build_registry_from(&[], &catalog);
        let cfg = lookup(&reg, "o1").expect("short bare id via exact-tail suffix");
        assert_eq!(cfg.id, "openai/o1", "exact tail — never grabs o1-mini");
        assert!((cfg.input_price_per_mtok - 15.0).abs() < f64::EPSILON);
        assert!((cfg.output_price_per_mtok - 60.0).abs() < f64::EPSILON);
        // With only the neighbour present, `o1` stays unresolved (no tail
        // equals it, and it is too short to prefix-match).
        let reg = build_registry_from(&[], &catalog[1..]);
        assert!(lookup(&reg, "o1").is_none());
        // Degenerate empty query never suffix-matches.
        assert!(lookup(&reg, "").is_none());
    }

    #[test]
    fn suffix_tiebreak_prefers_canonical_provider() {
        // C1: bare "o3" across azure/openai must be deterministic AND sensible
        // — the direct vendor wins, not whatever HashMap order or lexicography
        // surfaces.
        let catalog = vec![
            serde_json::json!({ "model_id": "azure/o3-pro", "provider": "azure",
                "context_window": 200_000, "input_price": 9.0 }),
            serde_json::json!({ "model_id": "openai/o3-pro", "provider": "openai",
                "context_window": 200_000, "input_price": 2.0 }),
        ];
        let reg = build_registry_from(&[], &catalog);
        let cfg = lookup(&reg, "o3-pro").expect("bare o3-pro suffix-matches");
        assert_eq!(cfg.provider, "openai", "canonical provider wins the tie");
    }

    #[test]
    fn recency_key_ignores_dates_and_param_counts() {
        // C3: a snapshot date must not masquerade as a higher version …
        assert_eq!(recency_key("claude-sonnet-4-20250514"), vec![4]);
        assert!(recency_key("claude-sonnet-4-6") > recency_key("claude-sonnet-4-20250514"));
        // … nor a param-count / size token.
        assert_eq!(recency_key("yi-34b"), Vec::<u64>::new());
        assert!(recency_key("claude-sonnet-5") > recency_key("yi-34b"));
        assert_eq!(recency_key("qwen-2.5-72b-instruct"), vec![2, 5]);
        // Genuine versions still compare correctly.
        assert!(recency_key("glm-5.2") > recency_key("glm-5"));
        assert_eq!(recency_key("gpt-4o"), vec![4]);
    }

    #[test]
    fn recency_key_ignores_dashed_date_stamps() {
        // R2: OpenAI's real snapshot format is dashed — the date must not
        // masquerade as a higher version within the family.
        assert_eq!(recency_key("o3-mini-2025-01-31"), vec![3]);
        assert!(recency_key("o3-mini-2025-01-31") <= recency_key("o3-pro"));
        assert!(recency_key("gpt-4.1") > recency_key("gpt-4-2025-11-20"));
        assert_eq!(recency_key("claude-3-5-sonnet-2024-10-22"), vec![3, 5]);
        // Near-misses stay version components: implausible month, and
        // non-dash separators.
        assert_eq!(recency_key("foo-2025-13-01"), vec![2025, 13, 1]);
        assert_eq!(recency_key("bar-2024.1"), vec![2024, 1]);
        // Compact stamps and plain versions are unchanged.
        assert_eq!(recency_key("claude-sonnet-4-20250514"), vec![4]);
        assert_eq!(recency_key("claude-sonnet-4-6"), vec![4, 6]);
    }

    #[test]
    fn env_var_name_and_url_validation() {
        assert!(is_valid_env_var_name("ZAI_API_KEY"));
        assert!(is_valid_env_var_name("_x1"));
        assert!(!is_valid_env_var_name(""));
        assert!(!is_valid_env_var_name("1ABC"));
        assert!(!is_valid_env_var_name("sk-live-abc123")); // a pasted key
        assert!(!is_valid_env_var_name("A B"));
        assert!(is_http_url("https://api.z.ai/api/anthropic"));
        assert!(is_http_url("http://localhost:11434/v1"));
        assert!(!is_http_url("api.z.ai"));
        assert!(!is_http_url("ftp://x"));
        assert!(!is_http_url("https://"));
    }

    #[test]
    fn user_model_validate_rejects_junk() {
        let base = || UserModel {
            provider: "zhipu".into(),
            base_url: None,
            api_key_env: None,
            input_price: 1.0,
            output_price: 3.2,
            context_window: 200_000,
            max_output_tokens: None,
            supports_tools: None,
            supports_thinking: None,
            supports_caching: None,
        };
        assert!(base().validate().is_ok());
        // C4: NaN/inf prices (clap/TOML accept them) are rejected.
        let mut m = base();
        m.input_price = f64::NAN;
        assert!(m.validate().is_err());
        let mut m = base();
        m.output_price = f64::INFINITY;
        assert!(m.validate().is_err());
        let mut m = base();
        m.input_price = -1.0;
        assert!(m.validate().is_err());
        // Zero context window / output cap.
        let mut m = base();
        m.context_window = 0;
        assert!(m.validate().is_err());
        let mut m = base();
        m.max_output_tokens = Some(0);
        assert!(m.validate().is_err());
        // S2: a pasted key as the "env var name", and a non-url base_url.
        let mut m = base();
        m.api_key_env = Some("sk-live-abc123".into());
        assert!(m.validate().is_err());
        let mut m = base();
        m.base_url = Some("api.z.ai".into());
        assert!(m.validate().is_err());
    }

    #[test]
    fn load_user_models_drops_invalid_entries() {
        // On LOAD (not just register), a hand-edited junk entry is dropped
        // with a warning while valid siblings survive.
        let raw = r#"
[models."good"]
provider = "zhipu"
input_price = 1.0
output_price = 2.0
context_window = 128000

[models."bad-price"]
provider = "zhipu"
input_price = -1.0
output_price = 2.0
context_window = 128000

[models."zero-ctx"]
provider = "zhipu"
input_price = 1.0
output_price = 2.0
context_window = 0
"#;
        let parsed = parse_user_models(raw).unwrap();
        assert_eq!(parsed.len(), 3, "parse is structural");
        let kept: Vec<&str> = parsed
            .iter()
            .filter(|(_, m)| m.validate().is_ok())
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(kept, vec!["good"]);
    }

    #[test]
    fn register_user_model_at_roundtrip_and_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.toml");
        let entry = UserModel {
            provider: "zhipu".into(),
            base_url: Some("https://api.z.ai/api/anthropic".into()),
            api_key_env: Some("ZAI_API_KEY".into()),
            input_price: 1.0,
            output_price: 3.2,
            context_window: 200_000,
            max_output_tokens: Some(128_000),
            supports_tools: None,
            supports_thinking: Some(true),
            supports_caching: None,
        };
        register_user_model_at(&path, "glm-5.2", entry.clone()).unwrap();
        // Update in place + add a second model; both survive a reread.
        let mut updated = entry.clone();
        updated.input_price = 0.9;
        register_user_model_at(&path, "glm-5.2", updated).unwrap();
        let second = UserModel {
            provider: "local".into(),
            base_url: Some("http://localhost:11434/v1".into()),
            api_key_env: None,
            input_price: 0.0,
            output_price: 0.0,
            context_window: 32_000,
            max_output_tokens: None,
            supports_tools: None,
            supports_thinking: None,
            supports_caching: None,
        };
        register_user_model_at(&path, "qwen3-local", second).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let models = parse_user_models(&raw).unwrap();
        assert_eq!(models.len(), 2);
        let glm = models.iter().find(|(id, _)| id == "glm-5.2").unwrap();
        assert!((glm.1.input_price - 0.9).abs() < f64::EPSILON);

        // Validation: junk is rejected before touching the file.
        assert!(register_user_model_at(&path, "", entry.clone()).is_err());
        let mut bad = entry;
        bad.context_window = 0;
        assert!(register_user_model_at(&path, "x", bad).is_err());
    }

    // ── Global-registry tests (isolated from the real ~/.prism) ─────

    #[test]
    fn exact_lookup() {
        let _iso = isolate();
        let cfg = get_model_config("claude-opus-4-6");
        assert_eq!(cfg.id, "claude-opus-4-6");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.context_window, 200_000);
        assert!((cfg.input_price_per_mtok - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn openrouter_prefix_strip() {
        let _iso = isolate();
        let cfg = get_model_config("anthropic/claude-sonnet-4-6");
        assert_eq!(cfg.id, "claude-sonnet-4-6");
    }

    #[test]
    fn fable_is_first_class_not_unknown_fallback() {
        let _iso = isolate();
        // The subagent default model must have REAL budget/context accounting.
        let cfg = get_model_config("claude-fable-5");
        assert_eq!(cfg.id, "claude-fable-5");
        assert_eq!(cfg.context_window, 1_000_000);
        assert_eq!(cfg.max_output_tokens, 128_000);
        assert!((cfg.input_price_per_mtok - 10.0).abs() < f64::EPSILON);
        assert!((cfg.output_price_per_mtok - 50.0).abs() < f64::EPSILON);
        assert!(cfg.supports_caching && cfg.supports_thinking && cfg.supports_tools);
        assert_eq!(
            get_model_config("anthropic/claude-fable-5").id,
            "claude-fable-5"
        );
    }

    #[test]
    fn unknown_fallback_is_honest_and_logged() {
        let _iso = isolate();
        let cfg = get_model_config("totally-unknown-model");
        assert_eq!(cfg.provider, "unknown");
        assert_eq!(cfg.context_window, 128_000);
        assert!((cfg.input_price_per_mtok).abs() < f64::EPSILON);
        // The miss is logged (once per id, not spammed every turn).
        assert!(warn_unknown_once("some-never-seen-model"));
        assert!(!warn_unknown_once("some-never-seen-model"));
        // S4: reload() clears the warned set so a re-orphaned id warns again.
        reload();
        assert!(warn_unknown_once("some-never-seen-model"));
    }

    #[test]
    fn register_then_resolve_end_to_end() {
        let (_guard, _dir) = isolate();
        // Before registration glm-5.2 is the UNKNOWN stub…
        assert_eq!(get_model_config("glm-5.2").provider, "unknown");
        // …after `prism models register` / `/models register` it is real.
        register_user_model(
            "glm-5.2",
            UserModel {
                provider: "zhipu".into(),
                base_url: Some("https://api.z.ai/api/anthropic".into()),
                api_key_env: Some("ZAI_API_KEY".into()),
                input_price: 1.0,
                output_price: 3.2,
                context_window: 200_000,
                max_output_tokens: Some(128_000),
                supports_tools: None,
                supports_thinking: Some(true),
                supports_caching: None,
            },
        )
        .unwrap();
        let cfg = get_model_config("glm-5.2");
        assert_eq!(cfg.provider, "zhipu");
        assert_eq!(cfg.context_window, 200_000);
        assert!((cfg.input_price_per_mtok - 1.0).abs() < f64::EPSILON);
        assert!((cfg.output_price_per_mtok - 3.2).abs() < f64::EPSILON);
        assert_eq!(cfg.base_url, Some("https://api.z.ai/api/anthropic"));
        // It also shows up in listings, ranked above glm-5.
        let ids: Vec<&str> = list_models().iter().map(|m| m.id).collect();
        let pos_52 = ids.iter().position(|id| *id == "glm-5.2").unwrap();
        let pos_5 = ids.iter().position(|id| *id == "glm-5").unwrap();
        assert!(pos_52 < pos_5, "glm-5.2 must rank above glm-5: {ids:?}");
    }

    #[test]
    fn catalog_cache_roundtrip_and_ttl() {
        let (_guard, _dir) = isolate();
        assert!(load_catalog_cache().is_none());
        save_catalog_cache(&mock_catalog()).unwrap();
        let cache = load_catalog_cache().expect("cache written");
        assert_eq!(cache.models.len(), 2);
        assert!(cache.is_fresh());
        assert!(cache.age() < CATALOG_TTL);
        // The registry now serves catalog entries.
        let cfg = get_model_config("glm-5.2");
        assert_eq!(cfg.provider, "zhipu");
        assert!((cfg.input_price_per_mtok - 1.10).abs() < f64::EPSILON);
        // A stale timestamp is not fresh (offline lookups still use it).
        let stale = CatalogCache {
            fetched_at: 0,
            models: vec![],
        };
        assert!(!stale.is_fresh());
    }

    #[test]
    fn estimate_cost_sonnet() {
        let _iso = isolate();
        let usage = UsageInfo {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 200_000,
        };
        let cfg = get_model_config("claude-sonnet-4-6");
        let cost = estimate_cost(&usage, &cfg);
        // input: 1M * 3.00 / 1M = 3.00
        // output: 500K * 15.00 / 1M = 7.50
        // cache_read: 200K * 3.00 * 0.1 / 1M = 0.06
        assert!((cost - 10.56).abs() < 0.001);
    }

    #[test]
    fn default_model_providers() {
        assert_eq!(get_default_model("anthropic"), "claude-sonnet-4-6");
        assert_eq!(get_default_model("openai"), "gpt-5");
        assert_eq!(get_default_model("google"), "gemini-2.5-pro");
        assert_eq!(get_default_model("vertexai"), "gemini-2.5-pro");
        assert_eq!(get_default_model("zhipu"), "glm-5");
    }
}
