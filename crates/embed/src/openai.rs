// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! OpenAI-compatible HTTP backend — the open replacement port.
//!
//! Anything that speaks `POST {base}/v1/embeddings` with
//! `{"model": …, "input": […]}` plugs in here: hosted providers, a local
//! TEI/vLLM server, or the MARC27 API (same convention as marc27-core).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::{EmbedBackend, EmbedFileConfig};

/// Remote embedding backend over the OpenAI `/v1/embeddings` wire shape.
pub struct OpenAiCompat {
    client: reqwest::Client,
    url: String,
    model: String,
    api_key: Option<String>,
    id: String,
    /// Learned from the first response; `0` until then.
    dim: AtomicUsize,
}

/// `{base}/embeddings` when the base already ends in `/v1`, else
/// `{base}/v1/embeddings`.
fn embeddings_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/embeddings")
    } else {
        format!("{base}/v1/embeddings")
    }
}

fn request_body(model: &str, texts: &[String]) -> serde_json::Value {
    serde_json::json!({ "model": model, "input": texts })
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Deserialize)]
struct EmbedDatum {
    #[serde(default)]
    index: Option<usize>,
    embedding: Vec<f32>,
}

impl OpenAiCompat {
    /// Env first (`PRISM_EMBED_ENDPOINT_URL`, `PRISM_EMBED_MODEL`,
    /// `PRISM_EMBED_API_KEY`), then the `[embedding]` section of
    /// `~/.prism/prism.toml`. Endpoint and model are required.
    pub fn from_config(file: &EmbedFileConfig) -> Result<Self> {
        let get = |env: &str, file_val: &Option<String>| {
            std::env::var(env)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| file_val.clone().filter(|s| !s.trim().is_empty()))
        };
        let base = get("PRISM_EMBED_ENDPOINT_URL", &file.endpoint_url).context(
            "openai embedding backend selected but PRISM_EMBED_ENDPOINT_URL \
             (or [embedding].endpoint_url in ~/.prism/prism.toml) is not set",
        )?;
        let model = get("PRISM_EMBED_MODEL", &file.model).context(
            "openai embedding backend selected but PRISM_EMBED_MODEL \
             (or [embedding].model in ~/.prism/prism.toml) is not set",
        )?;
        let api_key = get("PRISM_EMBED_API_KEY", &file.api_key);
        Ok(Self::new(&base, &model, api_key))
    }

    /// Build directly from parts (no env access) — used by tests and callers
    /// with their own config plumbing.
    pub fn new(base: &str, model: &str, api_key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            url: embeddings_url(base),
            model: model.to_string(),
            api_key,
            id: format!("openai:{model}"),
            dim: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl EmbedBackend for OpenAiCompat {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        prism_runtime::offline::check_url(&self.url).map_err(anyhow::Error::msg)?;
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut req = self
            .client
            .post(&self.url)
            .json(&request_body(&self.model, texts));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("embedding request to {} failed", self.url))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "embedding endpoint {} returned {status}: {}",
                self.url,
                body.chars().take(300).collect::<String>()
            );
        }
        let parsed: EmbedResponse = resp
            .json()
            .await
            .context("embedding endpoint returned malformed JSON")?;
        if parsed.data.len() != texts.len() {
            bail!(
                "embedding endpoint returned {} vectors for {} inputs",
                parsed.data.len(),
                texts.len()
            );
        }

        // Servers are allowed to reorder; restore input order via `index`
        // when present.
        let mut data = parsed.data;
        if data.iter().all(|d| d.index.is_some()) {
            data.sort_by_key(|d| d.index.unwrap_or(usize::MAX));
        }
        if let Some(first) = data.first() {
            self.dim.store(first.embedding.len(), Ordering::Relaxed);
        }
        Ok(data.into_iter().map(|d| d.embedding).collect())
    }

    fn dimensions(&self) -> usize {
        self.dim.load(Ordering::Relaxed)
    }

    fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_appends_v1_when_missing() {
        assert_eq!(
            embeddings_url("https://api.example.com"),
            "https://api.example.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("http://localhost:8080/"),
            "http://localhost:8080/v1/embeddings"
        );
    }

    #[test]
    fn url_respects_existing_v1_suffix() {
        assert_eq!(
            embeddings_url("https://api.example.com/v1"),
            "https://api.example.com/v1/embeddings"
        );
        assert_eq!(
            embeddings_url("https://api.example.com/v1/"),
            "https://api.example.com/v1/embeddings"
        );
    }

    #[test]
    fn request_body_matches_openai_shape() {
        let body = request_body("nv-embed-v2", &["a".to_string(), "b".to_string()]);
        assert_eq!(
            body,
            serde_json::json!({ "model": "nv-embed-v2", "input": ["a", "b"] })
        );
    }

    #[test]
    fn response_parses_and_reorders_by_index() {
        let raw = serde_json::json!({
            "object": "list",
            "data": [
                { "index": 1, "embedding": [0.0, 1.0] },
                { "index": 0, "embedding": [1.0, 0.0] }
            ],
            "model": "m"
        });
        let mut parsed: EmbedResponse = serde_json::from_value(raw).unwrap();
        parsed.data.sort_by_key(|d| d.index.unwrap_or(usize::MAX));
        assert_eq!(parsed.data[0].embedding, vec![1.0, 0.0]);
        assert_eq!(parsed.data[1].embedding, vec![0.0, 1.0]);
    }

    #[test]
    fn backend_id_and_initial_dimensions() {
        let b = OpenAiCompat::new("https://api.example.com", "text-embedding-3-small", None);
        assert_eq!(b.id(), "openai:text-embedding-3-small");
        assert_eq!(b.dimensions(), 0); // unknown until the first response
        assert_eq!(b.url, "https://api.example.com/v1/embeddings");
    }

    /// A target that is NOT loopback — so `PRISM_OFFLINE=1` must refuse it —
    /// and that refuses a connection immediately, so the not-offline half
    /// costs milliseconds. A TEST-NET-3 address blackholes instead of
    /// refusing, which is how a test in this repo once took 75 seconds.
    const UNREACHABLE_BASE: &str = "http://0.0.0.0:1";

    /// Embedding text is the one call in this crate that ships user content to
    /// a third party, so hard offline mode must refuse it before a socket
    /// opens.
    ///
    /// Both halves are asserted. Offline-only would pass just as happily
    /// against an `embed` that refused unconditionally, and would therefore
    /// prove nothing. The refusal is matched on its MESSAGE, not merely on
    /// `is_err`, so a connection failure cannot masquerade as a guard hit.
    // Holding the lock across the awaits is the point — it is what stops a
    // concurrent test from flipping `PRISM_OFFLINE` mid-call. Same precedent
    // as `client/src/api.rs` and `node/src/daemon.rs`.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn embed_is_refused_by_offline_mode_and_only_by_it() {
        // The shared lock in prism-runtime, deliberately not a local one:
        // `PRISM_OFFLINE` is process-global and two locks that do not exclude
        // each other serialize nothing.
        use prism_runtime::offline::test_support::{OfflineEnvGuard, env_lock};

        let _lock = env_lock();
        let backend = OpenAiCompat::new(UNREACHABLE_BASE, "offline-guard-probe", None);
        let texts = vec!["probe".to_string()];

        // Each guard restores `PRISM_OFFLINE` on drop, so an assertion panic —
        // exactly what this test exists to produce — cannot leak the variable
        // into every later test in the binary.
        let blocked = {
            let _offline = OfflineEnvGuard::set("1");
            format!("{:#}", backend.embed(&texts).await.unwrap_err())
        };
        assert!(
            blocked.contains("offline mode"),
            "PRISM_OFFLINE=1 must refuse {UNREACHABLE_BASE} as policy, got: {blocked}"
        );

        let attempted = {
            let _online = OfflineEnvGuard::clear();
            format!("{:#}", backend.embed(&texts).await.unwrap_err())
        };
        assert!(
            !attempted.contains("offline mode"),
            "offline mode must not refuse with PRISM_OFFLINE unset, got: {attempted}"
        );
        assert!(
            attempted.contains("embedding request to"),
            "with offline mode off the call must reach the transport, got: {attempted}"
        );
    }
}
