//! Manages a `llama-server` subprocess hosting FunctionGemma and exposes a
//! tool-routing API.
//!
//! FunctionGemma is a 270M-parameter Gemma-3 derivative trained specifically
//! to translate (user query, available function schemas) → a structured
//! function call. Out of the box it emits its native call format inside the
//! OpenAI `content` field rather than `tool_calls`:
//!
//! ```text
//! <start_function_call>call:NAME{arg:<escape>value<escape>,...}<end_function_call>
//! ```
//!
//! We send chat completions through llama-server with stop sequences anchored
//! on `<end_function_call>` and `<end_of_turn>`, then parse the emitted text
//! back into a structured `ToolCall`.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::process::{Child, Command};

use crate::config::Config;
use crate::error::Error;
use crate::routing::ToolCall;

const READY_TIMEOUT_MS: u64 = 60_000;
const READY_POLL_MS: u64 = 250;

pub struct FunctionServer {
    child: Child,
    base_url: String,
    http: Client,
}

impl FunctionServer {
    pub async fn spawn(config: &Config) -> Result<Self> {
        let model = config
            .function_gguf
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("function_gguf path not configured"))?;
        if !model.exists() {
            return Err(Error::ModelMissing(model.clone()).into());
        }
        if !config.llama_server_bin.exists() && !is_on_path(&config.llama_server_bin) {
            return Err(Error::LlamaServerMissing(config.llama_server_bin.clone()).into());
        }

        let port = pick_free_port()?;
        tracing::info!(
            target: "prism_tool_router",
            port,
            model = %model.display(),
            "spawning function-router llama-server"
        );

        // FunctionGemma supports 32K context. We provision 8K which fits
        // top-K=8 schemas plus conversation tail comfortably, while keeping
        // the KV cache small enough that the 270M model spawns reliably on
        // an M-series GPU. ubatch defaults to 512 — that's the physical
        // batch size and what counts for the "input N too large" error;
        // bump it just enough to swallow long single-tool schemas.
        let child = Command::new(&config.llama_server_bin)
            .arg("--model")
            .arg(model)
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--ctx-size")
            .arg("8192")
            .arg("--batch-size")
            .arg("8192")
            .arg("--ubatch-size")
            .arg("2048")
            .arg("--no-webui")
            .arg("--log-disable")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawn llama-server (function)")?;

        let base_url = format!("http://127.0.0.1:{port}");
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("build http client")?;

        let server = Self {
            child,
            base_url,
            http,
        };
        server.wait_ready().await?;
        Ok(server)
    }

    async fn wait_ready(&self) -> Result<()> {
        let url = format!("{}/health", self.base_url);
        let started = std::time::Instant::now();
        // last_err is overwritten on every loop iteration that fails, then
        // surfaced via bail!() if the loop times out. Clippy's
        // unused_assignments fires on the initial value because both arms
        // of the match always overwrite it; the initial empty is a safety
        // net for an unreachable code path.
        #[allow(unused_assignments)]
        let mut last_err = String::new();
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

    /// Run FunctionGemma against a single user query with the given tool
    /// schemas. Returns the parsed tool call when the model emits one,
    /// or None when it doesn't.
    ///
    /// `tools` is the OpenAI-shape array forge would normally send to a chat
    /// model: `[{ "type": "function", "function": { name, description, parameters } }, ...]`.
    pub async fn route(&self, user_query: &str, tools: &[Value]) -> Result<Option<ToolCall>> {
        if tools.is_empty() {
            return Ok(None);
        }
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = json!({
            "model": "functiongemma",
            "messages": [
                {"role": "user", "content": user_query}
            ],
            "tools": tools,
            // FunctionGemma emits its native delimiters; without these stops
            // it'll happily loop until max_tokens.
            "stop": ["<end_function_call>", "<end_of_turn>"],
            "max_tokens": 256,
            "temperature": 0.0,
        });
        let resp = self.http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("function-router returned {status}: {text}");
        }
        let parsed: Value = resp.json().await?;
        let content = parsed
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let result = parse_function_call(&content);
        // Trace only when the model produced something we couldn't parse —
        // a passthrough on routable text is the noise we don't want.
        if result.is_none() && !content.trim().is_empty() {
            tracing::debug!(
                target: "prism_tool_router",
                raw = %content,
                "FunctionGemma output not parseable as call — passthrough"
            );
        }
        Ok(result)
    }

    pub async fn shutdown(mut self) {
        let _ = self.child.kill().await;
    }
}

fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn is_on_path(bin: &std::path::Path) -> bool {
    bin.parent()
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true)
}

/// Parse FunctionGemma's native call format:
///   call:NAME{key:<escape>val<escape>,key:val,...}
/// Returns None for any text that doesn't look like a function call (the
/// model said "I can't help" or just answered conversationally).
pub fn parse_function_call(text: &str) -> Option<ToolCall> {
    // Strip outer wrappers if present.
    let mut s = text.trim();
    if let Some(after) = s.strip_prefix("<start_function_call>") {
        s = after;
    }
    if let Some(before_end) = s.find("<end_function_call>") {
        s = &s[..before_end];
    }
    s = s.trim();
    let rest = s.strip_prefix("call:")?;

    // Find the brace that opens args. Tool name is everything before it.
    let open = rest.find('{')?;
    let name = rest[..open].trim().to_string();
    if name.is_empty() {
        return None;
    }
    // Match the closing brace, balancing nesting in case args contain JSON
    // objects.
    let body = &rest[open + 1..];
    let mut depth = 1usize;
    let mut end = 0usize;
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = if depth == 0 { &body[..end] } else { body };
    let args = parse_args(inner);
    Some(ToolCall {
        name,
        arguments: args,
    })
}

/// Parse the comma-separated key:value list inside the braces.
fn parse_args(inner: &str) -> Value {
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    let mut obj = serde_json::Map::new();
    while i < bytes.len() {
        i = skip_ws(bytes, i);
        let key_start = i;
        while i < bytes.len() && bytes[i] != b':' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key = inner[key_start..i].trim().to_string();
        i += 1; // consume ':'
        if key.is_empty() {
            break;
        }
        let (value, next) = read_value(inner, bytes, i);
        i = next;
        i = skip_ws(bytes, i);
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
        }
        obj.insert(key, value);
    }
    Value::Object(obj)
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

const ESC: &str = "<escape>";

fn read_value(s: &str, bytes: &[u8], start: usize) -> (Value, usize) {
    let i = skip_ws(bytes, start);
    // <escape>...<escape> string literal
    if s[i..].starts_with(ESC) {
        let body_start = i + ESC.len();
        if let Some(rel_end) = s[body_start..].find(ESC) {
            let body = &s[body_start..body_start + rel_end];
            return (
                Value::String(body.to_string()),
                body_start + rel_end + ESC.len(),
            );
        }
        // No closing escape — take the rest as-is.
        return (Value::String(s[body_start..].to_string()), bytes.len());
    }
    // Nested object — pass through to JSON.
    if i < bytes.len() && bytes[i] == b'{' {
        let mut depth = 0usize;
        let mut j = i;
        while j < bytes.len() {
            match bytes[j] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        let blob = &s[i..j];
        let parsed = serde_json::from_str(blob).unwrap_or(Value::String(blob.to_string()));
        return (parsed, j);
    }
    // Bareword: read until comma at top level.
    let mut j = i;
    while j < bytes.len() && bytes[j] != b',' {
        j += 1;
    }
    let raw = s[i..j].trim().to_string();
    let v = if let Ok(n) = raw.parse::<i64>() {
        Value::from(n)
    } else if let Ok(f) = raw.parse::<f64>() {
        Value::from(f)
    } else {
        match raw.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "null" => Value::Null,
            _ => Value::String(raw),
        }
    };
    (v, j)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_call() {
        let text =
            "<start_function_call>call:get_weather{city:<escape>Paris<escape>}<end_function_call>";
        let call = parse_function_call(text).expect("parse");
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.arguments["city"], "Paris");
    }

    #[test]
    fn parses_multi_arg_with_number() {
        let text =
            "call:book_table{party_size:2,city:<escape>Seattle<escape>,time:<escape>19:00<escape>}";
        let call = parse_function_call(text).expect("parse");
        assert_eq!(call.name, "book_table");
        assert_eq!(call.arguments["party_size"], 2);
        assert_eq!(call.arguments["city"], "Seattle");
        assert_eq!(call.arguments["time"], "19:00");
    }

    #[test]
    fn returns_none_for_non_call_text() {
        assert!(parse_function_call("I cannot help with that.").is_none());
        assert!(parse_function_call("").is_none());
    }
}
