//! Goal endpoints — long-running research goals over HTTP.
//!
//! Goals are the durable objects behind long-running research: campaign
//! checkpoints at `~/.prism/campaigns/*.json` (goal, state, candidates,
//! progress). Reads report from checkpoints; `POST /api/goals` and
//! `POST /api/goals/{id}/resume` START/RESUME goals through the SAME
//! single-tool executor the agent and the relay use (`goal_start` /
//! `goal_resume`, detached workers) — one execution path, one audit trail,
//! full CLI parity for programmatic callers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::NodeState;
use crate::middleware::AuthenticatedUser;
use axum::Extension;

fn campaigns_dir() -> std::path::PathBuf {
    dirs_home().join(".prism").join("campaigns")
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn summarize(id: &str, raw: &Value) -> Value {
    // Checkpoints are engine-owned; read defensively so a schema change
    // degrades to fewer fields, never a 500.
    let state = raw.get("state").unwrap_or(raw);
    json!({
        "id": id,
        "goal": raw.get("goal").or_else(|| state.get("goal")),
        "candidates_evaluated": state
            .get("candidates")
            .and_then(|c| c.as_array())
            .map(|a| a.len()),
        "iteration": state.get("iteration"),
        "created": raw.get("created_at").or_else(|| state.get("created_at")),
    })
}

/// Run a goal verb through the node's single-tool executor. Shared by
/// create/resume so both get the same 503-when-no-executor honesty, the same
/// audit trail, and the same detached-worker semantics as the agent.
async fn run_goal_tool(
    state: &Arc<NodeState>,
    user: &AuthenticatedUser,
    tool: &str,
    args: Value,
) -> Response {
    let Some(service) = state.chat.get().cloned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "Goal engine is not running on this node — it needs a \
                          configured LLM ([chat] in ~/.prism/config.toml) and a \
                          Python tool environment. Check node logs."
            })),
        )
            .into_response();
    };
    // This endpoint is authenticated + RBAC-gated (ExecuteTools); hitting it
    // IS the explicit approval — same contract as `approve: true` on
    // /api/tools/{name}/run.
    match service
        .invoke_tool(tool, args, Some(&user.user_id), true)
        .await
    {
        Ok(result) => (
            StatusCode::ACCEPTED,
            Json(json!({ "tool": tool, "result": result })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": format!("{tool} failed: {e:#}") })),
        )
            .into_response(),
    }
}

/// POST /api/goals — start a long-running research goal (detached).
///
/// Body mirrors the `goal_start` tool schema: `goal` (required), `elements`,
/// `objective`, `max_iterations`, `batch_size`, `budget_usd`,
/// `approval_gates`. Returns 202 with the tool result carrying the goal id;
/// poll `GET /api/goals/{id}`.
pub async fn create_goal(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    body: Option<Json<Value>>,
) -> Response {
    let args = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    if args
        .get("goal")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "`goal` (non-empty string) is required" })),
        )
            .into_response();
    }
    run_goal_tool(&state, &user, "goal_start", args).await
}

/// POST /api/goals/{id}/resume — resume a paused goal (detached).
pub async fn resume_goal(
    State(state): State<Arc<NodeState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> Response {
    if id.contains('/') || id.contains("..") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid goal id" })),
        )
            .into_response();
    }
    run_goal_tool(&state, &user, "goal_resume", json!({ "id": id })).await
}

/// GET /api/goals — list campaign goals on this node.
pub async fn list_goals(
    State(_state): State<Arc<NodeState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let dir = campaigns_dir();
    let mut goals: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                Some(raw) => goals.push(summarize(&id, &raw)),
                None => goals.push(json!({ "id": id, "error": "unreadable checkpoint" })),
            }
        }
    }
    Ok(Json(json!({
        "goals": goals,
        "count": goals.len(),
        "source": dir.display().to_string(),
    })))
}

/// GET /api/goals/{id} — full checkpoint for one goal.
pub async fn get_goal(
    State(_state): State<Arc<NodeState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Checkpoint ids are filenames — refuse separators outright.
    if id.contains('/') || id.contains("..") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid goal id" })),
        ));
    }
    let path = campaigns_dir().join(format!("{id}.json"));
    match std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        Some(raw) => Ok(Json(raw)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("goal '{id}' not found") })),
        )),
    }
}
