# `agent-browser` integration

`agent-browser` (v0.34.0, Apache-2.0, pure-Rust CDP driver, installed at
`~/.local/bin/agent-browser`) is wired into PRISM as a **shelled-out external
binary**, exactly like `gh`/`git`/`hf`/`ollama`: `Command::new` from Rust,
never a linked crate. One shared read path serves both surfaces.

## Tool surface added

**Agent tool — `web_browse`** (`crates/agent/src/command_tools.rs`, spec in the
`COMMAND_TOOLS` registry, kind `CommandToolKind::WebBrowse`):

- args: `{ url: string }` (required, closed schema, `additionalProperties: false`)
- calls `agent-browser read <url>` — one URL per call; JavaScript renders
  before extraction, so JS-heavy pages come back as real text
- 60-second wall-clock budget (`kill_on_drop`, reported honestly as a timeout)
- `stdin` nulled, stdout/stderr piped, platform credentials stripped from the
  child env so a hostile page cannot exfiltrate them
- result is the standard CLI envelope (`root`/`invocation`/`success`/
  `timed_out`/`exit_code`/`stdout`/`stderr`), stdout+stderr bounded by
  `truncate_for_ui(…, CLI_ENVELOPE_STREAM_MAX_CHARS)` with an explicit
  `[Output truncated]` marker when cut

**TUI — three ways in, one implementation.** All TUI paths dispatch the `/browse`
slash command, which calls the *same* `command_tools::agent_browser_read` core
as the agent tool, so the human path and agent path cannot drift:

1. Command palette → `browse.open` ("Browse web page", Science category) opens
   a one-field URL form; submit sends `/browse <url>` to the backend
   (`crates/tui/src/command.rs`, `app.rs` `FormTarget::Browse`)
2. Typed directly: `/browse <url>` in the message box
   (`crates/agent/src/protocol.rs` `handle_browse_slash_command`)
3. `/browse` bare prints usage instead of guessing

The `/browse` transcript display is separately bounded at 30 000 chars
(`truncate_for_ui`) — a rendered page can be enormous.

## How absence is reported

If `agent-browser` is not on PATH, the spawn returns
`AgentBrowserOutcome::MissingBinary` and **both surfaces** print the same
shared message (`agent_browser_missing_message`): the binary is missing, the
page was **NOT fetched**, PRISM **does not fall back** to another fetch path,
and the install commands are named (`brew install agent-browser` /
`cargo install agent-browser`, plus one-time `agent-browser install`). Pinned
by `absent_agent_browser_is_reported_with_install_guidance`. No silent
degradation, no empty-string success.

## Real outcomes, always

Every failure mode is its own variant of `AgentBrowserOutcome` and is reported
as what it was: `MissingBinary`, `TimedOut { secs }` (child killed at the
window edge, `timed_out: true` in the envelope), `SpawnFailed`, `Completed`
with `success = exit_status.success()` — a non-zero exit keeps the child's
stderr. The one honest non-error edge — exit 0 with no extractable text — is
reported as exactly that ("exited 0 but returned no readable text"), never as
an empty success. Pinned by `spawned_child_outcomes_flow_through_unchanged`
and `a_hung_browser_is_reported_as_a_timeout` (both use temp scripts, no
network).

## Permission model

No new permission concept. Browsing reads the outside world, so `web_browse`
is classified like PRISM's other outward-facing READ tools
(`web_search`/`web_read` in `permissions.rs`):
`PermissionMode::ReadOnly`, `requires_approval: false`. It is dispatched
through the same `gate_command_execution` as every command tool — an
unverified HTTP caller fails closed (pinned), local dispatch is allowed. The
process-wide offline policy is checked *before* any child spawns
(`prism_runtime::offline::check_url`, loopback allowed); `Command::new("agent-browser")`
is registered in the `network_tools_are_offline_guarded` offline-marker list,
so the spawner stays provably offline-guarded.

## Relationship to the existing web/fetch tools

PRISM already has the Python `web` tool (`app/tools/web.py`: Firecrawl scrape
with httpx+BeautifulSoup fallback for `action='read'`, DuckDuckGo for
`action='search'`). `web_browse` **complements** it — it does not supersede it
and is not a silent duplicate. The `web_browse` description tells the model
when to choose which, and the rule is:

- **Default to `web` (action='read')** — cheaper, no external binary
  required, and only `web` can `search`.
- **Escalate to `web_browse`** when `web read` comes back empty or
  skeleton-only — i.e. the page needs JavaScript to render.
- Neither falls back to the other; each reports its own honest failure.

## What was NOT wired, and why

- **The interactive browser verbs** (`click`, `type`, `eval`, `snapshot`,
  `drag`, …). They *write* to the outside world — a click can submit a form,
  `eval` runs arbitrary JS in a page. A read-only tool must not smuggle a
  write surface through an argument; if act-on-the-page automation is ever
  wanted it deserves its own tool spec with its own approval gating. The
  exposed surface is exactly one verb: `read <url>`.
- **Screenshots/PDF export** (`screenshot`, `pdf`) — they produce binary
  artifacts the chat surfaces have no viewer for.
- **Session reuse / `connect`** — a stateful browser session is a capability
  the current approval model cannot describe; each `read` is a fresh,
  bounded, kill-on-drop run.
- **A linked-crate integration** — deliberately rejected per the standing
  decision (build speed, MSRV, fast-moving upstream).

## Licence position

Apache-2.0, invoked as a separate process via `Command::new`. Shelling out is
not linking, so there is no combination question and no licence text to
redistribute. No TEXT was copied from agent-browser (its help/skill output was
read for verification only); every prompt string, description and message in
this integration is PRISM-authored, so no `NOTICE` entry is required. If
agent-browser text is ever copied into the repo, it must be attributed in
`NOTICE` and marked as modified.

## Verification (gate)

```
$ cargo fmt --all ; echo $?
0
$ cargo test --workspace
test exit: 0
total passed: 3148  failed: 0        (baseline 3074 — higher, never lower)
$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 15.86s
clippy exit: 0
```

Tests added with this work (all fake/no-network — temp scripts, absent
binaries, offline env guard; nothing dials out):

- `command_tools::web_browse_tests` — registration/permission, URL required,
  real-invocation preview, absent-binary guidance, truncation marker,
  offline refusal, exit-0/exit-7 plumbing, hung-child timeout,
  executor-entry validation
- `protocol::browse_slash_tests` — dispatch fall-through, bare-`/browse`
  usage, offline refusal on the TUI path
- `app.rs` — `browse_requires_url_and_quotes_it`; palette entry + form
  snapshot re-blessed (`render_snapshots`)
- `network_tools_are_offline_guarded` — `Command::new("agent-browser")` added
  to the active offline markers

## Previous run vs this run

**Already present and verified correct** (previous run, cut off by quota —
this run changed none of it): the `web_browse` spec + executor + envelope +
outcome enum + all `web_browse_tests`, the `/browse` slash command and its
tests, the TUI palette entry / form / dispatch and snapshot, and the
offline-guard marker.

**Added by this run:**

1. Fixed the one gate failure: `command_tools.rs:2850` preview used
   `&[url.clone()]`, newly flagged by Rust 1.97 clippy
   (`cloned_ref_to_slice_refs`) — replaced with `std::slice::from_ref(url)`
   (identical output, no clone).
2. Ran the full gate and captured it (above).
3. Wrote this report, which the previous run left behind.

**Reachability, stated per the standing rule:** the agent calls it as the
`web_browse` tool; a TUI user reaches the same capability through the
`browse.open` palette form or by typing `/browse <url>`. Both surfaces share
one implementation (`agent_browser_read`) — 100% parity, no exit-to-CLI.
