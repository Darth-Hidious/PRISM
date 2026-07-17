//! Native Rust MCP server for PRISM's Rust-side tools.
//!
//! Speaks the Model Context Protocol over stdin/stdout JSON-RPC so any MCP
//! host (forge, Claude Desktop, etc.) can spawn `prism mcp-server-native`
//! as a subprocess and call PRISM's Rust tools directly — no Python in the
//! execution path.
//!
//! Python tools (`app/tools/*.py`) are served separately by
//! `python -m app.mcp_server`. The two MCP servers complement each other:
//!
//!   forge ──┬─ MCP ──> prism mcp-server-native   (Rust tools)
//!           └─ MCP ──> python -m app.mcp_server  (Python tools)

use std::path::PathBuf;

use anyhow::{Context, Result};
use prism_agent::command_tools::{CommandToolRuntime, command_tools, execute_command_tool};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// JSON-RPC 2.0 protocol version this server advertises.
const JSONRPC_VERSION: &str = "2.0";
/// MCP protocol version we implement.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

pub async fn run(project_root: PathBuf, python_bin: PathBuf) -> Result<()> {
    // Resolve the chat LLM endpoint so MCP-hosted `workflow_run` calls point
    // their `llm_*` steps at the real model (same resolution the chat path
    // uses). Unresolvable (e.g. no PrismPaths) ⇒ None ⇒ env fallback.
    let (llm_base_url, llm_model) = match prism_runtime::PrismPaths::discover().ok() {
        Some(paths) => match crate::resolve_workflow_llm_pair(&project_root, &paths) {
            Some((base_url, model)) => (
                Some(base_url).filter(|s| !s.is_empty()),
                Some(model).filter(|s| !s.is_empty()),
            ),
            None => (None, None),
        },
        None => (None, None),
    };
    let runtime = CommandToolRuntime {
        current_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("prism")),
        project_root,
        python_bin,
        llm_base_url,
        llm_model,
    };

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[prism-mcp-native] parse error: {e}");
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // Notifications have no id — execute side effects, send no response.
        if id.is_none() {
            handle_notification(method, &params);
            continue;
        }

        let response = match dispatch(method, params, &runtime).await {
            Ok(result) => json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "result": result,
            }),
            Err(err) => json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "error": {
                    "code": -32603,
                    "message": err.to_string(),
                },
            }),
        };

        let mut text = serde_json::to_string(&response).context("encode response")?;
        text.push('\n');
        stdout.write_all(text.as_bytes()).await?;
        stdout.flush().await?;
    }

    Ok(())
}

fn handle_notification(method: &str, _params: &Value) {
    // We don't need to react to client-side notifications, but we log unknowns
    // so misbehaviour is visible in forge's MCP debug output.
    if method != "notifications/initialized" {
        eprintln!("[prism-mcp-native] unhandled notification: {method}");
    }
}

async fn dispatch(method: &str, params: Value, runtime: &CommandToolRuntime) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {},
            },
            "serverInfo": {
                "name": "prism-rust",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })),

        "tools/list" => {
            let tools: Vec<Value> = command_tools()
                .into_iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }

        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .context("missing 'name'")?;
            let args = params.get("arguments").cloned().unwrap_or(json!({}));

            let result = execute_command_tool(runtime, name, &args, None)
                .await
                .with_context(|| format!("tool {name} failed"))?;

            // MCP convention: return content array with text blocks. We
            // serialise the JSON result to a single text block — forge will
            // surface it to the LLM as the tool result.
            let text = match &result {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string_pretty(&result).unwrap_or_default(),
            };

            Ok(json!({
                "content": [
                    { "type": "text", "text": text }
                ],
                "isError": false,
            }))
        }

        "ping" => Ok(json!({})),

        other => Err(anyhow::anyhow!("unknown method: {other}")),
    }
}
