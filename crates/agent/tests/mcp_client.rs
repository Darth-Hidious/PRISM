// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! MCP client end-to-end: PRISM's MCP client against a REAL stdio MCP server.
//!
//! The stub below is a genuine MCP server — newline-delimited JSON-RPC over
//! stdin/stdout speaking the real wire protocol (`initialize`, `tools/list`,
//! `tools/call`) — spawned as a child process through the same
//! `TokioChildProcess` transport production uses. No mocks: this proves the
//! handshake, tool listing, catalog admission (`extend_untrusted`,
//! `source: "mcp"`, approval-gated), discovery via catalog search, and a
//! round-trip `tools/call`.
//!
//! Requires `python3` on PATH; tests skip (with a note) when absent.

use std::path::{Path, PathBuf};

use prism_agent::mcp::{McpManager, McpServerConfig, load_config};
use prism_agent::tool_catalog::ToolCatalog;
use serde_json::json;

// ── Stub MCP server (real wire protocol, no SDK) ─────────────────────

const STUB_MCP_SERVER_PY: &str = r#"
import json, sys

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

TOOLS = [
    {
        "name": "echo",
        "description": "Echo back the provided text",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "always_fails",
        "description": "Returns an MCP-level tool error",
        "inputSchema": {"type": "object", "properties": {}},
    },
]

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    msg_id = msg.get("id")
    if msg_id is None:
        continue  # notification (e.g. notifications/initialized)
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg_id, "result": {
            "protocolVersion": msg["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "prism-stub", "version": "0.0.1"},
        }})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg_id, "result": {"tools": TOOLS}})
    elif method == "tools/call":
        params = msg.get("params") or {}
        args = params.get("arguments") or {}
        if params.get("name") == "echo":
            send({"jsonrpc": "2.0", "id": msg_id, "result": {
                "content": [{"type": "text", "text": "echo: " + str(args.get("text", ""))}],
            }})
        elif params.get("name") == "always_fails":
            send({"jsonrpc": "2.0", "id": msg_id, "result": {
                "content": [{"type": "text", "text": "deliberate stub failure"}],
                "isError": True,
            }})
        else:
            send({"jsonrpc": "2.0", "id": msg_id,
                  "error": {"code": -32602, "message": "unknown tool"}})
    elif method == "ping":
        send({"jsonrpc": "2.0", "id": msg_id, "result": {}})
    else:
        send({"jsonrpc": "2.0", "id": msg_id,
              "error": {"code": -32601, "message": "method not found"}})
"#;

fn find_python() -> Option<PathBuf> {
    let out = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    out.status.success().then(|| PathBuf::from("python3"))
}

fn write_stub_server(dir: &Path) -> PathBuf {
    let path = dir.join("stub_mcp_server.py");
    std::fs::write(&path, STUB_MCP_SERVER_PY).expect("write stub MCP server");
    path
}

fn stub_config(name: &str, python: &Path, server_py: &Path) -> McpServerConfig {
    // Built through the config-file parser so the test also covers the
    // documented ~/.prism/mcp.json schema, not just the struct.
    let raw = json!({
        "servers": [{
            "name": name,
            "command": python.display().to_string(),
            "args": [server_py.display().to_string()],
        }]
    });
    let path = server_py.with_file_name(format!("{name}-mcp.json"));
    std::fs::write(&path, raw.to_string()).expect("write mcp.json");
    load_config(&path)
        .expect("valid config parses")
        .pop()
        .expect("one server configured")
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn connects_lists_and_calls_a_real_stdio_mcp_server() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let server_py = write_stub_server(dir.path());

    // Connect: spawn + initialize handshake + tools/list over real stdio.
    let manager = McpManager::connect(vec![stub_config("stub", &python, &server_py)]).await;
    assert_eq!(manager.server_names(), vec!["stub".to_string()]);

    // Admission: the namespaced tools flow into the catalog as UNTRUSTED.
    let mut catalog = ToolCatalog::default();
    let rejected = catalog.extend_untrusted(manager.loaded_tools());
    assert!(rejected.is_empty(), "no collisions expected: {rejected:?}");
    let tool = catalog
        .find("mcp__stub__echo")
        .expect("echo tool admitted under its namespaced name");
    assert_eq!(tool.source.as_deref(), Some("mcp"));
    assert_eq!(tool.source_detail.as_deref(), Some("stub"));
    assert!(
        tool.requires_approval,
        "MCP tools must stay behind the approval gate"
    );
    assert_eq!(
        tool.input_schema["properties"]["text"]["type"],
        json!("string"),
        "remote inputSchema is preserved"
    );

    // Discovery: the same keyword search that backs find_tools sees it.
    let hits = catalog.search("echo text back", 5);
    assert!(
        hits.iter().any(|t| t.name == "mcp__stub__echo"),
        "MCP tool discoverable via catalog search"
    );

    // Round-trip call over the live session.
    let result = manager
        .call_tool("mcp__stub__echo", &json!({ "text": "hello prism" }))
        .await
        .expect("tools/call succeeds");
    assert_eq!(result, json!({ "result": "echo: hello prism" }));

    // MCP-level isError surfaces through the loop's error channel shape.
    let failure = manager
        .call_tool("mcp__stub__always_fails", &json!({}))
        .await
        .expect("transport-level success");
    assert_eq!(failure, json!({ "error": "deliberate stub failure" }));

    // Unknown names are refused instead of hitting the wire.
    assert!(
        manager
            .call_tool("mcp__stub__nope", &json!({}))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn broken_server_is_skipped_without_failing_the_rest() {
    let Some(python) = find_python() else {
        eprintln!("SKIP: python3 not on PATH");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let server_py = write_stub_server(dir.path());

    let good = stub_config("good", &python, &server_py);
    let broken = McpServerConfig {
        name: "broken".to_string(),
        transport: "stdio".to_string(),
        command: dir.path().join("no-such-binary").display().to_string(),
        args: Vec::new(),
        env: Default::default(),
        url: None,
    };
    let unsupported = McpServerConfig {
        name: "remote".to_string(),
        transport: "http".to_string(),
        command: String::new(),
        args: Vec::new(),
        env: Default::default(),
        url: Some("https://example.com/mcp".to_string()),
    };

    let manager = McpManager::connect(vec![broken, unsupported, good]).await;
    assert_eq!(
        manager.server_names(),
        vec!["good".to_string()],
        "only the healthy stdio server connects; failures are skipped"
    );
    let result = manager
        .call_tool("mcp__good__echo", &json!({ "text": "still up" }))
        .await
        .expect("healthy server still serves calls");
    assert_eq!(result, json!({ "result": "echo: still up" }));
}
