// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
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

/// Is the model already in `cache_dir`?
///
/// `hf-hub` lays a repo out as `models--<org>--<name>` — verified empirically
/// against this machine's cache (`models--Xenova--bge-small-en-v1.5`), not
/// assumed from the crate docs.
///
/// KNOWN RESIDUAL, deliberately not papered over: this proves the repo
/// directory exists, not that every weight file inside is complete, and it does
/// not rule out `hf-hub` issuing a revision/etag HEAD request against a warm
/// cache. Confirming that would require letting it talk to huggingface.co,
/// which is exactly what this guard exists to prevent, so it is recorded rather
/// than claimed. The cold-cache case — the ~90 MB download that carries the
/// token — is closed either way.
fn model_is_cached(cache_dir: &std::path::Path, model_code: &str) -> bool {
    cache_dir
        .join(format!("models--{}", model_code.replace('/', "--")))
        .is_dir()
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

        // Hard offline: never let `fastembed` fetch the weights.
        //
        // This bypassed the policy structurally, not by omission. `fastembed`
        // does not use any client PRISM constructs — on a cache miss it calls
        // `hf-hub`, which opens its OWN connection to huggingface.co and
        // unconditionally attaches `~/.cache/huggingface/token` as a Bearer if
        // that file exists. So neither a `reqwest` audit nor a subprocess audit
        // could see it, and `hf-hub` honours no `HF_HUB_OFFLINE` escape (checked
        // its source — the variable does not appear).
        //
        // Reached from background work too, not just interactive paths:
        // `agent_loop.rs:1279` and `hooks.rs:558` embed inside `tokio::spawn`,
        // after the turn that triggered them has already returned.
        //
        // A WARM cache still works, which is the module's stated contract ("no
        // network after the first model download") and the whole point of local
        // embedding offline. Only the download is refused.
        if prism_runtime::offline::enabled() && !model_is_cached(&cache_dir, &model_code) {
            anyhow::bail!(
                "offline mode: the native embedding model is not cached at {} \
                 and downloading it would fetch ~90 MB from huggingface.co \
                 (sending your HF token if you have one). Run once online, or \
                 use an endpoint-based embedding backend.",
                cache_dir.display()
            );
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A cold cache must not become a 90 MB download under hard offline.
    ///
    /// Only the COLD path is driven end-to-end: it returns before
    /// `TextEmbedding::try_new`, so nothing can reach the network. The warm
    /// path is asserted through `model_is_cached` instead — calling
    /// `with_cache_dir` on a directory that merely LOOKS warm would hand
    /// control to `fastembed`, which is what we are trying not to do in a test.
    #[test]
    fn a_cold_cache_is_not_downloaded_offline_and_a_warm_one_is_recognised() {
        let _lock = prism_runtime::offline::test_support::env_lock();
        let _on = prism_runtime::offline::test_support::OfflineEnvGuard::set("1");

        let dir = tempfile::tempdir().expect("tempdir");
        // `match`, not `expect_err`: `NativeOnnx` holds a `TextEmbedding` and
        // does not implement `Debug`, which `expect_err` requires of the Ok
        // type.
        let err = match NativeOnnx::with_cache_dir(dir.path().to_path_buf()) {
            Ok(_) => panic!("a cold cache must be refused offline"),
            Err(err) => err,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("offline mode"),
            "must be a POLICY refusal: {msg}"
        );
        assert!(
            msg.contains("huggingface.co"),
            "the refusal should name where it declined to go: {msg}"
        );

        // The cache check itself, both directions — this is what decides
        // whether a warm cache still works offline.
        assert!(!model_is_cached(dir.path(), "Xenova/bge-small-en-v1.5"));
        std::fs::create_dir_all(dir.path().join("models--Xenova--bge-small-en-v1.5"))
            .expect("mkdir");
        assert!(
            model_is_cached(dir.path(), "Xenova/bge-small-en-v1.5"),
            "a warm cache must be recognised, or offline would break local embedding"
        );
    }
}
