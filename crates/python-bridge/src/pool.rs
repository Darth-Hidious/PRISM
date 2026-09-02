// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! Bounded pool of Python tool-server children — the multi-lane substrate the
//! agent orchestrator runs on.
//!
//! # Why a pool of children, not request-id multiplexing over one pipe
//!
//! Two designs can give N agents concurrent tool execution:
//!
//! 1. **One child, request-id multiplexed protocol.** Requires changing the
//!    wire protocol (ids on both sides) AND rewriting `app.tool_server` from
//!    its sequential `for line in stdin` loop into an async/threaded server —
//!    and even then the GIL serializes the CPU-bound scientific tools that
//!    dominate PRISM workloads, so "concurrency" would be mostly cosmetic.
//!    One wedged tool would also still block or complicate every other
//!    caller's response ordering.
//! 2. **A bounded pool of children.** Zero Python-side changes (the protocol
//!    stays one request line → one response line per child), real OS-level
//!    parallelism (each child owns its interpreter and GIL), and response
//!    attribution is structural: a lane is exclusively checked out for the
//!    duration of a call, so a response physically cannot reach any caller
//!    but the one holding that lane's pipe. Multi-child is already proven in
//!    production — `ChatService` runs two children today (the main and the
//!    `local_only` tool server); this generalizes the count from "exactly
//!    two" to "a declared bound".
//!
//! We take (2). The cost is memory per child, which is why the bound is a
//! declared policy ([`ToolServerPoolPolicy`]) and children spawn lazily.
//!
//! # Isolation
//!
//! A pool is flavored at construction ([`LaneEnvironment`]): a `Clean` pool
//! only ever spawns children through
//! [`ToolServer::spawn_with_clean_environment`] (the LocalOnly credential
//! boundary), an `Inherited` pool only through [`ToolServer::spawn`]. There
//! is no per-call flavor switch, so a LocalOnly call routed to a `Clean` pool
//! can never land on a normal-environment child — the type of the pool it was
//! handed decides, not runtime data.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::PythonBridgeError;
use crate::tool_server::{ToolServer, ToolServerHandle};

/// Resource policy for a [`ToolServerPool`].
///
/// Mirrors the shape of `ParallelExecutionPolicy` in `prism-workflows`: the
/// bound is a claim about the execution environment, declared where it can be
/// seen and overridden, and typed so zero is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolServerPoolPolicy {
    /// Pool size: the maximum number of tool-server children that may exist
    /// (and therefore the maximum number of concurrently executing tool
    /// calls) at once. `NonZeroUsize` because a zero-lane pool would block
    /// every acquire on a permit that can never exist — a deadlock by
    /// construction.
    ///
    /// Default **4**, chosen from per-child cost: each lane is a full
    /// scientific-Python interpreter (`python3 -m app.tool_server` imports
    /// the complete tool registry at startup — `build_full_registry` — and
    /// individual tools pull in heavy ML/scientific stacks such as MACE,
    /// numpy/scipy and pymatgen, taking a child from tens of MB at spawn to
    /// several hundred MB resident once warm). Four lanes keep the
    /// worst-case pool footprint around 1–1.5 GB — acceptable on the 16 GB
    /// development target — while covering the deepest legal subagent chain
    /// (`MAX_SUBAGENT_DEPTH` = 2 ⇒ at most 2 simultaneous subagent lanes)
    /// with headroom for sibling fan-out. Children spawn lazily, so an idle
    /// pool costs nothing; the bound is a ceiling, not a pre-allocation.
    pub max_lanes: NonZeroUsize,

    /// How long an acquire may queue for a lane before failing with
    /// [`PythonBridgeError::PoolExhausted`]. Queueing briefly is normal (a
    /// full pool is the bound doing its job); waiting forever would turn a
    /// leaked or wedged lane into a silent hang. Default **90s**: one full
    /// per-call ceiling an operator might set (60 s was the old default)
    /// plus margin, so an acquire queued behind one worst-case call still
    /// succeeds, and anything slower surfaces as the named, actionable error
    /// instead of a stall.
    pub acquire_timeout: Duration,
}

impl Default for ToolServerPoolPolicy {
    fn default() -> Self {
        Self {
            max_lanes: NonZeroUsize::new(4).expect("the default lane count is non-zero"),
            acquire_timeout: Duration::from_secs(90),
        }
    }
}

/// Which environment a pool's children are spawned with. Decided once, at
/// pool construction — never per call — so environment isolation is carried
/// by which pool a caller was handed, not by runtime data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEnvironment {
    /// [`ToolServer::spawn`]: children inherit the parent process environment
    /// plus the configured overrides. For verified node-owner surfaces.
    Inherited,
    /// [`ToolServer::spawn_with_clean_environment`]: children start from an
    /// EMPTY environment plus the configured allowlist — the LocalOnly
    /// credential boundary. A credential added to the parent process later
    /// can never leak into these children.
    Clean,
}

struct PoolInner {
    config: ToolServer,
    environment: LaneEnvironment,
    policy: ToolServerPoolPolicy,
    /// One permit per lane. Removing this bound is what the
    /// `concurrency_never_exceeds_the_pool_bound` test exists to catch.
    limiter: Arc<Semaphore>,
    /// Healthy children awaiting reuse. Desynchronized children are never
    /// pushed here (see [`ToolServerLease`]'s `Drop`).
    idle: std::sync::Mutex<Vec<ToolServerHandle>>,
    /// Lanes currently checked out.
    in_flight: AtomicUsize,
    /// High-water mark of `in_flight` — instrumentation for the bound test
    /// and for operators sizing `max_lanes`.
    peak_in_flight: AtomicUsize,
    /// Children spawned over the pool's lifetime. Grows past `max_lanes`
    /// only when broken children were discarded and replaced.
    spawned_children: AtomicUsize,
}

/// Bounded, lazily-populated pool of tool-server children. Cheap to clone
/// (all clones share the same lanes).
#[derive(Clone)]
pub struct ToolServerPool {
    inner: Arc<PoolInner>,
}

impl ToolServerPool {
    /// Create an empty pool. No child is spawned until the first
    /// [`Self::acquire`] — an unused pool costs nothing.
    #[must_use]
    pub fn new(
        config: ToolServer,
        environment: LaneEnvironment,
        policy: ToolServerPoolPolicy,
    ) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                config,
                environment,
                policy,
                limiter: Arc::new(Semaphore::new(policy.max_lanes.get())),
                idle: std::sync::Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                peak_in_flight: AtomicUsize::new(0),
                spawned_children: AtomicUsize::new(0),
            }),
        }
    }

    /// Check out an exclusive lane, reusing an idle child or spawning a new
    /// one (bounded by [`ToolServerPoolPolicy::max_lanes`]).
    ///
    /// Fails honestly, never hangs:
    /// - every lane busy for the whole acquire deadline →
    ///   [`PythonBridgeError::PoolExhausted`] naming the bound and the wait;
    /// - replacement child would not start → [`PythonBridgeError::Spawn`].
    pub async fn acquire(&self) -> Result<ToolServerLease, PythonBridgeError> {
        let policy = self.inner.policy;
        let permit = tokio::time::timeout(
            policy.acquire_timeout,
            Arc::clone(&self.inner.limiter).acquire_owned(),
        )
        .await
        .map_err(|_| PythonBridgeError::PoolExhausted {
            lanes: policy.max_lanes.get(),
            waited: policy.acquire_timeout,
        })?
        .expect("the pool semaphore is never closed");

        let reused = self
            .inner
            .idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        let handle = match reused {
            Some(handle) => handle,
            None => {
                // Spawn failure: `permit` drops here, releasing the lane.
                let handle = match self.inner.environment {
                    LaneEnvironment::Inherited => self.inner.config.spawn().await?,
                    LaneEnvironment::Clean => {
                        self.inner.config.spawn_with_clean_environment().await?
                    }
                };
                self.inner.spawned_children.fetch_add(1, Ordering::Relaxed);
                handle
            }
        };

        let now = self.inner.in_flight.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner.peak_in_flight.fetch_max(now, Ordering::Relaxed);

        Ok(ToolServerLease {
            handle: Some(handle),
            pool: Arc::clone(&self.inner),
            _permit: permit,
        })
    }

    /// Lanes currently checked out.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::Relaxed)
    }

    /// High-water mark of concurrently checked-out lanes.
    #[must_use]
    pub fn peak_in_flight(&self) -> usize {
        self.inner.peak_in_flight.load(Ordering::Relaxed)
    }

    /// Children spawned over the pool's lifetime (> lanes ever in flight
    /// means broken children were discarded and replaced).
    #[must_use]
    pub fn spawned_children(&self) -> usize {
        self.inner.spawned_children.load(Ordering::Relaxed)
    }

    /// The declared policy this pool enforces.
    #[must_use]
    pub fn policy(&self) -> ToolServerPoolPolicy {
        self.inner.policy
    }
}

/// Exclusive checkout of one pool lane. Derefs to [`ToolServerHandle`], so a
/// lease is usable anywhere a `&mut ToolServerHandle` is expected.
///
/// On drop the child is triaged, not blindly returned:
/// - healthy → back to the idle set for reuse;
/// - desynchronized ([`ToolServerHandle::is_desynchronized`] — it timed out,
///   died, or broke framing mid-call) → dropped, which kills the process
///   (`kill_on_drop`); the NEXT acquire spawns a fresh replacement. A child
///   whose pipe may still carry a previous caller's response must never be
///   handed to another caller.
pub struct ToolServerLease {
    /// `Option` only so `Drop` can move the handle out; invariantly `Some`
    /// while the lease is live.
    handle: Option<ToolServerHandle>,
    pool: Arc<PoolInner>,
    /// Declared last: released after `Drop` has returned the child, so a
    /// waiter that wins this permit always finds the idle child already
    /// available.
    _permit: OwnedSemaphorePermit,
}

impl std::ops::Deref for ToolServerLease {
    type Target = ToolServerHandle;
    fn deref(&self) -> &ToolServerHandle {
        self.handle
            .as_ref()
            .expect("lease holds a handle until drop")
    }
}

impl std::ops::DerefMut for ToolServerLease {
    fn deref_mut(&mut self) -> &mut ToolServerHandle {
        self.handle
            .as_mut()
            .expect("lease holds a handle until drop")
    }
}

impl Drop for ToolServerLease {
    fn drop(&mut self) {
        let handle = self.handle.take().expect("lease drops exactly once");
        self.pool.in_flight.fetch_sub(1, Ordering::Relaxed);
        if handle.is_desynchronized() {
            tracing::warn!(
                "discarding desynchronized tool-server lane; the next acquire spawns a fresh child"
            );
            drop(handle); // kill_on_drop reaps the process
        } else {
            self.pool
                .idle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(handle);
        }
        // `_permit` drops after this body — the lane only becomes acquirable
        // once the idle set already reflects the returned (or discarded)
        // child.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// Test tool server: `call_tool` sleeps `args.delay_ms`, then echoes
    /// `args.token` plus its own pid and two environment probes. The `die`
    /// tool exits without replying. `set_session_id` answers like the real
    /// server.
    const POOL_TEST_SERVER_PY: &str = r#"
import json, os, sys, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "set_session_id":
        resp = {"status": "ok", "session_id": req.get("session_id")}
    elif method == "call_tool":
        args = req.get("args") or {}
        if req.get("tool") == "die":
            os._exit(1)
        time.sleep((args.get("delay_ms") or 0) / 1000.0)
        resp = {"result": {
            "token": args.get("token"),
            "pid": os.getpid(),
            "offline": os.environ.get("PRISM_OFFLINE"),
            "home": os.environ.get("HOME"),
        }}
    else:
        resp = {"error": "unknown method"}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

    fn python_executable() -> Option<PathBuf> {
        let output = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string()))
    }

    fn write_test_project(dir: &Path) {
        let app = dir.join("app");
        std::fs::create_dir_all(&app).expect("create app package");
        std::fs::write(app.join("__init__.py"), "").expect("write package marker");
        std::fs::write(app.join("tool_server.py"), POOL_TEST_SERVER_PY).expect("write worker");
    }

    fn test_config(project_root: &Path, env: BTreeMap<String, String>) -> ToolServer {
        ToolServer {
            python_bin: python_executable().expect("python3 on PATH"),
            project_root: project_root.to_path_buf(),
            env,
        }
    }

    fn policy(lanes: usize) -> ToolServerPoolPolicy {
        ToolServerPoolPolicy {
            max_lanes: NonZeroUsize::new(lanes).expect("test lane count is non-zero"),
            ..ToolServerPoolPolicy::default()
        }
    }

    fn probe_args(token: &str, delay_ms: u64) -> serde_json::Value {
        serde_json::json!({ "token": token, "delay_ms": delay_ms })
    }

    /// THE pool invariant: with interleaved requests of scrambled durations,
    /// every caller gets exactly the response to ITS request. In a provenance
    /// system a crossed response is data corruption, not a glitch.
    #[tokio::test(flavor = "multi_thread")]
    async fn interleaved_responses_are_never_mismatched() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(3),
        );

        let mut tasks = Vec::new();
        for i in 0..12u64 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                let token = format!("caller-{i}");
                // Scrambled, non-monotonic delays force responses to complete
                // out of submission order across the pool.
                let delay_ms = (i * 37) % 150;
                let mut lane = pool.acquire().await.expect("acquire lane");
                let response = lane
                    .call_tool("echo", probe_args(&token, delay_ms))
                    .await
                    .expect("pooled call succeeds");
                (token, response)
            }));
        }
        for task in tasks {
            let (token, response) = task.await.expect("task joins");
            assert_eq!(
                response["result"]["token"], token,
                "a response was delivered to the wrong caller: {response}"
            );
        }
    }

    /// The bound is real: 8 queued callers against 2 lanes never exceed 2
    /// concurrent checkouts. Delete the semaphore and the peak reads 8 —
    /// this test is the tripwire for that.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrency_never_exceeds_the_pool_bound() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(2),
        );

        let mut tasks = Vec::new();
        for i in 0..8u64 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                let mut lane = pool.acquire().await.expect("acquire lane");
                lane.call_tool("echo", probe_args(&format!("c{i}"), 150))
                    .await
                    .expect("pooled call succeeds")
            }));
        }
        for task in tasks {
            task.await.expect("task joins");
        }
        assert_eq!(
            pool.peak_in_flight(),
            2,
            "8 queued callers must saturate — and never exceed — the 2-lane bound"
        );
        assert!(
            pool.spawned_children() <= 2,
            "children must never exceed the lane bound: {}",
            pool.spawned_children()
        );
    }

    /// The point of the pool: N concurrent calls finish in materially less
    /// wall-clock than N serialized ones. Ratio-asserted (not raw timing) so
    /// machine speed does not decide the outcome.
    #[tokio::test(flavor = "multi_thread")]
    async fn pooled_calls_beat_serialized_calls_materially() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        const N: u64 = 4;
        const DELAY_MS: u64 = 400;

        // Serialized baseline: one lane, N sequential calls — the exact
        // one-child-at-a-time model the single-handle path has today.
        let serial_pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(1),
        );
        let serial_started = std::time::Instant::now();
        {
            let mut lane = serial_pool.acquire().await.expect("acquire serial lane");
            for i in 0..N {
                lane.call_tool("echo", probe_args(&format!("s{i}"), DELAY_MS))
                    .await
                    .expect("serial call succeeds");
            }
        }
        let serial_elapsed = serial_started.elapsed();

        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(N as usize),
        );
        let concurrent_started = std::time::Instant::now();
        let mut tasks = Vec::new();
        for i in 0..N {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                let mut lane = pool.acquire().await.expect("acquire lane");
                lane.call_tool("echo", probe_args(&format!("p{i}"), DELAY_MS))
                    .await
                    .expect("pooled call succeeds")
            }));
        }
        for task in tasks {
            task.await.expect("task joins");
        }
        let concurrent_elapsed = concurrent_started.elapsed();

        eprintln!(
            "pool speedup: serialized {serial_elapsed:?} vs pooled {concurrent_elapsed:?} \
             ({N} x {DELAY_MS}ms calls)"
        );
        assert!(
            concurrent_elapsed < serial_elapsed.mul_f64(0.7),
            "pooled ({concurrent_elapsed:?}) must be materially faster than \
             serialized ({serial_elapsed:?}) for {N} x {DELAY_MS}ms calls"
        );
    }

    /// The stale-response hazard: after a call times out, that child's pipe
    /// still owes the timed-out response. Reusing the child would deliver it
    /// to the NEXT caller. The lease must discard the child so the next
    /// caller gets a fresh one — and its own response.
    #[tokio::test(flavor = "multi_thread")]
    async fn timed_out_lane_is_discarded_not_reused() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(1),
        );

        // Caller A: the child will answer in 1500ms, but A only waits 200ms.
        {
            let mut lane = pool.acquire().await.expect("acquire lane");
            let request = serde_json::json!({
                "method": "call_tool", "tool": "echo",
                "args": probe_args("stale-A", 1500),
            });
            let err = lane
                .call_with_timeout(&request, Some(Duration::from_millis(200)))
                .await
                .expect_err("the slow call must time out");
            assert!(
                matches!(err, PythonBridgeError::Timeout(_)),
                "expected a timeout, got: {err}"
            );
        } // lease drop: desynchronized child discarded

        // Caller B on the same 1-lane pool: must get a FRESH child and ITS
        // OWN response — never A's late "stale-A" line.
        let mut lane = pool.acquire().await.expect("acquire replacement lane");
        let response = lane
            .call_tool("echo", probe_args("fresh-B", 0))
            .await
            .expect("replacement lane answers");
        assert_eq!(
            response["result"]["token"], "fresh-B",
            "caller B received a response that belongs to caller A: {response}"
        );
        assert_eq!(
            pool.spawned_children(),
            2,
            "the timed-out child must have been replaced, not reused"
        );
    }

    /// A child that dies mid-call yields the specific worker-exited error —
    /// promptly, with no hang — and the pool recovers with a fresh child.
    #[tokio::test(flavor = "multi_thread")]
    async fn killed_child_yields_a_specific_error_then_the_pool_recovers() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(1),
        );

        {
            let mut lane = pool.acquire().await.expect("acquire lane");
            let started = std::time::Instant::now();
            let err = lane
                .call_tool("die", serde_json::json!({}))
                .await
                .expect_err("a dead child cannot answer");
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "a dead child must fail promptly, not hang"
            );
            assert!(
                err.to_string().contains("exited"),
                "the error must name the child's death: {err}"
            );
        }

        let mut lane = pool.acquire().await.expect("acquire replacement lane");
        let response = lane
            .call_tool("echo", probe_args("after-death", 0))
            .await
            .expect("replacement lane answers");
        assert_eq!(response["result"]["token"], "after-death");
        assert_eq!(pool.spawned_children(), 2, "the dead child was replaced");
    }

    /// LocalOnly isolation under concurrency: every child a `Clean` pool ever
    /// vends carries the scrubbed environment (allowlist only, no inherited
    /// HOME), no matter how many lanes run at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn clean_environment_pool_scrubs_every_child() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(
                project.path(),
                BTreeMap::from([("PRISM_OFFLINE".to_string(), "1".to_string())]),
            ),
            LaneEnvironment::Clean,
            policy(3),
        );

        let mut tasks = Vec::new();
        for i in 0..6u64 {
            let pool = pool.clone();
            tasks.push(tokio::spawn(async move {
                let mut lane = pool.acquire().await.expect("acquire lane");
                lane.call_tool("echo", probe_args(&format!("env-{i}"), 50))
                    .await
                    .expect("pooled call succeeds")
            }));
        }
        for task in tasks {
            let response = task.await.expect("task joins");
            assert_eq!(
                response["result"]["offline"], "1",
                "allowlisted var must reach every child: {response}"
            );
            assert!(
                response["result"]["home"].is_null(),
                "no child of a Clean pool may inherit the parent environment: {response}"
            );
        }
    }

    /// The serialized path is unchanged: one sequential caller reuses one
    /// child for every call — the same single-child, one-call-at-a-time
    /// traffic a bare `ToolServerHandle` produces today.
    #[tokio::test(flavor = "multi_thread")]
    async fn sequential_single_caller_reuses_one_child() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            policy(4),
        );

        let mut pids = Vec::new();
        for i in 0..5u64 {
            let mut lane = pool.acquire().await.expect("acquire lane");
            let response = lane
                .call_tool("echo", probe_args(&format!("seq-{i}"), 0))
                .await
                .expect("sequential call succeeds");
            pids.push(response["result"]["pid"].as_u64().expect("pid"));
        }
        assert!(
            pids.windows(2).all(|w| w[0] == w[1]),
            "a sequential caller must keep hitting the same child: {pids:?}"
        );
        assert_eq!(
            pool.spawned_children(),
            1,
            "no speculative children for a serialized caller"
        );
    }

    /// Exhaustion is a named, actionable error — never an indefinite wait.
    #[tokio::test(flavor = "multi_thread")]
    async fn exhausted_pool_names_the_bound_instead_of_hanging() {
        if python_executable().is_none() {
            eprintln!("SKIP: python3 not on PATH");
            return;
        }
        let project = tempfile::tempdir().expect("temp project");
        write_test_project(project.path());
        let pool = ToolServerPool::new(
            test_config(project.path(), BTreeMap::new()),
            LaneEnvironment::Inherited,
            ToolServerPoolPolicy {
                max_lanes: NonZeroUsize::new(1).expect("non-zero"),
                acquire_timeout: Duration::from_millis(100),
            },
        );

        let _held = pool.acquire().await.expect("hold the only lane");
        let err = match pool.acquire().await {
            Err(err) => err,
            Ok(_) => panic!("the second acquire must fail, not queue forever"),
        };
        let message = err.to_string();
        assert!(
            message.contains("exhausted") && message.contains('1'),
            "the error must name the constraint and the bound: {message}"
        );
    }
}
