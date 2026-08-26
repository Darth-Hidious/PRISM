# Review of the 43 remediation commits on `hardening/integrity-remediation`

Range `4d91566c~1..add47122`, 141 files, 25,911 insertions. Reviewed 2026-08-26.

Method: four parallel reviewers over 11 commits each, then every non-SOUND
verdict re-checked against source by hand, and two settled by **executing** the
code rather than reading it. Three reviewer verdicts did not survive that check
and are recorded below as overturned — agent output is a lead, not evidence.

## Verdict

**40 SOUND · 2 MUZZLE · 1 SCOPE · 0 broken · 0 weakened tests.**

This is good work. The dominant pattern across all 43 is the right one: a state
that was previously indistinguishable from success is given its own identity and
a named, actionable error. Error paths preserve stdout so a message is never
swallowed. No test was deleted or weakened to make anything pass.

## The two muzzles — both must be fixed before merge

| Commit | Ceiling | Where |
| --- | --- | --- |
| `e6b1324f` | 10s notebook readiness, no override | `crates/cli/src/notebook.rs:21`, enforced at `:587-613`; a slow JupyterLab start is terminated |
| `ad679495` | 300s approval deadline, no override | `const APPROVAL_RESPONSE_TIMEOUT: Duration = from_secs(300)` — a human gets five minutes to answer |

Both violate the standing rule: PRISM imposes no deadline, the operator opts in.
The receiver-closing mechanism in `ad679495` is otherwise correct — only the
fixed constant is wrong. Fix is the same shape as the one applied today to
`DEFAULT_CALL_TIMEOUT`: keep the default, add an env override, fall back on a
malformed value.

## The one scope finding

`81a54722` — message names GPU and ingest; the commit also carries a
`semantic_hit_owners` -> `SemanticOwnerQuery` trait refactor. Hygiene only, the
substance is sound. Note the TUI changes in it are *not* out of scope: they are
the GPU picker, and they fix a genuine lying-UI bug ("an error is not the same
state as a successfully fetched empty catalog").

## Three reviewer verdicts overturned on verification

- **`016efc28` reported BROKEN** ("asserts registry behavior it does not
  implement"). Refuted by execution: `pytest tests/test_trainer.py` in Codex's
  own worktree gives **9 passed, 0 failed**. SOUND.
- **`72422842` reported MUZZLE** (hardcoded egress timeouts). The query timeout
  is a caller-supplied parameter — `with_platform_token(timeout, ...)`. Only the
  TCP *connect* is capped at 3s, which is ordinary. SOUND.
- **`c8a16607`/`fde41a34` flagged as breaking plain builds.** Real, but by
  design and with a supported path: `scripts/install-local.sh` sets
  `PRISM_PYTHON_WHEEL_SHA256`, and a test
  (`a_source_checkout_never_bypasses_wheel_attestation`) deliberately locks the
  behaviour in. The only defect is that the error message does not name the
  script. One-line fix, not a blocker.

## One I had wrong myself

`35feb268` regenerates a locked JSpace digest — the fixture I had refused to
touch, calling it an owner decision. Codex handled it correctly: the stale
candidates were fixed first in `1b9be61c` (zero occurrences of the collapsed
tool names remain), and the digest was then refreshed to match. Correct
sequencing, not a papered-over test.

## What is worth taking from it

Codex ran 5h 07m unattended. Three mechanisms make that possible, verified in
its source: no iteration ceiling (`needs_follow_up` is set on both tool call and
tool error), mid-turn compaction that continues rather than ending the turn, and
user messages preserved verbatim newest-first under a 20,000-token budget.

PRISM has none of these. `agent_loop.rs:2957` applies
`iteration_cap(config.max_iterations)` — a step ceiling codex does not have.
