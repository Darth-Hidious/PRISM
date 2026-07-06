# PRISM + marc27-core Audit Backlog

_Generated 2026-07-04 by a 42-agent static-analysis pass (14 Sonnet readers over both repos, every `critical`/`high` fake-or-broken claim re-checked by an Opus REFUTE-by-default verifier at its cited `file:line`)._

## How to read this

- **112 raw findings** — 9 critical, 47 high, 39 medium, 17 low.
- **20 CONFIRMED** = adversarially verified real defects. **Trust these.** Start here.
- **7 PARTIAL** = real code, but the verifier found the severity/scope overstated (usually an honest stub or a narrower blast radius). Read the "actually" note before acting.
- **1 REFUTED** = dropped (`prism federation peers` is honest — it self-discloses `platform_supported:false`).
- **84 NOT_VERIFIED** = perf/speed + medium/low findings **below the verify threshold**. Code-cited but **not** adversarially checked — plausible, not proven. Treat as leads, confirm before large refactors.

> Honesty note: "111 survive" from the run log is misleading — only the 20 CONFIRMED went through adversarial verification. Everything else is either nuanced (PARTIAL) or unverified (perf). Don't sprint on unverified items as if they're gospel.

---

## Tier 0 — CONFIRMED critical/high (verified real; "it just works" blockers)

> **Progress (2026-07-04):** PRISM-side 0.1–0.7 ✅ DONE. marc27-core: provider fabrication cluster ✅ DONE (`cargo check`+tests green). Rest of marc27-core (executors, GraphQL, tenant, bayesopt) open.
> **marc27-core cancellation cluster ✅:** DB `complete()`/`fail()` now guard terminal state (a finishing worker can't flip `cancelled`→`completed` — 0.17 race); `mark_running` guarded + `rows_affected` check (honest NotFound, no cancelled→running race); 7 executor `status`/`cancel` fake no-ops removed → honest trait DEFAULT (`Err` "not wired") so none can silently fake job control. `clippy --all-targets -D warnings` + 171 jobs tests green. (Self-caught: `cargo fix` broke 2 test imports; gate caught it, fixed.)
> **marc27-core api-honesty ✅ (0.8 + 3):** GraphQL `root.rs:50` graph now OPTIONAL — a FalkorDB outage no longer 500s the whole API (billing/me/jobs/marketplace keep working); `ingestJob` null-stub → explicit error; `computeGpus` empty-list → "broker not configured" error; admin `list_containers` empty-200 → honest error. `cargo check -p marc27-api` RC=0.
> **marc27-core providers ✅ (6 fake-successes killed):** runpod `result()` hardcoded $1.10-A100 cost → honest Err (+test); runpod `logs()` static→Err; aws `logs()` stops swallowing CLI errors; lambda `status()` missing-instance→`Completed` → `Err(NotFound)`; lambda launch empty-IDs→empty-string → Err; lambda `logs()` static→Err. `list_gpus available:true` LEFT deliberately — it's the broker's selection gate (`vast.rs` does the same); the real gap there is static pricing = connect-later.
> - 0.1 ✅ AllowAll now sticks (protocol.rs maps `all`→AllowAll; agent_loop writes `"*"` to session overrides; `is_allowed` honors wildcard; +2 tests)
> - 0.2 ✅ `/api/tools/:name/run` → honest 501, fake `Success` audit removed
> - 0.3 ✅ `/api/data/ingest` → honest 501
> - 0.4 ✅ provenance non-UTF-8 path → hard error (no silent `:memory:`)
> - 0.5/0.6/0.7 ✅ patent+MP collectors raise `CollectorConfigError` (surfaced as `patents_error`/`skipped`); `collect_all` + acquisition loop no longer swallow

### PRISM
| # | file:line | sev | defect | fix |
|---|-----------|-----|--------|-----|
| 0.1 | `agent/src/agent_loop.rs:999` | crit | **"Allow All" == "Allow Once."** The `AllowAll` arm is comments-only; no flag set, guard re-fires next tool call. (Latent: `AllowAll` is also never *constructed* anywhere — the UI can't send it today, so it bites the moment someone wires that button.) | add `let mut auto_approve_rest=false`, set on `AllowAll`, OR-into the approval gate |
| 0.2 | `server/src/handlers/tools.rs:117` | crit | **`POST /api/tools/:name/run` lies.** Returns 200 `"accepted"/"queued"` + writes an audit entry falsely marked `Success`, but nothing is dispatched — no queue, no channel, no spawn. | enqueue to a real mpsc consumed by the agent loop, or return 501. **First check reachability from shipped clients (API-first).** |
| 0.3 | `server/src/handlers/data.rs:51` | crit | **`POST /api/data/ingest` lies.** Same pattern — body discarded, nothing ingested, `"accepted"` returned (message even redirects to the CLI). | return 501/503 or honest status until the pipeline is wired |
| 0.4 | `provenance/src/lib.rs:100` | high | **Silent `:memory:` fallback.** Non-UTF-8 path → `unwrap_or(":memory:")` → whole session's provenance lost on exit, no warning. | `bail!` when `path.to_str()` is `None` |
| 0.5 | `app/tools/data_collectors/patent_collector.py:20` | high | Missing `LENS_API_TOKEN` → returns `[]`; agent reads count=0 as "no patents exist." | `{"error":"LENS_API_TOKEN not configured","results":[]}` |
| 0.6 | `app/tools/data_collectors/collector.py:86` | high | `MPCollector.collect()` returns `[]` when `MP_API_KEY` unset — source silently skipped. | return dict with `error` key |
| 0.7 | `app/tools/data_collectors/base_collector.py:45` | high | **`collect_all()` swallows every collector exception with bare `pass`.** Any source's network/auth/import failure vanishes; caller gets a partial/empty result, no warning. | log WARNING + per-source error entry in returned metadata |

_This Python-collector cluster (0.5–0.7) is the mechanism behind "most tools don't work": they don't crash, they return empty and the agent believes the empty._

### marc27-core
| # | file:line | sev | defect | fix |
|---|-----------|-----|--------|-----|
| 0.8 | `api/src/routes/graphql/root.rs:50` | crit | **No-FalkorDB → all GraphQL rejected**, including pure-Postgres resolvers (`me`, `billingBalance`, `agentRuns`, `jobs`, `marketplace`, `llmModels`, `deployments`, `discourseSpecs`). Those are mounted but unreachable without the graph. | pass `graph: Option<GraphClient>` into schema context; graph resolvers already `?`-unwrap, Postgres ones keep working |
| 0.9 | `core/src/compute/providers/lambda.rs:251` | high | **`status()` returns `Completed` when instance missing** from `GET /instances`. Pagination miss / transient error → live-or-failed job reported done. | `Err(NotFound)` on missing instance |
| 0.10 | `core/src/compute/providers/lambda.rs:224` | high | Empty `instance_ids` → empty-string stored as tracked ID; all later lifecycle calls fail on blank ID. | `is_empty()` → `Err` |
| 0.11 | `core/src/compute/providers/runpod.rs:65` | high | **`list_gpus()` hardcoded** 3-type static table `available:true`; never calls RunPod. | query RunPod machine-types/graphql |
| 0.12 | `core/src/compute/providers/aws.rs:82` | high | **`list_gpus()` hardcoded** 7-type static array `available:true`; no `describe-instance-type-offerings`. | call EC2 offerings by region |
| 0.13 | `core/src/compute/providers/runpod.rs:208` | high | **`result()` always bills A100 @ $1.10/hr**, always returns `gpu_type:"A100-80GB"` regardless of real GPU. | thread real GPU type + price from submit through the job ID |
| 0.14 | `core/src/agents/mod.rs:1` | high | **Whole ACP module (claims/channels/reputation/identity) has zero callers** — re-exported from lib, never instantiated. Feature doesn't exist at runtime. | wire persistence+broker+routes, or drop the re-export |
| 0.15 | `core/src/bayesopt/campaign.rs:519` | high | **`build_surrogate` always `NotImplemented`** → every `acquire_next` / BO loop errors out. | wire the existing `ConstantBaseline` as default |
| 0.16 | `core/src/identity/federation_grants.rs:210` | high | **`max_calls_per_day` discarded** (`let _ = row.max_calls_per_day`) → capped grant returns `Granted` forever. | `COUNT` from `federation_call_log` → `DailyQuotaExceeded`, or drop the column |
| 0.17 | `api/src/routes/jobs.rs:98` | high | **Cancel doesn't cancel + race un-cancels.** Marks row `cancelled` but never stops container/HPC work; `complete()` has no status guard (`WHERE id=$1`) so a finishing worker overwrites `cancelled`→`completed`. | call `executor.cancel()`; add status guard to `complete()`/`fail()` |
| 0.18 | `api/src/routes/graphql/ingest.rs:13` | high | `ingestJob` resolver is a permanent `null` stub — indistinguishable from "job missing." | `Err("use REST GET /knowledge/ingest-job/{id}")` |
| 0.19 | `knowledge-service/src/main.rs:1623` | high | **Query-ingest feeds the query string as both doc_id AND document body** → LLM extracts entities from `"What is Inconel 718?"` = garbage. | call OpenAlex/Semantic Scholar and ingest real papers, or `Err` |
| 0.20 | `knowledge-service/src/main.rs:1241` | high | **All Supabase/Platform JWT users tagged `tenant='public'`** → private embeddings/graph/provenance co-mingled in the globally-readable namespace. | derive `project_{id}` from the projects table (already done for API-key users) |

---

## Tier 1 — PARTIAL (real, but read the "actually")

| # | file:line | claim | actually |
|---|-----------|-------|----------|
| 1.1 | `ingest/src/ontology.rs:187` | `extract_entities` passes zero rows → blind LLM extraction | true, but **no production caller** — pipeline uses `extract_entities_with_mapping` with real rows. Unused stub. → deprecate/remove the blind method |
| 1.2 | `tui/src/backend.rs:553` | FakeBackend returns hardcoded gpus/models/sessions `available:true` | true, but gated behind documented `--fake-backend`/`--test-backend`; structurally unreachable from Real path. → low risk; optionally mark payloads synthetic |
| 1.3 | `api/src/routes/graphql/intelligence.rs:449` | `createDeployment` hardcodes project UUID → breaks tenant isolation | project_id IS hardcoded, but isolation is enforced by `user_id` (derived from auth, used in every read/write). → cosmetic project mislabel, not an isolation break |
| 1.4 | `core/src/compute/providers/aws.rs:152` | malformed JSON → `"unknown"` job ID corrupts lifecycle | malformed JSON is handled earlier (`from_str().map_err()?`); only a **missing** InstanceId hits `unwrap_or("unknown")`. Real but narrow. → `Err` on absent ID |
| 1.5 | `core/src/bayesopt/repl.rs:264` | `handle_list_active_hypotheses` returns `[]` unconditionally | honestly-labeled placeholder (comment cites PR #24), **zero callers**. → unwired stub, not a live lie |
| 1.6 | `llm-service/src/main.rs:638` | quota always uses `'free'` tier → paid users capped | hardcode is real, but real billing is **credit-based** (checked separately); this token quota is secondary. → fix, but not a billing breakage |
| 1.7 | `db/src/queries/billing.rs:46` | `debit_credits` allows silent overdraft (no `<0` check, no CHECK constraint) | true; sole caller documents it as intentional (credits can go negative by design). → decide: enforce, or fix the docstring's "error if insufficient" promise |

---

## Tier 2 — marc27-core honesty cluster (code-cited, medium; the #73 fakery target)

Not adversarially verified (medium sev), but these are exactly the "no stubs" list — and they corroborate the Track B map from memory:

- `jobs/types/hpc_submit/executor.rs:171,176` — `status()` always `Running`, `cancel()` no-op (explicit TODOs).
- `jobs/types/{ml_predict,plugin_exec,simulation,dataset_process,mcp_host,container}/executor.rs` — 6 container executors: `status()`/`cancel()` are no-ops.
- `db/queries/jobs.rs:75` — `mark_running` ignores `rows_affected()`, returns `Ok(())` for a nonexistent job ID.
- `core/compute/providers/{aws:193,runpod:173,lambda:257}` — `logs()` returns a static advisory string as `Ok(())`, presented as real log content.
- `api/routes/{compute.rs:286, graphql/compute.rs:19, admin_containers.rs:94, nodes.rs:508}` — return **empty-200 / hardcoded catalog** when the backend is unconfigured (should be honest "not configured"). _Note: `compute.rs:286` catalog `available` was already flipped to `false` this session — verify the fix landed._
- `core/identity/data_access.rs:17` — `assert_can_read/write` never called from graph/embedding paths (RBAC not enforced).
- `core/embeddings/search.rs:179` — non-UUID id/corpus_id silently swallowed → rows dropped.
- `discovery-service/main.rs:361` — model pricing hardcoded per name-pattern, not sourced.
- `jobs/scheduler.rs:7` — entire scheduler module is an empty placeholder.

---

## Tier 3 — Performance themes (UNVERIFIED — leads, confirm before refactor)

**T3a · Blocking I/O on the async executor (12).** Highest-leverage theme. Sync work stalling Tokio threads:
`command_tools.rs:2671` (blocking TCP connect 150ms), `doctor.rs:42` (blocking connect, up to OS timeout on firewalled port), `node/executor.rs:199` + `node/daemon.rs:514` (`sysinfo::new_all()` / `refresh_all()` inside `select!`), `workflows/lib.rs:157` (`std::fs` dir scan at every chat startup), `agent_loop.rs:684` (`skills_menu` sync `read_dir` per TAOR iteration), `compute/local.rs:149` (`Path::exists()` stat), `audit/lib.rs:316` (open+write+close per audit line), `tui/app.rs:1731` + `tui/knowledge.rs:107` (sync file reads on UI path), `render.rs:305` (`SystemTime::now()` syscall per frame). → `tokio::fs` / `spawn_blocking` / cache-at-startup.

**T3b · Resource rebuilt per call — connection pools destroyed (7).**
`campaign/lib.rs:689` (**crit** — `reqwest::Client::new()` per candidate in a serial loop) + `:526` (new `LlmClient` per iteration), `server/middleware/auth.rs:62` + `server/lib.rs:152` (new SQLite conn per request/audit-write), `handlers/query.rs:135` (new `Neo4jGraphStore` per query), `cli/main.rs:4433` (new client + disk cred read per ingest chunk), `app/mcp_client.py:124` (**crit** — fresh MCP client per tool call via `asyncio.run()`). → hold one client/pool as a field/`NodeState`.

**T3c · Per-frame TUI waste at idle (5), all `render.rs`.**
`:629` (`derive_tools` ×2/frame), `:240` (`markdown_lines` re-parsed every 100ms), `:786` (`derive_activity`+`derive_files` re-scan `messages`), `:102` (`clean_model_name` alloc/frame), `:336` (`format!` built then discarded — dead, delete). → compute once/pass by ref; cache rendered lines keyed on (msg idx, width); incremental side-tables on `App`.

**T3d · Sequential awaits that should be concurrent (10).**
`cli/main.rs:4717` (3 status requests serial), `campaign/lib.rs:440` (serial candidate eval), `orch/docker.rs:534` (serial 60s readiness waits → ~90s not ~30s), `provenance/lib.rs:243` (`query_chain` N round-trips → recursive CTE), federated-query serial peers `cli/main.rs:7384`. → `try_join!` / `join_all`. **Plus a real correctness bug here:** `server/ws.rs:83` **TOCTOU race** on WS connection cap (two upgrades both pass the check → 2×MAX). And `cli/main.rs:1112` — `unsafe set_var` **after** a background task is spawned (POSIX setenv not thread-safe, UB/torn reads).

**T3e · Micro-perf / alloc (rest).** `llm/lib.rs:531,667` (O(n²) SSE buffer reassembly per line), `agent_loop.rs:1037` (SQLite open per meta-tool dispatch), `command_tools.rs:1519` (linear scan of 47-entry array per dispatch → `phf`), `tui/app.rs:2882` (`Vec::remove(0)` → `VecDeque`), `cli/main.rs:1073` (**crit-tagged** — `ensure_venv` forks `python3 -c 'import app'` on *every* `prism gpus/billing/status`), `handlers/query.rs:529` (`to_uppercase()` alloc per query), etc.

---

## Tier 4 — Binary bloat / deps

- `agent/Cargo.toml:7` — **`prism-agent` depends on `prism-ingest`** (→ Polars + C extensions) just for a type alias. Break the dep.
- `cli/Cargo.toml:33` — the single `prism` binary **links all 22 crates unconditionally** (Polars, rdkafka, …). Feature-gate heavy providers.
- `Cargo.toml:108` — **two `sha2` versions** (0.10.9 + 0.11.0) both compiled in (confirmed in Cargo.lock). Unify.

_(Forge fork already removed this session — 131K LOC / 24 crates gone. These are what's left.)_

---

## Suggested sprint order

1. **Tier 0 honesty (0.1–0.20)** — verified lies/silent-loss. Highest trust impact; several are one-liners (`bail!`, `Err`, status guard).
2. **T3b + T3a crits** — `campaign` per-candidate client, `mcp_client` per-call client, `ensure_venv` subprocess-per-command: cheap, felt on every run.
3. **Tier 2 marc27-core executors** — finish the #73 honest-status sweep (status/cancel no-ops, empty-200s).
4. **T3c TUI idle CPU** — self-contained in `render.rs`.
5. **Tier 4 bloat** — smaller/faster binary for the KOM build.
6. Tier 1 PARTIAL — mostly relabel/deprecate, low urgency.
</content>
</invoke>
