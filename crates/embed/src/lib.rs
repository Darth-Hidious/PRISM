// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PRISM embedding port — native by default, pluggable by design.
//!
//! Semantic memory needs vectors; where those vectors come from is a
//! deployment decision, not an architectural one. This crate defines the
//! [`EmbedBackend`] port and ships two adapters:
//!
//! - [`NativeOnnx`] (**default**): fully local, BGE-small-en-v1.5 (384-dim)
//!   running on the bundled ONNX Runtime via `fastembed`. Runtime inference
//!   never downloads weights: the exact snapshot in
//!   [`BGE_SMALL_EN_V15_MANIFEST`] must be installed explicitly first.
//!   **Not available on Intel macOS** — ONNX Runtime ships no
//!   `x86_64-apple-darwin` build, so it is not compiled in there and the
//!   `native` choice degrades to keyword-only search.
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
//! verified (missing, modified, wrong revision) or the openai config is incomplete,
//! [`from_config`] returns `None` and callers degrade to keyword-only search.
//! [`from_config_with_status`] preserves an explicit caller-readable reason.
//! Nothing here panics or performs model acquisition. Native construction
//! does hash and load the installed snapshot, so call it from a
//! blocking-friendly context (e.g. `tokio::task::spawn_blocking`).

use anyhow::Result;
use async_trait::async_trait;

// Intel macOS is the one target without a native backend: ONNX Runtime
// publishes no `x86_64-apple-darwin` binaries, so `fastembed` is excluded
// there (see this crate's Cargo.toml). Everything else compiles it in.
mod manifest;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod native;
mod openai;

pub use manifest::{
    BGE_INSTALL_COMMAND, BGE_SMALL_EN_V15_MANIFEST, ModelArtifact, ModelSnapshotManifest,
    NativeModelStatus, NativeModelUnavailable, NativeModelUnavailableCode, native_model_status_at,
    verify_snapshot_at,
};
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub use native::NativeOnnx;

/// `~/.prism/models/embed/` — the single shared cache location used by
/// runtime, status, doctor, and explicit installation on every platform.
pub fn default_cache_dir() -> Result<std::path::PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| {
            anyhow::anyhow!("cannot resolve home directory for the embedding model cache")
        })?
        .join(".prism/models/embed"))
}
pub use openai::OpenAiCompat;

/// Inspect the default native model location without initializing ONNX.
pub fn native_model_status() -> NativeModelStatus {
    match default_cache_dir() {
        Ok(cache_dir) => native_model_status_at(&cache_dir),
        Err(err) => NativeModelStatus::Unavailable(NativeModelUnavailable {
            code: NativeModelUnavailableCode::CacheUnavailable,
            detail: err.to_string(),
            install_command: BGE_INSTALL_COMMAND,
        }),
    }
}

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

/// Configuration-only backend usability. Unlike [`BackendInit`], this never
/// loads ONNX weights and never contacts a hosted endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfiguredBackendStatus {
    NativeSupported,
    NativeUnsupported { reason: String },
    HostedReady { backend_id: String },
    HostedUnavailable { reason: String },
    Disabled,
}

/// Backend configuration and local snapshot integrity are deliberately
/// separate facts. A hosted backend does not require local weights, while a
/// verified snapshot cannot make native ONNX usable on Intel macOS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfigurationStatus {
    pub backend: ConfiguredBackendStatus,
    pub native_snapshot: NativeModelStatus,
}

/// Result of configured backend initialization without discarding why it was
/// unavailable.
pub enum BackendInit {
    Ready(Box<dyn EmbedBackend>),
    Disabled,
    Unavailable {
        backend: BackendChoice,
        reason: String,
    },
}

/// Stable high-level state for callers that do not need the backend object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendInitStatus {
    Ready,
    Disabled,
    Unavailable,
}

impl BackendInit {
    pub fn status(&self) -> BackendInitStatus {
        match self {
            Self::Ready(_) => BackendInitStatus::Ready,
            Self::Disabled => BackendInitStatus::Disabled,
            Self::Unavailable { .. } => BackendInitStatus::Unavailable,
        }
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable { reason, .. } => Some(reason),
            Self::Ready(_) | Self::Disabled => None,
        }
    }

    pub fn into_backend(self) -> Option<Box<dyn EmbedBackend>> {
        match self {
            Self::Ready(backend) => Some(backend),
            Self::Disabled | Self::Unavailable { .. } => None,
        }
    }
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

/// Whether this build includes the native ONNX backend.
#[must_use]
pub const fn native_backend_supported() -> bool {
    !cfg!(all(target_os = "macos", target_arch = "x86_64"))
}

/// Inspect configured usability and local snapshot integrity without model
/// initialization, acquisition, or network I/O.
pub fn configured_embedding_status() -> EmbeddingConfigurationStatus {
    let env = std::env::var("PRISM_EMBED_BACKEND").ok();
    let file = load_file_config();
    let backend = match choose_backend(env.as_deref(), file.backend.as_deref()) {
        BackendChoice::Off => ConfiguredBackendStatus::Disabled,
        BackendChoice::OpenAi => match OpenAiCompat::from_config(&file) {
            Ok(backend) => ConfiguredBackendStatus::HostedReady {
                backend_id: backend.id().to_string(),
            },
            Err(err) => ConfiguredBackendStatus::HostedUnavailable {
                reason: format!("{err:#}"),
            },
        },
        BackendChoice::Native if native_backend_supported() => {
            ConfiguredBackendStatus::NativeSupported
        }
        BackendChoice::Native => ConfiguredBackendStatus::NativeUnsupported {
            reason: "native embeddings are not available on Intel macOS (no ONNX Runtime build for x86_64-apple-darwin); set PRISM_EMBED_BACKEND=openai for semantic search".to_string(),
        },
    };
    EmbeddingConfigurationStatus {
        backend,
        native_snapshot: native_model_status(),
    }
}

/// Build the configured backend, or `None` when embedding is disabled or
/// unavailable. Never panics; failures are logged and swallowed so callers
/// can degrade to keyword-only search.
///
/// May block while verifying and loading an installed native snapshot. It
/// never downloads model files. Use [`from_config_with_status`] when the
/// fallback reason must be shown to a caller.
pub fn from_config() -> Option<Box<dyn EmbedBackend>> {
    let init = from_config_with_status();
    if let BackendInit::Unavailable { backend, reason } = &init {
        tracing::warn!(
            "{} embedding backend unavailable: {reason} — semantic search disabled",
            match backend {
                BackendChoice::Native => "native",
                BackendChoice::OpenAi => "openai",
                BackendChoice::Off => "disabled",
            }
        );
    }
    init.into_backend()
}

/// Build the configured backend while preserving disabled vs unavailable and
/// an actionable failure reason. Like [`from_config`], this never downloads
/// model files.
pub fn from_config_with_status() -> BackendInit {
    let env = std::env::var("PRISM_EMBED_BACKEND").ok();
    let file = load_file_config();
    match choose_backend(env.as_deref(), file.backend.as_deref()) {
        BackendChoice::Off => BackendInit::Disabled,
        BackendChoice::OpenAi => match OpenAiCompat::from_config(&file) {
            Ok(backend) => BackendInit::Ready(Box::new(backend)),
            Err(err) => BackendInit::Unavailable {
                backend: BackendChoice::OpenAi,
                reason: format!("{err:#}"),
            },
        },
        BackendChoice::Native => native_backend_init(),
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn native_backend_init() -> BackendInit {
    match NativeOnnx::new() {
        Ok(backend) => BackendInit::Ready(Box::new(backend)),
        Err(err) => BackendInit::Unavailable {
            backend: BackendChoice::Native,
            reason: format!("{err:#}"),
        },
    }
}

/// Intel macOS: the native backend is not compiled in (no ONNX Runtime build
/// exists for `x86_64-apple-darwin`). Set `PRISM_EMBED_BACKEND=openai` plus
/// `PRISM_EMBED_ENDPOINT_URL` for semantic search; otherwise search stays
/// keyword-only, which is what `None` means to every caller.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn native_backend_init() -> BackendInit {
    BackendInit::Unavailable {
        backend: BackendChoice::Native,
        reason: "native embeddings are not available on Intel macOS (no ONNX Runtime build for \
                 x86_64-apple-darwin); set PRISM_EMBED_BACKEND=openai for semantic search"
            .to_string(),
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
