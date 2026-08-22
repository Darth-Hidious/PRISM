// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Acceptance for the store release seam, against the REAL binary.
//!
//! The live failure this pins (measured 2026-08-21): a parent PRISM process
//! held the provenance store for its whole session, shelled out to
//! `prism papers claims … --store`, and the child always died on
//!
//!   Error: failed to open Turso database
//!     Locking error: Failed locking file '….db-wal'.
//!     File is locked by another process
//!
//! — 113 entities, zero facts. libsql/turso WAL holds an exclusive OS file
//! lock per open handle, across processes. The fix releases the parent's
//! handle around anything that spawns a store-needing child
//! (`agent_loop::RunLedger` opens per write and never parks a handle).
//!
//! Here the parent (this test) does exactly what the fixed agent does: open
//! the store, write, RELEASE, spawn the real `prism papers claims --url …
//! --store` child against a scripted loopback paper + extraction endpoint,
//! then reacquire and write again. Both sides must succeed, and the child's
//! facts must land in `emmo_edge` / `prov_assertion` of the SAME temp store.
//! The agent-side regression test (a real `run_turn` whose live run-ledger
//! must not starve a child process) lives in
//! `crates/agent/tests/store_lock_release.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

/// The paper the child fetches, as JATS. Line 1 of the extracted workspace is
/// the abstract sentence — the line the scripted extractor cites.
const PAPER_JATS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<article>
  <front>
    <article-meta>
      <title-group><article-title>Inconel 718 at temperature</article-title></title-group>
      <abstract><p>The yield strength of Inconel 718 is 1035 MPa at 650 C.</p></abstract>
    </article-meta>
  </front>
  <body>
    <sec>
      <title>Results</title>
      <p>Tensile tests confirm the alloy retains its strength at elevated temperature.</p>
    </sec>
  </body>
</article>
"#;

const SUBJECT: &str = "Inconel 718";
const PREDICATE: &str = "hasProperty";
const OBJECT: &str = "yield strength";

/// One loopback server plays both roles: the publisher (GET /paper.xml) and
/// the extraction LLM (POST /v1/chat/completions, OpenAI SSE with streamed
/// tool_calls). The LLM script is stateless, keyed on how many tool results
/// the conversation already carries: read the whole paper, propose one fact
/// cited to line 1, finish.
fn start_stub_server() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            std::thread::spawn(move || {
                let _ = handle_connection(stream);
            });
        }
    });
    Ok(port)
}

fn handle_connection(mut stream: TcpStream) -> std::io::Result<()> {
    // Serve sequential requests on one connection: reqwest pools and reuses
    // it, and refusing the second request would abort the extraction loop.
    loop {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(()); // closed (e.g. the CLI's TCP connectivity probe)
        }
        let mut content_length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 {
                return Ok(());
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
            {
                content_length = value.parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
        let body = String::from_utf8_lossy(&body).into_owned();

        let (status, content_type, payload) = if request_line.starts_with("GET /paper.xml") {
            ("200 OK", "application/xml", PAPER_JATS.to_string())
        } else if request_line.starts_with("POST /v1/chat/completions") {
            ("200 OK", "text/event-stream", sse_reply(&body))
        } else {
            // /props, /v1/models, … — probes with honest absence.
            ("404 Not Found", "text/plain", String::new())
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len(),
        )?;
        stream.flush()?;
    }
}

/// The scripted extractor. Turn index = tool results already in the request.
fn sse_reply(request_body: &str) -> String {
    let tool_results = request_body.matches("\"role\":\"tool\"").count();
    let chunk = match tool_results {
        0 => tool_call_chunk("read_paper", r#"{"from_line": 1, "to_line": 400}"#),
        1 => tool_call_chunk(
            "propose_fact",
            r#"{"fact": {"subject": "Inconel 718", "predicate": "hasProperty", "object": "yield strength", "value": 1035.0, "unit": "MPa", "confidence": 0.9}, "from_line": 1, "to_line": 1}"#,
        ),
        2 => tool_call_chunk("finish", "{}"),
        _ => r#"{"choices":[{"delta":{"content":"DONE"}}]}"#.to_string(),
    };
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn tool_call_chunk(name: &str, arguments: &str) -> String {
    serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{
            "index": 0,
            "id": format!("call_{name}"),
            "type": "function",
            "function": { "name": name, "arguments": arguments }
        }] } }]
    })
    .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parent_that_releases_the_store_and_a_claims_child_both_succeed() {
    let temp = tempfile::tempdir().expect("temp workspace");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("temp HOME");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("temp project root");
    let db_path = temp.path().join("provenance.db");
    let port = start_stub_server().expect("stub publisher + extractor");

    // ── The parent holds the store … ─────────────────────────────────
    let parent_store = prism_provenance::ProvenanceStore::open(&db_path)
        .await
        .expect("parent open");
    parent_store
        .write_extracted_entity("ParentProbeBefore", "Entity", None, "local")
        .await
        .expect("parent write while holding the store");
    // … and RELEASES it before spawning a child that needs it — the seam
    // under test. While this handle is held, the child's open dies with
    // "Failed locking file '….db-wal'" (proven RED by mutation in
    // crates/agent/tests/store_lock_release.rs).
    drop(parent_store);

    // ── The real binary ingests the paper into the same store. ──────
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_prism"))
        .arg("--project-root")
        .arg(&project_root)
        .args([
            "papers",
            "claims",
            "--url",
            &format!("http://127.0.0.1:{port}/paper.xml"),
            "--format",
            "jats",
            "--llm-url",
            &format!("http://127.0.0.1:{port}/v1"),
            "--model",
            "stub-extractor",
            "--store",
        ])
        .env("PRISM_PROVENANCE_DB", &db_path)
        .env("HOME", &home)
        .output()
        .expect("spawn the real prism binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "`prism papers claims --store` must succeed against a released \
         store\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n")
            .as_str(),
    )
    .unwrap_or_else(|error| panic!("child stdout must be JSON ({error}):\n{stdout}"));
    assert_eq!(
        report["claims"].as_array().map(Vec::len),
        Some(1),
        "the child must extract exactly the scripted claim:\n{stdout}"
    );
    let written = report["stored"]["written"].as_u64().unwrap_or(0);
    assert!(
        written >= 1,
        "the child must report its claim as stored:\n{stdout}"
    );

    // ── The child's facts LANDED in emmo_edge / prov_assertion. ──────
    let store = prism_provenance::ProvenanceStore::open(&db_path)
        .await
        .expect("parent reacquire after the child");
    let neighbors = store
        .get_neighbors(SUBJECT, None, prism_provenance::LOCAL_TENANT, 10)
        .await
        .expect("query emmo_edge");
    assert!(
        !neighbors.edges.is_empty(),
        "the child's claim must land in emmo_edge:\n{stdout}"
    );
    let assertion_id = prism_provenance::conditioned_assertion_id(
        prism_provenance::LOCAL_TENANT,
        SUBJECT,
        PREDICATE,
        OBJECT,
        Some(1035.0),
        Some("MPa"),
        &[],
    )
    .expect("compute the stored assertion id");
    let assertion = store
        .assertion_by_id(&assertion_id)
        .await
        .expect("query prov_assertion");
    assert!(
        assertion.is_some(),
        "the child's claim must land in prov_assertion:\n{stdout}"
    );

    // ── Reacquiring works: the parent writes again after the child. ──
    store
        .write_extracted_entity("ParentProbeAfter", "Entity", None, "local")
        .await
        .expect("parent write after reacquiring the store");
}
