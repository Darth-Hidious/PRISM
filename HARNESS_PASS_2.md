# HARNESS_PASS_2.md — Early-stop and weak-model robustness, read out of three coding-agent harnesses

Pass 2 of 4. Read-only pass; this file is the only artifact written.
Harnesses read: **Codex** (`~/.claude/jobs/bd21bca9/tmp/codex-src`, Apache-2.0,
weighted heaviest), **dsh** (`…/tmp/dsh`, MIT), **Grok CLI** (`…/tmp/grok`, MIT,
never covered by a prior pass). PRISM side read: `crates/ingest/src/paper_agent.rs`,
`crates/ingest/src/text_extract.rs`, `crates/agent/src/agent_loop.rs`,
`crates/llm/src/overflow.rs`, `crates/llm/src/lib.rs`, `crates/cli/src/main.rs`,
`crates/cli/src/papers.rs`, plus `PLUGIN_AUDIT_PASS1/2.md` and `READABILITY.md`.

**The frame, stated once up front.** All three harnesses are coding agents:
every "retry until it works" mechanism they own leans on a ground-truth
verifier that fires every turn — the compiler, the test suite, or a human who
can look at the diff. PRISM's paper loop has none of that; its verifier must be
constructed. Where this report proposes a pattern, it names what plays the role
of the compiler. Where a pattern only works *because* a compiler exists, the
report says it does not transfer.

**What was already known.** `PLUGIN_AUDIT_PASS2.md` §6 consolidated 10 actions
from dsh and Codex; items 1–3 are done and I verified them in-tree (overflow
classifier + retry-once recovery at `paper_agent.rs:353-372`; tool-pairing-safe
`compact_history` at `agent_loop.rs:868-902`; elision budget derived from
`context_window` at `paper_agent.rs:500-511`). Items 4–10 are open. This pass
goes beyond that list in §B, §D, and the Grok material, and re-ranks in §E.
The five given defects are treated as given; I only report when a harness
solves one better than the planned fix (§E notes).

---

## A. Failure taxonomy (brief)

One row per failure class; `file:line` in each tree; "PRISM?" = equivalent in
`paper_agent.rs` (PA) or `agent_loop.rs` (AL).

| Failure | dsh | Codex | Grok | PRISM |
|---|---|---|---|---|
| **Context overflow** | classifier `packages/llm/llm/src/error.ts:50-84` (PRISM copied it: `crates/llm/src/overflow.rs:1-14`); recovery `packages/compaction/compaction-basic/src/index.ts:179-223` — compact with trigger `context-overflow`, retry bounded by `maxOverflowRetries`, and a *surface-generation* check (`:216-221`: only retry if the surface actually changed) | classifier is a single exact code check `codex-api/src/sse/responses.rs:671-673` (`error.code == "context_length_exceeded"`) — **weaker than PRISM's**; recovery is structural: pre-turn compact (`codex-rs/core/src/session/turn.rs:994-1023`), mid-turn roll-over compact (`turn.rs:442-468`), and if *compaction itself* overflows it drops the oldest history item and retries (`codex-rs/core/src/compact.rs:309-321`) | loose regex `src/agent/agent.ts:2781-2784`; recovery once, and only when nothing has streamed yet (`agent.ts:2128-2137`, batch mode `:1766` requires `turnMessages.length === 0`), then retry with halved keep-recent budget (`src/agent/compaction.ts:342-347`) | PA: done, `paper_agent.rs:353-372` (retry once, halve budget, keep proposals, stop reason `Overflow`). AL: **none** — `agent_loop.rs:2110-2116` propagates any LLM error verbatim |
| **Repeated identical tool call** | advisory ladder at counts [3,5,8], canonicalized args, denied calls counted, user message resets: `packages/guard/repeat-tool-reminder/src/index.ts:46,89-105` (PASS2 §3.4) | none in-loop; relies on user + doom is visible in UI | none | AL only: 3-identical veto, no canonicalization, `agent_loop.rs:482-491,2796-2823`. PA: none |
| **Tool errors** | tool returns structured result; loop never dies on it (PASS2 §3.8 `timeout-policy`: structured `TOOL_TIMEOUT`, `packages/guard/timeout-policy/src/index.ts:56-80`) | two-valued error type `RespondToModel`/`Fatal` (`codex-rs/tools/src/function_call_error.rs:5-10`); `RespondToModel` becomes the tool output and the loop continues; arg-parse failure is `RespondToModel` (`codex-rs/core/src/tools/handlers/mod.rs:83-90`) | `{success:false, output:"Tool X failed: …"}` returned to model, never thrown (`src/agent/agent.ts:1007-1026`) | PA: every tool returns `PaperToolOutcome::failure` with *instructive* text (`paper_agent.rs:1533-1541`); unknown tool `:1379-1382`. AL: doom/empty-streak advisories `agent_loop.rs:2801-2845` |
| **Model stops early** | **nothing** — no tool calls = completed (`packages/core/agent-loop/src/agent.ts:394`) | **stop hooks**: an external program may veto the stop and force continuation with its own reason (`codex-rs/hooks/src/events/stop.rs`, decision at `codex-rs/core/src/session/turn.rs:484-518`) — see §B | nothing; `stepCountIs(maxToolRounds)` ends silently in stream mode (`agent.ts:1947`); batch mode at least announces it (`agent.ts:1744-1756`) | nothing gates `finish` (`paper_agent.rs:1369-1377`); only a prompt and a no-call nudge (`paper_agent.rs:390-402`) — see §B |
| **Malformed tool argument** | schema validation at tool boundary (typert/zod shapes per tool) | typed deserialize; failure text goes to the model (`handlers/mod.rs:83-90`) | `JSON.parse` catch → `received invalid JSON arguments: …` (`agent.ts:987-996`) | PA: records `{"unparsed": raw}` in trace and returns failure (`paper_agent.rs:428-436`) — the trace-preserving version is **better than all three** |
| **Truncated stream / transport** | `agent/request-error` waterfall, any policy votes retry (`agent.ts:354-371`) | explicit retryable taxonomy (`codex-rs/protocol/src/error.rs:363-401`); retry ladder with backoff+jitter (`codex-rs/core/src/util.rs:86-91`), connection backoff 5s→60s, WS→HTTPS transport fallback that resets the counter (`codex-rs/core/src/responses_retry.rs:18-19,64-106`), user-visible "Reconnecting… n/m" (`:117-127`) | `maxRetries: 0` (`agent.ts:1948`) — **no transport retry at all**; turn dies on the first stream error | none in either loop; honest propagation |
| **Run exceeds budget** | goal-level: `maxGoalRounds` → typed block `round-limit` (`packages/goal/goal-round-driver/src/index.ts:166-171`); no turn budget (PASS2 §5.2) | weighted-token rollout budget; threshold reminders injected as developer messages; `SessionBudgetExceeded` when exhausted (`codex-rs/core/src/rollout_budget.rs:47-94`, `codex-rs/core/src/session/rollout_budget.rs:29-40`) | `MAX_TOOL_ROUNDS = 400` (`agent.ts:102`); subagents capped lower (`agent.ts:1317,1329`); overage keeps the turn's work and reports error (`agent.ts:1744-1756`) | PA: line-scaled turn budget (`paper_agent.rs:44-46`), proposals kept on exhaustion (`:464-469`). AL: `max_iterations` → "[Agent reached maximum iterations]" (`agent_loop.rs:2983`) |
| **Abort/interruption** | abort → structured `turn/end` reason (`agent.ts:303-305,320-322`) | synthetic tool outputs so an aborted batch still answers every call id ("aborted by user after N.Ns", `codex-rs/core/src/tools/parallel.rs:257-279`); post-interrupt guidance injected next turn (`codex-rs/core/src/context/turn_aborted.rs:6-8`) | aborted turn's user message is *removed* so partial state doesn't persist (`agent.ts:864-869`) | PA: n/a (synchronous). AL: n/a here |

Two cross-cutting verdicts:

1. **On overflow, PRISM's classifier is the best of the four** (five
   conservative regexes, `overflow.rs:19-71`, vs Codex's exact-code check and
   Grok's regex that also matches output-token failures like
   "maximum output tokens exceeded" — `agent.ts:2783` matches `token…limit`).
   Defect 2 is a coverage gap in an otherwise sound design; extend it, do not
   replace it.
2. **On transport retry, PRISM is the worst of the four.** Neither PRISM loop
   retries a transient stream failure. That is defensible for a batch ingest
   run *if* the proposals survive the failure (defect 4) — today they do not,
   which makes every dropped connection a silent total loss. Fixing defect 4
   matters more than adding retries, and is cheaper.

---

## B. Early-stop and giving up (exhaustive)

This is PRISM's worst failure: a model calls `finish` after 3 turns on a
40-page paper, `stop_reason` records `Finish`, and the run is
indistinguishable from a genuinely quiet paper. What the three harnesses do
about this class of failure, in descending order of relevance:

### B.1 Codex: the stop is gated by a program, not by the model

Codex's only early-stop mechanism is the **stop hook**
(`codex-rs/core/src/hook_runtime.rs:302-352`, event contract in
`codex-rs/hooks/src/events/stop.rs`). When the model is about to end a turn,
user-configured hook programs run; any one may return
`{"decision":"block","reason":"retry with tests"}` (or exit 2 with the reason
on stderr — `stop.rs:291-312`). The loop then refuses the stop, sets
`stop_hook_active = true`, and injects the reason as a continuation prompt
(`codex-rs/core/src/session/turn.rs:491-508`). A block without a reason is a
hook failure, not a stop (`stop.rs` tests at :455-481). Aggregation across
hooks: an explicit `continue:false` overrides any block (`stop.rs:483-506`).

Read structurally, this is the most important idea in all three trees for
PRISM: **Codex does not trust the model's self-report of doneness; it hands
the stop decision to a program that knows what "done" means.** For a coding
agent that program is usually "run the tests, block if red." The model's
`finish` is a *request*, and the harness decides.

What does NOT transfer: the hook is external, interactive, and
user-supplied. PRISM's paper ingest is unattended batch work over arbitrary
customer domains; there is no user to write a hook per paper, and encoding
"what done means" in a script is exactly the hardcoded-domain move the
standing rules forbid. **But the shape transfers perfectly: `finish` should be
a request that a deterministic, domain-free checker can reject once.** PRISM
does not need a hook system; it needs the checker Codex's hook would run,
written once, because for a paper "done" has exactly one domain-free meaning:
*the document was read*.

Note also what Codex has *instead* of an intrinsic completeness check:
nothing. Without hooks configured, a Codex turn ends the moment the model
stops calling tools — identical to dsh (`agent.ts:394`) and Grok. The frontier
model is the assumed verifier. That assumption is precisely what dies on a 12B
model.

### B.2 dsh and Grok: nothing — and Grok's verify subsystem confirms the frame

dsh: no tool calls ⇒ `{kind:'completed'}`
(`packages/core/agent-loop/src/agent.ts:394`). There is an
`agent/turn-stopping` dispatch point (`agent.ts:296`) where policies can
observe the stop, but no shipped policy uses it to demand more work. The only
giving-up machinery is at the *goal* level: `maxGoalRounds` produces a typed,
loud block reason (`round-limit`, `queue-failed`,
`packages/goal/goal-round-driver/src/index.ts:166-171,199-200`). That
vocabulary — *give up loudly with a typed reason* — transfers (see B.5).

Grok: same termination semantics (`stopWhen: stepCountIs(this.maxToolRounds)`,
`src/agent/agent.ts:1947`; budget exhaustion is at least announced,
`:1744-1756`). But Grok is the only harness that *builds a verifier*:
`src/verify/` detects how this repo is built/tested (`orchestrator.ts:48-80`),
checkpoints a clean state, runs the verification, and its retry policy is
explicitly anti-thrash: "at most one bounded retry per known failure class…
If a failure is not covered by a known retry strategy, report it directly
instead of guessing" (`src/verify/retry.ts:48-62`). The transferable sentence
is the last one: **when the verifier cannot certify, report; do not improvise
and do not pretend.** For a paper, the verifier is cheaper than Grok's — the
document is already in memory — which leads to the concrete proposal.

### B.3 The compiler-analogue for "did we read this paper" — and PRISM already maintains it

The paper loop keeps `previously_read_ranges`, merged and exact
(`paper_agent.rs:340`, `returned_paper_ranges` at `:1224-1244`,
`merge_ranges` at `:1246-1259`). That structure is a **deterministic,
model-free record of which lines the model has ever seen**. Against
`workspace.lines.len()` it yields document coverage — the research analogue of
"the tests ran." It costs nothing to compute, cannot lie, and is
domain-neutral (lines, not content). No harness has anything equivalent,
because a coding agent's ground truth is execution, not coverage of a source;
PRISM's accident is that its ground truth *is* a source document, and it
already tracks reads of it.

Three uses, in increasing order of intervention:

**(B.3a) Inform the finish decision — zero risk, do it regardless.**
`finish` currently answers with three counters
(`paper_agent.rs:1369-1377`). Add to that result: total lines, read lines,
coverage fraction, and the *unread ranges themselves* (bounded list). The
model then decides with the same information a reviewer would have. This is
the Grok/Codex "tool result is feedback" principle applied to stopping.
~15 lines. Cannot false-positive because it decides nothing.

**(B.3b) Reject a premature `finish` ONCE — the actual fix.**
When `finish` is called and coverage is below a floor, do not stop: return a
tool failure that names the largest unread ranges ("finish refused: 312 of 667
lines were never read; largest unread: 88-240, 301-520. Read and propose, or
call finish again to accept partial coverage") and let the loop continue. The
second `finish` always succeeds.

- **What plays the compiler:** `previously_read_ranges` vs line count — the
  same structure the citation gate already trusts (`citation_was_read`,
  `paper_agent.rs:1261-1266`).
- **Bounded, like PRISM's existing gates:** the main loop already does
  exactly this "reject the stop, at most N times, then accept" shape in the
  execution-contract gate (`agent_loop.rs:2243-2288`,
  `MAX_CONTRACT_GATE_FIRINGS = 2` at `:141`). This is that gate's sibling for
  the ingest loop. One-shot rejection caps the worst case at one extra
  model call per paper.
- **False positives, honestly:** a paper whose extractable content is
  concentrated in one section; a review paper the model correctly skims; a
  paper genuinely outside the active ontology's vocabulary. The one-shot
  design makes these cost exactly one turn: the model calls `finish` again and
  is believed. The *second* finish should stamp the trace
  (`stop_reason: Finish` plus `coverage_at_finish`) so a reviewer sees the
  accepted risk. Today the false-negative cost is a silent 4-fact paper that
  looks like success; the asymmetry overwhelmingly favors the gate.
- **What this is not:** it is not a fact quota ("propose at least N facts"
  would be domain-dependent and would manufacture hallucinations — a model
  told it must produce more will produce false ones). Coverage measures
  *reading*, which is the precondition for extraction and is independent of
  how many facts the domain yields. A 40-page paper read at 90% that yields 4
  facts is honest; a 40-page paper read at 6% that yields 4 facts is the
  measured failure.
- **Floor choice:** not a domain claim — a reading-standard claim. Store it in
  config (default suggested: 0.25 for the reject-once gate, i.e. finishing
  having read under a quarter of the document is challenged once). The value
  belongs in `prism.toml`, not Rust logic, and the mechanism must work with
  any value including 0 (off).

**(B.3c) Coverage-weighted multi-sample agreement — fixes defect 3's worst
consequence.** Today `keep_recurring_cited_facts`
(`text_extract.rs:582-618`) counts a fact's passes across samples and marks
shortfalls as `SampleDisagreement`. If one sample died on `Overflow`
(`paper_agent.rs:358,369`) or finished at 8% coverage while the healthy
sample read the paper properly, the healthy sample's *correct* facts are
annotated as disagreement — the harness punishes the good sample for the bad
one's early stop. Fix: carry `stop_reason` + coverage into the trace
(already serialized; `PaperAgentTrace` has `stop_reason`), and in
`keep_recurring_cited_facts` count only *comparable* samples (stop reason
`Finish` at acceptable coverage, or `Budget`-exhausted samples which at least
spent their turns) toward the agreement denominator; a sample that stopped
early on overflow or refused-finish is excluded from the denominator and
reported as such. This is the first real reader of
`PaperAgentStopReason::Overflow` (defect 3), and it turns "which samples to
trust" into a typed, recorded decision instead of a silent blend. ~40-60 lines
in `text_extract.rs` + plumbing.

### B.4 The adjacent early-stop: no tool calls at all

PRISM's nudge ("Use the available tools to continue reading, or call finish
explicitly", `paper_agent.rs:390-402`) is already *better* than all three
harnesses (dsh/Grok treat silence as completion; Codex defers to hooks). Keep
it. One measured improvement from dsh's sticky `max-tokens` handling
(`agent.ts:289-292,391`): degraded stop reasons must be sticky — once a run
has hit overflow or budget, a later clean turn must not upgrade the recorded
reason. PRISM's loop already returns early on `Overflow` so this is satisfied
by construction, but the future `Error` stop reason (§E-7) must follow the
same rule: once set, never upgraded.

### B.5 Giving up loudly — the vocabulary PRISM is missing

All three harnesses, when a run cannot continue, produce a *typed, recorded*
reason: dsh `turn/end {reason: …}` with structured error codes
(`agent.ts:303-322`), goal blocks `round-limit`/`queue-failed`
(`goal-round-driver/src/index.ts:166-200`); Codex `SessionBudgetExceeded`,
`ContextWindowExceeded`, `RetryLimit` as distinct protocol errors
(`protocol/src/error.rs:363-401`); Grok "Reached max tool rounds (N)" with the
turn's work preserved (`agent.ts:1744-1756`). PRISM's paper loop has the
three-way `PaperAgentStopReason` and zero consumers of it (defect 3; verified:
no grep hit for `stop_reason` outside `paper_agent.rs` — the whole trace is
serialized into the paper report at `papers.rs:517`, but nothing ever reads
the stop reason out of it). The minimum
transfer: surface `stop_reason`, turns used vs budget, coverage, and
proposal/rejection counts in the ingest report `papers.rs` already builds
(`crates/cli/src/papers.rs:392-435` collects `agent_turns`,
`agent_tool_calls`, traces) — a thin "run health" block per paper. Then "why
did this paper yield 4 facts" has a first-line answer without re-running, and
`Overflow`/early-finish become campaign-visible instead of invisible.

---

## C. Observability (brief)

What each harness records that would let you explain a bad run afterwards:

- **Codex**: every response item persisted with harness metadata
  (`codex-rs/history/src/lib.rs:35-50`); per-request inference-trace attempts
  with started/failed/completed records (`codex-rs/core/src/client.rs:1502-1548`);
  compaction analytics with trigger/reason/implementation/phase and
  per-attempt results (`codex-rs/core/src/compact.rs:176-245`); token-status
  trace lines after every sample (`session/turn.rs:404-421`). The rollout file
  is a full replay.
- **Grok**: SQLite transcript — every message row, every tool call's parsed
  args, every tool result, and compaction rows carrying `first_kept_seq`,
  `summary`, `tokens_before` (`src/storage/transcript.ts:9-34`); usage
  attributed per source and model (`src/agent/agent.ts:919-927`).
- **dsh**: append-only event log: `turn/start`, `step/start`, every streamed
  chunk, the final assistant message **with provider+model attached**,
  `turn/end` with a reason (`packages/core/agent-loop/src/agent.ts:256,279,320,374-389`).
  Nothing the model saw is ever unrecoverable (PASS2 §3.7).

PRISM's `PaperAgentTrace` (`paper_agent.rs:211-241`) already records per-turn
tool calls with parsed-or-raw arguments and outcomes — better on malformed
args than any of the three. The gaps, mapped to the question "why did this
paper yield 4 facts?":

1. **No elision record.** `elide_stale_tool_results` (`paper_agent.rs:547-576`)
   mutates in place; after the fact you cannot say what the model saw at
   turn 40 (PASS2 §3.7, still open — audit item 8).
2. **No per-turn token/usage split.** `usage` is accumulated whole-run only
   (`paper_agent.rs:377-379`).
3. **No coverage at stop** (§B.3a), **no rejection rollup** (proposal
   rejections exist only as tool outcomes scattered through turns), and
   **no model/provider id** in the trace.
4. **No consumer** for any of `stop_reason` (defect 3).

**Minimum event set** (all derivable from existing structures, ~60 lines
total): per-turn `{turn, request_tokens_est, elided_tool_results: n,
elided_chars}`; per-stop `{stop_reason, turns_used, coverage, unread_ranges:
top-k, proposals: {facts, classes, relations}, rejections_by_reason, model,
provider}`. With those, the 4-fact paper answers for itself: "stop=Finish at
coverage 0.07, 2 of 3 turns spent, 6 proposal rejections: 4 citation-not-read,
2 missing parent_iris" is a complete diagnosis.

---

## D. Robustness to a WEAK model (exhaustive)

The measured baseline: gemma-4-12b → 7 facts, 6 degenerate, 0/16 class
proposals correctly parented (`paper_agent.rs:1528-1541` carries the
measurement in comments). Three honest statements first:

1. **None of the three harnesses solves this.** All assume a frontier model.
   The closest any of them comes is capability-conditional behaviour: Grok's
   `applyModelConstraints` appends prose to the system prompt — "MODEL
   CONSTRAINTS: do not call tools, answer directly" — for models without
   client-tool support (`src/agent/agent.ts:519-533`), and Codex keys
   behaviour off `ModelInfo` capability flags, including **model-owned**
   token-budget defaults (reminder text, thresholds) that are validated and
   then ignored if malformed (`codex-rs/core/src/session/token_budget.rs:24-51`).
   Neither adapts the loop to a model that is capable but weak; both only
   handle capability absence. dsh has nothing. Anything PRISM does here is
   new ground, and PRISM's annotate-not-refuse architecture is a better
   substrate for it than any of theirs.
2. **PRISM already has the two strongest levers in this space**, and they are
   worth naming before asking for more: (a) schema-forcing at the tool
   boundary — every paper tool has a closed JSON schema with `required` and
   `additionalProperties: false` (`paper_agent.rs:645-790`), and
   `parent_iris`/`source_class_iri`/`target_class_iri` were made required
   *because of the measured 12B failure* (`:751-758`, `:775-787`,
   runtime enforcement at `:1530-1541`, `:1593-1611`); (b) **error messages as
   narrowed asks** — a rejected proposal tells the model exactly which tool to
   call next ("Use search_ontology or read_ontology to find where this concept
   belongs, then propose it again", `:1537-1540`). That is the coding-agent
   "tool error → model self-corrects" loop (Codex `RespondToModel`,
   `codex-rs/tools/src/function_call_error.rs:5-10`; Grok `agent.ts:987-1026`)
   executed *better* than the harnesses do it, because PRISM's errors name the
   remediation, not just the fault.
3. The remaining gaps are below.

### D.1 Schema-forcing: keep both layers, close one hole

The two-layer design (schema `required` list + runtime validation with an
instructive error) is what turned "0/16 parented" from silent garbage into
16 *visible refusals the model can act on*. Two observations:

- **Malformed-JSON arguments are recorded and fed back**
  (`paper_agent.rs:428-436`) — good. But note the asymmetry: a weak model that
  emits slightly-wrong JSON pays a full turn per attempt. Codex's provider
  side enforces `strict` function schemas (`codex-rs/core/src/tools/handlers/mod.rs:83-90`
  parses what the API already constrained). For local llama.cpp-class models
  the equivalent is grammar-constrained decoding; the chat path in this repo
  already mentions grammar construction on the local backend
  (`agent_loop.rs:2133-2141` region), so the question is only whether
  `chat_with_tools_streaming` (used by `paper_agent.rs:277-281`) goes through
  it. If yes, this is free; if no, it is the single highest-value weak-model
  investment left, because it removes the entire malformed-argument failure
  class instead of charging one turn per occurrence. **Flagged as needing a
  build/runtime check this pass could not perform.**
- **Do not add more required fields to satisfy a weak model.** The measured
  fix (parent required) worked because a parent is *semantically* required —
  an unparented class is not an extension. Requiring more than the ontology
  semantics demand would be a muzzle in the owner's sense; the standing rule
  cuts both ways.

### D.2 Mid-run budget awareness: the Codex reminder pattern transfers directly

Codex does not just announce the budget; it re-announces it at threshold
crossings: rollout-budget reminders injected into the conversation as
developer-role messages when remaining tokens pass configured thresholds
(`codex-rs/core/src/rollout_budget.rs:68-94`; message text
`codex-rs/core/src/context/rollout_budget.rs:24-29`; delivery
`codex-rs/core/src/session/rollout_budget.rs:13-23`; re-arm after compaction
`rollout_budget.rs:109-113`). The token-budget variant even makes the
reminder text and thresholds **model-owned config**
(`codex-rs/core/src/session/token_budget.rs:24-51,76-92`).

PRISM tells the paper-reading model its budget once, in the system prompt
(`paper_agent.rs:600`), and never again. A weak model on turn 30 of 56 has no
idea it is out of time — and the measured 48-of-56-turns search spiral
(PASS2 §3.4, `paper_agent.rs:585-593` comment) is exactly a model with no
clock. The transfer is small and cannot false-positive because it is pure
information: at fixed budget fractions (e.g. 50% and 80% of turns spent),
inject one user-role status line — "Status: turn 28 of 56 used; 2 proposals
recorded; 412 of 667 lines read so far. Propose as you go." Every number
comes from state the loop already holds; no domain content; no decision
made. ~30 lines. This is also the natural carrier for the coverage numbers
of §B.3a. (Contrast with dsh, which has no turn budget at all and therefore
nothing to remind about — PASS2 §5.2; PRISM's budget is the asset, the
reminder makes it legible.)

### D.3 Failure-streak awareness in the paper loop

The main loop already detects two weak-model spirals: doom loop
(3 identical, `agent_loop.rs:2796-2823`) and empty-result streak
(2 empties, `:2826-2845`, whose advisory text — "Do NOT fill the gap from
memory" — is precisely research-agent-shaped). The paper loop has neither,
and a 12B model's most likely spiral is not identical calls (which it can't
quite reproduce) but **repeated structurally-rejected proposals**: the same
`propose_class` missing its parent, five turns in a row, each rejection
already saying what to do, each ignored. Today that spends the budget and the
run ends on `Budget` looking industrious. The fix is the audit's open item 4
(advisory ladder with canonicalized args, PASS2 §3.4) **plus one research-only
rung**: count consecutive rejections *per tool* with the same reason class
(e.g. `parent_iris-missing`), and at 3 append "You have made this same error
3 times. Stop proposing until you have called search_ontology." The reason
classes are the error strings the loop itself produced — no domain vocabulary,
and the mechanism is the empty-streak code re-used. ~50 lines. What plays the
compiler: the loop's own rejection record, which is ground truth by
construction.

### D.4 Degeneracy: record it, don't infer it

"6 of 7 facts degenerate" is the hardest measured fact here, because
*degenerate* threatens to become a domain judgment. The domain-free version —
the only kind allowed — is structural: subject/predicate/object that are
identical to each other or blank; a value without unit where the citation
span is not re-examined (leave that to the existing grounding path, not the
loop); every fact citing the same one-line span; proposals duplicated
verbatim across turns. PRISM's inversion (annotate, never drop) already
defines the shape: these become `VerificationStatus` annotations produced in
`materialize_proposal`'s neighborhood (`text_extract.rs:621-645`), not drops
and not refusals. **Explicitly ruled out:** any check that encodes what a
plausible materials value/unit/subject is — that is the muzzle the owner
documented (`adversarial.md` standing instruction). The structural markers
above are language- and domain-neutral by construction; anything more is a
reviewer-model job, and PRISM's multi-sample path is already the cheap
version of one.

### D.5 Refuse loudly when the model is simply not capable

The honest question: when should PRISM say "this model cannot read this
paper" instead of emitting thin results? After §B.3 and D.3/D.4, capability
is *measured*, not guessed:

- proposal **acceptance rate** over the run (recorded proposals ÷ attempted
  proposals; every attempt and rejection is already in the trace),
- structural-degeneracy rate (D.4 annotations),
- coverage at stop (§B.3a),
- stop reason (defect 3's `Overflow`, plus a future `Error`).

A campaign-level rule — e.g. acceptance rate below a config floor **and**
(degeneracy rate above a floor **or** coverage below the §B.3b floor) on
every sample — marks the *paper's extraction outcome* as
`model_insufficient: <model-id>` in the ingest report, keeps whatever passed
the annotations (never lie, never fake), and names the model and the numbers.
That is "fail honestly over falling back": no silent thin result, no silent
drop, a reviewer sees exactly why. This is dsh's typed-block vocabulary
(`round-limit`, `goal-round-driver/src/index.ts:166-171`) applied to the
model itself, and nothing in the three harnesses does it — PRISM would be the
first, and its verification-status architecture is what makes it possible.
~60 lines once the §B/§D records exist. Thresholds in config, not Rust.

### D.6 What weak-model pressure must NOT change

The 8-tool surface, the citation-was-read gate
(`paper_agent.rs:1261-1266,1293,1326,1350`), and the generic fact schema
(`paper_agent.rs:691-731`, pinned domain-free by the test at `:1815-1838`)
are what make a weak model's output *auditable*. Weakening any of them to
raise a weak model's acceptance rate would convert visible garbage into
invisible garbage — the exact trade the owner has already paid to reverse.
The citation gate in particular has no analogue in any of the three
harnesses (none of them checks that a claim's evidence was ever seen), and it
is the one mechanism here that works *better* the weaker the model is,
because fabrication is precisely what it catches.

---

## E. Ranked recommendations

Effort-honest; sizes are production code + tests. "Design" = idea only, no
licence obligation. "Copy" = literal text/pattern translation — Apache-2.0
(Codex) or MIT (dsh/Grok) attribution in `NOTICE` where marked. Conflicts
with standing rules are flagged; none of the below hardcodes domain knowledge.

| # | What | Files | ~Lines | Provenance | Notes |
|---|---|---|---|---|---|
| 1 | **Finish gate on read-coverage, reject once** (§B.3a+b): `finish` result carries coverage + unread ranges; first finish below config floor is refused with the unread map; second always accepted; coverage stamped into trace | `paper_agent.rs` | 90 | Design (Codex stop-hook *shape*; checker is new) | The single biggest lever on the measured worst failure. No domain content. Floor lives in config. False-positive cost = 1 extra turn, bounded |
| 2 | **Make `stop_reason` matter** (§B.3c, §B.5): coverage+stop_reason into `PaperAgentTrace`; `keep_recurring_cited_facts` excludes non-comparable samples from the agreement denominator; `papers.rs` report gains a per-paper run-health block | `paper_agent.rs`, `text_extract.rs:582-618`, `papers.rs:392-435` | 80 | Design (dsh typed stop reasons) | Gives defect 3 (`Overflow` unread) its first readers and stops healthy samples being annotated as disagreement |
| 3 | **Mid-run budget/coverage reminders** (§D.2) | `paper_agent.rs` | 30 | Design (Codex rollout-budget reminders, `rollout_budget.rs:68-94`) | Pure information, cannot false-positive. Rewrite the Codex message text (their text is tokens/coding-shaped — do not copy verbatim) |
| 4 | **Trace minimum event set** (§C): per-turn elision + token estimate; per-stop rollup (coverage, rejections by reason, model/provider) | `paper_agent.rs` | 60 | Design (dsh event principle, PASS2 item 8) | Closes "what did the model see at turn 40" and feeds #5's thresholds |
| 5 | **Loud `model_insufficient` campaign outcome** (§D.5) + structural degeneracy annotations (§D.4) | `text_extract.rs`, `papers.rs` | 100 | Design (dsh typed blocks) | Depends on #2/#4 records. Thresholds in config. Annotate-and-report, never drop |
| 6 | **Defect 2: widen overflow regexes** — Anthropic `prompt is too long`, Ollama `Requested tokens … exceed` (fix `\brequest\b` → `request(?:ed)?`), Gemini/Bedrock/Mistral wordings | `crates/llm/src/overflow.rs` | 25 | PRISM's own file (originally translated from dsh, already attributed) | PRISM's classifier is already the strongest of the four harnesses; extend, don't replace. Test each wording, and keep "maximum output tokens exceeded" negative (`overflow.rs:125-146`) |
| 7 | **Defect 4: non-overflow errors keep proposals** — new sticky stop reason `Error` + return `Ok(output)` instead of `Err`; propagate the message in the trace | `paper_agent.rs:375` | 35 | Design (Grok keeps turn work on budget exhaustion, `agent.ts:1744-1756`) | Cheaper than transport retry and removes the silent total-loss failure. Retry policy stays with the caller |
| 8 | **Defect 5: never blank the newest tool result** — when the newest result alone exceeds the budget, truncate it head+tail with an elision marker instead of blanking | `paper_agent.rs:547-576` | 30 | Design (dsh head+tail retainer, PASS2 item 6) | Stops the loop spending its whole turn reading nothing |
| 9 | **Advisory repeat-call ladder + per-tool rejection streak** (§D.3, PASS2 item 4) | shared by `paper_agent.rs` and `agent_loop.rs` | 110 | Design (dsh repeat-tool-reminder) | Canonicalize args via `BTreeMap` round-trip; rewrite reminder prose; keep AL's veto as top rung. The rejection-streak rung is new and research-only |
| 10 | **Defect 1: wire `context_window` into `build_llm_config`** — read the platform catalog (`model_limits`, `main.rs:2679-2690` already resolves it for the agent path) and/or call `probe_context_window` (`crates/llm/src/lib.rs:2105`) as the tabular path does (`pipeline.rs:338`) | `main.rs:6230-6320` | 30 | Design | The elision budget derivation (done, audit item 3) is currently dead letter on this path — `tool_result_budget` falls back to the 24k constant because `context_window` is `None` |

**Where a harness is worse than PRISM — said plainly.** Codex's overflow
classifier (one exact error code, `codex-api/src/sse/responses.rs:671-673`)
would miss every non-OpenAI wording PRISM actually meets (llama.cpp, Ollama);
Grok's regex (`agent.ts:2781-2784`) false-matches output-token failures;
Grok disables transport retry entirely (`maxRetries: 0`, `agent.ts:1948`);
none of the three checks that a claim's cited evidence was ever read —
PRISM's citation gate and multi-sample agreement have no counterpart in any
tree; and none of them retains a malformed tool argument in an audit trace
the way `paper_agent.rs:428-436` does. These are not shopping items.

**Given defects vs harness fixes.** For defects 1-5 the planned fixes remain
right; two harness details improve them: defect 4 should copy Grok's "keep the
turn's work, report the failure" shape rather than only retrying (row 7), and
defect 3's real fix is the agreement-denominator change in row 2, not merely
finding readers for `Overflow`. For everything in rows 1, 3, 5: no harness
has it — that is the research-agent part of this pass.

---

## If you only do three things

Do **#1, #2, and #3** — they are one coherent intervention, roughly 200 lines,
zero licence obligations, and they attack the measured failure end-to-end
instead of around it. #1 gives the paper loop the verifier it lacks: `finish`
becomes a request that the model's own read-record can reject once, with the
unread map handed back as the narrowed ask — the Codex stop-hook idea with the
human replaced by the one ground truth a research agent has, the document
itself. #2 makes the outcome of that gate (and of overflow, and of early
finish) visible to the agreement math and the ingest report, so a thin paper
and a badly-read paper stop looking identical — which also finally spends
`PaperAgentStopReason::Overflow` on something. #3 gives a weak model the clock
and the coverage numbers mid-run, which costs nothing and cannot misfire.
Everything else in §E is real but second-wave: #1-#3 change what the loop
does; the rest changes how honestly it reports what it did.
