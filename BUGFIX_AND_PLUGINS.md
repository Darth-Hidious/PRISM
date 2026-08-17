# Bugfix pass (READABILITY B1–B15) + confirmed defects + the standardised plugin contract

Branch `feat/annotate-not-refuse`. No commits made; working tree only.
Gate at the bottom: **3175 workspace tests pass, 0 fail, clippy clean, fmt
clean** (baseline at brief time: 3074 pass).

---

## Part 1 — the READABILITY.md ledger, B1–B15

| Bug | Verdict | What was done |
|-----|---------|---------------|
| **B1** byte-slice panic in `summarize_tool_result`'s error fallback | **FIXED** | `agent_loop.rs` error branch now uses the char-boundary-safe `first_line(content, 80)` helper (same helper the rest of the file uses) instead of `&content[..60]`. Regression test: a 40× multibyte error string at the old panic point. |
| **B2** `result_store` written but never read while the truncation message promises `recall(...)` | **FIXED (by deletion + promise made true)** | Investigation showed the promise was ALREADY fulfilled — but not by `result_store`: the provenance post-hook (h6) records every tool call's complete output to the durable Turso store *before* h8 truncates, and the `recall` meta-tool serves it by id/query. The in-memory `result_store` (dropped at turn end) was a redundant write-only half-path that could never serve "durable memory" anyway — deleted, per "delete what you replace". The residual untruth is also fixed: `recall(id=...)` used to clip at 8 000 chars — *exactly the preview size the model had already seen* — so "pull the rest back" returned nothing new. By-id fetches now return up to `RECALL_BY_ID_MAX_CHARS = 64 000` chars (≈2× the loop's 30k inline threshold; returns whole every result the loop would have kept inline) and, when a record still exceeds it, names the exact remainder instead of presenting itself as whole. Tests: rewritten pins with CONTRACT CHANGE comments + a new by-id test proving a 40k result returns whole and a 100k one is honestly marked. |
| **B3** `uuid_hex8` is not a UUID and has a small collision space | **FIXED (deleted with B2)** | It existed only to key the deleted in-memory store. Its pinning test was deleted with a CONTRACT CHANGE comment; durable records use the provenance store's real ids. |
| **B4** `unwrap_soft_line_breaks` built, tested, wired to nothing | **FIXED (wired)** | Wired into its documented home — the repair tier — in both places a soft-wrapped subject could die: (a) `repair.rs` `tier_subject_normalization` now applies the unambiguous hyphen-wrap join *before* typography folding (order matters: folding destroys the line signature the join reads), and (b) `repair_worker.rs` Gate 5, where the subject is FIELD-FROZEN so no model correction could ever overcome a wrapped document. It is deliberately NOT wired into `DocumentContext::document` — that field's contract ("raw line boundaries are the canonical citation coordinate, must never be soft-unwrapped") is the newer, language-agnostic design, and the two doc comments contradicted each other; `unwrap_soft_line_breaks`'s doc now names its actual home. New tests: a hyphen-wrapped `Ti-6Al-\n4V` fact refused as SubjectNotNamed is now repaired-and-accepted with fields frozen. |
| **B5** `DropUnreviewable` mislabels value-carrying facts as "value-less" | **FIXED** | The arm keeps its behaviour (a caller opting out of review opts out of trusted status for ALL proposals — the value-carrying ones most of all, since nothing grounds the number after `propose_fact`), but the reason string now names the actual shape: value-carrying facts get "…an agent proposal carrying a value… the value was not grounded"; value-less facts keep the historical wording (still pinned by the existing test). The `AssertionGrounding` doc no longer claims the policy is value-less-only. New unit test pins both strings. |
| **B6** value never tied to the predicate ("UTS was 950 and yield 880" stamps `yield=950`) | **NOT FIXED — stated precisely** | Closing this needs predicate→value binding inside the span scan: deciding which property word a number belongs to requires either property vocabulary (exactly the compiled-in domain knowledge the module contract deleted, round 14) or a model-side binding change (the extraction prompt/schema). No domain-independent structural signal distinguishes "the number nearest the object word" from "the number that belongs to it" in the pinned corpus row. Remains pinned as the module's recorded KNOWN row; the scoreboard keeps it honest. |
| **B7** `validate_and_stamp` checks a unit EXISTS, not that it matches the prose | **NOT FIXED — stated precisely** | A unit-vs-prose check needs a mapping from the claim's unit term (a QUDT-style identifier) to the lexical forms a paper prints ("MPa", "N mm⁻²", "MN/m²") — a unit lexicon. The de-hardcoding contract deleted the Rust unit lexicon deliberately, and the ontology artifact format has no slot for unit lexical forms yet (same gap as PLUGIN_AUDIT_PASS1 §4.2). The honest fix is an ontology-declared unit-lexeme channel + a containment check in `validate_and_stamp`; that is a feature, not a bug patch, and is recorded here as the follow-up. |
| **B8** numeric decorations invisible ("950 ± 30" stamps the tolerance 30) | **HALF FIXED — the ± family; locants stated** | The plus-minus half is closed: a new `RefusalGuard::Uncertainty` refuses a number whose left neighbour (skipping spaces) is `±` (U+00B1) or ASCII `+/-` — mathematical notation, not domain vocabulary, same class as the dash class. The decorated VALUE (950) still stamps (corpus control row pinned). Both KNOWN corpus rows graduated to genuine MustDrop pins. The digit-dash-LETTER locant family ("3-point", "2-propanol") stays recorded: refusing digit-dash-letter outright would also refuse hyphenated unit spellings ("2-mm"), and telling them apart needs the unit lexicon only the ontology can supply. |
| **B9** `NoSpan` conflates "no needle form matched anywhere" with "no span had evidence" | **FIXED** | New `SupportRefusal::ValueNotRendered` (and `ClaimRejection::ValueNotRendered`): after the scan finds nothing, both refusal entry points check the whole block — if no rendered form of the value occurs anywhere, the drop is filed as ValueNotRendered instead of NoSpan/MissingQuote. `numeric_fact_grounding` surfaces it as a distinct Unsupported message that names the ambiguity honestly ("matcher lacks the rendering, reader converted units, or the value is invented") rather than blaming the model. Two old tests that pinned the conflated classification were updated with CONTRACT CHANGE comments (the property they protect — SignDomain must not mask the cause — is unchanged). |
| **B10** `NumericValueWithoutUnit` returns before the quote check | **FIXED** | `validate_and_stamp` now runs the quote checks (MissingQuote, QuoteNotInCitedBlock) BEFORE the unit refusal, so a unitless numeric claim with a FALSE citation is recorded as the false citation it is — the more severe fault no longer hides behind the unitless one. RECORDED note replaced with the fix note. |
| **B11** "first refusal" is first in needle-form order, then position | **FIXED** | `scan_number_evidence` gathers occurrences of ALL needle forms, sorts by byte position, and reports the guard of the positionally-first refusal — the occurrence a reader meets first. Overlapping spellings ("1350" inside "-1350") keep natural left-to-right order. The lexeme scanner (`scan_numeric_lexeme_evidence`) was already position-ordered and is unchanged. |
| **B12** spaced / negative ranges still stamp endpoints | **HALF FIXED — spaced-dash; word/double-dash stated** | `dash_range_endpoint` now tolerates whitespace around the dash glyph on both sides (forward for the low endpoint, backward for the high), across the whole `MINUS_CAPABLE_DASHES` class. All four spaced-dash KNOWN corpus rows graduated to MustDrop pins; a spaced unary minus ("was - 950") still stamps because a digit must sit on the far side of the dash. NOT fixed, deliberately: the WORD form ("950 to 1100" — a range connector word is language vocabulary; "950 bis 1100" would sail past an English guard; that input belongs to the ontology) and the double-dash negative form ("-950--400", rarer glyph soup, negative endpoints already covered by SignDomain where declared). Both remain in the module ledger. (My first draft of the spaced-dash guard had a relative-vs-absolute byte-offset bug that panicked mid-multibyte — the corpus caught it immediately; the fix keeps all indexing inside the trimmed slice.) |
| **B13** line-start separator shape is a known live fabrication | **NOT FIXED — deliberate, stands as recorded** | "-950 MPa was recorded" is locally indistinguishable from a genuine line-start minus ("-350 MPa was the surface stress"); round 16 decided this on the record and the KNOWN corpus row carries it. No new information changes that decision. |
| **B14** `cap_at_literature` first branch is redundant | **FIXED** | Simplified to exactly what it always computed: rank ≥ research → research, else indeterminate. Comment names the B14 cleanup. |
| **B15** `tool_result_budget` divides before multiplying (drops ≤99 chars of budget) | **NOT FIXED — behaves as intended** | `paper_agent.rs` `window / 100 * 16 * 4`: documented as a rough elision budget, pinned by a test, and a ~0.5% budget shift has no behavioural value worth churning a pinned invariant for. Flag stands as the readability note recorded it. |

Claims-corpus scoreboard after this pass: **MUST_STAMP dropped 0, MUST_DROP
stamped 0, KNOWN held 34, KNOWN graduated 6** (4 spaced-dash rows + 2 ±
rows) — every graduation was flipped to a pinned `case(...)` with a note
naming the fix, which is the scoreboard's designed workflow.

## Part 2 — the two confirmed defects

### `class_iri_by_name` exact-match vs normalised induction

**FIXED** (`crates/ingest/src/induction/register.rs`). The adapter now
inserts every class identity (IRI, prefLabel, extraction label) under
`normalize_label`, and `quantity_sign_domain` folds the query the same way
— the same normalisation `validate.rs` and duplicate detection use. "yield
strength" / "Yield_Strength" / "yield-strength" now resolve to the declared
sign domain of "Yield Strength"; an unknown quantity is still silence,
never a guess. New test pins the folded spellings (and documents that a
fully-concatenated lowercase word does not fold — consistent with duplicate
detection's fold, not a regression).

### `registry.register_plugin` leaves a half-state

**FIXED — all-or-nothing** (`app/plugins/registry.py`). `register_plugin`
snapshots all five sub-registry tables plus the provider-factory table
before calling the plugin's `register()`, and restores them on any
exception before re-raising — a plugin that registers three tools and
raises on the fourth leaves NOTHING behind, and the failure is recorded as
queryable state (`failed_plugins()`), not just a log line. This is the same
snapshot/rollback shape the search-provider loader's
`_guarded_plugin_import` already used for its factory table. The loader
records import-phase failures the same way; `bootstrap.py`'s outer
`except Exception: pass` around the plugin subsystem now logs loudly.
Python tests: partial-registration rollback (pre-existing tools survive),
provider-factory rollback, stale-failure clearing, loader failure
recording — 23 plugin tests pass.

## Part 3 — the standardised plugin contract

**The contract** (documented at the top of `app/plugins/registry.py`, the
Python plane's reference implementation), converged on the shape of the two
healthiest doors (the ontology registry and the Python entry-point loader):

1. **DECLARE** — every plugin carries an id + a source
   (`entrypoint:<name>`, `local:<file>`, a config entry, an artifact path).
2. **DISCOVER** — fixed, documented locations only; a missing location
   means zero plugins, never an error; a malformed declaration is refused
   loudly by name.
3. **FAIL** — loudly, named, isolated (the rest keep loading), and
   RECORDED as queryable state, not only a log line.
4. **LIST** — one inventory per plane, plus ONE aggregated surface:
   `prism plugins list` (human + `--json`), reachable from all three
   doors.

### What each plane changed

| Plane | Declare | Discover | Fail | List |
|---|---|---|---|---|
| **Python plugins** | module + `register(registry)` | entry points group `prism.plugins` + `~/.prism/plugins/*.py` | **NEW: all-or-nothing + recorded** (`failed_plugins()`); bootstrap no longer silent | **NEW: `app/plugins/status.py`** feeds the aggregated list |
| **Ontologies** | TTL artifact with `prism:` annotations | `.prism/ontologies/<id>.ttl` + `[ontology] id` | loud refusal naming what IS registered (already healthy); **NEW: broken catalog artifacts are named failures in the listing** | **NEW: `prism ontology list`** + the aggregated list's ontology section |
| **MCP servers** | `~/.prism/mcp.json` entry | config file | already loud+isolated per server; **NEW: `McpManager` records failed servers with errors (`failed_servers()`)** and the listing names statically-detectable failures (unsupported transport, missing command) | configured servers + detectable failures in the aggregated list |
| **Skills** | JSON/Markdown files | `~/.prism/skills/` | malformed files skipped, load errors reported by `discover_human_skills` | already listed (`list_skills` tool, `/skills list`); now also in the aggregated list |
| **Workflows** | YAML spec | `.prism/workflows/`, `~/.prism/workflows/` | per-file log-and-skip (already healthy) | already listed (`prism workflow list`); now also in the aggregated list |
| **Rego policies** | `.rego` file | `~/.prism/policies/`, `.prism/policies/` | compile failures fail CLOSED at evaluation (already healthy) | **NEW: discovered files in the aggregated list** (compile status honestly noted as enforcement-time, not faked) |

Where a plane cannot fully conform: **live MCP connection state** belongs to
the running agent session (the manager connects at startup); a fresh CLI
process honestly reports the CONFIG plus statically-detectable failures and
says where live state lives, rather than pretending to know. The in-session
record (`failed_servers()`) is queryable by code in the agent process.

### Reachability, per capability touched (the hard rule)

| Capability | Agent calls it | TUI user reaches it |
|---|---|---|
| Plugin inventory (all planes, loaded + failed) | `plugins` command tool — typed `RootSubcommand{list}` umbrella, ReadOnly, no approval, flag allowlist `--json`; spawns `prism plugins list` | `/plugins list` — "plugins" added to `CLI_BACKED_ROOTS` (the in-app CLI-backed passthrough, no exit-to-CLI) + `/help` line; tested in both test files |
| Python plugin status | same `plugins` tool (python plane section) | same `/plugins list` |
| Ontology inventory | `plugins` tool (ontology section), or `prism ontology list` via any CLI-backed route | `/ontology list` (ontology already a CLI-backed root) + `/plugins list` |
| MCP server inventory + detectable failures | `plugins` tool (mcp section) | `/plugins list` |
| Skill inventory | `list_skills` meta-tool (existing) | `/skills list` (existing) + `/plugins list` |
| `recall(id=...)` full-result fetch | `recall` meta-tool (existing, now genuinely full up to 64k with honest remainder marker) | recall happens via the agent the TUI already drives |
| Repair-tier soft-wrap rescue | automatic — part of the repair queue the agent/TUI already surface | same |

**One implementation, three doors**: the TUI's `/plugins list` and the
agent's `plugins` tool both spawn the same `prism plugins list` binary
path, so the surfaces cannot drift. All listing is local/offline; the
python-plane section follows the `prism tools` precedent (spawning the
interpreter to discover) and says so in its doc.

### Files changed for Part 3

- `app/plugins/registry.py` — contract doc, all-or-nothing + failure record
- `app/plugins/loader.py` — failure recording
- `app/plugins/bootstrap.py` — de-silenced outer guard
- `app/plugins/status.py` — NEW, the python-plane JSON status
- `crates/cli/src/plugins_cmd.rs` — NEW, `prism plugins list` (6 planes, human + `--json`, 3 unit tests)
- `crates/cli/src/ontology_cmd.rs` — NEW `List` subcommand (+ test)
- `crates/cli/src/main.rs` — `Plugins` command wiring
- `crates/agent/src/commands.rs` — `plugins` TUI root + help + tests
- `crates/agent/src/command_tools.rs` — `plugins` tool spec (+ shape test, argv audit updated)
- `crates/agent/src/mcp.rs` — recorded per-server failures + `failed_servers()`

---

## Gate (pasted, final run)

```
$ cargo fmt --all && cargo fmt --all --check
fmt exit: 0
fmt --check exit: 0

$ cargo test --workspace
test exit: 0
93 test binaries: test result: ok (every binary)
TOTAL PASSED: 3175
TOTAL FAILED: 0
(baseline at brief time: 3074 pass — count is higher, never lower, no newly-failing test)

$ cargo clippy --workspace --all-targets -- -D warnings
clippy exit: 0
warnings: 0
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

Project harness (`PYTHON=python3 bash scripts/verify-tui.sh`), run after the
gate:

```
  [PASS] fmt clean
  [PASS] build clean
  [PASS] unit tests passed
  [PASS] clippy clean
  [PASS] PTY tests passed
  [PASS] no CJK language drift detected
```

Python plugin tests: `23 passed` (`tests/test_plugin_registry.py`,
`tests/test_plugin_registry_unified.py`). No commits, no pushes, no
release builds, nothing under `target/` deleted; `~/Downloads/prism-experiment/` untouched.
