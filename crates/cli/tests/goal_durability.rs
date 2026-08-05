// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
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

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PRISM: &str = env!("CARGO_BIN_EXE_prism");
const CAMPAIGN_ENTRYPOINT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/campaign-entrypoint.sh");
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct HomeGuard(Option<OsString>);

impl HomeGuard {
    fn set(home: &Path) -> Self {
        let previous = std::env::var_os("HOME");
        // SAFETY: identity setup is serialized by ENV_LOCK and restores HOME
        // before any child process is launched.
        unsafe { std::env::set_var("HOME", home) };
        Self(previous)
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: the caller still holds ENV_LOCK.
        unsafe {
            match &self.0 {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

fn install_test_identity(home: &Path) {
    let _env_lock = ENV_LOCK.lock().unwrap();
    let _home = HomeGuard::set(home);
    let paths = prism_runtime::PrismPaths::discover().unwrap();
    paths
        .save_cli_state(&prism_runtime::PrismCliState {
            credentials: Some(prism_runtime::StoredCredentials {
                user_id: Some("goal-durability-test-user".into()),
                display_name: Some("Goal Durability Test User".into()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
}

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
    let mut authorization = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_string());
            }
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);

    let (status, payload) = if request_line.contains("/chat/completions") {
        // Two fresh compositions per call, distinct per proposal so the
        // campaign never dedupes them away. Fractions must sum to 1.0 within
        // COMPOSITION_SUM_TOLERANCE or the candidate is rejected outright, so
        // vary a balanced *pair* — moving one fraction alone breaks the sum.
        let n = evaluations.load(Ordering::SeqCst);
        let w = 100 + n % 600; // 0.100 ..= 0.699, paired against 0.800 - w
        let cr = 100 + (n * 7) % 600;
        let content = format!(
            "[\"W{:.3} Mo{:.3} Ta0.2\", \"Cr{:.3} V{:.3} Ti0.2\"]",
            w as f64 / 1000.0,
            (800 - w) as f64 / 1000.0,
            cr as f64 / 1000.0,
            (800 - cr) as f64 / 1000.0
        );
        (
            "200 OK",
            serde_json::json!({
                "choices": [{ "message": { "role": "assistant", "content": content } }]
            }),
        )
    } else if request_line.contains("/api/sessions") {
        (
            "200 OK",
            serde_json::json!({"session_id": "goal-durability-node-session"}),
        )
    } else if request_line.contains("/api/tools/hea_descriptors/run")
        && authorization.as_deref() == Some("Bearer goal-durability-node-session")
    {
        // Every authenticated evaluation is counted; each reports a real cost
        // so the campaign's USD ceiling has something to measure.
        evaluations.fetch_add(1, Ordering::SeqCst);
        // Slow enough that the goal is still mid-flight when we kill it.
        std::thread::sleep(Duration::from_millis(120));
        (
            "200 OK",
            serde_json::json!({
                "density": 9.4,
                "mixing_entropy": 1.3,
                "cost_usd": 0.01
            }),
        )
    } else if request_line.contains("/api/tools/hea_descriptors/run") {
        (
            "401 Unauthorized",
            serde_json::json!({"error": "missing or invalid session token"}),
        )
    } else {
        ("404 Not Found", serde_json::json!({"error": "not found"}))
    };

    let body = payload.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

fn configure_prism_command(
    mut cmd: std::process::Command,
    home: &Path,
    port: u16,
) -> std::process::Command {
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

fn prism(home: &Path, port: u16) -> std::process::Command {
    configure_prism_command(std::process::Command::new(PRISM), home, port)
}

fn slurm_campaign(
    home: &Path,
    port: u16,
    task_id: &str,
    goal: &str,
    max_iterations: usize,
) -> std::process::Command {
    let inputs = serde_json::json!({
        "goal": goal,
        "elements": ["W", "Mo", "Ta", "Cr", "V", "Ti"],
        "objective": "maximize mixing entropy",
        "max_iterations": max_iterations,
        "batch_size": 2,
        "budget": 1000.0,
        "checkpoint_every": 1
    });
    let mut cmd =
        configure_prism_command(std::process::Command::new(CAMPAIGN_ENTRYPOINT), home, port);
    cmd.env("PRISM_BIN", PRISM)
        .env("PRISM_TASK_ID", task_id)
        .env("PRISM_INPUTS", inputs.to_string())
        .env("PRISM_CAMPAIGNS_DIR", home.join(".prism/campaigns"))
        .args(["campaign", "batch-entrypoint"]);
    cmd
}

fn wait_for_child_checkpoint(
    child: &mut std::process::Child,
    home: &Path,
    id: &str,
    label: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = None;
    while Instant::now() < deadline {
        if let Some(state) = checkpoint(home, id) {
            if predicate(&state) {
                return state;
            }
            last = Some(state);
        }
        if let Some(status) = child.try_wait().expect("poll batch campaign") {
            panic!("batch campaign exited before {label}: {status}; last checkpoint: {last:#?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {label}; last checkpoint: {last:#?}");
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
    install_test_identity(&home);
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

/// Simulate SLURM's signal/relaunch cycle against the real campaign worker.
#[cfg(unix)]
#[test]
fn slurm_signal_checkpoint_requeue_resumes_without_repeating_completed_work() {
    let temp = tempfile::tempdir().expect("temp home");
    let home: PathBuf = temp.path().to_path_buf();
    install_test_identity(&home);
    let stub = start_stub();
    let task_id = "scheduler-job_7";

    let mut child = slurm_campaign(&home, stub.port, task_id, "signal recovery alloy", 12)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start batch campaign");
    let before = wait_for_child_checkpoint(
        &mut child,
        &home,
        task_id,
        "two completed iterations",
        |state| iteration(state) >= 2,
    );
    let completed_before = before["candidates"]
        .as_array()
        .expect("candidate array")
        .clone();
    assert!(!completed_before.is_empty(), "campaign did no work");

    // Model SLURM's pre-walltime warning. The process must persist via the
    // production Campaign::checkpoint path before requesting requeue.
    let signal_result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGUSR1) };
    assert_eq!(signal_result, 0, "failed to send SIGUSR1");
    let status = child.wait().expect("reap signalled campaign");
    assert_eq!(
        status.code(),
        Some(140),
        "checkpointed SIGUSR1 exit must request requeue"
    );

    let at_requeue = checkpoint(&home, task_id).expect("signal checkpoint");
    assert!(iteration(&at_requeue) >= iteration(&before));
    assert!(spend(&at_requeue) >= spend(&before));
    let evals_before_restart = stub.evaluations.load(Ordering::SeqCst);

    // Model SLURM requeue: a fresh process receives the same task identity.
    let restarted = slurm_campaign(&home, stub.port, task_id, "ignored on resume", 12)
        .output()
        .expect("restart batch campaign");
    assert!(
        restarted.status.success(),
        "requeued campaign failed: {}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let after = checkpoint(&home, task_id).expect("checkpoint after requeue");
    assert_eq!(after["status"], "completed");
    assert_eq!(after["goal"]["description"], "signal recovery alloy");
    assert!(iteration(&after) > iteration(&at_requeue));
    assert!(spend(&after) >= spend(&at_requeue));
    for prior in &completed_before {
        assert!(
            after["candidates"]
                .as_array()
                .expect("candidate array after resume")
                .contains(prior),
            "completed candidate disappeared across requeue: {prior}"
        );
    }

    let evals_after = stub.evaluations.load(Ordering::SeqCst);
    assert!(evals_after > evals_before_restart, "resume did no new work");
    assert!(
        evals_after <= candidates(&after) + 1,
        "resume repeated completed work: {evals_after} evaluations for {} durable candidates",
        candidates(&after)
    );
}

/// Array identities address separate checkpoint files even when tasks overlap.
#[cfg(unix)]
#[test]
fn slurm_array_task_ids_do_not_clobber_campaign_checkpoints() {
    let temp = tempfile::tempdir().expect("temp home");
    let home: PathBuf = temp.path().to_path_buf();
    install_test_identity(&home);
    let stub = start_stub();

    let first = slurm_campaign(&home, stub.port, "scheduler-job_3", "array alloy alpha", 2)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("start first array task");
    let second = slurm_campaign(&home, stub.port, "scheduler-job_4", "array alloy beta", 2)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("start second array task");
    let first_status = first.wait_with_output().expect("wait first array task");
    let second_status = second.wait_with_output().expect("wait second array task");
    assert!(first_status.status.success());
    assert!(second_status.status.success());

    let first_state = checkpoint(&home, "scheduler-job_3").expect("first task checkpoint");
    let second_state = checkpoint(&home, "scheduler-job_4").expect("second task checkpoint");
    assert_eq!(first_state["campaign_id"], "scheduler-job_3");
    assert_eq!(second_state["campaign_id"], "scheduler-job_4");
    assert_eq!(first_state["goal"]["description"], "array alloy alpha");
    assert_eq!(second_state["goal"]["description"], "array alloy beta");
    assert_eq!(first_state["status"], "completed");
    assert_eq!(second_state["status"], "completed");
}

/// The scheduler — not a human — is what restarts a dead goal.
#[test]
fn the_scheduler_resumes_a_goal_whose_process_died() {
    let temp = tempfile::tempdir().expect("temp home");
    let home: PathBuf = temp.path().to_path_buf();
    install_test_identity(&home);
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
