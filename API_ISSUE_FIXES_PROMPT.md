# PRISM Agent — API Issue Fixes Required

Read `API_ISSUE_RESPONSES.md` in this repo for full root cause analysis. Here's exactly what to fix:

## Fix #1: Semantic search display (CRITICAL)

**File:** `crates/cli/src/main.rs` ~line 4360

The CLI reads `name` and `entity_type` from semantic search results. These fields DON'T EXIST. Semantic search returns completely different fields than graph search.

**Graph search fields:** `name, entity_type, label, tenant`
**Semantic search fields:** `doc_id, content, similarity, corpus_id, metadata, chunk_idx, id`

Change:
```rust
// WRONG — these fields don't exist in semantic search
r.get("name")       →  r.get("doc_id")
r.get("entity_type") →  remove or show "paper_chunk"
// ADD — show content preview and similarity score
r.get("content")     // paper text
r.get("similarity")  // 0.0 to 1.0
```

## Fix #3: Gemini usage tracking (MEDIUM)

**File:** `crates/ingest/src/llm.rs` in `chat_marc27_simple()`

Google's streaming endpoint doesn't return `usage` in SSE. This is a Google limitation. Handle it:

```rust
let prompt_tokens = usage.get("prompt_tokens")
    .or_else(|| usage.get("input_tokens"))
    .and_then(|v| v.as_u64());
let completion_tokens = usage.get("completion_tokens")
    .or_else(|| usage.get("output_tokens"))
    .and_then(|v| v.as_u64());
// If neither found, show "N/A" instead of "?/?"
```

## Fix #4: Support ticket field name (EASY)

**File:** wherever `prism report` builds the request body

Change `body` to `description`:
```rust
// WRONG
json!({"title": title, "body": body})
// RIGHT  
json!({"title": title, "description": body, "severity": "medium"})
```

The endpoint works — returns `{"ticket_id":"TKT-00001","status":"open"}`.

## Fix #5: Upstream 429 retry (MEDIUM)

Add retry-with-backoff for 429 responses from upstream providers. Our platform doesn't 429 — it's OpenAI/OpenRouter being rate-limited.

```rust
for attempt in 0..3 {
    let resp = client.post(url).send().await?;
    if resp.status() == 429 {
        let wait = 2u64.pow(attempt);
        tokio::time::sleep(Duration::from_secs(wait)).await;
        continue;
    }
    return Ok(resp);
}
```

## Fix #6: README discourse command

`prism discourse events` → should be `prism discourse turns`. Update README or add `events` as alias.

## Fix #8: Node up Docker check

Before starting the dashboard Axum server, check if Docker is available:
```rust
if !which::which("docker").is_ok() {
    eprintln!("Docker not found — running in headless mode");
    // Skip dashboard, just register with platform
}
```

## Fix #9: Bare prism TUI

Skip TUI binary check — go straight to help if no subcommand:
```rust
if args.is_empty() {
    print_help();
    return;
}
```

## What's NOT fixable on your side

- **#2 depth > 0:** Web search providers (Semantic Scholar, arXiv) not configured on the platform. Depth 0 works fully (111 queries, 15 LLM calls verified). Add a timeout for depth > 0 and return partial results.
- **#3 Google usage:** Google's streaming endpoint genuinely doesn't return usage. Show "N/A" for Google models.

## What's verified working on the API (2026-04-08)

- 28/28 provenance test passed
- All 4 LLM providers respond (Claude, Gemini, DeepSeek, Llama)
- Marketplace: 15 resources with full model cards
- Support tickets: endpoint works with correct field names
- Billing: credits system live, debits on every LLM call
- 519 models, 517 with pricing
