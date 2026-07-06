//! Workflow endpoints — list, inspect, and run workflow specs over HTTP.
//!
//! The chat app (and any external client) gets the same workflow engine the
//! CLI and agent use: specs discovered from the standard search paths,
//! executed with the real engine (`prism_workflows::execute_workflow`).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::NodeState;

fn discover() -> Result<BTreeMap<String, prism_workflows::WorkflowSpec>, String> {
    prism_workflows::discover_workflows(None).map_err(|e| e.to_string())
}

/// GET /api/workflows — list discovered workflow specs.
pub async fn list_workflows(
    State(_state): State<Arc<NodeState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let specs = discover().map_err(internal)?;
    let items: Vec<Value> = specs
        .values()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "steps": s.steps.len(),
                "arguments": s.arguments.iter().map(|a| &a.name).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({ "workflows": items, "count": items.len() })))
}

/// GET /api/workflows/{name} — full spec for one workflow.
pub async fn get_workflow(
    State(_state): State<Arc<NodeState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let specs = discover().map_err(internal)?;
    match prism_workflows::find_workflow(&specs, &name) {
        Some(spec) => Ok(Json(serde_json::to_value(spec).unwrap_or_default())),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("workflow '{name}' not found") })),
        )),
    }
}

#[derive(Deserialize, Default)]
pub struct RunWorkflowRequest {
    /// Argument values keyed by argument name.
    #[serde(default)]
    pub values: BTreeMap<String, String>,
    /// false = dry-run (resolve + plan, execute nothing). Defaults to true.
    #[serde(default = "default_true")]
    pub execute: bool,
}

fn default_true() -> bool {
    true
}

/// POST /api/workflows/{name}/run — execute a workflow with the real engine.
pub async fn run_workflow(
    State(_state): State<Arc<NodeState>>,
    Path(name): Path<String>,
    body: Option<Json<RunWorkflowRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let specs = discover().map_err(internal)?;
    let Some(spec) = prism_workflows::find_workflow(&specs, &name) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("workflow '{name}' not found") })),
        ));
    };

    let result = prism_workflows::execute_workflow(spec, &req.values, req.execute)
        .await
        .map_err(internal_display)?;
    Ok(Json(serde_json::to_value(result).unwrap_or_default()))
}

fn internal(e: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
}

fn internal_display(e: anyhow::Error) -> (StatusCode, Json<Value>) {
    internal(e.to_string())
}
