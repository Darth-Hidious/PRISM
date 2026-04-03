use std::collections::HashMap;
use std::sync::OnceLock;

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
}

// ---------------------------------------------------------------------------
// MODEL_REGISTRY — lazily-initialized static registry
// ---------------------------------------------------------------------------

static MODEL_REGISTRY: OnceLock<HashMap<&'static str, ModelConfig>> = OnceLock::new();

fn registry() -> &'static HashMap<&'static str, ModelConfig> {
    MODEL_REGISTRY.get_or_init(|| {
        let mut m = HashMap::new();

        macro_rules! reg {
            ($id:expr, $prov:expr, $ctx:expr, $max_out:expr, $default:expr,
             $pin:expr, $pout:expr $(, cache=$cache:expr)? $(, think=$think:expr)?) => {{
                #[allow(unused_mut, unused_assignments)]
                let mut cache = false;
                $(cache = $cache;)?
                #[allow(unused_mut, unused_assignments)]
                let mut think = false;
                $(think = $think;)?
                m.insert($id, ModelConfig {
                    id: $id,
                    provider: $prov,
                    context_window: $ctx,
                    max_output_tokens: $max_out,
                    default_max_tokens: $default,
                    input_price_per_mtok: $pin,
                    output_price_per_mtok: $pout,
                    supports_caching: cache,
                    supports_thinking: think,
                    supports_tools: true,
                });
            }};
        }

        // --- Anthropic ---
        reg!("claude-opus-4-6",           "anthropic", 200_000, 128_000, 32_768, 5.00, 25.00, cache=true, think=true);
        reg!("claude-sonnet-4-6",         "anthropic", 200_000,  64_000, 16_384, 3.00, 15.00, cache=true, think=true);
        reg!("claude-sonnet-4-20250514",  "anthropic", 200_000,  64_000, 16_384, 3.00, 15.00, cache=true, think=true);
        reg!("claude-sonnet-4-20250318",  "anthropic", 200_000,  64_000, 16_384, 3.00, 15.00, cache=true, think=true);
        reg!("claude-haiku-4-5-20251001", "anthropic", 200_000,  64_000,  8_192, 1.00,  5.00, cache=true);

        // --- OpenAI ---
        reg!("gpt-4o",      "openai", 128_000,   16_384,  8_192, 2.50, 10.00);
        reg!("gpt-4o-mini",  "openai", 128_000,   16_384,  4_096, 0.15,  0.60);
        reg!("gpt-4.1",     "openai", 1_000_000,  32_768, 16_384, 2.00,  8.00);
        reg!("gpt-4.1-mini", "openai", 1_000_000,  32_768,  8_192, 0.40,  1.60);
        reg!("gpt-5",       "openai", 400_000,  128_000, 16_384, 1.25, 10.00, think=true);
        reg!("o3",           "openai", 200_000,  100_000, 16_384, 2.00,  8.00, think=true);
        reg!("o3-mini",      "openai", 200_000,  100_000,  8_192, 1.10,  4.40, think=true);

        // --- Google ---
        reg!("gemini-2.5-pro",   "google", 1_000_000, 65_536, 16_384, 1.25, 10.00, think=true);
        reg!("gemini-2.5-flash", "google", 1_000_000, 65_536,  8_192, 0.30,  2.50);
        reg!("gemini-3.1-pro",   "google", 1_000_000, 65_536, 16_384, 2.00, 12.00, think=true);

        // --- Zhipu ---
        reg!("glm-5",       "zhipu", 200_000, 128_000, 16_384, 1.00, 3.20);
        reg!("glm-4.7",     "zhipu", 128_000,  16_384,  8_192, 0.38, 1.70);
        reg!("glm-4.5-air", "zhipu", 128_000,  16_384,  4_096, 0.10, 0.50);

        m
    })
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
/// 3. Prefix match (first registry key starting with `model_id`)
/// 4. Fallback: unknown provider, 128K context, $0 pricing
#[must_use]
pub fn get_model_config(model_id: &str) -> ModelConfig {
    let reg = registry();

    // 1. Exact match
    if let Some(cfg) = reg.get(model_id) {
        return *cfg;
    }

    // 2. Strip OpenRouter-style prefix
    if let Some(stripped) = model_id.split_once('/').map(|(_, s)| s) {
        if let Some(cfg) = reg.get(stripped) {
            return *cfg;
        }
    }

    // 3. Prefix match
    for (key, cfg) in reg {
        if key.starts_with(model_id) {
            return *cfg;
        }
    }

    // 4. Fallback
    UNKNOWN_MODEL_CONFIG
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
};

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

    #[test]
    fn exact_lookup() {
        let cfg = get_model_config("claude-opus-4-6");
        assert_eq!(cfg.id, "claude-opus-4-6");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.context_window, 200_000);
        assert!((cfg.input_price_per_mtok - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn openrouter_prefix_strip() {
        let cfg = get_model_config("anthropic/claude-sonnet-4-6");
        assert_eq!(cfg.id, "claude-sonnet-4-6");
    }

    #[test]
    fn prefix_match() {
        let cfg = get_model_config("claude-opus");
        assert_eq!(cfg.provider, "anthropic");
    }

    #[test]
    fn unknown_fallback() {
        let cfg = get_model_config("totally-unknown-model");
        assert_eq!(cfg.provider, "unknown");
        assert_eq!(cfg.context_window, 128_000);
        assert!((cfg.input_price_per_mtok).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_cost_sonnet() {
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
        // total: 10.56
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

    #[test]
    fn registry_has_all_models() {
        let reg = registry();
        assert!(reg.len() >= 18, "Expected 18+ models, got {}", reg.len());
    }
}
