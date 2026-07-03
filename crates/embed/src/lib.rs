// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! PRISM embedding port — native by default, pluggable by design.
//!
//! Semantic memory needs vectors; where those vectors come from is a
//! deployment decision, not an architectural one. This crate defines the
//! [`EmbedBackend`] port and ships two adapters:
//!
//! - [`NativeOnnx`] (**default**): fully local, BGE-small-en-v1.5 (384-dim)
//!   running on the bundled ONNX Runtime via `fastembed`. First use downloads
//!   ~90 MB into `~/.prism/models/embed/`; afterwards it is fully offline.
//! - [`OpenAiCompat`]: any `/v1/embeddings`-shaped HTTP endpoint (hosted
//!   provider, Hugging Face TEI, or the MARC27 API).
//!
//! # Selection contract ([`from_config`])
//!
//! 1. Env `PRISM_EMBED_BACKEND` = `native` | `openai` | `off`
//! 2. Else `~/.prism/prism.toml`:
//!    ```toml
//!    [embedding]
//!    backend = "native"        # or "openai" / "off"
//!    endpoint_url = "https://…" # openai backend only (env wins)
//!    model = "…"                # openai backend only (env wins)
//!    api_key = "…"              # openai backend only (env wins)
//!    ```
//! 3. Unset → `native`.
//!
//! OpenAI-compat parameters come from `PRISM_EMBED_ENDPOINT_URL`,
//! `PRISM_EMBED_MODEL`, `PRISM_EMBED_API_KEY`, each falling back to the
//! `[embedding]` keys above.
//!
//! # Failure model
//!
//! Construction is fallible but never fatal: if the native model cannot be
//! downloaded (offline, no disk) or the openai config is incomplete,
//! [`from_config`] returns `None` and callers degrade to keyword-only search.
//! Nothing here panics and nothing blocks startup — but note that
//! [`from_config`] itself may block for the initial model download, so call
//! it from a blocking-friendly context (e.g. `tokio::task::spawn_blocking`).

use anyhow::Result;
use async_trait::async_trait;

mod native;
mod openai;

pub use native::NativeOnnx;
pub use openai::OpenAiCompat;

/// The embedding port. Implementations must be cheap to share (`Arc`) and
/// safe to call concurrently.
#[async_trait]
pub trait EmbedBackend: Send + Sync {
    /// Embed a batch of texts. Returns one vector per input, in order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Vector dimensionality. May be `0` for remote backends that have not
    /// served a request yet (learned from the first response).
    fn dimensions(&self) -> usize;

    /// Stable identifier (`backend:model`), stored alongside vectors so
    /// mixed-model stores can be filtered.
    fn id(&self) -> &str;
}

// ── Backend selection ───────────────────────────────────────────────

/// Which backend the configuration asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    Native,
    OpenAi,
    Off,
}

/// Pure selection logic: env value wins over config-file value; unset or
/// unrecognized values fall back to the local-first default (`Native`).
pub fn choose_backend(env_value: Option<&str>, file_value: Option<&str>) -> BackendChoice {
    let raw = env_value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| file_value.map(str::trim).filter(|s| !s.is_empty()));
    match raw.map(str::to_ascii_lowercase).as_deref() {
        Some("openai") => BackendChoice::OpenAi,
        Some("off") | Some("none") | Some("disabled") => BackendChoice::Off,
        Some("native") | None => BackendChoice::Native,
        Some(other) => {
            tracing::warn!("unknown embedding backend '{other}' — using native");
            BackendChoice::Native
        }
    }
}

/// `[embedding]` section of `~/.prism/prism.toml` (all keys optional).
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct EmbedFileConfig {
    pub backend: Option<String>,
    pub endpoint_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

fn load_file_config() -> EmbedFileConfig {
    let Some(path) = dirs::home_dir().map(|h| h.join(".prism/prism.toml")) else {
        return EmbedFileConfig::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return EmbedFileConfig::default();
    };
    #[derive(serde::Deserialize)]
    struct Root {
        embedding: Option<EmbedFileConfig>,
    }
    toml::from_str::<Root>(&text)
        .ok()
        .and_then(|r| r.embedding)
        .unwrap_or_default()
}

/// Build the configured backend, or `None` when embedding is disabled or
/// unavailable. Never panics; failures are logged and swallowed so callers
/// can degrade to keyword-only search.
///
/// May block (native model download on first ever use) — call from a
/// blocking-friendly context.
pub fn from_config() -> Option<Box<dyn EmbedBackend>> {
    let env = std::env::var("PRISM_EMBED_BACKEND").ok();
    let file = load_file_config();
    match choose_backend(env.as_deref(), file.backend.as_deref()) {
        BackendChoice::Off => None,
        BackendChoice::OpenAi => match OpenAiCompat::from_config(&file) {
            Ok(b) => Some(Box::new(b)),
            Err(e) => {
                tracing::warn!(
                    "openai embedding backend unavailable: {e:#} — semantic search disabled"
                );
                None
            }
        },
        BackendChoice::Native => match NativeOnnx::new() {
            Ok(b) => Some(Box::new(b)),
            Err(e) => {
                tracing::warn!(
                    "native embedding backend unavailable: {e:#} — semantic search disabled"
                );
                None
            }
        },
    }
}

// ── Vector helpers ──────────────────────────────────────────────────

/// Cosine similarity in `[-1, 1]`. Mismatched lengths or zero vectors → `0.0`.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Serialize an embedding as a little-endian `f32` blob (storage format for
/// the provenance `vector` column).
pub fn vec_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Inverse of [`vec_to_le_bytes`]. Trailing partial floats are dropped.
pub fn le_bytes_to_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_backend_env_wins_over_file() {
        assert_eq!(
            choose_backend(Some("openai"), Some("native")),
            BackendChoice::OpenAi
        );
        assert_eq!(
            choose_backend(Some("off"), Some("openai")),
            BackendChoice::Off
        );
    }

    #[test]
    fn choose_backend_falls_back_to_file_then_default() {
        assert_eq!(choose_backend(None, Some("openai")), BackendChoice::OpenAi);
        assert_eq!(choose_backend(None, Some("off")), BackendChoice::Off);
        assert_eq!(choose_backend(None, None), BackendChoice::Native);
        // Empty strings are "unset", not a choice.
        assert_eq!(choose_backend(Some(""), Some(" ")), BackendChoice::Native);
    }

    #[test]
    fn choose_backend_is_case_insensitive_and_safe_on_garbage() {
        assert_eq!(choose_backend(Some("OpenAI"), None), BackendChoice::OpenAi);
        assert_eq!(choose_backend(Some("OFF"), None), BackendChoice::Off);
        assert_eq!(
            choose_backend(Some("quantum-flux"), None),
            BackendChoice::Native
        );
    }

    #[test]
    fn cosine_basics() {
        let a = [1.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let c = [0.0, 1.0, 0.0];
        let d = [-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &c).abs() < 1e-6);
        assert!((cosine_similarity(&a, &d) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_degenerate_inputs_are_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn blob_roundtrip() {
        let v = vec![0.25f32, -1.5, 3.75, f32::MIN_POSITIVE];
        assert_eq!(le_bytes_to_vec(&vec_to_le_bytes(&v)), v);
        // Trailing garbage byte is dropped, not an error.
        let mut bytes = vec_to_le_bytes(&v);
        bytes.push(0xFF);
        assert_eq!(le_bytes_to_vec(&bytes), v);
    }
}
