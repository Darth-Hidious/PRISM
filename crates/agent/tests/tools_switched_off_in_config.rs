//! Owner 2026-09-06: "all tools should be in the plugin format, I can remove
//! them." A `[tools.<name>] enabled = false` table in the project config must
//! remove the tool from what the model is offered AND refuse a call by name —
//! before approval, before policy, before the tool server hears of it.
//! Own binary: the live catalog is process-global.
#[path = "support/agent_run_harness.rs"]
mod agent_run_harness;
mod common;

use std::path::Path;

use prism_agent::agent_loop;
use prism_agent::protocol::{AgentSeed, build_agent_seed};
use prism_agent::types::AgentEvent;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

/// One free tool, `probe`, which logs every call it receives.
const STUB_TOOL_SERVER_PY: &str = r#"
import sys, json, os
LOG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "calls.log")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req.get("method") == "list_tools":
        resp = {"tools": [{"name": "probe", "description": "Probe (test only).",
                           "input_schema": {"type": "object", "properties": {}},
                           "requires_approval": False}]}
    elif req.get("method") == "call_tool":
        with open(LOG, "a") as f:
            f.write(json.dumps(req) + "\n")
        resp = {"result": {"ok": True}}
    else:
        resp = {"error": "unknown method"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

/// Calls `probe` once, then answers OFF_DONE.
async fn start_stub_llm() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let tool_msgs = body["messages"]
                    .as_array()
                    .map(|m| m.iter().filter(|x| x["role"] == "tool").count())
                    .unwrap_or(0);
                let chunk = if tool_msgs == 0 {
                    serde_json::json!({ "choices": [{ "delta": { "tool_calls": [{
                    "index": 0, "id": "call_1",
                    "function": { "name": "probe", "arguments": "{}" } }] } }] })
                } else {
                    serde_json::json!({ "choices": [{ "delta": { "content": "OFF_DONE" } }] })
                };
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(format!(
                        "data: {chunk}\n\ndata: [DONE]\n\n"
                    )))
                    .expect("stub answer")
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub llm");
    let addr = listener.local_addr().expect("stub llm addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1")
}

fn write_project(dir: &Path) {
    let app = dir.join("app");
    std::fs::create_dir_all(&app).expect("app dir");
    std::fs::write(app.join("__init__.py"), "").expect("init");
    std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER_PY).expect("stub");
    std::fs::create_dir_all(dir.join(".prism")).expect(".prism dir");
    std::fs::write(
        dir.join(".prism").join("prism.toml"),
        "[tools.probe]\nenabled = false\n",
    )
    .expect("project config");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_switched_off_in_config_is_neither_offered_nor_run() {
    let project = tempfile::tempdir().expect("tempdir");
    write_project(project.path());
    let base_url = start_stub_llm().await;
    let llm_config = agent_run_harness::llm_config(base_url);
    let seed = build_agent_seed(
        &ToolServer {
            python_bin: "python3".into(),
            project_root: project.path().to_path_buf(),
            env: std::collections::BTreeMap::new(),
        },
        &llm_config,
    )
    .await
    .expect("seed");
    let AgentSeed {
        mut tool_server,
        subagent_lanes: _,
        command_tool_runtime,
        tools,
        config,
        hooks,
        permissions,
    } = seed;
    assert!(
        !tools.tool_names().iter().any(|n| n == "probe"),
        "a switched-off tool is not offered: {:?}",
        tools.tool_names()
    );

    prism_agent::hooks::set_provenance_ctx("tools-off", agent_run_harness::TEST_MODEL);
    let llm = LlmClient::new(llm_config);
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut answer = String::new();
    agent_loop::run_turn(
        &llm,
        &mut tool_server,
        &command_tool_runtime,
        &mut history,
        tools.as_ref(),
        config.as_ref(),
        "probe it",
        None,
        &mut transcript,
        hooks.as_ref(),
        &permissions,
        None,
        &mut scratchpad,
        &mut |event| {
            if let AgentEvent::TurnComplete {
                text: Some(text), ..
            } = event
                && !text.is_empty()
            {
                answer = text;
            }
        },
        None,
        None,
        None,
    )
    .await
    .expect("turn");
    assert_eq!(answer, "OFF_DONE");

    let refusal = history
        .iter()
        .find(|m| m.role == "tool")
        .and_then(|m| m.content.clone())
        .expect("the call by name gets an answer");
    assert!(
        refusal.contains("switched off") && refusal.contains("[tools.probe]"),
        "the answer names the switch: {refusal}"
    );
    assert!(
        !project.path().join("calls.log").exists(),
        "the tool server must never hear of a switched-off tool"
    );
}
