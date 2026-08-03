## Goal

Complete the `prism run` LOCAL/BYOC compute-job lifecycle: poll to a terminal state, then fetch and emit the job's output. Surgical wiring of the already-real dispatch path — not a rewrite. No marc27-core changes.

## Branch & environment (do first, exactly as tasked)

- `git fetch origin`
- `git checkout -B feat/compute-dispatch origin/main` (current = 96a02b3; local `main` is ~24 behind — do NOT branch off it)
- `export CARGO_TARGET_DIR=/Volumes/Samsung SSD 1TB/cargo-targets/compute-dispatch`
- Verify baseline builds clean before touching anything: `cargo build -p prism-compute -p prism-cli`
- Local commits only — DO NOT push.

## Ground truth (confirmed on origin/main — actual line numbers; task's were ~200 lines off)

- `handle_run` — `crates/cli/src/main.rs:9249-9423`. Currently: `router.submit()` under a 120s timeout → `sleep(2s)` → ONE `router.status()` → print "Job submitted" → return. No poll loop, no `results()`. There is **no** `--timeout` flag on `prism run` today.
- `run_compute_job` (reference poll shape, marc27/raw-reqwest) — `main.rs:7265-7362`. Loop: `sleep(interval)` → GET status → match terminal strings → return result / bail on failed/cancelled/timeout.
- `ComputeBackend` trait — `crates/compute/src/lib.rs:31-38`: `async fn submit/status/results/cancel`. `JobStatus` enum (lib.rs:47-54): `Queued | Running{progress} | Completed | Failed{error} | Cancelled`.
- `ComputeRouter` — `crates/compute/src/backend.rs:24-208`: holds concrete `local/marc27/byoc` + in-memory `JobTracker` (job.rs:67, NOT persisted). Has inherent `pub async fn submit/status/results/cancel` (backend.rs:124/139/163/184). **Does NOT impl `ComputeBackend`.**
- `LocalBackend` — `crates/compute/src/local.rs`: `submit` (55-98) is detached `-d --network none` (safety default, keeping it); `status` (100-147) `docker inspect` → maps exit code, honestly bails "no such local compute job" for unknown ids (test at 240-260 must stay green); `results` (149-164) only reads host `/workspace/result.json` then `cleanup()` (`docker rm -f`).
- Node reference runner — `crates/node/src/executor.rs`: `execute_container_job` (183) + `poll_container` (448-485, 5s interval, `docker inspect` until exited) + `collect_output` (487-507, `docker logs --stdout/--stderr`). Compute layer has NO structured `JobOutput` (returns `serde_json::Value`).
- `handle_job_status` — `main.rs:9425-9451`: marc27-broker-only (in-process tracker is empty in a fresh process). FIX #3 deferred.
- `run` dispatch — `main.rs:3008-3039`. `Commands::Run` clap variant — `main.rs:256-301` (no timeout field).

## Design decisions (confirmed with user)

1. **Local output contract** = `docker logs + result.json`: `results()` prefers `/workspace/result.json` if present (preserves the convention), else wraps `docker logs` stdout/stderr (+ exit code) so ANY image returns output. Keep `--network none`.
2. **Tests** = trait-object helper + a `FakeBackend` test double (no docker, no network — AGENTS.md compliant).
3. **FIX #3 (job re-query)** = **defer**; flag honestly as a known gap in code + report.
4. **Timeout** = add `--timeout <secs>` to `prism run`, default 600s; honors it as the poll deadline.

---

## Patch 1 — `LocalBackend.results()`: docker logs + result.json  (FIX #2)

File: `crates/compute/src/local.rs`

- Add private helper `async fn collect_logs(&self, container_name: &str) -> (String, String)` mirroring `node/src/executor.rs:487-507`: two `docker logs --stdout --no-stderr` / `--stderr --no-stdout` calls, `.ok()`-tolerant (best-effort), truncated to a sane cap (reuse a small const, e.g. 256 KiB/stream, matching node's `MAX_PREVIEW_BYTES`).
- Rework `results(job_id)`:
  1. Resolve `container_name` from `self.active` (clone, drop lock). If unknown → existing honest `bail!("no such local compute job: {job_id}")` (do NOT regress the local.rs:240-260 test).
  2. If host `tmp_dir/result.json` exists and parses → use it as the primary value (existing contract preserved), but also attach `stdout`/`stderr`/`exit_code` alongside if logs are available (enrichment, non-breaking). Keep it simple: return the parsed JSON as-is when present.
  3. Else (no/invalid result.json) → build `{"stdout": ..., "stderr": ..., "exit_code": <from docker inspect>}` and return it. This is the path that makes arbitrary images return output.
  4. Always `self.cleanup(job_id).await` before returning (preserve existing behavior).
- No change to `submit` (`--network none` stays) or `status`.
- Tests: docker-dependent paths can't be unit-tested under AGENTS.md; add a small pure-helper test only if a pure piece is extractable (e.g. the "prefer result.json, else logs" merge decision as a pure fn over inputs). The end-to-end local-results behavior is covered structurally; the poll loop is covered by Patch 2's fake-backend tests.

## Patch 2 — `poll_to_terminal` helper + `impl ComputeBackend for ComputeRouter`  (FIX #1 core, enables tests)

File: `crates/compute/src/lib.rs` (trait impl + re-export) and new `crates/compute/src/poll.rs` (helper + tests), wired via `pub mod poll;` in lib.rs.

- `impl ComputeBackend for ComputeRouter` — thin delegation: each method calls the inherent method (`self.submit(...)` etc.). Inherent methods take priority over trait methods in Rust method-call resolution, so this delegates without recursion. Add an explanatory comment.
- `poll.rs`:
  ```rust
  pub async fn poll_to_terminal(
      backend: &dyn ComputeBackend,
      job_id: Uuid,
      poll_timeout_secs: u64,
      poll_interval: Duration,
  ) -> Result<Option<serde_json::Value>>
  ```
  - `Ok(Some(v))` = Completed, `results()` returned v.
  - `Ok(None)` = Completed but `results()` errored (e.g. no result.json and logs unavailable) — honest success-without-output.
  - `Err(...)` = Failed / Cancelled / deadline-expired / persistent status error → caller surfaces, exits non-zero. **Never** a fake success.
  - Loop mirrors `run_compute_job` (main.rs:7322-7361): `deadline = now + poll_timeout_secs`; each iteration `sleep(interval)` then `backend.status(job_id)`; match `Completed → results()`, `Failed{error}/Cancelled → bail!(honest msg)`, `Queued/Running → if now>=deadline bail!("still running after Ns; check prism job-status")`, `Err(e) → record last_err, retry until deadline, then bail(last_err)`.
- Tests (compute crate, in-process, no docker/network):
  - `FakeBackend` with a scripted `Vec<JobStatus>` (Arc<Mutex) status sequence + canned `results()` value + call counters.
  - `completes_and_returns_result` — [Running, Running, Completed], results `{"score":0.9}` → `Ok(Some({"score":0.9}))`, correct call count.
  - `failed_is_error_not_success` — [Running, Failed{..}] → `Err` mentioning "failed" (assert NOT `Ok`).
  - `cancelled_is_error` — [Cancelled] → `Err`.
  - `timeout_is_error` — always Running, tiny timeout, 1ms interval → `Err` mentioning timeout.
  - `completed_with_results_error_returns_none` — [Completed], `results()` → `Err` → `Ok(None)`.
  - `router_trait_impl_delegates_without_recursion` — `ComputeRouter::local_only()` as `&dyn ComputeBackend`, `cancel(<unknown uuid>)` returns `Ok(())` promptly (inherent cancel is Ok(()) on unknown jobs, backend.rs:184-207). Proves delegation + no infinite recursion, docker-free.

## Patch 3 — Wire `handle_run` to poll+results; add `--timeout`  (FIX #1 wiring)

File: `crates/cli/src/main.rs`

- `Commands::Run` (main.rs:256-301): add `#[arg(long, default_value_t = 600)] timeout: u64`.
- Dispatch (main.rs:3008-3039): thread `timeout` into `handle_run`.
- `handle_run` signature (9249-9263): add `timeout: u64`.
- Replace the single sleep+status block (9386-9420) with:
  ```rust
  let outcome = poll_to_terminal(&router, job_id, timeout, Duration::from_secs(2)).await;
  ```
  (Submit stays under its 120s timeout; `&router` coerces to `&dyn ComputeBackend` via Patch 2's impl. poll_interval=2s matches the old sleep.)
- Render by outcome, honoring `--json`:
  - `Err(e)` (Failed/Cancelled/timeout) → JSON `{"job_id","name","image","backend","target","inputs","status":"failed"|"cancelled"|"timeout","error": e}`; human → clear error line. Return the `Err` so the process exits **non-zero** (honest, no fake success).
  - `Ok(Some(result))` → JSON adds `"status":"completed","result":result`; human → `Status: completed` + `Output: {result}`.
  - `Ok(None)` → JSON `"status":"completed","result":null,"note":"completed; no output captured"`; human → `Status: completed (no output captured)`.
  - Drop the old `initial_status`/`status_error` keys (semantics changed: we now block to completion). Note in commit message.
- Marc27 path: now also polls to terminal via the router (consistent improvement). `run_compute_job` (the `prism compute run` path) is untouched — no regression.

## Patch 4 — Deferred follow-up note  (FIX #3, honest gap)

- Add a comment at `handle_job_status` (main.rs:9425) documenting that local/byoc jobs remain unqueryable post-submit (in-process `JobTracker` is gone in a fresh CLI process); recommend a future minimal persisted record mirroring `node::state::ActiveJobRecord` (`crates/node/src/state.rs:17`). No behavior change. Flag in the report.

## Tests — AGENTS.md compliance

- All new tests are in-process: `FakeBackend` (Patch 2) + pure helpers. **No real containers, no network calls.** No `mockito` needed (user chose fake-only); compute crate gains no new dev-dep.
- Do NOT weaken approval gating (`run_submit`/`compute_submit` stay `requires_approval: true`, command_tools.rs:452-459/580-587 — untouched).
- Do NOT add real-container integration tests. None exist today; none added.

## Gate (before each commit)

Run with the target-dir env set:
- `cargo fmt -p prism-compute -p prism-cli`
- `cargo clippy --workspace --all-targets -D warnings`
- `cargo test -p prism-compute -p prism-cli`
- `python3 scripts/check_no_cjk_in_agent_artifacts.py`
- `bash scripts/verify-tui.sh` (AGENTS.md-mandated; TUI/CLI-scoped, must stay green)
- Small, reviewable commits; stop and report after each. No push.

## Report will include

Branch + commits; the `docker logs + result.json` output-contract decision (with rationale); test approach (trait-object helper + `FakeBackend`); full gate results; FIX #3 deferred as a known gap.