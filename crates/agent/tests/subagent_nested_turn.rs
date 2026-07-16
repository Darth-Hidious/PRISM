// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! spawn_subagent end-to-end: a REAL parent turn delegates to a REAL nested
//! turn through the same stub OpenAI-compatible LLM and stub Python tool
//! server used by `http_chat_parity.rs`.
//!
//! The stub LLM routes on the request's `model` field — the parent runs as
//! `stub-model`; the nested turn runs as the subagent default
//! `claude-fable-5` (same endpoint, model swapped by `spawn_subagent`). This
//! proves, with no live LLM:
//!
//! 1. the loop intercepts `spawn_subagent` and runs a nested `run_turn`,
//! 2. the nested turn is routed to the requested model,
//! 3. the nested turn can CALL TOOLS (the stub `stub_echo` executes exactly
//!    once through the shared Python tool server),
//! 4. the subagent's answer comes back to the parent as the tool result and
//!    the parent finishes its own turn on top of it,
//! 5. the depth cap refuses to spawn from an agent already at max depth.
//!
//! Requires `python3` on PATH; tests skip (with a note) when absent.

use std::path::{Path, PathBuf};

use prism_agent::agent_loop;
use prism_agent::protocol::build_agent_seed;
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

// ── Stub Python tool server ──────────────────────────────────────────

/// One python tool: `stub_echo` needs no approval and logs every execution
/// to `<project>/calls.log` so tests can assert the nested turn really ran it.
const STUB_TOOL_SERVER_PY: &str = r#"
import sys, json, os

LOG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "calls.log")

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "list_tools":
        resp = {"tools": [
            {
                "name": "stub_echo",
                "description": "Stub echo tool: returns a canned payload (test only).",
                "input_schema": {"type": "object", "properties": {}},
                "requires_approval": False,
            },
        ]}
    elif method == "call_tool":
        with open(LOG, "a") as f:
            f.write(json.dumps(req) + "\n")
        resp = {"result": {"ok": True, "tool": req.get("tool"), "payload": "ECHO_PAYLOAD"}}
    else:
        resp = {"error": "unknown method"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

fn find_python() -> Option<PathBuf> {
    let out = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    out.status.success().then(|| PathBuf::from("python3"))
}

fn write_stub_project(dir: &Path) {
    let app = dir.join("app");
    std::fs::create_dir_all(&app).expect("create app dir");
    std::fs::write(app.join("__init__.py"), "").expect("write __init__");
    std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER_PY).expect("write stub");
}

fn tool_server_config(project: &Path, python: &Path) -> ToolServer {
    ToolServer {
        python_bin: python.to_path_buf(),
        project_root: project.to_path_buf(),
        env: std::collections::BTreeMap::new(),
    }
}

// ── Stub OpenAI-compatible LLM (routes on the `model` field) ─────────

fn sse_text(text: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{ "delta": { "content": text } }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn sse_tool_call(tool: &str, arguments: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "function": { "name": tool, "arguments": arguments }
        }] } }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Serve `/v1/chat/completions` on an ephemeral port.
///
/// - `stub-model` (the parent): first asks for `spawn_subagent`, then — once
///   a tool result is the last message — answers "PARENT_DONE".
/// - `claude-fable-5` (the subagent): first asks for `stub_echo`, then
///   answers "SUBAGENT_DONE".
async fn start_stub_llm() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                let last_is_tool = body["messages"]
                    .as_array()
                    .and_then(|msgs| msgs.last())
                    .map(|m| m["role"] == "tool")
                    .unwrap_or(false);
                let sse = match (model.as_str(), last_is_tool) {
                    ("claude-fable-5", false) => sse_tool_call("stub_echo", "{}"),
                    ("claude-fable-5", true) => sse_text("SUBAGENT_DONE"),
                    (_, false) => sse_tool_call(
                        "spawn_subagent",
                        "{\"task\": \"run the echo tool and report back\"}",
                    ),
                    (_, true) => sse_text("PARENT_DONE"),
                };
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(sse))
                    .expect("stub response")
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

fn llm_config(base_url: String) -> LlmConfig {
    LlmConfig {
        base_url,
        model: "stub-model".to_string(),
        api_key: None,
        embedding_model: None,
        timeout_secs: 30,
        ..Default::default()
    }
}

/// Drive one backend turn (the TUI dispatch path) and collect every emitted
/// event plus the final answer.
async fn run_parent_turn(
    project: &Path,
    python: &Path,
    base_url: String,
    subagent_depth: usize,
) -> (String, Vec<AgentEvent>) {
    let seed = build_agent_seed(&tool_server_config(project, python))
        .await
        .expect("backend seed");
    let prism_agent::protocol::AgentSeed {
        mut tool_server,
        command_tool_runtime,
        tools,
        config,
        hooks,
        permissions,
    } = seed;
    let mut config = config.as_ref().clone();
    config.subagent_depth = subagent_depth;

    let llm = LlmClient::new(llm_config(base_url));
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut answer = String::new();
    let mut events: Vec<AgentEvent> = Vec::new();
    agent_loop::run_turn(
        &llm,
        &mut tool_server,
        &command_tool_runtime,
        &mut history,
        tools.as_ref(),
        &config,
        "delegate the echo task",
        None, // chat path — no task context
        &mut transcript,
        hooks.as_ref(),
        &permissions,
        None,
        &mut scratchpad,
        &mut |event| {
            if let AgentEvent::TurnComplete {
                text: Some(text), ..
            } = &event
                && !text.is_empty()
            {
                answer = text.clone();
            }
            events.push(event);
        },
        None,
        None,
    )
    .await
    .expect("parent turn");
    (answer, events)
}

fn tool_result_content<'a>(events: &'a [AgentEvent], tool: &str) -> Option<&'a str> {
    events.iter().find_map(|e| match e {
        AgentEvent::ToolCallResult {
            tool_name, content, ..
        } if tool_name == tool => Some(content.as_str()),
        _ => None,
    })
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn spawn_subagent_runs_a_nested_turn_that_calls_tools() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");

    let (answer, events) = run_parent_turn(project.path(), &python, base_url, 0).await;

    // Parent finished ON TOP of the subagent's result.
    assert_eq!(answer, "PARENT_DONE");

    // The subagent's answer came back as the spawn_subagent tool result.
    let sub_result =
        tool_result_content(&events, "spawn_subagent").expect("spawn_subagent result event");
    assert!(
        sub_result.contains("SUBAGENT_DONE"),
        "subagent summary must reach the parent: {sub_result}"
    );
    assert!(
        sub_result.contains("claude-fable-5"),
        "result must name the model that ran: {sub_result}"
    );

    // The nested turn's tool activity was forwarded to the parent's sink…
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallStart { tool_name, .. } if tool_name == "stub_echo"
        )),
        "nested tool calls must be visible to the parent's event sink"
    );
    // …and the tool REALLY executed, exactly once, via the shared server.
    let log = std::fs::read_to_string(&calls_log).expect("nested tool must have executed");
    let calls = log.lines().filter(|l| l.contains("stub_echo")).count();
    assert_eq!(calls, 1, "nested tool executes exactly once");
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_subagent_refuses_beyond_max_depth() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");

    // Pretend this agent is ALREADY a depth-2 subagent: its spawn attempt
    // must be refused before any nested LLM call or tool execution.
    let (answer, events) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        prism_agent::subagent::MAX_SUBAGENT_DEPTH,
    )
    .await;

    assert_eq!(
        answer, "PARENT_DONE",
        "the refusal still completes the turn"
    );
    let sub_result =
        tool_result_content(&events, "spawn_subagent").expect("spawn_subagent result event");
    assert!(
        sub_result.contains("recursion cap"),
        "depth cap must be reported to the model: {sub_result}"
    );
    assert!(
        !calls_log.exists(),
        "no nested tool may run past the depth cap"
    );
}
