// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! orchestrate_agents end-to-end: a REAL parent turn fans out to REAL nested
//! turns through the same stub OpenAI-compatible LLM and stub Python tool
//! server used by `subagent_nested_turn.rs`.
//!
//! The stub LLM routes on the request's `model` field — the parent runs as
//! `stub-model`; every orchestrated item is asked to run as
//! `claude-fable-5`. This proves, with no live LLM:
//!
//! 1. the loop intercepts `orchestrate_agents` and runs N nested turns whose
//!    tools REALLY execute (the stub `stub_echo` logs each execution),
//! 2. every orchestrated agent is durable: N child rows under the
//!    orchestrating run, reconstructible via `list_agent_run_descendants`,
//! 3. the approval gate is NOT bypassed: with a wired approval channel a
//!    denied `orchestrate_agents` spawns NOTHING — no nested turn, no tool
//!    execution, no child ledger rows (this test FAILS if orchestrated
//!    spawns skip the gate),
//! 4. a LocalOnly (non-owner) caller is refused at the orchestrate call
//!    itself, before any nested turn or tool execution.
//!
//! Requires `python3` on PATH; tests skip (with a note) when absent.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use prism_agent::agent_loop;
use prism_agent::agent_loop::ApprovalResponse;
use prism_agent::command_tools::CommandToolPlatformAccess;
use prism_agent::protocol::build_agent_seed;
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

/// Serialize the tests in this binary: they all drive `run_turn`, whose entry
/// resets process-global state (`hooks::LAST_CODE_RUN`, provenance context).
static SERIAL_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

// ── Stub Python tool server ──────────────────────────────────────────

/// One python tool: `stub_echo` needs no approval and logs every execution
/// (with its pid) to `<project>/calls.log` so tests can assert which nested
/// turns really ran it, and on which pool lane.
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
            f.write(json.dumps({"pid": os.getpid(), "req": req}) + "\n")
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
        "choices": [{ "delta": { "content": text } }],
        "usage": { "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 }
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn sse_tool_call(tool: &str, arguments: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "function": { "name": tool, "arguments": arguments }
        }] } }],
        "usage": { "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120 }
    });
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// The model each orchestrated item is asked to run as.
const ITEM_MODEL: &str = "claude-fable-5";

/// The three-task batch the parent model requests. Built with serde so the
/// nested JSON survives the tool-argument string encoding.
fn orchestrate_args() -> String {
    // Each task names its model EXPLICITLY. An unnamed model now inherits the
    // parent's route (a subagent must not silently switch to a provider the
    // parent's endpoint does not serve), which would make every item run as
    // `stub-model` — indistinguishable from the parent to a stub that routes on
    // the model field. Naming it keeps parent and item scripts separable here,
    // and pins that an explicit model still beats inheritance.
    serde_json::json!({
        "tasks": [
            { "id": "one",   "model": ITEM_MODEL, "task": "run the echo tool and report back" },
            { "id": "two",   "model": ITEM_MODEL, "task": "run the echo tool and report back" },
            { "id": "three", "model": ITEM_MODEL, "task": "run the echo tool and report back" },
        ],
    })
    .to_string()
}

/// Serve `/v1/chat/completions` on an ephemeral port.
///
/// - `stub-model` (the parent): first asks for `orchestrate_agents` with the
///   three-task batch, then — once a tool result is the last message —
///   answers "PARENT_DONE".
/// - `claude-fable-5` (an orchestrated item): first asks for `stub_echo`,
///   then answers "ITEM_DONE".
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
                    ("claude-fable-5", true) => sse_text("ITEM_DONE"),
                    (_, false) => sse_tool_call("orchestrate_agents", &orchestrate_args()),
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
/// event plus the final answer — the same production wiring as
/// `subagent_nested_turn.rs`, extended with an optional approval channel so
/// the approval-gate test can wire a real Deny.
async fn run_parent_turn(
    project: &Path,
    python: &Path,
    base_url: String,
    access: CommandToolPlatformAccess,
    approval_rx: Option<agent_loop::SharedApprovalReceiver>,
) -> (String, Vec<AgentEvent>) {
    let seed = build_agent_seed(
        &tool_server_config(project, python),
        &llm_config(base_url.clone()),
    )
    .await
    .expect("backend seed");
    let prism_agent::protocol::AgentSeed {
        mut tool_server,
        subagent_lanes,
        command_tool_runtime,
        tools,
        config,
        hooks,
        permissions,
    } = seed;
    let config = config.as_ref().clone();

    let llm = LlmClient::new(llm_config(base_url));
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut answer = String::new();
    let mut events: Vec<AgentEvent> = Vec::new();
    let mut policy = prism_policy::PolicyEngine::with_discovery(None).ok();
    prism_agent::command_tools::with_platform_access(
        access,
        agent_loop::run_turn(
            &llm,
            &mut tool_server,
            &command_tool_runtime,
            &mut history,
            tools.as_ref(),
            &config,
            "delegate the echo tasks",
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
            approval_rx,
            // A REAL engine, as production passes. `None` now means "the policy
            // engine failed to load" and denies every tool fail-closed, so a
            // test that passed None was silently exercising a path production
            // never takes.
            policy.as_mut(),
            Some(&subagent_lanes),
        ),
    )
    .await
    .expect("parent turn");
    (answer, events)
}

/// The content of a tool result, whoever ran it.
///
/// Looks THROUGH `AgentActivity`: a delegated agent's activity is now tagged
/// with which agent produced it, and a helper asking "what did tool X answer"
/// should not care who ran it. `agent_of_tool_result` is the one that does.
fn tool_result_content<'a>(events: &'a [AgentEvent], tool: &str) -> Option<&'a str> {
    events.iter().find_map(|e| match untagged(e) {
        AgentEvent::ToolCallResult {
            tool_name, content, ..
        } if tool_name == tool => Some(content.as_str()),
        _ => None,
    })
}

/// Peel every attribution layer, returning the event underneath.
fn untagged(event: &AgentEvent) -> &AgentEvent {
    let mut current = event;
    while let AgentEvent::AgentActivity { event, .. } = current {
        current = event;
    }
    current
}

/// Which agents were seen running `tool`, in the order their results arrived.
fn agents_running(events: &[AgentEvent], tool: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AgentActivity { agent, event } => match untagged(event) {
                AgentEvent::ToolCallResult { tool_name, .. } if tool_name == tool => {
                    Some(agent.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The session's root run row (`role == "agent"`).
async fn root_run_of_session(session_id: &str) -> prism_provenance::AgentRun {
    let store = prism_provenance::ProvenanceStore::open(&prism_agent::hooks::provenance_db_path())
        .await
        .expect("open isolated agent-run ledger");
    let runs = store
        .list_agent_runs(&prism_provenance::AgentRunFilter {
            session_id: Some(session_id.to_string()),
            ..Default::default()
        })
        .await
        .expect("query session runs");
    runs.into_iter()
        .find(|run| run.role == "agent")
        .expect("the parent turn must leave a durable root run")
}

async fn descendants_of(root_run_id: &str) -> prism_provenance::AgentRunDescendants {
    let store = prism_provenance::ProvenanceStore::open(&prism_agent::hooks::provenance_db_path())
        .await
        .expect("open isolated agent-run ledger");
    store
        .list_agent_run_descendants(
            root_run_id,
            &prism_provenance::AgentRunTraversalPolicy::default(),
        )
        .await
        .expect("walk the spawn topology")
}

// ── Tests ────────────────────────────────────────────────────────────

/// The full production path: the parent's ONE `orchestrate_agents` call runs
/// three nested turns whose tools really execute, the per-item report reaches
/// the parent (every id, no aggregate), and the whole fan-out is
/// reconstructible from the durable ledger via `list_agent_run_descendants`.
#[tokio::test(flavor = "multi_thread")]
async fn orchestrate_agents_fans_out_and_records_descendants() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");
    let session_id = "orchestrator-fan-out-descendants";
    prism_agent::hooks::set_provenance_ctx(session_id, "stub-model");

    let (answer, events) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        None,
    )
    .await;

    // Parent finished ON TOP of the fan-out's report.
    assert_eq!(answer, "PARENT_DONE");

    // Every forwarded event NAMES the agent that produced it. Three items run
    // the same tool concurrently onto one sink; without the tag their activity
    // is a single interleaved stream and no interface can group a lane,
    // because the identity was never on the wire.
    let mut runners = agents_running(&events, "stub_echo");
    runners.sort();
    assert_eq!(
        runners,
        vec!["one".to_string(), "three".to_string(), "two".to_string()],
        "each item's tool activity is attributed to that item"
    );

    // The PARENT's own work stays untagged, so a single-agent session looks
    // exactly as it did before attribution existed.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallResult { tool_name, .. } if tool_name == "orchestrate_agents"
        )),
        "the parent's own tool result is not wrapped in an agent tag"
    );

    // The per-item report reached the parent: every id, individually, with
    // its own outcome — never an aggregate "ok".
    let report =
        tool_result_content(&events, "orchestrate_agents").expect("orchestrate_agents result");
    for id in ["one", "two", "three"] {
        assert!(report.contains(id), "item {id} must be reported: {report}");
    }
    assert!(
        report.contains("\"succeeded\":3") || report.contains("\"succeeded\": 3"),
        "the report must count 3 successes: {report}"
    );
    assert!(
        report.contains("ITEM_DONE"),
        "item summaries must reach the parent: {report}"
    );

    // Every nested turn REALLY executed its tool — exactly once each.
    let log = std::fs::read_to_string(&calls_log).expect("nested tools must have executed");
    let calls = log.lines().filter(|l| l.contains("stub_echo")).count();
    assert_eq!(calls, 3, "each of the 3 items executes stub_echo once");

    // Durable topology: the orchestrating run has EXACTLY the three items as
    // descendants, each carrying the explicit spawn edge, each completed.
    let root = root_run_of_session(session_id).await;
    let descendants = descendants_of(&root.id).await;
    assert_eq!(
        descendants.outcome,
        prism_provenance::AgentRunTraversalOutcome::Complete,
        "the walk must inspect every spawn edge"
    );
    assert_eq!(
        descendants.runs.len(),
        3,
        "every orchestrated agent must appear in the descendant tree: {:?}",
        descendants.runs
    );
    for run in &descendants.runs {
        assert_eq!(run.role, "subagent", "orchestrated items are subagent rows");
        assert_eq!(
            run.parent_run_id.as_deref(),
            Some(root.id.as_str()),
            "each item must carry the spawn edge to the orchestrating run"
        );
        assert_eq!(run.status, prism_provenance::AgentRunStatus::Completed);
    }
}

/// The approval gate is not weakened by orchestration: `orchestrate_agents`
/// is `requires_approval` and a wired channel answering Deny must stop the
/// WHOLE batch before anything runs. If orchestrated spawns bypassed the gate
/// (auto-approval, a dropped `requires_approval` flag), the nested turns
/// would run, `calls.log` would exist, and child ledger rows would appear —
/// and this test would FAIL.
#[tokio::test(flavor = "multi_thread")]
async fn denied_orchestrate_agents_spawns_nothing() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");
    let session_id = "orchestrator-fan-out-denied";
    prism_agent::hooks::set_provenance_ctx(session_id, "stub-model");

    // A real approval channel with ONE buffered answer: Deny.
    let (approval_tx, approval_rx) = tokio::sync::mpsc::channel(1);
    approval_tx
        .send(ApprovalResponse::Deny)
        .await
        .expect("buffer the denial");
    let approval_rx = Some(Arc::new(tokio::sync::Mutex::new(approval_rx)));

    let (answer, events) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        approval_rx,
    )
    .await;
    drop(approval_tx);

    // The denial still completes the turn honestly.
    assert_eq!(answer, "PARENT_DONE");
    let report =
        tool_result_content(&events, "orchestrate_agents").expect("orchestrate_agents result");
    assert!(
        report.contains("denied"),
        "the model must see the denial: {report}"
    );

    // NOTHING ran: no nested tool execution, no orchestrated child rows.
    assert!(
        !calls_log.exists(),
        "a denied batch must not execute any nested tool"
    );
    let root = root_run_of_session(session_id).await;
    let descendants = descendants_of(&root.id).await;
    assert!(
        descendants.runs.is_empty(),
        "a denied batch must spawn no agents: {:?}",
        descendants.runs
    );
}

/// orchestrate_agents is effect-classified ExecutesCode, like spawn_subagent:
/// a LocalOnly (non-owner) caller is refused at the orchestrate call itself —
/// before any nested turn, model call, or tool execution.
#[tokio::test(flavor = "multi_thread")]
async fn local_only_caller_cannot_orchestrate() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");

    let (answer, events) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        CommandToolPlatformAccess::LocalOnly,
        None,
    )
    .await;

    assert_eq!(
        answer, "PARENT_DONE",
        "the refusal still completes the turn"
    );
    let report =
        tool_result_content(&events, "orchestrate_agents").expect("orchestrate_agents result");
    assert!(
        report.contains("owner-only"),
        "refusal must say why approval is insufficient: {report}"
    );
    assert!(
        !calls_log.exists(),
        "no nested tool may run for a refused orchestrate call"
    );
}
