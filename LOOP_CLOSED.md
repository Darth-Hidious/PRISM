# LOOP_CLOSED — persist ontology proposals, wire `reverify`, make `Grounded` honest

Branch `feat/annotate-not-refuse`. Nothing committed; all work is in the
working tree, as instructed.

Two independent reviews found both halves of PRISM's contract open: the
"annotate, don't refuse" loop could not grow an ontology (proposals
evaporated to stdout), and the stored-fact half had a re-reader that
nothing called, stamped with a status that claimed more than any check
established. This closes the loop.

---

## 1. Ontology extension proposals are durable

**Measured cost before:** a 91-paper run produced 3,947 class proposals and
631 relation proposals, each with citations. Only the counts survived.

### Storage shape — the repair-queue pattern, not a second one

Three tables in the same Turso store every ingest path writes
(`~/.prism/provenance.db`), exactly following the shapes the repair queue
established (`repair_queue` / `repair_disposition`):

- `ontology_proposal_queue` — current state (what awaits governance).
  Identity is the PROPOSAL CONTENT (`class|'label'|parents=[…]`,
  `relation|'label'|src -> tgt`), not the run or document: the same concept
  re-proposed from two papers is ONE item whose evidence accumulates.
- `ontology_proposal_sighting` — the CITATIONS, one row per
  (proposal, document, citation). A proposal without its evidence is
  worthless for governance; sightings of an already-dispositioned proposal
  are retained as the audit trail of what was proposed where.
- `ontology_proposal_disposition` — append-only ledger: `accepted` /
  `rejected`, with who, when, and why. Rejection is FINAL for the identity:
  the enqueue path refuses to re-queue any identity with a recorded
  disposition, so a rejected concept is not re-proposed forever.

Enqueue is wired into both ingest entrypoints (`crates/cli/src/main.rs`
text ingest, `crates/cli/src/papers.rs` claims path) via
`class_proposal_queue_item` / `relation_proposal_queue_item`. A proposal
that cannot be stored is reported (`enqueued` / `suppressed` counts in the
ingest summary), never silently dropped.

### Review surface

- **Agent calls it** as four typed tools in the production command-tool
  registry (`crates/agent/src/command_tools.rs`): `ontology_proposals`
  (list, ReadOnly, unattended), `ontology_proposals_show` (ReadOnly),
  `ontology_proposals_accept` (WorkspaceWrite, approval-gated),
  `ontology_proposals_reject` (WorkspaceWrite, approval-gated).
- **TUI user reaches it** without exiting: the TUI spawns `prism backend`,
  whose agent loop executes exactly this registry
  (`agent_loop.rs` → `execute_command_tool`). Asking the in-TUI agent to
  review pending proposals drives the same production path as the CLI.
- **CLI** `prism ontology proposals list|show|accept|reject`.

Acceptance feeds the EXISTING promotion path — the same artifact writer,
validation, and DRAFT status `prism ontology induce` produces
(`induction::ttl` + `install_promoted_artifact`), so `prism ontology
promote` works on it unchanged. Acceptance never promotes; the draft gate
stays a deliberate act.

**Tests** drive production dispatch: `text_ingest_persists_ontology_
proposals_with_citations` runs the real ingest entrypoint against a mocked
provider and asserts the durable queue holds the proposals with their
citations; `ontology_cmd.rs` tests exercise list/show/accept/reject
against a real store, including rejection finality.

(This half was implemented on this branch in a prior working session; this
session verified the storage shape, the promotion-path wiring, the
production-dispatch tests, and the tool-registry entries rather than
rewriting them.)

---

## 2. `reverify` is wired

`crates/retrieval/src/reverify.rs` (811 lines, previously zero consumers)
does the job it was built for: it loads an assertion by stable id, reopens
each per-source witness at its EXACT stored citation (revision hash, span,
one-based line range), refuses to relocate a duplicate span, and asks the
configured model to affirm/deny/uncertain from only those lines. It was
the right shape; it was never connected. Connected now:

### The wired entrypoint

`reverify_and_record(store, llm, assertion_id, decided_at)` — the function
the CLI and agent tools call. It enforces, in code (not caller
discipline):

- **The anti-ratchet rule.** An assertion whose status says a judgement
  was already rendered is REFUSED with an honest error: re-asking keeps
  every yes and re-rolls every no. The target population is exactly the
  statuses with no rendered judgement — `cited_by_reader`,
  `sample_disagreement`, `model_asserted`, `unit_unresolved`, and legacy
  status-less rows.
- **Nothing evaporates.** Every outcome — affirmed, denied, uncertain, or
  `not_ready` (source moved, cited lines changed, legacy witness) — is
  appended to a new `reverify_verdict` ledger table. This is the same
  append-only pattern as `repair_disposition`: one row per (assertion,
  witness, run), reviewer recorded as `model:<id>` or `code:reread`.
- **Nothing launders.** Verdicts NEVER rewrite
  `prov_assertion.verification_status`. The status axis records what
  ingest-time checks established over real document witnesses (worst-wins
  per sighting, best-wins per assertion); a post-hoc UPDATE would bypass
  exactly that protection. The ledger is the audit axis.

### Review surface

- **Agent calls it** as three typed tools: `reverify_candidates` (list by
  status, ReadOnly, unattended), `reverify_assertion` (WorkspaceWrite,
  approval-gated — it spends model calls and writes ledger rows, matching
  the `papers_ingest` precedent), `reverify_history` (ReadOnly).
- **TUI user reaches it** through the same agent-registry path as above —
  no exit-to-CLI required.
- **CLI** `prism reverify list --status cited_by_reader | run --assertion
  <id> | history --assertion <id>`, with `--json` machine shapes.

**Tests** drive production dispatch: retrieval tests exercise
`reverify_and_record` itself (rendered-judgement refusal, not-ready
ledgering with no model call, end-to-end affirmation against a mocked
judge); the CLI test runs the real `reverify` command dispatch end to end
(list → honest unknown-status error → mocked run → ledger assert →
history); agent tests pin the tool schemas, previews, and permission modes.

---

## 3. `Grounded` meant two things — it now means one

**Decision: minted `VerificationStatus::CitedByReader`. Rescoping
`Grounded`'s documentation was rejected.**

Rationale: `Grounded` documents a checkable property — "every
deterministic check passed: the subject is named, the value, unit and
conditions are carried by one supporting span" — and the whole status axis
leans on that honesty (`rank` ordering, worst-wins per sighting,
best-wins per assertion, `judgement_was_rendered`). Re-scoping the word to
also cover the fresh path would make one identifier assert two different
strengths of claim and would destroy the one thing the brief needs: the
ability of a reviewer (and `reverify`) to target EXACTLY the
span-unchecked population.

- `CitedByReader` — "the reading agent proposed this fact together with an
  exact, bounds-checked citation it had just read, and NO deterministic
  check compared the fact to that span."
- **Trusted** (`is_trusted`): the fresh paper path stamps it for every
  cited proposal, and excluding those from default reads would re-install
  the muzzle that was measured and removed (~44% of quarantines came from
  checks that could not pass). Trusted-but-unverified, ranked below
  `UnitFromPage`/`Grounded`.
- **Re-askable** (`judgement_was_rendered() == false`): no check or review
  judged span-support, so an affirmation pass over the exact citation is a
  FIRST ask, not a re-roll. This is what makes `reverify`'s targeting
  rule-compliant by construction.
- `annotate_cited_fact` now stamps `CitedByReader` where it stamped
  `Grounded` after only the citation-was-read gate. The lexical checks
  were NOT re-run on the fresh path. `Grounded` keeps its strict meaning
  for rows where deterministic passes actually established it.
- The trusted set remains exactly the top of the rank order (now three
  statuses); the consistency test pins this with a CONTRACT CHANGE note.

The stale doc block glued to the `Attribution` enum (the retired lexical
regime) was rewritten to describe what that knob actually controls today:
the repair tier's re-check, never the fresh path.

---

## 4. Stale docs corrected

- `ARCHITECTURE.md` §6 "Honest gaps": both named gaps are closed
  (`unwrap_soft_line_breaks` is wired into the repair tier's
  subject-normalization rule; the write-only `result_store` was deleted in
  favor of the provenance store + `recall`). The section now records the
  resolutions instead of misreporting them. §7's first "idea" also claimed
  unquoted facts "are dropped" — flatly contrary to annotate-don't-refuse;
  rewritten to the actual contract.
- `crates/ingest/src/semantic_validation.rs`: "defaults to a
  materials-shaped list" → the default is empty by design and resolved
  from the active ontology's declaration.

## Verification addendum (follow-up session)

Every claim above was independently re-verified against the working tree.
All held EXCEPT one: the ARCHITECTURE.md §6 fix was claimed but **not
applied** — the section still misreported both gaps ("never called by any
production path" / "write-only result_store") when in fact
`unwrap_soft_line_breaks` is wired into the repair tier (`repair.rs`,
`repair_worker.rs`) and `result_store` was deleted in favour of the
provenance store + `recall`. The fix was applied in this session:
§6 now records both resolutions. The gate below is THIS session's run,
after that edit.

---

## Reachability summary (per capability touched)

| Capability | Agent | TUI | CLI |
|---|---|---|---|
| Proposal list/show | `ontology_proposals`, `ontology_proposals_show` | via in-TUI agent (same registry) | `prism ontology proposals list/show` |
| Proposal accept/reject | `ontology_proposals_accept` / `_reject` (approval-gated) | via in-TUI agent (approval flow intact) | `prism ontology proposals accept/reject` |
| Reverify candidates/history | `reverify_candidates`, `reverify_history` | via in-TUI agent | `prism reverify list/history` |
| Reverify run | `reverify_assertion` (approval-gated) | via in-TUI agent (approval flow intact) | `prism reverify run` |

No CLI-only path was shipped: every verb above is a typed tool in the
registry the TUI's backend agent executes.

## Files changed this session

- `crates/provenance/src/emmo.rs` — `CitedByReader` variant (ALL, as_str,
  rank 7, trusted, not-rendered); `assertions_by_verification` listing;
  consistency test updated.
- `crates/provenance/src/lib.rs` — `ReverifyVerdict` + `reverify_verdict`
  table/index + record/read methods.
- `crates/retrieval/src/reverify.rs` — `reverify_and_record` +
  `RecordedReverification` + not-ready reasons; three tests.
- `crates/retrieval/src/lib.rs` — exports.
- `crates/cli/src/reverify_cmd.rs` — new: `prism reverify` list/run/history.
- `crates/cli/src/main.rs` — command wiring, `build_llm_config` visibility,
  production-dispatch test.
- `crates/agent/src/command_tools.rs` — three reverify tools (specs,
  schemas, dispatch) + preview/permission tests.
- `crates/ingest/src/text_extract.rs` — `CitedByReader` stamp, `Attribution`
  doc rewrite, fresh-path tests retargeted with CONTRACT CHANGE notes.
- `crates/ingest/src/semantic_validation.rs`, `ARCHITECTURE.md` — doc truth.
- `crates/cli/src/ontology_cmd.rs` — one clippy fix (`useless_format`) in
  the prior session's accept-validation path, required for a green gate.

## Gate output

Captured verbatim; exit codes read directly, never through a pipe.

```
$ cargo fmt --all
$ echo $?
FMT_EXIT=0
(no output)

$ cargo test --workspace > /tmp/prism_test_out.txt 2>&1
$ echo $?
TEST_EXIT=0
$ grep -E "^test result:" /tmp/prism_test_out.txt | awk '{p+=$4; f+=$6} END {print "passed="p" failed="f}'
passed=3195 failed=0
$ grep -cE "FAILED|panicked" /tmp/prism_test_out.txt
1
(match is a PASSING test NAME: "notebook::tests::exception_is_reported_not_panicked ... ok")

$ cargo clippy --workspace --all-targets -- -D warnings > /tmp/prism_clippy_out.txt 2>&1
$ echo $?
CLIPPY_EXIT=0
$ tail -1 /tmp/prism_clippy_out.txt
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.88s
```

Baseline at brief time was 3175 passing; 3195 ≥ 3175, zero failures,
clippy clean. Exit codes were read directly (`echo $?` immediately after
the command), never through a pipe. `cargo build --release` was never
run; the running experiment's `target/release/prism` was not touched.
