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
// Static seed — offline last resort ONLY
// ---------------------------------------------------------------------------
//
// Deliberately tiny: a handful of well-known models so a fresh offline
// install still has sane budget/cost accounting for the compiled-in
// defaults (`get_default_model`, the subagent default, the TUI picker's
// curated fallback). Everything else comes from the platform catalog or
// `~/.prism/models.toml` — do NOT grow this list when a new model ships.

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
        "gpt-5", "openai", 400_000, 128_000, 16_384, 1.25, 10.00, false, true,
    ),
    seed(
        "gpt-4o", "openai", 128_000, 16_384, 8_192, 2.50, 10.00, false, false,
    ),
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
        "glm-5", "zhipu", 200_000, 128_000, 16_384, 1.00, 3.20, false, false,
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
        Ok(models) => models,
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
    if model.provider.trim().is_empty() {
        bail!("--provider must not be empty");
    }
    if model.context_window == 0 {
        bail!("--context-window must be > 0");
    }
    if model.input_price < 0.0 || model.output_price < 0.0 {
        bail!("prices must be >= 0 (USD per 1M tokens)");
    }

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

/// Intern a dynamic string. The registry is built at most once per
/// (re)load, so the leak is bounded: a few hundred bytes per model id,
/// re-leaked only on explicit `reload()` (i.e. after registering a model
/// or refreshing the catalog).
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// Map one raw platform-catalog entry to a `ModelConfig`. Entries without
/// a model id are skipped. Missing limits get the same conservative
/// defaults as `UNKNOWN_MODEL_CONFIG` (128k / 16k); missing prices are $0,
/// which is what the platform means by a free model.
fn catalog_entry_to_config(v: &serde_json::Value) -> Option<ModelConfig> {
    let id = v
        .get("model_id")
        .or_else(|| v.get("id"))
        .and_then(|x| x.as_str())?;
    let get_usize = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_u64()))
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
        input_price_per_mtok: v.get("input_price").and_then(|x| x.as_f64()).unwrap_or(0.0),
        output_price_per_mtok: v
            .get("output_price")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0),
        supports_caching: get_bool("supports_prompt_caching").unwrap_or(false),
        supports_thinking: get_bool("supports_reasoning").unwrap_or(false),
        supports_tools: get_bool("supports_function_calling").unwrap_or(true),
        base_url: v.get("base_url").and_then(|x| x.as_str()).map(leak),
        api_key_env: v.get("api_key_env").and_then(|x| x.as_str()).map(leak),
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
/// Called after `models.toml` or the catalog cache changes.
pub fn reload() {
    *REGISTRY.write().expect("registry lock") = None;
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

/// Look up model configuration by ID.
///
/// Lookup order:
/// 1. Exact match
/// 2. Strip OpenRouter prefix (`provider/model-name`)
/// 3. Bare-name suffix — `glm-5.2` matches a router-style catalog id
///    `z-ai/glm-5.2` (same rule as the CLI's `resolve_catalog_model`)
/// 4. Prefix match — newest matching entry wins ([`recency_key`])
/// 5. Fallback: unknown provider, 128K context, $0 pricing — logged once
///    per id so wrong cost accounting is visible.
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
    // Newest-first tie-break shared by the fuzzy steps below.
    let newest =
        |a: &String, b: &String| recency_key(a).cmp(&recency_key(b)).then_with(|| b.cmp(a));
    // 3. Bare-name suffix: `glm-5.2` → catalog id `z-ai/glm-5.2`.
    if !model_id.contains('/')
        && let Some(cfg) = reg
            .iter()
            .filter(|(key, _)| key.split_once('/').map(|(_, tail)| tail) == Some(model_id))
            .max_by(|(a, _), (b, _)| newest(a, b))
            .map(|(_, cfg)| *cfg)
    {
        return Some(cfg);
    }
    // 4. Prefix match — deterministic: the newest matching id wins.
    reg.iter()
        .filter(|(key, _)| key.starts_with(model_id))
        .max_by(|(a, _), (b, _)| newest(a, b))
        .map(|(_, cfg)| *cfg)
}

/// Warn once per unknown model id per process. Returns whether this call
/// produced the warning (first sighting).
fn warn_unknown_once(model_id: &str) -> bool {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = WARNED
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

/// Version-ish ranking key: the numeric components of a model id, compared
/// lexicographically. `glm-5.2` → [5, 2] sorts above `glm-4.7` → [4, 7];
/// `claude-sonnet-4-20250514` → [4, 20250514]. A heuristic — good enough
/// to put newer models first in listings and prefix matches.
#[must_use]
pub fn recency_key(id: &str) -> Vec<u64> {
    id.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
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
        assert_eq!(glm_ids, vec!["glm-5.2", "glm-5"]);
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
