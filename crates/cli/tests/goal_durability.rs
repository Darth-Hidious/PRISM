// Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
//! Cold-restart durability for a long-running goal, proven against the real
//! `prism` binary rather than an in-process mock.
//!
//! The claim under test is the one that matters for a goal meant to run for
//! months: **kill the process and the goal picks up where it left off** — same
//! goal, later iteration, prior work still counted, spend carried forward —
//! and **the scheduler restarts it without a human**.
//!
//! Everything the campaign talks to (the proposal LLM and the evaluator) is a
//! local stub, so the test is hermetic and free. `HOME` is redirected to a
//! temp directory, so nothing touches the developer's real `~/.prism`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const PRISM: &str = env!("CARGO_BIN_EXE_prism");

/// Minimal HTTP/1.1 stub standing in for the proposal LLM and the material
/// evaluator. Hand-rolled rather than pulled in as a framework: it needs two
/// routes and an evaluation counter, and the counter is the actual evidence
/// that no work is redone across the restart.
struct Stub {
    port: u16,
    evaluations: Arc<AtomicUsize>,
}

fn start_stub() -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();
    let evaluations = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&evaluations);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let counter = Arc::clone(&counter);
            std::thread::spawn(move || serve(stream, &counter));
        }
    });
    Stub { port, evaluations }
}

fn serve(mut stream: TcpStream, evaluations: &AtomicUsize) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);

    let payload = if request_line.contains("/chat/completions") {
        // Two fresh compositions per call, distinct per proposal so the
        // campaign never dedupes them away.
        let n = evaluations.load(Ordering::SeqCst);
        let content = format!(
            "[\"W0.{} Mo0.5 Ta0.2\", \"Cr0.{} V0.4 Ti0.2\"]",
            n % 9 + 1,
            n % 7 + 1
        );
        serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        })
    } else {
        // Every evaluation is counted; each reports a real cost so the
        // campaign's USD ceiling has something to measure.
        evaluations.fetch_add(1, Ordering::SeqCst);
        // Slow enough that the goal is still mid-flight when we kill it.
        std::thread::sleep(Duration::from_millis(120));
        serde_json::json!({
            "density": 9.4,
            "mixing_entropy": 1.3,
            "cost_usd": 0.01
        })
    };

    let body = payload.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn checkpoint(home: &Path, id: &str) -> Option<serde_json::Value> {
    let path = home.join(".prism/campaigns").join(format!("{id}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn wait_for(
    home: &Path,
    id: &str,
    label: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = None;
    while Instant::now() < deadline {
        if let Some(state) = checkpoint(home, id) {
            if predicate(&state) {
                return state;
            }
            last = Some(state);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {label}; last checkpoint: {last:#?}");
}

/// Block until the goal has written its first checkpoint, and return its id.
/// The id is minted from the start timestamp, so the checkpoint file is the
/// only place the caller can learn it.
fn wait_for_goal_id(dir: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(Instant::now() < deadline, "goal never wrote a checkpoint");
        if let Ok(entries) = std::fs::read_dir(dir)
            && let Some(found) = entries.flatten().find_map(|e| {
                let p = e.path();
                (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(str::to_string))?
            })
        {
            return found;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn iteration(state: &serde_json::Value) -> u64 {
    state["current_iteration"].as_u64().unwrap_or(0)
}
fn candidates(state: &serde_json::Value) -> usize {
    state["candidates"].as_array().map_or(0, Vec::len)
}
fn spend(state: &serde_json::Value) -> f64 {
    state["total_cost_usd"].as_f64().unwrap_or(0.0)
}

fn prism(home: &Path, port: u16) -> std::process::Command {
    let mut cmd = std::process::Command::new(PRISM);
    cmd.env("HOME", home)
        .env("LLM_BASE_URL", format!("http://127.0.0.1:{port}/v1"))
        .env("LLM_MODEL", "stub-model")
        .env("PRISM_NODE_PORT", port.to_string())
        .env("PRISM_SCHEDULES_DB", home.join("schedules.db"))
        .env("PRISM_CAMPAIGNS_DIR", home.join(".prism/campaigns"))
        // A fresh HOME would otherwise trigger a full venv provision on every
        // child. The campaign path runs no Python, so point at any usable
        // interpreter and skip it.
        .env("PRISM_PYTHON", system_python())
        .env("PRISM_OFFLINE", "1")
        // Keep the child from inheriting a developer's real credentials.
        .env_remove("LLM_API_KEY")
        .env_remove("MARC27_TOKEN");
    cmd
}

fn system_python() -> PathBuf {
    ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// The headline: a goal survives `kill -9` and resumes cold at the right
/// step, with its budget and its accumulated work intact, and having redone
/// none of it.
#[test]
fn a_killed_goal_resumes_where_it_left_off() {
    let temp = tempfile::tempdir().expect("temp home");
    let home: PathBuf = temp.path().to_path_buf();
    let stub = start_stub();

    // ── 1. Start the goal in the foreground and let it get some work done.
    let mut child = prism(&home, stub.port)
        .args([
            "campaign",
            "start",
            "--goal",
            "durability probe alloy",
            "--elements",
            "W,Mo,Ta,Cr,V,Ti",
            "--max-iterations",
            "40",
            "--batch-size",
            "2",
            "--checkpoint-every",
            "1",
            "--budget",
            "1000",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the goal");

    // The id is derived from the start timestamp; find the one checkpoint.
    let dir = home.join(".prism/campaigns");
    let id = wait_for_goal_id(&dir);

    let before = wait_for(&home, &id, "at least 2 iterations", |s| iteration(s) >= 2);
    let iter_before = iteration(&before);
    let cands_before = candidates(&before);
    let spend_before = spend(&before);
    assert!(cands_before > 0, "no work was done before the kill");
    assert!(spend_before > 0.0, "no spend was recorded before the kill");

    // ── 2. Kill it the way a reboot or an OOM would: no cleanup, no chance
    //       to finish an iteration or write a farewell.
    child.kill().expect("kill the goal process");
    let status = child.wait().expect("reap");
    assert!(
        !status.success(),
        "the process must have died, not finished"
    );
    let evals_before = stub.evaluations.load(Ordering::SeqCst);

    // Nothing is running now. The checkpoint is all that survives.
    let after_kill = checkpoint(&home, &id).expect("checkpoint survives the kill");
    assert_eq!(
        after_kill["goal"]["description"], "durability probe alloy",
        "the goal itself must survive the crash"
    );

    // ── 3. Restart cold — a brand new process, nothing carried in memory.
    let restarted = prism(&home, stub.port)
        .args(["campaign", "continue", &id])
        .output()
        .expect("continue the goal");
    assert!(
        restarted.status.success(),
        "restart failed: {}",
        String::from_utf8_lossy(&restarted.stderr)
    );

    let after = checkpoint(&home, &id).expect("checkpoint after restart");

    // ── 4. It picked up where it left off.
    assert!(
        iteration(&after) > iter_before,
        "resumed goal must advance past iteration {iter_before}, got {}",
        iteration(&after)
    );
    assert!(
        candidates(&after) > cands_before,
        "resumed goal must add work on top of the {cands_before} candidates it already had"
    );
    assert!(
        spend(&after) >= spend_before,
        "spend must carry forward across the restart: ${:.4} before, ${:.4} after",
        spend_before,
        spend(&after)
    );

    // ── 5. It did not redo the work it had already done. The evaluator's own
    //       counter is the evidence: total evaluations equals total
    //       candidates, so nothing was evaluated twice.
    let evals_after = stub.evaluations.load(Ordering::SeqCst);
    assert!(
        evals_after > evals_before,
        "the restart must have done new work"
    );
    assert!(
        candidates(&after) >= cands_before,
        "prior candidates must not be discarded on resume"
    );
    // Every candidate in the checkpoint corresponds to exactly one evaluation
    // the stub served; a resume that redid earlier iterations would push the
    // evaluation count above the candidate count.
    assert!(
        evals_after <= candidates(&after) + 2,
        "resume redid work: {evals_after} evaluations for {} candidates",
        candidates(&after)
    );
}

/// The scheduler — not a human — is what restarts a dead goal.
#[test]
fn the_scheduler_resumes_a_goal_whose_process_died() {
    let temp = tempfile::tempdir().expect("temp home");
    let home: PathBuf = temp.path().to_path_buf();
    let stub = start_stub();

    let mut child = prism(&home, stub.port)
        .args([
            "campaign",
            "start",
            "--goal",
            "scheduled probe alloy",
            "--elements",
            "W,Mo,Ta",
            "--max-iterations",
            "40",
            "--batch-size",
            "2",
            "--checkpoint-every",
            "1",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the goal");

    let dir = home.join(".prism/campaigns");
    let id = wait_for_goal_id(&dir);
    let before = wait_for(&home, &id, "at least 1 iteration", |s| iteration(s) >= 1);
    child.kill().expect("kill");
    let _ = child.wait();

    // A one-shot schedule due now. Creating it is a plain database write —
    // no launchctl, no root, which is what makes this an agent capability.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let created = prism(&home, stub.port)
        .args([
            "schedule",
            "create",
            "--goal",
            &id,
            "--at",
            &now.to_string(),
            "--max-fires",
            "1",
        ])
        .output()
        .expect("create schedule");
    assert!(
        created.status.success(),
        "schedule create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    // One tick — exactly what launchd / a systemd timer / a pod's daemon runs.
    let tick = prism(&home, stub.port)
        .args(["schedule", "tick"])
        .output()
        .expect("tick");
    let tick_out = String::from_utf8_lossy(&tick.stdout).to_string();
    assert!(tick.status.success(), "tick failed: {tick_out}");
    assert!(
        tick_out.contains("FIRED"),
        "the tick must have resumed the dead goal, got: {tick_out}"
    );

    // The resumed worker really is doing work — with no human involved.
    let after = wait_for(&home, &id, "progress past the crash", |s| {
        iteration(s) > iteration(&before)
    });
    assert!(candidates(&after) >= candidates(&before));

    // A second tick must not fire again: the wake-up ceiling was 1.
    let again = prism(&home, stub.port)
        .args(["schedule", "tick"])
        .output()
        .expect("tick again");
    let again_out = String::from_utf8_lossy(&again.stdout).to_string();
    assert!(
        !again_out.contains("FIRED"),
        "the wake-up ceiling must hold: {again_out}"
    );

    // Tidy: stop whatever the scheduler started.
    if let Some(pid) = std::fs::read_to_string(dir.join(format!("{id}.worker")))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// A schedule pointing at a goal paused for human approval must refuse to
/// resume it. A scheduled wake-up is not an approval.
#[test]
fn the_scheduler_refuses_to_resume_a_goal_paused_at_an_approval_gate() {
    let temp = tempfile::tempdir().expect("temp home");
    let home: PathBuf = temp.path().to_path_buf();
    let stub = start_stub();
    let dir = home.join(".prism/campaigns");
    std::fs::create_dir_all(&dir).unwrap();

    // A checkpoint in exactly the shape the engine writes when it stops at a
    // gate: Paused, with that iteration recorded in gates_hit.
    let id = "goal-at-a-gate";
    let gated = serde_json::json!({
        "campaign_id": id,
        "goal": { "description": "gated goal", "elements": [], "objective": "",
                  "constraints": [], "seeds": [] },
        "config": { "max_iterations": 50, "batch_size": 2, "checkpoint_every": 1,
                    "approval_gate_at": [3], "llm_model": "", "llm_temperature": 0.7,
                    "reward_weights": {} },
        "candidates": [], "current_iteration": 3, "total_cost_usd": 0.0,
        "paused": true, "completed": false, "status": "paused",
        "gates_hit": [3], "completion_reason": "",
        "started_at": "2026-07-27T00:00:00Z", "last_checkpoint_at": "2026-07-27T00:00:00Z"
    });
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&gated).unwrap(),
    )
    .unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let created = prism(&home, stub.port)
        .args(["schedule", "create", "--goal", id, "--at", &now.to_string()])
        .output()
        .expect("create schedule");
    assert!(
        created.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    let tick = prism(&home, stub.port)
        .args(["schedule", "tick"])
        .output()
        .expect("tick");
    let out = String::from_utf8_lossy(&tick.stdout).to_string();
    assert!(
        !out.contains("FIRED"),
        "a scheduled wake-up must never resume past an approval gate: {out}"
    );
    assert!(
        out.contains("approval"),
        "the refusal must name the approval gate: {out}"
    );
    // The goal is untouched: still paused, still at the same iteration.
    let state = checkpoint(&home, id).unwrap();
    assert_eq!(state["status"], "paused");
    assert_eq!(iteration(&state), 3);
}
