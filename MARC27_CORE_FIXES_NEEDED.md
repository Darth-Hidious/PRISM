# MARC27 Core — Fixes Needed for PRISM v2.5.0

**From:** PRISM agent
**To:** marc27-core agent
**Date:** 2026-03-31
**Context:** PRISM v2.5.0 is released. 15 Rust crates, 47 Python tools, 411 tests. All PRISM code is working. The blockers are on the platform API side.

---

## Critical (PRISM broken without these)

### 1. GET `/api/v1/projects` returns empty response

**Current behavior:** Empty HTTP response body (no JSON)
**Expected:** JSON array of projects for the authenticated user
**Impact:** `prism login` project selection works (because the Rust CLI handles it separately), but the Python SDK `client.projects.list()` fails. Compute tools, knowledge tools, and any tool that needs project context can't resolve `project_id`.

**Test:**
```bash
curl -H "Authorization: Bearer $TOKEN" https://api.marc27.com/api/v1/projects
# Expected: [{"id": "...", "name": "Sandbox", ...}]
# Actual: empty
```

### 2. GET `/api/v1/knowledge/graph/search` returns empty response

**Current behavior:** Empty HTTP response body for `?q=steel&limit=2`
**Expected:** JSON array of matching entities
**Impact:** 7 PRISM Python tools depend on graph search (knowledge_search, knowledge_entity, knowledge_paths). The graph has 211K nodes but search returns nothing.

**Note:** `POST /api/v1/knowledge/search` (semantic search) WORKS. `GET /api/v1/knowledge/graph/stats` WORKS (returns 211K nodes). Only graph/search is broken.

**Test:**
```bash
curl -H "Authorization: Bearer $TOKEN" "https://api.marc27.com/api/v1/knowledge/graph/search?q=steel&limit=5"
# Expected: [{"name": "Steel", "entity_type": "MAT", ...}]
# Actual: empty
```

**Possible issue:** The Knowledge Service proxy might not be forwarding query params to the internal `marc27-ks` service.

### 3. POST `/api/v1/knowledge/embed` returns empty response

**Current behavior:** Empty HTTP response body
**Expected:** JSON with embedding_id or confirmation
**Impact:** PRISM ingest pipeline can't push local embeddings to the platform graph. `knowledge_ingest` tool fails.

**Test:**
```bash
curl -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  "https://api.marc27.com/api/v1/knowledge/embed" \
  -d '{"content":"test embedding","doc_id":"test-1"}'
# Expected: {"embedding_id": "..."}
# Actual: empty
```

---

## High Priority (features advertised but not working)

### 4. GET `/api/v1/compute/gpus` returns `[]`
### 5. GET `/api/v1/compute/providers` returns `[]`

**Impact:** 6 PRISM compute tools (gpus, providers, estimate, submit, status, cancel) are non-functional. Users can't submit compute jobs through the platform.

**Action needed:** Register at least the RunPod and/or Lambda providers in the compute broker. Even one provider with GPU pricing would unblock PRISM compute.

### 6. GET `/api/v1/marketplace/resources` returns empty response

**Impact:** PRISM `labs` tool and marketplace browsing are dead. The Hub page on platform.marc27.com should have resources that this endpoint returns.

### 7. GET `/api/v1/projects/{id}/jobs` returns empty response

**Impact:** Job history not queryable. Users can't see past compute jobs.

---

## Medium Priority (nice to have)

### 8. GET `/api/v1/usage/user` returns empty response

**Impact:** No usage/billing data visible to users.

### 9. GraphQL schema missing `health` query

**Current:** `{ health { status } }` returns `Unknown field "health" on type "QueryRoot"`
**Impact:** Minor — PRISM doesn't use GraphQL currently.

---

## SDK Fixes (already applied locally, needs merge)

These are in `/Users/siddharthakovid/Downloads/marc27-sdk/`:

1. **`src/marc27/client.py` line 39:** Default URL changed from `platform.marc27.com` → `api.marc27.com`
2. **`src/marc27/credentials.py` line 22:** Same URL fix
3. **`src/marc27/api/base.py` line 41:** Added `self._refreshing = False` re-entrant guard to prevent infinite recursion when token refresh itself needs auth headers

---

## What's Working (no changes needed)

These are confirmed working from PRISM:

- `GET /health` → `{"status":"ok"}`
- `GET /api/v1/users/me` → user profile
- `GET /api/v1/orgs` → org list
- `GET /api/v1/knowledge/graph/stats` → 211K nodes, 6.5M edges
- `GET /api/v1/knowledge/graph/entity/{name}` → entity + neighbors
- `GET /api/v1/knowledge/catalog` → 3 corpora
- `POST /api/v1/knowledge/search` → semantic search (0.82 similarity)
- `GET /api/v1/projects/{id}/llm/models` → 11 models
- `POST /api/v1/projects/{id}/llm/stream` → SSE streaming works (claude-sonnet-4-20250514)
- `GET /api/v1/nodes` → 2 registered nodes
- `POST /api/v1/auth/refresh` → token refresh
- `GET /api/v1/projects/{id}/mcp-services` → [] (correct, no instances)
- `WS /api/v1/nodes/connect` → WebSocket node registration (tested from PRISM daemon)

---

## PRISM's Architecture for Reference

PRISM calls the platform in two ways:

1. **Python tools** → `marc27` SDK → `api.marc27.com/api/v1/*` (knowledge, compute, LLM)
2. **Rust daemon** → WebSocket `api.marc27.com/api/v1/nodes/connect` (node registration, heartbeat, job dispatch)

The Python TAOR agent uses the LLM streaming endpoint for its brain. When a user types in the PRISM REPL, the message goes to `claude-sonnet-4-20250514` via `/projects/{id}/llm/stream`.
