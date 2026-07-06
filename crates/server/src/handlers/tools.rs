//! Tool invocation handlers.

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use crate::NodeState;
use crate::middleware::AuthenticatedUser;

#[derive(Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub commands: Vec<ToolCommandInfo>,
}

#[derive(Serialize)]
pub struct ToolCommandInfo {
    pub name: String,
    pub description: String,
    pub args: Vec<ToolArgInfo>,
}

#[derive(Serialize)]
pub struct ToolArgInfo {
    pub name: String,
    pub arg_type: String,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// GET /api/tools — list available tools from the registry.
pub async fn list_tools(State(state): State<Arc<NodeState>>) -> Json<Vec<ToolInfo>> {
    let registry = state
        .tool_registry
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let tools = registry
        .list()
        .iter()
        .map(|entry| ToolInfo {
            name: entry.manifest.name.clone(),
            description: entry.manifest.description.clone(),
            version: entry.manifest.version.clone(),
            commands: entry
                .manifest
                .commands
                .iter()
                .map(|cmd| ToolCommandInfo {
                    name: cmd.name.clone(),
                    description: cmd.description.clone(),
                    args: cmd
                        .args
                        .iter()
                        .map(|a| ToolArgInfo {
                            name: a.name.clone(),
                            arg_type: a.arg_type.clone(),
                            required: a.required,
                            description: a.description.clone(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    Json(tools)
}

/// Build the args object a tool actually receives from the request body.
///
/// Three envelope forms are understood (checked in order):
///   * `{"args": { … }}`                    — explicit args, used verbatim
///     (relay-style / generic callers).
///   * `{"inputs": { … }, "command": "x"}`  — workflow `action: tool` shape:
///     `inputs` becomes the args and a non-null `command` is folded in as a
///     `command` kwarg (the Python tool server calls `tool.execute(**args)`,
///     so `command` is just another kwarg).
///   * a bare object `{ … }`                — the whole body is the args.
///
/// Anything that is not one of these (e.g. a JSON array/string body) yields an
/// empty args object; the tool then errors honestly on the missing inputs.
fn build_tool_args(body: &Value) -> Value {
    if let Some(args) = body.get("args") {
        return args.clone();
    }

    let has_envelope = body.get("inputs").is_some() || body.get("command").is_some();
    if !has_envelope {
        // No envelope keys → treat the entire object as the args.
        return if body.is_object() {
            body.clone()
        } else {
            Value::Object(Default::default())
        };
    }

    let mut args = match body.get("inputs") {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => Value::Object(Default::default()),
    };
    if let (Value::Object(map), Some(cmd)) = (&mut args, body.get("command"))
        && !cmd.is_null()
    {
        map.entry("command".to_string()).or_insert_with(|| cmd.clone());
    }
    args
}

/// POST /api/tools/:name/run — execute one tool once, deterministically.
///
/// Runs through the SAME executor a chat turn uses ([`ChatService::invoke_tool`]:
/// command-tool dispatch → Python/MCP tool server), minus the LLM. This is the
/// endpoint workflow `action: tool` steps call, and it returns the tool's real
/// result — no fake "accepted", no success audited for work that never ran
/// (the previous handler was an honest 501 that executed nothing; see
/// AUDIT_BACKLOG 0.2, now closed).
///
/// The tool runs as the authenticated caller (RBAC `ExecuteTools` is enforced
/// by the router layer). Unknown tools, and tools that fail, come back as
/// honest errors from the executor.
pub async fn run_tool(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Some(service) = state.chat.get().cloned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Tool executor is not running on this node — it needs a \
                        configured LLM ([chat] in ~/.prism/config.toml or [indexer] \
                        in prism.toml) and a Python tool environment. Check node logs."
                    .to_string(),
            }),
        )
            .into_response();
    };

    let body = body.map(|Json(v)| v).unwrap_or_else(|| Value::Object(Default::default()));
    let args = build_tool_args(&body);

    match service.invoke_tool(&name, args, Some(&user.user_id)).await {
        Ok(result) => Json(serde_json::json!({ "tool": name, "result": result })).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: format!("Tool '{name}' failed: {e:#}"),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::build_tool_args;
    use serde_json::json;

    #[test]
    fn explicit_args_used_verbatim() {
        let body = json!({ "args": { "formula": "Fe2O3" } });
        assert_eq!(build_tool_args(&body), json!({ "formula": "Fe2O3" }));
    }

    #[test]
    fn workflow_inputs_and_command_folded() {
        let body = json!({ "command": "train", "inputs": { "data": "d.csv", "target": "hardness" } });
        assert_eq!(
            build_tool_args(&body),
            json!({ "data": "d.csv", "target": "hardness", "command": "train" })
        );
    }

    #[test]
    fn null_command_is_not_folded() {
        let body = json!({ "command": null, "inputs": { "x": 1 } });
        assert_eq!(build_tool_args(&body), json!({ "x": 1 }));
    }

    #[test]
    fn bare_object_is_the_args() {
        let body = json!({ "formula": "Si", "relax": true });
        assert_eq!(build_tool_args(&body), json!({ "formula": "Si", "relax": true }));
    }

    #[test]
    fn caller_supplied_command_in_inputs_wins() {
        // If inputs already carries `command`, the envelope's command must not
        // clobber it (the tool author's explicit value takes precedence).
        let body = json!({ "command": "train", "inputs": { "command": "predict" } });
        assert_eq!(build_tool_args(&body), json!({ "command": "predict" }));
    }
}
