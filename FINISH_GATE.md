# FINISH_GATE.md — Coverage-gated `finish` + honest traces for the paper loop

Branch `feat/annotate-not-refuse`. No commits, no pushes, no `--release` build,
no process kills, nothing deleted under `target/`. All cargo commands run one
at a time in debug profile. The running experiment's `target/release/prism`
was not rebuilt (mtime predates this work; only `check`/`test`/`clippy` ran).

## The frame

PRISM is a research agent, not a coding agent. A coding agent gets ground truth
every turn (compiler, tests). The paper loop got none: a `finish` after 3 turns
on a 40-page paper was indistinguishable from success. The verifier is now
constructed from what the loop already maintains — `previously_read_ranges` is
a deterministic, model-free record of which lines the model saw. Against the
document's line count that is **coverage**: the research analogue of "the tests
ran." It cannot lie and it is domain-neutral (lines, not content).

---

## What was built (in the order given)

### 1. Inform the finish decision (§B.3a)
The `finish` tool result now returns, in addition to the three counters:
`total_lines`, `lines_read`, `coverage`, and `largest_unread` (a bounded list,
capped at `MAX_UNREAD_RANGES = 10`, longest first). The model decides with the
information a reviewer has. This decides nothing, so it cannot false-positive.
Test: `finish_reports_coverage_and_unread_ranges`.

### 2. Reject a premature `finish` ONCE (§B.3b) — the actual fix
When `finish` is called below the coverage floor, the loop does not stop. It
returns a tool FAILURE naming the largest unread ranges and continues; the
SECOND `finish` always succeeds. Implemented as the sibling of `agent_loop.rs`'s
execution-contract gate: `MAX_FINISH_GATE_FIRINGS = 1` bounds the false-positive
cost at exactly one extra model call. The accepted second `finish` stamps the
trace with the coverage it accepted, so a reviewer sees the risk taken.

This is NOT a fact quota. The refusal text names line coordinates and the
remediation ("read, or finish again to accept partial coverage") — never a
demand to produce more facts. Coverage measures reading, the precondition for
extraction, independent of how many facts a document contains.

- Test: `a_first_finish_below_the_floor_is_refused_once_and_names_the_unread`
- False-positive test: `a_concentrated_paper_can_still_finish_on_the_second_call`
- Off-switch test: `a_zero_floor_disables_the_gate`

### 3. Coverage into the trace, and into agreement (§B.3c)
`PaperAgentTrace` carries coverage-at-stop (`total_lines`, `lines_read`,
`coverage`, `unread_ranges`). The EXISTING `complete_samples` logic in
`text_extract.rs` was extended: a sample that stopped early at poor coverage is
not comparable and is excluded from the agreement denominator **and reported as
excluded** (`TextExtraction.agreement_exclusions`). A healthy sample's correct
facts are no longer stamped `SampleDisagreement` because a sibling died or
bailed early.

Comparability rule (`sample_counts_toward_agreement`):
- `Overflow` / `Failed` → not comparable (transport cut the reader off).
- `Finish` below the reading floor → not comparable (silence about skipped
  lines, not about the document).
- `Budget` → comparable (the turns were SPENT; silence is weak-but-real
  evidence), per HARNESS_PASS_2 §B.3c.

- E2E test: `a_sample_that_bailed_at_low_coverage_is_excluded_and_reported`
- Unit test: `agreement_comparability_follows_the_reading_standard`

### 4. Mid-run awareness for a weak model (§D.2, §D.3)
- **Budget/coverage reminders (§D.2):** at fixed budget fractions (50% and 80%
  of turns spent) one user-role status line is injected with turn position,
  proposal count, and current coverage. Pure information; decides nothing;
  cannot misfire. Test: `the_reader_is_told_its_budget_position_and_coverage_mid_run`.
- **Failure-streak awareness (§D.3):** consecutive rejections of the SAME
  proposal tool with the SAME reason class are counted; at
  `REJECTION_STREAK_LIMIT = 3` the loop names the spiral and the way out in the
  result the model reads next. Reason classes are the loop's own rejection
  strings — ground truth by construction, no domain vocabulary.
  Test: `a_third_consecutive_identical_rejection_names_the_spiral`.

### 5. The six-field trace minimum (PASS_1 §E6)
"Why did this paper yield 4 facts?" is now answerable by READING the trace:
1. Per-turn usage attached to the turn (`prompt_tokens`, `completion_tokens`).
2. The assistant turn as issued (`assistant_text`).
3. Coverage snapshot at stop (see task 3).
4. Elision + overflow events visible — a blanked tool result now leaves a
   record (`elided_tool_results`, `elided_chars` per turn;
   `PaperAgentTrace.overflow_events` with budget before/after and whether the
   retry landed).
5. Terminal classification with readers (`stop_reason` is now consumed by the
   agreement denominator and the capability verdict).
6. Rejections from EVERY sample (the `if pass == 0` guard was removed — drops
   in sample 2+ used to be invisible).

Plus the stop-time rollup: `PaperAgentTrace.proposals` (facts/classes/relations),
`rejections_by_reason` (stable reason classes), and `model` (the routed model's
identity). Test: `the_trace_is_a_post_mortem_of_the_run`.

### 6. Refuse loudly when the model is not capable (§D.5)
Capability is measured, not guessed. `assess_model_capability` marks the
extraction `model_insufficient` when EVERY sample shows proposal acceptance
below the floor AND (structural-degeneracy rate above its ceiling OR coverage
below the reading floor). A sample that attempted nothing is unmeasured, not
failed. The verdict names the model, the numbers, and what to change; the facts
that passed annotation are RETAINED (never lie, never fake). Surfaced on stderr
(WARNING) and in the machine-readable summary for both `papers claims` and the
local text-ingest path.

- Tests: `a_weak_model_is_refused_loudly_and_named`,
  `a_healthy_model_is_not_accused`, `a_quiet_paper_is_not_an_incapable_model`,
  `one_healthy_sample_acquits_the_model`.

---

## Config keys added (`prism.toml` `[ingest]`) and defaults

All thresholds that are policy choices live in config, not Rust logic. The loop
receives a `PaperAgentPolicy` built from these by `crate::paper_agent_policy`.

| Key | Default | Meaning |
|-----|---------|---------|
| `finish_coverage_floor` | `0.25` | Fraction of lines the reader must have seen before a FIRST `finish` is accepted. Below it the finish is refused ONCE (second always wins). **`0` disables the gate.** |
| `model_acceptance_floor` | `0.33` (1/3) | Proposal acceptance rate below which, on every sample, the run leans toward `model_insufficient`. |
| `model_degenerate_ceiling` | `0.5` | Structural-degeneracy rate above which a sample counts against the model. |

`PaperAgentPolicy::ensure_valid()` refuses NaN and out-of-range floors loudly at
the door (test: `policy_validation_refuses_nan_and_out_of_range_floors`).
Config parse tests: `ingest_reading_policy_defaults_and_overrides`.

---

## Every trace field added

`PaperAgentTrace` (per run):
- `total_lines`, `lines_read`, `coverage`, `unread_ranges: Vec<PaperLineRange>`
- `proposals: PaperProposalCounts { facts, classes, relations }`
- `rejections_by_reason: BTreeMap<String, usize>`
- `overflow_events: Vec<PaperOverflowEvent { turn, elision_budget_before, elision_budget_after, retry_landed }>`
- `model: Option<String>` (routed model identity, when the seam supplies one)

`PaperSampleTrace` (per turn):
- `assistant_text: Option<String>`
- `prompt_tokens`, `completion_tokens`
- `elided_tool_results`, `elided_chars`

New public types: `PaperLineRange`, `PaperProposalCounts`, `PaperOverflowEvent`,
`PaperAgentPolicy`. New text-extract types: `SampleExclusion`,
`SampleCapability`, `ModelInsufficiency`.

---

## How the gate behaves on the false-positive cases

The one-shot design makes every false positive cost exactly ONE turn:

- **Paper concentrated in one section / review paper skimmed correctly / paper
  outside the active ontology's vocabulary:** first `finish` below the floor is
  refused with the unread map; the model calls `finish` again and is believed.
  The accepted partial coverage is stamped on the trace. Pinned by
  `a_concentrated_paper_can_still_finish_on_the_second_call`.
- **Empty document:** counts as fully read (`coverage_fraction(0,0) = 1.0`), so
  the gate can never fire on it. Pinned by
  `coverage_of_an_empty_document_is_complete`.
- **Coverage at exactly the floor:** finishes first try, no friction. Pinned by
  `coverage_at_or_above_the_floor_finishes_first_try`.
- **Gate disabled (`0`):** the very first finish is accepted even with nothing
  read. Pinned by `a_zero_floor_disables_the_gate`.

---

## What was judged too large (and why)

- **A separate `provider` trace field:** collapsed into `model` as
  `"model @ base_url"` via `model_descriptor()`. A second free-text identity
  added no auditable signal over the one field.
- **Per-fact degeneracy as a new `VerificationStatus`:** degeneracy (§D.4) is
  used ONLY as an input to the capability verdict, kept strictly structural
  (two of subject/predicate/object identical or blank). Minting a new stored
  verification status would touch the persistence vocabulary and the repair
  queue's class set — a larger, riskier change than the task required.
- **Separate elision count for the overflow-retry shrink:** the re-elision on
  overflow recovery is captured by `overflow_events` (budget before/after), so
  a second per-result counter would duplicate the record.
- **The full §B.5 per-paper "run health" block in `papers.rs`:** task 3 scoped
  coverage-into-trace + agreement; the ingest summary already exposes `traces`,
  `agreement_exclusions`, and `model_insufficient`, which carry the health data.

---

## Gate output (exact)

```
$ cargo fmt --all
(no output — clean)

$ cargo test --workspace   (aggregated)
TOTAL passed=3092 failed=0 ignored=11

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.01s
```

Baseline before first edit was 3074 passed / 0 failed; this change adds 18
tests (11 in `paper_agent`, 6 in `text_extract`, 1 in `config`) and leaves
everything green. Language-drift checker: `OK: no CJK language drift detected
in agent artifacts`.

### Note on the pre-existing clippy failure
The pristine tree did NOT pass `cargo clippy -- -D warnings` on this toolchain:
`paper_agent.rs` had a const-folded `assert!(20_928 < MIN_ELISION_CHARS, …)`
that trips `clippy::assertions_on_constants`. This predates my edits. I fixed it
minimally (assert against the non-const `tool_result_budget(Some(32_768))`,
same semantics) so the gate could go green; flagged here for transparency.
