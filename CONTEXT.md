# CONTEXT — resume here

**Current Task**: Systematic find-and-fix across the crates for six recurring
defect classes (muzzles, lying signals, silent data loss, gates at the wrong
seam, tests that prove nothing, measurement hygiene). Four Opus hunters
produced ranked findings with file:line; fixes land one at a time with a
mutation-tested test each. Prior-session WIP is preserved untouched on `754eccca`.

## Already fixed — do NOT re-fix
- arXiv-only fetch in `papers.rs`: GONE. `full-text`/`claims` now build the
  engine with `parse_sources(&None)` = all sources (`papers.rs:367`, `:479`).
- `--base` refusing DRAFT shards (`5b10e6b6`); relations dropped at the fold
  (`238c8fd9`); exactMatch merge (`44a373e0`, pinned `14b12a35`); fold dropping
  `fact_kind`/`sign_domain` (`6523eae3`). The Aug 24 promoted
  `~/Downloads/prism-ontology-shards/ontology-polymers.ttl` PREDATES all of
  these — 8 copies of every EMMO upper class, zero typed relations. Re-fold with
  `~/Downloads/prism-ontology-shards/fold.sh`; review; only then promote.

## Key Decisions
- Park only what a missing capability actually blocks; never discard sound work.
- No synthetic health probes: the real read is the health signal (K=3 remote failures).
- Ontology binds AFTER extraction; `value`/`unit` are never required fields.

## Next Steps
1. Finish the hunter findings (playbook §13): arXiv parser text-buffer bleed
   (`arxiv.rs:87`, corrupts `published` and the dedup key on every record);
   usage always None on OpenAI streaming (`llm/lib.rs:2201`, so compaction never
   fires); `EndpointCall` at `vision.rs:139` untested; live-store guard disarmed
   under `cargo test -p prism-cli` (H3).
2. Then the LitXBench eval (`~/Downloads/prism-gold-eval/harness/`), scratch DB.
3. Merge to main; tag only after.
