# READABILITY.md — Comprehension pass, zero behaviour change

Branch `feat/annotate-not-refuse`. This was a **reading-and-naming pass**, not a
fix pass. Every bug below was found and **deliberately left in place**; the
proof that nothing changed behaviour is the gate at the bottom: same test count
(3066), same result (0 fail), clippy clean — before and after.

**Read fully:** `ingest/text_extract.rs`, `ingest/paper_agent.rs`,
`agent/agent_loop.rs` (production 1–3009), `ingest/local_facts.rs`,
`retrieval/claims.rs` (production 1–1660).
**Read structurally + spot-checked:** `provenance/emmo.rs` (11,450 lines;
~5,000–11,450 are tests). See "Coverage limits".

An honest headline before anything else: **this code is far better documented
than "spaghetti".** The recent annotate-not-refuse work left dense WHY-comments,
measured rationale, and (in `claims.rs`) an explicit "RECORDED, NOT FIXED" bug
ledger. The owner's pain is real but it is mostly *undiscoverability*, not
*absence of care* — the knowledge is in the files, just not reachable without
reading 30,000 lines. The two new artefacts (`ARCHITECTURE.md`, and the bug list
below) are the attempt to make it reachable.

---

## 1. What I renamed, and why

Renames were restricted to **local variables** (compiler-checked, cannot change
behaviour, cannot touch serialization). I did **not** rename any `pub` item,
struct field, or enum variant: `MaterialFact`, `LocalFact`, `ExtractedClaim`,
`PaperFactProposal`, etc. are serde-serialized into the database and across the
wire, so renaming a field would change the stored/JSON shape — a behaviour
change.

| File | Before | After | Why |
|------|--------|-------|-----|
| `agent/agent_loop.rs` `routing_query` | `q` | `query` | `q` says nothing; it is the routing query being built. |
| `agent/agent_loop.rs` `run_turn_inner` | `for tc in &tool_calls` | `for tool_call in &tool_calls` | `tc` is an initialism you must decode on every use. |
| `agent/agent_loop.rs` `pinned_by_relevance` | `.map(\|(i, n)\| (n.as_str(), i))` | `.map(\|(position, name)\| …)` | `i`/`n` forced the reader to infer index-vs-name. |
| `retrieval/claims.rs` `scan_number_evidence` | `while let Some(rel) = …find(&needle)` | `relative_offset` | `rel` reads as "relationship" in a file about claims; it is a byte offset. |

Everything else I examined that looked short (`e` for entity/error in closures,
`h` for a hasher, `i` for a loop index, `n` for a count) is idiomatic and
locally obvious, so I left it — renaming those is churn, not clarity.

## 2. Comments added or removed

**Added: none.** I could not find a spot where a genuinely-missing WHY-comment
was the problem. Where the code is subtle (`claims.rs` guards, `agent_loop.rs`
compaction/heartbeat, `text_extract.rs` anti-ratchet), the WHY is already there
and is good.

**Removed: none.** I found no "restates the code" noise comments (`// increment
i`). Every comment I read was a reason, a measurement, or a trap-warning — the
valuable kind. Deleting any would be a loss. (This is itself a finding: the
house style is already correct; the gap is discoverability, addressed by
`ARCHITECTURE.md`.)

**Preserved verbatim:** the `claims.rs` module doc and the many "RECORDED, NOT
FIXED / ROUND N" notes. These are the project's bug memory; I consolidated them
into Section 4 but did not edit them in source.

## 3. Functions split

**None split.** The one candidate is `run_turn_inner` (`agent_loop.rs`,
~1,275 lines). I left it whole deliberately:

- It is already navigable — every phase is a numbered banner (`2a` budget,
  `2b` tool selection, … `h1`–`h13` per tool call).
- It threads ~20 pieces of mutable state and exits via early `return`/`continue`
  in many places. Splitting it into helpers would mean passing that state around
  or restructuring control flow — exactly the kind of change that can silently
  alter behaviour, which this pass forbids.

Per the brief: a long function with clear sections beats six badly-named
helpers. This is that case.

## 4. BUGS FOUND AND DELIBERATELY NOT FIXED  *(the valuable list)*

Grouped by file, with locations. None of these were touched.

### `crates/agent/src/agent_loop.rs`

- **B1 — possible panic: byte-slice of a non-JSON error string.**
  `summarize_tool_result`, error fallback, ~line 705:
  `let preview = if content.len() > 60 { &content[..60] } else { content };`
  `&content[..60]` slices at **byte** 60; if that lands mid-UTF-8 character it
  panics and aborts the whole turn. The success path uses the char-boundary-safe
  `first_line`; this branch does not. Reachable whenever a tool errors with a
  non-JSON message longer than 60 bytes containing multibyte text near byte 60.

- **B2 — `result_store` is written but never read (dead state + unmet promise).**
  Created at ~1853, inserted into at ~366, ~2884, ~2935; **never read**, dropped
  at turn end. Yet `process_large_result`'s truncation message tells the model
  "the FULL result is in durable memory; call recall(...) to pull the rest back".
  Nothing consults `result_store`, so either the map is dead or that recall
  promise is not fulfilled by this mechanism. Kept because live tests
  (`test_process_large_result_small/large`, ~4180/4189) call
  `process_large_result(content, &mut store)` — deleting the parameter would edit
  tests and change the gate's test count. See also B3.

- **B3 — `uuid_hex8` is not a UUID and is a small collision space.** ~line 353.
  Named "uuid" but is a 32-bit timestamp-derived hash
  (`(ts ^ (ts>>32)) & 0xFFFF_FFFF`). Two large results whose truncated
  timestamps collide overwrite each other. Moot today only because `result_store`
  (B2) is unread; the name overstates the guarantee.

### `crates/ingest/src/text_extract.rs`

- **B4 — `unwrap_soft_line_breaks` is built, tested, and wired to nothing.**
  ~line 739. Its own doc says "Compute this ONCE per document per run and pass it
  as `DocumentContext::document`", but **no production path calls it** (verified
  repo-wide): neither the fresh agentic path nor the repair path applies it.
  Consequence: a subject name soft-wrapped across PDF lines (the hyphen-wrap case
  it exists for) can never ground through the repair/grounding path. Part
  hanging-chad, part latent functional gap. Cannot delete — live tests call it.

- **B5 — `annotate_cited_fact` mislabels a value-carrying fact as "value-less".**
  ~lines 683–690. The `else if policy.assertion_grounding ==
  AssertionGrounding::DropUnreviewable` arm is reached by **any** fact not caught
  by the first arm — including facts that HAVE a `value`. Such a fact is stamped
  `ModelAsserted` with the reason *"the policy does not promote a value-less
  agent proposal"*, which is wrong (the fact has a value) and arguably outside the
  policy's documented scope (it is described as governing value-less assertions).
  **Latent:** production always uses the default `ReviewWithModel`;
  `DropUnreviewable` is only exercised by one test. No live impact today, but the
  reason string and the scope are mismatched.

### `crates/retrieval/claims.rs`  *(already self-documented; consolidated here)*

The module header and guard docs carry a "RECORDED, NOT FIXED" ledger. I confirm
each is real and leave it. In rough order of severity:

- **B6 — value is never tied to the predicate** (round 9). "The Ti-6Al-4V UTS was
  950 MPa and the yield strength 880 MPa" claimed as `yield_strength = 950`
  STAMPS: the span holds subject + object word + number, and nothing asks which
  property the number belongs to. Right number, wrong property, perfect provenance.
- **B7 — `validate_and_stamp` checks a unit EXISTS, not that it matches the
  prose** (round 9). A `950 GPa` claim against "950 MPa" text stamps with a
  verbatim quote.
- **B8 — numeric decorations are invisible** (round 12). "950 ± 30 MPa" stamps the
  tolerance `30` as a value (the ± is never handled); the same family covers
  digit–dash–letter locants ("3-point", "2-propanol", "N-methyl-2-pyrrolidone").
- **B9 — `NoSpan` conflates two different faults** (round 7). "No needle form of
  the value matched anywhere" (a matcher over-refusal) surfaces as `NoSpan` →
  `MissingQuote`, which is defined as the *model's* fault — so over-refusals get
  misfiled as hallucinations.
- **B10 — `NumericValueWithoutUnit` returns before the quote check** (round 7), so
  a unitless numeric fact's drop carries no guard/span and is invisible in the
  drop record.
- **B11 — "first refusal" is first in needle-form order, then position**
  (round 8), so for multi-needle values (≥1000 or negative) the reported guard is
  not necessarily the occurrence a reader meets first.
- **B12 — spaced / negative ranges still stamp endpoints.** "950 to 1100",
  "950 – 1100" (spaced dash), "-950 to -400": the range guard only sees
  digit/dash/digit adjacency, so one space or a leading sign defeats it.
- **B13 — line-start separator shape is a known live fabrication.** "-950 MPa was
  recorded" is deliberately NOT refused (locally indistinguishable from a genuine
  line-start minus); carried as a KNOWN corpus row.
- **B14 — (minor, clarity) `cap_at_literature` first branch is redundant.**
  `rank(claimed) <= rank(EVIDENCE_RESEARCH) && claimed == EVIDENCE_RESEARCH` —
  the second conjunct already implies the first. Harmless; noted only because it
  reads as if it does more than it does.

### `crates/ingest/src/paper_agent.rs`

- **B15 — (note, not a defect) `tool_result_budget` truncates.**
  `window / 100 * 16 * 4` divides before multiplying, dropping up to ~99 chars of
  budget (32768 → 20928, not 20971). Documented as "rough" and pinned by a test,
  so it behaves as intended; flagging the truncation order in case precision ever
  matters.

### `crates/ingest/src/local_facts.rs`

- None found. This mapper is tightly tested; the anti-smearing attribution logic
  (a value on a shared property node is stored for nobody and reported) is correct
  and well-pinned.

### `crates/provenance/src/emmo.rs`

- None found in the areas reviewed (see coverage). The security-sensitive pure
  logic — `is_relay`/`is_mesh_tenant`/`origin_source_key_for` (peer-origin
  namespacing kept disjoint from local keys), `doi_suffix`, `canonical_file_path`
  / `remove_dot_segments` — is careful and correct on inspection.

## 5. Dead code: deleted vs. suspected

**Deleted: none.** Because `cargo check`/`clippy` pass clean with `-D warnings`,
the compiler has already removed every unused *private* item — there are no
unused private functions to delete. Every dangling *public* candidate below is
kept alive by a **live test**, so deleting it would edit tests and change the
3066-test count the gate forbids. All are therefore **report-only**:

| Item | File | Status |
|------|------|--------|
| `unwrap_soft_line_breaks` | `ingest/text_extract.rs:739` | `pub`, no production caller, only tests. **Also bug B4.** |
| `run_paper_agent` (sample=1 wrapper) | `ingest/paper_agent.rs` | `pub`, no production caller, only tests. Production uses `run_paper_agent_sample`. |
| `extract_facts_from_text` | `ingest/text_extract.rs` | `pub`, only tests. Production uses `…_with_ontology`. |
| `extract_facts_from_text_with_policy` | `ingest/text_extract.rs` | `pub`, only tests. |
| `result_store` (state, not fn) | `agent/agent_loop.rs` | write-only, never read. **Bug B2.** |

**Pattern worth naming for the owner:** these are not "functions nobody wrote
tests for" — they are the opposite. They are *aspirational or half-wired*
helpers that have tests but no production call-site. `unwrap_soft_line_breaks`
is the clearest "hanging chad": fully built, fully tested, never connected. That
specific shape — polished, green, and dangling — is harder to spot than a stub,
which is presumably why it survived.

## 6. Things I could not fully determine

- **`provenance/emmo.rs` full-line review.** I read it structurally and
  spot-checked the pure-logic / security-sensitive helpers, but did not read all
  ~5,000 production lines. It is the most heavily-tested file in the repo, which
  lowers the risk, but I am not claiming a clean bill of health for every line.
- **Does `recall` actually return an oversized tool result?** Related to B2. The
  truncation message promises `recall(...)` restores the full result. Whether the
  provenance hook independently records full tool outputs (making `result_store`
  redundant) or nothing does (making the promise false) could not be confirmed
  from `agent_loop.rs` alone. Flagged rather than guessed.

## 7. Gate (pasted)

Run with `CARGO_TARGET_DIR=/Users/siddharthakovid/Downloads/prism-unmuzzle/target`.

```
$ cargo fmt --all            # then: cargo fmt --all --check
cargo fmt --all --check: clean (exit 0)

$ cargo test --workspace
… 92 test binaries …
test result: ok  (every binary)
TOTAL PASSED: 3066
TOTAL FAILED: 0
(the only grep hits for "panicked"/"FAILED" are test NAMES and JSON fixtures,
 e.g. `notebook::tests::exception_is_reported_not_panicked ... ok`)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.87s
clippy exit: 0, warnings: 0
```

**Before the pass (freshly measured baseline): 3066 passed, 0 failed, clippy
clean. After the pass: 3066 passed, 0 failed, clippy clean. Test count and
result unchanged.**
