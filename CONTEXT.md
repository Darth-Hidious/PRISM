# CONTEXT — resume here

**Current task**: PRISM hardening after the line-by-line audit. Branch feat/composability-seam, HEAD 6f3f14fc. **177 commits unpushed — the harness blocks `git push`; the owner runs `! git push origin feat/composability-seam`.**

## Key decisions
- Audit delivered (scratchpad/audit/PRISM_audit_final.md, sent to the owner): 340,933 lines read; only crates/agent verified (38 confirmed); 355 serious + 329 minor findings elsewhere are UNVERIFIED leads. The owner stopped further agent fan-outs ("stop running one billion agents") — verify by hand, never by fleet.
- Fixed and gated today (RED→GREEN→mutant→fmt/clippy/suites): 4 blockers (digested results unlogged, rotation deleted segments, overflow compacted the request, 30 s SIGKILL default), 5 majors (apply_patch errors + Add File, web_browse pane read, per-question empty guard, auto_approve status truth, harness notes as user role), scroll lag (per-message cache + tick-only-when-volatile), and `[tools.<name>] enabled = false`.
- Binary installed from 6f3f14fc (sha 00919305c36a): carries every fix above including the [tools] switch.

## Next steps
1. Remaining verified majors from the audit backlog (§4 of the report): turn panic loses runtime, mid-turn slash command sent to the LLM, failed turn unlogged, compaction boundary, prune record, silent write failure, provenance spool, credential strip on caller URLs.
2. Harness path steps 1–8 (live catalog everywhere, capability tags, settings catalog + palette, CommandRegistry, TurnHooks/executor registry, SessionSink).
3. Font size is terminal-owned (⌘+/−); say so in the help view, offer density settings once the settings catalog exists.

Protected, never commit: holmquist2019_whiterose.txt, schellenberger2018_diva.txt, ontology-pfas-alternatives-candidate.ttl, pfas_alternatives_evidence_log*.md.
