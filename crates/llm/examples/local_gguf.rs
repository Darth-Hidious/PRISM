// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Non-interactive embedded-inference smoke: prints each decoded token piece.

use std::io::Write;

use anyhow::{Context, Result};
use prism_llm::{ChatMessage, LOCAL_GGUF_URL, LlmClient, LlmConfig};

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args
        .next()
        .context("usage: local_gguf <path-or-model-name> <prompt>")?;
    let prompt = args.collect::<Vec<_>>().join(" ");
    if prompt.is_empty() {
        anyhow::bail!("usage: local_gguf <path-or-model-name> <prompt>");
    }

    let client = LlmClient::new(LlmConfig {
        base_url: LOCAL_GGUF_URL.to_string(),
        model,
        max_output_tokens: Some(64),
        ..LlmConfig::default()
    });
    println!("backend={LOCAL_GGUF_URL}");
    println!("context_window={:?}", client.config().context_window);
    println!("prompt={prompt:?}");

    let messages = [ChatMessage {
        role: "user".to_string(),
        content: Some(prompt),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let mut token_index = 0_usize;
    let response = client
        .chat_with_tools_streaming(&messages, &[], |piece, is_reasoning| {
            token_index += 1;
            println!("token[{token_index}] reasoning={is_reasoning} piece={piece:?}");
            let _ = std::io::stdout().flush();
        })
        .await?;
    println!("response={:?}", response.message.content);
    println!("usage={:?}", response.usage);
    Ok(())
}
