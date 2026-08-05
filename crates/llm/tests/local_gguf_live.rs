// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

#![cfg(feature = "local-inference")]

use prism_llm::{ChatMessage, FunctionDef, LOCAL_GGUF_URL, LlmClient, LlmConfig, ToolDefinition};

fn lookup_tool() -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDef {
            name: "lookup_property".to_string(),
            description: "Look up a material property in the local materials database.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "material": {"type": "string"},
                    "property": {"type": "string"}
                },
                "required": ["material", "property"],
                "additionalProperties": false
            }),
        },
    }
}

#[tokio::test]
#[ignore = "requires PRISM_TEST_GGUF to name a real local generation model"]
async fn embedded_gguf_streams_real_token_pieces() {
    let model = std::env::var("PRISM_TEST_GGUF").expect("set PRISM_TEST_GGUF");
    let client = LlmClient::new(LlmConfig {
        base_url: LOCAL_GGUF_URL.to_string(),
        model,
        max_output_tokens: Some(16),
        ..LlmConfig::default()
    });
    let messages = [ChatMessage {
        role: "user".to_string(),
        content: Some("Reply with exactly one short greeting.".to_string()),
        tool_calls: None,
        tool_call_id: None,
    }];
    let mut streamed = String::new();
    let response = client
        .chat_with_tools_streaming(&messages, &[], |piece, is_reasoning| {
            assert!(!is_reasoning);
            streamed.push_str(piece);
        })
        .await
        .unwrap();

    assert!(
        !streamed.is_empty(),
        "the real model emitted no token pieces"
    );
    assert_eq!(response.message.content.as_deref(), Some(streamed.as_str()));
    assert!(response.usage.unwrap().completion_tokens > 0);
}

/// End-to-end local tool turn: the tool list reaches embedded generation,
/// the strict JSON response becomes the shared OpenAI-shaped ToolCall, and a
/// subsequent turn can consume the tool result without any HTTP adapter.
fn decoy_tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDef {
            name: name.to_string(),
            description: format!("An unrelated function named {name}."),
            parameters: serde_json::json!({"type": "object"}),
        },
    }
}

#[tokio::test]
#[ignore = "requires PRISM_TEST_GGUF to name a real local generation model"]
async fn embedded_gguf_tool_turn_calls_and_receives_a_result() {
    let model = std::env::var("PRISM_TEST_GGUF").expect("set PRISM_TEST_GGUF");
    let client = LlmClient::new(LlmConfig {
        base_url: LOCAL_GGUF_URL.to_string(),
        model,
        max_output_tokens: Some(128),
        ..LlmConfig::default()
    });
    let mut tools = vec![lookup_tool()];
    for name in [
        "recall",
        "find_tools",
        "write_skill",
        "run_skill",
        "list_skills",
        "spawn_subagent",
    ] {
        tools.push(decoy_tool(name));
    }
    println!("backend=gguf://local tools_len={}", tools.len());

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(
            "Call lookup_property now with material exactly titanium and property exactly melting_point."
                .to_string(),
        ),
        tool_calls: None,
        tool_call_id: None,
    }];
    let first = client
        .chat_with_tools_streaming(&messages, &tools, |piece, is_reasoning| {
            println!("first_delta reasoning={is_reasoning} piece={piece:?}");
        })
        .await
        .expect("local tool turn failed");
    println!(
        "first_response content={:?} tool_calls={:?} usage={:?}",
        first.message.content, first.message.tool_calls, first.usage
    );
    let calls = first
        .message
        .tool_calls
        .clone()
        .expect("the local model did not produce a tool call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "lookup_property");
    let arguments: serde_json::Value =
        serde_json::from_str(&calls[0].function.arguments).expect("strict JSON arguments");
    assert!(arguments["material"].is_string());
    assert!(arguments["property"].is_string());

    let mut follow_up = messages;
    follow_up.push(first.message);
    let tool_result = "Titanium melts at 1668 degrees Celsius.";
    println!("tool_result={tool_result:?}");
    follow_up.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(tool_result.to_string()),
        tool_calls: None,
        tool_call_id: Some(calls[0].id.clone()),
    });
    let second = client
        .chat_with_tools_streaming(&follow_up, &tools, |piece, is_reasoning| {
            println!("second_delta reasoning={is_reasoning} piece={piece:?}");
        })
        .await
        .expect("local tool result turn failed");
    println!(
        "second_response content={:?} tool_calls={:?} usage={:?}",
        second.message.content, second.message.tool_calls, second.usage
    );
    assert!(second.message.tool_calls.is_none());
    assert!(second.message.content.is_some());
}
