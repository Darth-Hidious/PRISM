// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.

#![cfg(feature = "local-inference")]

use prism_llm::{ChatMessage, LOCAL_GGUF_URL, LlmClient, LlmConfig};

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
