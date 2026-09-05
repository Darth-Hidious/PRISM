// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! The human can take the turn back.
//!
//! `turn.cancel` reaches [`agent_loop::run_turn_until`] as a oneshot. The
//! turn must end AT ONCE — not when the model's timeout finally fires — and
//! it must end honestly: named as a stop rather than a fault, its user
//! message kept, no tool call left without an answer, and its run ledger row
//! closed as `cancelled` rather than left `running` for ever.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use prism_agent::agent_loop;
use prism_agent::protocol::build_agent_seed;
use prism_ingest::LlmConfig;
use prism_ingest::llm::LlmClient;
use prism_python_bridge::ToolServer;

const SESSION_ID: &str = "turn-cancel-session";
const TEST_MODEL: &str = "claude-haiku-4-5";
/// Long enough that a turn ending inside the test can only be the stop.
const LLM_TIMEOUT_SECS: u64 = 120;

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

fn python3() -> Option<PathBuf> {
    let output = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    output.status.success().then(|| PathBuf::from("python3"))
}

fn write_stub_project(dir: &Path) -> Result<()> {
    let app = dir.join("app");
    std::fs::create_dir_all(&app)?;
    std::fs::write(app.join("__init__.py"), "")?;
    std::fs::write(app.join("tool_server.py"), STUB_TOOL_SERVER_PY)?;
    Ok(())
}

/// An LLM endpoint that accepts the request and never answers — the shape
/// of a stalled model, which is when a human reaches for the stop key.
async fn start_stalled_llm() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            // Hold the connection open, answering nothing, until the test ends.
            tokio::spawn(async move {
                let _held = socket;
                std::future::pending::<()>().await
            });
        }
    });
    Ok(format!("http://{address}/v1"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_order_ends_a_stalled_turn_at_once_and_closes_its_ledger_row_as_cancelled()
-> Result<()> {
    let Some(python) = python3() else {
        eprintln!("SKIP: python3 not on PATH");
        return Ok(());
    };
    let project = tempfile::tempdir()?;
    write_stub_project(project.path())?;
    let llm_config = LlmConfig {
        base_url: start_stalled_llm().await?,
        model: TEST_MODEL.to_string(),
        api_key: None,
        embedding_model: None,
        timeout_secs: LLM_TIMEOUT_SECS,
        ..Default::default()
    };
    let mut seed = build_agent_seed(
        &ToolServer {
            python_bin: python,
            project_root: project.path().to_path_buf(),
            env: std::collections::BTreeMap::new(),
        },
        &llm_config,
    )
    .await?;
    prism_agent::hooks::set_provenance_ctx(SESSION_ID, TEST_MODEL);

    let llm = LlmClient::new(llm_config);
    let mut history = Vec::new();
    let mut transcript = prism_agent::transcript::TranscriptStore::new(None);
    let mut scratchpad = prism_agent::scratchpad::Scratchpad::new();

    // The stop lands 300 ms into a turn whose model will never answer.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = cancel_tx.send(());
    });
    let started = Instant::now();
    let result = agent_loop::run_turn_until(
        Some(cancel_rx),
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
        &mut |_event| {},
        None,
        None,
        None,
    )
    .await;
    let elapsed = started.elapsed();

    let error = result.expect_err("a stopped turn does not report success");
    assert!(
        error.is::<agent_loop::TurnCancelled>(),
        "the stop is named as a stop, not a fault: {error:#}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the stop is immediate, not the {LLM_TIMEOUT_SECS}s model timeout: {elapsed:?}"
    );

    // The record is whole: the user's message went in, and nothing is left
    // without an answer.
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].role, "user");
    assert_eq!(
        agent_loop::close_dangling_tool_calls(&mut history, "stopped"),
        0
    );
    // No tool call was in flight, so the tool server is still in step.
    assert!(!seed.tool_server.is_desynchronized());

    // The run ledger row ends `cancelled` — not `running` for ever, which is
    // what dropping the turn's future without this seam would have left.
    let store =
        prism_provenance::ProvenanceStore::open(&prism_agent::hooks::provenance_db_path()).await?;
    let runs = store
        .list_agent_runs(&prism_provenance::AgentRunFilter {
            session_id: Some(SESSION_ID.to_string()),
            ..Default::default()
        })
        .await?;
    let run = runs
        .iter()
        .find(|run| run.role == "agent")
        .context("the turn wrote its run row before it was stopped")?;
    assert_eq!(
        run.status,
        prism_provenance::AgentRunStatus::Cancelled,
        "{run:?}"
    );
    assert!(run.ended_at.is_some(), "{run:?}");
    assert_eq!(run.last_error.as_deref(), Some("turn stopped by the user"));
    Ok(())
}
