// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! LLM client — OpenAI-compatible + MARC27 platform proxy.
//!
//! Wire formats:
//! - OpenAI: `/v1/chat/completions`, `/v1/embeddings`
//! - MARC27: `/stream` (SSE), text-based tool calling
//!
//! Works with: llama.cpp, Ollama, vLLM, LiteLLM, OpenAI, Anthropic,
//! MARC27 platform, and any OpenAI-compatible endpoint.

use anyhow::{Context, Result, bail};
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
    /// The model's context window in tokens, from the platform catalog.
    /// `None` = unknown (e.g. local llama.cpp) — consumers must fall back
    /// to conservative behavior, never assume a size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// The model's max output tokens, from the platform catalog. Used to
    /// reserve room for the response when budgeting input context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

fn default_max_sample_rows() -> usize {
    10
}
fn default_timeout_secs() -> u64 {
    300
}

/// Tokens kept free between the estimated prompt and the context window, so a
/// requested `max_tokens` can never overrun the input. Feeds the client-side
/// output clamp ([`LlmClient::effective_max_tokens`]).
const CONTEXT_MARGIN_TOKENS: u64 = 1024;

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
            timeout_secs: 300,
            context_window: None,
            max_output_tokens: None,
        }
    }
}

// ── API-key hydration ────────────────────────────────────────────────

/// Hydrate provider API keys from `~/.prism/api_keys.json` (written by the
/// TUI's API-key window, 0600) into the process environment.
///
/// Env vars that are ALREADY set win — the file is a fallback, never an
/// override. Call this at process start (CLI) and again before switching
/// providers (backend), so a key saved mid-session takes effect without a
/// restart. Never fails: a missing or malformed file is a no-op.
pub fn hydrate_env_from_api_keys() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let path = std::path::PathBuf::from(home)
        .join(".prism")
        .join("api_keys.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw) else {
        return;
    };
    hydrate_env_from_map(&map);
    // Bridge the Google naming split: the TUI saves GOOGLE_API_KEY, while
    // some consumers default to GEMINI_API_KEY. Whichever exists serves both.
    for (have, want) in [
        ("GOOGLE_API_KEY", "GEMINI_API_KEY"),
        ("GEMINI_API_KEY", "GOOGLE_API_KEY"),
    ] {
        if let Ok(v) = std::env::var(have)
            && !v.is_empty()
            && std::env::var_os(want).is_none()
        {
            // SAFETY: called at process start / in the backend's
            // single-threaded command loop, before any concurrent
            // env reads for these provider vars.
            unsafe { std::env::set_var(want, v) };
        }
    }
}

/// File-independent core of [`hydrate_env_from_api_keys`] (unit-testable).
fn hydrate_env_from_map(map: &serde_json::Map<String, serde_json::Value>) {
    for (name, value) in map {
        let Some(v) = value.as_str().filter(|v| !v.is_empty()) else {
            continue;
        };
        if std::env::var_os(name).is_none() {
            // SAFETY: see hydrate_env_from_api_keys — single-threaded
            // call sites only.
            unsafe { std::env::set_var(name, v) };
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
            .connect_timeout(Duration::from_secs(30))
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

    /// Build the OpenAI chat-completions URL. Handles base URLs that
    /// already include `/v1` (e.g. `http://localhost:8081/v1`) by not
    /// double-appending, and base URLs that don't (e.g.
    /// `http://localhost:8081`) by appending `/v1`.
    fn chat_completions_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }

    /// Extract the assistant's text from an OpenAI-compatible response.
    /// Falls back to `reasoning_content` when `content` is empty (e.g.
    /// Gemma 4 thinking mode puts all text in `reasoning_content`).
    fn extract_content(data: &serde_json::Value) -> String {
        let msg = &data["choices"][0]["message"];
        let content = msg["content"].as_str().unwrap_or_default();
        if !content.is_empty() {
            return content.to_string();
        }
        msg["reasoning_content"]
            .as_str()
            .unwrap_or_default()
            .to_string()
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
        let url = self.chat_completions_url();
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "max_tokens": self.effective_max_tokens(Self::estimate_tokens(&messages)),
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;
        Ok(Self::extract_content(&data))
    }

    /// MARC27 platform LLM: POST /stream with SSE response.
    async fn chat_marc27_simple(&self, messages: &serde_json::Value) -> Result<String> {
        let url = format!("{}/stream", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            // Was previously omitted entirely, letting the platform generate
            // unbounded output (thousands of tokens observed) on every call —
            // real, billed credits with no cap. Send the same context-clamped
            // budget every other chat path uses.
            "max_tokens": self.effective_max_tokens(Self::estimate_tokens(messages)),
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
        let url = self.chat_completions_url();

        let est = Self::estimate_tokens(&serde_json::to_value(messages).unwrap_or_default())
            + Self::estimate_tokens(&serde_json::to_value(tools).unwrap_or_default());
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "max_tokens": self.effective_max_tokens(est),
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

        let content_str = Self::extract_content(&data);
        let content = if content_str.is_empty() {
            None
        } else {
            Some(content_str)
        };

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

    /// Token budget shared by every chat/completion request this client
    /// sends. Honors `LlmConfig::max_output_tokens` (fetched from the
    /// platform model catalog) instead of a fixed guess — a reasoning/
    /// "thinking" model can burn its entire budget on `reasoning_content`
    /// before writing any JSON or visible text, and a caller may know the
    /// model needs more (or less) room than a hardcoded 4096. Falls back to
    /// 4096 only when the config doesn't carry a value (e.g. local llama.cpp
    /// with no catalog entry).
    fn effective_max_tokens(&self, est_prompt_tokens: u64) -> u64 {
        const FLOOR: u64 = 256;
        let model_max = self.config.max_output_tokens.unwrap_or(4096);
        // Clamp the requested output so it can never collide with the input:
        // context_window − estimated prompt − margin. When the context window is
        // unknown (local models), only the configured max applies. Unknown/local
        // models also have no catalog max_output_tokens, so they naturally floor
        // at 4096 — the same ceiling the compact profile's Capped(4096) would set,
        // which is why no per-profile cap needs plumbing across the crate boundary.
        let by_context = match self.config.context_window {
            Some(cw) => cw
                .saturating_sub(est_prompt_tokens)
                .saturating_sub(CONTEXT_MARGIN_TOKENS),
            None => model_max,
        };
        model_max.min(by_context).max(FLOOR)
    }

    /// Rough prompt-token estimate for a serialized request value (~4 chars per
    /// token). Only feeds the [`Self::effective_max_tokens`] safety clamp, so a
    /// slight under-count is harmless — the margin and the server-side clamp
    /// absorb the slack.
    fn estimate_tokens(value: &serde_json::Value) -> u64 {
        value.to_string().len() as u64 / 4
    }

    /// Extract strict JSON output from a chat-completions choice.
    ///
    /// Unlike [`Self::extract_content`] (used for human-readable chat, where
    /// falling back to `reasoning_content` is a reasonable best-effort),
    /// JSON extraction must NEVER return reasoning text: it is never valid
    /// JSON, so silently returning it only turns this into an opaque
    /// "invalid JSON" parse error one layer up. When `content` is empty we
    /// diagnose *why* and bail with an actionable message instead
    /// (AUDIT_BACKLOG #6 / INGESTION_AUDIT #6 — a thinking model hitting
    /// `finish_reason: "length"` while reasoning silently broke every
    /// extraction on that hardware).
    fn extract_json_content(choice: &serde_json::Value) -> Result<String> {
        let msg = &choice["message"];
        let content = msg["content"].as_str().unwrap_or_default();
        if !content.is_empty() {
            return Ok(content.to_string());
        }
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("unknown");
        let reasoning_len = msg["reasoning_content"].as_str().unwrap_or_default().len();
        if finish_reason == "length" && reasoning_len > 0 {
            bail!(
                "LLM produced no JSON output: it hit max_tokens before finishing, having \
                 spent the whole budget on {reasoning_len} chars of reasoning_content \
                 (finish_reason=length). This model is running in 'thinking' mode — raise \
                 max_output_tokens in the LLM config, or disable thinking for extraction \
                 requests if the backend supports it."
            );
        }
        bail!("LLM returned empty content for JSON extraction (finish_reason={finish_reason})");
    }

    /// Generate text and parse as JSON (uses response_format).
    pub async fn generate_json(&self, prompt: &str) -> Result<String> {
        // MARC27 platform: this method used to skip the is_marc27() branch
        // that chat()/chat_with_tools() have, so ingest ontology extraction
        // against a platform URL hit `{base}/v1/chat/completions` → 404
        // (the platform speaks `/stream`). The stream path has no
        // response_format, so fenced output is tolerated instead.
        if self.is_marc27() {
            let msgs = serde_json::json!([{ "role": "user", "content": prompt }]);
            let text = self.chat_marc27_simple(&msgs).await?;
            return Ok(Self::strip_json_fences(&text).to_string());
        }
        let url = self.chat_completions_url();
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.1,
            "max_tokens": self.effective_max_tokens(prompt.len() as u64 / 4),
            "response_format": {"type": "json_object"},
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad chat response")?;
        Self::extract_json_content(&data["choices"][0])
    }

    /// Strip a Markdown code fence (```json … ``` or ``` … ```) from around
    /// a JSON payload. Providers without a JSON response mode (the MARC27
    /// `/stream` path) often fence their JSON; the parser downstream wants
    /// the bare object. Text without a fence is returned unchanged.
    fn strip_json_fences(text: &str) -> &str {
        let trimmed = text.trim();
        let Some(rest) = trimmed.strip_prefix("```") else {
            return trimmed;
        };
        // Drop an optional language tag (e.g. `json`) up to the first newline.
        let body = match rest.split_once('\n') {
            Some((_lang, body)) => body,
            None => rest,
        };
        body.strip_suffix("```").map_or(body, str::trim).trim()
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
        let base = self.config.base_url.trim_end_matches('/');
        let url = if base.ends_with("/v1") {
            format!("{base}/embeddings")
        } else {
            format!("{base}/v1/embeddings")
        };
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
        mut on_delta: impl FnMut(&str, bool),
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

            // Estimate before the body moves `aug_messages` into the request.
            let est: u64 = aug_messages
                .iter()
                .map(|m| m.to_string().len() as u64)
                .sum::<u64>()
                / 4;
            let body = serde_json::json!({
                "model": self.config.model,
                "messages": aug_messages,
                // Same fix as chat_marc27_simple: this path previously sent
                // no cap at all, so a tool-calling turn could generate an
                // unbounded (and unbounded-billed) response.
                "max_tokens": self.effective_max_tokens(est),
            });
            // Use a direct request (not the retry-wrapper post()) so we control headers
            let mut req = self
                .client
                .post(&url)
                .json(&body)
                .header("Accept", "text/event-stream");
            if let Some((name, value)) = self.auth_header() {
                req = req.header(name, value);
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
                        if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str())
                            && !delta.is_empty()
                        {
                            full_text.push_str(delta);
                            // Don't call on_delta during streaming for MARC27 path.
                            // We collect full_text, strip tool calls, then emit clean
                            // content_text after the response completes. This prevents
                            // partial tool call JSON from leaking into visible text.
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
        let url = self.chat_completions_url();

        let est = Self::estimate_tokens(&serde_json::to_value(messages).unwrap_or_default())
            + Self::estimate_tokens(&serde_json::to_value(tools).unwrap_or_default());
        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "temperature": 0.1,
            "max_tokens": self.effective_max_tokens(est),
            "stream": true,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        let mut req = self.client.post(&url).json(&body);
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
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
                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data)
                {
                    // Extract text delta — separate content from
                    // reasoning_content. Content is the actual response;
                    // reasoning_content is thinking/reasoning tokens that
                    // should be rendered dimmed/collapsed in the TUI.
                    let content_delta = chunk
                        .pointer("/choices/0/delta/content")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty());

                    let reasoning_delta = chunk
                        .pointer("/choices/0/delta/reasoning_content")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty());

                    if let Some(delta) = content_delta {
                        on_delta(delta, false);
                        full_content.push_str(delta);
                    } else if let Some(delta) = reasoning_delta {
                        // Reasoning tokens — is_reasoning=true so the
                        // agent loop can emit them as ui.thinking.delta
                        on_delta(delta, true);
                        full_content.push_str(delta);
                    }

                    // Extract streaming tool calls
                    if let Some(tcs) = chunk
                        .pointer("/choices/0/delta/tool_calls")
                        .and_then(|t| t.as_array())
                    {
                        for tc in tcs {
                            let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
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
        if let Some((name, value)) = self.auth_header() {
            req = req.header(name, value);
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

    /// The auth header `(name, value)` for the configured credential, routing
    /// by shape: `m27_*` MARC27 platform API keys go on `X-API-Key` (the
    /// platform rejects them on Bearer); session JWTs and provider keys stay on
    /// `Authorization: Bearer`. This lets a headless chat server reach the
    /// MARC27 LLM proxy with a non-expiring API key — no login, no refresh.
    fn auth_header(&self) -> Option<(&'static str, String)> {
        let key = self.config.api_key.as_ref().filter(|k| !k.is_empty())?;
        if key.starts_with("m27_") {
            Some(("X-API-Key", key.clone()))
        } else {
            Some(("Authorization", format!("Bearer {key}")))
        }
    }

    async fn post(&self, url: &str, body: &serde_json::Value) -> Result<reqwest::Response> {
        debug!(%url, "LLM request");
        for attempt in 0..3u32 {
            let mut req = self.client.post(url).json(body);
            if let Some((name, value)) = self.auth_header() {
                req = req.header(name, value);
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
}

// ── MARC27 text-based tool calling helpers ──────────────────────────

/// Build a lightweight tool catalog for the system prompt.
///
/// Instead of dumping all 108 tool definitions (11K+ tokens), we give the model:
/// 1. A categorized summary of what's available
/// 2. Instructions to call `find_tools` for specifics
/// 3. The tool calling syntax
///
/// Full tool definitions are injected only after find_tools returns.
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
        - Call `find_tools` (with a `query`) first if you need to discover what tools exist\n\
        - Output ONE ```tool_call block, then STOP IMMEDIATELY. Do not write anything after it.\n\
        - Do NOT output multiple tool_call blocks in one response.\n\
        - Do NOT guess, fabricate, or hallucinate tool results. EVER.\n\
        - After your ```tool_call block, the system executes it and returns the result.\n\
        - You will see the result in your next message, then you can respond or call another tool.\n\
        - If you need multiple tools, call them one at a time across multiple turns.\n\n\
        **When a tool fails (recovery rules — DO NOT GIVE UP):**\n\
        - A tool returning an error is NORMAL. It is NOT a signal to stop.\n\
        - If a tool returns a missing-API-key error (e.g. \"MP_API_KEY not set\"), \
        immediately try a keyless alternative: `materials_search` (OPTIMADE federation, \
        no key needed) or `prior_art_search` (literature) before giving up.\n\
        - If a tool returns \"unknown tool\", call `find_tools` to see real \
        names, then try the closest match. Do not give up.\n\
        - If two tools have failed for the same goal, call `find_tools` again, \
        then propose the next-best tool. The user expects multiple tool attempts on \
        failure — silence is the worst outcome.\n\
        - NEVER respond with empty content + no tool call after a tool error. Either \
        try a different tool, or explicitly tell the user which tools you tried and \
        why none of them worked.\n\n\
        ## Quick reference (most common tools)\n\n\
        - `find_tools` — discover tools by capability/keyword (progressive tool discovery)\n\
        - `query_platform` — search the MARC27 knowledge graph (plain text = graph, semantic=true = vector)\n\
        - `materials_search` — federated search across 20+ materials databases (OPTIMADE)\n\
        - `predict` — predict a material property from composition (ML)\n\
        - `execute_python` — run Python code for analysis\n\
        - `web` — fetch a URL or search the open web (action='read' / 'search')\n\
        - `prior_art_search` — search arXiv, Semantic Scholar, and patents (Lens.org)\n\
        - `research` — iterative research loop via the MARC27 platform\n\n\
        Names above MUST match the actual registry. If a tool you'd expect \
        isn't in this list, call `find_tools` instead of guessing.\n\n\
        ## Tool-composition patterns (USE THESE for the common tasks)\n\n\
        PRISM is a materials-discovery strategy engine, not just a chat model. \
        For non-trivial questions you should COMPOSE multiple tools instead of \
        relying on a single one.\n\n\
        **CRITICAL — where materials data actually lives:**\n\
        - Materials property data (creep, modulus, density, band gap, etc.) \
        lives in `materials_search` (federated DB across MP / OPTIMADE / 18 \
        others) and in academic papers via `prior_art_search`. NOT on vendor \
        websites.\n\
        - Vendor PDFs (specialmetals.com, haynesintl.com, nickelinstitute.org, \
        matweb.com, hightempmetals.com, …) are paywalled, robots-blocked, or \
        gated. The `web` tool WILL return 403 / 404 / robots.txt on them. \
        Do not chain guesses at vendor URLs — that loop never converges.\n\
        - **Search engines + government repos block the `web` tool's User-Agent.** \
        Do NOT call `web` GET on `google.com/search`, `bing.com/search`, \
        `duckduckgo.com`, `osti.gov/servlets/*`, `osti.gov/biblio/*` — every one \
        returns robots.txt or 403. Observed cost in real runs: ~15 wasted tool \
        calls per question. Use `prior_art_search` (Semantic Scholar / arXiv / \
        OpenAlex / PubMed) or `research` instead. The CrossRef API \
        (`api.crossref.org/works`) IS accessible and is the right place for \
        DOI-based citation lookups.\n\
        - For ANY question of the form \"compare property X of alloys A, B, C\" \
        or \"what is property Y of material Z\", your FIRST tool call should be \
        `materials_search` or `prior_art_search` — never a `web` GET against a \
        vendor domain.\n\
        - `research` (the server-side RLM) is the right call when the question \
        spans multiple alloy systems + multiple properties + needs synthesis. \
        It already searches Semantic Scholar / arXiv / OpenAlex / the KG \
        internally; you do not need to do that hop yourself.\n\n\
        The most common patterns:\n\n\
        - **Materials-discovery**: \
        `materials_search` (federated DB lookup) → `prior_art_search` (literature \
        cross-check on the candidates that came back) → `predict` (only if you \
        need a property the DB didn't return). Output candidates with BOTH a \
        DB id AND a paper citation. Never propose a composition without a \
        traceable source.\n\
        - **Property-prediction**: `predict` first, then validate with \
        `prior_art_search` on the predicted property to see if literature \
        agrees with the model output.\n\
        - **Use-case scoping** (\"can material X be used for Y?\"): \
        `prior_art_search` first (does anyone publish on this?), then \
        `materials_search` for compositional alternatives, then `web` only \
        for industry / regulatory context that isn't in academic papers.\n\
        - **Knowledge-graph queries**: `knowledge` for MARC27-internal \
        provenance. Use BEFORE `materials_search` if the user is asking \
        about a specific project / dataset rather than a general material.\n\n\
        For ANY recommendation you give the user: cite the source. \
        \"Composition X has property Y\" must come with a tool result reference \
        (DB id, paper DOI, predict() output id). \"It's a known refractory \
        alloy\" without a citation is hallucination, not strategy.\n\n\
        ## Long-horizon discipline (the difference between PRISM and a chatbot)\n\n\
        Real materials questions take MANY tool calls — typically 8 to 30 — \
        and span minutes, not seconds. The literature shows that LLMs at long \
        horizons fail in two predictable ways: they (a) terminate early after \
        2–3 tool calls, returning a thin answer, or (b) forget the original \
        constraint by turn 10. Both are unacceptable here. The user is paying \
        for a strategy engine; behave like one.\n\n\
        **For any non-trivial question (not a single-fact lookup), follow this \
        loop:**\n\n\
        1. **Plan first, in writing.** Before ANY tool call, emit a numbered \
        plan listing the sub-questions you need to answer and which tool you \
        will use for each. This is your scratchpad and your contract with \
        the user. Re-read it before every subsequent tool call.\n\
        2. **Use `research` for deep multi-hop questions.** `research(question=...)` \
        runs a server-side Recursive Language Model that does iterative \
        decomposition + literature search + KG traversal in ONE call. Prefer \
        ONE `research` call over five hand-rolled `prior_art_search` + `web` \
        calls when the question is open-ended (\"design an alloy for X\", \
        \"compare approaches to Y\"). It exists because of arxiv:2512.24601; \
        you are the one calling it.\n\
        3. **Persist past the urge to wrap up.** If you've made fewer than \
        five tool calls on a multi-part question, you are NOT done. Asking \
        yourself \"do I have enough?\" after two calls is the failure mode. \
        Instead ask: \"which sub-question on my plan is still un-answered?\" \
        and call the next tool.\n\
        4. **Re-anchor on the original goal every ~5 turns.** Quote the \
        user's original ask back to yourself in your reasoning. The most \
        common long-horizon failure is silently drifting from \"design an RHEA \
        for LPBF at 2200 °C\" to \"list some refractory metals\".\n\
        5. **Deliberate completion.** When you ARE done, emit the marker \
        `FINAL ANSWER:` followed by the synthesized answer with citations. \
        This is the only acceptable way to end a research turn. An empty \
        response, or a response that just summarizes one tool's output \
        without synthesis, is not completion — it is giving up.\n\n\
        Long horizon is the product. The compaction system, the research \
        tool, and the recovery rules above all exist so you can sustain \
        20+ tool calls on one question without losing the thread. Use them.\n\
    ");

    block
}

/// Parse ```tool_call blocks from response text.
/// Return the byte index just past the `}` that closes the JSON object
/// starting at `start` (which must be a `{`), respecting string literals and
/// escapes. `None` if the object is unbalanced. Only ever returns indices at
/// ASCII `}` positions, so the result is a valid char boundary.
fn balanced_object_end(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    if b.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// If a loose tool-call marker token (`tool_call`, `_call`, `call`,
/// `function_call`, optionally back-ticked / colon-suffixed) immediately
/// precedes the JSON object at `obj_start`, return the marker's start offset so
/// callers can strip it too; otherwise return `obj_start`.
fn marker_start_before(text: &str, obj_start: usize) -> usize {
    const MARKERS: &[&str] = &["tool_call", "_call", "call", "function_call"];
    let trimmed = text[..obj_start].trim_end();
    let token_start = trimmed
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let norm = trimmed[token_start..]
        .trim_matches('`')
        .trim_end_matches(':')
        .to_ascii_lowercase();
    if MARKERS.contains(&norm.as_str()) {
        token_start
    } else {
        obj_start
    }
}

/// Recover a bare (un-fenced) JSON tool call: the first `{...}` object that
/// deserializes and carries a non-empty, whitespace-free `name` plus an
/// `arguments` **object**. Returns `(region_start, name, arguments_json)` where
/// `region_start` includes any immediately-preceding loose marker line.
///
/// This is the recovery path for models that drop the ```tool_call fence.
/// Observed live on the MARC27 `/stream` text-tool-calling path: after a
/// natural-language preamble, Claude emitted `_call\n{"name":...}` with no
/// backticks, so the fenced/XML parsers missed it and the call silently leaked
/// into visible text and ended the turn. The guards (name shape + arguments is
/// an object) keep incidental prose JSON from being mistaken for a call.
fn find_bare_json_tool_call(text: &str) -> Option<(usize, String, String)> {
    let mut from = 0;
    while let Some(rel) = text[from..].find('{') {
        let obj_start = from + rel;
        if let Some(obj_end) = balanced_object_end(text, obj_start)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[obj_start..obj_end])
        {
            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args_is_obj = v.get("arguments").map(|a| a.is_object()).unwrap_or(false);
            if !name.is_empty() && !name.contains(char::is_whitespace) && args_is_obj {
                let arguments = v
                    .get("arguments")
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let region_start = marker_start_before(text, obj_start);
                return Some((region_start, name.to_string(), arguments));
            }
        }
        from = obj_start + 1;
    }
    None
}

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

    // Format 3: bare JSON tool call — recovery for a dropped/mangled
    // ```tool_call fence. Observed on the MARC27 /stream path: after a prose
    // preamble the model emitted `_call\n{"name":...,"arguments":{...}}` with no
    // backticks, so Formats 1-2 missed it and the call silently leaked as text.
    if calls.is_empty()
        && let Some((_, name, arguments)) = find_bare_json_tool_call(text)
    {
        calls.push(ToolCallResponse {
            id: format!("tc_{call_idx}"),
            call_type: "function".to_string(),
            function: FunctionCall { name, arguments },
        });
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
    // Also strip a bare/mangled-fence tool call (Format 3), truncating at its
    // marker so `_call\n{...}` doesn't survive into visible content.
    let bare = find_bare_json_tool_call(text).map(|(start, _, _)| start);
    let first = [fenced, xml, bare].into_iter().flatten().min();
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
        assert_eq!(
            client.auth_header(),
            Some(("Authorization", "Bearer sk-test123".to_string()))
        );
    }

    #[test]
    fn auth_header_routes_m27_api_key_to_x_api_key() {
        // `m27_*` platform keys must go on X-API-Key — the MARC27 backend
        // rejects them on Bearer. This is what lets a headless chat server
        // reach the LLM proxy with a non-expiring key.
        let config = LlmConfig {
            api_key: Some("m27_live_abc123".into()),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        assert_eq!(
            client.auth_header(),
            Some(("X-API-Key", "m27_live_abc123".to_string()))
        );
    }

    #[test]
    fn auth_header_none_when_no_key() {
        let config = LlmConfig::default();
        let client = LlmClient::new(config);
        assert!(client.auth_header().is_none());
    }

    #[test]
    fn effective_max_tokens_defaults_to_4096() {
        // No catalog max + unknown context (local model) → conservative 4096.
        let client = LlmClient::new(LlmConfig::default());
        assert_eq!(client.effective_max_tokens(0), 4096);
    }

    #[test]
    fn effective_max_tokens_honors_config_when_context_roomy() {
        let config = LlmConfig {
            max_output_tokens: Some(16_384),
            context_window: Some(200_000),
            ..Default::default()
        };
        let client = LlmClient::new(config);
        // Small prompt, huge context → the model max is the binding limit.
        assert_eq!(client.effective_max_tokens(1_000), 16_384);
    }

    #[test]
    fn effective_max_tokens_clamps_to_context_remaining() {
        // gpt-5-shaped: 128k max output but only 400k context. A 396k-token
        // prompt leaves far less than 128k of room — the clamp must bind, and
        // never exceed context − prompt − margin, nor drop below the floor.
        let config = LlmConfig {
            max_output_tokens: Some(128_000),
            context_window: Some(400_000),
            ..Default::default()
        };
        let client = LlmClient::new(config);

        // Roomy prompt: model max binds.
        assert_eq!(client.effective_max_tokens(10_000), 128_000);

        // Tight prompt: context binds. 400k − 396k − 1024 margin = 2976.
        assert_eq!(
            client.effective_max_tokens(396_000),
            400_000 - 396_000 - 1024
        );

        // Prompt bigger than the whole window: never underflows, floors at 256.
        assert_eq!(client.effective_max_tokens(500_000), 256);
    }

    #[test]
    fn extract_json_content_returns_real_content() {
        let choice = serde_json::json!({
            "finish_reason": "stop",
            "message": {"content": "{\"entities\": []}", "reasoning_content": ""}
        });
        assert_eq!(
            LlmClient::extract_json_content(&choice).unwrap(),
            "{\"entities\": []}"
        );
    }

    #[test]
    fn extract_json_content_never_falls_back_to_reasoning() {
        // Empty content + finish_reason "stop" (not length) with reasoning
        // text present must still fail rather than return the reasoning
        // text as if it were JSON.
        let choice = serde_json::json!({
            "finish_reason": "stop",
            "message": {"content": "", "reasoning_content": "I am thinking about it..."}
        });
        let err = LlmClient::extract_json_content(&choice).unwrap_err();
        assert!(err.to_string().contains("empty content"));
    }

    #[test]
    fn extract_json_content_diagnoses_thinking_model_length_cutoff() {
        // The live failure mode this fixes: a reasoning model burns the
        // whole max_tokens budget on reasoning_content and never emits
        // JSON content, ending with finish_reason=length.
        let choice = serde_json::json!({
            "finish_reason": "length",
            "message": {
                "content": "",
                "reasoning_content": "a very long chain of thought".repeat(100),
            }
        });
        let err = LlmClient::extract_json_content(&choice).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thinking"), "message was: {msg}");
        assert!(msg.contains("max_output_tokens"), "message was: {msg}");
    }

    #[test]
    fn extract_json_content_generic_empty_response_is_honest() {
        let choice = serde_json::json!({
            "finish_reason": "stop",
            "message": {"content": ""}
        });
        let err = LlmClient::extract_json_content(&choice).unwrap_err();
        assert!(err.to_string().contains("finish_reason=stop"));
    }

    #[test]
    fn format1_fenced_tool_call_still_parses() {
        // Regression: the well-formed fence must keep working unchanged.
        let text = "Let me search.\n```tool_call\n{\"name\": \"web\", \"arguments\": {\"query\": \"x\"}}\n```";
        let calls = parse_text_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "web");
        assert_eq!(strip_tool_call_blocks(text), "Let me search.");
    }

    #[test]
    fn format3_recovers_mangled_underscore_call_marker() {
        // The exact live failure: after a prose preamble Claude dropped the
        // ```tool_call fence and emitted `_call\n{...}`. Must be recovered.
        let text = "The prior_art results were noisy — not useful. I'll cross-check \
                    the knowledge graph directly.\n\n_call\n{\"name\": \"knowledge\", \
                    \"arguments\": {\"action\": \"search\", \"query\": \"HfC-TaC\"}}";
        let calls = parse_text_tool_calls(text);
        assert_eq!(calls.len(), 1, "mangled-marker call must be recovered");
        assert_eq!(calls[0].function.name, "knowledge");
        assert!(calls[0].function.arguments.contains("\"action\""));
    }

    #[test]
    fn format3_recovers_bare_json_with_no_marker() {
        let text = "Let me look that up.\n{\"name\": \"web\", \"arguments\": \
                    {\"action\": \"search\", \"query\": \"x\"}}";
        let calls = parse_text_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "web");
    }

    #[test]
    fn format3_ignores_prose_json_that_is_not_a_tool_call() {
        // arguments is a string, not an object → not a call.
        assert!(
            parse_text_tool_calls("A record: {\"name\": \"Alice\", \"arguments\": \"none\"}.")
                .is_empty()
        );
        // no `arguments` key at all → not a call.
        assert!(parse_text_tool_calls("Config: {\"name\": \"widget\", \"value\": 3}").is_empty());
        // name contains whitespace → not a tool name.
        assert!(parse_text_tool_calls("{\"name\": \"John Smith\", \"arguments\": {}}").is_empty());
    }

    #[test]
    fn strip_removes_mangled_call_region_keeps_preamble() {
        let text = "Cross-checking the graph directly.\n\n_call\n\
                    {\"name\": \"knowledge\", \"arguments\": {\"action\": \"search\"}}";
        assert_eq!(
            strip_tool_call_blocks(text),
            "Cross-checking the graph directly."
        );
    }

    /// Pin the curated tool names in the system-prompt quick-reference.
    ///
    /// Each name below MUST be a real tool registered in `app/tools/*.py`
    /// (`registry.register(Tool(name=...))`). When a tool is renamed,
    /// update both this list AND `build_tool_prompt_block` together —
    /// otherwise the LLM gets a stale name in its system prompt and
    /// hallucinates calls to it (we shipped this exact bug: the old
    /// `search_materials` line stayed in the prompt for ~2 rounds after
    /// the tool was renamed to `materials_search`, and gemini-3.1 dutifully
    /// called the dead name on every materials request).
    ///
    /// This test only proves the strings render into the prompt block.
    /// A future `boot_checks` entry can run the actual cross-check
    /// against `tool_server.list_tools()` at startup.
    const QUICK_REFERENCE_TOOL_NAMES: &[&str] = &[
        "find_tools",
        "query_platform",
        "materials_search",
        "predict",
        "execute_python",
        "web",
        "prior_art_search",
        "research",
    ];

    #[test]
    fn quick_reference_names_appear_in_prompt() {
        let block = build_tool_prompt_block(&[]);
        for name in QUICK_REFERENCE_TOOL_NAMES {
            assert!(
                block.contains(&format!("`{name}`")),
                "tool `{name}` missing from quick-reference block — \
                 either restore it or remove it from QUICK_REFERENCE_TOOL_NAMES"
            );
        }
    }

    /// Pin the long-horizon orchestration patterns shipped in PRs #109 and #111.
    ///
    /// These markers exist because the BimoTech / Fraunhofer end-to-end test
    /// surfaced two real failure modes: (1) the LLM gave up after one tool
    /// error, and (2) the LLM wrapped up after 2-3 tool calls on a question
    /// that needed 8-30. The fixes are SYSTEM PROMPT TEXT — they have no
    /// other code path. If a future refactor silently drops these strings,
    /// the regression isn't visible until a customer hits it. This test
    /// catches the silent-drop case.
    ///
    /// Backed by the literature: arxiv 2604.11978 (Long-Horizon Mirage),
    /// arxiv 2603.29231 (Beyond pass@1), arxiv 2512.24601 (RLM `FINAL()`),
    /// arxiv 2605.02572 (empirical horizon-length study).
    #[test]
    fn long_horizon_orchestration_markers_present() {
        let block = build_tool_prompt_block(&[]);
        let required_markers: &[(&str, &str)] = &[
            ("DO NOT GIVE UP", "recovery-rules header from #109"),
            (
                "NEVER respond with empty content",
                "anti-early-termination rule from #109",
            ),
            (
                "Tool-composition patterns",
                "composition cookbook header from #109",
            ),
            (
                "Long-horizon discipline",
                "long-horizon section header from #111",
            ),
            ("Plan first, in writing", "plan-emission rule from #111"),
            ("FINAL ANSWER:", "deliberate-completion marker from #111"),
            // Tightened from a bare `research` substring (which would match
            // `research`, `prior_art_search`, `research_query`, and 12 other
            // unrelated occurrences — the previous form was effectively a
            // no-op). The new pin is the specific guidance string that
            // PR #111 added to direct the agent at the RLM tool for deep
            // multi-hop questions.
            (
                "Use `research` for deep multi-hop questions",
                "RLM-as-default rule from #111",
            ),
            // PR #114 — vendor-PDF clarifier. Without these pins, the
            // entire "where materials data actually lives" block can be
            // silently deleted with green tests. The end-to-end Test 3
            // trace (2026-05-10 ODS-alloy prompt) confirmed the agent
            // genuinely changes behaviour when this section is present.
            (
                "where materials data actually lives",
                "vendor-PDF clarifier section header from #114",
            ),
            ("Vendor PDFs", "vendor-PDF do-not-call rule from #114"),
            (
                "Do not chain guesses at vendor URLs",
                "anti-URL-enumeration rule from #114",
            ),
            // PR #115 — search engine + OSTI blacklist. Concrete domain
            // names are pinned because the rule's effectiveness depends on
            // the agent reading them verbatim.
            (
                "Search engines + government repos block",
                "search-engine blacklist section header from #115",
            ),
            (
                "google.com/search",
                "blacklisted Google search URL pattern from #115",
            ),
            ("osti.gov", "blacklisted OSTI repo pattern from #115"),
            (
                "CrossRef API",
                "allowed-fallback CrossRef pointer from #115",
            ),
        ];
        for (marker, why) in required_markers {
            assert!(
                block.contains(marker),
                "long-horizon marker `{marker}` missing from prompt block ({why}). \
                 If you intentionally removed it, update this test. If not, \
                 you've silently regressed PR #109, #111, #114, or #115."
            );
        }
    }

    #[test]
    fn quick_reference_does_not_mention_renamed_tools() {
        // Belt-and-braces: explicit deny-list of names we've previously
        // renamed and don't want sneaking back into the prompt.
        let block = build_tool_prompt_block(&[]);
        for stale in &[
            "search_materials",
            "knowledge_search",
            "predict_property",
            "web_search",
            "web_read",
            "literature_search",
            "research_query",
            "semantic_search",
        ] {
            assert!(
                !block.contains(&format!("`{stale}`")),
                "stale tool name `{stale}` reappeared in quick-reference block"
            );
        }
    }

    /// Cross-check the curated list against the actual Python tool registry.
    ///
    /// The two earlier tests catch *internal* drift (curated list vs prompt
    /// text). They DO NOT catch the worst case: someone renames a tool in
    /// `app/tools/*.py` and forgets to update the prompt — both sides of
    /// the internal check still agree, but the LLM gets a name that no
    /// longer matches reality. That's exactly how PR #91's `search_materials`
    /// bug shipped.
    ///
    /// This test reads `app/tools/*.py` directly and confirms every name in
    /// `QUICK_REFERENCE_TOOL_NAMES` appears as a `name="..."` registration.
    /// No Python subprocess, no runtime cost — just file IO at test time.
    ///
    /// If `app/tools/` is missing (e.g., someone runs the test outside a
    /// full PRISM checkout), the test soft-skips so it doesn't break
    /// downstream builds of the crate in isolation.
    #[test]
    fn quick_reference_names_are_registered_in_python() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let tools_dir = std::path::Path::new(manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|p| p.join("app").join("tools"));

        let Some(tools_dir) = tools_dir else {
            eprintln!("skipping cross-check: cannot resolve workspace root");
            return;
        };
        if !tools_dir.is_dir() {
            eprintln!(
                "skipping cross-check: app/tools/ not found at {}",
                tools_dir.display()
            );
            return;
        }

        // Recursively scan every .py file under app/tools/ for
        // `name="..."` (or `name='...'`) tokens. The matcher is
        // intentionally simple — looking for the exact registration
        // pattern `name="<identifier>"` on its own line, which is
        // how every existing tool registers (see e.g.
        // `app/tools/research.py:6` "        name="research",").
        let mut registered = std::collections::BTreeSet::<String>::new();
        let mut stack = vec![tools_dir.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "py") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for line in text.lines() {
                    let trimmed = line.trim();
                    // Match either name="x" or name='x'.
                    for quote in ['"', '\''] {
                        let prefix = format!("name={quote}");
                        if let Some(rest) = trimmed.strip_prefix(&prefix)
                            && let Some(end) = rest.find(quote)
                        {
                            let name = &rest[..end];
                            if !name.is_empty()
                                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            {
                                registered.insert(name.to_string());
                            }
                        }
                    }
                }
            }
        }

        if registered.is_empty() {
            // Defensive: if our matcher ever stops finding any registrations
            // at all, prefer a clear failure to a silent green.
            panic!(
                "cross-check found ZERO tool registrations under {} — \
                 either the matcher is broken or the file layout changed. \
                 Update this test before continuing.",
                tools_dir.display()
            );
        }

        // Spine tools live in Rust, not app/tools/*.py: `find_tools` is an
        // always-on meta-tool (crates/agent/src/meta_tools.rs); `query_platform`
        // and `research` are Rust command-tools (crates/agent/src/command_tools.rs)
        // that replaced retired Python tools (knowledge.py / research.py). They
        // are real, just not Python-registered — exempt them from the Python
        // cross-check (the anti-dead-tool intent still covers the rest).
        const RUST_NATIVE: &[&str] = &["find_tools", "query_platform", "research"];

        let missing: Vec<&str> = QUICK_REFERENCE_TOOL_NAMES
            .iter()
            .copied()
            .filter(|n| !RUST_NATIVE.contains(n))
            .filter(|n| !registered.contains(*n))
            .collect();

        assert!(
            missing.is_empty(),
            "tool name(s) in quick-reference are NOT registered in app/tools/ or RUST_NATIVE: {:?}\n\
             registered names found: {:?}\n\
             Either restore the registration in Python, add it to RUST_NATIVE if it is a \
             Rust command/meta tool, or remove the name from both \
             QUICK_REFERENCE_TOOL_NAMES and build_tool_prompt_block.",
            missing,
            registered.iter().take(20).collect::<Vec<_>>()
        );
    }

    #[test]
    fn strip_json_fences_variants() {
        // Fenced with language tag — the common Claude/marc27 shape.
        assert_eq!(
            LlmClient::strip_json_fences("```json\n{\"a\":1}\n```"),
            "{\"a\":1}"
        );
        // Fenced without a tag.
        assert_eq!(
            LlmClient::strip_json_fences("```\n{\"a\":1}\n```"),
            "{\"a\":1}"
        );
        // Bare JSON passes through untouched.
        assert_eq!(LlmClient::strip_json_fences("  {\"a\":1} "), "{\"a\":1}");
        // Unterminated fence: still yields the body rather than erroring.
        assert_eq!(
            LlmClient::strip_json_fences("```json\n{\"a\":1}"),
            "{\"a\":1}"
        );
    }
}

#[cfg(test)]
mod hydration_tests {
    use super::hydrate_env_from_map;

    /// Unique var names so parallel tests can't race on shared env state.
    #[test]
    fn file_fills_unset_env_but_never_overrides() {
        let mut map = serde_json::Map::new();
        map.insert(
            "PRISM_HYDRATE_TEST_UNSET_A".into(),
            serde_json::Value::String("from-file".into()),
        );
        map.insert(
            "PRISM_HYDRATE_TEST_PRESET_B".into(),
            serde_json::Value::String("from-file".into()),
        );
        map.insert(
            "PRISM_HYDRATE_TEST_EMPTY_C".into(),
            serde_json::Value::String(String::new()),
        );
        // SAFETY: test-only unique var names — no concurrent readers.
        unsafe { std::env::set_var("PRISM_HYDRATE_TEST_PRESET_B", "from-env") };

        hydrate_env_from_map(&map);

        assert_eq!(
            std::env::var("PRISM_HYDRATE_TEST_UNSET_A").as_deref(),
            Ok("from-file")
        );
        // Env wins over file — the file is a fallback, never an override.
        assert_eq!(
            std::env::var("PRISM_HYDRATE_TEST_PRESET_B").as_deref(),
            Ok("from-env")
        );
        // Empty strings are not exported.
        assert!(std::env::var_os("PRISM_HYDRATE_TEST_EMPTY_C").is_none());

        // SAFETY: test-only unique var names — no concurrent readers.
        unsafe {
            std::env::remove_var("PRISM_HYDRATE_TEST_UNSET_A");
            std::env::remove_var("PRISM_HYDRATE_TEST_PRESET_B");
        }
    }
}
