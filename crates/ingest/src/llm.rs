//! LLM provider abstraction — route generation and embedding requests to any
//! compatible backend: Ollama, OpenAI, Anthropic, MARC27 managed, vLLM, etc.
//!
//! Two wire formats:
//! - **Ollama native** (`/api/generate`, `/api/embed`) — default for local Ollama
//! - **OpenAI-compatible** (`/v1/chat/completions`, `/v1/embeddings`) — works with
//!   OpenAI, Anthropic (via proxy), MARC27 platform, vLLM, LiteLLM, and Ollama ≥0.1.24

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::debug;

use crate::LlmConfig;

/// Which API wire format to use.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    /// Ollama native API (`/api/generate`, `/api/embed`). Default.
    #[default]
    Ollama,
    /// OpenAI-compatible API (`/v1/chat/completions`, `/v1/embeddings`).
    /// Works with: OpenAI, Anthropic (via proxy), MARC27, vLLM, LiteLLM, Ollama.
    OpenAi,
}

/// Unified LLM client — dispatches to the right wire format based on provider.
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

    /// Generate text from a prompt. Returns the raw text response.
    pub async fn generate(&self, prompt: &str) -> Result<String> {
        match self.config.provider {
            LlmProvider::Ollama => self.generate_ollama(prompt).await,
            LlmProvider::OpenAi => self.generate_openai(prompt).await,
        }
    }

    /// Generate text with a system message + user message (chat format).
    pub async fn chat(&self, system: &str, user: &str) -> Result<String> {
        match self.config.provider {
            LlmProvider::Ollama => {
                // Ollama /api/generate supports system field
                self.generate_ollama_with_system(system, user).await
            }
            LlmProvider::OpenAi => self.chat_openai(system, user).await,
        }
    }

    /// Generate text and parse as JSON. Ollama supports `format: "json"`,
    /// OpenAI uses `response_format: { type: "json_object" }`.
    pub async fn generate_json(&self, prompt: &str) -> Result<String> {
        match self.config.provider {
            LlmProvider::Ollama => self.generate_ollama_json(prompt).await,
            LlmProvider::OpenAi => self.generate_openai_json(prompt).await,
        }
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
        match self.config.provider {
            LlmProvider::Ollama => self.embed_ollama(texts).await,
            LlmProvider::OpenAi => self.embed_openai(texts).await,
        }
    }

    /// Health check — verify the LLM backend is reachable.
    pub async fn health_check(&self) -> Result<()> {
        match self.config.provider {
            LlmProvider::Ollama => self.health_ollama().await,
            LlmProvider::OpenAi => self.health_openai().await,
        }
    }

    fn auth_header(&self) -> Option<String> {
        self.config
            .api_key
            .as_ref()
            .filter(|k| !k.is_empty())
            .map(|k| format!("Bearer {k}"))
    }

    // ── Ollama native ──────────────────────────────────────────────

    async fn generate_ollama(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "prompt": prompt,
            "stream": false,
            "options": { "temperature": 0.1, "num_predict": 4096 },
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad Ollama generate response")?;
        Ok(data["response"].as_str().unwrap_or_default().to_string())
    }

    async fn generate_ollama_json(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "prompt": prompt,
            "stream": false,
            "format": "json",
            "options": { "temperature": 0.1, "num_predict": 4096 },
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad Ollama generate response")?;
        Ok(data["response"].as_str().unwrap_or_default().to_string())
    }

    async fn generate_ollama_with_system(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "system": system,
            "prompt": user,
            "stream": false,
            "options": { "temperature": 0.1, "num_predict": 4096 },
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad Ollama generate response")?;
        Ok(data["response"].as_str().unwrap_or_default().to_string())
    }

    async fn embed_ollama(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embed", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "input": texts,
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad Ollama embed response")?;
        let embeddings: Vec<Vec<f32>> = serde_json::from_value(
            data["embeddings"].clone(),
        )
        .context("failed to parse Ollama embeddings")?;
        Ok(embeddings)
    }

    async fn health_ollama(&self) -> Result<()> {
        let url = format!("{}/api/tags", self.config.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("LLM not reachable — is it running?")?;
        if !resp.status().is_success() {
            bail!("LLM health check returned {}", resp.status());
        }
        Ok(())
    }

    // ── OpenAI-compatible ──────────────────────────────────────────

    async fn generate_openai(&self, prompt: &str) -> Result<String> {
        self.chat_openai("You are a helpful assistant.", prompt).await
    }

    async fn generate_openai_json(&self, prompt: &str) -> Result<String> {
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
        let data: serde_json::Value = resp.json().await.context("bad OpenAI chat response")?;
        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    async fn chat_openai(&self, system: &str, user: &str) -> Result<String> {
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
        let data: serde_json::Value = resp.json().await.context("bad OpenAI chat response")?;
        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    async fn embed_openai(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/v1/embeddings", self.config.base_url);
        let body = serde_json::json!({
            "model": self.config.model,
            "input": texts,
        });
        let resp = self.post(&url, &body).await?;
        let data: serde_json::Value = resp.json().await.context("bad OpenAI embed response")?;
        let arr = data["data"]
            .as_array()
            .context("expected data array in embeddings response")?;
        let mut embeddings = Vec::with_capacity(arr.len());
        for item in arr {
            let vec: Vec<f32> =
                serde_json::from_value(item["embedding"].clone()).context("bad embedding vector")?;
            embeddings.push(vec);
        }
        Ok(embeddings)
    }

    async fn health_openai(&self) -> Result<()> {
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

    // ── shared HTTP helper ─────────────────────────────────────────

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
    fn provider_serde_roundtrip() {
        let ollama: LlmProvider = serde_json::from_str(r#""ollama""#).unwrap();
        assert_eq!(ollama, LlmProvider::Ollama);
        let openai: LlmProvider = serde_json::from_str(r#""openai""#).unwrap();
        assert_eq!(openai, LlmProvider::OpenAi);
    }

    #[test]
    fn provider_default_is_ollama() {
        assert_eq!(LlmProvider::default(), LlmProvider::Ollama);
    }

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
