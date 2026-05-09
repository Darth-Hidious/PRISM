use std::sync::Arc;

use derive_setters::Setters;
use forge_domain::{
    ChatCompletionMessageFull, Context, ContextMessage, ConversationId, ModelId, ProviderId,
    ReasoningConfig, ResponseFormat, ResultStreamExt, UserPrompt,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::TemplateEngine;
use crate::agent::AgentService as AS;

/// Structured response for title generation using JSON format
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(title = "title")]
pub struct TitleResponse {
    /// The generated title for the conversation
    pub title: String,
}

/// Service for generating contextually appropriate titles
#[derive(Setters)]
pub struct TitleGenerator<S> {
    /// Shared reference to the agent services used for AI interactions
    services: Arc<S>,
    /// The user prompt to generate a title for
    user_prompt: UserPrompt,
    /// The model ID to use for title generation
    model_id: ModelId,
    /// Reasoning configuration for the generator.
    reasoning: Option<ReasoningConfig>,
    /// The provider ID to use for title generation
    provider_id: Option<ProviderId>,
}

impl<S: AS> TitleGenerator<S> {
    pub fn new(
        services: Arc<S>,
        user_prompt: UserPrompt,
        model_id: ModelId,
        provider_id: Option<ProviderId>,
    ) -> Self {
        Self {
            services,
            user_prompt,
            model_id,
            reasoning: None,
            provider_id,
        }
    }

    pub async fn generate(&self) -> anyhow::Result<Option<String>> {
        let template = TemplateEngine::default().render(
            "forge-system-prompt-title-generation.md",
            &Default::default(),
        )?;

        let prompt = format!("<user_prompt>{}</user_prompt>", self.user_prompt.as_str());

        // Generate JSON schema from TitleResponse using schemars
        let schema = schemars::schema_for!(TitleResponse);

        let mut ctx = Context::default()
            .temperature(1.0f32)
            .conversation_id(ConversationId::generate())
            .add_message(ContextMessage::system(template))
            .add_message(ContextMessage::user(prompt, Some(self.model_id.clone())))
            .response_format(ResponseFormat::JsonSchema(Box::new(schema)));

        // Set the reasoning if configured.
        if let Some(reasoning) = self.reasoning.as_ref() {
            ctx = ctx.reasoning(reasoning.clone());
        }

        let stream = self
            .services
            .chat_agent(&self.model_id, ctx, self.provider_id.clone())
            .await?;
        let ChatCompletionMessageFull { content, .. } = stream.into_full(false).await?;

        // Parse the response - try JSON first (structured output), fallback to plain
        // text
        match serde_json::from_str::<TitleResponse>(&content) {
            Ok(response) => Ok(Some(response.title)),
            Err(_) => {
                // Fallback: some providers' structured-output mode returns
                // malformed JSON like `{"titleSimple Greeting Test"}` (missing
                // `":"` between key and value, or stray quotes). Saving that
                // verbatim leaks JSON syntax into the conversation picker —
                // titles show up as `{"titleSimple Greeting Test"}` instead
                // of `Simple Greeting Test`. See PRISM Bug #30.
                //
                // Strip JSON-shape leftovers from the raw content so the
                // human-readable value stays. Conservative: only touches
                // characters that are clearly JSON syntax + the literal
                // "title" key; word content is preserved.
                Ok(Some(salvage_title_from_malformed(&content)))
            }
        }
    }
}

/// Best-effort extraction of a human-readable title from a content string
/// that wasn't valid JSON.
///
/// Handles the observed real-world failure shape — the upstream emits
/// `{"title<value>"}` (no `:` separator, value not quoted). Strips the
/// JSON-object braces, the `"title"` key, residual quotes, and
/// surrounding whitespace.
fn salvage_title_from_malformed(content: &str) -> String {
    let s = content.trim();
    // 1. Drop leading "{ and trailing }" if present.
    let s = s.trim_start_matches('{').trim_end_matches('}').trim();
    // 2. Drop a single leading or trailing quote (the JSON wrapper).
    let s = s.trim_start_matches('"').trim_end_matches('"').trim();
    // 3. Drop the literal `"title"` key + any `:` / `"` / space that follow.
    //    Variants we've seen: `"title":"X"`, `"titleX"`, `title:"X"`, `titleX`.
    let s = s.trim_start_matches("\"title\"");
    let s = s.trim_start_matches("title");
    let s = s
        .trim_start_matches(['"', ':', ' ', '\t'])
        .trim_end_matches(['"', ' ', '\t']);
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salvage_handles_well_formed_json_value_substring() {
        // Already-clean title text passes through unchanged.
        assert_eq!(salvage_title_from_malformed("Hello"), "Hello");
        assert_eq!(salvage_title_from_malformed("  Hello  "), "Hello");
    }

    #[test]
    fn salvage_strips_full_quoted_string() {
        assert_eq!(
            salvage_title_from_malformed("\"Hello World\""),
            "Hello World"
        );
    }

    #[test]
    fn salvage_handles_real_malformed_output_from_marc27_upstream() {
        // The actual shape we observed via PRISM_BRIDGE_DUMP: the model's
        // JSON-mode output came back missing the `":"` between key and
        // value, which serde_json correctly rejects.
        // Bug #30 reproduction.
        assert_eq!(
            salvage_title_from_malformed("{\"titleSpecific Sentence\"}"),
            "Specific Sentence"
        );
        assert_eq!(
            salvage_title_from_malformed("{\"titleSimple Greeting Test\"}"),
            "Simple Greeting Test"
        );
    }

    #[test]
    fn salvage_handles_well_quoted_but_unparseable() {
        // `"title":"Foo"` is valid JSON-fragment-ish but missing braces;
        // serde would reject it too. Should still produce a clean title.
        assert_eq!(salvage_title_from_malformed("\"title\":\"Foo\""), "Foo");
    }

    #[test]
    fn salvage_handles_object_with_proper_separator_but_missing_value_quotes() {
        assert_eq!(
            salvage_title_from_malformed("{\"title\":Bar Baz}"),
            "Bar Baz"
        );
    }

    #[test]
    fn salvage_empty_input_returns_empty() {
        assert_eq!(salvage_title_from_malformed(""), "");
        assert_eq!(salvage_title_from_malformed("   "), "");
        assert_eq!(salvage_title_from_malformed("{}"), "");
    }
}
