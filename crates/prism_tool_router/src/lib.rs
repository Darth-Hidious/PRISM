//! PRISM tool router — local semantic retrieval + routing layer.
//!
//! Sits between forge's agent loop and the chat LLM. Two stages, both
//! served by `llama-server` subprocesses on free localhost ports:
//!
//!   1. **EmbeddingGemma-300M** (GGUF) — embeds tool descriptions and the
//!      user's latest message into a shared 768-dim space; cosine search
//!      returns top-K=8 candidate tools per query.
//!
//!   2. **FunctionGemma-270M** (GGUF, optionally LoRA-fine-tuned on a
//!      PRISM-specific corpus) — given (query, top-K tool schemas) emits
//!      either a concrete tool call or a passthrough signal.
//!
//! Stage 2 lands in this order:
//!   - 2.1: index + retrieval (this crate)
//!   - 2.2: FunctionGemma routing (this crate, gated by feature later)
//!   - 2.3: fine-tune workflow (Colab notebook, separate)

pub mod config;
pub mod download;
pub mod error;
pub mod function;
pub mod index;
pub mod routing;
pub mod server;

pub use config::{Config, ModelRemote};
pub use download::ensure_model;
pub use error::Error;
pub use function::FunctionServer;
pub use index::{ToolDef, ToolIndex};
pub use routing::{RoutingDecision, ToolCall};
pub use server::EmbedderServer;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Top-level handle. Owns the two llama-server subprocesses (embedder +
/// function router) and the persistent tool index.
pub struct ToolRouter {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    config: Config,
    embedder: Option<EmbedderServer>,
    function: Option<FunctionServer>,
    index: ToolIndex,
}

impl ToolRouter {
    pub async fn new(config: Config) -> Result<Self> {
        let index = ToolIndex::load_or_init(&config)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                embedder: None,
                function: None,
                index,
            })),
        })
    }

    /// Spawn the embedder subprocess and wait for readiness. Idempotent.
    pub async fn start(&self) -> Result<()> {
        let mut g = self.inner.lock().await;
        if g.embedder.is_some() {
            return Ok(());
        }
        let server = EmbedderServer::spawn(&g.config).await?;
        g.embedder = Some(server);
        Ok(())
    }

    /// Spawn the FunctionGemma router subprocess. Optional — if this is
    /// not called, `route()` falls through to `RoutingDecision::Passthrough`
    /// for every query. Idempotent.
    pub async fn start_function_router(&self) -> Result<()> {
        let mut g = self.inner.lock().await;
        if g.function.is_some() {
            return Ok(());
        }
        let server = FunctionServer::spawn(&g.config).await?;
        g.function = Some(server);
        Ok(())
    }

    /// Run FunctionGemma against the user's last query with the given tool
    /// schemas (typically the top-K returned by `search`). Returns
    /// `Invoke(tool_call)` when the model emits a parseable function call,
    /// `Passthrough` otherwise (including: function router not started,
    /// model output not a call, model errored).
    pub async fn route(
        &self,
        user_query: &str,
        tool_schemas: &[serde_json::Value],
    ) -> RoutingDecision {
        let g = self.inner.lock().await;
        let server = match g.function.as_ref() {
            Some(s) => s,
            None => return RoutingDecision::Passthrough,
        };
        match server.route(user_query, tool_schemas).await {
            Ok(Some(call)) => RoutingDecision::Invoke(call),
            Ok(None) => RoutingDecision::Passthrough,
            Err(e) => {
                eprintln!("[prism_tool_router] function-router error: {e:#}");
                RoutingDecision::Passthrough
            }
        }
    }

    /// Ensure every tool in `tools` is embedded. Hashes (name|description)
    /// are checked against the on-disk catalog; only missing/changed tools
    /// are sent to the embedder. Returns the number of newly-embedded tools.
    ///
    /// Per-tool failures (e.g. one tool's description exceeds the batch
    /// size) are isolated — the offending tool is skipped, others get
    /// embedded. We retry one-at-a-time on batch failure so a single bad
    /// row can't sink the whole index update.
    pub async fn index_tools(&self, tools: &[ToolDef]) -> Result<usize> {
        // Two-phase to keep the lock fine-grained: phase 1 reads the index
        // and clones the embedder reference, phase 2 mutates the index with
        // the results. Inner mutex re-acquired between phases so we don't
        // hold the embedder borrow across mutable index updates.
        let to_embed: Vec<ToolDef> = {
            let g = self.inner.lock().await;
            tools
                .iter()
                .filter(|t| !g.index.has_current(t))
                .cloned()
                .collect()
        };
        if to_embed.is_empty() {
            return Ok(0);
        }

        let texts: Vec<String> = to_embed.iter().map(|t| t.embed_text()).collect();

        // Borrow embedder for the network call; clone the Child-less HTTP
        // wrapper out by holding the lock only as long as needed.
        let batch_result = {
            let g = self.inner.lock().await;
            let server = g
                .embedder
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("router not started"))?;
            server.embed_batch(&texts).await
        };

        let mut tool_vec_pairs: Vec<(ToolDef, Vec<f32>)> = Vec::new();

        match batch_result {
            Ok(vectors) => {
                for (tool, vec) in to_embed.into_iter().zip(vectors) {
                    tool_vec_pairs.push((tool, vec));
                }
            }
            Err(_batch_err) => {
                // Batch failed (almost always: one over-long input rejected
                // the whole batch). Retry per-tool so we make progress on
                // the rest.
                for tool in to_embed {
                    let text = tool.embed_text();
                    let single = {
                        let g = self.inner.lock().await;
                        let server = g
                            .embedder
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("router not started"))?;
                        server.embed_one(&text).await
                    };
                    match single {
                        Ok(vec) => tool_vec_pairs.push((tool, vec)),
                        Err(e) => {
                            tracing::warn!(
                                target: "prism_tool_router",
                                tool = %tool.name,
                                error = %e,
                                "skipping over-long or otherwise unembeddable tool"
                            );
                        }
                    }
                }
            }
        }

        let embedded = tool_vec_pairs.len();
        if embedded > 0 {
            let mut g = self.inner.lock().await;
            for (tool, vec) in tool_vec_pairs {
                g.index.upsert(tool, vec);
            }
            g.index.persist(&g.config)?;
        }
        Ok(embedded)
    }

    /// Top-K tool names by cosine similarity to `query`. Restricts results
    /// to the names in `available` so callers can pass forge's per-turn
    /// tool list and get back a subset of names from THAT list (not from
    /// the global index).
    pub async fn search(&self, query: &str, available: &[String], k: usize) -> Result<Vec<String>> {
        let g = self.inner.lock().await;
        let server = g
            .embedder
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("router not started"))?;
        let qv = server.embed_one(query).await?;
        Ok(g.index.top_k_filtered(&qv, available, k))
    }

    pub async fn shutdown(self) {
        let mut g = self.inner.lock().await;
        if let Some(server) = g.embedder.take() {
            server.shutdown().await;
        }
        if let Some(server) = g.function.take() {
            server.shutdown().await;
        }
    }
}
