// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Endpoint resolution for workflow `llm_*` (`action: llm`) steps.
//!
//! An `llm` step resolves its endpoint + model with the precedence:
//! step config → context (`llm_base_url` / `llm_model`, injected by the
//! agent/CLI from the SAME resolved chat config the chat path uses) → env →
//! built-in default. Before the fix nothing injected `llm_base_url`/`llm_model`
//! from the resolved config, so agent-driven `llm_*` steps fell through to the
//! engine's dead `127.0.0.1:8081` default.
//!
//! These tests drive the engine against a fake OpenAI-compatible endpoint and
//! prove the step calls the injected base URL with the injected model.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn fake_chat_completions(State(seen): State<Seen>, Json(body): Json<Value>) -> Json<Value> {
    seen.requests.lock().unwrap().push(body);
    Json(json!({
        "choices": [{ "message": { "content": "ack" } }]
    }))
}

async fn spawn_llm(seen: Seen) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(fake_chat_completions))
        .with_state(seen);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

fn single_llm_workflow() -> prism_workflows::WorkflowSpec {
    prism_workflows::load_workflow_from_str(
        r#"
kind: workflow
name: llm_probe
command_name: llm_probe
arguments:
  - name: llm_base_url
  - name: llm_model
steps:
  - id: think
    action: llm
    prompt: "summarize titanium properties"
"#,
        "inline:llm_probe",
    )
    .expect("probe workflow must load")
}

/// An `llm` step calls the injected base URL with the injected model — the
/// exact context values the agent/CLI thread from the resolved chat config.
#[tokio::test(flavor = "multi_thread")]
async fn llm_step_uses_injected_endpoint_and_model() {
    let seen = Seen::default();
    let llm_port = spawn_llm(seen.clone()).await;
    let spec = single_llm_workflow();

    let mut values = BTreeMap::new();
    values.insert(
        "llm_base_url".to_string(),
        format!("http://127.0.0.1:{llm_port}/v1"),
    );
    values.insert("llm_model".to_string(), "claude-sonnet-5".to_string());

    let result = prism_workflows::execute_workflow(&spec, &values, true)
        .await
        .expect("llm step must reach the injected endpoint");

    assert_eq!(result.steps.len(), 1);
    assert_eq!(result.steps[0].status, "completed");

    let requests = seen.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "the model must have been called once");
    assert_eq!(
        requests[0].get("model").and_then(Value::as_str),
        Some("claude-sonnet-5"),
        "the step must send the injected model, not the built-in default"
    );
}

/// Guard: with NO injected model (only base_url), the step still runs against
/// the injected endpoint — proving the new `llm_model` context read is additive
/// and does not regress the base_url-only path. (The exact fallback model id is
/// env-dependent, so this asserts reachability, not the model string.)
#[tokio::test(flavor = "multi_thread")]
async fn llm_step_runs_with_only_base_url_injected() {
    let seen = Seen::default();
    let llm_port = spawn_llm(seen.clone()).await;
    let spec = single_llm_workflow();

    let mut values = BTreeMap::new();
    values.insert(
        "llm_base_url".to_string(),
        format!("http://127.0.0.1:{llm_port}/v1"),
    );

    let result = prism_workflows::execute_workflow(&spec, &values, true)
        .await
        .expect("llm step must still run with only base_url injected");
    assert_eq!(result.steps[0].status, "completed");

    let requests = seen.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the step must still reach the injected endpoint"
    );
    assert!(
        requests[0].get("model").and_then(Value::as_str).is_some(),
        "a model id must always be sent"
    );
}
