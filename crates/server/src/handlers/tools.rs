//! Tool invocation handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Serialize;
use std::sync::Arc;

use crate::NodeState;

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

/// POST /api/tools/:name/run — tool execution is NOT wired on this endpoint.
///
/// Honest 501: this handler never dispatched anything — no queue, no channel,
/// no spawned task. It only checked the registry, wrote an audit entry falsely
/// marked `Success`, and returned a fake `"accepted"`. Real execution runs
/// through the TAOR agent loop (POST /api/chat) or the `prism run` CLI. We keep
/// the genuine 404 so callers can still distinguish "no such tool" from "exists
/// but not executable here", and no longer audit a success that never happened.
/// See AUDIT_BACKLOG 0.2.
pub async fn run_tool(
    State(state): State<Arc<NodeState>>,
    Path(name): Path<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    {
        let registry = state
            .tool_registry
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if registry.get(&name).is_none() {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Tool '{name}' not found in registry."),
                }),
            );
        }
    }

    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse {
            error: format!(
                "Tool '{name}' exists but this endpoint does not execute tools. \
                 Use POST /api/chat (agent loop) or `prism run` for execution."
            ),
        }),
    )
}
