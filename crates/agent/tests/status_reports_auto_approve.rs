//! Audit 2026-09-07: every `ui.status` snapshot reported `config.auto_approve`
//! — the seed's never-updated default — while the approval gate read the flag
//! `init` set. The status bar said "off" while every approval was bypassed.
//! This drives the real server loop: `init` with `auto_approve: true`, then
//! the first status snapshot must say so.
#[path = "support/agent_run_harness.rs"]
mod agent_run_harness;
mod common;

use std::time::{Duration, Instant};

use prism_agent::protocol::run_server_native;
use prism_python_bridge::ToolServer;

#[test]
fn a_status_snapshot_reports_the_auto_approve_the_session_was_started_with() {
    let project = agent_run_harness::stub_project().expect("stub project");
    let llm_config = agent_run_harness::llm_config("http://127.0.0.1:9/v1".to_string());
    let tool_server = ToolServer {
        python_bin: project.python.clone(),
        project_root: project.dir.path().to_path_buf(),
        env: std::collections::BTreeMap::new(),
    };
    let (input_tx, input_rx) = std::sync::mpsc::channel::<String>();
    let (output_tx, output_rx) = std::sync::mpsc::channel::<serde_json::Value>();
    // The server owns its own runtime and blocks until its input ends; the
    // stub project keeps it from touching anything real.
    let server =
        std::thread::spawn(move || run_server_native(llm_config, tool_server, input_rx, output_tx));
    input_tx
        .send(
            r#"{"jsonrpc":"2.0","id":1,"method":"init","params":{"auto_approve":true}}"#
                .to_string(),
        )
        .expect("server accepts input");

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut status = None;
    while Instant::now() < deadline {
        match output_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(message) if message["method"] == "ui.status" => {
                status = Some(message["params"].clone());
                break;
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(input_tx);
    let _ = server.join();

    let status = status.expect("init emits a status snapshot");
    assert_eq!(
        status["auto_approve"], true,
        "init said auto_approve=true; the status bar must say so too: {status}"
    );
}
