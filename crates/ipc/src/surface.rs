//! External-frontend JSON-RPC surface.
//!
//! This is the LSP-server analog for the PRISM agent: an external frontend
//! (PRISM Desktop, an IDE extension) drives the agent by exchanging JSON-RPC
//! 2.0 messages over the server's stdin/stdout — one JSON object per line.
//!
//! The surface is a thin adapter. [`dispatch`] maps a small, honest method set
//! onto an [`AgentBackend`] implementation; it contains no agent business
//! logic of its own. The real implementation ([`crate::bridge::BackendBridge`])
//! drives a live `prism backend` child; tests use an in-process stub.
//!
//! # Trust boundary
//!
//! stdio is a **same-user** channel: whoever spawned this process already runs
//! as the user and can drive the agent directly. The surface therefore does no
//! authentication and **opens no network socket** — exposing it beyond the
//! local process (over a TCP/WebSocket listener) would cross a trust boundary
//! this layer does not defend and MUST NOT be added here.

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::Value;

use crate::methods;
use crate::types::{RpcRequest, RpcResponse};

// Standard JSON-RPC 2.0 error codes (spec §5.1).
const PARSE_ERROR: i32 = -32700;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// The minimal set of agent operations an external frontend can drive.
///
/// Each method returns the JSON `result` payload for its JSON-RPC response, or
/// an error that [`dispatch`] reports as a `-32603` internal error. Kept
/// deliberately small — this is the contract the surface promises, not the full
/// native backend protocol.
#[allow(async_fn_in_trait)] // Impls and callers are in-crate; no `Send` bound needed.
pub trait AgentBackend {
    /// Version + capabilities of the backend (handshake).
    async fn initialize(&mut self) -> Result<Value>;

    /// The tools the agent can call.
    async fn list_tools(&mut self) -> Result<Value>;

    /// Current session status (model, mode, message count).
    async fn session_status(&mut self) -> Result<Value>;

    /// Drive one agent turn with `text`; returns the assistant reply.
    async fn chat_send(&mut self, text: &str) -> Result<Value>;
}

/// Map one JSON-RPC request onto the backend and build its response.
pub async fn dispatch<B: AgentBackend>(request: &RpcRequest, backend: &mut B) -> RpcResponse {
    let id = request.id;
    let result = match request.method.as_str() {
        methods::INITIALIZE => backend.initialize().await,
        methods::TOOLS_LIST => backend.list_tools().await,
        methods::SESSION_STATUS => backend.session_status().await,
        methods::CHAT_SEND => match request.params.get("text").and_then(Value::as_str) {
            Some(text) if !text.is_empty() => backend.chat_send(text).await,
            _ => {
                return RpcResponse::err(
                    id,
                    INVALID_PARAMS,
                    "chat/send requires a non-empty string `params.text`".into(),
                );
            }
        },
        other => {
            return RpcResponse::err(id, METHOD_NOT_FOUND, format!("Method not found: {other}"));
        }
    };

    match result {
        Ok(value) => RpcResponse::ok(id, value),
        Err(error) => RpcResponse::err(id, INTERNAL_ERROR, error.to_string()),
    }
}

/// Serve the JSON-RPC surface on stdin/stdout until EOF.
///
/// Reads one JSON-RPC request per line, dispatches it, and writes one response
/// per line. Malformed lines get a `-32700` parse error; the loop keeps going.
/// All diagnostics MUST go to stderr — stdout is the protocol channel.
pub async fn serve_stdio<B: AgentBackend>(mut backend: B) -> Result<()> {
    let mut reader = std::io::BufReader::new(std::io::stdin());
    let stdout = std::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break; // EOF: frontend closed the pipe.
        }
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(request) => dispatch(&request, &mut backend).await,
            Err(error) => RpcResponse::err(None, PARSE_ERROR, format!("Parse error: {error}")),
        };
        write_response(&stdout, &response)?;
    }

    Ok(())
}

fn write_response(stdout: &std::io::Stdout, response: &RpcResponse) -> Result<()> {
    let json = serde_json::to_string(response)?;
    let mut out = stdout.lock();
    writeln!(out, "{json}")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// In-process stub — mirrors the tui `FakeBackend` pattern: deterministic
    /// canned responses, no subprocess, no network.
    #[derive(Default)]
    struct StubBackend {
        last_prompt: Option<String>,
    }

    impl AgentBackend for StubBackend {
        async fn initialize(&mut self) -> Result<Value> {
            Ok(json!({ "version": "test", "capabilities": { "tool_count": 2 } }))
        }
        async fn list_tools(&mut self) -> Result<Value> {
            Ok(json!({ "tools": [{ "name": "read_file" }, { "name": "execute_bash" }] }))
        }
        async fn session_status(&mut self) -> Result<Value> {
            Ok(json!({ "model": "stub-model", "session_mode": "chat", "message_count": 0 }))
        }
        async fn chat_send(&mut self, text: &str) -> Result<Value> {
            self.last_prompt = Some(text.to_string());
            Ok(json!({ "reply": format!("echo: {text}") }))
        }
    }

    fn call(method: &str, params: Value) -> RpcRequest {
        RpcRequest::new(method, params, 1)
    }

    #[tokio::test]
    async fn initialize_round_trip() {
        let mut backend = StubBackend::default();
        let resp = dispatch(&call(methods::INITIALIZE, Value::Null), &mut backend).await;
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(1));
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["version"], "test");
        assert_eq!(result["capabilities"]["tool_count"], 2);
    }

    #[tokio::test]
    async fn tools_list_round_trip() {
        let mut backend = StubBackend::default();
        let resp = dispatch(&call(methods::TOOLS_LIST, Value::Null), &mut backend).await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["tools"][0]["name"], "read_file");
    }

    #[tokio::test]
    async fn session_status_round_trip() {
        let mut backend = StubBackend::default();
        let resp = dispatch(&call(methods::SESSION_STATUS, Value::Null), &mut backend).await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["model"], "stub-model");
    }

    #[tokio::test]
    async fn chat_send_drives_a_turn() {
        let mut backend = StubBackend::default();
        let resp = dispatch(
            &call(methods::CHAT_SEND, json!({ "text": "hello" })),
            &mut backend,
        )
        .await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["reply"], "echo: hello");
        assert_eq!(backend.last_prompt.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn chat_send_missing_text_is_invalid_params() {
        let mut backend = StubBackend::default();
        let resp = dispatch(&call(methods::CHAT_SEND, json!({})), &mut backend).await;
        assert!(resp.result.is_none());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
        // The turn must NOT have run.
        assert!(backend.last_prompt.is_none());
    }

    #[tokio::test]
    async fn chat_send_empty_text_is_invalid_params() {
        let mut backend = StubBackend::default();
        let resp = dispatch(
            &call(methods::CHAT_SEND, json!({ "text": "" })),
            &mut backend,
        )
        .await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let mut backend = StubBackend::default();
        let resp = dispatch(&call("does/not/exist", Value::Null), &mut backend).await;
        assert!(resp.result.is_none());
        let error = resp.error.unwrap();
        assert_eq!(error.code, METHOD_NOT_FOUND);
        assert!(error.message.contains("does/not/exist"));
    }

    #[tokio::test]
    async fn response_id_echoes_request_id() {
        let mut backend = StubBackend::default();
        let mut req = call(methods::INITIALIZE, Value::Null);
        req.id = Some(4242);
        let resp = dispatch(&req, &mut backend).await;
        assert_eq!(resp.id, Some(4242));
    }
}
