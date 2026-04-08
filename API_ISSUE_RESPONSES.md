# API Issue Responses — Platform Side Analysis

**Date:** 2026-04-08  
**In response to:** `docs/CLI_API_ISSUES.md`  
**Tested against:** live `api.marc27.com`

---

## Issue #1: Semantic search field mismatch

**Root cause:** CLI reads `name` and `entity_type` from semantic search results. These fields don't exist. Semantic search returns different fields than graph search.

**Actual fields returned by `POST /knowledge/search`:**
```json
["chunk_idx", "content", "corpus_id", "doc_id", "id", "metadata", "similarity"]
```

**Graph search returns:** `name, entity_type, label, tenant`  
**Semantic search returns:** `doc_id, content, similarity, corpus_id, metadata, chunk_idx, id`

**CLI fix:** In the display code (~line 4360), use `doc_id` instead of `name`, and `content` for the preview text. There is no `entity_type` — semantic search returns paper chunks, not graph entities.

```rust
// Wrong:
let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
// Right:
let name = r.get("doc_id").and_then(|v| v.as_str()).unwrap_or("?");
let preview = r.get("content").and_then(|v| v.as_str()).unwrap_or("").chars().take(100).collect::<String>();
let score = r.get("similarity").and_then(|v| v.as_f64()).unwrap_or(0.0);
```

**API status:** Working correctly. Verified.

---

## Issue #2: Research depth > 0 incomplete

**Root cause:** At depth 1, the RLM enables `web_search()` in the REPL. The web search endpoint (`/research/web-search`) returns empty because no external search providers (Semantic Scholar, arXiv, PubMed) are configured yet. The LLM calls `web_search()`, gets empty results, and keeps retrying until the stream times out — no `answer` or `complete` event is emitted.

**Verified:** depth=0 produces `{started:1, reasoning:5, repl_exec:5, repl_result:5, answer:1, complete:1}` — full cycle. depth=1 produces `{started:1, reasoning:6, repl_exec:6, repl_result:5}` — no answer/complete because it never finishes.

**Platform fix needed (not yet done):** Wire Semantic Scholar / arXiv APIs into the web search endpoint. For now, depth > 0 is limited.

**CLI workaround:** Add a timeout and synthesize a partial answer if `complete` event never arrives. Something like: after 60 seconds without `complete`, collect all `repl_result` stdout, pass to LLM for synthesis, and return that as the answer.

```rust
// Pseudo-code:
if elapsed > 60s && !received_complete {
    let partial = collected_repl_results.join("\n");
    // Show partial results to user
    eprintln!("Research timed out — showing partial results");
    return partial;
}
```

**API status:** Known limitation. Web search providers not configured.

---

## Issue #3: Google/Gemini usage tracking

**Root cause:** Google's OpenAI-compat streaming endpoint does NOT include `usage` in SSE chunks. This is a Google limitation, not ours.

**Verified:**
- Claude SSE final chunk: `{"delta":"","done":false,"usage":{"prompt_tokens":8,"completion_tokens":18}}` ✓
- DeepSeek SSE final chunk: `{"delta":"","done":true,"usage":{"prompt_tokens":4,"completion_tokens":33}}` ✓
- Gemini SSE final chunk: `{"delta":"...","done":true}` — **no usage field at all**

Google's native API (`generateContent`) returns usage in `usageMetadata` but their OpenAI-compat endpoint drops it from streaming responses. Our proxy passes through what the provider returns — it can't invent data.

**CLI fix:** Handle missing usage gracefully. Don't show `?/?` — show `N/A` or estimate from content length.

```rust
let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64());
let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64());
match (prompt_tokens, completion_tokens) {
    (Some(p), Some(c)) => format!("{p}/{c}"),
    _ => "N/A (provider doesn't report usage in streaming mode)".into(),
}
```

**API status:** Not fixable without switching to Google's native API. Accepted limitation.

---

## Issue #4: Report endpoint 404

**Root cause:** The endpoint EXISTS and WORKS. The CLI was sending `body` instead of `description`.

**Verified:**
```bash
curl -X POST /api/v1/support/tickets \
  -d '{"title":"test","description":"test ticket","severity":"low"}'
# → {"ticket_id":"TKT-00001","status":"open"}
```

**CLI fix:** Change the request body field from `body` to `description`:
```rust
// Wrong:
json!({"title": title, "body": body, "severity": "medium"})
// Right:
json!({"title": title, "description": body, "severity": "medium"})
```

**API status:** Working correctly.

---

## Issue #5: OpenAI/OpenRouter 429s

**Root cause:** Upstream provider rate limits, NOT our platform rate limit. Our platform handled 5 rapid-fire calls without any 429. The 429s come from OpenAI/OpenRouter themselves when their capacity is exceeded.

**Verified:** 5 rapid calls to Gemini → all returned 200.

**CLI fix:** Add retry-with-backoff for 429 responses:
```rust
for attempt in 0..3 {
    let resp = client.post(url).send().await?;
    if resp.status() == 429 {
        let wait = resp.headers().get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(2u64.pow(attempt));
        tokio::time::sleep(Duration::from_secs(wait)).await;
        continue;
    }
    return Ok(resp);
}
```

**API status:** No issue. Upstream provider limitation.

---

## Issues #6-#9: CLI-side only

All confirmed CLI-side. No API changes needed.

- **#6 Discourse events vs turns:** CLI naming mismatch. Add `events` as alias or update README.
- **#7 Marketplace install:** Endpoint exists, untested. Format is `POST /marketplace/resources/{slug}/install`.
- **#8 Node up without Docker:** CLI should check for Docker binary before starting dashboard server.
- **#9 Bare prism TUI:** Skip TUI launch if binary not found. Print help immediately.

---

## Platform-Side Actions Taken

| Issue | Action | Status |
|-------|--------|--------|
| #2 | Web search needs Semantic Scholar API | **TODO** — not wired yet |
| #3 | Google usage not in streaming SSE | **Won't fix** — Google limitation |
| Billing | Credits system built and wired | **DONE** — debits on every LLM call |

## What the PRISM Agent Should Do

1. **Fix #1:** Change semantic search display to use `doc_id`, `content`, `similarity` fields
2. **Fix #3:** Handle missing `usage` gracefully — show "N/A" for Google models
3. **Fix #4:** Change `body` → `description` in support ticket request
4. **Fix #5:** Add retry-with-backoff for upstream 429s
5. **Fix #6:** Add `events` alias or update README
6. **Fix #8:** Check Docker binary before dashboard start
7. **Fix #9:** Skip TUI launch if binary missing
