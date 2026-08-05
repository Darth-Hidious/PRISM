// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Embedded GGUF adapter: model resolution is always available; inference is opt-in.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::ChatMessage;

/// Explicit base URL sentinel selecting embedded GGUF inference.
pub const LOCAL_GGUF_URL: &str = "gguf://local";

/// The generation-model directory. Embeddings use its `embed/` child.
pub fn default_model_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory for local GGUF models"))?
        .join(".prism/models"))
}

/// Whether a configured endpoint explicitly selects embedded inference.
#[must_use]
pub fn is_local_gguf_url(base_url: &str) -> bool {
    base_url.trim_end_matches('/') == LOCAL_GGUF_URL
}

/// Resolve an absolute/relative path directly, or a model name below
/// `~/.prism/models`. No download or remote fallback is attempted.
pub fn resolve_model_path(model: &str) -> Result<PathBuf> {
    resolve_model_path_in(model, &default_model_dir()?)
}

fn resolve_model_path_in(model: &str, model_dir: &Path) -> Result<PathBuf> {
    let requested = model.trim();
    let expanded = requested
        .strip_prefix("~/")
        .and_then(|rest| dirs::home_dir().map(|home| home.join(rest)))
        .unwrap_or_else(|| PathBuf::from(requested));
    let path_like =
        expanded.is_absolute() || requested.starts_with('.') || expanded.components().count() > 1;

    let mut searched = if path_like {
        vec![expanded]
    } else {
        let mut candidates = vec![model_dir.join(&expanded)];
        if expanded.extension().is_none() && !requested.is_empty() {
            candidates.push(model_dir.join(format!("{requested}.gguf")));
        }
        candidates
    };
    searched.dedup();

    if let Some(found) = searched
        .iter()
        .find(|path| path.is_file() && has_gguf_extension(path))
    {
        return Ok(found.clone());
    }

    bail!(missing_weights_refusal(requested, model_dir, &searched))
}

fn has_gguf_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
}

fn missing_weights_refusal(requested: &str, model_dir: &Path, searched: &[PathBuf]) -> String {
    let searched: Vec<String> = searched
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let body = serde_json::json!({
        "status": "refused",
        "error": format!("No readable local GGUF weights were found for {requested:?}"),
        "refusal": {
            "code": "local_llm_weights_unavailable",
            "requested_model": requested,
            "model_directory": model_dir.display().to_string(),
            "searched": searched,
            "expected_format": ".gguf"
        },
        "install_hint": format!(
            "Place a user-obtained .gguf model in {} or configure an explicit .gguf path. PRISM does not download or substitute model weights.",
            model_dir.display()
        )
    });
    format!(
        "local GGUF inference refused:\n{}",
        serde_json::to_string_pretty(&body).expect("JSON values above are serializable")
    )
}

/// Error returned when `gguf://local` is selected in a build without llama.cpp.
#[cfg(not(feature = "local-inference"))]
pub fn feature_disabled_error() -> anyhow::Error {
    anyhow::anyhow!(
        "local GGUF inference was selected with {LOCAL_GGUF_URL}, but this PRISM binary was built without embedded inference. Rebuild with `cargo build -p prism-cli --features local-inference`. No remote endpoint was tried."
    )
}

/// Local adapter state. The model is loaded once and shared; every request gets
/// a fresh context/KV cache so conversation state remains explicit in messages.
pub(crate) struct LocalGguf {
    model_spec: String,
    #[cfg(feature = "local-inference")]
    context_size: u32,
    #[cfg(feature = "local-inference")]
    model: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<llama_cpp_2::model::LlamaModel>>>,
}

impl LocalGguf {
    pub(crate) fn new(model_spec: String) -> Self {
        #[cfg(feature = "local-inference")]
        let context_size = std::env::var("PRISM_LOCAL_CONTEXT_SIZE")
            .ok()
            .and_then(|raw| raw.parse::<u32>().ok())
            .filter(|size| *size >= 512)
            .unwrap_or(4096);
        Self {
            model_spec,
            #[cfg(feature = "local-inference")]
            context_size,
            #[cfg(feature = "local-inference")]
            model: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }

    pub(crate) fn trained_context_window(&self) -> Option<u64> {
        let path = resolve_model_path(&self.model_spec).ok()?;
        trained_context_window(&path)
    }

    #[cfg(feature = "local-inference")]
    pub(crate) async fn health_check(&self) -> Result<()> {
        let model_spec = self.model_spec.clone();
        let model_cache = std::sync::Arc::clone(&self.model);
        tokio::task::spawn_blocking(move || load_model(&model_spec, &model_cache).map(|_| ()))
            .await
            .map_err(|error| anyhow::anyhow!("local GGUF health check task panicked: {error}"))?
    }

    #[cfg(not(feature = "local-inference"))]
    pub(crate) async fn health_check(&self) -> Result<()> {
        Err(feature_disabled_error())
    }

    #[cfg(feature = "local-inference")]
    pub(crate) async fn generate_streaming(
        &self,
        messages: &[ChatMessage],
        max_tokens: u64,
        mut on_delta: impl FnMut(&str),
    ) -> Result<LocalGeneration> {
        let messages = messages.to_vec();
        let model_spec = self.model_spec.clone();
        let model_cache = std::sync::Arc::clone(&self.model);
        let context_size = self.context_size;
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_guard = CancelOnDrop(std::sync::Arc::clone(&cancelled));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

        let worker = tokio::task::spawn_blocking(move || {
            let result = generate(
                &model_spec,
                &model_cache,
                &messages,
                context_size,
                max_tokens,
                &cancelled,
                |piece| sender.send(GenerationEvent::Delta(piece)).is_ok(),
            )
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(GenerationEvent::Finished(result));
        });

        let result = loop {
            match receiver.recv().await {
                Some(GenerationEvent::Delta(piece)) => on_delta(&piece),
                Some(GenerationEvent::Finished(result)) => {
                    break result.map_err(anyhow::Error::msg);
                }
                None => bail!("local GGUF generation worker stopped without a result"),
            }
        };
        worker
            .await
            .map_err(|error| anyhow::anyhow!("local GGUF generation task panicked: {error}"))?;
        drop(cancel_guard);
        result
    }

    #[cfg(not(feature = "local-inference"))]
    pub(crate) async fn generate_streaming(
        &self,
        _messages: &[ChatMessage],
        _max_tokens: u64,
        _on_delta: impl FnMut(&str),
    ) -> Result<LocalGeneration> {
        Err(feature_disabled_error())
    }
}

fn trained_context_window(path: &Path) -> Option<u64> {
    #[cfg(feature = "local-inference")]
    {
        let metadata = llama_cpp_2::gguf::GgufContext::from_file(path)?;
        let architecture_index = metadata.find_key("general.architecture");
        if architecture_index < 0 {
            return None;
        }
        let architecture = metadata.val_str(architecture_index)?;
        let context_index = metadata.find_key(&format!("{architecture}.context_length"));
        if context_index < 0 {
            return None;
        }
        match metadata.kv_type(context_index) {
            llama_cpp_sys_2::GGUF_TYPE_UINT32 => Some(u64::from(metadata.val_u32(context_index))),
            llama_cpp_sys_2::GGUF_TYPE_UINT64 => Some(metadata.val_u64(context_index)),
            _ => None,
        }
    }
    #[cfg(not(feature = "local-inference"))]
    {
        let _ = path;
        None
    }
}

#[cfg(feature = "local-inference")]
#[derive(Debug)]
enum GenerationEvent {
    Delta(String),
    Finished(std::result::Result<LocalGeneration, String>),
}

#[derive(Debug)]
pub(crate) struct LocalGeneration {
    pub(crate) text: String,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
}

#[cfg(feature = "local-inference")]
struct CancelOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(feature = "local-inference")]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(feature = "local-inference")]
fn llama_backend() -> Result<&'static llama_cpp_2::llama_backend::LlamaBackend> {
    static BACKEND: std::sync::OnceLock<
        std::result::Result<llama_cpp_2::llama_backend::LlamaBackend, String>,
    > = std::sync::OnceLock::new();
    BACKEND
        .get_or_init(|| {
            let mut backend = llama_cpp_2::llama_backend::LlamaBackend::init()
                .map_err(|error| format!("failed to initialize embedded llama.cpp: {error}"))?;
            backend.void_logs();
            Ok(backend)
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))
}

#[cfg(feature = "local-inference")]
fn load_model(
    model_spec: &str,
    cache: &std::sync::OnceLock<std::sync::Arc<llama_cpp_2::model::LlamaModel>>,
) -> Result<std::sync::Arc<llama_cpp_2::model::LlamaModel>> {
    if let Some(model) = cache.get() {
        return Ok(std::sync::Arc::clone(model));
    }
    let path = resolve_model_path(model_spec)?;
    let backend = llama_backend()?;
    let mut params = llama_cpp_2::model::params::LlamaModelParams::default();
    #[cfg(any(target_os = "macos", feature = "local-inference-cuda"))]
    {
        params = params.with_n_gpu_layers(u32::MAX);
    }
    let loaded = std::sync::Arc::new(
        llama_cpp_2::model::LlamaModel::load_from_file(backend, &path, &params).map_err(
            |error| {
                anyhow::anyhow!(
                    "failed to load local GGUF model {}: {error}",
                    path.display()
                )
            },
        )?,
    );
    if cache.set(std::sync::Arc::clone(&loaded)).is_err() {
        return Ok(std::sync::Arc::clone(
            cache
                .get()
                .expect("another thread just initialized the model"),
        ));
    }
    Ok(loaded)
}

#[cfg(feature = "local-inference")]
fn generate(
    model_spec: &str,
    model_cache: &std::sync::OnceLock<std::sync::Arc<llama_cpp_2::model::LlamaModel>>,
    messages: &[ChatMessage],
    requested_context_size: u32,
    requested_max_tokens: u64,
    cancelled: &std::sync::atomic::AtomicBool,
    mut emit: impl FnMut(String) -> bool,
) -> Result<LocalGeneration> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::{AddBos, LlamaChatMessage};
    use llama_cpp_2::sampling::LlamaSampler;
    use std::num::NonZeroU32;
    use std::sync::atomic::Ordering;

    if messages
        .iter()
        .any(|message| message.tool_calls.is_some() || message.tool_call_id.is_some())
    {
        bail!(
            "embedded GGUF inference cannot render prior tool-call messages yet; no model request was made"
        );
    }
    if cancelled.load(Ordering::Acquire) {
        bail!("local GGUF generation cancelled");
    }

    let model = load_model(model_spec, model_cache)?;
    let template = model.chat_template(None).map_err(|error| {
        anyhow::anyhow!(
            "local GGUF model has no usable embedded chat template: {error}. Use an instruct/chat GGUF with tokenizer.chat_template metadata."
        )
    })?;
    let chat = messages
        .iter()
        .map(|message| {
            LlamaChatMessage::new(
                message.role.clone(),
                message.content.clone().unwrap_or_default(),
            )
            .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let prompt = model
        .apply_chat_template(&template, &chat, true)
        .map_err(|error| anyhow::anyhow!("failed to apply the GGUF chat template: {error}"))?;
    let tokens = model
        .str_to_token(&prompt, AddBos::Never)
        .map_err(|error| anyhow::anyhow!("failed to tokenize the local prompt: {error}"))?;

    let trained_context = model.n_ctx_train().max(512);
    let context_size = requested_context_size.min(trained_context);
    if tokens.len() >= context_size as usize {
        bail!(
            "local GGUF prompt is {} tokens but the active context is {context_size}. Set PRISM_LOCAL_CONTEXT_SIZE to a larger value no greater than the model's trained context ({trained_context}).",
            tokens.len()
        );
    }
    let available = u64::from(context_size) - tokens.len() as u64;
    let max_tokens = requested_max_tokens.min(available);
    if max_tokens == 0 {
        bail!("local GGUF context has no room for output tokens");
    }

    let batch_size = context_size.min(512);
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(context_size))
        .with_n_batch(batch_size)
        .with_n_ubatch(batch_size);
    let backend = llama_backend()?;
    let mut context = model
        .new_context(backend, params)
        .map_err(|error| anyhow::anyhow!("failed to create local GGUF context: {error}"))?;
    let mut batch = LlamaBatch::new(batch_size as usize, 1);

    for (chunk_index, chunk) in tokens.chunks(batch_size as usize).enumerate() {
        if cancelled.load(Ordering::Acquire) {
            bail!("local GGUF generation cancelled");
        }
        batch.clear();
        let base_position = chunk_index * batch_size as usize;
        for (offset, token) in chunk.iter().enumerate() {
            let is_last = base_position + offset + 1 == tokens.len();
            batch.add(
                *token,
                i32::try_from(base_position + offset)
                    .map_err(|_| anyhow::anyhow!("local prompt position exceeds i32"))?,
                &[0],
                is_last,
            )?;
        }
        context
            .decode(&mut batch)
            .map_err(|error| anyhow::anyhow!("failed to evaluate local prompt: {error}"))?;
    }

    let mut sampler = LlamaSampler::greedy();
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut text = String::new();
    let mut completion_tokens = 0_u64;
    let mut logits_index = batch.n_tokens() - 1;
    let mut position = i32::try_from(tokens.len())
        .map_err(|_| anyhow::anyhow!("local prompt position exceeds i32"))?;

    while completion_tokens < max_tokens {
        if cancelled.load(Ordering::Acquire) {
            bail!("local GGUF generation cancelled");
        }
        let token = sampler.sample(&context, logits_index);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        let piece = model
            .token_to_piece(token, &mut decoder, false, None)
            .map_err(|error| anyhow::anyhow!("failed to decode a local output token: {error}"))?;
        completion_tokens += 1;
        if !piece.is_empty() {
            text.push_str(&piece);
            if !emit(piece) {
                bail!("local GGUF generation cancelled");
            }
        }

        batch.clear();
        batch.add(token, position, &[0], true)?;
        context
            .decode(&mut batch)
            .map_err(|error| anyhow::anyhow!("failed to evaluate local output token: {error}"))?;
        logits_index = 0;
        position += 1;
    }

    let mut tail = String::new();
    let _ = decoder.decode_to_string(b"", &mut tail, true);
    if !tail.is_empty() {
        text.push_str(&tail);
        if !emit(tail) {
            bail!("local GGUF generation cancelled");
        }
    }

    Ok(LocalGeneration {
        text,
        prompt_tokens: tokens.len() as u64,
        completion_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_model_name_from_established_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tiny.gguf");
        std::fs::write(&path, b"fixture").unwrap();
        assert_eq!(resolve_model_path_in("tiny", temp.path()).unwrap(), path);
    }

    #[test]
    fn absent_weights_error_is_structured_and_actionable() {
        let temp = tempfile::tempdir().unwrap();
        let error = resolve_model_path_in("missing-model", temp.path())
            .unwrap_err()
            .to_string();
        let json = error
            .strip_prefix("local GGUF inference refused:\n")
            .expect("structured refusal prefix");
        let refusal: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(refusal["status"], "refused");
        assert_eq!(refusal["refusal"]["code"], "local_llm_weights_unavailable");
        assert_eq!(refusal["refusal"]["expected_format"], ".gguf");
        assert_eq!(
            refusal["refusal"]["model_directory"],
            temp.path().display().to_string()
        );
        assert!(
            refusal["install_hint"]
                .as_str()
                .unwrap()
                .contains("does not download")
        );
    }

    #[cfg(feature = "local-inference")]
    #[test]
    fn cancellation_guard_signals_the_blocking_worker() {
        use std::sync::atomic::Ordering;

        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let _guard = CancelOnDrop(std::sync::Arc::clone(&cancelled));
            assert!(!cancelled.load(Ordering::Acquire));
        }
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn wrong_file_format_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("weights.bin");
        std::fs::write(&path, b"fixture").unwrap();
        let error = resolve_model_path_in(path.to_str().unwrap(), temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected_format"));
        assert!(error.contains(".gguf"));
    }
}
