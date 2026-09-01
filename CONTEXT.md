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
  `fact_kind`/`sign_domain` (`6523eae3`); arXiv text bleed (`74f3a2ad`);
  namespace read-compat (`2c95ad2f`); search engine self-strikes (`ec74c992`);
  one bad file ending the corpus (`8f0c8e57`); config silent-ignore + whole
  replace (`ef8667a4`); search/recall withhold (`65d84c17`); recall divisor
  (`5539eaed`); streaming usage None (`0a04aaf1`); patents dropped from digest
  (`02715a3a`); warn! discarded by default (`ea21414c`); deny-error → Allow
  (`8641226b`); synthetic backend labelled validated (`c73643e9`); XML entities deleted (`2e243e58`); reward sign from substring (`e9b373a8`); "Not done" scored done (`372194d0`); audit append lied (`96a7a8bc`); short vision recovery rejected (`57ce345e`); vision page loop discarded pages (`7b1cccd8`); error objects read as success (`e34df92c`); failed fetch = absent (`5c81e3e2`); identity loss silent (`dd6fb995`); claims probe + exit 0 (`e38f3f57`); governance wiped provenance (`67f606cc`); owners file honest+atomic (`5cfdfdec`); reasoning leaked into content (`60f39bee`); narrowed recall counts withheld (`98e6144c`); Kafka probed at its host (`a39860f4`); live-store guard armed for cli/frontend (`ee58a885`); papers/reverify built an unused venv, 65 s→7.5 s (`03bef50f`); policy-load lie (`43bef019`); sidecar crash reported as timeout (`1f5bea58`); DAG budget per wave (`6459675f`); untyped base relation note (`189b485c`). Playbook §13 has the full ranked list with ✅ marks. The Aug 24 promoted
  `~/Downloads/prism-ontology-shards/ontology-polymers.ttl` PREDATES all of
  these — 8 copies of every EMMO upper class, zero typed relations. Re-fold with
  `~/Downloads/prism-ontology-shards/fold.sh`; review; only then promote.

## Key Decisions
- Park only what a missing capability actually blocks; never discard sound work.
- No synthetic health probes: the real read is the health signal (K=3 remote failures).
- `polymers.ttl` (6542 classes / 6172 relations) PROMOTED 1 Sep on owner say-so; installed at `.prism/ontologies/polymers.ttl`.
- Ontology binds AFTER extraction; `value`/`unit` are never required fields.

## Next Steps
1. §13 ranked lists CLOSED (T1–T19, B1–B15) except BOOKED B11 attempt-evidence + audit hash chain. Owner decision open: author domain/range for the 5 EMMO properties in the materialised TTL (currently unseeded, now honestly reported).
   Tailcat (`tailscale/tailcat`) = mesh DATA PLANE only, never a PRISM tool; NOT installed; needs Mirdyne control plane first.
2. Then the LitXBench eval (`~/Downloads/prism-gold-eval/harness/`), scratch DB.
3. Merge to main; tag only after.
