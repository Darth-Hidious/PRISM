// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Embedded GGUF adapter: model resolution is always available; inference is opt-in.

use std::path::{Path, PathBuf};

#[cfg(feature = "local-inference")]
use anyhow::Context;
use anyhow::{Result, bail};

use crate::{ChatMessage, RenderedLocalPrompt, ToolDefinition};

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
        "network": "No remote endpoint was tried.",
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

/// Conservative fallback only for unreadable/legacy GGUF metadata. Normal
/// local operation derives the active context directly from the model.
#[cfg(feature = "local-inference")]
const FALLBACK_CONTEXT_SIZE: u32 = 4096;

/// Local adapter state. The model is loaded once and shared; every request gets
/// a fresh context/KV cache so conversation state remains explicit in messages.
pub(crate) struct LocalGguf {
    #[cfg(feature = "local-inference")]
    model_spec: String,
    #[cfg(feature = "local-inference")]
    context_size: u32,
    #[cfg(feature = "local-inference")]
    model: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<llama_cpp_2::model::LlamaModel>>>,
}

impl LocalGguf {
    pub(crate) fn new(model_spec: String) -> Self {
        #[cfg(feature = "local-inference")]
        let context_size = active_context_size(&model_spec);
        #[cfg(not(feature = "local-inference"))]
        let _ = model_spec;
        Self {
            #[cfg(feature = "local-inference")]
            model_spec,
            #[cfg(feature = "local-inference")]
            context_size,
            #[cfg(feature = "local-inference")]
            model: std::sync::Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Context the runtime will actually allocate. GGUF metadata is the
    /// default authority; an explicit smaller override remains a valid cap.
    pub(crate) fn context_window(&self) -> Option<u64> {
        #[cfg(feature = "local-inference")]
        {
            Some(u64::from(self.context_size))
        }
        #[cfg(not(feature = "local-inference"))]
        {
            None
        }
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
        tools: &[ToolDefinition],
        max_tokens: u64,
        mut on_delta: impl FnMut(&str),
    ) -> Result<LocalGeneration> {
        let messages = messages.to_vec();
        let tools = tools.to_vec();
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
                &tools,
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
        _tools: &[ToolDefinition],
        _max_tokens: u64,
        _on_delta: impl FnMut(&str),
    ) -> Result<LocalGeneration> {
        Err(feature_disabled_error())
    }

    #[cfg(feature = "local-inference")]
    pub(crate) async fn render_prompt(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<RenderedLocalPrompt> {
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model_spec = self.model_spec.clone();
        let model_cache = std::sync::Arc::clone(&self.model);
        tokio::task::spawn_blocking(move || {
            let model = load_model(&model_spec, &model_cache)?;
            prepare_prompt(&model_spec, &model, &messages, &tools).map(|prompt| prompt.rendered)
        })
        .await
        .map_err(|error| anyhow::anyhow!("local GGUF prompt-render task panicked: {error}"))?
    }

    #[cfg(not(feature = "local-inference"))]
    pub(crate) async fn render_prompt(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> Result<RenderedLocalPrompt> {
        Err(feature_disabled_error())
    }
}

#[cfg(feature = "local-inference")]
fn active_context_size(model_spec: &str) -> u32 {
    let trained = resolve_model_path(model_spec)
        .ok()
        .and_then(|path| trained_context_window(&path))
        .and_then(|size| u32::try_from(size).ok())
        .filter(|size| *size >= 512);
    let requested = std::env::var("PRISM_LOCAL_CONTEXT_SIZE")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|size| *size >= 512);

    match (requested, trained) {
        (Some(requested), Some(trained)) => requested.min(trained),
        (Some(requested), None) => requested,
        (None, Some(trained)) => trained,
        (None, None) => FALLBACK_CONTEXT_SIZE,
    }
}

#[cfg(feature = "local-inference")]
fn trained_context_window(path: &Path) -> Option<u64> {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeToolProtocol {
    /// The original Liquid function-call tokens used by compatible GGUFs.
    Legacy,
    /// LiquidAI LFM 2.5's embedded Jinja tool-call format.
    Lfm,
    /// Google Gemma 4's template-native declarations, calls, and responses.
    Gemma,
}

#[cfg(feature = "local-inference")]
fn native_tool_protocol(template: &str) -> Option<NativeToolProtocol> {
    if template.contains("<|tool_call_start|>") && template.contains("<|tool_call_end|>") {
        Some(NativeToolProtocol::Lfm)
    } else if template.contains("<|tool>")
        && template.contains("<|tool_call>")
        && template.contains("<tool_call|>")
    {
        Some(NativeToolProtocol::Gemma)
    } else if template.contains("<start_function_declaration>")
        && template.contains("<start_function_call>")
    {
        Some(NativeToolProtocol::Legacy)
    } else {
        None
    }
}

#[cfg(feature = "local-inference")]
fn native_escape(value: &str) -> String {
    // Legacy templates use `<escape>` delimiters. The payload itself is JSON
    // escaped so punctuation remains data and a literal '<' cannot terminate
    // the delimiter before its matching `<escape>` marker.
    let encoded = serde_json::to_string(value).expect("strings are JSON serializable");
    let payload = &encoded[1..encoded.len() - 1];
    format!("<escape>{}<escape>", payload.replace('<', "\\u003C"))
}

#[cfg(feature = "local-inference")]
fn native_type(schema: &serde_json::Value) -> &'static str {
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => "STRING",
        Some("number") => "NUMBER",
        Some("integer") => "INTEGER",
        Some("boolean") => "BOOLEAN",
        Some("array") => "ARRAY",
        Some("null") => "NULL",
        _ => "OBJECT",
    }
}

#[cfg(feature = "local-inference")]
fn native_declaration(tool: &ToolDefinition) -> String {
    let schema = &tool.function.parameters;
    let mut declaration = format!(
        "<start_function_declaration>declaration:{}{{description:{},parameters:{{",
        tool.function.name,
        native_escape(&tool.function.description)
    );
    if let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    {
        declaration.push_str("properties:{");
        for (index, (name, property)) in properties.iter().enumerate() {
            if index > 0 {
                declaration.push(',');
            }
            let description = property
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            declaration.push_str(name);
            declaration.push_str("{description:");
            declaration.push_str(&native_escape(description));
            declaration.push_str(",type:");
            declaration.push_str(&native_escape(native_type(property)));
            declaration.push('}');
        }
        declaration.push_str("},");
    }
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        declaration.push_str("required:[");
        for (index, name) in required
            .iter()
            .filter_map(serde_json::Value::as_str)
            .enumerate()
        {
            if index > 0 {
                declaration.push(',');
            }
            declaration.push_str(&native_escape(name));
        }
        declaration.push_str("],");
    }
    declaration.push_str("type:");
    declaration.push_str(&native_escape(native_type(schema)));
    declaration.push_str("}}<end_function_declaration>");
    declaration
}

#[cfg(feature = "local-inference")]
fn native_tool_declarations(tools: &[ToolDefinition], protocol: NativeToolProtocol) -> String {
    match protocol {
        NativeToolProtocol::Legacy => tools.iter().map(native_declaration).collect(),
        // This is the exact template-native declaration shape embedded in the
        // official LFM 2.5 GGUF. llama-cpp-2 currently only takes rendered
        // messages, not a separate `tools` parameter, so it is injected into
        // the system message rather than silently omitted.
        NativeToolProtocol::Lfm => format!(
            "List of tools: {}",
            serde_json::to_string(tools).expect("tool definitions are serializable")
        ),
        // Gemma receives the definitions through the template's `tools`
        // variable, so injecting a second prose projection would change both
        // the schema and the intervention being measured.
        NativeToolProtocol::Gemma => String::new(),
    }
}

#[cfg(feature = "local-inference")]
fn native_tool_call_text(
    tool_calls: &[crate::ToolCallResponse],
    protocol: NativeToolProtocol,
) -> Result<String> {
    let mut result = String::new();
    for call in tool_calls {
        let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
            .context("prior local tool call arguments were not valid JSON")?;
        let object = arguments
            .as_object()
            .context("prior local tool call arguments were not an object")?;
        match protocol {
            NativeToolProtocol::Legacy => {
                result.push_str("<start_function_call>call:");
                result.push_str(&call.function.name);
                result.push('{');
                for (index, (key, value)) in object.iter().enumerate() {
                    if index > 0 {
                        result.push(',');
                    }
                    result.push_str(key);
                    result.push(':');
                    result.push_str(&legacy_native_value(value));
                }
                result.push_str("}<end_function_call>");
            }
            NativeToolProtocol::Lfm => {
                result.push_str("<|tool_call_start|>[");
                result.push_str(&call.function.name);
                result.push('(');
                for (index, (key, value)) in object.iter().enumerate() {
                    if index > 0 {
                        result.push_str(", ");
                    }
                    result.push_str(key);
                    result.push('=');
                    result.push_str(&serde_json::to_string(value)?);
                }
                result.push_str(")]<|tool_call_end|>");
            }
            NativeToolProtocol::Gemma => {
                result.push_str("<|tool_call>call:");
                result.push_str(&call.function.name);
                result.push('{');
                for (index, (key, value)) in object.iter().enumerate() {
                    if index > 0 {
                        result.push(',');
                    }
                    result.push_str(key);
                    result.push(':');
                    result.push_str(&gemma_native_value(value));
                }
                result.push_str("}<tool_call|>");
            }
        }
    }
    Ok(result)
}

#[cfg(feature = "local-inference")]
fn gemma_native_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => format!(r#"<|"|>{value}<|"|>"#),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(gemma_native_value)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{key}:{}", gemma_native_value(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}

#[cfg(feature = "local-inference")]
fn legacy_native_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => native_escape(value),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(legacy_native_value)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{key}:{}", legacy_native_value(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}

#[cfg(feature = "local-inference")]
fn native_tool_result(name: &str, content: &str) -> String {
    format!(
        "<start_function_response>response:{name}{{value:{}}}<end_function_response>",
        native_escape(content)
    )
}

#[cfg(feature = "local-inference")]
fn native_value_rule(schema: &serde_json::Value, protocol: NativeToolProtocol) -> &'static str {
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => match protocol {
            NativeToolProtocol::Legacy => "escaped",
            NativeToolProtocol::Lfm => "string",
            NativeToolProtocol::Gemma => "gemmastring",
        },
        Some("object") => "object",
        Some("array") => "array",
        Some("number") | Some("integer") => "number",
        Some("boolean") => "boolean",
        _ => "value",
    }
}

#[cfg(all(test, feature = "local-inference"))]
fn native_tool_call_grammar(tools: &[ToolDefinition]) -> String {
    native_tool_call_grammar_for(tools, NativeToolProtocol::Legacy)
}

#[cfg(feature = "local-inference")]
fn native_tool_call_grammar_for(tools: &[ToolDefinition], protocol: NativeToolProtocol) -> String {
    match protocol {
        NativeToolProtocol::Legacy => legacy_tool_call_grammar(tools),
        NativeToolProtocol::Lfm => lfm_tool_call_grammar(tools),
        NativeToolProtocol::Gemma => gemma_tool_call_grammar(tools),
    }
}

#[cfg(feature = "local-inference")]
fn legacy_tool_call_grammar(tools: &[ToolDefinition]) -> String {
    let call_rules = (0..tools.len())
        .map(|index| format!("call{index}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut grammar = format!("root ::= {call_rules} | final\n");
    grammar.push_str(
        r#"final ::= [^<]*
value ::= escaped | object | array | number | boolean | "null"
boolean ::= "true" | "false"
escaped ::= "<escape>" escapedchar* "<escape>"
escapedchar ::= [^<\\] | "\\" (["\\/bfnrt] | "u" hex hex hex hex)
hex ::= [0-9a-fA-F]
object ::= "{" ws members ws "}"
array ::= "[" ws (value (ws "," ws value)*)? ws "]"
members ::= pair (ws "," ws pair)* | ""
pair ::= key ws ":" ws value
key ::= [a-zA-Z_] [a-zA-Z0-9_-]*
number ::= [+-]? [0-9]+ ("." [0-9]+)? ([eE] [+-]? [0-9]+)?
ws ::= [ \t\n]*
"#,
    );
    for (index, tool) in tools.iter().enumerate() {
        append_tool_rule(
            &mut grammar,
            tool,
            index,
            NativeToolProtocol::Legacy,
            "<start_function_call>call:",
            "{",
            "}",
            ":",
            "<end_function_call>",
        );
    }
    grammar
}

#[cfg(feature = "local-inference")]
fn lfm_tool_call_grammar(tools: &[ToolDefinition]) -> String {
    let call_rules = (0..tools.len())
        .map(|index| format!("call{index}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut grammar = format!("root ::= {call_rules} | final\n");
    grammar.push_str(
        r#"final ::= [^<]*
value ::= string | object | array | number | boolean | "null"
string ::= jsonstring | singlestring
jsonstring ::= "\"" jsonchar* "\""
jsonchar ::= [^"\\] | "\\" (["\\/bfnrt] | "u" hex hex hex hex)
singlestring ::= "'" singlechar* "'"
singlechar ::= [^'\\] | "\\" (["'\\/bfnrt] | "u" hex hex hex hex)
hex ::= [0-9a-fA-F]
boolean ::= "true" | "false"
object ::= "{" ws members ws "}"
array ::= "[" ws (value (ws "," ws value)*)? ws "]"
members ::= pair (ws "," ws pair)* | ""
pair ::= (key | jsonstring) ws ":" ws value
lfmpair ::= key ws "=" ws value
key ::= [a-zA-Z_] [a-zA-Z0-9_-]*
number ::= [+-]? [0-9]+ ("." [0-9]+)? ([eE] [+-]? [0-9]+)?
ws ::= [ \t\n]*
"#,
    );
    for (index, tool) in tools.iter().enumerate() {
        append_tool_rule(
            &mut grammar,
            tool,
            index,
            NativeToolProtocol::Lfm,
            "<|tool_call_start|>[",
            "(",
            ")",
            "=",
            "]<|tool_call_end|>",
        );
    }
    grammar
}

#[cfg(feature = "local-inference")]
fn gemma_tool_call_grammar(tools: &[ToolDefinition]) -> String {
    let call_rules = (0..tools.len())
        .map(|index| format!("call{index}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut grammar = format!("root ::= {call_rules} | final\n");
    grammar.push_str(
        r#"final ::= [^<]*
value ::= gemmastring | object | array | number | boolean | "null"
gemmastring ::= "<|\"|>" gemmachar* "<|\"|>"
gemmachar ::= [^<]
boolean ::= "true" | "false"
object ::= "{" ws members ws "}"
array ::= "[" ws (value (ws "," ws value)*)? ws "]"
members ::= pair (ws "," ws pair)* | ""
pair ::= key ws ":" ws value
key ::= [a-zA-Z_] [a-zA-Z0-9_-]*
number ::= [+-]? [0-9]+ ("." [0-9]+)? ([eE] [+-]? [0-9]+)?
ws ::= [ \t\n]*
"#,
    );
    for (index, tool) in tools.iter().enumerate() {
        append_tool_rule(
            &mut grammar,
            tool,
            index,
            NativeToolProtocol::Gemma,
            "<|tool_call>call:",
            "{",
            "}",
            ":",
            "<tool_call|>",
        );
    }
    grammar
}

#[cfg(feature = "local-inference")]
#[allow(clippy::too_many_arguments)]
fn append_tool_rule(
    grammar: &mut String,
    tool: &ToolDefinition,
    index: usize,
    protocol: NativeToolProtocol,
    call_prefix: &str,
    argument_open: &str,
    argument_close: &str,
    assignment: &str,
    call_suffix: &str,
) {
    let label = index.to_string();
    let required = tool
        .function
        .parameters
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    grammar.push_str(&format!(
        "call{label} ::= \"{}\" \"{}\" \"{}\" args{label} \"{}{}\"\n",
        grammar_literal(call_prefix),
        grammar_literal(&tool.function.name),
        grammar_literal(argument_open),
        grammar_literal(argument_close),
        grammar_literal(call_suffix),
    ));
    let separator = match protocol {
        NativeToolProtocol::Legacy | NativeToolProtocol::Lfm | NativeToolProtocol::Gemma => {
            " ws \",\" ws "
        }
    };
    let sequence = required
        .iter()
        .enumerate()
        .map(|(required_index, _)| format!("arg{label}x{required_index}"))
        .collect::<Vec<_>>()
        .join(separator);
    let generic_pair = match protocol {
        NativeToolProtocol::Legacy | NativeToolProtocol::Gemma => "pair",
        NativeToolProtocol::Lfm => "lfmpair",
    };
    if required.is_empty() {
        grammar.push_str(&format!(
            "args{label} ::= {generic_pair} ({separator}{generic_pair})* | \"\"\n"
        ));
    } else {
        grammar.push_str(&format!(
            "args{label} ::= {sequence} ({separator}{generic_pair})*\n"
        ));
        for (required_index, name) in required.iter().enumerate() {
            let value_rule = tool
                .function
                .parameters
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .and_then(|properties| properties.get(*name))
                .map(|schema| native_value_rule(schema, protocol))
                .unwrap_or("value");
            grammar.push_str(&format!(
                "arg{label}x{required_index} ::= \"{}\" ws \"{}\" ws {value_rule}\n",
                grammar_literal(name),
                grammar_literal(assignment),
            ));
        }
    }
}

#[cfg(feature = "local-inference")]
fn grammar_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(feature = "local-inference")]
#[derive(Clone, Debug, serde::Serialize)]
struct TemplateMessage {
    role: String,
    content: String,
}

#[cfg(feature = "local-inference")]
fn render_gemma_messages(messages: &[ChatMessage]) -> Result<Vec<serde_json::Value>> {
    messages
        .iter()
        .map(|message| {
            let mut rendered = serde_json::Map::new();
            rendered.insert("role".to_string(), message.role.clone().into());
            if let Some(content) = &message.content {
                rendered.insert("content".to_string(), content.clone().into());
            }
            if let Some(tool_call_id) = &message.tool_call_id {
                rendered.insert("tool_call_id".to_string(), tool_call_id.clone().into());
            }
            if let Some(tool_calls) = &message.tool_calls {
                let tool_calls = tool_calls
                    .iter()
                    .map(|call| {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&call.function.arguments).with_context(|| {
                                format!(
                                    "prior Gemma tool call {} arguments were not valid JSON",
                                    call.id
                                )
                            })?;
                        if !arguments.is_object() {
                            bail!(
                                "prior Gemma tool call {} arguments were not an object",
                                call.id
                            );
                        }
                        Ok(serde_json::json!({
                            "id": call.id,
                            "type": call.call_type,
                            "function": {
                                "name": call.function.name,
                                "arguments": arguments,
                            }
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                rendered.insert("tool_calls".to_string(), tool_calls.into());
            }
            Ok(rendered.into())
        })
        .collect()
}

#[cfg(feature = "local-inference")]
fn render_messages(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    native_protocol: Option<NativeToolProtocol>,
) -> Result<Vec<TemplateMessage>> {
    let instructions = if tools.is_empty() {
        None
    } else if let Some(protocol) = native_protocol {
        Some(format!(
            "Use a declared function whenever it is needed. After every tool result, either call another declared function or return a final natural-language answer. Never use placeholder arguments.\n\n{}",
            native_tool_declarations(tools, protocol)
        ))
    } else {
        let tool_json = serde_json::to_string_pretty(tools)?;
        Some(format!(
            concat!(
                "Tool protocol for this turn. Return exactly one JSON object and no Markdown, commentary, or code fence.\n\n",
                "If the user request needs a function, return {{\"kind\":\"tool_call\",\"name\":\"EXACT_FUNCTION_NAME\",\"arguments\":{{...}}}}. The name must be one of the listed functions and arguments must follow its schema.\n",
                "If no function is needed, return {{\"kind\":\"final\",\"content\":\"answer\"}}. Never invent a tool result.\n\n",
                "Available functions:\n{}\n\nAfter every tool result, call another function when needed to answer the user."
            ),
            tool_json
        ))
    };

    let names_by_id: std::collections::HashMap<&str, &str> = messages
        .iter()
        .filter_map(|message| message.tool_calls.as_ref())
        .flatten()
        .map(|call| (call.id.as_str(), call.function.name.as_str()))
        .collect();
    let results_by_id: std::collections::HashMap<&str, &str> = messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| {
            message
                .tool_call_id
                .as_deref()
                .zip(message.content.as_deref())
        })
        .collect();
    let mut rendered = Vec::with_capacity(messages.len() + usize::from(instructions.is_some()));
    let mut injected = false;
    for message in messages {
        let mut role = message.role.clone();
        let mut content = message.content.clone().unwrap_or_default();

        if let Some(tool_calls) = &message.tool_calls {
            let mut calls = match native_protocol {
                Some(protocol) => native_tool_call_text(tool_calls, protocol)?,
                None => format!(
                    "Previous function call completed: {}",
                    serde_json::to_string(tool_calls)?
                ),
            };
            if native_protocol == Some(NativeToolProtocol::Legacy) {
                for call in tool_calls {
                    if let Some(result) = results_by_id.get(call.id.as_str()) {
                        calls.push_str(&native_tool_result(&call.function.name, result));
                    }
                }
            }
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(&calls);
        }
        if message.role == "tool" {
            let id = message.tool_call_id.as_deref().unwrap_or("local_tool");
            let name = names_by_id.get(id).copied().unwrap_or("unknown_tool");
            if native_protocol == Some(NativeToolProtocol::Legacy) {
                if name == "unknown_tool" {
                    bail!("local GGUF tool result {id:?} has no matching prior tool call");
                }
                continue;
            }
            role = "user".to_string();
            content = format!("Tool result from {name} ({id}):\n{content}");
        }

        if !injected
            && let Some(instructions) = &instructions
            && role == "system"
        {
            content.push_str("\n\n");
            content.push_str(instructions);
            injected = true;
        }

        rendered.push(TemplateMessage { role, content });
    }
    if let Some(instructions) = instructions
        && !injected
    {
        rendered.insert(
            0,
            TemplateMessage {
                role: "system".to_string(),
                content: instructions,
            },
        );
    }
    Ok(rendered)
}

#[cfg(feature = "local-inference")]
struct PreparedPrompt {
    rendered: RenderedLocalPrompt,
    tokens: Vec<llama_cpp_2::token::LlamaToken>,
    native_protocol: Option<NativeToolProtocol>,
}

#[cfg(feature = "local-inference")]
fn special_token_piece(
    model: &llama_cpp_2::model::LlamaModel,
    token: llama_cpp_2::token::LlamaToken,
) -> Result<String> {
    // llama.cpp represents an absent special token as LLAMA_TOKEN_NULL (-1).
    // Its own template context exposes that case as an empty string.
    if token.0 == -1 {
        return Ok(String::new());
    }
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    model
        .token_to_piece(token, &mut decoder, true, None)
        .map_err(|error| anyhow::anyhow!("failed to decode a GGUF special token: {error}"))
}

#[cfg(feature = "local-inference")]
fn prepare_prompt(
    model_spec: &str,
    model: &llama_cpp_2::model::LlamaModel,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
) -> Result<PreparedPrompt> {
    use llama_cpp_2::model::AddBos;

    let template = model.chat_template(None).map_err(|error| {
        anyhow::anyhow!(
            "local GGUF model {model_spec:?} has no usable embedded chat template: {error}. Use an instruct/chat GGUF with tokenizer.chat_template metadata. No remote endpoint was tried."
        )
    })?;
    let template_source = template.to_string().map_err(|error| {
        anyhow::anyhow!(
            "local GGUF model {model_spec:?} has invalid chat template metadata: {error}. No remote endpoint was tried."
        )
    })?;
    let native_protocol = (!tools.is_empty())
        .then(|| native_tool_protocol(&template_source))
        .flatten();
    let chat = if native_protocol == Some(NativeToolProtocol::Gemma) {
        render_gemma_messages(messages)?
    } else {
        serde_json::to_value(render_messages(messages, tools, native_protocol)?)?
            .as_array()
            .cloned()
            .expect("serializing a message vector produces a JSON array")
    };
    let template_tools = if native_protocol == Some(NativeToolProtocol::Gemma) {
        serde_json::to_value(tools)?
    } else {
        serde_json::json!([])
    };
    let context = serde_json::json!({
        "messages": chat,
        "tools": template_tools,
        "bos_token": special_token_piece(model, model.token_bos())?,
        "eos_token": special_token_piece(model, model.token_eos())?,
        "enable_thinking": false,
        "preserve_thinking": false,
        "add_generation_prompt": true,
    });
    let text = crate::minja::render(&template_source, &context)
        .context("failed to render the GGUF chat template with Minja")?;
    let tokens = model
        .str_to_token(&text, AddBos::Never)
        .map_err(|error| anyhow::anyhow!("failed to tokenize the local prompt: {error}"))?;
    let template_sha256 = crate::model_artifact::sha256_hex(template_source.as_bytes());
    let token_count = tokens.len() as u64;
    Ok(PreparedPrompt {
        rendered: RenderedLocalPrompt {
            text,
            token_count,
            template_sha256,
        },
        tokens,
        native_protocol,
    })
}

#[cfg(feature = "local-inference")]
fn local_tool_response_schema(tools: &[ToolDefinition]) -> serde_json::Value {
    let final_response = serde_json::json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "enum": ["final"]},
            "content": {"type": "string"}
        },
        "required": ["kind", "content"],
        "additionalProperties": false
    });
    let mut alternatives = Vec::with_capacity(tools.len() + 1);
    for tool in tools {
        alternatives.push(serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["tool_call"]},
                "name": {"type": "string", "enum": [tool.function.name]},
                "arguments": tool.function.parameters
            },
            "required": ["kind", "name", "arguments"],
            "additionalProperties": false
        }));
    }
    alternatives.push(final_response);
    serde_json::json!({"oneOf": alternatives})
}

#[cfg(feature = "local-inference")]
#[allow(clippy::too_many_arguments)]
fn generate(
    model_spec: &str,
    model_cache: &std::sync::OnceLock<std::sync::Arc<llama_cpp_2::model::LlamaModel>>,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    requested_context_size: u32,
    requested_max_tokens: u64,
    cancelled: &std::sync::atomic::AtomicBool,
    mut emit: impl FnMut(String) -> bool,
) -> Result<LocalGeneration> {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::sampling::LlamaSampler;
    use std::num::NonZeroU32;
    use std::sync::atomic::Ordering;

    if cancelled.load(Ordering::Acquire) {
        bail!("local GGUF generation cancelled");
    }

    let model = load_model(model_spec, model_cache)?;
    let prepared = prepare_prompt(model_spec, &model, messages, tools)?;
    let native_protocol = prepared.native_protocol;
    let tool_grammar = if let Some(protocol) = native_protocol {
        let grammar = native_tool_call_grammar_for(tools, protocol);
        Some(
            LlamaSampler::grammar(&model, &grammar, "root").map_err(|error| {
                anyhow::anyhow!(
                    "local GGUF model {model_spec:?} cannot initialize its embedded chat-template tool grammar: {error}. Tool calling was refused before generation. No remote endpoint was tried."
                )
            })?,
        )
    } else if !tools.is_empty() {
        let schema = local_tool_response_schema(tools);
        let schema_json = serde_json::to_string(&schema)?;
        let grammar = llama_cpp_2::json_schema_to_grammar(&schema_json).map_err(|error| {
            anyhow::anyhow!(
                "local GGUF model {model_spec:?} cannot constrain its tool response with llama.cpp JSON grammar: {error}. Tool calling was refused before generation. No remote endpoint was tried."
            )
        })?;
        Some(
            LlamaSampler::grammar(&model, &grammar, "root").map_err(|error| {
                anyhow::anyhow!(
                    "local GGUF model {model_spec:?} cannot initialize llama.cpp tool grammar: {error}. Tool calling was refused before generation. No remote endpoint was tried."
                )
            })?,
        )
    } else {
        None
    };
    let tokens = prepared.tokens;

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

    let mut sampler = match tool_grammar {
        Some(grammar) => LlamaSampler::chain_simple([grammar, LlamaSampler::greedy()]),
        None => LlamaSampler::greedy(),
    };
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
        if model.is_eog_token(token) {
            break;
        }
        let piece = model
            .token_to_piece(token, &mut decoder, true, None)
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

    #[cfg(feature = "local-inference")]
    fn test_tool(name: impl Into<String>) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".to_string(),
            function: crate::FunctionDef {
                name: name.into(),
                description: "test tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }
    }

    /// Regression H1: every declared function needs its own GBNF rule, even
    /// when a request carries a catalog-sized tool surface.
    #[cfg(feature = "local-inference")]
    #[test]
    fn native_grammar_uses_distinct_rule_names_for_one_hundred_tools() {
        let tools = (0..100)
            .map(|index| test_tool(format!("tool_{index}")))
            .collect::<Vec<_>>();
        for protocol in [NativeToolProtocol::Legacy, NativeToolProtocol::Lfm] {
            let grammar = native_tool_call_grammar_for(&tools, protocol);
            let call_rules = grammar
                .lines()
                .filter_map(|line| line.split_once(" ::= "))
                .map(|(name, _)| name)
                .filter(|name| name.starts_with("call"))
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(call_rules.len(), tools.len(), "{protocol:?}: {grammar}");
        }
    }

    /// Regression H2: a real text argument must retain delimiters and angle
    /// brackets rather than being token-masked into a different value.
    #[cfg(feature = "local-inference")]
    #[test]
    fn native_string_with_comma_brace_and_angle_bracket_round_trips() {
        let (name, arguments) = crate::parse_native_tool_call(
            "<start_function_call>call:recall{query:<escape>alpha, {beta} \\u003Cgamma\\u003E<escape>}<end_function_call>",
        )
        .unwrap();
        assert_eq!(name, "recall");
        assert_eq!(arguments["query"], "alpha, {beta} <gamma>");

        let grammar = native_tool_call_grammar(&[test_tool("recall")]);
        assert!(
            grammar.contains("escapedchar ::= [^<\\\\]"),
            "string grammar still bans delimiters: {grammar}"
        );
    }

    /// Regression H3: each later turn retains declarations, the prior native
    /// call, and its paired result, while the grammar still permits a call or
    /// ordinary final answer.
    #[cfg(feature = "local-inference")]
    #[test]
    fn lfm_multiturn_render_keeps_tools_calls_results_and_final_alternative() {
        let tools = vec![test_tool("find_tools"), test_tool("recall")];
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: Some("system prompt".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: Some("Find a material tool, then recall its result.".to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![crate::ToolCallResponse {
                    id: "call_1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::FunctionCall {
                        name: "find_tools".to_string(),
                        arguments: r#"{"query":"materials"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("materials_search is available".to_string()),
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
            },
        ];
        let rendered = render_messages(&messages, &tools, Some(NativeToolProtocol::Lfm)).unwrap();
        let prompt = rendered
            .iter()
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(prompt.contains("List of tools:"), "{prompt}");
        assert!(
            prompt.contains(
                "<|tool_call_start|>[find_tools(query=\\\"materials\\\")]<|tool_call_end|>"
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains("Tool result from find_tools (call_1):"),
            "{prompt}"
        );

        let grammar = native_tool_call_grammar_for(&tools, NativeToolProtocol::Lfm);
        assert!(
            grammar.contains("root ::= call0 | call1 | final"),
            "{grammar}"
        );
    }

    #[cfg(feature = "local-inference")]
    #[test]
    fn gemma_render_keeps_full_openai_tool_fields_for_minja() {
        let messages = vec![
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![crate::ToolCallResponse {
                    id: "call_7".to_string(),
                    call_type: "function".to_string(),
                    function: crate::FunctionCall {
                        name: "lookup".to_string(),
                        arguments: r#"{"query":"IN718","limit":2}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some("found".to_string()),
                tool_calls: None,
                tool_call_id: Some("call_7".to_string()),
            },
        ];
        let rendered = render_gemma_messages(&messages).unwrap();
        assert_eq!(rendered[0]["role"], "assistant");
        assert_eq!(rendered[0]["tool_calls"][0]["id"], "call_7");
        assert_eq!(
            rendered[0]["tool_calls"][0]["function"]["arguments"]["query"],
            "IN718"
        );
        assert!(rendered[0]["tool_calls"][0]["function"]["arguments"].is_object());
        assert_eq!(rendered[1]["role"], "tool");
        assert_eq!(rendered[1]["tool_call_id"], "call_7");

        let grammar = gemma_tool_call_grammar(&[test_tool("lookup")]);
        assert!(grammar.contains(r#"<|tool_call>call:"#), "{grammar}");
        assert!(grammar.contains(r#"gemmastring ::= "<|\"|>""#), "{grammar}");
    }
}
