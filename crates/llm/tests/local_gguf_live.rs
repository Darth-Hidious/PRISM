// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

#![cfg(feature = "local-inference")]

#[cfg(unix)]
use prism_llm::{BUNDLED_GEMMA, LocalModelIdentityOutcome};
use prism_llm::{ChatMessage, FunctionDef, LOCAL_GGUF_URL, LlmClient, LlmConfig, ToolDefinition};

fn meta_tool(name: &str, description: &str, parameters: serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDef {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}

/// The seven production meta-tool names and executable argument schemas from
/// `prism_agent::meta_tools::definitions` plus `prism_agent::subagent::definition`.
/// Kept local so this `prism-llm` integration test does not couple the LLM crate
/// to the agent crate.
fn production_meta_tools() -> Vec<ToolDefinition> {
    vec![
        meta_tool(
            "recall",
            "Retrieve earlier tool results from durable memory.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                }
            }),
        ),
        meta_tool(
            "find_tools",
            "Search the full tool catalog for capabilities relevant to a task.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 25}
                },
                "required": ["query"]
            }),
        ),
        meta_tool(
            "write_skill",
            "Author a reusable shell or Python skill and verify it once.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "language": {"type": "string", "enum": ["shell", "python"]},
                    "code": {"type": "string"}
                },
                "required": ["name", "description", "code"]
            }),
        ),
        meta_tool(
            "run_skill",
            "Execute a previously authored skill by name.",
            serde_json::json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
        ),
        meta_tool(
            "list_skills",
            "List the authored reusable skills.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        meta_tool(
            "spawn_subagent",
            "Delegate a self-contained task to a nested subagent turn.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string"},
                    "model": {"type": "string"},
                    "max_tokens": {"type": "integer"}
                },
                "required": ["task"]
            }),
        ),
        meta_tool(
            "list_failures",
            "List failed tool runs from durable memory.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {"type": "string"},
                    "limit": {"type": "integer"}
                }
            }),
        ),
    ]
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
        reasoning_content: None,
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

/// End-to-end proof for the pinned, manifest-only "bundled Gemma" contract:
/// exact artifact, exact embedded template, canonical Minja render, and greedy
/// generation all use one warm model. It never downloads weights.
#[tokio::test]
#[cfg(unix)]
#[ignore = "requires PRISM_TEST_GGUF to name the pinned Gemma artifact"]
async fn pinned_gemma_manifest_template_and_generation_proof() {
    let path =
        std::path::PathBuf::from(std::env::var("PRISM_TEST_GGUF").expect("set PRISM_TEST_GGUF"));
    let client = LlmClient::new(LlmConfig {
        base_url: LOCAL_GGUF_URL.to_string(),
        model: path.display().to_string(),
        max_output_tokens: Some(8),
        ..LlmConfig::default()
    });
    let messages = [ChatMessage {
        role: "user".to_string(),
        content: Some("Reply with one word: READY".to_string()),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];

    let identity = match client.local_model_identity().await.unwrap() {
        LocalModelIdentityOutcome::Verified { identity } => identity,
        LocalModelIdentityOutcome::Unavailable { code, detail } => {
            panic!("local model identity unexpectedly unavailable ({code:?}): {detail}")
        }
    };
    assert_eq!(identity.sha256, BUNDLED_GEMMA.sha256);
    assert_eq!(identity.size_bytes, BUNDLED_GEMMA.size_bytes);

    // Identity, rendering, and generation share the client's loaded model and
    // receipt, so the proof hashes and loads once through the production path.
    let rendered = client.render_local_prompt(&messages, &[]).await.unwrap();
    assert_eq!(
        rendered.template_sha256,
        "ae53464bf3be25802b3a5b37def7fd89667067d7577049b3b2d74c4d8de4c6d4"
    );
    assert!(rendered.text.starts_with("<bos><|turn>user\n"));
    assert!(rendered.token_count > 0);

    let response = client
        .chat_with_tools_streaming(&messages, &[], |_, is_reasoning| {
            assert!(!is_reasoning);
        })
        .await
        .unwrap();
    let usage = response.usage.expect("local generation must report usage");
    assert_eq!(usage.prompt_tokens, rendered.token_count);
    assert!(usage.completion_tokens > 0);
}

/// Live regression H3: tool call -> tool result -> second tool call -> final
/// answer using exactly the seven production meta-tools. This makes no network
/// request: both tool results are deterministic local fixtures.
///
/// Regression C1: each hop's result is pushed under the id of its own call,
/// and the two call ids must differ — a constant `local_call_0` made the
/// results collide and corrupt the rendered history.
#[tokio::test]
#[ignore = "requires PRISM_TEST_GGUF to name a real local generation model"]
async fn embedded_gguf_multistep_turn_uses_all_production_meta_tools() {
    let model = std::env::var("PRISM_TEST_GGUF").expect("set PRISM_TEST_GGUF");
    let client = LlmClient::new(LlmConfig {
        base_url: LOCAL_GGUF_URL.to_string(),
        model,
        max_output_tokens: Some(128),
        ..LlmConfig::default()
    });
    let tools = production_meta_tools();
    let names = tools
        .iter()
        .map(|tool| tool.function.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "recall",
            "find_tools",
            "write_skill",
            "run_skill",
            "list_skills",
            "spawn_subagent",
            "list_failures",
        ]
    );
    println!("backend=gguf://local exact_meta_tools={names:?}");

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some(
            "Follow this exact sequence. First call find_tools with query exactly \"materials discovery\" and limit 1. After its result, call recall with query exactly \"materials discovery\". After the recall result, give a short final answer."
                .to_string(),
        ),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let first = client
        .chat_with_tools_streaming(&messages, &tools, |piece, is_reasoning| {
            println!("first_delta reasoning={is_reasoning} piece={piece:?}");
        })
        .await
        .expect("first local tool turn failed");
    println!(
        "first_response content={:?} tool_calls={:?} usage={:?}",
        first.message.content, first.message.tool_calls, first.usage
    );
    let first_call = first
        .message
        .tool_calls
        .clone()
        .expect("the local model did not produce the first tool call");
    assert_eq!(first_call.len(), 1);
    assert_eq!(first_call[0].function.name, "find_tools");
    assert!(
        first_call[0].id.starts_with("local_call_"),
        "unexpected local call id shape: {}",
        first_call[0].id
    );

    let mut follow_up = messages;
    follow_up.push(first.message);
    let first_result = "find_tools result: materials_search is available. Now call recall with query materials discovery.";
    println!("first_tool_result={first_result:?}");
    follow_up.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(first_result.to_string()),
        tool_calls: None,
        tool_call_id: Some(first_call[0].id.clone()),
        reasoning_content: None,
    });
    let mut second_streamed = String::new();
    let second = client
        .chat_with_tools_streaming(&follow_up, &tools, |piece, is_reasoning| {
            println!("second_delta reasoning={is_reasoning} piece={piece:?}");
            second_streamed.push_str(piece);
        })
        .await
        .expect("second local tool turn failed");
    println!(
        "second_response content={:?} tool_calls={:?} usage={:?}",
        second.message.content, second.message.tool_calls, second.usage
    );
    let second_call = second
        .message
        .tool_calls
        .clone()
        .expect("the local model did not produce the second tool call");
    assert_eq!(second_call.len(), 1);
    assert_eq!(second_call[0].function.name, "recall");
    assert_ne!(
        first_call[0].id, second_call[0].id,
        "two tool calls share one id; their results would collide in history"
    );
    println!(
        "call_attribution find_tools_call={} find_tools_result_for={} recall_call={} recall_result_for={}",
        first_call[0].id, first_call[0].id, second_call[0].id, second_call[0].id
    );
    assert!(
        second_streamed.is_empty(),
        "native tool protocol leaked into streamed text: {second_streamed:?}"
    );

    follow_up.push(second.message);
    let second_result = "recall result: materials discovery context was recovered.";
    println!("second_tool_result={second_result:?}");
    follow_up.push(ChatMessage {
        role: "tool".to_string(),
        content: Some(second_result.to_string()),
        tool_calls: None,
        tool_call_id: Some(second_call[0].id.clone()),
        reasoning_content: None,
    });
    let final_response = client
        .chat_with_tools_streaming(&follow_up, &tools, |piece, is_reasoning| {
            println!("final_delta reasoning={is_reasoning} piece={piece:?}");
        })
        .await
        .expect("final local tool turn failed");
    println!(
        "final_response content={:?} tool_calls={:?} usage={:?}",
        final_response.message.content, final_response.message.tool_calls, final_response.usage
    );
    assert!(final_response.message.tool_calls.is_none());
    assert!(final_response.message.content.is_some());
}
