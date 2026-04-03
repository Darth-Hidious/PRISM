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

    /// Generate text with a system + user message.
    pub async fn chat(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
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

    /// Chat with tool-calling support.
    /// Sends full message history + tool definitions, returns response
    /// which may contain tool_calls.
    pub async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<ChatResponse> {
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
