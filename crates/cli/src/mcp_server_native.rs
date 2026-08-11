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
use prism_agent::command_tools::{
    CommandToolRuntime, command_tool_requires_approval, command_tools, execute_command_tool,
};
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
        // No node-held LLM credential is handed to tools spawned by the native
        // MCP server. `resolve_workflow_llm_pair` yields only (base_url, model)
        // — there is no key to pass — and an MCP client is an arbitrary external
        // process, so `None` is also the safe default. The workflow engine
        // does not fall back to process platform credentials when a trusted
        // endpoint has no paired key, and a caller-supplied `--llm-url` can
        // therefore never receive a node credential.
        llm_api_key: None,
        llm_credential_kind: None,
    };

    // OPA policy engine for `tools/call` — the same gate the agent loop (h4)
    // and manual `/command` dispatch run behind. An MCP host is an unattended
    // caller with no human at the keyboard, so it gets the same standard, not
    // a bypass. Fail-closed: if the engine cannot initialize, `None` makes
    // every tool call refuse rather than run unchecked.
    let mut policy = match prism_policy::PolicyEngine::with_discovery(Some(&runtime.project_root)) {
        Ok(engine) => Some(engine),
        Err(error) => {
            eprintln!(
                "[prism-mcp-native] policy engine failed to initialize — \
                 every tool call will be refused (fail-closed): {error:#}"
            );
            None
        }
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

        let response = match dispatch(method, params, &runtime, &mut policy).await {
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

/// An MCP tool result carrying a refusal or failure the host model must see.
///
/// MCP convention: tool-level failures are `isError: true` RESULTS, not
/// JSON-RPC protocol errors — a protocol error tells the host the server
/// broke; an error result tells its model the call was denied and why.
fn tool_error(text: String) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": true,
    })
}

async fn dispatch(
    method: &str,
    params: Value,
    runtime: &CommandToolRuntime,
    policy: &mut Option<prism_policy::PolicyEngine>,
) -> Result<Value> {
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
                        // Not part of the MCP core schema (hosts ignore unknown
                        // fields), but without it a host cannot distinguish a
                        // gated tool from an unattended one — it would offer
                        // its model tools this server is going to refuse.
                        "requires_approval": t.requires_approval,
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

            // Approval gate. An MCP host has nobody at the keyboard and this
            // protocol carries no approval round-trip, so approval-gated
            // tools are refused outright — the same standard the single-tool
            // executor applies to relay callers (agent/src/service.rs).
            if command_tool_requires_approval(name) == Some(true) {
                return Ok(tool_error(format!(
                    "'{name}' is approval-gated and cannot run over MCP: this \
                     server has no human at the keyboard to approve it. Run it \
                     from the PRISM TUI, where the approval prompt is shown."
                )));
            }

            // OPA policy gate, fail-closed — mirrors the agent loop's h4
            // check (role "agent": unattended automation; the principal names
            // the actual caller class for policy authors and audit).
            let Some(engine) = policy.as_mut() else {
                return Ok(tool_error(format!(
                    "'{name}' refused: the OPA policy engine failed to \
                     initialize and policy cannot be bypassed (fail-closed). \
                     Check ~/.prism/policies and .prism/policies for invalid \
                     .rego files."
                )));
            };
            let policy_input = prism_policy::PolicyInput {
                action: "tool.call".to_string(),
                principal: "mcp-host".to_string(),
                role: "agent".to_string(),
                resource: name.to_string(),
                context: args.clone(),
            };
            if let prism_policy::GateOutcome::Deny { reason } =
                prism_policy::gate_outcome(engine.evaluate(&policy_input))
            {
                return Ok(tool_error(format!(
                    "'{name}' denied by OPA policy: {reason}"
                )));
            }

            // The engine rides into execution too, so a `workflow_run`
            // reaching the workflow engine gets the same `workflow.execute`
            // evaluation the chat path performs.
            let result = execute_command_tool(runtime, name, &args, policy.as_mut())
                .await
                .with_context(|| format!("tool {name} failed"))?;

            // Command tools report execution failure in-band as
            // `"success": false` — reflect it instead of hardcoding
            // `isError: false` over a failed run.
            let is_error = result.get("success").and_then(Value::as_bool) == Some(false);

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
                "isError": is_error,
            }))
        }

        "ping" => Ok(json!({})),

        other => Err(anyhow::anyhow!("unknown method: {other}")),
    }
}
