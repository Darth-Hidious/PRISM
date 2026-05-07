//! Manages the `llama-server` subprocess hosting EmbeddingGemma and exposes
//! a small HTTP client for embedding requests.
//!
//! `llama-server` speaks an OpenAI-compatible `/v1/embeddings` endpoint when
//! launched with `--embeddings`. We spawn one per router instance, claim a
//! free port, wait for the `/health` endpoint to return ok, then issue
//! batched embedding requests.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use tokio::process::{Child, Command};

use crate::config::Config;
use crate::error::Error;

const READY_TIMEOUT_MS: u64 = 60_000;
const READY_POLL_MS: u64 = 250;

pub struct EmbedderServer {
    child: Child,
    base_url: String,
    http: Client,
    embed_dim: usize,
}

impl EmbedderServer {
    pub async fn spawn(config: &Config) -> Result<Self> {
        if !config.embedder_gguf.exists() {
            return Err(Error::ModelMissing(config.embedder_gguf.clone()).into());
        }
        if !config.llama_server_bin.exists() && !is_on_path(&config.llama_server_bin) {
            return Err(Error::LlamaServerMissing(config.llama_server_bin.clone()).into());
        }

        let port = pick_free_port(config.port_floor)?;
        tracing::info!(
            target: "prism_tool_router",
            port,
            model = %config.embedder_gguf.display(),
            "spawning embedder llama-server"
        );

        let child = Command::new(&config.llama_server_bin)
            .arg("--model")
            .arg(&config.embedder_gguf)
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--embeddings")
            .arg("--ctx-size")
            .arg(config.embed_ctx.to_string())
            // Bump the physical batch size to match ctx-size so we can embed
            // long tool descriptions without "input N tokens too large to
            // process" errors. Some forge built-ins (e.g. tools with full
            // JSON schemas) clear 1000 tokens.
            .arg("--batch-size")
            .arg(config.embed_ctx.to_string())
            .arg("--ubatch-size")
            .arg(config.embed_ctx.to_string())
            // Quieter logs — llama.cpp is chatty by default.
            .arg("--log-disable")
            .arg("--no-webui")
            // Pooling matters for sentence-style embeddings; mean is a
            // reasonable default for EmbeddingGemma's encoder output.
            .arg("--pooling")
            .arg("mean")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawn llama-server")?;

        let base_url = format!("http://127.0.0.1:{port}");
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("build http client")?;

        let server = Self {
            child,
            base_url: base_url.clone(),
            http,
            embed_dim: config.embed_dim,
        };
        server.wait_ready().await?;
        Ok(server)
    }

    async fn wait_ready(&self) -> Result<()> {
        let url = format!("{}/health", self.base_url);
        let started = std::time::Instant::now();
        // last_err is overwritten on every loop iteration that fails, then
        // surfaced via bail!() if the loop times out. Initial value is an
        // unreachable safety net.
        #[allow(unused_assignments)]
        let mut last_err = String::from("never reached");
        loop {
            match self.http.get(&url).send().await {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => last_err = format!("status {}", r.status()),
                Err(e) => last_err = e.to_string(),
            }
            if started.elapsed().as_millis() as u64 > READY_TIMEOUT_MS {
                return Err(Error::ServerTimeout {
                    timeout_ms: READY_TIMEOUT_MS,
                    detail: last_err,
                }
                .into());
            }
            tokio::time::sleep(Duration::from_millis(READY_POLL_MS)).await;
        }
    }

    /// Single-input convenience.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed_batch(&[text.to_string()]).await?;
        Ok(out.remove(0))
    }

    /// Batch embedding via the OpenAI-compatible endpoint.
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = json!({
            "model": "embedding-gemma",  // server ignores; field required by OpenAI shape
            "input": texts,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("embedder returned {status}: {text}");
        }
        #[derive(serde::Deserialize)]
        struct EmbeddingItem {
            embedding: Vec<f32>,
        }
        #[derive(serde::Deserialize)]
        struct EmbResponse {
            data: Vec<EmbeddingItem>,
        }
        let parsed: EmbResponse = resp.json().await?;
        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(parsed.data.len());
        for item in parsed.data {
            if item.embedding.len() != self.embed_dim {
                return Err(Error::DimensionMismatch {
                    expected: self.embed_dim,
                    actual: item.embedding.len(),
                }
                .into());
            }
            vectors.push(item.embedding);
        }
        Ok(vectors)
    }

    pub async fn shutdown(mut self) {
        // Try graceful kill; kill_on_drop is the safety net.
        let _ = self.child.kill().await;
    }
}

fn pick_free_port(floor: u16) -> Result<u16> {
    // OS-picked free port is most reliable. We bind to 0 then read back.
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .with_context(|| "bind for port discovery")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    // floor is advisory; if the OS gave us something below it that's still fine.
    let _ = floor;
    Ok(port)
}

fn is_on_path(bin: &std::path::Path) -> bool {
    // Treat a relative name (no dir component) as "look on PATH".
    bin.parent()
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true)
}
