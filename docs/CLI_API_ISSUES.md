# PRISM CLI ↔ API Issues — 2026-04-08

Issues found during end-to-end testing. The API is final — all fixes are on the PRISM CLI/client side.

## Critical (blocks core functionality)

### 1. Semantic search response format mismatch
**Command:** `prism query --platform --semantic "creep resistant superalloys" --json`  
**API endpoint:** `POST /api/v1/knowledge/search` (semantic)  
**Problem:** Results come back but `name` and `entity_type` fields are missing/empty. The CLI tries to display `r.get("name")` and `r.get("entity_type")` but gets null.  
**Fix needed:** Read one raw response from the API, check actual field names, update the display code in `crates/cli/src/main.rs` (around line 4360).

### 2. Research depth > 0 returns incomplete
**Command:** `prism research "high entropy alloys" --depth 1 --json`  
**API endpoint:** `POST /api/v1/knowledge/research/query`  
**Problem:** At depth 0 it works (111 queries, 15 LLM calls). At depth 1 it returns 0 queries, 0 LLM calls, and the "answer" is just the LLM's raw thinking with code blocks — not synthesized research.  
**Fix needed:** Check if depth>0 triggers a different response format or async job that the CLI isn't waiting for. Inspect the API response at depth 1 vs depth 0.

### 3. LLM proxy usage tracking incomplete
**Command:** Any chat via MARC27 proxy (Gemini specifically)  
**Problem:** Google models return `tokens: ?in/?out` — the `usage` field in the SSE `done` event has different field names than expected (`prompt_tokens`/`completion_tokens`).  
**Fix needed:** In `crates/ingest/src/llm.rs` `chat_marc27_simple()`, check what Google's usage object actually contains and map it. May need to check `input_tokens`/`output_tokens` as alternatives.

## Medium (works but degraded)

### 4. Report endpoint 404
**Command:** `prism report "bug description" --no-github`  
**API endpoint:** `POST /api/v1/support/tickets`  
**Problem:** Returns HTTP 404 — endpoint not deployed yet.  
**Fix needed (API side):** Deploy the support tickets endpoint. No CLI fix required.

### 5. OpenAI/OpenRouter models 429
**Command:** Chat with `gpt-4o-mini` or `google/gemma-3-27b-it:free`  
**Problem:** `429 Too Many Requests` from upstream providers through MARC27 proxy.  
**Fix needed (API side):** Rate limit handling or retry logic in the platform proxy. CLI could add retry-with-backoff for 429s.

### 6. Discourse events command mismatch
**Command:** `prism discourse events <id>` (in README)  
**Actual CLI:** `prism discourse turns <id>` (the real subcommand)  
**Fix needed:** Update README to match actual CLI. Or add `events` as an alias.

## Low (cosmetic or future)

### 7. Marketplace install URL format
**Command:** `prism marketplace install <slug>`  
**Problem:** Not tested — the `install_url` endpoint (`POST /marketplace/resources/{name}/install`) may return a different format than expected.  
**Fix needed:** Test with an actual install and verify the response `{url: "..."}` format.

### 8. Node up without Docker
**Command:** `prism node up` on a machine without Docker  
**Problem:** Registers with platform but dashboard server can fail if port is busy. No graceful "Docker not found, running in headless mode" path.  
**Fix needed:** After Docker warning, skip dashboard start or bind to a different port. Currently it tries to start the Axum server regardless and fails if port 7327 is occupied.

### 9. Bare `prism` TUI fallback message
**Command:** `prism` (no subcommand)  
**Problem:** Shows spinner for a few seconds, then the PRISM welcome screen from the old TUI code path before falling back to help text.  
**Fix needed:** Skip the TUI launch attempt entirely when no TUI binary exists. Check for binary first, print help immediately if missing.

---

## What's Working (verified 2026-04-08)

All tested against live `api.marc27.com`:

- `prism login` — device flow auth, token refresh
- `prism status` — auth state, paths, endpoints
- `prism configure` — read/write prism.toml, show LLM config
- `prism tools` — 108 tools loaded
- `prism models list` — 519 models across 4 providers
- `prism node status` / `prism node probe` — capabilities detection
- `prism mesh discover` — mDNS scan
- `prism workflow list` — forge workflow found
- `prism deploy list` — deployments shown
- `prism discourse list` — specs shown
- `prism marketplace search` — **FIXED: 15 resources with full metadata**
- `prism query --platform` — graph entity search (10 results)
- `prism ingest --schema-only` — schema detection
- `prism query --cypher` — correct Neo4j-not-running error
- `prism research --depth 0` — full research loop (111 queries, 15 LLM calls)
- `prism job-status` — status check
- `prism publish` (dry) — path validation
- LLM chat via Claude Haiku, Gemini Flash, DeepSeek V3 — all work through agent

## Models Tested

| Model | Provider | Status |
|-------|----------|--------|
| claude-haiku-4-5-20251001 | Anthropic | PASS |
| claude-sonnet-4-6 | Anthropic | PASS |
| gemini-2.5-flash | Google | PASS (no usage tracking) |
| deepseek/deepseek-chat-v3 | DeepSeek | PASS |
| gpt-4o-mini | OpenAI | 429 (upstream rate limit) |
| google/gemma-3-27b-it:free | OpenRouter | 429 (upstream rate limit) |
