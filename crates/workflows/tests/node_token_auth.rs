// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Node-token authentication for workflow `tool` steps.
//!
//! A workflow `tool` step calls the local PRISM node at
//! `POST /api/tools/{name}/run`, which is auth-gated (session token +
//! `ExecuteTools`) whenever the node is online. The engine forwards the
//! reserved `_node_token` context value as `Authorization: Bearer <token>`.
//!
//! The CLI (`prism workflow run … --execute`) and the agent-tool path both
//! MINT a loopback session token and inject it under `_node_token`. The
//! interactive slash path (`/workflow run … --execute`) historically did
//! NOT — so its execute-mode tool steps hit the auth-gated node with no
//! credential and 401'd.
//!
//! These tests pin the mechanism the slash-path fix depends on:
//!   * values WITHOUT `_node_token` (what the old slash path produced) →
//!     the tool step 401s and the run fails, and
//!   * values WITH `_node_token` (what every fixed caller produces) →
//!     the node authenticates the call and the step completes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

const EXPECTED_BEARER: &str = "loopback-session-token-abc123";

#[derive(Clone, Default)]
struct Seen {
    auth_headers: Arc<Mutex<Vec<Option<String>>>>,
}

/// Auth-gated fake node: mirrors the real node's `POST /api/tools/{name}/run`,
/// but 401s any request whose `Authorization` header is not the expected
/// bearer — exactly how the online node gates tool execution.
async fn gated_run_tool(
    State(seen): State<Seen>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    seen.auth_headers.lock().unwrap().push(auth.clone());

    let authorized = auth.as_deref() == Some(&format!("Bearer {EXPECTED_BEARER}"));
    if !authorized {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing or invalid session token" })),
        );
    }
    (
        axum::http::StatusCode::OK,
        Json(json!({ "tool": name, "result": { "ok": true } })),
    )
}

async fn spawn_gated_node(seen: Seen) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/api/tools/{name}/run", post(gated_run_tool))
        .with_state(seen);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

fn single_tool_workflow() -> prism_workflows::WorkflowSpec {
    prism_workflows::load_workflow_from_str(
        r#"
kind: workflow
name: node_token_probe
command_name: node_token_probe
arguments:
  - name: node_port
    required: true
steps:
  - id: call_tool
    action: tool
    name: knowledge_search
    inputs:
      action: semantic
      query: "titanium"
"#,
        "inline:node_token_probe",
    )
    .expect("probe workflow must load")
}

/// REPRODUCTION: the value map the OLD slash path produced (user `--set`
/// values only, no `_node_token`) makes the tool step 401 against the
/// auth-gated node. This is the exact failure `/workflow run … --execute`
/// hit before the fix.
#[tokio::test(flavor = "multi_thread")]
async fn tool_step_401s_without_node_token() {
    let seen = Seen::default();
    let node_port = spawn_gated_node(seen.clone()).await;
    let spec = single_tool_workflow();

    // Slash-path-style values: no `_node_token` injected.
    let mut values = BTreeMap::new();
    values.insert("node_port".to_string(), node_port.to_string());

    let result = prism_workflows::execute_workflow(&spec, &values, true).await;

    let err = result.expect_err("tool step must fail without a node token");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("401"),
        "expected a 401 from the auth-gated node, got: {msg}"
    );
    // The node saw a call with no Authorization header.
    assert_eq!(
        seen.auth_headers.lock().unwrap().as_slice(),
        &[None],
        "the tokenless run must reach the node with no Authorization header"
    );
}

/// FIX PROOF: threading `_node_token` into the values (what the fixed slash
/// path, the CLI, and the agent-tool path all do) authenticates the call and
/// the step completes.
#[tokio::test(flavor = "multi_thread")]
async fn tool_step_succeeds_with_node_token() {
    let seen = Seen::default();
    let node_port = spawn_gated_node(seen.clone()).await;
    let spec = single_tool_workflow();

    let mut values = BTreeMap::new();
    values.insert("node_port".to_string(), node_port.to_string());
    values.insert("_node_token".to_string(), EXPECTED_BEARER.to_string());

    let result = prism_workflows::execute_workflow(&spec, &values, true)
        .await
        .expect("tool step must succeed with a valid node token");

    assert_eq!(result.steps.len(), 1);
    assert_eq!(result.steps[0].status, "completed");
    assert_eq!(
        seen.auth_headers.lock().unwrap().as_slice(),
        &[Some(format!("Bearer {EXPECTED_BEARER}"))],
        "the node must have received the forwarded bearer token"
    );
    // The reserved credential is stripped from the returned context.
    assert!(
        !result.context.contains_key("_node_token"),
        "the injected _node_token must not leak into the returned context"
    );
}
