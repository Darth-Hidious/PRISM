//! Adding an MCP server takes effect without restarting PRISM.
//!
//! Drives the REAL path end to end — a real config file, a real stdio server
//! spawned as a child process, a real MCP handshake, the real
//! `mcp::reload_global()` — because the unit tests exercise the catalog
//! rebuild against hand-made `LoadedTool`s, and a catalog that rebuilds
//! correctly proves nothing about whether a server ever connects.
//!
//! Skipped, loudly, when no MCP server binary is on this machine: a test that
//! silently passes because it found nothing to talk to is worse than one that
//! does not run.

use std::path::PathBuf;

use prism_agent::tool_catalog::{ToolCatalog, install_live, live};
use serde_json::json;

/// A real, spec-compliant stdio MCP server, if one is installed.
fn server_command() -> Option<PathBuf> {
    let candidate =
        PathBuf::from("/opt/homebrew/lib/node_modules/@so2liu/pty-mcp-server/dist/index.js");
    candidate.exists().then_some(candidate)
}

fn write_config(home: &std::path::Path, servers: serde_json::Value) {
    let dir = home.join(".prism");
    std::fs::create_dir_all(&dir).expect("temp .prism");
    std::fs::write(
        dir.join("mcp.json"),
        serde_json::to_string_pretty(&json!({ "servers": servers })).expect("config json"),
    )
    .expect("write mcp.json");
}

fn mcp_tool_names() -> Vec<String> {
    live()
        .expect("a catalog is published")
        .tool_names()
        .into_iter()
        .filter(|name| name.starts_with("mcp__"))
        .collect()
}

/// The whole point, in one arc: a server the process did not start with
/// becomes callable, and then stops being callable, with no restart in
/// between.
#[tokio::test]
async fn a_server_added_to_the_config_becomes_callable_without_a_restart() {
    let Some(server) = server_command() else {
        eprintln!(
            "SKIPPED: no stdio MCP server installed to test against. \
             This test is a no-op here and proves nothing."
        );
        return;
    };

    let home = tempfile::tempdir().expect("temp home");
    // `default_config_path()` reads the home directory, so the test owns one.
    // Safe here: this integration binary holds exactly one test, so nothing
    // else in the process is reading the environment concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
    }

    // Start from a catalog with no MCP tools at all — the state of a PRISM
    // launched before the server existed.
    install_live(
        ToolCatalog::from_tool_server_json(&json!({ "tools": [] })),
        Vec::new(),
    );
    assert!(
        mcp_tool_names().is_empty(),
        "precondition: nothing external is loaded yet"
    );

    // The agent writes the config itself — this is the surface that was
    // already open, with `file`/`execute_bash` in its core set.
    write_config(
        home.path(),
        json!([{
            "name": "pty",
            "transport": "stdio",
            "command": "node",
            "args": [server.to_string_lossy()],
        }]),
    );

    let report = prism_agent::mcp::reload_global().await;

    assert!(
        report.failed.is_empty(),
        "the server must connect; failures: {:?}",
        report.failed
    );
    assert_eq!(report.servers, vec!["pty".to_string()]);
    assert_eq!(report.added, vec!["pty".to_string()], "reported as new");
    let after_add = mcp_tool_names();
    eprintln!(
        "connected {:?}; tools now callable: {:?}",
        report.servers, after_add
    );
    assert!(
        !after_add.is_empty(),
        "the server's tools must be callable without a restart"
    );
    assert!(
        after_add.iter().all(|name| name.starts_with("mcp__pty__")),
        "namespaced under the server that supplied them: {after_add:?}"
    );

    // Listing it is not using it. CALL one, through the manager that was
    // published by the reload, and require a real answer back — a catalog
    // entry proves the name is known, not that anything is on the other end.
    let called = prism_agent::mcp::global()
        .expect("the reload published a manager")
        .call_tool("mcp__pty__list_sessions", &json!({}))
        .await
        .expect("a hot-added server must actually answer");
    eprintln!("called mcp__pty__list_sessions -> {called}");
    // A round trip is not a successful call. The first version of this test
    // asserted only that the Result was Ok, and passed while the server was
    // replying "Invalid input: expected object, received undefined" — PRISM
    // was dropping `{}` instead of sending it, so the tool never ran.
    assert!(
        called.get("error").is_none(),
        "the tool itself must accept and answer the call: {called}"
    );

    // Now the operator removes it again. Reload is the undo — nothing
    // accumulates, and no second mechanism is needed to take a server away.
    write_config(home.path(), json!([]));
    let report = prism_agent::mcp::reload_global().await;

    assert!(report.servers.is_empty(), "no servers configured any more");
    assert_eq!(report.removed, vec!["pty".to_string()], "reported as gone");
    assert!(
        mcp_tool_names().is_empty(),
        "a server deleted from the config stops being callable: {:?}",
        mcp_tool_names()
    );
}
