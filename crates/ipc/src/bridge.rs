//! [`AgentBackend`] implementation backed by a live `prism backend` child.
//!
//! This is the real adapter behind `prism ipc-serve`. It spawns `prism backend`
//! — the same native JSON-RPC stdio server the ratatui TUI drives — and
//! translates the small external surface ([`crate::surface`]) onto its native
//! protocol. No agent logic lives here: `initialize`/`tools/list`/`session/status`
//! surface the payloads the backend emits during its `init` handshake, and
//! `chat/send` forwards an `input.message` and drains the turn's streamed text.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::methods;
use crate::server::IpcServer;
use crate::surface::AgentBackend;
use crate::types::RpcRequest;

/// Max idle wait for the next line from the backend before giving up. Turns
/// stream text frequently, so a long-but-bounded gap catches a wedged child
/// without cutting off a working turn.
const RECV_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Drives a `prism backend` subprocess and adapts it to [`AgentBackend`].
pub struct BackendBridge {
    server: IpcServer,
    next_id: u64,
    /// `ui.welcome` payload captured during `init` (version, tool count, session id).
    welcome: Value,
    /// `ui.tools.catalog` payload captured during `init` (`{ "tools": [...] }`).
    tools: Value,
    /// `ui.status` payload captured during `init` (model, session mode, counts).
    status: Value,
}

impl BackendBridge {
    /// Spawn `prism backend` and complete its `init` handshake.
    ///
    /// `prism_binary` is the path to the current `prism` executable (spawn the
    /// same binary via `std::env::current_exe()`); it is invoked as
    /// `prism --python <python_bin> backend --project-root <project_root>`, the
    /// exact command the native TUI uses.
    pub async fn spawn(prism_binary: &str, project_root: &str, python_bin: &str) -> Result<Self> {
        let args = [
            "--python".to_string(),
            python_bin.to_string(),
            "backend".to_string(),
            "--project-root".to_string(),
            project_root.to_string(),
        ];
        let mut server = IpcServer::spawn(prism_binary, &args)
            .await
            .context("failed to spawn `prism backend`")?;

        // The backend answers `init` with a result, then emits ui.welcome,
        // ui.tools.catalog and finally ui.status. Drain up to ui.status.
        let init = RpcRequest::new(
            methods::BACKEND_INIT,
            json!({ "auto_approve": false, "resume": "" }),
            1,
        );
        server.send_request(&init).await?;

        let mut welcome = Value::Null;
        let mut tools = json!({ "tools": [] });
        // ui.status is the last init emission — its arrival ends the drain.
        let status = loop {
            let msg = recv_message(&mut server).await?;
            match msg.get("method").and_then(Value::as_str) {
                Some(methods::UI_WELCOME) => welcome = params(&msg),
                Some(methods::UI_TOOLS_CATALOG) => tools = params(&msg),
                Some(methods::UI_STATUS) => break params(&msg),
                _ => {} // init response or other notifications: ignore.
            }
        };

        Ok(Self {
            server,
            next_id: 2,
            welcome,
            tools,
            status,
        })
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

impl AgentBackend for BackendBridge {
    async fn initialize(&mut self) -> Result<Value> {
        // Honest capability report assembled from what the backend advertised.
        Ok(json!({
            "version": self.welcome.get("version").cloned().unwrap_or(Value::Null),
            "capabilities": {
                "protocol": "jsonrpc-2.0",
                "methods": [
                    methods::INITIALIZE,
                    methods::CHAT_SEND,
                    methods::TOOLS_LIST,
                    methods::SESSION_STATUS,
                ],
                "tool_count": self.welcome.get("tool_count").cloned().unwrap_or(Value::Null),
                "model": self.status.get("model").cloned().unwrap_or(Value::Null),
                "model_tool_selection": self
                    .welcome
                    .get("model_tool_selection")
                    .cloned()
                    .unwrap_or(Value::Null),
            },
        }))
    }

    async fn list_tools(&mut self) -> Result<Value> {
        Ok(self.tools.clone())
    }

    async fn session_status(&mut self) -> Result<Value> {
        Ok(self.status.clone())
    }

    async fn chat_send(&mut self, text: &str) -> Result<Value> {
        let id = self.next_id();
        let request = RpcRequest::new(methods::INPUT_MESSAGE, json!({ "text": text }), id);
        self.server.send_request(&request).await?;

        // Drain the turn: accumulate streamed text until ui.turn.complete.
        let mut reply = String::new();
        loop {
            let msg = recv_message(&mut self.server).await?;
            match msg.get("method").and_then(Value::as_str) {
                Some(methods::UI_TEXT_DELTA) => {
                    if let Some(chunk) = params(&msg).get("text").and_then(Value::as_str) {
                        reply.push_str(chunk);
                    }
                }
                Some(methods::UI_TURN_COMPLETE) => break,
                _ => {} // ack response, thinking deltas, tool cards, status: not part of the reply text.
            }
        }

        Ok(json!({ "reply": reply }))
    }
}

/// Receive and parse the next JSON line from the backend, with an idle timeout.
async fn recv_message(server: &mut IpcServer) -> Result<Value> {
    match timeout(RECV_IDLE_TIMEOUT, server.recv()).await {
        Ok(Some(line)) => serde_json::from_str(&line)
            .with_context(|| format!("backend sent malformed JSON: {line}")),
        Ok(None) => bail!("`prism backend` closed the connection"),
        Err(_) => bail!("`prism backend` went idle (no output within timeout)"),
    }
}

fn params(message: &Value) -> Value {
    message.get("params").cloned().unwrap_or(Value::Null)
}
