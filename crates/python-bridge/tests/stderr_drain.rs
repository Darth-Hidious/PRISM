//! Regression: `ToolServer` must drain the child's stderr.
//!
//! `tool_server.rs` pipes child stderr (so it can never reach the
//! operator terminal or corrupt the stdout JSON-line protocol). The
//! Python side deliberately routes ALL non-protocol output to stderr
//! (heavy import banners, library warnings, tool logs). A piped stream
//! that is never read fills the ~64 KB OS pipe buffer and then BLOCKS
//! the child on its next stderr write — hanging the whole tool layer.
//!
//! This test points `python_bin` at a fake that floods ~300 KB to
//! stderr BEFORE it reads the request. Without the drain task the child
//! blocks at ~64 KB and `list_tools()` never returns (the timeout below
//! fires). With the drain it completes.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use prism_python_bridge::tool_server::ToolServer;

#[tokio::test]
async fn tool_server_stderr_is_drained_no_deadlock() {
    let dir = std::env::temp_dir().join(format!("prism_ts_drain_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("fakepy.sh");
    // Ignores its `-m app.tool_server` args. Floods stderr past the pipe
    // buffer, THEN consumes the request line and emits one JSON line.
    fs::write(
        &fake,
        "#!/bin/sh\n\
         yes 'noise-noise-noise-noise-noise-noise-noise-noise' 2>/dev/null \
           | head -c 300000 1>&2\n\
         head -n 1 >/dev/null\n\
         printf '{\"tools\": []}\\n'\n",
    )
    .unwrap();
    let mut perm = fs::metadata(&fake).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&fake, perm).unwrap();

    let ts = ToolServer {
        python_bin: fake,
        project_root: dir.clone(),
        env: BTreeMap::new(),
    };
    let mut handle = ts.spawn().await.expect("spawn");

    let resp = tokio::time::timeout(Duration::from_secs(20), handle.list_tools())
        .await
        .expect("DEADLOCK: child stderr not drained — pipe buffer filled and the child blocked before reading the request")
        .expect("list_tools call failed");

    assert_eq!(resp, serde_json::json!({ "tools": [] }));
    let _ = fs::remove_dir_all(&dir);
}
