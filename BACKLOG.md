# PRISM backlog — archived from the session task list, 2026-08-26

Preserved verbatim before clearing the stale list. These are real, unfixed findings.

## Data-plane defects (measured, unfixed)
- **Properties are extracted then discarded** — extraction produces properties that never reach storage.
- **Stored data is unreachable** — `props_json`, edge confidence and evidence are written but no read path exposes them.
- **Value-in-assertion-id defeats corroboration** — the value is part of the identity key, so two sources reporting the same fact with different precision never corroborate.
- **QUDT unit rejection kills whole documents** — one unrecognised unit discards the entire document instead of the single fact.
- **Measurement conditions are never written** — one production line sets them empty.

## Prediction plane
- **Data gaps become confident numbers** — one root cause, 4 sites.
- **`predict_property` has never worked** — CI is green because the test mocks the wrong type.

## Adapter / policy
- **R0.5 adapter conformance suite** — never built.
- **Policy gate blocks agent ingestion** — owner decision pending.
- **File sandbox enforced on 2 tools, bypassable via a 3rd** — owner decision pending.
- **Provider capabilities are a uniform fiction** — all 49 declare the same 4 filterable fields.

## Error corpus
- **E4** — run the labelled corpus and measure detection rate.
- **E5** — make the record sufficient for the model to re-check itself.

## Larger workstreams
- **Semantic validation via embedding priors** (Knowledge Vault style: extractor confidence fused with a graph-derived prior). Substrate exists — BGE-small 384-dim + `vector_distance_cos` in Turso — but every `graph_validation` check is exact string matching.
- **Bundle Gemma 4 as PRISM's own model.** Apache-2.0, builds with `--features local-inference`, loads 6.7GB on Metal. One blocker: `local.rs` `apply_chat_template` fails with `ffi error -1` on Gemma 4's Jinja template.
