# Fixing what the two plugin audits found

Branch `feat/annotate-not-refuse`. Nothing committed, nothing pushed.

Source audits: `PLUGIN_AUDIT_PASS1.md` (what PRISM's plugin points actually
are) and `PLUGIN_AUDIT_PASS2.md` (what to take from the DeepSeek harness).

## Gate

```
$ cargo fmt --all
(exit 0)

$ cargo test --workspace
3066 passed; 0 failed
(exit 0)

$ cargo clippy --workspace --all-targets -- -D warnings
(exit 0)
```

Baseline when the brief was written was 3053 passing. The +13 are the new
regression tests listed per item below. The clippy exit code was captured
directly from `$?`, not read through a pipe.

## Item 1 — Overflow recovery

**Done.** A real run on a 36-page paper had reached turn 41, the provider
answered `request (32786 tokens) exceeds the available context size (32768)`,
and the `?` discarded every proposal the run had already made — nineteen
minutes and the whole document's facts lost to an error the loop could have
answered.

- `crates/llm/src/overflow.rs` — recognition patterns for provider
  context-overflow errors, translated from the DeepSeek harness (MIT).
- `crates/ingest/src/paper_agent.rs:177` — `PaperAgentStopReason::Overflow`,
  set at `:358` and `:369`. The loop classifies the overflow, forces an
  elision, retries **once**, and if it still fails stops with the distinct
  reason while **keeping the proposals gathered so far**.
- Regression test at `paper_agent.rs:2319-2349` asserts the run stops with
  `Overflow` *instead of* returning an error, and that the proposals survive.

## Item 2 — Elision budget from the model, not a constant

`MAX_TOOL_RESULT_CHARS = 24_000` was hand-fitted to the one model that died.

`paper_agent.rs:341` now derives the budget via
`tool_result_budget(model.context_window())`, reading the real window from
`LlmClient::context_window`. The constant survives only as the fallback for a
model that does not report its window (`:509`) — a local 32k model and a 1M
hosted model no longer share a hardcoded cap.

## Item 3 — `quantity_sign_domain` made real

Pass 1 established the doc claim at `ontologies.rs:299-306` was false three
ways: no artifact slot, no adapter override, and a consumer hardwired to
`active(None)`.

Took the **preferred** fix, not the delete-the-claim fallback:

- The induced-TTL format carries an optional `prism:signDomain` annotation.
- `InducedVocabulary::quantity_sign_domain` (`induction/register.rs:110`)
  serves it, resolving whichever identity the fact carried — class IRI,
  `prefLabel`, or extraction label — via `class_iri_by_name`, then walking
  declared ancestors so a declaration on a **dimensional parent** also
  applies. No declaration anywhere on that path answers `None`: silence,
  never a guess.
- The run's ontology is threaded into `quantity_sign_for_fact` in place of
  `active(None)`.

Test `text_extract::tests::grounding_reads_the_sign_domain_of_the_runs_ontology`
proves both directions: a negative claim against a declared non-negative
quantity is refused with `RefusalGuard::SignDomain`, and the same fact grounds
unharmed under an ontology that declares nothing.

A customer can now supply sign domains with zero Rust edits, so the doc claim
is true as written.

## Item 4 — Measurement relations from the ontology, not string literals

`local_facts.rs:138-212` matched the literals `"HAS_PROPERTY"` /
`"PROCESSED_BY"` / `"CONTAINS"`, so any ontology that does not happen to use
those exact English tokens had its numeric values collapse into untyped
generic edges, while any ontology that did use them inherited measurement
mapping by lexical accident. The defect is the literal match itself; the
particular vocabulary a probe used to expose it is incidental.

`local_facts.rs:140-147` now consults the trait: `measurement_relations()`,
`phase_relations()`, `quantitative_labels()`. No domain vocabulary was added
to Rust, and there is **no fallback to the literals** — an ontology that
declares nothing reports the numeric claim it cannot store rather than
guessing. The `"HAS_PROPERTY"` strings that remain in that file are test
fixtures constructing edges with EMMO's own declared labels.

Verified domain-agnostic: every non-English/non-materials string in the
touched crates (`text_extract.rs`, `paper_agent.rs`, `ontologies.rs`) sits
inside a `#[cfg(test)]` module. Those fixtures exist only to prove a
*declaring* ontology is obeyed and a *silent* one is not second-guessed —
they are arbitrary stand-ins, not a domain PRISM now supports, and no
vocabulary from them reaches shipped code.

## Item 5 — Text-path tenant composition

`crates/cli/src/main.rs:7561` and `:8121` now compose the tenant with
`prism_ingest::ontologies::storage_tenant(LOCAL_TENANT, ontology.id())`, the
same shape the tabular path already used. Two ontologies no longer blend on
the paper path.

## Item 6 — Python plugin failures are loud

`app/plugins/loader.py` logged nothing when a plugin failed to import. It now
`logger.exception`s each failure with the plugin name and the error — both at
the per-entry-point level and for a total discovery failure — and keeps
loading the rest. It is the healthiest runtime extension door in the product
and it no longer fails silently.

## MIT attribution

`NOTICE:41-52` names the DeepSeek harness (MIT, Copyright (c) 2026 DeepSeek),
identifies the single derived file `crates/llm/src/overflow.rs` and the exact
upstream source it came from (`packages/llm/llm/src/error.ts`,
`isContextWindowExceededError`), and points at `LICENSES/DEEPSEEK_MIT` for the
full text. Design ideas taken from the harness carry no obligation and are not
claimed as derived.

## Defects found in the delivered work and fixed

Both were left by interrupted runs, not by the audits:

1. `induction/register.rs:560` — `useless use of format!` on a string
   literal. Clippy error under `-D warnings`. Fixed; the binding is now a
   plain `&'static str`.
2. `crates/retrieval/tests/scratch_sign_diag.rs` — an untracked printf-debugger
   whose body is an unconditional `panic!`, and whose own header read
   "TEMPORARY … Deleted before the gate — never lands in the tree." Its author
   was killed before it could clean up. Deleted. Because `cargo test` aborts
   at the first failing binary, this one file was truncating the reported
   workspace count.

Also improved: the second assertion in
`grounding_reads_the_sign_domain_of_the_runs_ontology` was a bare
`assert!(… .is_ok(), "…")` that hid *which* refusal fired. It now prints the
refusal on failure. No contract change.

## Judged too large — reported, not fixed

**`crates/cli/src/main.rs:9040` is a third instance of the item-5 defect.**
`record_platform_ingest_provenance` writes `tenant: "local"` on the platform
holistic-ingest path, so two ontologies blend there exactly as they did on the
paper path.

It was not in the brief and, unlike the two named sites, it is not a
one-liner: the function's signature is `(path: &Path, steps: &[(String,
serde_json::Value)])` and **no ontology is in scope anywhere in it**. Fixing
it means threading an ontology through a path where the ontology is chosen
server-side by the platform, which is a design question about what the local
store should record for a remote extraction — not a mechanical edit. Left for
an owner decision.

(The remaining `tenant: "local"` at `:17603` is a test fixture and is correct.)

## Contract changes

No test had its meaning changed by these fixes. The tests whose contracts
changed earlier on this branch are inventoried in `AGENTIC_EXTRACTION.md` and
`DEHARDCODE_CLAIMS.md`.

## Housekeeping question for the owner

`notes.md` at the repo root is untracked working-notes from the 2026-08-15
agentic-extraction run. Its content is superseded by `AGENTIC_EXTRACTION.md`,
which it explicitly defers to. Should it be removed?

## Process note

The three dispatched runs of this brief did not die as believed — all three
took and ran concurrently in the same working tree, sharing one
`CARGO_TARGET_DIR`. They collided: one ran `cargo clean`, one `rm -rf`'d
`target/debug`, and one `pkill`ed `cargo test --workspace`. Every build error
observed during that window was that race, not a code defect, and the
`target/` tree had grown to 110 GB against 15 GiB free. The tree was wiped and
rebuilt from scratch to produce the gate above. Future dispatches against this
repo must be serialized to one agent.
