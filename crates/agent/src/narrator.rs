//! The story of a run, told by a model as it happens.
//!
//! The transcript is dense: cards, source tables, digests. A materials
//! scientist watching a research run asked for something else beside it — a
//! box per step saying, in plain language, what was just done and what it
//! means for the question, written by a model rather than a template, because
//! a template cannot say what a result MEANS. Each box points at the exact
//! transcript entry it narrates (its tool call id), so the reader can jump.
//!
//! The narrator is a second, small conversation on the session's own model:
//! it is handed ONE step (tool, arguments, an excerpt of the result) and
//! nothing else, so it cannot narrate what did not happen. It runs off the
//! agent's path — spawned, one at a time — and never blocks or slows the
//! run; a box appears as "narrating…" at once and fills in when the model
//! answers. Failure is a box that says narration failed, not silence.
//!
//! `PRISM_STORY=0` turns it off.

use prism_llm::{ChatMessage, LlmClient, LlmConfig};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::Semaphore;

/// Result excerpt handed to the narrator. Long results are cut; the narrator
/// is told they were.
const RESULT_EXCERPT_CHARS: usize = 1_500;
const ARGS_EXCERPT_CHARS: usize = 600;

/// Tools whose steps are the agent's own bookkeeping, not research: narrating
/// them would tell the reader "looked up which tools exist" ten times.
const QUIET_TOOLS: &[&str] = &[
    "find_tools",
    "tool_reasoning",
    "list_failures",
    "usage_status",
];

static IN_FLIGHT: OnceLock<Semaphore> = OnceLock::new();

/// Whether a step of this tool is narrated at all.
pub fn narrates(tool: &str) -> bool {
    if std::env::var("PRISM_STORY")
        .map(|v| v == "0")
        .unwrap_or(false)
    {
        return false;
    }
    !QUIET_TOOLS.contains(&tool)
}

/// The two messages the narrator is given for one step. Grounded in the step
/// alone: it is told to name counts and sources, to say when a result is
/// empty or red, and never to invent.
pub fn story_prompt(tool: &str, args: &Value, result_excerpt: &str) -> Vec<ChatMessage> {
    let args_text = {
        let s = serde_json::to_string(args).unwrap_or_default();
        if s.chars().count() > ARGS_EXCERPT_CHARS {
            format!(
                "{}…",
                s.chars().take(ARGS_EXCERPT_CHARS).collect::<String>()
            )
        } else {
            s
        }
    };
    let (excerpt, cut) = if result_excerpt.chars().count() > RESULT_EXCERPT_CHARS {
        (
            result_excerpt
                .chars()
                .take(RESULT_EXCERPT_CHARS)
                .collect::<String>(),
            true,
        )
    } else {
        (result_excerpt.to_string(), false)
    };
    let system = "You narrate a materials-research agent's work to the scientist watching it. \
                  You are given ONE step: the tool it called, the arguments, and an excerpt of \
                  the result. Write one or two plain sentences: what was just done and what it \
                  means for the research question. Name counts, databases, materials and papers \
                  that appear in the result. If the result is empty, failed, refused or marked \
                  red/indeterminate, say so and what it means. Never add facts that are not in \
                  the excerpt; never guess at what the agent will do next. No preamble, no \
                  bullet points, no markdown.";
    let user = format!(
        "Tool: {tool}\nArguments: {args_text}\nResult excerpt{}:\n{excerpt}",
        if cut {
            " (cut; the full result is longer)"
        } else {
            ""
        }
    );
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some(system.to_string()),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some(user),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ]
}

/// The notification the TUI turns into a box. `status` is `pending` (box
/// shown, model not yet answered), `done`, or `failed`.
pub fn story_payload(call_id: &str, seq: usize, tool: &str, status: &str, text: &str) -> Value {
    serde_json::json!({
        "call_id": call_id,
        "seq": seq,
        "tool": tool,
        "status": status,
        "text": text,
    })
}

/// Ask the model for the narration of one step. Returns the text.
pub async fn narrate_once(
    config: LlmConfig,
    tool: &str,
    args: &Value,
    result_excerpt: &str,
) -> anyhow::Result<String> {
    let client = LlmClient::new(config);
    let messages = story_prompt(tool, args, result_excerpt);
    let response = client.chat_with_tools(&messages, &[]).await?;
    let text = response
        .message
        .content
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        anyhow::bail!("the narrator returned no text");
    }
    Ok(text)
}

/// Narrate one finished step off the agent's path. Emits `ui.story` twice:
/// `pending` now, then `done` or `failed`. Steps are narrated one at a time
/// so the narrator never competes with the agent for the model.
pub fn narrate_step(
    config: LlmConfig,
    call_id: String,
    seq: usize,
    tool: String,
    args: Value,
    result_excerpt: String,
) {
    if !narrates(&tool) {
        return;
    }
    crate::protocol::emit_notification(
        "ui.story",
        story_payload(&call_id, seq, &tool, "pending", ""),
    );
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        crate::protocol::emit_notification(
            "ui.story",
            story_payload(
                &call_id,
                seq,
                &tool,
                "failed",
                "narration unavailable: no async runtime",
            ),
        );
        return;
    };
    handle.spawn(async move {
        let gate = IN_FLIGHT.get_or_init(|| Semaphore::new(1));
        let _permit = gate.acquire().await;
        let (status, text) = match narrate_once(config, &tool, &args, &result_excerpt).await {
            Ok(text) => ("done", text),
            Err(e) => ("failed", format!("narration failed: {e:#}")),
        };
        crate::protocol::emit_notification(
            "ui.story",
            story_payload(&call_id, seq, &tool, status, &text),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_narrator_is_handed_the_step_and_told_not_to_invent() {
        let args = serde_json::json!({"query": "RD-0120 fuel rich preburner", "source": "papers"});
        let messages = story_prompt(
            "prior_art_search",
            &args,
            "24 result(s) … openalex 20 · crossref 4",
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        let system = messages[0].content.clone().unwrap_or_default();
        assert!(system.contains("Never add facts"), "{system}");
        assert!(system.contains("red/indeterminate"), "{system}");
        let user = messages[1].content.clone().unwrap_or_default();
        assert!(user.contains("Tool: prior_art_search"), "{user}");
        assert!(user.contains("RD-0120"), "{user}");
        assert!(user.contains("openalex 20"), "{user}");
    }

    #[test]
    fn a_long_result_is_cut_and_the_narrator_is_told() {
        let long = "x".repeat(RESULT_EXCERPT_CHARS + 500);
        let messages = story_prompt("web", &serde_json::json!({}), &long);
        let user = messages[1].content.clone().unwrap_or_default();
        assert!(user.contains("(cut; the full result is longer)"), "{user}");
        assert!(
            user.chars().count() < RESULT_EXCERPT_CHARS + 300,
            "{}",
            user.chars().count()
        );
    }

    #[test]
    fn bookkeeping_tools_are_not_narrated_and_the_switch_works() {
        assert!(!narrates("find_tools"));
        assert!(!narrates("tool_reasoning"));
        assert!(narrates("prior_art_search"));
        assert!(narrates("web"));
    }

    #[test]
    fn the_box_names_its_transcript_entry() {
        let payload = story_payload("call-7", 3, "web", "done", "Searched the web…");
        assert_eq!(payload["call_id"], "call-7");
        assert_eq!(payload["seq"], 3);
        assert_eq!(payload["status"], "done");
        assert_eq!(payload["tool"], "web");
    }

    /// The narrator's answer is the box text; an empty answer is a failure,
    /// not an empty box.
    #[tokio::test]
    async fn the_model_answer_becomes_the_box() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"choices":[{"message":{"role":"assistant","content":"Searched prior art for RD-0120 preburner conditions: 24 papers, 20 of them from OpenAlex."}}]}"#)
            .create_async()
            .await;
        let config = LlmConfig {
            base_url: format!("{}/v1", server.url()),
            model: "narrator".into(),
            streaming: false,
            ..Default::default()
        };
        let text = narrate_once(
            config,
            "prior_art_search",
            &serde_json::json!({}),
            "24 result(s)",
        )
        .await
        .unwrap();
        assert!(text.starts_with("Searched prior art"), "{text}");
    }
}
