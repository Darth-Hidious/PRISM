//! Workflow endpoints — list, inspect, and run workflow specs over HTTP.
//!
//! The chat app (and any external client) gets the same workflow engine the
//! CLI and agent use: specs discovered from the standard search paths,
//! executed with the real engine (`prism_workflows::execute_workflow_with_policy`)
//! under the same OPA/Rego policy the agent loop enforces. The caller's role
//! is taken from the authenticated RBAC context (never from request `values`).

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

use prism_core::rbac::LocalRole;

use crate::NodeState;
use crate::middleware::{AuthenticatedUser, UserRole};

/// Map an authenticated RBAC role to the policy engine's role vocabulary
/// (`admin` / `operator` / `agent` / `viewer`). This is the ONLY source of
/// the policy role for server-run workflows — it is derived from the
/// authenticated identity, not from caller-supplied input.
fn policy_role(role: LocalRole) -> &'static str {
    match role {
        LocalRole::NodeAdmin => "admin",
        LocalRole::Engineer => "operator",
        // Analyst/Viewer have no execute privilege; policy denies them.
        LocalRole::Analyst | LocalRole::Viewer => "viewer",
    }
}

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

/// POST /api/workflows/{name}/run — execute a workflow with the real engine
/// under OPA/Rego policy. The route is already gated by `require_permission`,
/// but the workflow engine additionally enforces per-step tool policy using
/// the caller's authenticated role.
pub async fn run_workflow(
    State(_state): State<Arc<NodeState>>,
    Path(name): Path<String>,
    role_ext: Option<Extension<UserRole>>,
    user_ext: Option<Extension<AuthenticatedUser>>,
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

    // Wire the SAME policy engine the agent loop uses (built-in + discovered
    // OPA/Rego). If it can't be constructed, fail the request honestly —
    // running a workflow policy-less would silently bypass the gate.
    let mut policy = prism_policy::PolicyEngine::with_discovery(None).map_err(|e| {
        tracing::error!(error = %e, "policy engine failed to load — refusing to run workflow policy-less");
        internal(format!("policy engine unavailable: {e}"))
    })?;

    // Role and principal come from the authenticated RBAC context, never from
    // request `values`. Missing role ⇒ least-privileged (denied by policy).
    let role = role_ext
        .map(|Extension(UserRole(r))| policy_role(r))
        .unwrap_or("viewer");
    let principal = user_ext
        .map(|Extension(u)| u.user_id)
        .unwrap_or_else(|| "unknown".to_string());

    let result = prism_workflows::execute_workflow_with_policy(
        spec,
        &req.values,
        req.execute,
        Some(&mut policy),
        Some(principal.as_str()),
        Some(role),
    )
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
