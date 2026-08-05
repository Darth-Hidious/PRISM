// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
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
//! 3. `unsupported_execution_claim_cannot_finalize_a_turn` — the Agent
//!    Execution Contract's structural half: a final answer claiming execution
//!    that no matching tool performed is rejected by `run_turn`, not merely
//!    discouraged by prompt text. Asserts the streamed transcript and the
//!    system message the stub LLM actually received, not just the final string.
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
    /// Never calls a tool. Answers with a FABRICATED execution claim ("I ran
    /// the test suite…") until the execution-contract reminder shows up in the
    /// history, then answers honestly. Drives the finalization-gate test.
    ClaimsWithoutTools,
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

/// Every system message the stub LLM was actually sent, in arrival order.
type SystemMessageLog = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// Serve `/v1/chat/completions` on an ephemeral port; returns the base_url
/// (`http://127.0.0.1:<port>/v1`) for `LlmConfig`.
async fn start_stub_llm(mode: StubMode) -> String {
    start_stub_llm_recording(mode).await.0
}

/// Same, but also hands back a log of the system messages the stub received —
/// the only way to prove what the model was ACTUALLY sent, rather than
/// re-deriving it from the prompt-assembly functions.
async fn start_stub_llm_recording(mode: StubMode) -> (String, SystemMessageLog) {
    use axum::routing::post;
    let systems: SystemMessageLog = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = systems.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let sink = sink.clone();
            async move {
                if let Some(msgs) = body["messages"].as_array() {
                    let mut log = sink.lock().expect("system log");
                    for m in msgs {
                        if m["role"] == "system"
                            && let Some(c) = m["content"].as_str()
                        {
                            log.push(c.to_string());
                        }
                    }
                }
                let last_is_tool = body["messages"]
                    .as_array()
                    .and_then(|msgs| msgs.last())
                    .map(|m| m["role"] == "tool")
                    .unwrap_or(false);
                let saw_contract_reminder = body["messages"]
                    .as_array()
                    .map(|msgs| {
                        msgs.iter().any(|m| {
                            m["content"]
                                .as_str()
                                .is_some_and(|c| c.contains("EXECUTION CONTRACT"))
                        })
                    })
                    .unwrap_or(false);
                let sse = match mode {
                    StubMode::PlainAnswer => sse_text("PARITY_OK"),
                    StubMode::GatedTool if last_is_tool => sse_text("GATED_DONE"),
                    StubMode::GatedTool => sse_tool_call("stub_gated"),
                    StubMode::ClaimsWithoutTools if saw_contract_reminder => {
                        sse_text("HONEST: I did not run anything.")
                    }
                    StubMode::ClaimsWithoutTools => {
                        sse_text("I ran the test suite and everything passes.")
                    }
                };
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(sse))
                    .expect("stub response")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub llm");
    let addr = listener.local_addr().expect("stub llm addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), systems)
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
    let seed = build_agent_seed(
        &tool_server_config(project.path(), &python),
        &llm_config(base_url.clone()),
    )
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
        None, // chat path — no task context
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
async fn anonymous_caller_can_resume_own_session_but_not_anothers() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm(StubMode::PlainAnswer).await;
    let sessions = tempfile::tempdir().expect("sessions dir");
    let service = ChatService::spawn(
        llm_config(base_url),
        tool_server_config(project.path(), &python),
        Some(sessions.path().to_path_buf()),
    )
    .await
    .expect("spawn chat service");

    let caller_a = prism_agent::service::anonymous_caller_id("transport-a");
    let caller_b = prism_agent::service::anonymous_caller_id("transport-b");

    let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
    let first = service
        .chat(
            ChatRequest {
                message: "anonymous one".into(),
                session_id: None,
                approve: vec![],
            },
            &caller_a,
            tx1,
        )
        .await
        .expect("first anonymous session");
    let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
    let second = service
        .chat(
            ChatRequest {
                message: "anonymous two".into(),
                session_id: None,
                approve: vec![],
            },
            &caller_b,
            tx2,
        )
        .await
        .expect("second anonymous session");
    assert_ne!(first.session_id, second.session_id);
    assert_eq!(service.list_sessions(&caller_a).len(), 1);
    assert_eq!(service.list_sessions(&caller_b).len(), 1);
    assert!(service.read_session(&first.session_id, &caller_a).is_ok());
    assert!(service.read_session(&second.session_id, &caller_a).is_err());

    let (tx3, _rx3) = tokio::sync::mpsc::unbounded_channel();
    let own_resume = service
        .chat(
            ChatRequest {
                message: "resume my session".into(),
                session_id: Some(first.session_id.clone()),
                approve: vec![],
            },
            &caller_a,
            tx3,
        )
        .await
        .expect("anonymous caller can resume its own session");
    assert_eq!(own_resume.session_id, first.session_id);

    let (tx4, _rx4) = tokio::sync::mpsc::unbounded_channel();
    let other_resume = service
        .chat(
            ChatRequest {
                message: "try to resume another caller's session".into(),
                session_id: Some(second.session_id.clone()),
                approve: vec![],
            },
            &caller_a,
            tx4,
        )
        .await;
    assert!(matches!(
        other_resume,
        Err(prism_agent::service::ChatError::SessionNotFound(_))
    ));

    let (tx5, _rx5) = tokio::sync::mpsc::unbounded_channel();
    service
        .chat(
            ChatRequest {
                message: "my own second session".into(),
                session_id: Some(second.session_id),
                approve: vec![],
            },
            &caller_b,
            tx5,
        )
        .await
        .expect("the other anonymous caller retains its own access");
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

/// STRUCTURAL PROOF for the Agent Execution Contract: an answer that claims
/// execution while no tool of the matching class ran must NOT be allowed to
/// terminate the turn.
///
/// The stub LLM never calls a tool. Its first answer is a fabrication ("I ran
/// the test suite and everything passes."). If the gate is wired, `run_turn`
/// rejects that finalization, injects the contract reminder, and the model's
/// second answer is what completes the turn. If the gate is missing or the
/// evidence tracking is wrong, the fabrication ships and this fails.
///
/// This also asserts on the STREAMED transcript, not only the final string. The
/// rejected text has already reached the user by the time the gate runs (2e
/// streams before the tool-call check), so without a separating marker the
/// retry lands glued onto the fabrication as one self-contradicting message —
/// a worse outcome than no gate at all. A final-string-only assertion is blind
/// to that, which is exactly how this test read in its first draft.
///
/// It further asserts that the contract text is present in the system message
/// the stub LLM ACTUALLY RECEIVED — end-to-end injection proof, as opposed to
/// the `protocol.rs` unit test that re-walks the assembly functions.
#[tokio::test(flavor = "multi_thread")]
async fn unsupported_execution_claim_cannot_finalize_a_turn() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let (base_url, system_messages) = start_stub_llm_recording(StubMode::ClaimsWithoutTools).await;

    let seed = build_agent_seed(
        &tool_server_config(project.path(), &python),
        &llm_config(base_url.clone()),
    )
    .await
    .expect("seed");
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
    let mut answer = String::new();
    let mut streamed = String::new();
    agent_loop::run_turn(
        &llm,
        &mut tool_server,
        &command_tool_runtime,
        &mut history,
        tools.as_ref(),
        config.as_ref(),
        "run the test suite",
        None,
        &mut transcript,
        hooks.as_ref(),
        &permissions,
        None,
        &mut scratchpad,
        &mut |event| match event {
            AgentEvent::TextDelta { text } => streamed.push_str(&text),
            AgentEvent::TurnComplete {
                text: Some(text), ..
            } if !text.is_empty() => answer = text,
            _ => {}
        },
        None,
        None,
    )
    .await
    .expect("turn");

    assert_eq!(
        answer, "HONEST: I did not run anything.",
        "the gate must reject the unsupported claim and force a second pass"
    );
    assert!(
        history.iter().all(|m| m.content.as_deref()
            != Some(prism_agent::execution_contract::UNSUPPORTED_CLAIM_REMINDER)),
        "the reminder is per-finalization scaffolding and must be stripped from \
         history before the turn ends — `history` outlives the turn on the TUI path"
    );

    // What the user actually saw. The rejected text streams before the gate can
    // run, so the marker between the two answers is the only thing preventing
    // one glued, self-contradicting message.
    let fabrication = streamed
        .find("I ran the test suite")
        .expect("the rejected answer did stream to the user");
    let marker = streamed
        .find("unverified claim")
        .expect("the retraction marker must separate the rejected answer from the retry");
    let retry = streamed
        .find("HONEST:")
        .expect("the corrected answer must stream too");
    assert!(
        fabrication < marker && marker < retry,
        "streamed order must be: rejected answer, retraction marker, corrected answer — got {streamed:?}"
    );

    // End-to-end injection proof: the contract is in the system message the
    // model was actually sent, not merely in a constant or a re-derived string.
    let systems = system_messages.lock().expect("system log");
    assert!(
        systems.iter().any(|s| {
            s.contains("execution agent, not an advice-only assistant")
                && s.contains("unless a tool result for it exists in THIS run")
        }),
        "the Execution Contract must be in a system message the model received"
    );
}
