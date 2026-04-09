//! LLM client — OpenAI-compatible + MARC27 platform proxy.
//!
//! Wire formats:
//! - OpenAI: `/v1/chat/completions`, `/v1/embeddings`
//! - MARC27: `/stream` (SSE), text-based tool calling
//!
//! Works with: llama.cpp, Ollama, vLLM, LiteLLM, OpenAI, Anthropic,
//! MARC27 platform, and any OpenAI-compatible endpoint.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

// ── Configuration ────────────────────────────────────────────────────

/// Configuration for connecting to an LLM backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Base URL of the LLM API.
    pub base_url: String,
    /// Model name (e.g. "gemma-3-27b", "gpt-4o", "claude-sonnet-4-6").
    pub model: String,
    /// API key for authenticated providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Separate embedding model. If not set, uses `model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model: Option<String>,
    /// Maximum sample rows for extraction prompts.
    #[serde(default = "default_max_sample_rows")]
    pub max_sample_rows: usize,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_max_sample_rows() -> usize {
    10
}
fn default_timeout_secs() -> u64 {
    120
}

impl Default for LlmConfig {
    fn default() -> Self {
        // These are fallback defaults only — real values come from prism.toml
        // or server config on login. Don't hardcode provider-specific values here.
        Self {
            base_url: String::new(), // Must be set from config
            model: String::new(),    // Must be set from config or server default
            api_key: None,
            embedding_model: None,
            max_sample_rows: 10,
            timeout_secs: 120,
        }
    }
}

// ── Client ───────────────────────────────────────────────────────────

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
        // MARC27 platform: use /stream with SSE.
        // The platform proxy doesn't support OpenAI-style tool_calls in the response,
        // so we inject tool definitions into the messages and parse structured tool
        // calls from the response text.
        if self.is_marc27() {
            let url = format!("{}/stream", self.config.base_url);

            // Inject tool definitions as a system message so the LLM knows what's available
            let mut aug_messages: Vec<serde_json::Value> = serde_json::to_value(messages)?
                .as_array()
                .cloned()
                .unwrap_or_default();

            if !tools.is_empty() {
                let tool_block = build_tool_prompt_block(tools);
                // Append after the system prompt as a system message
                let inject_idx = if aug_messages
                    .first()
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("system")
                {
                    1
                } else {
                    0
                };
                aug_messages.insert(
                    inject_idx,
                    serde_json::json!({
                        "role": "system",
                        "content": tool_block,
                    }),
                );
            }

            // Convert OpenAI-format messages to what MARC27 accepts.
            // MARC27 only understands system/user/assistant with string content.
            for msg in &mut aug_messages {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                if role == "tool" {
                    // Convert tool results to user messages
                    let tool_id = msg
                        .get("tool_call_id")
                        .and_then(|t| t.as_str())
                        .unwrap_or("tool");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    *msg = serde_json::json!({
                        "role": "user",
                        "content": format!("[Tool result from {tool_id}]\n{content}"),
                    });
                } else if role == "assistant" {
                    // Strip tool_calls and ensure content is a string (not null)
                    if let Some(obj) = msg.as_object_mut() {
                        obj.remove("tool_calls");
                        obj.remove("tool_call_id");
                        // Ensure content is always a string
                        if obj.get("content").is_none()
                            || obj.get("content") == Some(&serde_json::Value::Null)
                        {
                            obj.insert(
                                "content".to_string(),
                                serde_json::Value::String(String::new()),
                            );
                        }
                    }
                }
            }

            let body = serde_json::json!({
                "model": self.config.model,
                "messages": aug_messages,
            });
            // Use a direct request (not the retry-wrapper post()) so we control headers
            let mut req = self
                .client
                .post(&url)
                .json(&body)
                .header("Accept", "text/event-stream");
            if let Some(auth) = self.auth_header() {
                req = req.header("Authorization", auth);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("MARC27 stream request to {url} failed"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                bail!("MARC27 LLM returned HTTP {status}: {text}");
            }
            debug!("MARC27 stream response received, reading chunks...");

            // Read SSE stream incrementally — don't use resp.text() which
            // blocks until the connection closes (SSE keeps it open).
            use futures_util::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut sse_buf = String::new();
            let mut full_text = String::new();
            let mut usage_info = None;
            let mut hit_tool_call = false;
            let mut done = false;

            while let Some(chunk) = stream.next().await {
                let bytes = chunk.context("error reading SSE chunk")?;
                sse_buf.push_str(&String::from_utf8_lossy(&bytes));

                // Process complete lines from the buffer
                while let Some(nl) = sse_buf.find('\n') {
                    let line = sse_buf[..nl].trim().to_string();
                    sse_buf = sse_buf[nl + 1..].to_string();

                    let line = line.strip_prefix("data: ").unwrap_or(&line).trim();
                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                            if !delta.is_empty() {
                                full_text.push_str(delta);

                                if !hit_tool_call {
                                    if full_text.contains("```tool_call")
                                        || full_text.contains("<tool_call>")
                                    {
                                        hit_tool_call = true;
                                    } else {
                                        on_delta(delta);
                                    }
                                }
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
                        if chunk.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                            done = true;
                            break;
                        }
                    }
                }
                if done {
                    break;
                }
            }

            // Parse tool calls — only take the FIRST batch (before any "Results:" hallucination)
            let tool_calls = parse_text_tool_calls(&full_text);
            // Only unique tool calls (LLM sometimes duplicates)
            let tool_calls = dedup_tool_calls(tool_calls);
            let mut content_text = strip_tool_call_blocks(&full_text);

            // If we found tool calls, suppress any JSON/code artifacts in content.
            // Gemini often leaks partial tool call JSON or closing ``` into the
            // content when it outputs a tool call. Only keep content that looks
            // like actual natural language prose.
            if !tool_calls.is_empty() && !content_text.is_empty() {
                let trimmed = content_text.trim();
                let looks_like_json = trimmed.contains("}}")
                    || trimmed.contains("\"name\":")
                    || trimmed.contains("\"arguments\":")
                    || trimmed.starts_with('{')
                    || trimmed.starts_with('"')
                    || trimmed.starts_with("```")
                    || trimmed.ends_with("```");
                if looks_like_json {
                    content_text.clear();
                }
            }

            return Ok(ChatResponse {
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: if content_text.is_empty() {
                        None
                    } else {
                        Some(content_text)
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
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

// ── MARC27 text-based tool calling helpers ──────────────────────────

/// Build a lightweight tool catalog for the system prompt.
///
/// Instead of dumping all 108 tool definitions (11K+ tokens), we give the model:
/// 1. A categorized summary of what's available
/// 2. Instructions to call `discover_capabilities` for specifics
/// 3. The tool calling syntax
///
/// Full tool definitions are injected only after discover_capabilities returns.
fn build_tool_prompt_block(tools: &[ToolDefinition]) -> String {
    // Categorize tools by prefix/name patterns
    let mut categories: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for tool in tools {
        let name = tool.function.name.as_str();
        let cat = if name.starts_with("knowledge_")
            || name.starts_with("semantic_")
            || name == "list_corpora"
        {
            "Knowledge Graph"
        } else if name.starts_with("search_") || name.starts_with("query_") {
            "Search & Query"
        } else if name.starts_with("predict_")
            || name.starts_with("list_models")
            || name.starts_with("list_predictable")
        {
            "ML Prediction"
        } else if name.starts_with("compute_")
            || name.starts_with("run")
            || name.starts_with("job")
            || name.starts_with("deploy")
        {
            "Compute & Deploy"
        } else if name.starts_with("mesh_") || name.starts_with("node_") {
            "Mesh & Nodes"
        } else if name.starts_with("workflow") || name.starts_with("forge") {
            "Workflows"
        } else if name.starts_with("marketplace") {
            "Marketplace"
        } else if name.starts_with("ingest")
            || name.starts_with("import")
            || name.starts_with("export")
        {
            "Data & Ingest"
        } else if name.starts_with("execute_")
            || name.starts_with("read_")
            || name.starts_with("write_")
            || name.starts_with("edit_")
        {
            "Code & Files"
        } else if name.starts_with("plot_") || name.starts_with("visualize") {
            "Visualization"
        } else if name.starts_with("literature_")
            || name.starts_with("patent_")
            || name.starts_with("web_")
        {
            "Literature & Web"
        } else if name.starts_with("discourse") || name.starts_with("research") {
            "Research & Discourse"
        } else {
            "Other"
        };
        categories.entry(cat).or_default().push(name);
    }

    let mut block = format!(
        "# Tool Calling\n\n\
         You have {} tools available across these categories:\n\n",
        tools.len()
    );

    for (category, tool_names) in &categories {
        block.push_str(&format!(
            "- **{}** ({} tools): {}\n",
            category,
            tool_names.len(),
            tool_names
                .iter()
                .take(4)
                .copied()
                .collect::<Vec<_>>()
                .join(", "),
        ));
        if tool_names.len() > 4 {
            block.push_str(&format!("  ... and {} more\n", tool_names.len() - 4));
        }
    }

    block.push_str("\n\
        ## IMPORTANT: When NOT to call tools\n\n\
        For greetings, casual conversation, explanations, general knowledge questions, \
        or anything that does not need live data — respond with plain text. \
        Do NOT call tools for simple chat like \"hello\", \"what can you do?\", or \"explain X\".\n\n\
        ## How to call tools\n\n\
        ONLY when a task explicitly requires data retrieval, computation, search, or platform interaction, call a tool:\n\n\
        ```tool_call\n\
        {\"name\": \"tool_name\", \"arguments\": {\"arg1\": \"value1\"}}\n\
        ```\n\n\
        **CRITICAL rules:**\n\
        - Call `discover_capabilities` first if you need to see what tools exist\n\
        - Output ONE ```tool_call block, then STOP IMMEDIATELY. Do not write anything after it.\n\
        - Do NOT output multiple tool_call blocks in one response.\n\
        - Do NOT guess, fabricate, or hallucinate tool results. EVER.\n\
        - After your ```tool_call block, the system executes it and returns the result.\n\
        - You will see the result in your next message, then you can respond or call another tool.\n\
        - If you need multiple tools, call them one at a time across multiple turns.\n\n\
        ## Quick reference (most common tools)\n\n\
        - `discover_capabilities` — see all available tools, providers, models, corpora\n\
        - `knowledge_search` — search the MARC27 knowledge graph (211K+ entities)\n\
        - `search_materials` — search 20+ materials databases (OPTIMADE)\n\
        - `semantic_search` — vector similarity search over embedded documents\n\
        - `predict_property` — predict material property from composition\n\
        - `execute_python` — run Python code for analysis\n\
        - `web_search` / `web_read` — search or read web pages\n\
        - `literature_search` — search arXiv, Semantic Scholar\n\
        - `research_query` — iterative research loop via MARC27 platform\n\
    ");

    block
}

/// Parse ```tool_call blocks from response text.
fn parse_text_tool_calls(text: &str) -> Vec<ToolCallResponse> {
    let mut calls = Vec::new();
    let mut call_idx = 0;

    // Format 1: ```tool_call JSON blocks (Claude, Gemini)
    {
        let mut rest = text;
        while let Some(start) = rest.find("```tool_call") {
            let after = &rest[start + 12..];
            let after = after.trim_start_matches(|c: char| c != '\n');
            let after = after.strip_prefix('\n').unwrap_or(after);

            if let Some(end) = after.find("```") {
                let json_str = after[..end].trim();
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let name = parsed
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = parsed
                        .get("arguments")
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "{}".to_string());

                    calls.push(ToolCallResponse {
                        id: format!("tc_{call_idx}"),
                        call_type: "function".to_string(),
                        function: FunctionCall { name, arguments },
                    });
                    call_idx += 1;
                }
                rest = &after[end + 3..];
            } else {
                break;
            }
        }
    }

    // Format 2: <tool_call><function=name><parameter=key>value</parameter></function></tool_call>
    // Used by Nvidia, Llama, and some open models
    if calls.is_empty() {
        let mut rest = text;
        while let Some(start) = rest.find("<tool_call>") {
            let after = &rest[start + 11..];
            if let Some(end) = after.find("</tool_call>") {
                let block = &after[..end];
                // Parse <function=NAME>
                if let Some(fn_start) = block.find("<function=") {
                    let fn_after = &block[fn_start + 10..];
                    let fn_name_end = fn_after.find('>').unwrap_or(fn_after.len());
                    let fn_name = fn_after[..fn_name_end].to_string();

                    // Parse all <parameter=KEY>VALUE</parameter>
                    let mut args = serde_json::Map::new();
                    let mut param_rest = fn_after;
                    while let Some(p_start) = param_rest.find("<parameter=") {
                        let p_after = &param_rest[p_start + 11..];
                        if let Some(p_name_end) = p_after.find('>') {
                            let p_name = p_after[..p_name_end].to_string();
                            let p_value_start = &p_after[p_name_end + 1..];
                            let p_value_end = p_value_start
                                .find("</parameter>")
                                .unwrap_or(p_value_start.len());
                            let p_value = p_value_start[..p_value_end].trim().to_string();
                            args.insert(p_name, serde_json::Value::String(p_value));
                            param_rest = &p_value_start[p_value_end..];
                        } else {
                            break;
                        }
                    }

                    calls.push(ToolCallResponse {
                        id: format!("tc_{call_idx}"),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: fn_name,
                            arguments: serde_json::Value::Object(args).to_string(),
                        },
                    });
                    call_idx += 1;
                }
                rest = &after[end + 12..];
            } else {
                break;
            }
        }
    }

    calls
}

/// Deduplicate tool calls by name+arguments (LLM sometimes repeats the same call).
fn dedup_tool_calls(calls: Vec<ToolCallResponse>) -> Vec<ToolCallResponse> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for call in calls {
        let key = format!("{}:{}", call.function.name, call.function.arguments);
        if seen.insert(key) {
            result.push(call);
        }
    }
    result
}

/// Strip everything from the first ```tool_call block onwards.
/// The LLM outputs preamble text, then tool calls, then hallucinated results.
/// We only keep the preamble — tool results come from actual execution.
fn strip_tool_call_blocks(text: &str) -> String {
    // Truncate at first tool_call — everything after is hallucination
    // Handle both ```tool_call (Claude/Gemini) and <tool_call> (Nvidia/Llama)
    let fenced = text.find("```tool_call");
    let xml = text.find("<tool_call>");
    let first = match (fenced, xml) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    if let Some(start) = first {
        return text[..start].trim().to_string();
    }

    // No tool calls — return as-is (dead code path kept for safety)
    let mut result = String::new();
    let mut rest = text;

    while let Some(start) = rest.find("```tool_call") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 12..];
        if let Some(end) = after.find("```") {
            rest = &after[end + 3..];
        } else {
            break;
        }
    }
    result.push_str(rest);
    result.trim().to_string()
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
