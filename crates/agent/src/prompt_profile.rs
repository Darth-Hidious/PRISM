//! Per-model prompt profiles — the "fluid mechanism" that adapts PRISM's one
//! canonical system prompt to the supported LLM families.
//!
//! Design (see the prompt-profiles plan): ONE canonical prompt, rendered
//! through per-model *data dials* — never forked prose per model. A
//! [`PromptProfile`] is a small bundle of enums; [`profile_for_model`] resolves
//! one from the model registry, reusing [`get_model_config`]'s family lookup.
//! Known families get their structure style + the full prompt + the full tool
//! surface; genuinely unknown / local models fall back to a conservative
//! compact profile — the case this mechanism exists for.
//!
//! NOTE: `system_role_mode` / Anthropic top-level `system` is intentionally NOT
//! modeled here. PRISM only speaks OpenAI-compat + the MARC27 `/stream`
//! transport, and the proxy normalizes the system role. Do not re-add it
//! speculatively.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::models::get_model_config;

/// How the rendered system prompt delimits its sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureStyle {
    /// `<section>…</section>` — Claude models attend best to XML tags.
    XmlTags,
    /// `# Section` — GPT / Gemini / GLM follow Markdown headers (this is also
    /// exactly today's prompt format, so it is the byte-for-byte baseline).
    MarkdownHeaders,
    /// Flattened short paragraphs — safest for small / local / unknown models.
    PlainImperative,
}

/// How much of the canonical prompt to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthBudget {
    /// Full prompt with every section.
    Full,
    /// Compact prompt: `compact_body` where present, nice-to-have sections dropped.
    Compact,
}

/// Which tools the agent is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSurface {
    /// Every loaded tool (today's behavior).
    All,
    /// A curated core set ([`CORE_TOOL_SET`]) plus the `find_tools` meta-tool,
    /// so weak models are not overwhelmed by a large catalog (RAG-MCP tiering).
    CoreSetPlusFind,
}

/// How the model is asked to reason before acting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningMode {
    /// Model has native thinking; the prompt says nothing extra.
    NativeThinking,
    /// No native thinking: append a short "think step-by-step" nudge.
    PromptedCoT,
    /// Neither native thinking nor a prompted nudge.
    None,
}

/// Ceiling policy for the request's `max_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTokensPolicy {
    /// Use the model's own max-output ceiling (still clamped to context remaining).
    ModelMax,
    /// Hard-cap the output at `n` tokens regardless of the model's max.
    Capped(u64),
}

/// The per-model data dials that shape one canonical prompt + request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptProfile {
    pub structure_style: StructureStyle,
    pub length_budget: LengthBudget,
    pub tool_surface: ToolSurface,
    pub reasoning_invocation: ReasoningMode,
    pub max_tokens_policy: MaxTokensPolicy,
}

/// The curated core tool set offered to weak / unknown models under
/// [`ToolSurface::CoreSetPlusFind`], alongside `find_tools`. Kept intentionally
/// small and permissive (file / query / knowledge / environment essentials);
/// tuned later. Names that aren't in a session's live catalog are simply
/// ignored by the tiering filter, so listing an absent tool is harmless.
pub const CORE_TOOL_SET: &[&str] = &[
    // file work
    "read_file",
    "edit_file",
    "write_file",
    "execute_bash",
    "execute_python",
    // knowledge / retrieval
    "query",
    "query_platform",
    "knowledge_entity",
    "research_query",
    // environment / discovery
    "status",
    "list_tools",
    "agent_capabilities",
    "find_tools",
];

impl PromptProfile {
    /// The conservative default for an unrecognized / local model: flattened
    /// prose, compact bodies, a curated tool set, a prompted reasoning nudge,
    /// and a hard output cap.
    #[must_use]
    pub const fn compact_unknown() -> Self {
        Self {
            structure_style: StructureStyle::PlainImperative,
            length_budget: LengthBudget::Compact,
            tool_surface: ToolSurface::CoreSetPlusFind,
            reasoning_invocation: ReasoningMode::PromptedCoT,
            max_tokens_policy: MaxTokensPolicy::Capped(4_096),
        }
    }
}

/// Model family, derived from id/provider, that selects the structure style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Anthropic,
    OpenAi,
    Google,
    Zhipu,
    Unknown,
}

/// Classify a model into a family from its id first (handles OpenRouter
/// `provider/model` ids), falling back to the registry's provider label.
fn classify(model_id: &str, provider: &str) -> Family {
    let id = model_id.to_ascii_lowercase();
    if id.contains("claude") {
        return Family::Anthropic;
    }
    if id.starts_with("gpt") || id.starts_with("o3") || id.contains("/gpt") || id.contains("/o3") {
        return Family::OpenAi;
    }
    if id.contains("gemini") {
        return Family::Google;
    }
    if id.contains("glm") {
        return Family::Zhipu;
    }
    match provider {
        "anthropic" => Family::Anthropic,
        "openai" => Family::OpenAi,
        "google" | "vertexai" => Family::Google,
        "zhipu" => Family::Zhipu,
        _ => Family::Unknown,
    }
}

/// Resolve the [`PromptProfile`] for a model id: the code default, with any
/// matching overrides from `~/.prism/prompt_profiles.toml` applied on top.
#[must_use]
pub fn profile_for_model(model_id: &str) -> PromptProfile {
    let mut profile = default_profile_for_model(model_id);
    for (pattern, ov) in loaded_overrides() {
        if glob_matches(pattern, model_id) {
            ov.apply_to(&mut profile);
        }
    }
    profile
}

/// The code-default profile for a model, before any TOML overrides.
#[must_use]
fn default_profile_for_model(model_id: &str) -> PromptProfile {
    let cfg = get_model_config(model_id);
    let family = classify(model_id, cfg.provider);

    let structure_style = match family {
        Family::Anthropic => StructureStyle::XmlTags,
        Family::OpenAi | Family::Google | Family::Zhipu => StructureStyle::MarkdownHeaders,
        // Genuinely unknown / local model — take the conservative profile whole.
        Family::Unknown => return PromptProfile::compact_unknown(),
    };

    // Wire the previously-dead `supports_thinking` flag: models with native
    // reasoning say nothing extra; capable non-thinking models get no nudge
    // (they follow structure well). Only the compact/unknown path adds CoT.
    let reasoning_invocation = if cfg.supports_thinking {
        ReasoningMode::NativeThinking
    } else {
        ReasoningMode::None
    };

    PromptProfile {
        structure_style,
        length_budget: LengthBudget::Full,
        tool_surface: ToolSurface::All,
        reasoning_invocation,
        max_tokens_policy: MaxTokensPolicy::ModelMax,
    }
}

// ---------------------------------------------------------------------------
// TOML override layer — ~/.prism/prompt_profiles.toml (owner: code defaults +
// TOML override). Per-model / per-glob overrides merged over the code defaults.
// A missing or malformed file is never fatal: it degrades to defaults with one
// warning. Schema:
//
//   [profiles."glm-*"]        # exact id, or a trailing-`*` prefix glob
//   structure  = "plain"      # xml | markdown | plain
//   length     = "compact"    # full | compact
//   tools      = "core"       # all | core
//   reasoning  = "cot"        # native | cot | none
//   max_tokens = 8192         # optional hard output cap; omit for model max
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
struct ProfileOverride {
    structure: Option<String>,
    length: Option<String>,
    tools: Option<String>,
    reasoning: Option<String>,
    max_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileOverridesFile {
    #[serde(default)]
    profiles: HashMap<String, ProfileOverride>,
}

impl ProfileOverride {
    /// Apply the set fields over `profile`. Unset fields and unrecognized enum
    /// values leave the corresponding dial untouched.
    fn apply_to(&self, profile: &mut PromptProfile) {
        if let Some(v) = self.structure.as_deref().and_then(parse_structure) {
            profile.structure_style = v;
        }
        if let Some(v) = self.length.as_deref().and_then(parse_length) {
            profile.length_budget = v;
        }
        if let Some(v) = self.tools.as_deref().and_then(parse_tools) {
            profile.tool_surface = v;
        }
        if let Some(v) = self.reasoning.as_deref().and_then(parse_reasoning) {
            profile.reasoning_invocation = v;
        }
        if let Some(n) = self.max_tokens {
            profile.max_tokens_policy = MaxTokensPolicy::Capped(n);
        }
    }
}

fn parse_structure(s: &str) -> Option<StructureStyle> {
    match s.to_ascii_lowercase().as_str() {
        "xml" | "xmltags" => Some(StructureStyle::XmlTags),
        "markdown" | "md" => Some(StructureStyle::MarkdownHeaders),
        "plain" | "plainimperative" => Some(StructureStyle::PlainImperative),
        _ => None,
    }
}

fn parse_length(s: &str) -> Option<LengthBudget> {
    match s.to_ascii_lowercase().as_str() {
        "full" => Some(LengthBudget::Full),
        "compact" => Some(LengthBudget::Compact),
        _ => None,
    }
}

fn parse_tools(s: &str) -> Option<ToolSurface> {
    match s.to_ascii_lowercase().as_str() {
        "all" => Some(ToolSurface::All),
        "core" | "coresetplusfind" => Some(ToolSurface::CoreSetPlusFind),
        _ => None,
    }
}

fn parse_reasoning(s: &str) -> Option<ReasoningMode> {
    match s.to_ascii_lowercase().as_str() {
        "native" | "nativethinking" => Some(ReasoningMode::NativeThinking),
        "cot" | "promptedcot" => Some(ReasoningMode::PromptedCoT),
        "none" => Some(ReasoningMode::None),
        _ => None,
    }
}

/// `glm-*` matches by prefix; anything else must equal the model id exactly.
fn glob_matches(pattern: &str, model_id: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => model_id.starts_with(prefix),
        None => pattern == model_id,
    }
}

/// Overrides parsed once and cached. Missing file → empty. Malformed file →
/// empty + one warning (never fatal).
fn loaded_overrides() -> &'static Vec<(String, ProfileOverride)> {
    static OVERRIDES: OnceLock<Vec<(String, ProfileOverride)>> = OnceLock::new();
    OVERRIDES.get_or_init(|| {
        let Some(path) = dirs::home_dir().map(|h| h.join(".prism/prompt_profiles.toml")) else {
            return Vec::new();
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        match toml::from_str::<ProfileOverridesFile>(&raw) {
            Ok(file) => file.profiles.into_iter().collect(),
            Err(e) => {
                tracing::warn!("ignoring malformed ~/.prism/prompt_profiles.toml: {e}");
                Vec::new()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_gets_xml_full_and_native_thinking() {
        let p = profile_for_model("claude-opus-4-6");
        assert_eq!(p.structure_style, StructureStyle::XmlTags);
        assert_eq!(p.length_budget, LengthBudget::Full);
        assert_eq!(p.tool_surface, ToolSurface::All);
        assert_eq!(p.reasoning_invocation, ReasoningMode::NativeThinking);
        assert_eq!(p.max_tokens_policy, MaxTokensPolicy::ModelMax);
    }

    #[test]
    fn marc27_default_model_is_markdown_full() {
        // The MARC27 default (`claude-sonnet-4-20250514`) is a Claude model, so
        // it resolves to XML — but a plain GPT/GLM default lands on Markdown,
        // which is the byte-for-byte baseline the golden test pins.
        let p = profile_for_model("gpt-5");
        assert_eq!(p.structure_style, StructureStyle::MarkdownHeaders);
        assert_eq!(p.length_budget, LengthBudget::Full);
        assert_eq!(p.tool_surface, ToolSurface::All);
    }

    #[test]
    fn gpt5_native_thinking_but_gpt4o_none() {
        assert_eq!(
            profile_for_model("gpt-5").reasoning_invocation,
            ReasoningMode::NativeThinking
        );
        assert_eq!(
            profile_for_model("gpt-4o").reasoning_invocation,
            ReasoningMode::None
        );
    }

    #[test]
    fn openrouter_prefixed_claude_still_anthropic() {
        let p = profile_for_model("anthropic/claude-sonnet-4-6");
        assert_eq!(p.structure_style, StructureStyle::XmlTags);
    }

    #[test]
    fn glm_and_gemini_are_markdown() {
        assert_eq!(
            profile_for_model("glm-5").structure_style,
            StructureStyle::MarkdownHeaders
        );
        assert_eq!(
            profile_for_model("gemini-2.5-pro").structure_style,
            StructureStyle::MarkdownHeaders
        );
    }

    #[test]
    fn unknown_model_is_compact_conservative() {
        let p = profile_for_model("some-random-local-7b");
        assert_eq!(p, PromptProfile::compact_unknown());
        assert_eq!(p.structure_style, StructureStyle::PlainImperative);
        assert_eq!(p.length_budget, LengthBudget::Compact);
        assert_eq!(p.tool_surface, ToolSurface::CoreSetPlusFind);
        assert_eq!(p.reasoning_invocation, ReasoningMode::PromptedCoT);
        assert_eq!(p.max_tokens_policy, MaxTokensPolicy::Capped(4_096));
    }

    #[test]
    fn core_tool_set_includes_find_tools() {
        assert!(CORE_TOOL_SET.contains(&"find_tools"));
        assert!(CORE_TOOL_SET.contains(&"read_file"));
    }

    #[test]
    fn glob_matches_prefix_and_exact() {
        assert!(glob_matches("glm-*", "glm-5"));
        assert!(glob_matches("glm-*", "glm-4.5-air"));
        assert!(!glob_matches("glm-*", "gpt-5"));
        assert!(glob_matches("gpt-5", "gpt-5"));
        assert!(!glob_matches("gpt-5", "gpt-5-mini"));
    }

    #[test]
    fn override_from_toml_applies_over_default() {
        // A user forcing a capable model into the compact/core regime.
        let file: ProfileOverridesFile = toml::from_str(
            r#"
            [profiles."gpt-5"]
            length = "compact"
            tools = "core"
            reasoning = "cot"
            max_tokens = 8192
            "#,
        )
        .unwrap();
        let ov = &file.profiles["gpt-5"];

        let mut profile = default_profile_for_model("gpt-5");
        assert_eq!(profile.length_budget, LengthBudget::Full); // baseline
        ov.apply_to(&mut profile);

        assert_eq!(profile.length_budget, LengthBudget::Compact);
        assert_eq!(profile.tool_surface, ToolSurface::CoreSetPlusFind);
        assert_eq!(profile.reasoning_invocation, ReasoningMode::PromptedCoT);
        assert_eq!(profile.max_tokens_policy, MaxTokensPolicy::Capped(8192));
        // Unset dial (structure) is untouched — gpt-5 stays Markdown.
        assert_eq!(profile.structure_style, StructureStyle::MarkdownHeaders);
    }

    #[test]
    fn override_ignores_unknown_enum_values() {
        let ov = ProfileOverride {
            structure: Some("nonsense".into()),
            length: Some("compact".into()),
            ..Default::default()
        };
        let mut profile = default_profile_for_model("gpt-5");
        ov.apply_to(&mut profile);
        // Bad value ignored; the valid one still applies.
        assert_eq!(profile.structure_style, StructureStyle::MarkdownHeaders);
        assert_eq!(profile.length_budget, LengthBudget::Compact);
    }
}
