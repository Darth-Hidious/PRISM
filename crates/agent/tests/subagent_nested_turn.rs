// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! spawn_subagent end-to-end: a REAL parent turn delegates to a REAL nested
//! turn through the same stub OpenAI-compatible LLM and stub Python tool
//! server used by `http_chat_parity.rs`.
//!
//! The stub LLM routes on the request's `model` field — the parent runs as
//! `stub-model`; the nested turn is asked to run as
//! `claude-fable-5` (same endpoint, model swapped by `spawn_subagent`). This
//! proves, with no live LLM:
//!
//! 1. the loop intercepts `spawn_subagent` and runs a nested `run_turn`,
//! 2. the nested turn is routed to the requested model,
//! 3. the nested turn can CALL TOOLS (the stub `stub_echo` executes exactly
//!    once — on the subagent's own pool lane when lanes are provided, on the
//!    parent's handle otherwise),
//! 4. the subagent's answer comes back to the parent as the tool result and
//!    the parent finishes its own turn on top of it,
//! 5. the depth cap refuses to spawn from an agent already at max depth,
//! 6. with a lane pool the subagent's tool calls run on a DIFFERENT child
//!    process than the parent's; without one they share the parent's child.
//!
//! Requires `python3` on PATH; tests skip (with a note) when absent.

/// Pre-main store isolation — every integration binary must declare this.
mod common;

use std::path::{Path, PathBuf};

use prism_agent::agent_loop;
use prism_agent::command_tools::CommandToolPlatformAccess;

/// Serialize the tests in this binary. They all drive `run_turn`, whose entry
/// resets the PROCESS-GLOBAL repair-chain memory (`hooks::LAST_CODE_RUN`); the
/// H4 test also ASSERTS on that global, so a concurrent turn's entry-reset would
/// wipe its state mid-test. One async lock keeps the turns from overlapping
/// (tokio Mutex so it can be held across `.await` without deadlocking).
static SERIAL_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
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
                    // The model is named EXPLICITLY. An unnamed model now
                    // inherits the parent's route — a subagent must not silently
                    // switch to a provider the parent's endpoint does not serve
                    // — which would make the nested turn run as `stub-model`,
                    // indistinguishable from the parent to a stub that routes on
                    // the model field. Naming it keeps the two scripts separable
                    // and pins that an explicit model still beats inheritance.
                    (_, false) => sse_tool_call(
                        "spawn_subagent",
                        "{\"task\": \"run the echo tool and report back\", \
                          \"model\": \"claude-fable-5\"}",
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
/// event plus the final answer. `access` is the platform-credential boundary
/// the transport establishes around the turn: the real TUI dispatch scopes
/// `VerifiedNodeOwner` (protocol::spawn_agent_turn); tests for non-owner
/// callers pass `LocalOnly` (the unscoped default). `use_lanes` mirrors the
/// production dispatch (which passes the seed's subagent lane pool); `false`
/// exercises the legacy serialized path where the subagent borrows the
/// parent's tool-server handle.
async fn run_parent_turn(
    project: &Path,
    python: &Path,
    base_url: String,
    subagent_depth: usize,
    access: CommandToolPlatformAccess,
    use_lanes: bool,
) -> (
    String,
    Vec<AgentEvent>,
    prism_agent::transcript::CostTracker,
) {
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
    let mut config = config.as_ref().clone();
    config.subagent_depth = subagent_depth;

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
            // A REAL engine, as production passes. `None` now means "the policy
            // engine failed to load" and denies every tool fail-closed, so a
            // test passing None exercised a path production never takes.
            policy.as_mut(),
            use_lanes.then_some(&subagent_lanes),
        ),
    )
    .await
    .expect("parent turn");
    // The parent's cost log is returned too: it is where a delegated turn's
    // spend is charged, and the only place a missing charge is observable.
    (answer, events, transcript.cost)
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
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let python = find_python().expect("python3 is required for the durable parent-edge test");
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");
    let session_id = "subagent-run-parent-edge";
    prism_agent::hooks::set_provenance_ctx(session_id, "stub-model");

    let (answer, events, _cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        0,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        true,
    )
    .await;

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
    // …and the tool REALLY executed, exactly once (on the subagent's own
    // pool lane — the production dispatch shape).
    let log = std::fs::read_to_string(&calls_log).expect("nested tool must have executed");
    let calls = log.lines().filter(|l| l.contains("stub_echo")).count();
    assert_eq!(calls, 1, "nested tool executes exactly once");

    // The same real path must leave a durable, reconstructable spawn edge.
    let store = prism_provenance::ProvenanceStore::open(&prism_agent::hooks::provenance_db_path())
        .await
        .expect("open isolated agent-run ledger");
    let runs = store
        .list_agent_runs(&prism_provenance::AgentRunFilter {
            session_id: Some(session_id.to_string()),
            ..Default::default()
        })
        .await
        .expect("query parent and child runs");
    assert_eq!(runs.len(), 2, "one root and one child must be durable");
    let root = runs
        .iter()
        .find(|run| run.role == "agent")
        .expect("root run");
    let child = runs
        .iter()
        .find(|run| run.role == "subagent")
        .expect("subagent run");
    assert_eq!(root.status, prism_provenance::AgentRunStatus::Completed);
    assert_eq!(child.status, prism_provenance::AgentRunStatus::Completed);
    assert_eq!(
        child.parent_run_id.as_deref(),
        Some(root.id.as_str()),
        "the child row must carry the explicit spawn edge"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_subagent_refuses_beyond_max_depth() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
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
    let (answer, events, _cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        prism_agent::subagent::MAX_SUBAGENT_DEPTH,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        true,
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

/// Round 7: spawn_subagent is effect-classified ExecutesCode — it drives a
/// nested turn over the same code-running tool surface — so a LocalOnly
/// (non-owner) caller is refused at the spawn itself. The turn still
/// completes honestly; no nested LLM call or tool execution may happen.
#[tokio::test(flavor = "multi_thread")]
async fn local_only_caller_cannot_spawn_a_subagent() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_stub_llm().await;
    let calls_log = project.path().join("calls.log");

    let (answer, events, _cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        0,
        CommandToolPlatformAccess::LocalOnly,
        true,
    )
    .await;

    assert_eq!(
        answer, "PARENT_DONE",
        "the refusal still completes the turn"
    );
    let sub_result =
        tool_result_content(&events, "spawn_subagent").expect("spawn_subagent result event");
    assert!(
        sub_result.contains("owner-only"),
        "refusal must say why approval is insufficient: {sub_result}"
    );
    assert!(
        sub_result.contains("verified node-owner session"),
        "{sub_result}"
    );
    assert!(
        !calls_log.exists(),
        "no nested tool may run for a refused spawn"
    );
}

// ── Lane separation: the subagent's tools run on its OWN child ───────

/// Routes so BOTH the parent and the subagent call `stub_echo` once:
/// - `stub-model` (parent): stub_echo → spawn_subagent → PARENT_DONE.
/// - `claude-fable-5` (subagent): stub_echo → SUBAGENT_DONE.
async fn start_lane_stub_llm() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                let tool_msgs = body["messages"]
                    .as_array()
                    .map(|msgs| msgs.iter().filter(|m| m["role"] == "tool").count())
                    .unwrap_or(0);
                let sse = match (model.as_str(), tool_msgs) {
                    ("claude-fable-5", 0) => sse_tool_call("stub_echo", "{}"),
                    ("claude-fable-5", _) => sse_text("SUBAGENT_DONE"),
                    (_, 0) => sse_tool_call("stub_echo", "{}"),
                    (_, 1) => sse_tool_call(
                        "spawn_subagent",
                        "{\"task\": \"run the echo tool and report back\", \"model\": \"claude-fable-5\"}",
                    ),
                    (_, _) => sse_text("PARENT_DONE"),
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

/// The pids of the Python children that answered each logged `stub_echo`
/// call, in execution order.
fn logged_echo_pids(calls_log: &Path) -> Vec<u64> {
    let log = std::fs::read_to_string(calls_log).expect("calls.log written");
    log.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["req"]["tool"] == "stub_echo")
        .map(|entry| entry["pid"].as_u64().expect("stub logs its pid"))
        .collect()
}

/// With a lane pool (the production dispatch), the subagent executes tools
/// on its OWN tool-server child: the parent's `stub_echo` and the nested
/// `stub_echo` answer from different pids. This is the falsifiable core of
/// "the subagent takes its own lane" — if the subagent still borrowed the
/// parent's handle, both calls would log the same pid and this test fails.
#[tokio::test(flavor = "multi_thread")]
async fn subagent_tools_run_on_their_own_lane() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_lane_stub_llm().await;
    let calls_log = project.path().join("calls.log");

    let (answer, events, _cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        0,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        true,
    )
    .await;

    assert_eq!(answer, "PARENT_DONE");
    let sub_result =
        tool_result_content(&events, "spawn_subagent").expect("spawn_subagent result event");
    assert!(sub_result.contains("SUBAGENT_DONE"), "{sub_result}");

    let pids = logged_echo_pids(&calls_log);
    assert_eq!(pids.len(), 2, "parent + subagent each echo once: {pids:?}");
    assert_ne!(
        pids[0], pids[1],
        "the subagent must execute tools on its own lane's child, \
         not the parent's: {pids:?}"
    );
}

/// Without a pool (`subagent_lanes: None` — callers that do not opt in), the
/// legacy path is unchanged: the subagent borrows the PARENT's handle, so
/// both `stub_echo` calls answer from the same child process.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_pool_the_subagent_borrows_the_parents_handle() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_lane_stub_llm().await;
    let calls_log = project.path().join("calls.log");

    let (answer, _events, _cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        0,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        false,
    )
    .await;

    assert_eq!(answer, "PARENT_DONE");
    let pids = logged_echo_pids(&calls_log);
    assert_eq!(pids.len(), 2, "parent + subagent each echo once: {pids:?}");
    assert_eq!(
        pids[0], pids[1],
        "the no-pool path must keep sharing the parent's child: {pids:?}"
    );
}

// ── H4: the G2/H3 repair-chain guard, through the REAL nested wiring ──
//
// The only prior G2 test (hooks::tests::g2_snapshot_restore_roundtrip) called
// the snapshot/restore helpers directly, never subagent.rs. This drives the
// ACTUAL parent-fail -> spawn_subagent(fails) -> parent wiring and asserts the
// parent's repair-chain memory survives and the subagent's code-run does NOT
// splice into it — the property the CodeRunChainGuard exists to guarantee.

/// A tool server exposing the two code-exec tools whose runs populate the
/// repair-chain memory (`hooks::LAST_CODE_RUN`). Both return a FAILURE payload
/// (`success: false`) so the run is recorded as a failed code-exec.
const H4_CODE_EXEC_TOOL_SERVER_PY: &str = r#"
import sys, json

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "list_tools":
        resp = {"tools": [
            {"name": "execute_python", "description": "stub code exec (test only).",
             "input_schema": {"type": "object", "properties": {}}, "requires_approval": False},
            {"name": "execute_bash", "description": "stub code exec (test only).",
             "input_schema": {"type": "object", "properties": {}}, "requires_approval": False},
        ]}
    elif method == "call_tool":
        # A FAILED code run: is_error via inner success:false + error string.
        resp = {"result": {"ok": True, "success": False, "error": "stub code-exec failure"}}
    else:
        resp = {"error": "unknown method"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

fn write_h4_project(dir: &Path) {
    let app = dir.join("app");
    std::fs::create_dir_all(&app).expect("create app dir");
    std::fs::write(app.join("__init__.py"), "").expect("write __init__");
    std::fs::write(app.join("tool_server.py"), H4_CODE_EXEC_TOOL_SERVER_PY).expect("write stub");
}

/// - `stub-model` (parent): execute_python (fails) -> spawn_subagent -> PARENT_DONE.
/// - `claude-fable-5` (subagent): execute_bash (fails) -> SUBAGENT_DONE.
///
/// Routes on the number of `tool`-role messages already in the request so each
/// step is deterministic.
async fn start_h4_stub_llm() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                let tool_msgs = body["messages"]
                    .as_array()
                    .map(|msgs| msgs.iter().filter(|m| m["role"] == "tool").count())
                    .unwrap_or(0);
                let sse = match (model.as_str(), tool_msgs) {
                    ("claude-fable-5", 0) => sse_tool_call("execute_bash", "{}"),
                    ("claude-fable-5", _) => sse_text("SUBAGENT_DONE"),
                    (_, 0) => sse_tool_call("execute_python", "{}"),
                    (_, 1) => sse_tool_call(
                        "spawn_subagent",
                        "{\"task\": \"run a code cell and report back\", \
                          \"model\": \"claude-fable-5\"}",
                    ),
                    (_, _) => sse_text("PARENT_DONE"),
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

#[tokio::test(flavor = "multi_thread")]
async fn spawn_subagent_preserves_parent_repair_chain() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_h4_project(project.path());
    let base_url = start_h4_stub_llm().await;

    // Clean the process-global chain so we assert only on THIS turn's records.
    prism_agent::hooks::reset_code_run_chain();

    let (answer, events, _cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        0,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        true,
    )
    .await;

    // The parent finished ON TOP of the subagent (the real wiring ran).
    assert_eq!(answer, "PARENT_DONE");
    let sub_result =
        tool_result_content(&events, "spawn_subagent").expect("spawn_subagent result event");
    assert!(
        sub_result.contains("SUBAGENT_DONE"),
        "the nested subagent turn really ran: {sub_result}"
    );

    // The guard restored the parent's chain across the nested turn: the parent's
    // execute_python slot SURVIVES, and the subagent's execute_bash slot was
    // DISCARDED (never spliced into the parent's chain). Without the guard the
    // nested run_turn's entry-reset would have wiped execute_python and left
    // execute_bash — the exact corruption H3 fixes.
    let chain = prism_agent::hooks::snapshot_code_run_chain();
    let keys: Vec<&String> = chain.keys().collect();
    assert!(
        chain.contains_key("execute_python"),
        "parent's repair chain must survive the nested subagent turn: {keys:?}"
    );
    assert!(
        !chain.contains_key("execute_bash"),
        "subagent's code-run slot must NOT splice into the parent's chain: {keys:?}"
    );

    prism_agent::hooks::reset_code_run_chain();
}

// ── W2: the parent is charged for a subagent that DIED mid-turn ──────

/// - `stub-model` (parent): spawn_subagent -> PARENT_DONE.
/// - `claude-fable-5` (subagent): one billed `stub_echo` call, then the
///   provider hard-fails the second call with HTTP 402.
///
/// 402 is terminal on the billable retry policy (`status_is_retryable_when_
/// billable` = 429|503 only) and carries no tool-schema signal, so the nested
/// turn dies on exactly the second call — no retry, no fallback.
async fn start_dying_subagent_stub_llm() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(
            |axum::Json(body): axum::Json<serde_json::Value>| async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                let tool_msgs = body["messages"]
                    .as_array()
                    .map(|msgs| msgs.iter().filter(|m| m["role"] == "tool").count())
                    .unwrap_or(0);
                match (model.as_str(), tool_msgs) {
                    ("claude-fable-5", 0) => axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse_tool_call("stub_echo", "{}")))
                        .expect("stub response"),
                    ("claude-fable-5", _) => axum::response::Response::builder()
                        .status(402)
                        .body(axum::body::Body::from("out of credits mid-delegation"))
                        .expect("stub response"),
                    (_, 0) => axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse_tool_call(
                            "spawn_subagent",
                            "{\"task\": \"run the echo tool and report back\", \"model\": \"claude-fable-5\"}",
                        )))
                        .expect("stub response"),
                    (_, _) => axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse_text("PARENT_DONE")))
                        .expect("stub response"),
                }
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

/// Tokens spent are spent; a failed delegation does not refund them.
///
/// The pre-fix dispatch gated the charge on `if let Ok(value) = &sub_result`
/// and then read the spend back out of the RESULT JSON, which the subagent
/// only ever populated from `TurnComplete` — an event that never fires when a
/// turn errors. A subagent that made billed calls and then died therefore
/// contributed ZERO to the parent's budget: an unbounded escape hatch, since
/// the parent could delegate again immediately with its budget untouched.
///
/// This fails if either half of that shape comes back — the `Ok` gate, or
/// sourcing the charge from the tool result instead of the accumulator.
#[tokio::test(flavor = "multi_thread")]
async fn parent_is_charged_for_a_subagent_that_died_after_billed_calls() {
    let _serial = SERIAL_TEST_LOCK.lock().await;
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let base_url = start_dying_subagent_stub_llm().await;
    prism_agent::hooks::set_provenance_ctx("subagent-charge-on-failure", "stub-model");

    let (answer, events, cost) = run_parent_turn(
        project.path(),
        &python,
        base_url,
        0,
        CommandToolPlatformAccess::VerifiedNodeOwner,
        true,
    )
    .await;

    // The delegation really failed — no result JSON to read a figure out of.
    assert_eq!(answer, "PARENT_DONE");
    let sub_result =
        tool_result_content(&events, "spawn_subagent").expect("spawn_subagent result event");
    assert!(
        sub_result.starts_with("Tool error"),
        "the subagent must have died on the provider error, so there is no result \
         JSON to read a spend figure out of: {sub_result}"
    );

    // …and the parent was still charged the tokens the nested turn really
    // burned before it died: one stub call at 100 in / 20 out.
    let charged = cost
        .events
        .iter()
        .find(|event| event.label == "subagent")
        .expect("a failed delegation must still produce a parent charge event");
    assert_eq!(
        (charged.input_tokens, charged.output_tokens),
        (100, 20),
        "the parent must be charged the subagent's real pre-failure spend"
    );
}
