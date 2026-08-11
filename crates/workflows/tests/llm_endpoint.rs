// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
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
use axum::http::HeaderMap;
use axum::response::Json;
use axum::routing::post;
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<Value>>>,
    auth_headers: Arc<Mutex<Vec<Option<String>>>>,
    api_key_headers: Arc<Mutex<Vec<Option<String>>>>,
}

async fn fake_chat_completions(
    State(seen): State<Seen>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    seen.auth_headers.lock().unwrap().push(auth);
    let api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    seen.api_key_headers.lock().unwrap().push(api_key);
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

/// Regression for the workflow credential-exfiltration door: a caller may
/// choose an LLM endpoint, but that endpoint must not receive the node's
/// MARC27 credential.
#[tokio::test(flavor = "multi_thread")]
async fn caller_supplied_llm_endpoint_never_receives_node_credential() {
    let seen = Seen::default();
    let llm_port = spawn_llm(seen.clone()).await;
    let spec = single_llm_workflow();

    let mut values = BTreeMap::new();
    values.insert(
        "llm_base_url".to_string(),
        format!("http://127.0.0.1:{llm_port}/v1"),
    );
    let options = prism_workflows::WorkflowExecutionOptions {
        trusted_llm_base_url: Some(format!("http://127.0.0.1:{llm_port}/v1")),
        trusted_llm_api_key: Some("node-secret-token".to_string()),
        trusted_llm_credential_kind: None,
        caller_supplied_llm_base_url: true,
        trusted_node_port: None,
        trusted_node_token: None,
    };
    let result = prism_workflows::execute_workflow_with_policy_and_options(
        &spec, &values, true, None, None, None, &options,
    )
    .await;

    result.expect("caller-selected endpoint should still be usable");
    assert_eq!(
        seen.auth_headers.lock().unwrap().as_slice(),
        &[None],
        "caller-selected endpoint must receive no node credential"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_endpoint_without_launcher_key_never_uses_marc27_token() {
    let seen = Seen::default();
    let llm_port = spawn_llm(seen.clone()).await;
    let spec = single_llm_workflow();
    let options = prism_workflows::WorkflowExecutionOptions {
        trusted_llm_base_url: Some(format!("http://127.0.0.1:{llm_port}/v1")),
        trusted_llm_api_key: None,
        trusted_llm_credential_kind: None,
        caller_supplied_llm_base_url: false,
        trusted_node_port: None,
        trusted_node_token: None,
    };

    prism_workflows::execute_workflow_with_policy_and_options(
        &spec,
        &BTreeMap::new(),
        true,
        None,
        None,
        None,
        &options,
    )
    .await
    .expect("trusted endpoint without a launcher key remains callable");
    assert_eq!(
        seen.auth_headers.lock().unwrap().as_slice(),
        &[None],
        "MARC27_TOKEN must not be attached without a paired launcher key"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_llm_endpoint_keeps_its_paired_credential() {
    let seen = Seen::default();
    let llm_port = spawn_llm(seen.clone()).await;
    let spec = single_llm_workflow();
    let options = prism_workflows::WorkflowExecutionOptions {
        trusted_llm_base_url: Some(format!("http://127.0.0.1:{llm_port}/v1")),
        trusted_llm_api_key: Some("trusted-node-key".to_string()),
        trusted_llm_credential_kind: None,
        caller_supplied_llm_base_url: false,
        trusted_node_port: None,
        trusted_node_token: None,
    };

    prism_workflows::execute_workflow_with_policy_and_options(
        &spec,
        &BTreeMap::new(),
        true,
        None,
        None,
        None,
        &options,
    )
    .await
    .expect("trusted endpoint should remain usable");
    assert_eq!(
        seen.auth_headers.lock().unwrap().as_slice(),
        &[Some("Bearer trusted-node-key".to_string())]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_platform_api_key_keeps_explicit_wire_kind() {
    let seen = Seen::default();
    let llm_port = spawn_llm(seen.clone()).await;
    let spec = single_llm_workflow();
    let options = prism_workflows::WorkflowExecutionOptions {
        trusted_llm_base_url: Some(format!("http://127.0.0.1:{llm_port}/v1")),
        trusted_llm_api_key: Some("provider-defined-key".to_string()),
        trusted_llm_credential_kind: Some(prism_llm::LlmCredentialKind::ApiKey),
        caller_supplied_llm_base_url: false,
        trusted_node_port: None,
        trusted_node_token: None,
    };

    prism_workflows::execute_workflow_with_policy_and_options(
        &spec,
        &BTreeMap::new(),
        true,
        None,
        None,
        None,
        &options,
    )
    .await
    .expect("trusted API-key endpoint should remain usable");
    assert_eq!(seen.auth_headers.lock().unwrap().as_slice(), &[None]);
    assert_eq!(
        seen.api_key_headers.lock().unwrap().as_slice(),
        &[Some("provider-defined-key".to_string())]
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
