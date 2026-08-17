# Notes: Agentic Paper Extraction and Retrieval Re-verification

## Codebase Findings

- Codebase-memory MCP has no index for this exact worktree, so discovery used the documented targeted local fallback.
- Current population chain: one complete source workspace reaches `run_paper_agent_sample`, which loops over streaming tool calls, returns cited proposals and ontology bindings, then writes them without a second lexical/domain review.
- The CLI resolves built-in or project-installed promoted ontologies and passes the exact active adapter to the reader and storage writer.
- `prism_llm::LlmClient::chat_with_tools_streaming` already supports the required multi-turn message/tool protocol across HTTP, MARC27, and embedded GGUF.
- The ingest ontology registry stores `Arc<dyn Ontology>`; built-ins are backed by `prism_ontology::OntologyGraph`, while the trait needs object-safe navigation defaults/overrides so a reader never bypasses the registry.
- `numeric_fact_grounding` and the numeric-tolerance quote wrapper now preserve exact spans and named refusal guards for the legacy repair path; fresh population persists the agent-selected citation directly.
- `prov_assertion` is an aggregate with immutable first attribution and best-wins verification. Exact citations must live on `prov_assertion_evidence`, keyed by assertion and source.
- Citation revision, span, one-based inclusive line range, and locator are persisted atomically on `prov_assertion_evidence`.
- Local and fetched papers store SHA-addressed UTF-8 source snapshots; re-verification opens that exact numbered representation.
- `CONTINUATION_WORDS`, closed unit/quantity tables, alias prompt, and classifier prompt are deleted.

## Test Contract Changes

Every renamed/retargeted contract has an in-body `CONTRACT CHANGE` explanation; the complete inventory is in `AGENTIC_EXTRACTION.md`.

## Gate Output

Required and additional command output is recorded in `AGENTIC_EXTRACTION.md`. Format and clippy pass. The exact combined test command reports 292 pass and 45 Wiremock listener permission failures; the network-free suites pass.
