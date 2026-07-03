use derive_more::derive::Display;
use derive_setters::Setters;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum_macros::EnumString;

/// Represents input modalities that a model can accept
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum InputModality {
    /// Text input (all models support this)
    Text,
    /// Image input (vision-capable models)
    Image,
}

/// Default input modalities when not specified (text-only)
fn default_input_modalities() -> Vec<InputModality> {
    vec![InputModality::Text]
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Setters)]
pub struct Model {
    pub id: ModelId,
    pub name: Option<String>,
    pub description: Option<String>,
    pub context_length: Option<u64>,
    // TODO: add provider information to the model
    pub tools_supported: Option<bool>,
    /// Whether the model supports parallel tool calls
    pub supports_parallel_tool_calls: Option<bool>,
    /// Whether the model supports reasoning
    pub supports_reasoning: Option<bool>,
    /// Input modalities supported by the model (defaults to text-only)
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
}

impl Model {
    /// Effective context window in tokens, with sane fallbacks.
    ///
    /// Returns:
    ///   1. `self.context_length` if the upstream registry reported it,
    ///   2. otherwise a value inferred from `self.id` against a small
    ///      registry of well-known model families (so multi-hour
    ///      chats on Gemini 3 / Claude / GPT don't get artificially
    ///      compacted at 128K when the model supports 1M),
    ///   3. otherwise `None` — caller should fall back to its own
    ///      conservative default.
    ///
    /// The id-based fallback matters because the MARC27 platform's
    /// `/projects/{id}/llm/models/hosted` endpoint sometimes returns
    /// `context_length: null` for active models, which silently caps
    /// the per-model auto-compaction threshold at the configured
    /// default and makes long Gemini 3 / Claude sessions feel
    /// shorter than the user paid for.
    pub fn effective_context_length(&self) -> Option<u64> {
        if let Some(n) = self.context_length {
            return Some(n);
        }
        infer_context_length(self.id.as_str())
    }
}

/// Best-effort context-length lookup keyed on common model-id
/// patterns. Conservative — when in doubt, returns the smaller of
/// plausible options. Pattern matched against the lowercased id.
fn infer_context_length(id: &str) -> Option<u64> {
    let lc = id.to_ascii_lowercase();

    // Anthropic Claude
    if lc.starts_with("claude-") {
        // 4-series, 3.5/3.7-sonnet, 3-opus all expose 200K.
        return Some(200_000);
    }

    // Google Gemini
    if lc.starts_with("gemini-") {
        // 1.5-pro is 2M; 2.x/3.x are 1M. Conservative: 1M.
        if lc.contains("1.5-pro") {
            return Some(2_000_000);
        }
        return Some(1_000_000);
    }

    // OpenAI
    if lc.starts_with("gpt-5") {
        // gpt-5 / gpt-5.5: 256K.
        return Some(256_000);
    }
    if lc.starts_with("gpt-4o") || lc.starts_with("gpt-4.1") {
        return Some(128_000);
    }
    if lc.starts_with("gpt-4-turbo") || lc.starts_with("gpt-4-0125") {
        return Some(128_000);
    }
    if lc.starts_with("gpt-4-32k") {
        return Some(32_000);
    }
    if lc.starts_with("gpt-4") {
        return Some(8_000);
    }
    if lc.starts_with("gpt-3.5-turbo-16k") {
        return Some(16_000);
    }
    if lc.starts_with("gpt-3.5") {
        return Some(4_000);
    }
    if lc.starts_with("o1") || lc.starts_with("o3") || lc.starts_with("o4") {
        return Some(128_000);
    }

    // Mistral
    if lc.starts_with("mistral-large") || lc.starts_with("mistral-medium") {
        return Some(128_000);
    }

    // DeepSeek
    if lc.starts_with("deepseek-") {
        return Some(128_000);
    }

    // Llama-3.x via Ollama / open-router
    if lc.contains("llama-3.1-405b") {
        return Some(128_000);
    }
    if lc.contains("llama-3.1") || lc.contains("llama-3.2") || lc.contains("llama-3.3") {
        return Some(128_000);
    }
    if lc.contains("llama-3") {
        return Some(8_000);
    }

    // Unknown — let caller fall back.
    None
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct Parameters {
    pub tool_supported: bool,
}

impl Parameters {
    pub fn new(tool_supported: bool) -> Self {
        Self { tool_supported }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Hash, Eq, Display, JsonSchema)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn new<T: Into<String>>(id: T) -> Self {
        Self(id.into())
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        ModelId(value)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        ModelId(value.to_string())
    }
}

impl ModelId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for ModelId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ModelId(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, context_length: Option<u64>) -> Model {
        Model {
            id: ModelId::new(id),
            name: None,
            description: None,
            context_length,
            tools_supported: None,
            supports_parallel_tool_calls: None,
            supports_reasoning: None,
            input_modalities: vec![InputModality::Text],
        }
    }

    #[test]
    fn effective_uses_platform_value_when_set() {
        // Even for a model the registry knows about, prefer the
        // upstream-reported value (the platform may have a custom
        // window for cost / safety reasons).
        let m = model("gemini-3.1-flash-lite-preview", Some(500_000));
        assert_eq!(m.effective_context_length(), Some(500_000));
    }

    #[test]
    fn effective_falls_back_to_id_registry_for_gemini() {
        // The MARC27 platform's models endpoint returns null for
        // Gemini-3 today; without the registry the default 128K
        // would clip Gemini's actual 1M context.
        let m = model("gemini-3.1-flash-lite-preview", None);
        assert_eq!(m.effective_context_length(), Some(1_000_000));
    }

    #[test]
    fn effective_falls_back_for_claude() {
        let m = model("claude-sonnet-4-20250514", None);
        assert_eq!(m.effective_context_length(), Some(200_000));
    }

    #[test]
    fn effective_falls_back_for_gpt5() {
        let m = model("gpt-5.5", None);
        assert_eq!(m.effective_context_length(), Some(256_000));
    }

    #[test]
    fn effective_falls_back_for_gpt4o() {
        let m = model("gpt-4o", None);
        assert_eq!(m.effective_context_length(), Some(128_000));
    }

    #[test]
    fn effective_returns_none_for_unknown_model() {
        // Caller (Agent::compaction_threshold) applies its own
        // 128K default in this case.
        let m = model("some-private-fine-tune-v1", None);
        assert_eq!(m.effective_context_length(), None);
    }

    #[test]
    fn effective_handles_case_insensitively() {
        let m = model("GPT-5.5", None);
        assert_eq!(m.effective_context_length(), Some(256_000));
    }

    #[test]
    fn effective_returns_none_for_empty_id() {
        let m = model("", None);
        assert_eq!(m.effective_context_length(), None);
    }
}
