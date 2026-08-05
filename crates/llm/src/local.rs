// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Embedded GGUF adapter: model resolution is always available; inference is opt-in.

use std::path::{Path, PathBuf};

#[cfg(feature = "local-inference")]
use anyhow::Context;
use anyhow::{Result, bail};

use crate::{ChatMessage, ToolDefinition};

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
fn native_escape(value: &str) -> String {
    format!("<escape>{value}<escape>")
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
fn native_tool_declarations(tools: &[ToolDefinition]) -> String {
    tools.iter().map(native_declaration).collect()
}

#[cfg(feature = "local-inference")]
fn native_tool_call_text(tool_calls: &[crate::ToolCallResponse]) -> Result<String> {
    let mut result = String::new();
    for call in tool_calls {
        let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
            .context("prior local tool call arguments were not valid JSON")?;
        let object = arguments
            .as_object()
            .context("prior local tool call arguments were not an object")?;
        result.push_str("<start_function_call>call:");
        result.push_str(&call.function.name);
        result.push('{');
        for (index, (key, value)) in object.iter().enumerate() {
            if index > 0 {
                result.push(',');
            }
            result.push_str(key);
            result.push(':');
            result.push_str(&native_value(value));
        }
        result.push_str("}<end_function_call>");
    }
    Ok(result)
}

#[cfg(feature = "local-inference")]
fn native_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => native_escape(value),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(native_value)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{key}:{}", native_value(value)))
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
fn native_value_rule(schema: &serde_json::Value) -> &'static str {
    match schema.get("type").and_then(serde_json::Value::as_str) {
        Some("string") => "escaped",
        Some("object") => "object",
        Some("array") => "array",
        Some("number") | Some("integer") => "number",
        Some("boolean") => "boolean",
        _ => "value",
    }
}

#[cfg(feature = "local-inference")]
fn native_tool_call_grammar(tools: &[ToolDefinition]) -> String {
    let call_rules = tools
        .iter()
        .enumerate()
        .map(|(index, _)| format!("call{}", grammar_label(index)))
        .collect::<Vec<_>>()
        .join(" | ");
    let mut grammar = format!(
        concat!(
            "root ::= {}\n",
            "value ::= escaped | object | array | number | boolean | \"null\"\n",
            "boolean ::= \"true\" | \"false\"\n",
            "escaped ::= \"<escape>\" [^<}},]+ \"<escape>\"\n",
            "object ::= \"{{\" ws members ws \"}}\"\n",
            "array ::=\n",
            "  \"[\" ws (\n",
            "    value\n",
            "    (ws \",\" ws value)*\n",
            "  )? \"]\"\n",
            "members ::= pair (ws \",\" ws pair)* | \"\"\n",
            "pair ::= key ws \":\" ws value\n",
            "key ::= [a-zA-Z_] [a-zA-Z0-9_-]*\n",
            "number ::= [+-]? [0-9]+ (\".\" [0-9]+)? ([eE] [+-]? [0-9]+)?\n",
            "ws ::= [ \\t\\n]*\n"
        ),
        call_rules
    );
    for (index, tool) in tools.iter().enumerate() {
        let label = grammar_label(index);
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
            "call{label} ::= \"<start_function_call>\" \"call:\" \"{}\" \"{{\" ws args{label} ws \"}}<end_function_call>\"\n",
            grammar_literal(&tool.function.name)
        ));
        let sequence = required
            .iter()
            .enumerate()
            .map(|(required_index, _)| format!("arg{}{}", label, grammar_label(required_index)))
            .collect::<Vec<_>>()
            .join(" ws \",\" ws ");
        if required.is_empty() {
            grammar.push_str(&format!(
                "args{label} ::= pair (ws \",\" ws pair)* | \"\"\n"
            ));
        } else {
            grammar.push_str(&format!("args{label} ::= {sequence} (ws \",\" ws pair)*\n"));
            for (required_index, name) in required.iter().enumerate() {
                let value_rule = tool
                    .function
                    .parameters
                    .pointer(&format!("/properties/{name}"))
                    .map(native_value_rule)
                    .unwrap_or("value");
                grammar.push_str(&format!(
                    "arg{}{} ::= \"{}\" ws \":\" ws {value_rule}\n",
                    label,
                    grammar_label(required_index),
                    grammar_literal(name)
                ));
            }
        }
    }
    grammar
}

#[cfg(feature = "local-inference")]
fn grammar_label(index: usize) -> char {
    char::from_u32(u32::from(b'a') + u32::try_from(index).unwrap_or(25).min(25)).unwrap_or('z')
}

#[cfg(feature = "local-inference")]
fn grammar_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(feature = "local-inference")]
fn render_messages(
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    native_tools: bool,
) -> Result<Vec<llama_cpp_2::model::LlamaChatMessage>> {
    use llama_cpp_2::model::LlamaChatMessage;

    let has_tool_result = messages.iter().any(|message| message.role == "tool");
    let instructions = if tools.is_empty() {
        None
    } else if native_tools {
        Some(format!(
            "{}\n{}",
            if has_tool_result {
                "A function result is already supplied. Do not call any function; return only the final natural-language answer."
            } else {
                "Use the declared functions when needed. Fill arguments from the user's exact request; never use placeholder values. Return a final natural-language answer after a function result."
            },
            native_tool_declarations(tools)
        ))
    } else if has_tool_result {
        Some(
            "A tool result is already in this conversation. Return only the final natural-language answer; do not call a function or repeat the tool protocol."
                .to_string(),
        )
    } else {
        let tool_json = serde_json::to_string_pretty(tools)?;
        Some(format!(
            concat!(
                "Tool protocol for this turn. Return exactly one JSON object and no Markdown, commentary, or code fence.\n\n",
                "If the user request needs a function, return {{\"kind\":\"tool_call\",\"name\":\"EXACT_FUNCTION_NAME\",\"arguments\":{{...}}}}. The name must be one of the listed functions and arguments must follow its schema.\n",
                "If no function is needed, return {{\"kind\":\"final\",\"content\":\"answer\"}}. Never invent a tool result.\n\n",
                "Available functions:\n{}\n\nCall a function when it is needed to answer the user."
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
            let native_history = native_tools && !has_tool_result;
            let mut calls = if native_history {
                native_tool_call_text(tool_calls)?
            } else if has_tool_result {
                String::new()
            } else {
                format!(
                    "Previous function call completed: {}",
                    serde_json::to_string(tool_calls)?
                )
            };
            if native_history {
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
            if native_tools && !has_tool_result {
                if !names_by_id.contains_key(id) {
                    bail!("local GGUF tool result {id:?} has no matching prior tool call");
                }
                continue;
            }
            role = "user".to_string();
            content = format!("Tool result for {id}:\n{content}");
        }

        if !injected
            && let Some(instructions) = &instructions
            && role == "system"
        {
            content.push_str("\n\n");
            content.push_str(instructions);
            injected = true;
        }

        rendered.push(LlamaChatMessage::new(role, content).map_err(anyhow::Error::from)?);
    }
    if let Some(instructions) = instructions
        && !injected
    {
        rendered.insert(
            0,
            LlamaChatMessage::new("system".to_string(), instructions)
                .map_err(anyhow::Error::from)?,
        );
    }
    Ok(rendered)
}

#[cfg(feature = "local-inference")]
fn local_tool_response_schema(
    tools: &[ToolDefinition],
    has_tool_result: bool,
) -> serde_json::Value {
    let final_response = serde_json::json!({
        "type": "object",
        "properties": {
            "kind": {"type": "string", "enum": ["final"]},
            "content": {"type": "string"}
        },
        "required": ["kind", "content"],
        "additionalProperties": false
    });
    if has_tool_result {
        return final_response;
    }

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
    use llama_cpp_2::model::AddBos;
    use llama_cpp_2::sampling::LlamaSampler;
    use std::num::NonZeroU32;
    use std::sync::atomic::Ordering;

    if cancelled.load(Ordering::Acquire) {
        bail!("local GGUF generation cancelled");
    }

    let model = load_model(model_spec, model_cache)?;
    let template = model.chat_template(None).map_err(|error| {
        anyhow::anyhow!(
            "local GGUF model {model_spec:?} has no usable embedded chat template: {error}. Use an instruct/chat GGUF with tokenizer.chat_template metadata. No remote endpoint was tried."
        )
    })?;
    let template_source = template
        .to_string()
        .map_err(|error| anyhow::anyhow!("local GGUF model {model_spec:?} has invalid chat template metadata: {error}. No remote endpoint was tried."))?;
    let has_tool_result = messages.iter().any(|message| message.role == "tool");
    let native_tools = !tools.is_empty()
        && !has_tool_result
        && template_source.contains("<start_function_declaration>")
        && template_source.contains("<start_function_call>");
    let chat = render_messages(messages, tools, native_tools)?;
    let tool_grammar = if native_tools && !has_tool_result {
        let grammar = native_tool_call_grammar(tools);
        Some(
            LlamaSampler::grammar(&model, &grammar, "root").map_err(|error| {
                anyhow::anyhow!(
                    "local GGUF model {model_spec:?} cannot initialize its embedded chat-template tool grammar: {error}. Tool calling was refused before generation. No remote endpoint was tried."
                )
            })?,
        )
    } else if !native_tools && !tools.is_empty() && !has_tool_result {
        let schema = local_tool_response_schema(tools, false);
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
}
