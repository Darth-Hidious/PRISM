// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Native local backend — BGE-small-en-v1.5 on the bundled ONNX Runtime.
//!
//! No server, no API key, and no model acquisition during inference. The
//! pinned snapshot must be installed explicitly before this backend is built.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};

use crate::manifest::{VerifiedSnapshot, open_verified_snapshot};
use crate::{BGE_SMALL_EN_V15_MANIFEST, EmbedBackend, default_cache_dir};

const MODEL: EmbeddingModel = EmbeddingModel::BGESmallENV15;
const FALLBACK_DIM: usize = 384;

/// Local ONNX embedding backend. Construction is blocking (it verifies and
/// reads the installed snapshot) and fallible — callers treat `Err` as
/// "semantic search unavailable", never as a startup failure.
pub struct NativeOnnx {
    // `TextEmbedding::embed` takes `&mut self`; the Mutex serializes calls
    // and the Arc lets `embed()` move the model into `spawn_blocking`.
    model: Arc<Mutex<TextEmbedding>>,
    dim: usize,
    id: String,
}

impl NativeOnnx {
    /// Build with the default cache dir. Blocking; returns `Err` when the
    /// pinned model is absent or invalid. This never downloads model files.
    pub fn new() -> Result<Self> {
        Self::with_cache_dir(default_cache_dir()?)
    }

    /// Build with an explicit cache dir (tests, custom layouts).
    pub fn with_cache_dir(cache_dir: std::path::PathBuf) -> Result<Self> {
        let model = with_verified_snapshot(&cache_dir, build_model_from_snapshot)?;
        let (dim, model_code) = TextEmbedding::get_model_info(&MODEL)
            .map(|info| (info.dim, info.model_code.clone()))
            .unwrap_or((
                FALLBACK_DIM,
                BGE_SMALL_EN_V15_MANIFEST.repository.to_string(),
            ));
        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            dim,
            id: format!("native:{model_code}@{}", BGE_SMALL_EN_V15_MANIFEST.revision),
        })
    }
}

fn with_verified_snapshot<T>(
    cache_dir: &Path,
    initialize: impl FnOnce(VerifiedSnapshot) -> Result<T>,
) -> Result<T> {
    initialize(open_verified_snapshot(
        cache_dir,
        &BGE_SMALL_EN_V15_MANIFEST,
    )?)
}

/// Load fastembed through its user-supplied-files API. Unlike `try_new`, this
/// API has no Hugging Face client and therefore cannot download, even when
/// `HF_HOME` or `HF_ENDPOINT` is configured in the process environment.
fn build_model_from_snapshot(mut snapshot: VerifiedSnapshot) -> Result<TextEmbedding> {
    let snapshot_path = snapshot.snapshot_dir.display().to_string();
    let mut take = |relative: &str| {
        snapshot
            .take(relative)
            .with_context(|| format!("verified snapshot {snapshot_path} did not retain {relative}"))
    };
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: take("tokenizer.json")?,
        config_file: take("config.json")?,
        special_tokens_map_file: take("special_tokens_map.json")?,
        tokenizer_config_file: take("tokenizer_config.json")?,
    };
    let mut model = UserDefinedEmbeddingModel::new(take("onnx/model.onnx")?, tokenizer_files)
        .with_quantization(TextEmbedding::get_quantization_mode(&MODEL));
    if let Some(pooling) = TextEmbedding::get_default_pooling_method(&MODEL) {
        model = model.with_pooling(pooling);
    }
    if let Ok(info) = TextEmbedding::get_model_info(&MODEL) {
        model.output_key = info.output_key.clone();
    }
    TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::new())
        .context("failed to initialize the verified native embedding snapshot")
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
    use crate::BGE_INSTALL_COMMAND;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn missing_snapshot_refuses_before_model_initialization() {
        let dir = tempfile::tempdir().expect("tempdir");
        let initialized = AtomicBool::new(false);
        let err = with_verified_snapshot(dir.path(), |_| {
            initialized.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect_err("an absent snapshot must be refused");
        let msg = format!("{err:#}");
        assert!(!initialized.load(Ordering::SeqCst));
        assert!(
            msg.contains(BGE_INSTALL_COMMAND),
            "the refusal must provide explicit setup: {msg}"
        );
        assert!(
            msg.contains("never downloads"),
            "the runtime acquisition policy must be visible: {msg}"
        );
    }

    #[test]
    fn mismatched_snapshot_refuses_before_model_initialization() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reference = dir
            .path()
            .join("models--Xenova--bge-small-en-v1.5/refs/main");
        std::fs::create_dir_all(reference.parent().unwrap()).unwrap();
        std::fs::write(&reference, "not-the-pinned-revision").unwrap();
        let initialized = AtomicBool::new(false);
        let err = with_verified_snapshot(dir.path(), |_| {
            initialized.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect_err("a mismatched snapshot must be refused");
        assert!(!initialized.load(Ordering::SeqCst));
        assert!(format!("{err:#}").contains("revision mismatch"));
    }
}
