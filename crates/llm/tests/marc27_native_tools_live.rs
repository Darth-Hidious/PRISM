//! LIVE probe — does the MARC27 platform LLM endpoint accept an OpenAI-style
//! `tools` array, and does it stream native `tool_calls` back?
//!
//! This is the evidence behind sending real tool definitions on the MARC27
//! path instead of a text summary (see `build_tool_guidance_block` in the
//! crate root). It is `#[ignore]`d because it spends real credits and needs a
//! logged-in `~/.prism/credentials.json`; the offline gates never run it.
//!
//! Run it explicitly:
//!
//! ```text
//! cargo test -p prism-llm --test marc27_native_tools_live -- --ignored --nocapture
//! ```
//!
//! Override the model with `PRISM_LIVE_MODEL` (default: the cheapest hosted
//! model that supports tools).

use std::time::Duration;

/// (api_base, bearer_token, project_id) from the SDK credential mirror.
fn creds() -> Option<(String, String, String)> {
    let home = std::env::var("HOME").ok()?;
    let raw = std::fs::read_to_string(format!("{home}/.prism/credentials.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let url = v.get("platform_url")?.as_str()?.trim_end_matches('/');
    let api_base = if url.ends_with("/api/v1") {
        url.to_string()
    } else {
        format!("{url}/api/v1")
    };
    Some((
        api_base,
        v.get("access_token")?.as_str()?.to_string(),
        v.get("project_id")?.as_str()?.to_string(),
    ))
}

/// A minimal, unambiguous tool the model can only satisfy by calling it.
fn probe_tools() -> serde_json::Value {
    serde_json::json!([{
        "type": "function",
        "function": {
            "name": "lookup_property",
            "description": "Look up a physical property of a material from the \
                            materials database. Use for any material property question.",
            "parameters": {
                "type": "object",
                "properties": {
                    "material": {"type": "string", "description": "Material name"},
                    "property": {"type": "string", "description": "Property name"}
                },
                "required": ["material", "property"]
            }
        }
    }])
}

#[tokio::test]
#[ignore = "live: spends real credits and needs `prism login`"]
async fn marc27_stream_accepts_tools_array_and_returns_native_tool_calls() {
    let Some((api_base, token, project)) = creds() else {
        panic!("no ~/.prism/credentials.json — run `prism login` first");
    };
    let model =
        std::env::var("PRISM_LIVE_MODEL").unwrap_or_else(|_| "openai/gpt-oss-120b".to_string());

    let url = format!("{api_base}/projects/{project}/llm/stream");
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "You are a materials assistant. Use tools."},
            {"role": "user", "content": "What is the melting point of titanium?"}
        ],
        "max_tokens": 256,
        "tools": probe_tools(),
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("request failed");

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    println!("--- model={model} status={status} ---\n{text}\n--- end ---");
    assert!(status.is_success(), "HTTP {status}: {text}");

    // The platform's StreamChunk serializes as
    // {"delta": "...", "done": bool, "usage": {..}, "tool_calls": [..]}
    // with `tool_calls` carrying OpenAI-style deltas verbatim.
    let mut names = Vec::new();
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
            continue;
        };
        for tc in chunk
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(n) = tc.pointer("/function/name").and_then(|n| n.as_str())
                && !n.is_empty()
            {
                names.push(n.to_string());
            }
        }
    }
    assert!(
        names.contains(&"lookup_property".to_string()),
        "no native tool_call for `lookup_property` in the stream — \
         the endpoint did NOT honour the tools array for model {model}"
    );
}

/// The self-heal, end-to-end through `LlmClient`, against a model the platform
/// routes to its DIRECT Anthropic provider — which rejects OpenAI-shaped tool
/// schemas outright (`tools.0: Input tag 'function' …`). The turn must still
/// produce a tool call, via the text protocol.
#[tokio::test]
#[ignore = "live: spends real credits and needs `prism login`"]
async fn marc27_turn_recovers_when_the_provider_rejects_tool_schemas() {
    let Some((api_base, token, project)) = creds() else {
        panic!("no ~/.prism/credentials.json — run `prism login` first");
    };
    let model = std::env::var("PRISM_LIVE_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string());

    let client = prism_llm::LlmClient::new(prism_llm::LlmConfig {
        base_url: format!("{api_base}/projects/{project}/llm"),
        model,
        api_key: Some(token),
        context_window: Some(200_000),
        max_output_tokens: Some(1024),
        ..Default::default()
    });
    let tools = vec![prism_llm::ToolDefinition {
        tool_type: "function".to_string(),
        function: prism_llm::FunctionDef {
            name: "lookup_property".to_string(),
            description: "Look up a physical property of a material.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "material": {"type": "string"},
                    "property": {"type": "string"}
                },
                "required": ["material", "property"]
            }),
        },
    }];
    let messages = vec![prism_llm::ChatMessage {
        role: "user".to_string(),
        content: Some("What is the melting point of titanium?".to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];

    let resp = client
        .chat_with_tools_streaming(&messages, &tools, |_, _| {})
        .await
        .expect("the turn must recover, not fail");
    println!(
        "content={:?} calls={:?}",
        resp.message.content, resp.message.tool_calls
    );
    let calls = resp
        .message
        .tool_calls
        .expect("no tool call after the schema-rejection fallback");
    assert_eq!(calls[0].function.name, "lookup_property");
}
