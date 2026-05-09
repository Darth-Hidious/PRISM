# SHIPPED — what's actually in PRISM main, by date

> **Why this file exists.** Long sessions get compacted; recent-memory bias starts producing claims that aren't backed by code. This log is the source of truth for what was actually merged, what was tested, and what was *claimed* but not verified.
>
> **How to extend.** Append a new dated section. Don't edit existing entries — strike-through (`~~text~~`) and add a follow-up note instead. Each PR row needs a verifiable evidence column: a merged commit hash, a passing test name, or a "tested live" note with what was actually run.
>
> **How to verify before relying on this.** Use the `codebase-memory-mcp` tools — `list_projects`, `search_graph` for symbols, `search_code` for content. Don't trust the claims here without spot-checking against the index. Specifically:
>
> ```
> mcp__codebase-memory-mcp__list_projects                               # PRISM should be there
> mcp__codebase-memory-mcp__search_graph(name_pattern="X")              # find a symbol claimed below
> mcp__codebase-memory-mcp__search_code(pattern="X", project="PRISM")   # confirm it's where claimed
> ```

---

## 2026-05-09 — Fabric v1 sprint

### Bugs found, fixed, and verified

| # | Bug | Fix | Verified live? |
|---|-----|-----|----------------|
| 12 | `_notes_for_installer` array in `.mcp.json` broke forge's strict parser → TUI failed to boot | PR #41 — moved notes to `.mcp.json.notes.md` sidecar | Yes — TUI booted clean post-merge (pty-debug snapshot) |
| 13 | `normalize_url` in fetch tool didn't recover leading-dot URLs (`.wikipedia.org/path` → `https://.wikipedia.org/...` still unparseable) | PR #42 — strip 1 leading `.` when followed by domain-lead char; tightened scheme-less branch to reject `..foo` | Tested via 9 unit tests (`normalize_url_strips_stray_leading_dot`, `normalize_url_does_not_strip_double_dot_or_space_after_dot`); strings present in built binary |
| 14 | Generic fetch errors gave the agent no signal whether to retry or fall back → URL hallucination retry loops | PR #42 — `classify_request_error` + `classify_status_error` for DNS/4xx/5xx/timeout with explicit fallback nudges only on hallucination signals | Verified via `strings prism \| grep "training knowledge"` — all 6 classified strings present |
| 16 | `terminalcp` MCP printed usage banner to stdout without `--mcp` arg → rmcp parse error in TUI chat panel | PR #45 — added `--mcp` to args | **Live verified**: rebooted TUI under pty-debug; `ERROR rmcp::transport::async_rw` gone, prompt clean |
| 17 | `install-tui-mcps.sh` failed silently on `mcp-tui-test` because upstream pyproject has flat-layout + 2 top-level modules | PR #46 — script now patches pyproject to constrain `py-modules = ["server"]` | Re-ran the script: clean install, both binaries on disk, `.mcp.json` wired |
| 18 | `prism node up --offline` still tried to refresh the platform token → 401 crash | PR #50 — added `offline` to `DaemonOptions`, short-circuit `run_daemon` on ctrl-c when offline | **Live verified**: two `prism node up --offline` processes both came up green and served `/api/status` |
| 19 | mDNS peer discovery between two local prism processes returns `peer_count: 0` on both | **NOT FIXED YET.** Found 2026-05-09 by running #18 fix end-to-end. Next on the list. | n/a |

### Features merged (all on 2026-05-09)

PR list from `gh pr list --state merged`:

| PR | Layer | What it adds |
|----|-------|--------------|
| #37 | `forge_domain` | Infer model context window from id when platform reports null |
| #38 | F1c2 (cli) | `prism federation whoami` + `peers` (read-only) |
| #39 | F2 (policy) | Cross-org policy intersection — strictest-wins. `crates/policy/src/intersect.rs::intersect_decisions` |
| #40 | (chore) | Wired 3 TUI MCPs in `.mcp.json` + install script |
| #41 | (fix) | `.mcp.json` strict-parser hotfix (Bug #12) |
| #42 | (fix) | URL hallucination guard (Bugs #13, #14) + streaming regression test |
| #43 | F1c3 (mesh) | `PlatformPubkeyFetcher` + `ActionRoleTable` in `crates/mesh/src/federation_lookup.rs` |
| #44 | F3 (mesh) | Locality scoring primitive — `crates/mesh/src/locality.rs` |
| #45 | (fix) | `terminalcp --mcp` flag (Bug #16) |
| #46 | (fix) | install-script pyproject patch (Bug #17) |
| #47 | F4 (mesh) | Capability descriptors + burst routing — `crates/mesh/src/burst_routing.rs` |
| #48 | F5 (audit) | Signed cross-org audit envelopes — new `prism-audit` crate at `crates/audit/` |

PRs still open at time of writing (verify with `gh pr list --state open`):

| PR | What | Status |
|----|------|--------|
| #49 | F6 — cross-site inference demo (single-process) | Open, awaiting CI |
| #50 | Bug #18 — `--offline` daemon fix | Open, awaiting CI |

### What's tested vs claimed-but-not-tested

This is the section that matters. **Don't trust "we shipped F-N" without checking the column on the right.**

| Layer | What was claimed | What was actually tested |
|-------|------------------|--------------------------|
| F1 (federation primitives) | Identity sign+verify, request sign+verify, role check, expiry | 9 unit tests in `crates/mesh/src/federation.rs::tests` (happy path, bad platform sig, wrong root, bad request sig, expired identity, missing role, malformed key, malformed sig) |
| F1c2 (CLI) | `prism federation whoami` + `peers` | CLI tests in `crates/cli`. **Not run live against api.marc27.com this session.** |
| F1c3 (mesh) | Pubkey fetcher 3-layer cache + ActionRoleTable | 11 unit tests with mock HTTP source. **Not tested against the real platform endpoint** (`/federation/platform-pubkey` may not even exist on api.marc27.com yet) |
| F2 (policy) | Cross-org strictest-wins intersection | Unit tests in `crates/policy/src/intersect.rs`. Default policy is `crates/policy/src/default.rego`, only allows `workflow.execute / tool.call / data.query` — caught at F6 demo |
| F3 (locality) | Region/zone/latency/residency scoring | 14 unit tests in `crates/mesh/src/locality.rs`. **All in-memory; no real geographic data ever fed in** |
| F4 (burst routing) | Local-first, peer-fallback | 11 unit tests. Reuses `prism_proto::NodeCapabilities`. **Never hit a real mesh of nodes** |
| F5 (audit envelopes) | Sign + verify + JSONL append-only log | 14 unit tests. **Not wired into anything that emits real Fabric events; only F6 demo uses it** |
| F6 (demo) | End-to-end happy path | Single-process `cargo run --example`, all 10 steps print correctly, audit chain verifies. **Both orgs run in one binary; transport is in-memory; not real network** |
| TUI bug fixes (#12, #16) | Boot is clean post-fix | Verified live by rebooting under pty-debug |
| URL bug fixes (#13, #14) | Recovery + classifier wired | `cargo test` + `strings <binary> \| grep` — **agent's actual retry behaviour with the new error messages is NOT verified live; would need a chat session against a live LLM** |
| `--offline` daemon fix (#18) | Daemon boots without platform | **Verified live**: both nodes ran, dashboards served, `/api/mesh/nodes` returned valid JSON |
| Two-node mesh discovery | n/a | **NOT WORKING** (`peer_count: 0`). Bug #19. |

### Duplication audit (vs existing PRISM)

Used `codebase-memory-mcp::search_graph(name_pattern=...)` to verify F-series didn't reinvent existing modules.

| Layer | Existing equivalent? | Verdict |
|-------|----------------------|---------|
| F1 federation primitives | None | Genuinely new |
| F1c3 federation_lookup | None named `ActionRoleTable*` or `PlatformPubkey*` | Genuinely new |
| F2 policy intersect | `crates/policy/src/lib.rs` already had `PolicyEngine` + `PolicyDecision`; intersect added new function. Composes, doesn't duplicate. | Clean addition |
| F3 locality | No existing `Locality*` types in repo (codebase-memory confirms 0 hits other than F3 itself) | Genuinely new |
| F4 burst_routing | No existing `BurstRouter` / `ResourceRequirement` types; reuses `prism_proto::NodeCapabilities` for the descriptor side | Genuinely new |
| F5 prism-audit | **`crates/core/src/audit.rs` already exists** (~630 lines, SQLite-backed, with server handler + dashboard page) | **PARTIAL OVERLAP — different purpose, same name `AuditLog`**. See note below. |

**F5 / core::audit reconciliation needed.** The two are conceptually complementary (operational audit vs cross-org Fabric audit) but:

1. Both define a type called `AuditLog`. Anyone importing both crates gets a name conflict.
2. A reader looking at "audit" in the dashboard doesn't know there's a second log.
3. The dashboard's `dashboard/src/pages/AuditLog.tsx` doesn't render Fabric envelopes.

Follow-up needed (not this PR): rename `prism-audit` → `prism-fabric-audit`, extend the existing dashboard page to surface Fabric envelopes alongside operational entries.

### MCPs in use this session

| MCP | What it provides | Used? |
|-----|------------------|-------|
| `pty-debug` | Local PTY text snapshots | Yes — verified PR #45 + #18 live |
| `terminalcp` | Full PTY emulation | Wired post-#45 but not yet driven |
| `iterm-tmux` (mcpretentious) | iTerm2 + tmux + layered screenshots | Used for initial `-list` / `-screenshot` |
| `tui-driver` | PNG screenshots via headless TUI driver | Installed post-#46, smoke-tested (`tui_list_sessions`) |
| `tui-test` | Playwright-style buffer-mode assertions | Installed post-#46, smoke-tested (`list_sessions`) |
| `codebase-memory-mcp` | Repo-indexed knowledge graph (16,075 nodes, 27,466 edges for PRISM) | **Now using** — used to find F5/audit duplication |

### What's still NOT tested (the gap list)

This is the list of things that need tests / live runs before the v1 done bar can claim "done":

1. **mDNS local discovery between two `prism node up --offline` processes** — Bug #19. peer_count=0 even with `--broadcast`.
2. **Real platform endpoint for F1c3 pubkey fetcher** — does `api.marc27.com/api/v1/federation/platform-pubkey` exist? PR #43 has a mock test only.
3. **Real cross-org request through the mesh transport** — F6 is in-memory. The actual `mesh::subscription` channel carrying a `CrossOrgRequest` between two nodes has no test.
4. **Kafka pub/sub between two nodes** — `tests/test_mesh_e2e.sh` exists but I have not run it (Docker daemon was down).
5. **Two-machine federation** — user offered their head server. Not yet attempted.
6. **Inference path with real model** — F6 returns a fixed string. No actual inference happens.
7. **The CLI/TUI agent's reaction to the new fetch error messages** (PR #42) — strings are in the binary but no live chat turn was driven through them.
8. **Security/exposure surface review** — what does `/api/status` leak? Are any tokens logged? No audit done this session.

### How to verify each PR independently

Don't trust this file. Each row above can be checked:

```bash
# Existence + content of a feature
gh pr view <num>                                                              # description
git show <commit>                                                             # actual code
mcp__codebase-memory-mcp__search_graph(project="...PRISM", name_pattern="X")  # symbol exists

# What was tested
cargo test -p <crate-name>                                                    # unit tests pass
cargo test --example <name> -p prism-mesh                                     # F6 demo runs

# What was NOT tested
grep -rn "TODO\|FIXME\|not yet tested" docs/SHIPPED.md                        # gaps live here
```

---

## Pre-2026-05-09 history

For context — what was already in main when today's work started:

- v2.7.0 native Ratatui-flavoured TUI shipped earlier
- PR #34 (forge_main test unflake), PR #35 (pen-test report — 0 exploitable / 7/15 Python CVEs patched), PR #36 (table column word-floor fix)
- F1 chunk 1 (PeerIdentity, CrossOrgRequest, verify_peer in `crates/mesh/src/federation.rs`) was merged before this session — see `git log` for the actual commit
- 5 TUI MCPs were wired by PR #40 but only 3 of them survived without manual install fixes (the others needed PR #46)

Use `git log --oneline --first-parent main` to trace earlier history; this file only owns 2026-05-09 and forward.
