// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Pre-flight reprompter, driven through a REAL `run_turn` against a stub
//! OpenAI-compatible LLM that records every request body it receives.
//!
//! The wire log is the point. Unit tests can prove `triage` returns Proceed;
//! only the recorded traffic proves what the expert query actually COST, and
//! that the boss query never reached the agent loop at all.
//!
//! 1. `expert_query_reaches_the_model_untouched` — the negative case, and the
//!    one that matters most: a well-formed expert query produces exactly ONE
//!    request, it is the agent turn itself, and the classifier prompt never
//!    appears on the wire. Zero added tokens, zero added round-trips.
//! 2. `supplier_request_is_answered_honestly_without_running_the_agent` — the
//!    boss case: one cheap classifier call, the agent loop never runs, and the
//!    user gets a plain statement of the capability gap plus options.
//! 3. `the_same_slot_is_not_asked_twice_in_a_session` — a second supplier
//!    request in the same session proceeds (routed, with the honesty carried
//!    in the routing hint) instead of repeating the question.
//!
//! Requires `python3` on PATH; tests skip (with a note) when absent.

use std::path::{Path, PathBuf};

use prism_agent::agent_loop;
use prism_agent::protocol::{AgentSeed, build_agent_seed};
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

// ── Stub python tool server (no tools; the loop only needs it alive) ──

const STUB_TOOL_SERVER_PY: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if req.get("method") == "list_tools":
        resp = {"tools": []}
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

// ── Stub LLM that records every request ──────────────────────────────

/// Every request body the stub received, in arrival order.
pub type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

/// Serve `/v1/chat/completions`, answering the STREAMING agent turn with SSE
/// and the NON-streaming classifier call with a plain JSON completion whose
/// content is `classifier_reply`. Two shapes on one route is exactly how the
/// real backends behave, and it is what lets the log distinguish the two.
async fn start_stub_llm(classifier_reply: &'static str) -> (String, RequestLog) {
    use axum::routing::post;
    let log: RequestLog = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = log.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let sink = sink.clone();
            async move {
                let streaming = body["stream"] == serde_json::Value::Bool(true);
                sink.lock().expect("request log").push(body);
                if streaming {
                    let chunk = serde_json::json!({
                        "choices": [{ "delta": { "content": "AGENT_ANSWER" } }]
                    });
                    return axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(format!(
                            "data: {chunk}\n\ndata: [DONE]\n\n"
                        )))
                        .expect("stub sse response");
                }
                let completion = serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": classifier_reply },
                        "finish_reason": "stop"
                    }],
                    "usage": { "prompt_tokens": 150, "completion_tokens": 2, "total_tokens": 152 }
                });
                axum::response::Response::builder()
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(completion.to_string()))
                    .expect("stub json response")
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
    (format!("http://{addr}/v1"), log)
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

/// True when this request carries the pre-flight classifier's system prompt —
/// i.e. the user paid for the reprompter on this turn.
fn is_classifier_request(body: &serde_json::Value) -> bool {
    body["messages"].as_array().is_some_and(|msgs| {
        msgs.iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("Classify the user's request"))
        })
    })
}

/// One session's worth of state, so multi-turn tests share a scratchpad — the
/// never-ask-twice ledger lives there.
struct Session {
    seed: AgentSeed,
    llm: LlmClient,
    history: Vec<prism_ingest::llm::ChatMessage>,
    transcript: prism_agent::transcript::TranscriptStore,
    scratchpad: prism_agent::scratchpad::Scratchpad,
}

impl Session {
    async fn new(project: &Path, python: &Path, base_url: String) -> Self {
        let seed = build_agent_seed(
            &tool_server_config(project, python),
            &llm_config(base_url.clone()),
        )
        .await
        .expect("agent seed");
        Self {
            seed,
            llm: LlmClient::new(llm_config(base_url)),
            history: Vec::new(),
            transcript: prism_agent::transcript::TranscriptStore::new(None),
            scratchpad: prism_agent::scratchpad::Scratchpad::new(),
        }
    }

    async fn turn(&mut self, message: &str) -> String {
        let mut answer = String::new();
        agent_loop::run_turn(
            &self.llm,
            &mut self.seed.tool_server,
            &self.seed.command_tool_runtime,
            &mut self.history,
            self.seed.tools.as_ref(),
            self.seed.config.as_ref(),
            message,
            None,
            &mut self.transcript,
            self.seed.hooks.as_ref(),
            &self.seed.permissions,
            None,
            &mut self.scratchpad,
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
        )
        .await
        .expect("turn");
        answer
    }
}

// ── Tests ────────────────────────────────────────────────────────────

/// THE test that matters. A well-formed expert query must pay nothing: one
/// request, and it is the agent turn — no classifier call, no extra tokens, no
/// extra round-trip, no question asked.
#[tokio::test(flavor = "multi_thread")]
async fn expert_query_reaches_the_model_untouched() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    // If the reprompter DID fire here, the stub would answer "supplier" and the
    // turn would end in a question — so this reply is a tripwire, not scenery.
    let (base_url, log) = start_stub_llm("supplier").await;

    let mut session = Session::new(project.path(), &python, base_url).await;
    let answer = session
        .turn("What is the yield strength of Inconel 718 at 650 C?")
        .await;

    assert_eq!(answer, "AGENT_ANSWER", "expert query must reach the model");
    let requests = log.lock().expect("request log").clone();
    assert_eq!(
        requests.len(),
        1,
        "expert query must cost exactly one request (the turn itself)"
    );
    assert!(
        !requests.iter().any(is_classifier_request),
        "the classifier prompt must never reach the wire for a well-formed query"
    );
    assert_eq!(
        requests[0]["stream"],
        serde_json::Value::Bool(true),
        "the one request is the streaming agent turn"
    );
}

/// The boss case. Supplier discovery is recognised, the agent loop never runs,
/// and the answer is an honest statement of the gap plus concrete options —
/// not a generic web search dressed as materials science.
#[tokio::test(flavor = "multi_thread")]
async fn supplier_request_is_answered_honestly_without_running_the_agent() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let (base_url, log) = start_stub_llm("supplier").await;

    let mut session = Session::new(project.path(), &python, base_url).await;
    let answer = session
        .turn("please research the web and find companies that can do that machining in Poland")
        .await;

    assert!(
        answer.contains("cannot answer it") && answer.contains("no company registry"),
        "must state the capability gap plainly: {answer}"
    );
    assert!(
        answer.contains("plain web search"),
        "must offer the web search LABELLED as one, not disguised: {answer}"
    );
    assert_ne!(answer, "AGENT_ANSWER", "the agent loop must not have run");

    let requests = log.lock().expect("request log").clone();
    assert_eq!(
        requests.len(),
        1,
        "one cheap classifier call and nothing else — cheaper than the wrong answer"
    );
    assert!(is_classifier_request(&requests[0]));
    assert!(
        requests[0]["stream"] != serde_json::Value::Bool(true),
        "the classifier call is not the agent turn"
    );
}

/// Never ask twice. The second supplier request in the same session proceeds —
/// routed, with the capability gap carried into the model's context — instead
/// of repeating the question the user already answered or ignored.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_slot_is_not_asked_twice_in_a_session() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let (base_url, log) = start_stub_llm("supplier").await;

    let mut session = Session::new(project.path(), &python, base_url).await;
    let first = session
        .turn("find companies in Poland that can do this machining")
        .await;
    assert!(first.contains("cannot answer it"), "first turn asks");

    let second = session
        .turn("no, I really need suppliers for this — companies, in Poland")
        .await;
    assert_eq!(
        second, "AGENT_ANSWER",
        "second time the turn must run, not re-ask: {second}"
    );

    // …and the model was handed the capability gap rather than left to guess.
    let requests = log.lock().expect("request log").clone();
    let turn_request = requests
        .iter()
        .rfind(|r| r["stream"] == serde_json::Value::Bool(true))
        .expect("an agent turn ran");
    let carried = turn_request["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.contains("PRISM HAS NO SUCH CAPABILITY"))
        });
    assert!(carried, "routing hint must carry the honesty into the turn");
}

/// The ledger must survive what the HTTP chat surface actually does. That
/// transport (`service.rs`) builds a FRESH `Scratchpad` on every turn, and
/// `restore_history_and_transcript_from_messages` clears it on `/resume` — so a
/// scratchpad-based ledger would be silently inert there and the same question
/// would be asked forever. Resetting the scratchpad between turns here
/// reproduces that exactly; the ledger lives in `history`, which both paths
/// restore, so it still holds.
#[tokio::test(flavor = "multi_thread")]
async fn the_ledger_survives_a_scratchpad_reset() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let (base_url, _log) = start_stub_llm("supplier").await;

    let mut session = Session::new(project.path(), &python, base_url).await;
    let first = session.turn("find companies in Poland").await;
    assert!(first.contains("cannot answer it"), "first turn asks");

    // What service.rs does on every single turn.
    session.scratchpad = prism_agent::scratchpad::Scratchpad::new();

    let second = session.turn("I need suppliers in Poland").await;
    assert_eq!(
        second, "AGENT_ANSWER",
        "re-asked after a scratchpad reset — the ledger is not session state: {second}"
    );
}

/// The routing hint is turn-scoped scaffolding. It must never survive into a
/// later turn's request, or every subsequent turn is misrouted by a stale
/// classification. Stripping happens at the START of each turn precisely so no
/// exit path (error, budget exhaustion, max iterations) can leak it.
#[tokio::test(flavor = "multi_thread")]
async fn the_routing_hint_does_not_leak_into_the_next_turn() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    // `literature` is served, so a routing hint is injected rather than a question.
    let (base_url, log) = start_stub_llm("literature").await;

    let mut session = Session::new(project.path(), &python, base_url).await;
    session
        .turn("which companies have published on Inconel 718 fatigue")
        .await;
    let after_first = log.lock().expect("log").len();

    // A clean expert query: no marker, so no classifier call and no new hint.
    session
        .turn("What is the yield strength of Inconel 718 at 650 C?")
        .await;

    let requests = log.lock().expect("log").clone();
    let second_turn = &requests[after_first..];
    assert_eq!(
        second_turn.len(),
        1,
        "the follow-up expert query must cost exactly one request"
    );
    // Match the injected hint EXACTLY. The system prompt also mentions
    // "PRE-FLIGHT ROUTING" (it tells the model how to honour one), so a loose
    // substring search here would report a leak on every turn.
    let leaked = second_turn[0]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("<system-reminder>PRE-FLIGHT ROUTING"))
        });
    assert!(!leaked, "stale routing hint leaked into the next turn");
}
