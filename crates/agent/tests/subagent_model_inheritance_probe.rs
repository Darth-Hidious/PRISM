// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! PROBE (not a permanent test): what model string does a subagent actually
//! send when the caller does NOT name one?
//!
//! `subagent::parse_args` inherits `AgentConfig.model`, but every LLM request
//! the parent makes uses `LlmConfig.model`. These are two different fields.
//! This probe drives the REAL dispatch path — `build_agent_seed` →
//! `agent_loop::run_turn` → `execute_spawn_subagent` — against a stub
//! OpenAI-compatible endpoint that RECORDS the `model` field of every request
//! it receives, and reports what actually reached the wire.
//!
//! Deliberately different from `subagent_nested_turn.rs`: that file's parent
//! script names `"model": "claude-fable-5"` explicitly in the spawn args, so
//! it never exercises inheritance. Here the args carry NO model.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use prism_agent::agent_loop;
use prism_agent::command_tools::CommandToolPlatformAccess;
use prism_agent::protocol::build_agent_seed;
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

/// Marker planted in the delegated task text so the stub can tell the nested
/// turn's requests apart from the parent's WITHOUT routing on the model field
/// (routing on the model would beg the very question being measured).
const PROBE_MARKER: &str = "PROBE_SUBTASK_MARKER";

/// The parent's real route. Stands in for `glm-5.3` on the z.ai coding
/// endpoint: a model only THIS endpoint serves.
const PARENT_MODEL: &str = "stub-model";

const STUB_TOOL_SERVER_PY: &str = r#"
import sys, json
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
                "description": "Stub echo tool (test only).",
                "input_schema": {"type": "object", "properties": {}},
                "requires_approval": False,
            },
        ]}
    elif method == "call_tool":
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

/// One recorded inbound request: the `model` field on the wire, and whether
/// this request belongs to the NESTED turn (its user message carries the
/// delegated task marker).
#[derive(Debug, Clone)]
struct SeenRequest {
    model: String,
    is_nested: bool,
}

/// Stub endpoint. Routes on message shape ONLY — never on the model — and
/// records every `model` string it is asked for.
async fn start_recording_stub_llm() -> (String, Arc<Mutex<Vec<SeenRequest>>>) {
    use axum::routing::post;
    let seen: Arc<Mutex<Vec<SeenRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_for_handler = seen.clone();

    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let seen = seen_for_handler.clone();
            async move {
                let model = body["model"].as_str().unwrap_or_default().to_string();
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                // The nested turn's USER message is the delegated task. The
                // parent's user message never contains the marker (the marker
                // reaches the parent's history only inside an assistant
                // tool_call and a tool result, not a user message).
                let is_nested = messages.iter().any(|m| {
                    m["role"] == "user"
                        && m["content"]
                            .as_str()
                            .unwrap_or_default()
                            .contains(PROBE_MARKER)
                });
                let last_is_tool = messages
                    .last()
                    .map(|m| m["role"] == "tool")
                    .unwrap_or(false);
                seen.lock().expect("record lock").push(SeenRequest {
                    model: model.clone(),
                    is_nested,
                });

                let sse = if is_nested {
                    sse_text("SUBAGENT_DONE")
                } else if last_is_tool {
                    sse_text("PARENT_DONE")
                } else {
                    // NO `model` key in the args — this is the whole point:
                    // exercise the inheritance branch of `parse_args`.
                    sse_tool_call(
                        "spawn_subagent",
                        &format!("{{\"task\": \"{PROBE_MARKER}: say something and stop\"}}"),
                    )
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
    (format!("http://{addr}/v1"), seen)
}

fn llm_config(base_url: String) -> LlmConfig {
    LlmConfig {
        base_url,
        model: PARENT_MODEL.to_string(),
        api_key: None,
        embedding_model: None,
        timeout_secs: 30,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn probe_what_model_an_unnamed_subagent_actually_requests() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let project = tempfile::tempdir().expect("tempdir");
    write_stub_project(project.path());
    let (base_url, seen) = start_recording_stub_llm().await;
    prism_agent::hooks::set_provenance_ctx("subagent-model-probe", PARENT_MODEL);

    // EXACTLY the production seed: `build_agent_seed` is what the stdio and
    // native backends call, and it is where the parent's AgentConfig is born.
    let seed = build_agent_seed(
        &tool_server_config(project.path(), &python),
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

    // The FIRST measurement, before any turn runs: what does the seed's
    // AgentConfig say the model is, versus what the LLM client is actually
    // configured with? `protocol::spawn_agent_turn` clones this config and
    // overrides auto_approve / core_tools_only / system_prompt — never model.
    let seed_agent_config_model = config.model.clone();
    let llm = LlmClient::new(llm_config(base_url));
    let live_llm_model = llm.config().model.clone();
    eprintln!("PROBE: AgentConfig.model = {seed_agent_config_model:?}");
    eprintln!("PROBE: LlmConfig.model   = {live_llm_model:?}");

    let config = config.as_ref().clone();
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut events: Vec<AgentEvent> = Vec::new();
    let mut policy = prism_policy::PolicyEngine::with_discovery(None).ok();

    prism_agent::command_tools::with_platform_access(
        CommandToolPlatformAccess::VerifiedNodeOwner,
        agent_loop::run_turn(
            &llm,
            &mut tool_server,
            &command_tool_runtime,
            &mut history,
            tools.as_ref(),
            &config,
            "delegate the echo task",
            None,
            &mut transcript,
            hooks.as_ref(),
            &permissions,
            None,
            &mut scratchpad,
            &mut |event| events.push(event),
            None,
            policy.as_mut(),
            Some(&subagent_lanes),
        ),
    )
    .await
    .expect("parent turn");

    // What the subagent REPORTED it ran as (production-produced JSON).
    let sub_result = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallResult {
                tool_name, content, ..
            } if tool_name == "spawn_subagent" => Some(content.clone()),
            _ => None,
        })
        .expect("spawn_subagent result event");
    eprintln!("PROBE: spawn_subagent tool result = {sub_result}");

    // What actually reached the wire.
    let recorded = seen.lock().expect("record lock").clone();
    for r in &recorded {
        eprintln!("PROBE: request model={:?} nested={}", r.model, r.is_nested);
    }
    let nested_models: Vec<String> = recorded
        .iter()
        .filter(|r| r.is_nested)
        .map(|r| r.model.clone())
        .collect();
    eprintln!("PROBE: NESTED REQUEST MODELS = {nested_models:?}");

    assert!(
        !nested_models.is_empty(),
        "no nested request was observed — the probe did not exercise the path"
    );

    // THE ASSERTION UNDER TEST: a subagent that names no model must be sent to
    // the SAME model the parent is actually routed to. Anything else is a
    // request the parent's endpoint may not serve.
    assert_eq!(
        nested_models[0], live_llm_model,
        "subagent inherited the wrong model: sent {:?} to an endpoint serving {:?} \
         (AgentConfig.model was {:?})",
        nested_models[0], live_llm_model, seed_agent_config_model
    );
}
