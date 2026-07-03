// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Native local backend — BGE-small-en-v1.5 on the bundled ONNX Runtime.
//!
//! No server, no API key, no network after the first model download.
//! Weights are cached under `~/.prism/models/embed/` (~90 MB, once).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::EmbedBackend;

const MODEL: EmbeddingModel = EmbeddingModel::BGESmallENV15;
const FALLBACK_DIM: usize = 384;

/// Local ONNX embedding backend. Construction is blocking (may download the
/// model on first ever use) and fallible — callers treat `Err` as "semantic
/// search unavailable", never as a startup failure.
pub struct NativeOnnx {
    // `TextEmbedding::embed` takes `&mut self`; the Mutex serializes calls
    // and the Arc lets `embed()` move the model into `spawn_blocking`.
    model: Arc<Mutex<TextEmbedding>>,
    dim: usize,
    id: String,
}

/// `~/.prism/models/embed/` — PRISM-owned cache, independent of `HF_HOME`.
pub fn default_cache_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("cannot resolve home directory for the embedding model cache")?
        .join(".prism/models/embed"))
}

impl NativeOnnx {
    /// Build with the default cache dir. Blocking; returns `Err` when the
    /// model is absent and cannot be downloaded (offline, no disk).
    pub fn new() -> Result<Self> {
        Self::with_cache_dir(default_cache_dir()?)
    }

    /// Build with an explicit cache dir (tests, custom layouts).
    pub fn with_cache_dir(cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir).with_context(|| {
            format!(
                "cannot create embedding model cache at {}",
                cache_dir.display()
            )
        })?;
        let (dim, model_code) = TextEmbedding::get_model_info(&MODEL)
            .map(|info| (info.dim, info.model_code.clone()))
            .unwrap_or((FALLBACK_DIM, "bge-small-en-v1.5".to_string()));
        let model = TextEmbedding::try_new(
            TextInitOptions::new(MODEL)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(false),
        )
        .context("failed to initialize the native embedding model (offline and not yet cached?)")?;
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dim,
            id: format!("native:{model_code}"),
        })
    }
}

#[async_trait]
impl EmbedBackend for NativeOnnx {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // ONNX inference is CPU-bound; keep it off the async workers.
        let model = Arc::clone(&self.model);
        let texts = texts.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut m = model
                .lock()
                .map_err(|_| anyhow::anyhow!("embedding model mutex poisoned"))?;
            m.embed(&texts, None)
        })
        .await
        .context("embedding task panicked")?
    }

    fn dimensions(&self) -> usize {
        self.dim
    }

    fn id(&self) -> &str {
        &self.id
    }
}
