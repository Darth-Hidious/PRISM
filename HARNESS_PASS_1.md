# HARNESS PASS 1 — Failure taxonomy and observability, from three coding-agent harnesses to a research agent

Pass 1 of 4. Emphasis per dispatch: **Section A (failure taxonomy) and
Section C (observability) exhaustive; B/D/E briefer.** Weighting: dsh and
Grok read deepest; Codex read for the mechanisms it alone has.

**Trees read** (all under `~/.claude/jobs/bd21bca9/tmp/`):

- `dsh` — DeepSeek harness, MIT, TypeScript monorepo. Read: `core/agent-loop`
  (agent.ts, tool-calls.ts), `core/session/src/types.ts`, `llm/llm` (error.ts,
  retry-policy.ts, adapter-failure.ts, assembler.ts), `llm/llm-retry`,
  `llm/llm-deepseek/src/translate.ts`, `llm/llm-pi-ai/src/stream.ts`,
  `compaction/*` (basic engine, summarizer, tool-pairing, tool-result-pruner),
  `guard/repeat-tool-reminder`, `spill/spill-policy`.
- `grok` — Grok CLI, MIT, TypeScript. Read: `src/agent/agent.ts` (both loops),
  `src/agent/compaction.ts`, `src/agent/delegations.ts`, `src/verify/*`
  (orchestrator, retry, evidence, checkpoint), `src/storage/{usage,transcript}.ts`,
  `src/headless/output.ts`, `src/hooks/types.ts`.
- `codex-src` — OpenAI Codex, Apache-2.0, Rust. Read: `codex-rs/protocol/src/error.rs`,
  `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/responses_retry.rs`,
  `codex-rs/core/src/compact.rs`, `codex-rs/core/src/compact_model_fallback.rs`,
  `codex-rs/core/src/hook_runtime.rs`, `codex-rs/codex-api/src/sse/responses.rs`,
  `codex-rs/core/src/tools/handlers/new_context_window.rs`.

**PRISM side re-verified on this tree** (branch state as found):
`crates/ingest/src/paper_agent.rs`, `crates/ingest/src/text_extract.rs`,
`crates/agent/src/agent_loop.rs`, `crates/llm/src/{overflow.rs,lib.rs}`,
`crates/cli/src/main.rs` (`build_llm_config`), plus `PLUGIN_AUDIT_PASS1.md`,
`PLUGIN_AUDIT_PASS2.md`, `READABILITY.md`. No code changed; no builds run.

**The frame, stated once and applied throughout:** all three harnesses are
coding agents. Their retry-until-green patterns lean on a verifier PRISM does
not have. Every recommendation below names what plays the compiler's role in
PRISM — or says plainly that nothing does and the pattern does not transfer.

**Relationship to the prior audit:** `PLUGIN_AUDIT_PASS2.md` consolidated list
items 1–3 are confirmed DONE on this tree (overflow classifier + retry-once at
`paper_agent.rs:349-378`; the tool-pairing split fix at
`crates/agent/src/agent_loop.rs:868-900`; `tool_result_budget` derives from
`context_window` at `paper_agent.rs:500-517`). **One correction:** audit item 3 is only half-done.
The *consumer* (budget derivation) shipped; the *producer* never did —
`build_llm_config` (`crates/cli/src/main.rs:6230-6318`) ends in
`..Default::default()`, so `context_window` stays `None`
(`crates/ingest/src/lib.rs:229`), `LlmClient` only fills it for local GGUF
(`crates/llm/src/lib.rs:577-581`), and the elision budget silently uses the
24k constant for every HTTP model. The tabular path solved exactly this with
`probe_context_window` (`crates/ingest/src/pipeline.rs:330-340`). Items 4–10
of that list remain open, and Grok was never seen by either audit; both are
covered below.

---

## A. Failure taxonomy (exhaustive)

Seven failure classes, per the dispatch. For each: mechanism and `file:line`
in their trees, then whether PRISM has an equivalent. A class-level summary
table closes the section.

### A1. Context overflow

**dsh.** Three layers:

1. *Classification.* `isContextWindowExceededError` — five regexes over joined
   provider code/type/message text (`dsh/packages/llm/llm/src/error.ts:51-90`).
   Canonical code `CONTEXT_WINDOW_EXCEEDED_CODE` (`error.ts:25`). PRISM already
   copied this verbatim (`crates/llm/src/overflow.rs:9-13` attribution, five
   patterns at :19-72).
2. *Silent overflow detection.* The pi-ai adapter also classifies overflow
   **from usage, not from an error**: a `stop` whose reported token usage
   exceeds the routed model's `contextWindow`, and a zero-output `length` that
   fills the window, both map to `CONTEXT_WINDOW_EXCEEDED`
   (`dsh/packages/llm/llm-pi-ai/src/stream.ts:60-84`, upstream
   `isContextOverflow` from pi-ai). This catches providers that truncate
   instead of refusing. **PRISM has nothing equivalent** — it only recognizes
   overflow when the provider throws.
3. *Recovery.* On `agent/request-error` with that code: run tool-result
   pruning first, then force one compaction **bypassing the normal threshold
   and retention policy**, then return `{kind:'retry'}`; bounded by
   `maxOverflowRetries` (default 1, `dsh/packages/compaction/compaction-basic/src/config.ts:93`).
   Two judged details: a successful assistant response resets the retry
   counter for the next request in the same turn (`compaction-basic/src/index.ts:171-177`),
   and if the summarizer throws but a model-free prune already durably shrank
   the surface, it retries anyway — "did the surface shrink" is the success
   test, not "did every phase succeed" (`compaction-basic/src/index.ts:191-220`).

**PRISM equivalent: partial.** The ingest loop classifies, halves the elision
budget, re-elides, retries once, and stops with `Overflow` retaining proposals
(`paper_agent.rs:349-378`). Differences that matter:

- PRISM shrinks by *halving a char budget*; dsh shrinks by pruning + summary.
  For the ingest loop PRISM's choice is right (blanked tool results are paper
  excerpts; the paper is one `read_paper` away — nothing is lost).
- PRISM's retry counter is per-run, never reset; a long run that survives one
  overflow on turn 12 has no recovery left at turn 40. dsh resets on every
  successful response. **This is a genuine gap** — a 56-turn run is exactly
  where two separated overflows happen.
- The main agent loop has no overflow recovery at all (verified: the only
  callers of `error_is_context_window_exceeded` are `paper_agent.rs:353,367`).

**Grok.** One loose regex: `/(context|token|prompt).*(limit|length|large|window|overflow)|too many tokens|maximum context/i`
(`grok/src/agent/agent.ts:2781-2784`). **Worse than PRISM's classifier**: no
word boundaries, and the proximity form will false-positive on harmless
sentences containing both "prompt" and "large". Recovery: only when **no
assistant text has been produced yet** and recovery has not been attempted;
then set a flag and re-enter the loop, which force-compacts (the flag is the
`force` argument to `compactForContext`, `grok/src/agent/agent.ts:1594-1604,
1907-1917`) with *relaxed* settings — `keepRecentTokens` halved with a 4000-token
floor (`grok/src/agent/compaction.ts:30,331-336`). Prevention runs every loop
iteration: compact when `tokens > contextWindow - reserveTokens` (reserve
16384, keep-recent 20000; `compaction.ts:29-31,238-243`). `maxRetries: 0` is
set on the SDK call (`agent.ts:1949`), so this hand-rolled path is the only
recovery. One attempt; a second overflow surfaces the raw API error to the user.

Two Grok details PRISM lacks: forcing compaction *below* the normal threshold
on overflow (PRISM's halve-and-elide is the same idea, executed differently —
fine), and the explicit **keep-something floor on the recovery path** — Grok
refuses to compress below 4000 retained tokens. PRISM's analogue floor
(`MIN_ELISION_CHARS = 1024`, `paper_agent.rs:524`) floors the *budget*, but
see defect 5 — it does not floor what survives.

**Codex.** Structured enum, not regex: `CodexErrorDetails::ContextWindowExceeded`
(`codex-rs/protocol/src/error.rs:94-97`), classified from the provider's exact
error code `context_length_exceeded` (`codex-rs/codex-api/src/sse/responses.rs:671-673`)
— possible only because Codex talks to one provider; PRISM is
multi-provider and cannot do this. Overflow is deliberately **not retryable**
at the stream layer (`error.rs:362-386` lists it in the `false` arm); the turn
loop records `set_total_tokens_full` and fails the turn
(`codex-rs/core/src/session/turn.rs:1390-1393`). The recovery is structural
instead: mid-turn, when the token limit is hit and a follow-up is needed, the
loop runs auto-compact and **continues the same turn**
(`turn.rs:440-481`), and the compaction call itself has overflow recovery — on
`ContextWindowExceeded` during compaction it drops the oldest history item and
retries (`codex-rs/core/src/compact.rs:309-318`). The model can also request a
fresh window itself via the `new_context_window` tool
(`codex-rs/core/src/tools/handlers/new_context_window.rs:28-40`).

**Better than PRISM's planned fix?** For defect 1 (context_window plumbing),
no harness adds anything to the planned fix — the tabular path's
`probe_context_window` already exists in PRISM; this is wiring, not design.
For defect 5, Grok's retained-floor and dsh's "newest reasoning is what the
model is doing" discipline are exactly the right reference points (E3 below).

### A2. Repeated identical tool calls

**dsh.** Advisory ladder, never a veto. Chain keyed on tool name + deep
key-sorted canonical arguments (`dsh/packages/guard/repeat-tool-reminder/src/index.ts:88-105`),
thresholds `[3, 5, 8]` (`:46`), gentle reminder at the first threshold and a
detailed reminder quoting tool/run-length/capped arguments after (`:63-79`).
Three judged details: denied calls are counted because denials flow through
the same post-execute point (`:182-194` comment + code); a user interjection
resets the chain (`:228-231`); and the reminder rides the result as
`additionalContexts` even when the downstream decision blocks the call
(`:212-224`). Config errors fail loud at plugin load (`:126-142`).

**Grok.** Nothing. No repeat detection anywhere in either loop (verified by
grep over `src/agent/`, `src/grok/`). PRISM is ahead here.

**Codex.** I did not find a repeat-call guard in the files read; the codex
tree is large and I scope this claim to the read set (turn loop, tools,
hooks). Absence of evidence, recorded as such.

**PRISM equivalent: partial, and only in the main loop.** Doom-loop veto:
three identical signatures abort the call (`crates/agent/src/agent_loop.rs:482-492`,
fired at `:2796-2812`). No argument canonicalization (key order breaks the
signature), no advisory tier, and the ingest loop has nothing — a weak model
re-searching the same lines spends real turns with no nudge. The prior audit
(item 5) already recommended the ladder; this pass confirms the Grok gap makes
PRISM's veto the only protection in two of three trees.

**Transfer verdict:** transfers cleanly — the compiler analogue is the
identical-input/identical-output test itself; no domain knowledge involved.

### A3. A tool erroring

**dsh.** Tool results carry `isError` plus structured `{name, code}` failure
identity, persisted on the `tool/result` session event
(`dsh/packages/core/session/src/types.ts:288-303`); timeouts map to a
structured `TOOL_TIMEOUT` result rather than an abandoned promise
(`dsh/packages/guard/timeout-policy/src/index.ts`, per prior audit §3.8 — I
re-confirmed the package exists and is shaped as described). A tool error is
never a loop error; it is model input.

**Grok.** Same shape: `ToolResult {success: false, output}` returned to the
model; unavailable tools in batch mode produce `success:false, "Tool X is
unavailable in batch mode"` (`grok/src/agent/agent.ts:974-983`).

**Codex.** `FunctionCallError::RespondToModel` routes tool failures back to
the model as tool output (e.g. `codex-rs/core/src/tools/handlers/new_context_window.rs:33-36`).

**PRISM equivalent: yes, and well-built.** `PaperToolOutcome::failure`
(`paper_agent.rs:200-208`) feeds every tool error — including validation
errors with the exact violated field — back to the model; only unknown tool
names and transport errors escape the loop. One gap relative to dsh: PRISM's
failures are free strings. dsh's stable machine-routable `code`
(`dsh/packages/llm/llm/src/error.ts:10-17`: "route on this, never by parsing
message") is what lets a loop *react differently* to different failure
classes. For PRISM that matters in Section D: a `citation_not_read` code can
drive a narrowed re-ask; a free string cannot be routed on without parsing.

### A4. A model that stops early

Covered in depth in Section B; taxonomy entry only.

- **dsh:** no gating at all. A step with zero tool calls is `completed`
  (`dsh/packages/core/agent-loop/src/agent.ts:392-394`). The only pushback is
  on an *empty* completion: a `stop` with zero content blocks is reclassified
  as an `EMPTY_RESPONSE` **error** (`error.ts:28-38`;
  `dsh/packages/llm/llm-deepseek/src/translate.ts:108-114`;
  `dsh/packages/llm/llm-pi-ai/src/stream.ts:90-100`), and `EMPTY_RESPONSE` is
  in the default retryable set (`dsh/packages/llm/llm/src/retry-policy.ts:18-24`,
  max 2 retries, 500ms→10s backoff). Also: a `max-tokens` finish is sticky —
  a later normal completion cannot downgrade the turn's outcome
  (`dsh/packages/core/agent-loop/src/agent.ts:285-290`).
- **Grok:** no gating. `stopWhen: stepCountIs(...)` is a ceiling only
  (`grok/src/agent/agent.ts:1329,1947`). Stop hooks fire-and-forget with the
  result discarded (`agent.ts:2153-2157`, `.catch(() => {})`). But its batch
  sub-task loop marks budget exhaustion as an explicit **failure**:
  `success: false, "Task stopped after N batch rounds. Last action: …"`
  (`agent.ts:1152-1162`).
- **Codex:** the only harness that can *block* a stop. Stop hooks return
  `should_block` + continuation fragments; the loop injects the fragments as
  a new user message, sets `stop_hook_active`, and **continues the turn**
  (`codex-rs/core/src/session/turn.rs:483-522`). A block with no prompt is
  ignored with a visible warning (`turn.rs:510-517`). The
  `stop_hook_active` flag is handed to the hook so policy can refuse to block
  twice — the anti-infinite-loop fence.
- **PRISM:** `finish` is accepted unconditionally (`paper_agent.rs:458-461`).
  The no-tool-call nudge exists (`paper_agent.rs:393-403`) and is *stronger*
  than dsh's "no calls = completed" for a reader — but a model that calls
  `finish` on turn 3 of a 40-page paper stops the run with `stop_reason:
  Finish`, indistinguishable from a satisfied reader. That is the defect this
  pass is asked about most.

### A5. Malformed tool arguments

- **dsh:** invalid JSON is preserved **as raw text** and handed to the tool
  (`dsh/packages/core/agent-loop/src/tool-calls.ts:105-111`); the tool owns
  the rejection. Also a canonicalization-safe argument domain note at
  `repeat-tool-reminder/src/index.ts:83-87`.
- **Grok:** invalid JSON → structured `success:false` result quoting the parse
  error (`grok/src/agent/agent.ts:985-997`); elsewhere raw-text fallback
  (`agent.ts:2542-2548`).
- **Codex:** schema-validated tool specs; failures returned to the model (not
  re-derived here in depth; the `FunctionCallError::RespondToModel` route at
  `new_context_window.rs:33-36` is the pattern).
- **PRISM:** explicit failure result "tool arguments are not valid JSON: …"
  (`paper_agent.rs:430-436`) — equivalent to Grok, better than dsh's
  raw-passthrough for PRISM's tools (which would all reject it anyway).
  Argument *validation* errors from `validate_and_normalize_fact`
  (`paper_agent.rs:1385-1473`) name the exact field — this is already
  retry-with-narrowed-ask in embryo (Section D).

### A6. A truncated stream / transport failure mid-response

**dsh.** The stream assembler ends with a structured finish
(`{kind:'error'|'aborted'|...}`); an error finish fires the
`agent/request-error` waterfall where **any** policy gets a vote on retry
(`dsh/packages/core/agent-loop/src/agent.ts:352-369`) — if no policy votes
retry, the loop throws `LlmError` and the turn ends `error` with the full
failure facts (`agent.ts:306-316`). Adapters classify transport truncation
separately from model errors: `stream ended before|without …` wordings →
`TRANSPORT` (`dsh/packages/llm/llm-pi-ai/src/stream.ts:46-58`), and
`TRANSPORT`, `TIMEOUT`, `SERVER`, `RATE_LIMIT` are retryable by default
(`retry-policy.ts:18-24`). The retry executor durably logs `llm/retry`
**before** the cancellable wait and `llm/retry-started` after
(`dsh/packages/llm/llm-retry/src/index.ts:148-152`), honors provider
`Retry-After` but refuses delays above `maxDelayMs` under a normal policy
(`llm-retry/src/index.ts:194-204`), and derives the retry count **from the
session log** (`findLast` over `llm/retry` events, `:182-189`) — so a resumed
session continues its budget instead of resetting it.

**Grok.** No retry (`maxRetries: 0`, `agent.ts:1949`), but a partial-save:
`streamOk` tracks whether the stream completed; if it failed after producing
text, the partial assistant text is appended as the turn
(`agent.ts:2145-2149`). Tool calls in flight are lost. Honest but lossy.

**Codex.** `CodexErrorDetails::Stream` is documented as transient and
auto-retried (`codex-rs/protocol/src/error.rs:88-93`); the retry state machine
distinguishes sampling retries from connection retries with 5s→60s backoff
(`codex-rs/core/src/responses_retry.rs:17-40`), applied at
`codex-rs/core/src/session/turn.rs:1409-1424` after an `is_retryable()` gate
(`error.rs:362-386`).

**PRISM equivalent: none.** In the ingest loop, any non-overflow error
propagates (`paper_agent.rs:377`) and the caller's `?` (`crates/ingest/src/text_extract.rs:510`)
discards the **entire run's proposals** — known defect 4, confirmed still
present. Note the asymmetry PRISM already accepts for overflow ("a run that
dies on transport must not discard the work it already recorded",
`paper_agent.rs:172-181`): the same sentence is true of every other transport
failure. The harness consensus is that *recoverable* transport errors get
retried (dsh, codex) and *terminal* ones at least preserve partial output
(grok). PRISM does neither.

### A7. A run that exceeds its budget

- **dsh:** has no turn budget by design; unboundedness is policed by token
  pressure → compaction (`compaction-basic/src/index.ts:151-177` pre-step
  pressure hook) and, a layer up, goal rounds (`maxGoalRounds`, prior audit
  §3.8). PRISM's line-scaled budget (`paper_agent.rs:26-46`) remains the
  better shape for a billed reader; keep it.
- **Grok:** ceilings only (`maxToolRounds`, explore 60 / agent 120,
  `agent.ts:1317-1329`); hitting the ceiling in interactive mode just ends —
  only the batch sub-task loop marks it `success:false` (`agent.ts:1152-1162`).
- **Codex:** `SessionBudgetExceeded` — a shared rollout token budget that
  ends the turn with a tracked error event (`protocol/src/error.rs:85-86`;
  surfaced at `compact.rs:303-308`).
- **PRISM:** budget exhaustion returns everything recorded with
  `stop_reason: Budget` (`paper_agent.rs:465-469`) — proposals preserved, good.
  But `Budget` is **unread downstream** exactly like `Overflow` (defect 3):
  `text_extract.rs:511-514` pushes the trace into `agent_traces` and never
  branches on `stop_reason`. A paper that exhausted 56 turns mid-extraction
  merges into multi-sample agreement as "a sample that found little".

### A8. Taxonomy summary table

| Failure class | dsh | Grok | Codex | PRISM today |
|---|---|---|---|---|
| Context overflow (loud) | classify → prune+compact → retry ≤1, counter resets per success | one-shot flag → forced relaxed compaction | classify by exact code; mid-turn compact-and-continue; compaction self-recovers | classify → halve elision → retry once → stop `Overflow`; main loop: none |
| Context overflow (silent, usage-based) | **yes** (usage ≥ window) | no | no (single provider, loud) | **no** |
| Repeated identical calls | advisory ladder [3,5,8], counts denials, resets on user input | nothing | not found in read set | veto-at-3 in main loop only, no canonicalization |
| Tool error | structured `{isError, code}` back to model | `success:false` back to model | `RespondToModel` back to model | failure string back to model; no stable code |
| Early stop | EMPTY_RESPONSE retry only; no gating | nothing; batch marks budget as failure | **stop hooks can block & continue** | `finish` unconditional; nudge only for zero-call turns |
| Malformed args | raw passthrough to tool | structured parse-error result | schema error to model | structured parse-error result |
| Truncated stream | classify TRANSPORT → bounded retry with durable events | no retry; partial text saved | transient retry with backoff | **error propagates; whole run discarded** |
| Budget exceeded | n/a (no turn budget) | ceiling; batch marks failure | session token budget | proposals retained, `Budget` recorded — then unread |

---

## B. Early-stop and giving up (brief)

**The honest headline: none of the three harnesses solves this, because none
of them has the problem.** A coding agent that declares done gets an immediate
ground-truth answer — the user runs it, the tests run, the failure is visible.
Their loops can afford "model says done ⇒ done". PRISM cannot: a reader that
calls `finish` after the abstract has produced a result indistinguishable from
a genuinely thin paper. The compiler-analogue question for early-stop is:
**document coverage and proposal density, computed by the harness, not by the
model.** PRISM already computes the decisive signal and throws it away:
`previously_read_ranges` (`paper_agent.rs:1224-1259`, merged) is the exact set
of paper lines the model ever saw. At `finish` time nobody compares it to
`workspace.lines.len()`.

What transfers, from the only harness with a block-and-continue mechanism:

1. **Codex's stop-hook shape is the right control structure**
   (`codex-rs/core/src/session/turn.rs:483-522`): block ⇒ inject a concrete
   continuation prompt ⇒ continue; a second stop is honored; a block with
   nothing to say is ignored loudly; and a `stop_hook_active` flag prevents
   infinite blocking. Translation to PRISM, concretely: when `finish` arrives
   and (a) merged read coverage is below a floor of total lines AND (b) turns
   used are below a floor of the budget, reject the `finish` **once** with a
   tool result that states the harness-computed facts — "N of M lines read;
   unread spans: …; X turns remaining" — and accept an immediate re-`finish`
   unconditionally. The pushback text is affordance, not domain knowledge;
   the numbers come from the workspace, not from any vocabulary.
2. **What would false-positive, stated plainly:** papers whose information
   density is front-loaded (everything in the first third), review papers
   where only one chapter is in scope, and short dense papers. Hence: one
   pushback only, low floors (suggest starting at ≤25% coverage AND ≤33%
   budget spent, tunable, defaulting OFF until measured), and the re-`finish`
   always wins. This is advisory friction, never a veto — the same judgment
   dsh made for repeat calls ("observe-and-enrich, never veto",
   `dsh/packages/guard/repeat-tool-reminder/src/index.ts:212-214`).
3. **The multi-sample poison pill (defect 3) is the bigger lever.** An
   overflow-truncated or early-stopped sample contributes zero facts, and
   `keep_recurring_cited_facts` (`text_extract.rs:541-545`) counts that as
   disagreement, so a healthy paper's facts get *dropped* because one sample
   died. Fix before any pushback work: branch on `stop_reason` at
   `text_extract.rs:510-524` — samples that did not stop `Finish` are marked
   (in the trace/report) and excluded from the agreement denominator, or
   counted as abstentions, never as dissent. ~15 lines. No harness has this
   because none of them multi-samples; this one is PRISM's own.
4. From Grok, the cheap honesty bit: a run that stops on `Budget` or
   `Overflow` should surface in the ingest report the way Grok's batch loop
   says `success: false, "Task stopped after N rounds"` (`agent.ts:1152-1162`).
   Today the report cannot say it happened at all.

---

## C. Observability (exhaustive)

### C1. dsh — the session log is the product

Everything is an event on one append-only log with contiguous sequence
numbers; message history is *derived* from it
(`dsh/packages/core/session/src/types.ts:236-246`). The vocabulary relevant to
PRISM (`types.ts:236-333`):

- `turn/start`, `turn/end{reason}` — where reason is a sum type:
  `completed | aborted | blocked | error{structured LlmFailure} | max-tokens |
  interrupted` (`types.ts:150-176`). **"Why did the turn end" always has a
  typed answer.** A crash-orphaned turn is closed as `interrupted` on reload
  by the persistence backend, never silently.
- `step/start`, `step/end` — one step = one model call + its tool executions.
- `assistant/chunk` — raw stream chunks kept for token-level replay fidelity
  (`types.ts:268-269`).
- `assistant/message` — carries the step's **usage attached to the message**
  (`types.ts:270-278`): "the model output and its accounting travel together
  (there is no separate usage record)".
- `tool/call` — raw argument string **exactly as the model produced it,
  unparsed** (`types.ts:280-286`); `tool/result` cites its call by event seq
  (`tool-calls.ts:283-300`), carries `isError`, structured `{name, code}`,
  and an opaque tool-private `meta` payload for UI replay.
- `request/header` — the full next-request envelope (config, system prompt,
  tools) appended on `initial | resume | change` (`agent.ts:444-456`,
  `types.ts:310-316`); `request/context` logs provider/model/**contextWindow**
  whenever route or capacity changes (`agent.ts:470-483`).
- `llm/retry` / `llm/retry-started` — every retry scheduled with its failure
  payload, policy key, retry number, and delay, durably **before** the wait
  (`llm-retry/src/index.ts:128-152`).
- Compaction/prune events record which seqs were shadowed, the token estimate,
  and — for model-written summaries — **which provider/model wrote the summary
  and what it cost** (prior audit §3.7; re-verified in
  `compaction/compaction/src/types.ts` event shapes and
  `compaction-basic/src/index.ts:152-157` logging).
- Unknown events: a reader meeting an unrecognized type without an
  explicitly-skippable marker **must refuse to reconstruct** — lossless by
  default (`types.ts:404-416`).

On top of the log: telemetry capture maps severity at capture time —
`isError` tool results and `turn/end` error reasons become `error` severity
records, everything else `info` (`dsh/packages/session/session-telemetry/src/index.ts:45-55`),
and whole-session stats (turn/step counts, LLM/tool wall times) survive
paging and compaction because they fold the whole log
(`dsh/packages/session/session-stats/src/index.ts:1-14`).

### C2. Grok — thin but honest accounting

SQLite-backed: per-message **usage events** with model, input/output/total
tokens and computed `cost_micros` (`grok/src/storage/usage.ts:5-55`);
compactions persisted with `firstKeptSeq` and `tokensBefore`
(`grok/src/storage/transcript.ts:210`). Headless mode emits a JSONL event
stream (`grok/src/headless/output.ts:12-58`). But tool calls and results ride
as ordinary messages in the transcript, there is no per-step structure, no
stop-reason vocabulary beyond the AI SDK's finish reasons
(`agent.ts:2600-2615`), and no retry events (there are no retries). Recaps —
periodic session summaries — are recorded with the model that wrote them and
are explicitly best-effort: "should never make the completed turn fail"
(`agent.ts:872-897`). **Grok is weaker than PRISM here**: PRISM's
`PaperAgentTrace` already has per-turn, per-call structure
(`paper_agent.rs:211-241`) that Grok lacks.

### C3. Codex — typed error surfaces + telemetry counters

Errors cross boundaries as a typed enum with per-variant user-facing messages
(`codex-rs/protocol/src/error.rs:81-141`) and are mapped again for the
app-server protocol (`codex-rs/app-server-protocol/src/protocol/v2/shared.rs:78,124`).
Every compaction/fallback records telemetry counters with reason,
implementation and outcome tags (`codex-rs/core/src/compact_model_fallback.rs:22-60`),
and turn errors are tracked per turn (`track_turn_codex_error`, `compact.rs:303-312`).
The lesson is the *classification before recording*: because the failure is a
typed variant before it hits the log, the log is queryable by failure class.

### C4. PRISM's minimum event set — "why did this paper yield 4 facts?"

`PaperAgentTrace` today: sample id, budgets, turn count, per-turn tool calls
with args and outcomes, stop_reason (`paper_agent.rs:211-241`). That answers
"what tools ran" and nothing else. Answering the question without re-running
needs exactly these additions (each mapped to the harness precedent above):

1. **Per-turn usage, attached to the turn** (dsh `assistant/message` carries
   usage): PRISM aggregates into one `output.usage` (`paper_agent.rs:377-380`);
   a turn-by-turn prompt-token curve is what shows the turn the window filled.
   ~10 lines.
2. **The model's assistant turn as issued** — including "no tool calls this
   turn" events, which today leave no trace at all (the nudge path at
   `paper_agent.rs:393-403` pushes a sample with empty `tool_calls` — good —
   but the assistant's own text/reasoning is never recorded). Verbatim
   assistant text in a research trace is also the only way to audit *what the
   model believed* when it proposed. ~15 lines.
3. **Coverage snapshot at stop**: the merged `previously_read_ranges` and
   `read_lines / total_lines` (see B). Zero harness precedent — this is the
   research-agent-specific signal. ~10 lines.
4. **Elision and overflow-retry events**: what was blanked, when the budget
   halved, whether the retry succeeded (dsh's compaction + `llm/retry`
   events; prior audit §3.7 made the same call). Today
   `overflow_retried` is a local bool that dies with the run
   (`paper_agent.rs:345`). ~25 lines.
5. **Terminal classification with readers**: `stop_reason` needs consumers
   (defect 3), and non-overflow errors need a stop-reason-style outcome
   instead of `Err` (defect 4) so the report can say *which* failure class
   ended which sample (codex's typed-enum lesson). ~20 lines plus the
   `text_extract.rs` branch.
6. **Rejections from every sample**: `text_extract.rs:534-536` appends
   materialization rejections **only for pass 0** — drops in samples 2+ are
   invisible today. Two lines.

Items 1–6 are ~80–100 lines total and turn the trace into a post-mortem
record: for any paper you can then read, in order — how many tokens each turn
cost, what the model said, what it read (coverage), what got blanked and why,
what it proposed, what was rejected and by which guard, and exactly which
class of event ended the run.

---

## D. Robustness to a weak model (brief)

Measured baseline to beat: 12B local model → 7 facts, 6 degenerate, 0/16
classes correctly parented. What the harnesses offer:

1. **Schema-forcing at the tool boundary, with the rejection as the re-ask.**
   PRISM already does this better than it knows: `validate_and_normalize_fact`
   (`paper_agent.rs:1385-1473`), mandatory non-optional relation domain/range
   (`paper_agent.rs:147-166`), and the citation-was-read gate
   (`paper_agent.rs:1293-1300`) all return the exact violation as the tool
   result. That *is* retry-with-narrowed-ask — the harness simply hands the
   model its error and lets it try again. What PRISM lacks is dsh's stable
   failure `code` (`dsh/packages/llm/llm/src/error.ts:10-17`) so the loop can
   route: e.g. after 3 consecutive citation-gate failures, stop re-serving the
   same rejection text and inject one affordance line ("proposals must cite
   lines from an EARLIER turn; search first, propose next"). Adding a `code`
   field to `PaperToolOutcome` is ~10 lines and domain-neutral.
2. **Count degenerate output; stop feeding a loop that isn't learning.**
   dsh's answer to a model that emits nothing is EMPTY_RESPONSE → bounded
   retry → then a typed failure (`error.ts:28-38`, `retry-policy.ts:18-24`).
   PRISM's analogue: count consecutive no-tool-call nudges
   (`paper_agent.rs:393-403`) and consecutive failed-validation proposals; at
   a small bound (say 3 + 3), stop with a **new, distinct stop reason**
   (`NoProgress`) rather than grinding the remaining 50 turns. "Fail honestly
   over falling back" — this is the house rule applied to model capability.
3. **Bounded retry per failure class, then report — do not thrash.** Grok's
   verify system states it best for any agent: "Retry policy: at most one
   bounded retry per known failure class. Do not loop or improvise …" and,
   when no strategy covers the failure, "report it directly instead of
   guessing" (`grok/src/verify/retry.ts:44-53`). PRISM translation: a
   proposal shape that fails validation K times is *reported* in the trace as
   a model-capability failure, not retried forever and not silently dropped.
4. **What to do when the model is simply not capable: refuse loudly.** None of
   the harnesses does this — frontier models never hit it — so PRISM must own
   the design. The honest version, consistent with the standing rules: a run
   whose sample results are dominated by validation failures and degenerate
   proposals (a harness-computable ratio — no domain knowledge needed) stores
   **nothing**, and the ingest report names the failure class ("model produced
   N proposals, M rejected by validation/citation gates; below acceptance
   ratio"). That is the research-agent form of a build failure: red, loud,
   attributed to the runner, not a quiet thin paper.

---

## E. Ranked recommendations

Effort estimates are lines of Rust including tests, on this tree. "Copy"
flags licence obligations: dsh is MIT, Codex Apache-2.0 — verbatim text needs
a `NOTICE` entry (PRISM already keeps `NOTICE` + `LICENSES/`); designs and
re-implementations need none. Nothing below hardcodes domain vocabulary; items
touching prompts carry only affordance text.

| # | Change | Files | ~Lines | Kind / licence | Fixes |
|---|---|---|---|---|---|
| E1 | Branch on `stop_reason` in the multi-sample merger: non-`Finish` samples abstain from agreement, never dissent; report says which sample died how | `crates/ingest/src/text_extract.rs:510-545` | 25 | design; none | defect 3; B4 |
| E2 | Wire `context_window` into the ingest path: `build_llm_config` sets it from the models registry, or the paper path calls `probe_context_window` once like the tabular path does | `crates/cli/src/main.rs:6230-6318`, `crates/cli/src/papers.rs:291` | 30 | design; none — completes audit item 3's producer side | defect 1 |
| E3 | Never blank the newest tool result in `elide_stale_tool_results` (keep-latest-1 unconditional), and reset `overflow_retried` after a successful response post-retry | `paper_agent.rs:547-576`, `:342-380` | 20 | design (Grok retained-floor `compaction.ts:331-336`; dsh counter-reset `compaction-basic/src/index.ts:171-177`) | defect 5 |
| E4 | Non-overflow transport errors return `Ok(output)` with a new `stop_reason: Error(classified)` instead of `Err`; proposals retained, class recorded | `paper_agent.rs:377`, `text_extract.rs:510` | 25 | design (grok partial-save `agent.ts:2145-2149`; dsh turn-end `error{failure}`) | defect 4 |
| E5 | Extend overflow classifier: Anthropic `prompt is too long`, Ollama `Requested tokens … exceed` (fix the `\brequest\b` gap), Gemini/Bedrock/Mistral wordings — with the existing negative tests grown | `crates/llm/src/overflow.rs` | 40 | re-implementation of PRISM's own module; new wordings are not dsh text → no new obligation beyond the existing DEEPSEEK notice | defect 2 |
| E6 | Trace minimum event set C4 items 1–6: per-turn usage, assistant text, coverage snapshot, elision/overflow events, sample-0-only rejection fix | `paper_agent.rs`, `text_extract.rs:534-536` | 90 | design; none | C; the "why 4 facts" question |
| E7 | Coverage-gated `finish` pushback, advisory-once, default OFF, floors configurable; re-`finish` always honored | `paper_agent.rs:458-461` + workspace coverage | 80 | design (Codex stop-hook control structure `turn.rs:483-522`, Apache idea-only) | B; the 3-turns-on-40-pages loss |
| E8 | No-progress stop reason: bound consecutive nudges and consecutive validation failures → `NoProgress` stop, proposals retained | `paper_agent.rs:393-403` | 25 | design (dsh EMPTY_RESPONSE bounded retry) | D; weak-model cost bleed |
| E9 | `code` field on `PaperToolOutcome` failures + one affordance injection after repeated same-code failures | `paper_agent.rs:183-208` | 35 | design (dsh `HarnessError.code` `error.ts:10-17`) | D |
| E10 | Loud refusal on capability failure: harness-computed rejected/total ratio below acceptance ⇒ store nothing, report names the model | ingest report layer (`papers.rs`) | 40 | design; none — no harness precedent | D |
| E11 | (Main loop, later) overflow recovery + `B1`/`B2` fixes from READABILITY.md | `crates/agent/src/agent_loop.rs` | 100+ | design | ledger B1/B2; main-loop overflow gap |

Conflicts with standing rules: **none found.** E7's pushback text and E9's
affordance line must stay affordance-only (budgets, coordinates, mechanics —
never reading strategy about subject matter); that keeps them on the right
side of "domain knowledge never in Rust". E10 is the one item that *looks*
like a fallback risk; it is the opposite — it refuses rather than stores
garbage.

Where a harness solves a known defect better than the planned fix: **none of
the five confirmed defects needs a different fix after this pass** — the
harnesses corroborate the planned shapes. The two upgrades worth taking are
inside E3 (counter reset — dsh does it, PRISM's one-shot counter will bind on
long runs) and E1 (no harness has multi-sampling; PRISM's merger semantics
are its own design decision and should be made explicitly, not inherited from
an accident).

Where the harnesses are WORSE than PRISM, said plainly: Grok's overflow
classifier (one loose regex, false-positive-prone, `agent.ts:2781-2784`);
Grok's absence of any repeat-call protection; Grok's transcript structure
(flat messages vs PRISM's per-turn per-call trace); dsh's lack of any turn
budget (fine for interactive coding, wrong for a billed batch reader); and all
three's early-stop blindness (A4/B) — inherent to coding agents, and the
single thing PRISM must build for itself.

Where patterns do NOT transfer, loudly: "retry until the verifier passes" has
no research form — there is no compiler; the nearest honest signals are
coverage, citation gates, and cross-sample agreement, and none of them is a
ground truth. "Stop hooks running user shell commands" (Codex) is the wrong
surface for an ingest pipeline; take the control structure, not the
mechanism. "Model-written compaction summaries" (dsh/Grok) matter only for
the research/main loop where dropped content is not re-fetchable — in the
ingest loop, blanking with re-read is correct and cheaper.

---

## If you only do three things

Do **E1** (make dead samples abstain instead of dissent — ~25 lines in
`text_extract.rs`; today an overflow-truncated sample quietly deletes the
healthy samples' facts from the agreement set, which is the single largest
silent quality loss in the multi-sample path), **E2+E3** (wire
`context_window` into the ingest path and stop the elision budget from
blanking the newest tool result — together ~50 lines; they close defects 1
and 5 and make the already-shipped budget derivation real), and **E6** (the
six-field trace minimum — ~90 lines; it converts "why did this paper yield 4
facts" from a re-run into a read). Everything else — the finish pushback, the
weak-model refusal, the main-loop recovery — builds on a loop that can no
longer fail silently, which is the thing these three harnesses actually teach:
none of their cleverness matters as much as the fact that every turn ending,
every retry, and every blanked byte in them is a recorded, typed, queryable
event.
