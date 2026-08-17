# Task Plan: Agentic Paper Extraction and Retrieval Re-verification

## Goal
Replace single-shot paper extraction with a bounded ontology-and-paper tool loop, and independently make stored assertions re-checkable against exact source evidence.

## Phases
- [x] Phase 1: Map current extraction, ontology, provenance, and retrieval flows
- [x] Phase 2: Implement population agent loop and remove replaced domain hardcoding
- [x] Phase 3: Persist exact evidence and implement retrieval re-reading
- [x] Phase 4: Update tests and documentation
- [x] Phase 5: Run required gates and record verbatim output

## Key Questions
1. What model client/tool-calling abstractions already exist and can support a bounded loop?
2. How is the promoted ontology selected and passed through paper ingestion today?
3. Which provenance schema/API changes preserve compatibility while storing source locations?
4. Where should retrieval re-open source text and expose an affirmation result?

## Decisions Made
- Keep population and retrieval as separate APIs and control flows, sharing only source-location data types where appropriate.
- Use `OntologyRegistry` and existing ontology lookup APIs; do not introduce a second vocabulary representation.
- Treat checks as annotations and never use a failed check to drop a proposed fact.
- Store citations on `prov_assertion_evidence`, the per-source record, rather than only on the aggregate `prov_assertion`; otherwise a later supporting paper can be paired with the first paper's immutable source.
- Use `LlmClient::chat_with_tools_streaming`; its MARC27 path preserves tools while `chat_with_tools` does not.
- Keep raw, one-based paper lines as the canonical citation coordinate. Soft-wrap normalization may assist annotations but cannot define a source location.

## Errors Encountered
- Codebase-memory MCP has no index for this worktree (`project not found`): use the repository-approved `rg`/targeted-read fallback and record exact symbol locations.
- The exact combined test command cannot bind 45 local Wiremock listeners in the sandbox. Escalation was rejected under the repository no-network policy. All three test binaries compile; network-free focused suites, provenance, ontology, retrieval, and CLI storage tests pass. Exact output is in `AGENTIC_EXTRACTION.md`.

## Status
**Complete** - Implementation, focused verification, exact gates, and evidence report are finished. The only non-green exact gate is the documented sandbox listener restriction.
