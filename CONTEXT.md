# CONTEXT — resume here

**Current task**: PRISM hardening after the preburner campaign. Branch feat/composability-seam, HEAD c007eead. Nine gated commits today; **164 commits unpushed — the harness blocks `git push`; the owner runs `! git push origin feat/composability-seam`.**

## Key decisions
- Trajectory logging (c007eead): sessions now carry turn/step/approval/decision entries + content-addressed prompt sections (blobs/<sha>), schema_version 2, additive; binary rebuilt and installed.
- Ponytail ruleset (scratchpad/ponytail, MIT) governs every change: ladder, root cause not symptom, one runnable check, `ponytail:` ceilings. DeepSeek Harness (deepseek-ai/deepseek-harness, MIT) is the plugin yardstick: "no privileged core", "model-visible means logged".
- Line-by-line audit: 20 of 31 readers done (230k Rust lines), 427 findings UNVERIFIED (20 blocker) — verifiers/synthesis hit the account limit (resets 01:10). Resume with Workflow resumeFromRunId wf_2b41dbdd-6b2 (readers cached) as a verify-only workflow; treat zero votes as unverified.

## Next steps
1. Scroll lag: per-message render cache (design + RED test drafted in scratchpad/designs_pending_audit.md, red_test_transcript_cache.rs); draw_chat builds five side tables keyed by absolute row — cache must store relative indices. ~1 day.
2. Resume audit verification after 01:10; then the harness-gap work: builtins through the plugin contract + a disable list; palette → real settings.
3. Font size is terminal-owned (⌘+/−) — not a PRISM change; say so in the backlog.

Protected, never commit: holmquist2019_whiterose.txt, schellenberger2018_diva.txt, ontology-pfas-alternatives-candidate.ttl, pfas_alternatives_evidence_log*.md.
