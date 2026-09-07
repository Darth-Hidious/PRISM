// Each test binary includes this file and uses a subset of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use prism_agent::agent_loop;
use prism_agent::protocol::{AgentSeed, build_agent_seed};
use prism_agent::types::AgentEvent;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

// Deliberately differs from AgentConfig::default(). This catches pricing the
// turn with stale config instead of the model used by the actual LLM client.
pub const TEST_MODEL: &str = "claude-haiku-4-5";
pub const TEST_INPUT_TOKENS: u64 = 1_000;
pub const TEST_OUTPUT_TOKENS: u64 = 200;
pub const TEST_EXPECTED_COST_USD: f64 = 0.002;

const STUB_TOOL_SERVER_PY: &str = r#"
import json, sys
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

pub struct StubTurnOutcome {
    pub answer: String,
    pub estimated_cost: f64,
}

fn require_python() -> Result<PathBuf> {
    let output = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .context("python3 is required for the real agent-turn harness")?;
    anyhow::ensure!(output.status.success(), "python3 --version failed");
    Ok(PathBuf::from("python3"))
}

fn write_stub_project(dir: &Path) -> Result<()> {
    let app = dir.join("app");
    std::fs::create_dir_all(&app)?;
    std::fs::write(app.join("__init__.py"), "")?;
    std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER_PY)?;
    Ok(())
}

pub fn llm_config(base_url: String) -> LlmConfig {
    LlmConfig {
        base_url,
        model: TEST_MODEL.to_string(),
        api_key: None,
        embedding_model: None,
        timeout_secs: 30,
        ..Default::default()
    }
}

async fn start_stub_llm() -> Result<String> {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            let chunk = serde_json::json!({
                "choices": [{ "delta": { "content": "LEDGER_TURN_DONE" } }],
                "usage": {
                    "prompt_tokens": TEST_INPUT_TOKENS,
                    "completion_tokens": TEST_OUTPUT_TOKENS,
                    "total_tokens": TEST_INPUT_TOKENS + TEST_OUTPUT_TOKENS,
                }
            });
            axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(format!(
                    "data: {chunk}\n\ndata: [DONE]\n\n"
                )))
                .expect("stub SSE response")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(format!("http://{address}/v1"))
}

/// A throwaway project whose tool server answers `list_tools` with nothing.
pub struct StubProject {
    pub dir: tempfile::TempDir,
    pub python: PathBuf,
}

pub fn stub_project() -> Result<StubProject> {
    let python = require_python()?;
    let dir = tempfile::tempdir()?;
    write_stub_project(dir.path())?;
    Ok(StubProject { dir, python })
}

/// The same seed builder the backend uses, pointed at the stub project.
pub async fn stub_seed(project: &StubProject, llm_config: &LlmConfig) -> Result<AgentSeed> {
    build_agent_seed(
        &ToolServer {
            python_bin: project.python.clone(),
            project_root: project.dir.path().to_path_buf(),
            env: std::collections::BTreeMap::new(),
        },
        llm_config,
    )
    .await
}

/// Drive the public, production `run_turn` entry against local deterministic
/// stubs and return the completion data emitted by the real loop.
pub async fn run_stub_turn(session_id: &str) -> Result<StubTurnOutcome> {
    let project = stub_project()?;
    let base_url = start_stub_llm().await?;
    let llm_config = llm_config(base_url.clone());
    let mut seed = stub_seed(&project, &llm_config).await?;
    prism_agent::hooks::set_provenance_ctx(session_id, TEST_MODEL);

    let llm = LlmClient::new(llm_config);
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();
    let mut answer = String::new();
    let mut estimated_cost = None;
    agent_loop::run_turn(
        &llm,
        &mut seed.tool_server,
        &seed.command_tool_runtime,
        &mut history,
        seed.tools.as_ref(),
        seed.config.as_ref(),
        "What is the yield strength of Inconel 718 at 650 C?",
        None,
        &mut transcript,
        seed.hooks.as_ref(),
        &seed.permissions,
        None,
        &mut scratchpad,
        &mut |event| {
            if let AgentEvent::TurnComplete {
                text,
                estimated_cost: cost,
                ..
            } = event
            {
                answer = text.unwrap_or_default();
                estimated_cost = cost;
            }
        },
        None,
        None,
        None,
    )
    .await?;

    Ok(StubTurnOutcome {
        answer,
        estimated_cost: estimated_cost.context("turn did not emit its estimated cost")?,
    })
}
