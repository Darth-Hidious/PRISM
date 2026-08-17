# UNMUZZLE — annotate, don't refuse

Branch `feat/annotate-not-refuse`, uncommitted by instruction. The inversion:
`parse_extraction` / `retain_grounded` (crates/ingest/src/text_extract.rs) no
longer DROP a fact that fails a deterministic check. The checks all still run;
their verdict now rides the stored fact as a first-class, queryable
**verification status** (plus the check's own reason), on the assertion row
AND on the graph edge. Reads default to the trusted subset. The glm-5.2 case
that motivated this — 74 facts, 0 fabricated, 0 stored, 63 refused as
`SubjectNotNamed` for being MORE precise than the paper — now stores all 63
under `subject_not_verbatim`, findable, never promoted.

## The status set, and why

`prism_provenance::VerificationStatus` (crates/provenance/src/emmo.rs), stored
as `prov_assertion.verification_status` / `verification_reason` (additive
columns, NULL on legacy rows) and mirrored into `emmo_edge.props_json` and the
Measurement node props. One status per fact — the most disqualifying defect —
designed from what `RejectionClass` already distinguished:

| status | from RejectionClass | trusted | judgement rendered |
|---|---|---|---|
| `grounded` | (all checks passed / review asserted) | yes | yes |
| `unit_from_page` | UnresolvedUnit, doc-rescue succeeded + grounded | yes | yes |
| `subject_not_verbatim` | SubjectNotNamed | no | yes |
| `value_not_in_source` | NumericUnsupported (value/unit/condition span checks) | no | yes |
| `unit_unresolved` | UnresolvedUnit (rescue failed) + "no unit at all" | no | no |
| `sample_disagreement` | SampleDisagreement | no | no |
| `model_asserted` | PolicyDeferred, ReviewMissing, ValuelessWithUnit | no | no |
| `review_uncertain` | ReviewUncertain | no | yes |
| `review_denied` | ReviewDenied | no | yes |

Why more than the six sketched in the brief: the review verdicts must stay
distinct ("the source says otherwise" vs "the reviewer abstained" vs "no
verdict was ever rendered") because the anti-ratchet rule keys on exactly that
— `VerificationStatus::judgement_was_rendered()` carries the old
`RejectionClass::judgement_was_rendered()` semantics into the new vocabulary,
so the follow-on reviewer can know what it may re-ask. `sample_disagreement`
stays separate because it is the one status that says nothing about the
document. `unit_from_page` records that the DOCUMENT chose the identifier over
the model's spelling — it is trusted because the rescued fact still cleared
the full grounding pass with that unit.

Trust ranking (`VerificationStatus::rank`, a total order): review_denied <
value_not_in_source < unit_unresolved < subject_not_verbatim <
review_uncertain < sample_disagreement < model_asserted < unit_from_page <
grounded. Used two ways, in opposite directions on purpose:

- **Worst-wins within one sighting** (`mark_at_most` in text_extract.rs): a
  fact that trips several checks wears the most disqualifying verdict (the
  qwen regression fact — subject absent AND value absent — reports
  `value_not_in_source`, the fabrication signal, not the milder subject miss).
- **Best-wins across sightings** (SQL CASE in `record_assertion_in_open_txn`):
  a grounding witness in ANY source is a real witness; a later sloppy
  extraction cannot un-ground a fact, and a status-less write (tabular, mesh,
  legacy) never touches the stored value. The upgrade can only come from a
  deterministic pass over a real document, so it launders nothing. This is
  deliberately the opposite direction from the evidence class (worst-wins) —
  the class records production quality, the status records verification
  against a source; both rules are stated at the one SQL site.

`NULL` status = "no verification recorded" (legacy rows, and the paths that
run their own guard regimes: tabular ingest, the claims path in papers.rs,
alias edges, mesh relays, repair-worker accepts). Default reads INCLUDE NULL —
hiding it would silently vanish every pre-existing fact.

## The evidence-class spine is reused, not paralleled

Per the brief's point 4: untrusted statuses fold through the EXISTING
`evidence_for_result` ceiling at the end of `retain_grounded_with` — an
extraction the checks could not verify is, in the spine's own vocabulary, a
`ModelAssertion`, so its stored class is `indeterminate` (RED), never
`research` (ORANGE). A status-blind reader of `evidence_class` alone therefore
cannot mistake a quarantined fact for verified literature. The monotone
ceiling is untouched: a status can never raise a class, corroboration still
cannot upgrade one, and `WORST_CLASS_CASE` still governs the aggregate.

## Every check converted from fatal to annotating

All in crates/ingest/src/text_extract.rs; each check's code is unchanged —
only its consequence:

1. **Subject presence** (`subject_appears` vs document + chunk) → marks
   `subject_not_verbatim`, continues to the other checks.
2. **Numeric span support** (`numeric_fact_grounding`: value + unit +
   conditions in one span, chunk-local) → marks `value_not_in_source` with the
   check's reason.
3. **Assertion condition grounding**
   (`assertion_conditions_grounded_in_text`) → marks `value_not_in_source`.
4. **Value-less fact carrying a unit** (contradictory shape) → marks
   `model_asserted`; skips review (nonsense is not worth a model call).
5. **Semantic review** — `denied` → `review_denied`; `uncertain` →
   `review_uncertain`; missing/failed call → `model_asserted`; `asserted` →
   `grounded`. Facts already weak skip review (no call spent on them).
6. **Policy deferral** (`DropUnreviewable`) → `model_asserted`.
7. **Sampling agreement** (`keep_recurring_facts`) → below-agreement facts are
   returned marked `sample_disagreement` and still face grounding.
8. **Own-unit resolution** (`convert_fact_with(…, UnitDefects::Annotate)`) —
   after the document rescue fails, the unusable unit is STRIPPED and the fact
   converts with `unit_unresolved` carrying the strict error (naming what the
   model wrote). "Numeric value with no unit at all" (previously
   MalformedShape) joins this bucket — same hazard, same quarantine. A
   value-less fact with a garbage unit becomes `model_asserted`.
9. **The document rescue itself** (`unit_from_document`) — previously a
   silent success — is now annotated `unit_from_page` with both spellings in
   the reason.

`retain_grounded` lost its `rejections` parameter (it produces none);
`report_grounding_drop` is deleted. The grounding reason strings dropped their
"; the fact is dropped whole" clause — they are stored reasons now, and that
clause would be a lie on a stored fact.

## What stayed fatal, and why

- **A `measurement` with no numeric value** — the store cannot represent it
  (`write_fact_as` writes nothing for the shape); there is no row to
  annotate. Still `RejectionClass::MalformedShape` → repair queue.
- **A numeric CONDITION with a missing or unresolvable unit** — the typed
  contract (`MeasurementCondition.unit: Option<QudtUnit>` +
  `validate_conditions`) cannot carry a raw condition unit, and silently
  dropping the condition would change what the measurement MEANS ("at 1200
  of WHAT?"). Still MalformedShape.
- **Undeserializable fact JSON / unparseable envelopes** — nothing to store.
- **SHA-256 artifact verification, `assertion_id` / `entity_key` hashing** —
  untouched (identity and cryptography).
- **Orphan-relationship refusal** — untouched (tabular graph-validation path;
  out of this change's scope by design).
- **The monotone evidence ceiling** — untouched, and now also applied TO the
  weak statuses (above).
- **The store's unit guard, inverted but not softened**: `write_fact_as`
  still refuses a unit-less measurement — UNLESS the payload's verification
  records exactly `unit_unresolved`. The number may be stored; what can never
  happen is it being stored as if it were clean (880 GPa vs 880 MPa). Even a
  WRONG status (e.g. `grounded` with no unit) still refuses — tested.

## Reads

- `recall_with_context` / `recall_with_context_scoped` (and legacy `recall`)
  now return the TRUSTED subset by default: `verification_status IS NULL OR
  IN ('grounded','unit_from_page')`.
- New `recall_with_context_filtered(…, VerificationFilter)` with `Trusted` /
  `Any` / `Status(s)` — the review surface (pull exactly every
  `subject_not_verbatim` fact, etc.).
- `RecalledMaterialFact` gains `verification_status` + `verification_reason`
  (serde-default, additive — old payloads still deserialize).
- CLI `prism query` defaults to trusted; `--include-unverified` widens to
  everything, and the printer stamps every weak fact
  `UNVERIFIED <status> — <reason>` so nothing weak prints bare. The empty
  result hint names the flag.
- Graph traversals (`get_neighbors`) return weak facts' edges with
  `verification_status` in `props_json` (and on the Measurement node beside
  its honest `unit: null`); entity nodes for weak facts exist — that is the
  point: present and findable, not promoted.

## Guard relocations the inversion forced

- **Fabrication amplifier**: `EntityRegistry::record_grounded_facts` now
  skips untrusted facts ITSELF (structural, not caller discipline) — a
  hallucinated subject can no longer echo forward through the known-entities
  prompt block. `a_hallucinated_name_never_enters_the_registry` pins it.
- **Alias pass** (CLI): names offered to the alias model come from trusted
  facts only.
- **CLI funnel** repartitioned: `facts_proposed = stored_trusted +
  stored_unverified + dropped_malformed + deduped + store_failed`, plus
  `unverified_by_status` breakdown; both the JSON and both human printers
  updated; the partition identity is still asserted by test
  (`assert_funnel_partitions`, now also checking the stored halves and the
  per-status sum). Ingest dedup across windows keys on the fact identity with
  the status cleared (a seam re-judgement is the same fact).

## The repair machinery (kept, and why)

`RejectionClass` keeps all its variants and the repair queue/worker/tiers are
untouched: `repair_queue.class` is PERSISTED user data — stores in the field
hold items of every class, and the worker must go on draining them (it already
withdraws unknown classes gracefully). Fresh ingest now enqueues only
MalformedShape (and, rarely, UnresolvedUnit when a unit-broken fact ALSO has a
fatal shape defect). Consequences stated plainly:

- `repair::dispose`'s arms for SubjectNotNamed / NumericUnsupported /
  ReviewDenied / ReviewUncertain / PolicyDeferred / SampleDisagreement /
  ValuelessWithUnit / ReviewMissing are no longer reachable from fresh ingest
  — they are the exhaustive match of a retained enum, exercised by repair's
  own tests. Deleting them would break draining pre-inversion queues and was
  deliberately not done in this change.
- `RejectedSubject::Converted` is likewise no longer constructed by fresh
  ingest (grounding no longer rejects); it remains the deserialization target
  for persisted queue items.
- Facts written by the worker's accept path carry `verification: None` (own
  gate regime — it re-runs the full grounding gates), so they stay visible by
  default, as before.

## Known residuals (stated, not hidden)

- A fact stored `unit_unresolved` (unit NULL in the assertion id) and a later
  clean-united sighting of the same fact are DIFFERENT assertion ids (the
  unit is part of conditioned identity). Both rows exist — the weak one
  hidden by default. Reconciling them is reviewer work, not storage work.
- For a `unit_unresolved` fact the numeric span check cannot run (it needs
  the canonical unit to bind), so the value itself is not span-checked; the
  fact is already quarantined by the unit status, which ranks below
  `subject_not_verbatim` precisely because of this.
- The papers/claims path (`prism papers`, crates/cli/src/papers.rs) now
  receives weak facts as claim CANDIDATES; its own quote-based
  `validate_and_stamp` guards still refuse what lacks verbatim support —
  that path's contract (claims are quote-backed by definition) is unchanged.
- The tabular pipeline (local_facts.rs / pipeline.rs) still drops at its own
  gates — it was outside this change's stated scope (text path only).

## What this unblocks (not built here)

The record now EXISTS for the LLM to catch the big stuff: the orchestrator in
crates/agent/src/orchestrator.rs can fan reviewers out over
`recall_with_context_filtered(…, VerificationFilter::Status(s))` per weak
status, with the check's reason on every row and
`VerificationStatus::judgement_was_rendered()` telling each reviewer what it
may re-ask (nothing rendered may be re-rolled — the anti-ratchet rule
survives the inversion). A review that verifies a fact writes a grounded
sighting and best-wins lifts it into the default view; PROV-O already records
who asserted what, so "I was stupid here" is auditable.

## Every test whose meaning changed

Each carries a `CONTRACT CHANGE (annotate-not-refuse)` note in its body
saying why. crates/ingest/src/text_extract.rs:

- `a_measurement_cannot_be_reattributed_to_an_absent_material` — kept +
  `subject_not_verbatim` instead of dropped; true attribution asserts
  `grounded`.
- `extraction_itself_refuses_facts_the_document_never_stated` — the wired-in
  test now pins statuses (and evidence classes) attached on the production
  path.
- `extraction_drops_a_positive_assertion_when_the_source_denies_it` — stored
  `review_denied` with the reviewer's reason.
- `extraction_fails_closed_when_assertion_review_is_malformed` — stored
  `model_asserted`; asserts the judgement was NOT rendered.
- `extraction_drops_a_unit_absent_from_the_document`,
  `extraction_requires_the_unit_in_the_values_supporting_span`,
  `extraction_binds_units_to_their_numeric_values`,
  `extraction_never_borrows_a_unit_from_a_refused_equal_value`,
  `extraction_uses_the_callers_numeric_tolerance` (tight arm),
  `extraction_rejects_unit_homographs_and_compound_prefixes`,
  `extraction_keeps_retrievals_numeric_refusal_guards_connected`,
  `extraction_drops_an_unsupported_condition`,
  `extraction_never_joins_lowercase_source_records_for_grounding`,
  `extraction_drops_an_unsupported_condition_on_an_assertion`,
  `a_number_from_another_section_cannot_support_this_chunks_fact` — every
  span/unit/condition guard now asserts `value_not_in_source` + reason
  instead of absence; the guard logic itself is pinned unchanged.
- `extraction_requires_a_complete_subject_mention`,
  `a_relational_fact_about_an_absent_subject_is_still_dropped`,
  `grounding_consults_the_document_not_the_chunk` (invention arm) —
  `subject_not_verbatim` instead of absence.
- `caller_can_choose_fail_closed_without_a_review_call` — `model_asserted`,
  still zero review calls.
- `the_document_supplies_the_unit_when_the_model_invents_one` — now also
  asserts the `unit_from_page` stamp and its two-spelling reason.
- `a_value_that_changes_between_passes_is_not_believed` — all 4 facts return;
  recurring one `grounded`, fabricated variants `value_not_in_source`
  (worst-wins over the also-true sample disagreement).
- `one_pass_repeating_itself_is_still_one_vote` — kept + marked
  `sample_disagreement`; the vote-counting rule is pinned unchanged.
- `a_unit_the_page_does_not_print_is_still_refused` — stored with `unit:
  None` + `unit_unresolved` + the model's spelling in the reason; the
  invented identifier still never becomes the unit.
- `invented_numbers_are_withdrawn_by_code_with_zero_model_calls` — stored
  `value_not_in_source`, zero extra model calls; no longer drives
  `repair::dispose` (nothing is rejected).
- `facts_the_document_never_stated_are_dropped_not_stored` — THE regression:
  quarantined (`value_not_in_source`, untrusted), not vanished.
- `facts_the_document_states_survive` — now also asserts `grounded` with no
  reason.
- `one_bad_fact_costs_that_fact_not_the_document`,
  `numeric_value_with_unresolvable_unit_is_dropped_never_stored_unitless`,
  `numeric_value_with_no_unit_at_all_is_dropped` — conversion keeps the fact
  with the unit stripped + `unit_unresolved`; unchanged-meaning neighbours
  (`unresolvable_condition_unit_drops_the_whole_fact`,
  `numeric_condition_without_unit_drops_the_fact`,
  `a_missing_unit_is_still_refused` on the STRICT converter) still assert the
  fatal path.

crates/cli/src/main.rs:

- `text_ingest_normalises_units_and_reports_per_fact_drops` → renamed
  `…_and_quarantines_unresolvable_ones`: the banana-unit fact is stored (3
  written), invisible to the default recall, found by
  `Status(UnitUnresolved)` with value 349, `unit: None`, reason naming
  "banana"; repair ledger/queue empty.
- `repair_pass_drains_the_queue_phase_one_enqueued` — the Phase-1 feeder
  changed from an ambiguous-unit refusal (no longer a refusal) to a
  value-less measurement (`malformed_shape`, the one refusal left); the
  Phase-2 drain, field-freeze, gates, ledger and idempotence assertions are
  unchanged.
- `assert_funnel_partitions` — new partition (see above), stricter (also
  checks stored halves and per-status sum).

New tests: crates/provenance/src/emmo.rs
`weak_facts_are_stored_findable_and_excluded_from_the_default_read`,
`verification_upgrades_on_a_grounding_witness_and_never_downgrades`,
`a_unitless_number_stores_only_under_a_recorded_unresolved_status`,
`the_verification_status_rides_the_graph_edge_props`,
`verification_rank_trust_and_ratchet_are_consistent`.

## Gate output

```
$ cargo fmt -p prism-ingest -p prism-provenance -p prism-cli -- --check
fmt clean

$ cargo clippy -p prism-provenance -p prism-ingest -p prism-cli --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s)   (no warnings)

$ cargo test -p prism-provenance
test result: ok. 126 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out   (doc-tests)

$ cargo test -p prism-ingest
test result: ok. 387 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p prism-cli
test result: ok. 219 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo check --workspace --all-targets    (sibling-crate safety)
clean
```

Diff footprint: 8 files, +1989 −719 (crates/provenance/src/{emmo.rs,lib.rs},
crates/ingest/src/{text_extract.rs,alias.rs,repair.rs,repair_worker/tests.rs},
crates/cli/src/{main.rs,papers.rs}).
