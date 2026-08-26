//! End-to-end tests for `prism mcp-server-native` — the JSON-RPC surface an
//! MCP host actually speaks to.
//!
//! Every test here spawns the REAL binary and drives `tools/list` /
//! `tools/call` over stdin/stdout. None of them calls a gating helper
//! directly, because the defect being pinned was precisely a handler that
//! skipped the helpers: `tools/call` executed with `policy: None`, no
//! approval check, and a hardcoded `"isError": false` — so any MCP host that
//! could spawn the binary could run `doctor_fix`, `deploy_create`,
//! `compute_submit` and every other gated tool with no prompt and no policy
//! evaluation.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

const PRISM: &str = env!("CARGO_BIN_EXE_prism");

/// How long one JSON-RPC response may take. The gated paths answer without
/// executing anything, so this is generous on purpose: a response that takes
/// minutes means a refusal was dropped and a real tool ran.
const RESPONSE_DEADLINE: Duration = Duration::from_secs(120);

struct McpServer {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
    home: tempfile::TempDir,
    next_id: u64,
}

impl McpServer {
    fn spawn() -> Self {
        let home = tempfile::tempdir().expect("create temp HOME");
        let mut child = Command::new(PRISM)
            .arg("mcp-server-native")
            // Isolated HOME: only the built-in default policy loads (no
            // ~/.prism/policies), no credentials, and anything a broken
            // build might execute cannot touch the real ~/.prism.
            .env("HOME", home.path())
            // Hard offline: if a refusal is ever dropped and a tool runs,
            // it must not reach the network from a test.
            .env("PRISM_OFFLINE", "1")
            .env("PRISM_PYTHON", "/usr/bin/python3")
            .env_remove("MARC27_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn prism mcp-server-native");

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        // Reader thread: `tools/call` on a mutated build can block for a
        // long time, and a plain `read_line` would hang the whole test
        // binary instead of failing the assertion.
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let mut server = Self {
            child,
            stdin,
            lines,
            home,
            next_id: 0,
        };
        // MCP handshake, as a host would perform it.
        let init = server.request("initialize", json!({}));
        assert_eq!(
            init["result"]["serverInfo"]["name"], "prism-rust",
            "initialize handshake failed: {init}"
        );
        server
    }

    /// Send one JSON-RPC request and wait for its response line.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&request).expect("encode request");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.flush().expect("flush request");

        let line = self
            .lines
            .recv_timeout(RESPONSE_DEADLINE)
            .unwrap_or_else(|_| {
                panic!(
                    "no response to {method} within {RESPONSE_DEADLINE:?} — \
                     a gate refusal answers instantly, so a stall means a \
                     refused tool actually executed"
                )
            });
        serde_json::from_str(&line).expect("response is JSON")
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    /// The single text block of an MCP tool result.
    fn result_text(response: &Value) -> &str {
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in {response}"))
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `tools/list` must let the host tell a gated tool from an unattended one.
/// The pre-fix listing stripped `requires_approval`, so a host offered its
/// model tools this server should refuse.
#[test]
fn tools_list_surfaces_requires_approval() {
    let mut server = McpServer::spawn();
    let response = server.request("tools/list", json!({}));
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list returned no array: {response}"));

    let approval_of = |name: &str| -> bool {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from tools/list"))["requires_approval"]
            .as_bool()
            .unwrap_or_else(|| panic!("{name} carries no requires_approval flag"))
    };

    assert!(approval_of("doctor_fix"), "doctor_fix is approval-gated");
    // `mesh_publish` folded into `mesh_write(action:"publish")`; the approval
    // rode the name, so the gated surface is the one to assert on. A test
    // naming a collapsed tool fails on the name and says nothing about the
    // rule it exists to guard.
    assert!(approval_of("mesh_write"), "mesh_write is approval-gated");
    assert!(!approval_of("doctor"), "doctor runs unattended");
    // compute_cancel is NOT approval-gated — the OPA policy gate is the only
    // thing standing between an MCP host and a spend-affecting broker call,
    // which is why `policy_denied_tool_is_refused` below must exist.
    assert!(!approval_of("compute_cancel"));
}

/// Approval-gated tools must be refused outright: an MCP host has nobody at
/// the keyboard, and this protocol carries no approval round-trip. Same
/// standard as the single-tool executor's relay-caller refusal.
#[test]
fn approval_gated_tools_are_refused_not_executed() {
    let mut server = McpServer::spawn();

    // Sentinel: doctor_fix destroys and rebuilds ~/.prism/venv. If the
    // refusal is ever dropped, the message assertion fails AND this file
    // disappears — two independent ways to see the same catastrophe.
    let venv = server.home.path().join(".prism/venv");
    std::fs::create_dir_all(&venv).unwrap();
    let sentinel = venv.join("sentinel");
    std::fs::write(&sentinel, b"still here").unwrap();

    for tool in ["doctor_fix", "mesh_publish"] {
        let response = server.call_tool(tool, json!({}));
        assert_eq!(
            response["result"]["isError"], true,
            "{tool} must come back as a tool error: {response}"
        );
        let text = McpServer::result_text(&response);
        assert!(
            text.contains("approval-gated"),
            "{tool} refusal must say why: {text}"
        );
    }

    assert!(
        sentinel.exists(),
        "doctor_fix executed — the venv sentinel is gone"
    );
}

/// A tool that is NOT approval-gated but is destructive per policy
/// (`compute_cancel` — spend-affecting, `requires_approval: false`) must be
/// stopped by the OPA gate. This is the test that dies if `tools/call` goes
/// back to `policy: None`.
#[test]
fn policy_denied_tool_is_refused() {
    let mut server = McpServer::spawn();
    let response = server.call_tool("compute_cancel", json!({ "job_id": "job-123" }));
    assert_eq!(
        response["result"]["isError"], true,
        "compute_cancel must be denied: {response}"
    );
    let text = McpServer::result_text(&response);
    assert!(
        text.contains("denied by OPA policy"),
        "the refusal must name the policy gate: {text}"
    );
}

/// A tool that runs and fails must say so. The pre-fix handler hardcoded
/// `"isError": false` over every result, so a host's model read failures as
/// successes.
#[test]
fn failed_execution_reports_is_error() {
    let mut server = McpServer::spawn();
    // `goal_status` passes both gates (read-only, no approval) and executes
    // `prism campaign status <id>`; a nonexistent goal id in an empty HOME
    // fails deterministically, fast, and fully on-device.
    let response = server.call_tool("goal_status", json!({ "id": "no-such-goal" }));
    let text = McpServer::result_text(&response);
    assert_eq!(
        response["result"]["isError"], true,
        "a failed child process must be an error result: {response} / {text}"
    );
}
