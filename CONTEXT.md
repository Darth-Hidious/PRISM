# CONTEXT — resume here

**Current Task**: Measure PRISM's extraction against LitXBench gold data. The
harness is built and BLOCKED on one bug (below). Fix that first tomorrow, then
run it — the number is the goal.

## The blocker, found last thing, fix this first

`crates/cli/src/papers.rs:310` and `:417` — `full-text` and `claims` build their
fetch engine with **arXiv only**:

```rust
let engine = build_engine(vec![SourceId::Arxiv.as_str().to_string()], &None, false);
```

The other four `build_engine` call sites pass the user's resolved `source_ids`.
So SEARCH can use every source and EXTRACTION can only fetch arXiv. Any other URL
returns `None` and PRISM reports **`no_fulltext_available`** — which is a lie: the
paper has full text, PRISM has no adapter wired for that host.

**This probably accounts for part of the "464 papers, no fetchable full text"
number.** Fix = pass the resolved source list like the other four sites, and make
the refusal name the real reason ("no adapter for host X").

It is also why the benchmark cannot run: gold papers served over localhost are
refused as having no full text.

## Key Decisions
- Ontology binds AFTER extraction, never in the prompt (controlled ablation:
  in-prompt costs 41% of triples; post-hoc gets same conformance, 1.7x recall).
- NEVER make `value`/`unit` required fields — required fields drive fabrication
  to 100% in 10 of 13 models. Filter after; never compel a number.
- Destructive-intent gates key on registered write capability, never on English
  words in user text.

## Next Steps
1. Fix the arXiv-only fetch (above), then run the LitXBench eval —
   `~/Downloads/prism-gold-eval/harness/` has `convert_claims.py`, `score.py`,
   `md_to_jats.py`, and 5 papers already converted to JATS in `harness/jats/`.
   Serve over localhost; `papers claims --url ... --format jats`. Use
   `PRISM_PROVENANCE_DB` pointed at a scratch db — never the live store.
2. Read the Gemini CLI recon result (agent was still running at cutoff) — how a
   general agent scores 0.80 where purpose-built pipelines lose by up to 0.37 F1.
3. Then merge to main (563 commits ahead) and only then tag. Release workflow
   fires on a `v*` tag push; CHANGELOG is already written.

Full detail: `~/Downloads/PRISM_HARNESS_PLAYBOOK.md` §10-§12.
