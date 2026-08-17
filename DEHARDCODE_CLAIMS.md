# DEHARDCODE_CLAIMS — removing the materials/English vocabulary from `crates/retrieval/src/claims.rs`

Branch `feat/annotate-not-refuse`, worktree `/Users/siddharthakovid/Downloads/prism-unmuzzle`.
Nothing committed, nothing pushed. All changes verified with the gate at the bottom.

The acceptance test applied to every decision below: **could a customer promote a
German-language pharma ontology and have this behave correctly with ZERO Rust
edits?** Where a check could not answer "yes", it was deleted rather than kept
wrong — on this branch a failed check stores the fact carrying a
`VerificationStatus`, it never drops it, so a deleted guard costs a weaker note,
never a verdict.

---

## 1. What was removed, and where the knowledge now comes from

### 1.1 `NONNEGATIVE_QUANTITIES` + `SIGNED_DIFFERENTIAL_MARKERS` + `SIGNED_STRENGTH_HOMOGRAPHS` (+ `is_nonnegative_quantity`, `strip_trailing_unit`)

A compiled English/materials table deciding that "uts", "hardness", "density",
"grain size", "yield strength" (plus suffix rules, differential-marker and
homograph exception lists) cannot be negative. It was the owner's exact example
of telling the model what density is.

**Now:** the sign domain is a property of the QUANTITY KIND and arrives through
the ontology at grounding time:

- New type `prism_provenance::QuantitySignDomain { Unspecified, NonNegative, Signed }`
  (vocabulary-neutral crate; `Unspecified` is the default and means *the ontology
  said nothing*).
- New `Ontology::quantity_sign_domain(&self, quantity_id: &str) -> Option<QuantitySignDomain>`
  trait method (`crates/ingest/src/ontologies.rs`), default `None` (silence).
- The matcher takes the resolved value per claim as
  `claims::GuardPolicy { quantity_sign, unit_term }` and fires `RefusalGuard::SignDomain`
  **only** on `QuantitySignDomain::NonNegative`. Silence leaves the check inert —
  it is never replaced by inference from the quantity's name, in any language.
- Grounding wiring: `text_extract::quantity_sign_for_fact` asks the active ontology
  (predicate identity first, then object); `numeric_fact_grounding` builds the
  per-fact `GuardPolicy` and threads it through every matcher call.

Evidence in tests: `claims::tests::sign_domain_reads_the_ontology_not_the_quantity_name`
(Negative refuses under a NonNegative policy for *any* spelling — German, French,
symbol forms — and stamps under silence), `ontologies::tests::a_builtin_ontology_that_declares_no_sign_domain_says_so`
(the bundled EMMO stays silent — no compiled residue survives anywhere).

### 1.2 `UNIT_TOKENS` (+ `EXTRA_UNIT_INITIALS`, `DENIED_UNIT_INITIALS`, `unit_initial`, `unit_follows`)

A second, independent unit vocabulary (~60 lowercased tokens), disconnected from
`prism_provenance::units` (the ontology-served exact-term resolvers) and colliding
across domains (`nm`/`nM`). It fed three guards: glued-letter redemption in
`clean_number_boundary`, the label exemption, and SeparatorDash shape (B).

**Now:** the fact's OWN unit term — exactly the term the reader/ontology chose —
is the only unit knowledge the matcher has:

- `GuardPolicy::unit_term` carries it (case-folded for comparison, exactly the
  convention of the pre-existing `numeric_value_has_grounded_unit`).
- Glued letters redeem only when they begin the fact's own term
  (`glued_fact_unit`); SeparatorDash shape (B) reads the same term and is inert
  without one. No term supplied → no redemption, no guess.
- Unit grounding itself still uses the existing ontology-served resolvers
  `prism_provenance::units::span_value_has_resolved_unit` / `span_contains_resolved_unit`
  — the resolvers the file's own `QudtUnit` doc said to use.

Evidence: `claims::tests::glued_letters_redeem_only_through_the_facts_own_unit`
(11 glyph forms stamp when the term is supplied; silence refuses the same
sentences; a DIFFERENT supplied unit — kPa against an MPa page — does not redeem;
designation digits and digit-glue stay refused even with a unit).

### 1.3 `LABEL_WORDS` (44 entries), 1.4 `ABBREV_LABEL_WORDS`, 1.5 `LIST_CONTINUATIONS`

English structure words ("table", "figure", "ref", …) and materials-testing
jargon ("specimen", "batch", "coupon", "grade", …) deciding that a number after
such a word is a citation label, plus the abbreviation list used to keep
"Fig. 2" inside one span and the English conjunction list ("and", "or", "to",
"through") driving the label-list walk-back.

**Deleted outright** — see section 2. Every function that existed only to serve
them is deleted with them: `preceding_word_is_label`, `chain_ends_in_unit`,
`walk_comma_items`, `trailing_numeric_lexeme_start`, `leading_number_len`,
`trailing_word`, `period_ends_abbreviation`, and the `RefusalGuard::Label`
variant itself.

---

## 2. Checks deleted outright, and why no domain-independent rule replaces them

### The Label check (`RefusalGuard::Label`)

Which words introduce citation labels is **domain AND language** knowledge: a
German paper writes "Tabelle 1", "Abb. 3", "Probe 5"; a pharma paper labels
samples with vocabulary no materials list ever carried. The corpus measured the
list could not converge (the "label-word horizon" rows: Layer/Track/Build/Heat/Lot
stamped at HEAD while likelier than Inset 2), and every entry added cost
glued-recall drops. The brief's suggested structural rule — "a token the document
itself uses as a caption prefix" — needs thresholds and shape heuristics that are
themselves invented determinism, and it still could not tell "Probe 5" (specimen
label) from a measurement. No honest structural rule exists, so:

- the check is deleted; the matcher now reports co-occurrence honestly and the
  label-vs-measurement judgement moves to the ontology and the re-checking model;
- the **Citation guard survives** — bracketed markers `[12]`, `(12)`, `{12}` are
  punctuation structure, not vocabulary;
- span splitting lost the abbreviation exception: every sentence-final period
  splits (except in-token decimals like `1.5` / `MPa.m^0.5`, which are
  structural). Consequence, recorded honestly: "Refs. 25, 26" now splits at the
  period and the bare numbers strand without the subject → they drop as `NoSpan`
  (the right ground truth, by a different mechanism, with no guard name — the
  corpus rows say so).

### The compiled sign check's NAME side

`is_nonnegative_quantity` matched the object's NAME (exact strings, suffixes,
exception words). Deleted entirely: the guard now reads `GuardPolicy::quantity_sign`
and nothing else. A negative claim against an undeclared quantity stamps — that is
the permissive, honest direction (round 14 item 5 measured the open set of
non-negative quantities as UNBOUNDED; no enumeration in Rust closes it, and every
entry would be one domain's vocabulary imposed on every customer).

### `period_ends_abbreviation` (the `ABBREV_LABEL_WORDS` consumer)

Deciding which trailing periods abbreviate English label words ("Fig.", "Ref.",
"Eq.") is language knowledge; a German "Abb." or "Glg." would need another list.
Deleted; see span-splitting consequence above.

---

## 3. What the ontology must supply for the sign constraint

The Rust read-path is complete (`Ontology::quantity_sign_domain` →
`quantity_sign_for_fact` → `GuardPolicy::quantity_sign` → `RefusalGuard::SignDomain`).
For the check to fire, an ontology must declare, **per quantity kind**:

1. An annotation on the quantity class or its dimensional parent — a boolean
   "non-negative by definition" (e.g. a `prism:nonNegativeQuantity "true"` style
   annotation in the promoted TTL artifact, or an override of
   `quantity_sign_domain` in a custom adapter).
2. Resolution from the fact's quantity identity: the predicate IRI the reader
   bound (preferred) or the extraction label.

The **bundled EMMO artifact carries no such annotation today, and none was
faked**: materialising one would re-hardcode materials physics into a new
artifact (sha256-manifested, ESA-delivered) — the same failure in a new place.
EMMO therefore answers silence and the sign check is inert for EMMO facts, which
is correct: the facts store carrying status, and a later ontology-authoring
change (annotation channel + re-materialisation, or an adapter override) turns
the check on **without any Rust edit** — which is precisely the customer story.

---

## 4. Second task — the evidence is no longer discarded

`SupportRefusal::Guarded { guard, span }` already recorded which named guard
refused and the exact span examined. The branch had already lifted the matcher
API to return it; this change finishes the plumbing so it reaches persistence:

- `text_extract::numeric_fact_grounding` now returns
  `Result<String, GroundingRefusal>` where
  `GroundingRefusal::Guarded { guard, span }` keeps the pair structured (all
  other failures are `GroundingRefusal::Unsupported(String)`). No more
  guard+span folded into one prose string at the source.
- The pass-through compatibility alias `numeric_tolerant_supporting_quote_or_refusal`
  was deleted with its callers rewired to `supporting_quote_with_numeric_tolerance`:
  one numeric matcher, one structured-refusal contract, no parallel old path.
- `repair::tier_subject_normalization` and `repair_worker` gate 5 persist it: a
  guarded withdraw now rides `withdraw_with_evidence`, putting the exact examined
  span into the repair ledger's existing `evidence` column (`RepairDisposition.evidence`,
  stored by `ProvenanceStore`), with the guard named in the reason. This is the
  existing evidence channel — the same one the accept path uses for the
  supporting span, and the same store whose `prov_assertion_evidence` rows
  already carry `evidence_span`/`line_start`/`line_end` for agent citations.
  Nothing parallel was invented.
- Evidence in tests: `repair::tests::a_guarded_grounding_refusal_persists_the_guard_and_span_as_evidence`
  and `text_extract::tests::a_guarded_grounding_failure_names_the_guard_and_keeps_the_span`.
- Condition values are matched with the condition's OWN unit term supplied to
  the policy (`conditions_grounded_in_span`), exactly as the fact's own value
  receives the fact's unit — reader/ontology knowledge, never a compiled
  lexicon. The sign domain stays Unspecified for conditions: a condition is a
  separate quantity from the fact's own, and inferring its sign from the name
  is the hardcoding this branch deletes.

---

## 5. Tests whose meaning changed

Verdict per family — did the test pin a REAL property (kept, re-expressed) or the
hardcoding itself (rewritten)? Every rewritten row/test carries a
`CONTRACT CHANGE (de-hardcoding)` marker in its body saying which.

**Kept, mechanism unchanged (real properties):** substring/boundary pins,
comma-grouping, decimal-point guards, sign-flip refusals, en-dash "not a sign
glyph", Range (digit/dash/digit on every glyph), Citation (bracketed/paren/brace
markers, dash-walk glyphs), InsideName (designation digits, spaced and
dash-class), table row-span separation (cross-row claims), value-list recall,
true-negative recall on every minus glyph, UTF-8 advance-bug pins, NoSpan ↔
MissingQuote mapping.

**Re-expressed with ontology-served inputs (real property, new source):**
- Corpus sign-domain pins (hardness/density/grain size/yield strength, the UTS
  separator shapes, negative UTS ranges, the round-14 open-set tripwires): now
  `case_with(nonnegative_policy(), …)` — they pin the READ PATH, not a compiled
  entry. A paired KNOWN row records the other half: under a silent ontology the
  same negative stamps.
- Corpus glued-unit family ("950MPa", "1073K", "50µm", "980oC", "1000rpm",
  "2.95Å", "5bar", "72F", "50l", "5wt%"): now `case_with(unit_policy(…), …)` —
  stamps through the fact's own term, as the reader supplies it.
- SeparatorDash bracketed shape (residual_stress): `unit_policy("MPa")` supplies
  what `UNIT_TOKENS` used to fake; the row also documents that without a term
  the shape is inert (lib test pins that direction).

**Rewritten — the tests pinned the hardcoding itself:**
- Lib: `table_figure_ref_label_numbers_are_not_support`,
  `section_and_kindred_label_numbers_are_not_support`,
  `label_list_numbers_after_a_conjunction_are_not_support`,
  `comma_separated_reference_list_numbers_are_not_support`,
  `abbreviated_label_numbers_are_not_support`,
  `dotted_and_space_separated_label_lists_are_not_support`,
  `sample_and_run_label_numbers_are_not_support`,
  `every_kept_label_word_refuses_its_number`,
  `unit_exemption_requires_a_space_before_the_unit` → replaced by
  `label_words_have_no_special_standing_in_the_matcher` (the matcher reports
  co-occurrence; judgement moved out of Rust) and
  `methods_prose_numbers_with_spaced_units_still_stamp` (recall kept).
- Lib: `f_and_l_recall_initials_stamp_glued_units_again` →
  `glued_letters_redeem_only_through_the_facts_own_unit`;
  `sign_domain_matches_head_noun_suffix_without_over_refusal` →
  `sign_domain_reads_the_ontology_not_the_quantity_name` (pins silence-inertness
  AND policy-driven refusal; proves the name is never consulted).
- Corpus label/specimen families (Table/Figure/Ref/Sample/Run + the 19-word
  round-9 family + label-list dash walks): flipped to MustStamp with CONTRACT
  CHANGE reasons — ground truth ("a label is not a measurement") still holds in
  the world; the matcher no longer pretends to know it.
- Corpus KNOWN rows satisfied by the removal (markers stripped, re-recorded):
  "sample 980°C cycle" (the degree sign never needed redemption; the deleted
  label word refused it) and "4a and 4b" sub-panel letters (only the deleted
  ampere entry redeemed them — the gap closed by removal, not growth).
- "cross-section 10 mm" and "batch 25 kg" spaced rows: flipped — position/lot
  semantics are judgements the matcher must not encode; the glued twins keep
  MustDrop (silence refuses glued letters) with reasons stating the mechanism.
- "Refs. 25, 26" / "Refs. 25–27 and 28" (7 rows): back to MustDrop, new
  mechanism — abbreviation splitting strands the numbers (NoSpan), no guard name.

**Deleted, not rewritten:** `every_kept_label_word_refuses_its_number` (44
cannot-fail vocabulary pins) — its whole subject matter is gone by design.

---

## 6. Honest regressions (recorded, not hidden)

- Under a silent ontology (today: all of them), negative claims against
  non-negative quantities stamp as support. That is the point of the branch: the
  fact stores with status; the ontology author turns the note back on.
- Label numbers beside a subject ("Sample 5 of Ti-6Al-4V") report support under
  silence. Same disposition.
- Abbreviation periods split spans; a few shapes lose recall or drop unnamed
  (NoSpan). Recorded in the corpus reasons.

---

## 7. Files changed

| File | Change |
|---|---|
| `crates/provenance/src/emmo.rs` | `QuantitySignDomain` type |
| `crates/provenance/src/lib.rs` | export |
| `crates/retrieval/src/claims.rs` | vocabulary deleted; `GuardPolicy`; guards rewired; compatibility alias deleted; header/test rewrite |
| `crates/retrieval/tests/claim_corpus.rs` | `policy` per row; `case_with`/`known_with`; rows re-expressed |
| `crates/ingest/src/ontologies.rs` | `Ontology::quantity_sign_domain` (+ silence test) |
| `crates/ingest/src/text_extract.rs` | `GroundingRefusal`, `quantity_sign_for_fact`, policy threading; condition values matched with their own unit term |
| `crates/ingest/src/repair.rs` | `withdraw_with_evidence`, structured refusal handling (+ test) |
| `crates/ingest/src/repair_worker.rs` | same on the model tier; silent policy where no fact knowledge exists |

---

## 8. Gate output (exact)

Final gate, re-run end to end after the last cleanup pass (alias deletion,
condition-unit threading):

```
$ cargo fmt -p prism-retrieval -p prism-ingest
(exit 0)

$ cargo test -p prism-retrieval -p prism-ingest -p prism-provenance
     Running unittests src/lib.rs (target/debug/deps/prism_ingest-f375e9dbf94b5ef8)
test result: ok. 340 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 2.46s
     Running tests/ontology_induction.rs (target/debug/deps/ontology_induction-264ab4292fe12166)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
     Running unittests src/lib.rs (target/debug/deps/prism_provenance-33c34211e5df8bac)
test result: ok. 127 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 5.76s
     Running unittests src/lib.rs (target/debug/deps/prism_retrieval-64c661429de61ac6)
test result: ok. 117 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.12s
     Running tests/capability_declarations.rs (target/debug/deps/capability_declarations-1001b9a2de1d555a)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
     Running tests/claim_corpus.rs (target/debug/deps/claim_corpus-d7b105e8df4ba932)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/engine_integration.rs (target/debug/deps/engine_integration-90cd0ce39f8a81da)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
     Running tests/failure_taxonomy.rs (target/debug/deps/failure_taxonomy-dcbe265cd564b4fc)
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s
     Running tests/novel_adapter_selection.rs (target/debug/deps/novel_adapter_selection-cfb0eb496470e4d4)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/pagination_completeness.rs (target/debug/deps/pagination_completeness-c5e3972db742fb7e)
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.01s
     Running tests/relevance_filtering.rs (target/debug/deps/relevance_filtering-c81e836064f6249b)
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/sweep_resume.rs (target/debug/deps/sweep_resume-1a6859b4661e3499)
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 15.02s
   Doc-tests prism_ingest
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests prism_provenance
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests prism_retrieval
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
(exit 0)

$ cargo clippy -p prism-retrieval -p prism-ingest --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.32s
(exit 0)
```

Additionally verified (beyond the gate): `cargo check --workspace` clean,
`cargo test -p prism-cli` green (223 + integration suites), and
`python scripts/check_no_cjk_in_agent_artifacts.py` → no language drift
(re-verified after the final cleanup pass).

Baseline at start of this work (same gate): 337+6+127+127+12+1+3+8+3+13+2+4
tests passing, clippy clean. The retrieval lib delta (127 → 117) is the test
rewrite in section 5: 12 vocabulary-pinning tests removed/replaced by 5
contract tests; ingest gained 3 (evidence plumbing + silence pin).
