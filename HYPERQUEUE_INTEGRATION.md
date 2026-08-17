# HyperQueue Integration — Decision, Seam, and Build Report

**Branch:** `feat/annotate-not-refuse` · **Scope:** `crates/compute` (backend
+ router + tracker), `prism run`/`prism job-status` CLI surface, `run_submit`
agent tool, `NOTICE`, `README.md` · **Baseline kept:** `byoc.rs` intact
and reachable; nothing deleted.

## 1. The question: link `hyperqueue` as a git dependency, or shell out to `hq`?

**Decision: shell out to the `hq` binary.** Verified against the clone at
`~/.claude/jobs/bd21bca9/tmp/hq` (v0.26.2, `680857b`):

1. **The `[lib]` is not an embedding API.** `crates/hyperqueue/src/lib.rs`
   exposes `client`/`server`/`worker` modules as `pub` only because HQ's own
   binary (`bin/hq.rs`) is a separate target that imports them
   (`hyperqueue::client::commands::submit::submit_computation`, …). There is
   no documented embedding contract, no crates.io publication, no semver
   promise. A git dependency would pin the *internals* of a fast-moving
   project.
2. **Single-threaded core.** HQ/tako is built on
   `WrappedRcRefCell` = `Rc<RefCell<T>>`
   (`crates/tako/src/internal/common/wrapped.rs`) — `!Send`. PRISM's
   `ComputeBackend: Send + Sync` runs on multithreaded tokio. Embedding would
   force a dedicated single-threaded `LocalSet` island plus channels around
   every call — complexity that buys nothing.
3. **MSRV collision.** HQ declares `rust-version = 1.95.0`; PRISM declares
   `1.92`. Linking forces either raising PRISM's declared MSRV or lying about
   it. Shelling out keeps PRISM's MSRV untouched — the new module needs only
   `std` + `serde` + `toml`, all already in the workspace. (The installed
   1.97.1 toolchain builds both, but the *declared* MSRV is the contract.)
4. **Build weight.** Linking drags HQ's tree (tako, nom, nom-supreme,
   chumsky, cli-table, ratatui, sysinfo, orion, …) into every PRISM build.
5. **Upstream maintains a first-class scripting surface:** `--output-type
   json` across `submit` / `job list` / `job info` / `job wait` / `worker
   list` / `alloc list`, and `hq job submit-file` (TOML Job Definition File)
   for heterogeneous task sets. The JSON shapes used here were read out of
   `client/output/json.rs`.
6. **PRISM precedent.** `byoc.rs` in this same crate shells out to
   ssh/sbatch/squeue/sacct/scancel/kubectl; the workspace shells out to
   `gh`, `git`, `hf`, `ollama`, `python3`, `agent-browser`. A subprocess
   boundary also keeps an HQ server or worker crash from taking down the
   PRISM process.

## 2. What was built

New module `crates/compute/src/hyperqueue.rs` (~850 lines of implementation,
~650 of tests):

- `HyperQueueBackend` implements the existing `ComputeBackend` trait, so it
  plugs into `ComputeRouter`, `JobTracker`, and the persisted
  `compute-jobs.json` with **no new status vocabulary**:
  - `BackendKind::HyperQueue { server_dir }` variant,
    `ComputeRouter::with_hyperqueue(config)`, dispatch arms in
    `status`/`results`/`cancel`.
  - `JobTarget::HyperQueue { server_dir }` + `JobRecord.hyperqueue_job_id`
    (`#[serde(default)]`, additive for existing state files) +
    `JobTracker::register_with_hyperqueue_job_id` — mirroring how the Slurm
    scheduler id is persisted.
- **Task-set submission.** A PRISM plan carrying `inputs.tasks`
  (`[{command: [...], cwd?, env?, stdin?}, …]`) or `inputs.command` (single
  argv) is serialised to an HQ Job Definition File (TOML) and submitted with
  one `hq job submit-file` — one HQ job regardless of task count. PRISM Uuid
  ↔ HQ job id is recorded in-memory and persisted. `HqTask` argv is exec'd
  by HQ with **no shell**, and the JDF is a file PRISM writes, not shell
  text — no quoting/injection layer.
- **Status in the existing vocabulary.** `hq job list --output-type json`
  task counters fold into `JobStatus` (progress = finished fraction);
  `task_statuses()` maps per-task HQ states (`waiting/running/finished/
  failed/canceled/aborted`) into `JobStatus` values via
  `HqTaskStatus { task_id, status }`.
- **Two worker sources (`HqMode`):**
  - `Standalone { workers }` — PRISM starts `hq server start --host
    127.0.0.1` in its own server dir and spawns N local workers. No Slurm
    anywhere; testable on this machine.
  - `AutoAlloc { scheduler, time_limit, extra_args }` — PRISM registers
    `hq alloc add slurm|pbs --time-limit … -- <extra>`; HQ's automatic
    allocator asks the scheduler for allocations and spawns workers inside
    them.
- **Results** reads each task's stdout file (path taken from `hq job info`
  JSON, which handles HQ's `%{CWD}/job-%{JOB_ID}/…` templating for us),
  parsing JSON with a text fallback; refuses honestly if the set is not
  finished.
- `shutdown()` stops the PRISM-owned server (workers follow); never called
  automatically because in-flight tasks would be lost.

### Failure semantics (no fake success)

- `hq` missing → every verb fails with `cargo install hyperqueue` / the
  GitHub releases URL. No silent fallback to another backend. (Pinned by
  test.)
- Server cannot be polled → `status` returns `Err`, never a fabricated
  `Running`. A vanished job id (server restarted without a journal) reports
  `JobStatus::Failed` with that explanation. (Both pinned by tests using a
  fake `hq` script.)
- Offline mode (`PRISM_OFFLINE=1`): `Standalone` is allowed (loopback-bound
  server, local workers — same category as a local SSH daemon in
  `byoc.rs`); `AutoAlloc` is refused because the compute leaves the machine
  through the scheduler and the target is unverifiable — the same
  fail-closed reasoning `byoc.rs` applies to kubectl contexts. All four
  trait methods call `check_offline` first (wiring test mirrors the byoc
  one).

## 3. The seam, and how workloads are routed HQ vs `byoc.rs`

The seam is the `ComputeBackend` trait plus router heuristics; `byoc.rs` is
byte-for-byte the same implementation (only a `git status`-visible change in
this branch that predates this work remains; nothing here edits its logic).

Routing rules, in order, inside `ComputeRouter::resolve_backend`:

1. `marc27`/`platform` images → MARC27 (existing heuristic, unchanged).
2. **Task-set-shaped plan** (`inputs.tasks` or `inputs.command`) **and a
   HyperQueue backend is configured → HyperQueue**, even if the default is
   byoc. N independent tasks are one HQ job; through byoc they would be N
   `sbatch` submissions and hit queue limits — the exact failure mode of the
   motivating case.
3. Everything else → the caller's default backend. A single long job that
   needs checkpoint/requeue therefore stays on `ByocTarget::Slurm`, because
   HQ has no checkpoint/resume for a monolithic job (it has task-level retry
   via crash limits, which is not the same contract as exit-140 →
   `scontrol requeue`).

So the decision is: **caller picks the default** (`.with_byoc(...)` vs
`.with_hyperqueue(...)`), and the router only overrides for the unmistakable
many-task shape. A caller that wants explicit control can always talk to
`HyperQueueBackend::submit_tasks` / `ByocBackend` directly.

### The 91-paper corpus run, expressed

```rust
let tasks: Vec<HqTask> = papers.iter().map(|p| HqTask {
    command: vec!["python3".into(), "ingest_paper.py".into(), p.path.clone()],
    cwd: Some(corpus_dir.clone()),
    env: BTreeMap::from([("VENV".into(), shared_venv.clone())]), // one shared venv
    ..Default::default()
}).collect();

let router = ComputeRouter::local_only_persistent(&data_dir)?
    .with_hyperqueue(HyperQueueConfig::standalone(data_dir.join("hyperqueue"), 2));
let job_id = router.submit(&ExperimentPlan {
    name: "corpus-ingest-91".into(),
    image: "unused-by-hq".into(),
    inputs: json!({ "tasks": tasks }),
}).await?;
```

That is one `hq job submit-file` instead of a bash `wait -n` loop: the
concurrency cap is HQ's load balancer (works on macOS bash 3.2 by not
involving bash at all), the two workers share ONE venv instead of 91
isolated 1.6 GB builds, `prism job-status` shows `Running { progress:
47/91 }`, and a failed paper shows up as a named failed task instead of a
disk-full abort at paper 47.

The same run from the shell (no Rust needed):

```bash
# one line of jq/python writes tasks.json from the paper list
cat > tasks.json <<'EOF'
[{"command": ["python3", "ingest_paper.py", "paper-0047.pdf"],
  "cwd": "/work/corpus", "env": {"VENV": "/work/venv"}},
 ... 91 task objects ...]
EOF
prism run --backend hyperqueue --hq-tasks tasks.json --hq-workers 2 \
  --name corpus-ingest unused-by-hyperqueue   # image is positional; HQ tasks carry commands
prism job-status <uuid>
```

## 4. What I did NOT duplicate from `byoc.rs`

- No SSH layer, no sbatch script generation, no squeue/sacct polling, no
  checkpoint/requeue (exit-140) contract — all of that stays exclusively in
  `byoc.rs`, which remains the only Slurm-direct path.
- No parallel status vocabulary: HQ states fold into the existing
  `JobStatus`/`TrackedStatus`, and the HQ job id is stored exactly like the
  existing `slurm_job_id` field.
- No second job tracker — the same persistent `JobTracker` records HQ jobs.

## 5. MIT attribution obligation

HyperQueue is MIT. Verified from the clone's `LICENSE`: the copyright line
reads **"Copyright (c) 2021-present, Ada Böhm, Jakub Beranek"** (the repo
lives under the It4Innovations org, but those two are the named holders).
PRISM shells out rather than links or vendors, so no HQ code or binaries are
distributed inside PRISM and there is no license-text shipping obligation
for this change. Because PRISM's error message tells users to install `hq`
itself — a real runtime dependency — `NOTICE` now carries the attribution:
project name, copyright holders, repo URL, MIT reference, plus the rule that
bundling the binary would require shipping the license text with it.

## 6. Constraints stated, not papered over

- **MSRV:** HQ needs Rust 1.95; PRISM declares 1.92. Resolved by shelling
  out — the integration code itself is 1.92-compatible. If PRISM ever links
  HQ, the workspace MSRV must move to ≥1.95 deliberately.
- **`hq` is not installed on this machine** (verified: not on PATH). That is
  why tests drive the backend through a fake `hq` shell script plus pure
  parsing/folding tests; none of them require HyperQueue.
- **HQ version drift:** JSON shapes were verified against 0.26.2. An unknown
  task state fails loudly (`hq version drift?`) instead of guessing.

## 7. Reachability — how the capability is reached (agent and TUI parity)

- **Agent:** the typed `run_submit` tool now takes `backend:
  "hyperqueue"` plus `hq_tasks_path` (path to a task-set JSON file the agent
  writes with its existing file-writing tool), `hq_workers`,
  `hq_server_dir`, `hq_autoalloc`, `hq_time_limit`, and `hq_extra`. The
  tool's JSON schema and description name the many-task use, the permission
  mode is `FullAccess` with `requires_approval: true` — exactly like its
  byoc/marc27 neighbours. The raw `run` tool (`FlagPolicy::AnyBehindApproval`)
  also reaches the new CLI flags behind approval, as before.
- **TUI:** a TUI user reaches the same capability by asking the in-TUI agent
  to submit the task set (the agent calls `run_submit`); `prism job-status
  <uuid>`-equivalent polling is likewise agent-reachable. This matches the
  established parity bar: byoc/marc27 compute has no dedicated TUI panel
  either — the agent tool IS the TUI surface, same completeness (submit,
  poll, cancel all flow through the same tool/CLI paths). No exit-to-CLI is
  introduced: everything the CLI can do here the agent can do in-session.
- **CLI:** `prism run --backend hyperqueue --hq-tasks tasks.json [--hq-workers
  N] [--hq-server-dir D] [--hq-autoalloc slurm|pbs] [--hq-time-limit 1h]
  [--hq-extra --partition=main]` and `prism job-status <uuid>` (now resuming
  the HQ backend from the persisted tracker record instead of bailing).

## 8. What remains unbuilt

- **AutoAlloc is coded but unverified against a real cluster** (none
  available here); standalone mode is the tested path.
- No HQ journal file is configured, so a crashed PRISM-owned server loses
  job state; a `--journal` flag on `server start` is a one-line follow-up.
- `results()` reads task stdout files on the local filesystem — correct for
  standalone and for shared-FS clusters, not for scratch-only nodes.
- The task-set file is JSON written up front; there is no streaming/generator
  submission for very large sets (the 91-paper case fits comfortably).
- Nothing deletes anything: `byoc.rs`, `local.rs`, `marc27.rs` unchanged.

## 9. Files changed

| File | Change |
| --- | --- |
| `crates/compute/src/hyperqueue.rs` | **new** — backend, modes, JDF generation, JSON parsing, status folding, offline policy, 40+ tests |
| `crates/compute/src/backend.rs` | `BackendKind::HyperQueue`, `with_hyperqueue`, task-set routing heuristic, dispatch arms, tracker registration |
| `crates/compute/src/job.rs` | `JobTarget::HyperQueue`, `JobRecord.hyperqueue_job_id`, `register_with_hyperqueue_job_id`, serde-compat tests |
| `crates/compute/src/lib.rs` | module wiring + re-exports; `PartialEq` added to `JobStatus` derive (additive) |
| `crates/compute/Cargo.toml` | `toml = { workspace = true }` (already a workspace dep, v0.8) |
| `crates/cli/src/main.rs` | `prism run` gains `--backend hyperqueue` + `--hq-tasks/--hq-workers/--hq-server-dir/--hq-autoalloc/--hq-time-limit/--hq-extra` with cross-backend validation; `prism job-status` resumes `HyperQueueBackend` from the persisted record instead of bailing "not wired"; JSON payload and human output carry `hyperqueue_job_id`; parse/validation/task-file tests |
| `crates/agent/src/command_tools.rs` | `run_submit` schema + builder emit the `hq_*` fields; description names the many-task backend; schema + preview tests |
| `NOTICE` | HyperQueue MIT runtime-dependency attribution (holders verified from HQ's LICENSE) |
| `README.md` | one line in the compute example block |

## 10. Gate output

Pasted verbatim (exit codes captured directly, not through a pipe):

```
$ cargo fmt --all
FMT_EXIT=0

$ cargo test --workspace
92 test-result lines, all ok — passed=3157 failed=0
TEST_EXIT=0

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking prism-cli v1.0.0 (/Users/siddharthakovid/Downloads/prism-unmuzzle/crates/cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.24s
CLIPPY_EXIT=0
```

Baseline at task start was 3074 passing; the count is higher because other
work landed on this branch concurrently — no test newly fails, clippy clean.
All gates ran as debug builds; `target/release/prism` was never rebuilt.
