# Plugin Audit — Pass 2: What PRISM should take from DeepSeek's harness, and what it should refuse

**Lens:** COPY THE CODE vs STEAL THE DESIGN vs REFUSE, per item.
**Read:** `deepseek-ai/deepseek-harness` (dsh, MIT, TypeScript) at
`~/.claude/jobs/bd21bca9/tmp/dsh` — the `compaction`, `spill`, `core/agent-loop`,
`core/tools`, `skill`, `guard`, `goal`, `hooks`, `sandbox`, `subagent`, `mcp`,
`plan`, `todo`, `llm` packages, source not READMEs. PRISM at
`~/Downloads/prism-unmuzzle` (branch `feat/annotate-not-refuse`) — both agent
loops (`crates/agent/src/agent_loop.rs`, `crates/ingest/src/paper_agent.rs`),
`transcript.rs`, `skills.rs`, `hooks.rs`, `subagent.rs`, `orchestrator.rs`,
`mcp.rs`, `ontologies.rs`, `crates/llm`.
Every claim below was verified in code; where I could not verify, I say so.
Date: 2026-08-16.

---

## 0. The verdict in one paragraph

DeepSeek's repo is two things fused together: a **plugin kernel** (cordis
services, 47 packages, declaration-merged event maps) and a set of
**battle-tested loop-survival mechanisms** (overflow recovery, balanced
compaction cuts, spill discipline, repeat-call guards). PRISM should take
almost nothing from the first and four or five specific things from the
second. The kernel exists because DeepSeek's customers extend the harness by
*writing code*; PRISM's customers extend it by *supplying an ontology
artifact*, and the Rust never changes — that is the business model, and a
code-plugin substrate would dilute it, not serve it. The loop mechanisms,
by contrast, address failures PRISM hit **this week**, one of them fixing a
latent bug PRISM still has (§3.2). Everything worth taking is a design plus
at most ~100 lines of Rust each; nothing worth taking is a package port.

An important correction to the framing in the brief: PRISM is not "behind
by 47 packages." The main agent loop (`crates/agent/src/agent_loop.rs`)
already has auto-compaction, token-pressure triggers, doom-loop detection,
spill-to-durable-memory with `recall()`, trajectory injection, a Tool-RAG
catalog with token budgets, MCP client, hooks, subagents with depth/budget
caps, and a fan-out orchestrator whose per-item truthful outcomes are
*better* than dsh's `tool-subagent` for ingestion campaigns. The gaps are
narrower than the package count suggests: they are concentrated in the
**ingest loop** (`paper_agent.rs`, which reimplements a primitive subset of
the main loop's protections) and in **recovery after the provider says no**
(neither PRISM loop can recover from a provider-confirmed context overflow).

---

## 1. PRISM's genuine extension points (compact — grounds the verdicts below)

Verified in code; pass 1 owns the exhaustive inventory.

| Surface | Swapped by supplying | Recompile? | Genuinely open? |
|---|---|---|---|
| Ontology (`crates/ingest/src/ontologies.rs:1-30`) | a promoted artifact in `.prism/ontologies/<id>.ttl` + `[ontology] id` in `prism.toml`; or a `dyn Ontology` impl via `register_ontology`/`replace_ontology` | No (artifact) / Yes (impl) | **Yes** — the register-refuses-taken-id / replace-refuses-free-id contract is real; this is the clearest plugin point and the business one |
| MCP servers (`crates/agent/src/mcp.rs:1-26`) | an entry in `~/.prism/mcp.json` (stdio spawn) | No | Yes — tools admitted as untrusted, approval-gated |
| Skills (`crates/agent/src/skills.rs:1-18`) | a JSON manifest (verified-by-execution) or a Markdown `SKILL.md` under `~/.prism/skills/` | No | Yes — both machine and human authored |
| Python tool plane (`app/tools/`) | a module with `create_*_tools` | No (Python) | Yes |
| External binaries (`gh`, `hf`, `ollama`, `agent-browser`) | a binary on PATH | No | Yes |
| Paper-agent tool surface (`paper_agent.rs:560` `paper_tools()`) | nothing — one hardcoded function | Yes | **No** — closed by design (deliberately small), and that is fine |
| Hooks (`crates/agent/src/hooks.rs`) | Rust registrations per session | Yes | Half-open: native only, no config-file hook surface |

## 2. Hardcoded where it should be pluggable — ranked by the "new domain, zero Rust edits" test

Only the items dsh's design actually speaks to; pass 1 ranks the full list.

1. **Per-ontology extraction guidance lives in Rust prompt text.** The system
   prompt in `paper_agent.rs:510-529` is affordance-only today (correct), but
   any future domain-specific reading guidance ("attend to processing
   parameters", in any language) has nowhere to live except Rust. dsh's skill
   shape — a described body loaded on demand, owned by a *provider* — maps
   cleanly onto the ontology artifact: an optional guidance block **in the
   artifact**, surfaced by the harness verbatim. That keeps domain knowledge
   in the ontology and passes the owner's test. (Design steal, §3.6.)
2. **Elision budget is a hardcoded char count** (`MAX_TOOL_RESULT_CHARS:
   24_000`, `paper_agent.rs:439`) instead of deriving from the routed model's
   context window — which PRISM already knows (`crates/llm/src/lib.rs:169`
   `context_window`, populated from GGUF metadata at `local.rs:413-420`).
   The 24k value was hand-fit to the 32k model that died; an 8k model dies
   again tomorrow. (§3.1.)
3. **Compaction summary shape is hardcoded heuristic** (`transcript.rs:285`) —
   counts and tool names, no model-written checkpoint. (§3.3.)

---

## 3. TAKE — ranked by consequence

### 3.1 Context-overflow **recovery** (catch the 400, shrink, retry once)

- **Theirs:** `packages/llm/llm/src/error.ts:51-86` — a provider-neutral
  classifier `isContextWindowExceededError()` (four regexes covering
  OpenAI-compatible wordings). `packages/compaction/compaction-basic/src/index.ts:179-223`
  — on `agent/request-error` with that code: compact with trigger
  `'context-overflow'` (bypasses the normal threshold, forces a useful
  reduction), then return `{kind:'retry'}`, bounded by `maxOverflowRetries`
  (default 1, `config.ts:93`), with a "did the surface durably shrink"
  generation check so a failed summarizer that still pruned something retries
  anyway instead of dying.
- **PRISM today:** prevention only. `elide_stale_tool_results`
  (`paper_agent.rs:462-491`) keeps the newest tool bodies under 24k chars —
  but assistant messages are never counted or elided, a single fat result plus
  prompt can still overflow, and when the provider rejects, `run_paper_agent_sample`
  propagates the error and **the whole run's proposals are lost** (`?` at
  line 322). The main loop has no overflow catch either (verified: no
  context-overflow classifier anywhere in `crates/llm`).
- **Consequence stated plainly:** today, one oversized request at turn 41 of
  a 56-turn run throws away 40 turns of recorded work. With this, the run
  shrinks itself and continues; worst case it retries once and returns what
  it has.
- **Verdict: COPY THE CODE for the classifier** — the regexes in
  `error.ts:51-86` translate to Rust `regex` almost token-for-token (~40
  lines). **STEAL THE DESIGN for the recovery loop**: in the ingest loop,
  wrap the `sample_with_tools` error; if classified, halve the elision budget,
  re-elide, retry once; if it still fails, return `output` with a new stop
  reason `Overflow` instead of `Err`. ~60-100 lines + tests. Do the same at
  the main loop's request site later.
- **MIT obligation:** copying the regexes = include DeepSeek's copyright +
  permission notice in PRISM's third-party notices (PRISM already has
  `NOTICE` and `LICENSES/`). Design has no obligation.

### 3.2 Tool-pairing-balanced compaction cuts — **fixes a live PRISM bug**

- **Theirs:** `packages/compaction/compaction/src/tool-pairing.ts:117-131` —
  a cut is legal only when no unanswered tool call crosses it;
  `compaction-basic/src/region.ts:120-127` walks the proposed retention
  boundary backward until balanced. Every compaction and prune in dsh goes
  through this check.
- **PRISM today:** `compact_history` (`agent_loop.rs:868-884`) splits at
  `history.len() - keep_last` **blindly**. If the split lands between an
  assistant message carrying `tool_calls` and its `tool` role replies, the
  kept transcript starts with tool results whose calls were deleted — an
  OpenAI-compatible provider rejects that request. I found no repair anywhere
  in `crates/llm` (grep for pairing/sanitize: only JSON-parsing helpers).
  The ingest loop avoided this exact trap deliberately — the comment at
  `paper_agent.rs:453-458` explains blanking-not-removing for this reason —
  which makes the main loop's blind split an inconsistency inside PRISM
  itself, not a theoretical risk.
- **Consequence:** with `keep_last=6` and multi-call turns, compaction at
  `agent_loop.rs:2345-2349` can produce a request the provider refuses,
  in a session that just crossed the token threshold — the worst moment.
- **Verdict: STEAL THE DESIGN.** ~30 lines of Rust: after computing
  `split_at`, advance it forward while `history[split_at].role == "tool"`
  (or scan for the owning assistant message). Do **not** port their
  incremental balance cache (`BalanceCache`, surface generations) — it exists
  because their surface is a mutable projection queried repeatedly; PRISM's
  is a `Vec` split once.
- **MIT obligation:** none (idea only).

### 3.3 Model-written structured checkpoint + prefix-cache-reusing summarizer

- **Theirs:** `compaction-basic/src/summarizer.ts:31-66` — the compaction
  instruction is a fixed 8-section template (Primary Request and Intent /
  Key Technical Concepts / Files and Code / Errors and Fixes / Pending Jobs /
  Current Work / Next Step / Critical Context), with three rules PRISM's
  heuristic summary lacks: preserve exact paths/commands/error
  strings/numbers; never mention the compaction; **merge a prior checkpoint
  instead of copying it forward** (the rule that stops checkpoint rot across
  repeated compactions). Delivery detail worth as much as the template: the
  summarize call replays the conversation's own system prompt + tools +
  messages and appends the instruction as the **final user message**
  (`summarizer.ts:24-30`, `region.ts:498-514`), so the call is a genuine
  prefix of the last request and **reuses the provider's KV cache** — the
  summary costs almost nothing extra on a warm local model. Guard at
  `region.ts:374-377`: a summary not strictly smaller than what it replaces
  is a failure, not a compaction.
- **PRISM today:** `transcript.rs:285-330` builds a heuristic summary
  (message counts, deduped tool names, keyword soup). It works but loses
  exactly what the template preserves: error strings, decisions, the next
  step. The ingest loop drops content entirely (correctly — see §5.1).
- **Verdict: COPY THE TEXT, STEAL THE DELIVERY.** The template is a prompt —
  copy it, changing the first line ("AI coding assistant" → task-neutral
  wording; keep the structure). Wire it as an optional model-written summary
  in `TranscriptStore::compact`, falling back to the current heuristic when
  no model is available. The prefix-reuse trick is free to adopt: build the
  summarize request from the same history you are about to compact plus one
  appended instruction message. ~80 lines. Adopt the smaller-than guard
  verbatim (it is one comparison).
- **MIT obligation:** copying the template text = notice entry, same as §3.1.

### 3.4 Repeat-call guard: advisory ladder before the veto

- **Theirs:** `packages/guard/repeat-tool-reminder/src/index.ts` — per-agent
  chain keyed on tool name + **canonicalized** (deep key-sorted) arguments
  (`:89-105`), escalating reminders at counts [3, 5, 8] (`:46`): gentle text
  first, then a detailed notice quoting the tool, run length, and capped
  arguments. Three judged details: denied calls are **counted** (a model
  hammering a denied call is exactly the loop worth breaking, `:186-189`);
  a user interjection **resets** the chain (`:229-232`); it never vetoes —
  it enriches the result with context and lets the model correct itself.
- **PRISM today:** doom-loop **veto only** (`agent_loop.rs:482-492`): three
  identical signatures → the call is aborted with "doom loop aborted". No
  canonicalization (key order changes the signature), no advisory tier, and
  the ingest loop has neither. Honest scoping: neither theirs nor PRISM's
  catches the actual 48-of-56-turns failure — that was *varied* search terms,
  not identical calls, and the fix that worked was telling the model its
  budget and batching rights in the prompt (`paper_agent.rs:504-529`), which
  PRISM already shipped. What the guard buys is the adjacent failure: the
  identical-call spiral, currently handled abruptly (veto) in one loop and
  not at all in the other.
- **Verdict: STEAL THE DESIGN**, ~80 lines of Rust shared by both loops:
  canonicalize args (serde_json `BTreeMap` round-trip gives key-sorting for
  free), advisory reminder at 3 and 5, keep PRISM's existing veto at the top
  of the ladder (theirs has no veto; PRISM's cost profile — every turn is
  billed — justifies keeping one). Reset on user message. Do not port the
  wildcard include/exclude config; two hardcoded sets suffice for two loops.
- **MIT obligation:** none if reimplemented; notice entry if the reminder
  strings are copied verbatim (they are two sentences — rewrite them).

### 3.5 Spill discipline — three details, not the seam

PRISM's main loop **already spills**: `process_large_result`
(`agent_loop.rs:361-380`) stores the full oversized result in durable memory
and replaces it with an 8k head + "call `recall(query=…)`" notice. That is
dsh's `spill-policy` in substance. Three of their details are worth folding in:

1. **The replacement must never exceed the advertised cap** — dsh *reserves
   the notice's byte cost inside the budget* before slicing the preview
   (`spill-policy/src/index.ts:171-187`). PRISM appends its notice after the
   8k preview, so the replacement exceeds 8k by the notice length. Cosmetic
   today; adopt the reservation when touching the function.
2. **Head + tail, not head only** (`spill-policy/src/index.ts:95-102`,
   `TextRetainer headTail`): the tail of a long tool result is where totals,
   "N more rows", and trailing errors live. PRISM keeps only the head.
   ~10 lines.
3. **Best-effort invariant, stated and tested:** a spill/storage failure must
   never convert a successful tool call into an error or hide the inline
   result (`spill/src/index.ts:42-44`, policy `:155-160`). PRISM appears to
   behave this way but has no test pinning it. One test.

- **Verdict: STEAL THREE DETAILS.** REFUSE the `SpillStore` abstract seam —
  PRISM already has two fit stores (durable memory in the agent loop; the
  paper text itself in the ingest loop, where "re-read if still needed"
  *is* the locator-and-refetch pattern with the paper as the store). A third
  abstraction would be symmetry, not capability.

### 3.6 Skill catalog + on-demand loader — the minimum version

- **Theirs:** two-part pattern. (a) A durable session **catalog** — one line
  per skill, name + ≤500-char description (`tool-skill/src/index.ts:27,
  34-58`); (b) one `skill(name)` tool that loads the full body on demand,
  rendered inside `<skill_content>` with a resource-base hint
  (`skill/src/index.ts:171-184`). Progressive disclosure: the model always
  sees *that* a skill exists, pays for the body only when it loads it.
  Provider layering with ranks (`skill/src/index.ts:74-83`) merges bundled /
  project / user skill sources.
- **PRISM today:** `skills.rs` already stores both executable
  (verified-by-execution) and Markdown skills under `~/.prism/skills/` with
  `list_skills` / `find_tools` / `run_skill`. So the storage and execution
  halves exist. The difference is surfacing: PRISM's skills reach the model
  through Tool-RAG selection (probabilistic), not through an always-visible
  catalog line (deterministic).
- **For `agent-browser` specifically:** the minimum version is **zero Rust**.
  Write `~/.prism/skills/agent-browser/SKILL.md` whose body says to run
  `agent-browser skills get core --full` and follow what it prints. PRISM's
  existing Markdown-skill path (skills are instructions, execution still goes
  through the ordinary tool path with hooks/approval, `skills.rs:12-18`)
  already carries this. Do that this week; it is a file, not a feature.
- **For the ingest loop:** do **not** add a skill system. The one legitimate
  skill-shaped need there is per-domain extraction guidance, and the owner's
  rule dictates where it lives: **in the ontology artifact**. Steal the
  *shape* — an optional described guidance block the harness surfaces
  verbatim (catalog line in the system prompt, body on demand via a
  `read_ontology_guidance` tool or inlined if short) — with the artifact as
  the provider. That extends `Ontology` (one optional method, default
  `None`), changes no domain vocabulary in Rust, and passes the
  "non-materials, non-English ontology with zero Rust edits" test. Medium
  cost (~half a day), high alignment with the moat.
- **Verdict: STEAL THE PATTERN, refuse the registry.** No ranks, no
  providers, no invocation-policy lattice — one directory and one artifact
  hook.

### 3.7 Non-destructive context accounting — the principle, sized honestly

Everything dsh does to a transcript is an **event on an append-only log**:
compaction appends `compaction/start`/`summary`/`end` plus a replacement
message that *shadows* the old range (`compaction/src/types.ts:16-90`);
prunes append a shadow-price record (`compaction-tool-result-pruner/src/index.ts:162-166`);
the summary event records **which provider/model wrote the summary and what
it cost** (`types.ts:40-52`). Nothing the model saw is ever unrecoverable,
and "which model wrote this checkpoint" always has an answer.

PRISM preaches exactly this — provenance is the moat, "every line
accountable" is the ESA bar — but its own agent loops mutate destructively:
`elide_stale_tool_results` overwrites bodies in place, `compact_history`
deletes messages; the `PaperAgentTrace` records tool calls but not elisions,
so "what did the model actually see at turn 40" is unanswerable after the
fact. That question is precisely what you needed this week when debugging
the 56-turn death.

- **Verdict: STEAL THE PRINCIPLE, not the machinery.** Do not build an
  event-sourced session store. Minimum honest version: when eliding or
  compacting, append a record to the existing trace (`PaperSampleTrace` /
  the agent-run ledger) saying what was blanked or summarized (turn, which
  tool results, char/token estimate, and — for a model-written summary —
  provider and model). ~40 lines across both loops. That closes the audit
  gap for the price of a struct.

### 3.8 Smaller items worth one look each

- **Timeout policy** (`guard/timeout-policy/src/index.ts:56-80`): tools
  declare `timeoutMs`; a wrapper arms the deadline, maps expiry to a
  structured `TOOL_TIMEOUT` result, never abandons the tool promise. If
  PRISM's Python-sidecar calls lack per-tool budgets, steal the shape (the
  structured error code the model can route on, rather than a generic
  failure). Small.
- **Todo tool description semantics** (`todo/tool-todo/src/index.ts:44-67`):
  whole-list-replacement per call ("send the ENTIRE list; it REPLACES") —
  measurably more reliable for models than incremental edits. If PRISM ever
  exposes a task list to a loop, use that shape; the description text is the
  asset. No package needed.
- **Goal vocabulary** (`goal/goal/src/domain.ts:14-21`,
  `goal-round-driver/src/index.ts:166-171`): a goal as a durable object with
  `create/edit/pause/resume/complete/block/clear`, `maxGoalRounds`, and
  typed block reasons (`round-limit`, `queue-failed`). This matches the
  owner's standing "goal = persistent object w/ progress+cost" direction —
  take the **state vocabulary** when building goal-driven research. REFUSE
  the driver implementation wholesale: its 445 lines are race fences for
  their inbox/steering concurrency model and are meaningless in PRISM's
  synchronous loops.
- **Bounded parallel tool execution** (`core/agent-loop/src/tool-calls.ts`):
  per-tool `exclusive | parallel` modes, a rolling pool capped at 10
  (`constants.ts:6`), results committed in model order, synthetic error
  results for calls skipped on abort so replay stays valid (`:249-258`).
  For the **ingest** loop this buys nothing — its tools are microsecond
  in-memory lookups; sequential is fine. For the **research** loop, where a
  turn can emit several multi-second searches, a `parallel_safe` flag on
  `LoadedTool` plus a `JoinSet` with model-ordered commit is a real latency
  win. Medium cost (~200 lines), medium value — do it when research latency
  is the complaint, not before. The one detail to keep whenever it happens:
  synthetic results for skipped calls (their insight that an aborted batch
  must still answer every `tool_call_id`).

---

## 4. REFUSE — and why, concretely

1. **The cordis plugin kernel and the 47-package factoring.** Services,
   fiber lifecycles, declaration-merged `SessionEventMap`/`Context`
   interfaces, per-package `invariant.ts`, schemastery config schemas. This
   is the load-bearing 60% of their repo and the wrong thing for PRISM. It
   exists so *deployers extend the harness by writing TypeScript packages*.
   PRISM's extension contract is the opposite and is the moat: **customers
   supply artifacts (ontologies), and the Rust never changes.** A code-plugin
   substrate would invite exactly the drift the owner's rule forbids — the
   moment domain behavior can be patched in via a plugin, it will be, and
   the "promote a non-materials ontology with zero Rust edits" test starts
   failing in spirit while passing in letter. Port cost would be months;
   strategic value negative.
2. **The subagent provider family** (11 packages: ACP, Codex, Claude Code,
   dsh-SDK children, continuable Activations, control/report tools). Driving
   third-party coding agents as children is their product requirement.
   PRISM's `orchestrator.rs:1-50` already does bounded fan-out with
   budget-reserved spawns, per-item truthful outcomes in input order, and
   schema-verified results with exactly-one repair — a design *better
   fitted* to ingestion campaigns than dsh's generic delegation. Nothing to
   take here except the reassurance that PRISM's shape is sound.
3. **Hooks bridges** (`hooks-claude-code`, `hooks-codex`): adapters that run
   *other products'* `hooks.json` shell hooks. PRISM's hooks are native Rust
   registrations; it has no Claude Code hook files to honor. The shared
   `hook-protocol` library solves a problem PRISM does not have.
4. **Per-model compaction policy tables and the settings plane**
   (`compaction-basic/src/config.ts` — 310 lines of override merging,
   duplicate detection, ratio validation; `dsh-settings` namespaces). This
   is deployment-product surface for a tool with thousands of
   configurations. PRISM needs the **numbers** (threshold ≈ 0.8 × window,
   retain ≈ 0.16 × window verbatim, `config.ts:20-23`) and one env var, not
   the table.
5. **The `SpillStore` capability seam** — see §3.5. PRISM has two stores
   already; an abstract third is over-engineering.
6. **Plan-mode and todo as session-event packages.** PRISM has plan mode in
   its TUI plane and task tracking; dsh's versions are session-event folds
   over their log architecture, which PRISM is not adopting (§3.7 takes the
   accounting principle only). Take the todo *description text* (§3.8),
   refuse the packages.
7. **`sandbox` / `e2b` / `code-runtime`.** Local-process confinement and
   remote execution for a coding agent. PRISM's compute story (AENV sandbox
   runtime, JIT broker) owns this layer with different requirements
   (billed-to-user, nested-virt); their macOS/Windows-ACL confinement
   backends solve a different problem. Also `lsp`, `terminal`, `acp`,
   `web`, `client`, `host`, `identity`, `credentials`, `feedback`,
   `runtime-diagnostics` — coding-IDE surface, no PRISM counterpart needed.
8. **`typert` / brand / invariant ceremony.** Branded string types and
   runtime invariant modules recreate what Rust's newtypes and `Result` give
   natively. Porting would be motion without progress.
9. **Skill provider layering** (rank-merged registries, `BUNDLED_SKILL_RANK
   = 600`, collect caches). One directory plus one ontology hook covers
   PRISM's need (§3.6).
10. **A domain-vocabulary caution on everything copied verbatim:** dsh ships
    English coding-agent prose as package constants — the compaction template
    opens "…for this AI coding assistant", reminder strings reference coding
    workflows, `context/` packages inject workspace instructions. Any text
    taken into PRISM's extraction path must be re-audited for domain and
    language neutrality, or it becomes exactly the hardcoded vocabulary the
    owner's rule bans. The §3.3 template survives this audit with a one-line
    edit; take nothing else verbatim into ingest prompts.

---

## 5. Direct answers to the dispatch's questions

### 5.1 Does their compaction/spill handle things `elide_stale_tool_results` cannot?

Point by point:

- **Summarizing rather than dropping: yes** — but note where it matters.
  For the *ingest* loop, dropping is actually correct: every blanked tool
  result is an excerpt of the paper, the paper is still in the workspace,
  and re-fetching is one `read_paper` call — nothing is lost, which is
  something dsh's spill design would merely re-implement with extra steps.
  Where summarizing matters is the *research/main* loop, where dropped
  content (decisions, error strings) is not re-fetchable. That is §3.3.
- **Spilling to storage and re-fetching: PRISM already does this** in the
  main loop (`process_large_result` → durable memory → `recall()`); the
  ingest loop's "re-read if still needed" is the same pattern with the paper
  as the store. Take the three details in §3.5, not the seam.
- **A budget rather than a character count: yes, and this is the sharpest
  gap.** dsh prices the whole surface with a token meter against the routed
  model's `contextWindow` (threshold 0.8×, retained tail 0.16×) and, when
  wrong, *recovers from the provider's own overflow verdict* (§3.1). PRISM's
  ingest loop counts only tool-body chars against a constant hand-fit to one
  model, counts assistant messages not at all, and cannot recover when
  wrong. The proportionate fix is not a token meter: derive the elision
  budget from `LlmClient::context_window` (already available) with a chars≈
  4×tokens rule (~20 lines), and add the §3.1 recovery for when the estimate
  is still wrong. Estimation can be sloppy once overflow is recoverable —
  that is the actual lesson of their design.
- **Two things theirs has that were not asked about and matter:** the
  tool-pairing balance rule (§3.2 — PRISM has the bug it prevents) and the
  never-shrink-into-a-bigger-summary guard (§3.3).

### 5.2 `core/agent-loop` vs `run_paper_agent_sample`

- **Turn budgets: theirs has none.** The dsh inner loop runs until the model
  stops calling tools, a tool marks `concludesTurn` (their `finish`
  equivalent, `tools/src/index.ts:565`), max-tokens, abort, or error.
  Bounded-ness lives a layer up: goal rounds (`maxGoalRounds`) and token
  pressure via compaction. PRISM's line-scaled turn budget
  (`turn_budget_for`, `paper_agent.rs:44-46`) plus the budget-and-batching
  prompt is a *domain-appropriate* design dsh simply lacks — cost per turn
  is the binding constraint in ingestion, and PRISM measured its shape
  working. **Keep PRISM's; do not import theirs.** The one idea worth
  filing: when goal-driven research lands, budget *rounds* at the objective
  level and let context management (not the clock) handle turn waste —
  that is their split.
- **Tool-call batching:** theirs schedules model-emitted parallel calls in a
  bounded pool with per-tool modes and model-ordered commits; PRISM executes
  batches sequentially in both loops. For ingest: refuse (pure in-memory
  tools, no win). For research: worth stealing later (§3.8).
- **Stop conditions:** equivalent in kind (`finish` ≈ `concludesTurn`;
  budget ≈ nothing of theirs; PRISM's no-tool-call nudge at
  `paper_agent.rs:340-349` vs their "no calls = completed" — PRISM's choice
  is deliberate and correct for a reader that must be pushed to continue).
  The one superior mechanism of theirs is the `agent/request-error`
  waterfall giving *any* policy a vote on retry (`agent.ts:355-371`) — in
  PRISM's world that is simply "classify the error before propagating,"
  which §3.1 covers without the event system.
- **What PRISM's ingest loop has that theirs does not:** the
  citation-was-read gate (`paper_agent.rs:1176-1216`) — a proposal must cite
  lines returned in an *earlier* turn. That is a domain-integrity guard dsh
  has no counterpart for. It is worth protecting during any refactor.

### 5.3 Skills — is the pattern real, and the minimum version?

Real — DeepSeek, Anthropic, and `agent-browser` converged on the same shape
(described catalog always visible; body loaded on demand; resources resolved
relative to the skill). PRISM already owns 80% of it in `skills.rs`. Minimum
version: (a) the `agent-browser` SKILL.md file, zero Rust, this week (§3.6);
(b) the ontology-guidance hook when a customer ontology first needs
extraction guidance — artifact-provided, catalog-line + on-demand body,
one optional trait method. Refuse everything else in their skill family.

---

## 6. Consolidated action list (effort-honest)

| # | Action | Size | Kind | MIT notice? |
|---|---|---|---|---|
| 1 | Overflow classifier + retry-once recovery in ingest loop; stop reason `Overflow` instead of lost run | ~100 lines | Copy regexes + steal design | Yes (regexes) |
| 2 | Fix `compact_history` split to respect tool-call pairing | ~30 lines | Steal design (bug fix) | No |
| 3 | Derive elision budget from `context_window` instead of 24k const | ~20 lines | Steal design | No |
| 4 | Model-written checkpoint template + prefix-reuse + smaller-than guard in `TranscriptStore::compact` | ~80 lines | Copy template text + steal delivery | Yes (template) |
| 5 | Advisory repeat-call ladder (canonicalized args) under existing veto, both loops | ~80 lines | Steal design | No |
| 6 | Spill details: notice-inside-cap, head+tail, best-effort test | ~30 lines | Steal details | No |
| 7 | `agent-browser` SKILL.md | 0 lines Rust | File only | No |
| 8 | Elision/compaction events into the existing trace (audit answerability) | ~40 lines | Steal principle | No |
| 9 | (Later) ontology-artifact guidance hook | ~½ day | Steal pattern | No |
| 10 | (Later, if research latency hurts) parallel-safe tool pool with ordered commit + synthetic aborted results | ~200 lines | Steal design | No |

Items 1-3 are the same incident (this week's context death and its
neighbors) and together are roughly one day. Nothing on this list is a
package port; nothing requires cordis, event sourcing, or a new crate.
Everything refused in §4 stays refused even if it looks free — the cost of
their factoring is paid in ownership and drift, not in lines.
