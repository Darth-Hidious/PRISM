// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! FIX D parity + approval-gating proof for the HTTP chat service.
//!
//! These tests drive REAL turns through both transports with a stub
//! OpenAI-compatible LLM and a stub Python tool server:
//!
//! 1. `http_chat_service_and_backend_share_loop_and_catalog` — the HTTP
//!    service (`ChatService::chat`) and the backend path (direct
//!    `agent_loop::run_turn`, what `spawn_agent_turn` does for the TUI)
//!    produce the same answer from the same stub LLM and expose the SAME
//!    tool catalog, because both are built by `protocol::build_agent_seed`.
//! 2. `gated_tool_is_skipped_then_runs_when_approved` — headless approval:
//!    a `requires_approval` tool is NEVER executed without explicit
//!    pre-approval (surfaced as an `approval_required` event), and runs
//!    exactly once when the client re-sends with `approve: ["<tool>"]`.
//!
//! Requires `python3` on PATH; tests skip (with a note) when absent.

use std::path::{Path, PathBuf};

use prism_agent::agent_loop;
use prism_agent::protocol::{AgentSeed, build_agent_seed};
use prism_agent::service::{ChatEvent, ChatRequest, ChatService};
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

// ── Stub Python tool server ──────────────────────────────────────────

/// One python tool: `stub_gated` requires approval and logs every
/// execution to `<project>/calls.log` so tests can assert whether the
/// tool actually ran.
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
                "name": "stub_gated",
                "description": "Stub gated tool: performs a privileged stub action (test only).",
                "input_schema": {"type": "object", "properties": {}},
                "requires_approval": True,
            },
        ]}
    elif method == "call_tool":
        with open(LOG, "a") as f:
            f.write(json.dumps(req) + "\n")
        resp = {"result": {"ok": True, "tool": req.get("tool")}}
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

// ── Stub OpenAI-compatible LLM ───────────────────────────────────────

#[derive(Clone, Copy)]
enum StubMode {
    /// Always answers with plain text "PARITY_OK".
    PlainAnswer,
    /// Calls the `stub_gated` tool whenever the LAST message is from the
    /// user; once a tool result (or denial) is the last message, answers
    /// "GATED_DONE". Keyed on the last message so resumed sessions that
    /// already contain old tool messages still trigger a fresh call.
    GatedTool,
}

fn sse_text(text: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{ "delta": { "content": text } }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn sse_tool_call(tool: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "function": { "name": tool, "arguments": "{}" }
        }] } }]
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Serve `/v1/chat/completions` on an ephemeral port; returns the base_url
/// (`http://127.0.0.1:<port>/v1`) for `LlmConfig`.
async fn start_stub_llm(mode: StubMode) -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            move |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let last_is_tool = body["messages"]
                    .as_array()
                    .and_then(|msgs| msgs.last())
                    .map(|m| m["role"] == "tool")
                    .unwrap_or(false);
                let sse = match mode {
                    StubMode::PlainAnswer => sse_text("PARITY_OK"),
                    StubMode::GatedTool if last_is_tool => sse_text("GATED_DONE"),
                    StubMode::GatedTool => sse_tool_call("stub_gated"),
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

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ChatEvent>) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn http_chat_service_and_backend_share_loop_and_catalog() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm(StubMode::PlainAnswer).await;
    let sessions = tempfile::tempdir().expect("sessions dir");

    // ── Transport 1: the HTTP chat service ────────────────────────
    let service = ChatService::spawn(
        llm_config(base_url.clone()),
        tool_server_config(project.path(), &python),
        Some(sessions.path().to_path_buf()),
    )
    .await
    .expect("spawn chat service");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = service
        .chat(
            ChatRequest {
                message: "hello".to_string(),
                session_id: None,
                approve: vec![],
            },
            "user-a",
            tx,
        )
        .await
        .expect("http turn");
    assert_eq!(outcome.answer, "PARITY_OK");
    assert!(outcome.approvals_required.is_empty());

    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ChatEvent::Answer { text } if text.contains("PARITY_OK"))),
        "streamed answer event expected"
    );
    assert!(
        matches!(events.last(), Some(ChatEvent::Done { session_id, .. }) if *session_id == outcome.session_id),
        "stream must terminate with done"
    );

    // Session persistence + per-user scoping.
    let sessions_a = service.list_sessions("user-a");
    assert_eq!(sessions_a.len(), 1, "one session for its owner");
    assert_eq!(sessions_a[0].session_id, outcome.session_id);
    let messages = service
        .read_session(&outcome.session_id, "user-a")
        .expect("owner can read");
    assert!(messages.iter().any(|m| m["role"] == "user"));
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "assistant" && m["content"] == "PARITY_OK")
    );
    assert!(
        service.list_sessions("user-b").is_empty(),
        "other users must not see the session"
    );
    assert!(
        service.read_session(&outcome.session_id, "user-b").is_err(),
        "other users must not read the session"
    );

    // Follow-up turn in the same session must carry context (resume path).
    let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
    let outcome2 = service
        .chat(
            ChatRequest {
                message: "again".to_string(),
                session_id: Some(outcome.session_id.clone()),
                approve: vec![],
            },
            "user-a",
            tx2,
        )
        .await
        .expect("follow-up turn");
    assert_eq!(outcome2.session_id, outcome.session_id);
    let messages = service
        .read_session(&outcome.session_id, "user-a")
        .expect("read after follow-up");
    let user_turns = messages.iter().filter(|m| m["role"] == "user").count();
    assert_eq!(user_turns, 2, "both user turns persisted in one session");

    // ── Transport 2: the backend path (what the TUI uses) ─────────
    // Same seed builder, same run_turn — this is spawn_agent_turn's
    // dispatch without the stdio framing.
    let seed = build_agent_seed(&tool_server_config(project.path(), &python))
        .await
        .expect("backend seed");

    // Catalog parity: identical tool names on both transports.
    let mut service_tools = service.tool_names();
    service_tools.sort();
    let mut seed_tools = seed
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect::<Vec<_>>();
    seed_tools.sort();
    assert_eq!(
        service_tools, seed_tools,
        "HTTP service and backend must expose the same tool catalog"
    );
    assert!(
        seed_tools.iter().any(|name| name == "stub_gated"),
        "python tool present on both"
    );
    assert!(
        seed_tools.iter().any(|name| name == "status"),
        "rust command tool present on both"
    );

    let AgentSeed {
        mut tool_server,
        command_tool_runtime,
        tools,
        config,
        hooks,
        permissions,
    } = seed;
    let llm = LlmClient::new(llm_config(base_url));
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut backend_answer = String::new();
    agent_loop::run_turn(
        &llm,
        &mut tool_server,
        &command_tool_runtime,
        &mut history,
        tools.as_ref(),
        config.as_ref(),
        "hello",
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
                backend_answer = text;
            }
        },
        None,
        None,
    )
    .await
    .expect("backend turn");
    assert_eq!(
        backend_answer, "PARITY_OK",
        "same loop, same stub LLM, same answer on both transports"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gated_tool_is_skipped_then_runs_when_approved() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm(StubMode::GatedTool).await;
    let sessions = tempfile::tempdir().expect("sessions dir");
    let calls_log = project.path().join("calls.log");

    let service = ChatService::spawn(
        llm_config(base_url),
        tool_server_config(project.path(), &python),
        Some(sessions.path().to_path_buf()),
    )
    .await
    .expect("spawn chat service");

    // ── Turn 1: no pre-approval → tool must be SKIPPED ────────────
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = service
        .chat(
            ChatRequest {
                message: "use the stub_gated tool".to_string(),
                session_id: None,
                approve: vec![],
            },
            "user-a",
            tx,
        )
        .await
        .expect("denied turn still completes");
    let events = drain(&mut rx);

    assert_eq!(
        outcome.approvals_required,
        vec!["stub_gated".to_string()],
        "outcome must name the skipped tool"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ChatEvent::ApprovalRequired { tool_name, .. } if tool_name == "stub_gated"
        )),
        "approval_required event must be emitted"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ChatEvent::ToolResult { tool_name, is_error: true, .. } if tool_name == "stub_gated"
        )),
        "denied call surfaces as an error tool_result"
    );
    assert!(
        !calls_log.exists(),
        "gated tool must NOT execute without approval"
    );

    // ── Turn 2: explicit approval → tool runs exactly once ────────
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    let outcome2 = service
        .chat(
            ChatRequest {
                message: "use the stub_gated tool".to_string(),
                session_id: Some(outcome.session_id.clone()),
                approve: vec!["stub_gated".to_string()],
            },
            "user-a",
            tx2,
        )
        .await
        .expect("approved turn");
    let events2 = drain(&mut rx2);

    assert!(
        !events2
            .iter()
            .any(|e| matches!(e, ChatEvent::ApprovalRequired { .. })),
        "no approval_required once pre-approved"
    );
    assert!(
        events2.iter().any(|e| matches!(
            e,
            ChatEvent::ToolResult { tool_name, is_error: false, .. } if tool_name == "stub_gated"
        )),
        "approved tool produces a successful tool_result"
    );
    assert!(outcome2.approvals_required.is_empty());
    assert_eq!(outcome2.answer, "GATED_DONE");

    let log = std::fs::read_to_string(&calls_log).expect("tool must have executed");
    let calls = log
        .lines()
        .filter(|line| line.contains("stub_gated"))
        .count();
    assert_eq!(calls, 1, "approved tool executes exactly once");
}
