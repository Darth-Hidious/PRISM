//! Tool invocation handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use serde::Serialize;
use std::sync::Arc;

use crate::middleware::AuthenticatedUser;
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
pub struct RunResponse {
    pub status: &'static str,
    pub message: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// GET /api/tools — list available tools from the registry.
pub async fn list_tools(State(state): State<Arc<NodeState>>) -> Json<Vec<ToolInfo>> {
    let registry = state.tool_registry.read().unwrap_or_else(|e| e.into_inner());
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

/// POST /api/tools/:name/run — invoke a tool by name.
pub async fn run_tool(
    State(state): State<Arc<NodeState>>,
    user: Option<Extension<AuthenticatedUser>>,
    Path(name): Path<String>,
) -> Result<Json<RunResponse>, (StatusCode, Json<ErrorResponse>)> {
    let registry = state.tool_registry.read().unwrap_or_else(|e| e.into_inner());
    if registry.get(&name).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Tool '{name}' not found in registry."),
            }),
        ));
    }

    let user_id = user.as_ref().map(|u| u.user_id.as_str()).unwrap_or("anonymous");
    state.audit_and_broadcast(&prism_core::audit::AuditEntry {
        id: 0,
        timestamp: chrono::Utc::now(),
        user_id: user_id.to_string(),
        action: prism_core::audit::AuditAction::ToolExecution,
        target: name.clone(),
        detail: None,
        outcome: prism_core::audit::AuditOutcome::Success,
    });

    // Tool execution is a long-running operation — for now return acknowledgment.
    // Full execution is handled by the TAOR agent loop.
    Ok(Json(RunResponse {
        status: "accepted",
        message: format!("Tool '{name}' execution queued. Use `prism run` for interactive execution."),
    }))
}
