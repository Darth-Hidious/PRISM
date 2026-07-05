//! Deployment endpoints — the platform's deployment surface, proxied through
//! the node's authenticated `PlatformClient`.
//!
//! The chat app gets first-class deployment control (list / create / status /
//! stop) without re-implementing platform auth. All tenancy, solvency, and
//! billing enforcement stays platform-side — this is a pass-through, not a
//! second implementation.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::NodeState;

fn no_platform() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "no platform session — run `prism login` on this node first"
        })),
    )
}

fn upstream(e: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": format!("platform request failed: {e}") })),
    )
}

/// GET /api/deployments — list deployments visible to this node's session.
pub async fn list_deployments(
    State(state): State<Arc<NodeState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = state.platform_client.as_ref().ok_or_else(no_platform)?;
    let deployments: Value = client
        .get("/compute/deployments")
        .await
        .map_err(upstream)?;
    Ok(Json(deployments))
}

/// POST /api/deployments — create a deployment (body forwarded verbatim).
pub async fn create_deployment(
    State(state): State<Arc<NodeState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = state.platform_client.as_ref().ok_or_else(no_platform)?;
    let created: Value = client
        .post("/compute/deployments", &body)
        .await
        .map_err(upstream)?;
    Ok(Json(created))
}

/// GET /api/deployments/{id} — one deployment's status.
pub async fn get_deployment(
    State(state): State<Arc<NodeState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = state.platform_client.as_ref().ok_or_else(no_platform)?;
    let deployment: Value = client
        .get(&format!("/compute/deployments/{id}"))
        .await
        .map_err(upstream)?;
    Ok(Json(deployment))
}

/// DELETE /api/deployments/{id} — stop a deployment.
pub async fn stop_deployment(
    State(state): State<Arc<NodeState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = state.platform_client.as_ref().ok_or_else(no_platform)?;
    client
        .delete(&format!("/compute/deployments/{id}"))
        .await
        .map_err(upstream)?;
    Ok(Json(json!({ "stopped": id })))
}
