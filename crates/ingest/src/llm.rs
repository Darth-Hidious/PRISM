//! LLM client — OpenAI-compatible API for all backends.
//!
//! Single wire format: `/v1/chat/completions`, `/v1/embeddings`.
//! Works with: llama.cpp server, Ollama, vLLM, LiteLLM, OpenAI, Anthropic
//! (via proxy), MARC27 platform, and any OpenAI-compatible endpoint.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

use crate::LlmConfig;

/// A message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallResponse>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Unified LLM client — all backends via OpenAI-compatible API.
///
/// Works with:
/// - **llama.cpp** (`llama-server --port 8080`) — local inference
/// - **Ollama** (`http://localhost:11434/v1/`) — local inference
/// - **vLLM** — local or remote inference
/// - **MARC27 platform** — managed cloud inference
/// - **OpenAI** — cloud inference
/// - **Anthropic** (via OpenAI proxy) — cloud inference
/// - **LiteLLM** — proxy to any provider
pub struct LlmClient {
    client: reqwest::Client,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .expect("failed to build HTTP client");
        Self { client, config }
    }

    /// Generate text from a prompt.
    pub async fn generate(&self, prompt: &str) -> Result<String> {
        self.chat("You are a helpful assistant.", prompt).await
    }

    /// Whether this client targets the MARC27 platform LLM proxy
    /// (which uses `/stream` + SSE instead of OpenAI `/v1/chat/completions`).
    fn is_marc27(&self) -> bool {
        self.config.base_url.contains("marc27.com") || self.config.base_url.contains("/llm")
    }

    /// Generate text with a system + user message.
    pub async fn chat(&self, system: &str, user: &str) -> Result<String> {
        let messages = serde_json::json!([
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ]);
        if self.is_marc27() {
            return self.chat_marc27_simple(&messages).await;
        }
        let url = format!("{}/v1/chat/completions", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "max_tokens": 4096,
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;
        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// MARC27 platform LLM: POST /stream with SSE response.
    async fn chat_marc27_simple(&self, messages: &serde_json::Value) -> Result<String> {
        let url = format!("{}/stream", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
        });
        let resp = self.post(&url, &body).await?;
        let text = resp.text().await.context("failed to read MARC27 stream")?;
        let mut result = String::new();
        for line in text.lines() {
            let line = line.strip_prefix("data: ").unwrap_or(line).trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                    result.push_str(delta);
                }
                if chunk.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                    break;
                }
            }
        }
        if result.is_empty() {
            bail!("MARC27 LLM returned empty response");
        }
        Ok(result)
    }

    /// Chat with tool-calling support.
    /// Sends full message history + tool definitions, returns response
    /// which may contain tool_calls.
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatResponse> {
        // MARC27 platform proxy: use /stream, collect text (no tool-calling support yet)
        if self.is_marc27() {
            let msgs = serde_json::to_value(messages)?;
            let text = self.chat_marc27_simple(&msgs).await?;
            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                },
                usage: None,
            });
        }
        let url = format!("{}/v1/chat/completions", self.config.base_url);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "max_tokens": 4096,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;

        let choice = &data["choices"][0];
        let msg_val = &choice["message"];

        let tool_calls: Option<Vec<ToolCallResponse>> = msg_val
            .get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok());

        let content = msg_val["content"].as_str().map(|s| s.to_string());

        let usage = data
            .get("usage")
            .and_then(|u| serde_json::from_value::<UsageInfo>(u.clone()).ok());

        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".to_string(),
                content,
                tool_calls,
                tool_call_id: None,
            },
            usage,
        })
    }

    /// Generate text and parse as JSON (uses response_format).
    pub async fn generate_json(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.1,
            "max_tokens": 4096,
            "response_format": {"type": "json_object"},
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;
        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Embed a single text string. Returns the embedding vector.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let vecs = self.embed(vec![text.to_string()]).await?;
        vecs.into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("LLM returned no embedding"))
    }

    /// Batch embedding. Returns one vector per input text.
    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/v1/embeddings", self.config.base_url);
        let body = serde_json::json!({
            "model": self.embed_model(),
            "input": texts,
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad embedding response")?;
        let arr = data["data"]
            .as_array()
            .context("expected data array in embeddings response")?;
        let mut embeddings = Vec::with_capacity(arr.len());
        for item in arr {
            let vec: Vec<f32> = serde_json::from_value(item["embedding"].clone())
                .context("bad embedding vector")?;
            embeddings.push(vec);
        }
        Ok(embeddings)
    }

    /// Chat with tool-calling support and SSE streaming.
    /// Calls `on_delta` for each text chunk as it arrives.
    /// Returns the final assembled response (same as `chat_with_tools`).
    pub async fn chat_with_tools_streaming(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        mut on_delta: impl FnMut(&str),
    ) -> Result<ChatResponse> {
        // MARC27 platform: use /stream with SSE
        if self.is_marc27() {
            let url = format!("{}/stream", self.config.base_url);
            let body = serde_json::json!({
                "model": self.config.model,
                "messages": messages,
            });
            let resp = self.post(&url, &body).await?;
            let text = resp.text().await.context("failed to read MARC27 stream")?;
            let mut full_text = String::new();
            let mut usage_info = None;
            for line in text.lines() {
                let line = line.strip_prefix("data: ").unwrap_or(line).trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            on_delta(delta);
                            full_text.push_str(delta);
                        }
                    }
                    if let Some(u) = chunk.get("usage") {
                        let pt = u
                            .get("prompt_tokens")
                            .or_else(|| u.get("input_tokens"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let ct = u
                            .get("completion_tokens")
                            .or_else(|| u.get("output_tokens"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        if pt > 0 || ct > 0 {
                            usage_info = Some(UsageInfo {
                                prompt_tokens: pt,
                                completion_tokens: ct,
                                total_tokens: pt + ct,
                            });
                        }
                    }
                }
            }
            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: if full_text.is_empty() {
                        None
                    } else {
                        Some(full_text)
                    },
                    tool_calls: None,
                    tool_call_id: None,
                },
                usage: usage_info,
            });
        }
        let url = format!("{}/v1/chat/completions", self.config.base_url);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "max_tokens": 4096,
            "stream": true,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        let mut req = self.client.post(&url).json(&body);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("LLM streaming request to {url} failed"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("LLM returned HTTP {status}: {text}");
        }

        // Parse SSE stream
        let mut full_content = String::new();
        let mut tool_calls_map: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new(); // index -> (id, name, args)
        let mut usage_info: Option<UsageInfo> = None;

        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut sse_buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("error reading SSE chunk")?;
            sse_buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete SSE lines from the buffer
            while let Some(newline_pos) = sse_buffer.find('\n') {
                let line = sse_buffer[..newline_pos].trim().to_string();
                sse_buffer = sse_buffer[newline_pos + 1..].to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    continue;
                }
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
                        // Extract text delta
                        if let Some(delta) = chunk
                            .pointer("/choices/0/delta/content")
                            .and_then(|c| c.as_str())
                        {
                            if !delta.is_empty() {
                                on_delta(delta);
                                full_content.push_str(delta);
                            }
                        }

                        // Extract streaming tool calls
                        if let Some(tcs) = chunk
                            .pointer("/choices/0/delta/tool_calls")
                            .and_then(|t| t.as_array())
                        {
                            for tc in tcs {
                                let idx =
                                    tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                                let entry = tool_calls_map.entry(idx).or_insert_with(|| {
                                    let id = tc
                                        .get("id")
                                        .and_then(|i| i.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let name = tc
                                        .pointer("/function/name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    (id, name, String::new())
                                });
                                // Append argument chunks
                                if let Some(args_chunk) =
                                    tc.pointer("/function/arguments").and_then(|a| a.as_str())
                                {
                                    entry.2.push_str(args_chunk);
                                }
                            }
                        }

                        // Extract usage from final chunk
                        if let Some(u) = chunk.get("usage") {
                            usage_info = serde_json::from_value::<UsageInfo>(u.clone()).ok();
                        }
                    }
                }
            }
        }

        // Assemble tool calls
        let tool_calls = if tool_calls_map.is_empty() {
            None
        } else {
            let mut calls: Vec<(u32, ToolCallResponse)> = tool_calls_map
                .into_iter()
                .map(|(idx, (id, name, args))| {
                    (
                        idx,
                        ToolCallResponse {
                            id,
                            call_type: "function".to_string(),
                            function: FunctionCall {
                                name,
                                arguments: args,
                            },
                        },
                    )
                })
                .collect();
            calls.sort_by_key(|(idx, _)| *idx);
            Some(calls.into_iter().map(|(_, tc)| tc).collect())
        };

        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: if full_content.is_empty() {
                    None
                } else {
                    Some(full_content)
                },
                tool_calls,
                tool_call_id: None,
            },
            usage: usage_info,
        })
    }

    /// Health check — verify the LLM backend is reachable.
    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/v1/models", self.config.base_url);
        let mut req = self.client.get(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await.context("LLM not reachable")?;
        if !resp.status().is_success() {
            bail!("LLM health check returned {}", resp.status());
        }
        Ok(())
    }

    // ── Internal ──────────────────────────────────────────────────

    fn embed_model(&self) -> &str {
        self.config
            .embedding_model
            .as_deref()
            .unwrap_or(&self.config.model)
    }

    fn auth_header(&self) -> Option<String> {
        self.config
            .api_key
            .as_ref()
            .filter(|k| !k.is_empty())
            .map(|k| format!("Bearer {k}"))
    }

    async fn post(&self, url: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        debug!(%url, "LLM request");
        for attempt in 0..3u32 {
            let mut req = self.client.post(url).json(body);
            if let Some(auth) = self.auth_header() {
                req = req.header("Authorization", auth);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("LLM request to {url} failed"))?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(2u64.pow(attempt));
                debug!(attempt, wait_secs = wait, "429 — retrying after backoff");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                bail!("LLM returned HTTP {status}: {text}");
            }
            return Ok(resp);
        }
        bail!("LLM request to {url} failed after 3 retries (429 rate limit)");
    }

    // Keep old signature for callers that held the single-attempt path
    #[allow(dead_code)]
    async fn post_no_retry(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        debug!(%url, "LLM request (no retry)");
        let mut req = self.client.post(url).json(body);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("LLM request to {url} failed"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("LLM returned HTTP {status}: {text}");
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_client_constructs_with_defaults() {
        let config = LlmConfig::default();
        let _client = LlmClient::new(config);
    }

    #[test]
    fn auth_header_present_when_key_set() {
        let config = LlmConfig {
            api_key: Some("sk-test123".into()),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        assert_eq!(client.auth_header(), Some("Bearer sk-test123".to_string()));
    }

    #[test]
    fn auth_header_none_when_no_key() {
        let config = LlmConfig::default();
        let client = LlmClient::new(config);
        assert!(client.auth_header().is_none());
    }
}
