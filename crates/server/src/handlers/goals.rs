//! Goal endpoints — surface long-running campaign goals over HTTP.
//!
//! Goals are the durable objects behind long-running research: campaign
//! checkpoints at `~/.prism/campaigns/*.json` (goal, state, candidates,
//! progress). This read surface lets the chat app / dashboard show what the
//! system is working toward without going through the agent. Creation and
//! resumption stay with the engine (CLI `prism campaign ...` / the agent) —
//! this endpoint reports, it does not orchestrate.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::NodeState;

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
