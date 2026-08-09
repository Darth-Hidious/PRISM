//! The mesh must not report states it is not in.
//!
//! Pinned here, against the real binary:
//!
//! 1. A node whose mesh REFUSED to start (offline / unauthenticated) must not
//!    answer `/api/mesh/nodes` with `"online": true`. Pre-fix, the boot path
//!    wrote an `Online` handle into server state before the mesh decided
//!    whether it may run, so the REST API, `mesh peers` and `mesh health`
//!    all reported online while the same process printed "Mesh disabled" —
//!    and `mesh sync` against such a node printed `✓ 0 entities synced`
//!    (exit 0) instead of failing.
//!
//! 2. `mesh sync` must refuse to pull from the node itself. The Kafka path
//!    skips self-publishes (`node_id == our_node_id`); the direct CLI path
//!    had no such guard, so a node could sync its own facts into
//!    `mesh:{its own id}` and then serve them back as peer knowledge.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PRISM: &str = env!("CARGO_BIN_EXE_prism");

/// Kills the daemon even when an assertion panics first.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A port nothing else on this machine is answering right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0")
        .local_addr()
        .expect("local addr")
        .port()
}

/// Recursively look for a file named `name` under `root`.
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|f| f == name) {
            return Some(path);
        }
    }
    None
}

/// Minimal HTTP GET — enough to read one JSON body from the node's own API
/// without adding an HTTP client to the dev-dependencies.
fn http_get_json(port: u16, path: &str) -> serde_json::Value {
    // The dashboard task is spawned before the mesh section, so by the time
    // the boot line has printed the listener is up in practice — the retry
    // only absorbs scheduler jitter.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("could not reach the node dashboard: {e}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .expect("send request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    // Body = first '{' to last '}' — tolerant of both content-length and
    // chunked framing for a single JSON object.
    let start = response
        .find('{')
        .unwrap_or_else(|| panic!("no JSON object in response to {path}:\n{response}"));
    let end = response.rfind('}').expect("closing brace");
    serde_json::from_str(&response[start..=end])
        .unwrap_or_else(|e| panic!("unparsable body ({e}) in:\n{response}"))
}

/// 1 — the REST API must report the mesh the way the mesh decided.
///
/// This drives the REAL boot path: `prism node up --offline` in an isolated
/// HOME, then asks the running daemon's own API. The synchronization point
/// is the boot line ("Mesh: ...") on the child's stdout: it prints strictly
/// AFTER the boot code has written the mesh handle into server state, so a
/// pass here can never be the pre-write default state masquerading as the
/// fix. (Rust's stdout is line-buffered even when piped, so the line
/// arrives promptly.)
#[test]
fn a_refused_mesh_is_reported_offline_by_the_nodes_own_api() {
    let home = tempfile::tempdir().expect("temp HOME");
    let port = free_port();

    let mut child = Command::new(PRISM)
        .args([
            "node",
            "up",
            "--offline",
            "--no-services",
            "--dashboard-port",
            &port.to_string(),
        ])
        .env("HOME", home.path())
        .env("PRISM_OFFLINE", "1")
        .env("PRISM_PYTHON", "/usr/bin/python3")
        .env_remove("MARC27_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn prism node up");
    let stdout = child.stdout.take().expect("child stdout");
    // Named binding (not `_`) so the guard lives to the end of the test.
    let _child = KillOnDrop(child);

    // Feed boot lines through a channel so a hung boot fails the deadline
    // below instead of blocking the test binary forever.
    let (tx, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in std::io::BufRead::lines(reader).map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(90);
    let mesh_line = loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("node up never printed its mesh boot line within 90s");
        match lines.recv_timeout(remaining) {
            Ok(line) if line.contains("Mesh:") => break line,
            Ok(_) => continue,
            Err(e) => panic!("boot output ended before the mesh line: {e}"),
        }
    };
    // The attribution fix rides along: --offline is the operator's own
    // instruction, not a missing login.
    assert!(
        mesh_line.contains("disabled (offline mode)"),
        "the boot line must attribute the refusal to offline, got: {mesh_line}"
    );

    let status = http_get_json(port, "/api/mesh/nodes");
    assert_eq!(
        status["online"], false,
        "the boot line says the mesh is disabled; the API must not say \
         online — got {status}"
    );
    assert_eq!(
        status["node_id"],
        serde_json::Value::Null,
        "a refused mesh advertises no node id (this is also what makes \
         `mesh sync` against this node fail honestly at its \
         publisher-lookup guard instead of printing `✓ 0 entities \
         synced`) — got {status}"
    );
}

/// 2 — a node must not pull its own dataset back as peer knowledge.
#[test]
fn mesh_sync_refuses_to_pull_from_this_node_itself() {
    let home = tempfile::tempdir().expect("temp HOME");

    let sync = |peer_url: &str, home: &Path| {
        Command::new(PRISM)
            .args(["mesh", "sync", "some-dataset", "--peer", peer_url])
            .env("HOME", home)
            .env_remove("PRISM_OFFLINE")
            .env_remove("MARC27_API_KEY")
            .stdin(Stdio::null())
            .output()
            .expect("run prism mesh sync")
    };

    let peer_reporting = |node_id: &str| {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/mesh/nodes")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(format!(
                r#"{{"online":true,"node_id":"{node_id}","peer_count":0,"peers":[]}}"#
            ))
            .create();
        server
    };

    // First run: the peer reports a FOREIGN node id. This passes the
    // self-peer guard (and mints this HOME's own persistent mesh identity
    // on the way), then fails downstream against the stub — which is fine;
    // only the second run's assertion matters.
    let foreign_peer = peer_reporting(uuid_v4_fixture());
    let first = sync(&foreign_peer.url(), home.path());
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        !first_stderr.contains("is this node itself"),
        "a foreign publisher must pass the self-peer guard: {first_stderr}"
    );

    // The identity the first run minted — the same one `node up` would use.
    let id_file = find_file(home.path(), "mesh_node_id")
        .expect("mesh sync minted this node's persistent mesh identity");
    let our_id = std::fs::read_to_string(id_file).expect("read node id");
    let our_id = our_id.trim();

    // Second run: the "peer" reports OUR OWN id — i.e. the user pointed
    // --peer at this node's own dashboard. Must refuse, by name.
    let own_peer = peer_reporting(our_id);
    let second = sync(&own_peer.url(), home.path());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        !second.status.success(),
        "self-sync must fail, got exit 0 with stderr: {stderr}"
    );
    assert!(
        stderr.contains("is this node itself"),
        "the refusal must name the self-peer condition: {stderr}"
    );
}

/// A fixed, valid, non-nil UUID that can never collide with a freshly minted
/// v4 identity file (it is constant; the minted one is random — and the
/// first run asserts the guard PASSED, so a collision would fail loudly
/// there, not silently pass).
fn uuid_v4_fixture() -> &'static str {
    "9f4c31e2-7c0a-4b7e-9d3d-2f6a5b8c1d0e"
}
