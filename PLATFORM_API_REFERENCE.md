# PRISM V1 — Final Platform Integration Reference

**Date:** 2026-04-06 | **Platform:** api.marc27.com | **Status:** Production

---

## Platform State (Live-Verified)

| Service | URL | Status |
|---------|-----|--------|
| API | api.marc27.com | OK, v0.1.0, 45 REST endpoints |
| Knowledge | marc27-knowledge-production.up.railway.app | OK |
| Runtime | marc27-runtime-production.up.railway.app | OK, v0.4.0 |
| LLM | marc27-llm-production.up.railway.app | Needs stability fix |

| Data | Value |
|------|-------|
| Neo4j | 69,785 nodes, 5.38M edges, 10 entity types (MatKG backbone) |
| pgvector | 8,612 real paper content embeddings |
| LLM Models | 519 from 4 providers, 517 with pricing |
| Marketplace | 15 resources (datasets, models, workflows) |
| Sentry | Active on all 3 Rust services |

---

## Auth

### API Key (simplest — for CLI)
```
X-API-Key: m27_your_key_here
```
Get one: `POST /api/v1/api-keys` (needs JWT first) or generate at platform.marc27.com

### Device Flow (first-time CLI setup)
```
POST /api/v1/auth/device/start
  {"client_id": "prism-cli"}
  → {"device_code": "...", "user_code": "ABCD-1234", "verification_uri": "https://platform.marc27.com/device"}

POST /api/v1/auth/device/poll
  {"device_code": "..."}
  → 202 {"status": "pending"} (keep polling every 5s)
  → 200 {"access_token": "...", "refresh_token": "...", "platform_email": "user@id.marc27.com"}
```

### Token Refresh
```
POST /api/v1/auth/refresh
  {"refresh_token": "..."}
  → {"access_token": "...", "refresh_token": "..."}
```

### Email Identity
On first login, users get `username@id.marc27.com` — a platform signature, NOT a mailbox. Returned in token exchange response as `platform_email`. Shows on all published resources.

**NEVER use `X-Service-Secret` from PRISM. That's internal service-to-service only.**

---

## GraphQL (PRIMARY INTERFACE)

```
POST /api/v1/graphql
Authorization: Bearer <token> OR X-API-Key: m27_xxx
Content-Type: application/json
```

### Queries (20)

| Query | Example |
|-------|---------|
| `search` | `{ search(term: "nickel", limit: 5) { name entityType label props } }` |
| `entity` | `{ entity(name: "Titanium") { name entityType props neighbors(limit: 5) { target { name } relType count } } }` |
| `paths` | `{ paths(from: "Nickel", to: "Creep Resistance", maxHops: 3) }` |
| `graphStats` | `{ graphStats { nodes edges entityTypes } }` |
| `embeddingStats` | `{ embeddingStats { count } }` |
| `semanticSearch` | `{ semanticSearch(query: "thermal protection", limit: 3) { docId content similarity } }` |
| `llmModels` | `{ llmModels(provider: "anthropic", limit: 5) { modelId displayName inputPrice outputPrice contextWindow } }` |
| `deployments` | `{ deployments(status: "running", limit: 5) { id name status target gpuType costUsd } }` |
| `discourseSpecs` | `{ discourseSpecs(limit: 5) { id slug name version } }` |
| `nodes` | `{ nodes(limit: 5) { id name status visibility lastSeen pricePerHour } }` |
| `marketplace` | `{ marketplace(limit: 10) { name slug resourceType author contentMarkdown } }` |
| `resource` | `{ resource(slug: "mace-mp-0") { name description contentMarkdown citations } }` |
| `computeGpus` | `{ computeGpus { gpuType provider pricePerHourUsd vramGb } }` |
| `computeJob` | `{ computeJob(id: "...") { status provider costUsd } }` |
| `computeEstimate` | `{ computeEstimate(input: {...}) { estimatedCostUsd } }` |
| `me` | `{ me { id displayName avatarUrl } }` |
| `corpora` | `{ corpora { name slug kind entryCount } }` |
| `provenance` | `{ provenance(entityType: "CHM", entityId: "Nickel") }` |
| `jobs` | `{ jobs(limit: 5) { id jobType status } }` |
| `ingestJob` | `{ ingestJob(id: "...") { id status } }` |

### Mutations (6)

| Mutation | Example |
|----------|---------|
| `createDeployment` | `mutation { createDeployment(name: "mace", image: "marc27/mace:latest", gpuType: "A100-80GB") { id name status } }` |
| `stopDeployment` | `mutation { stopDeployment(id: "...") }` |
| `createDiscourseSpec` | `mutation { createDiscourseSpec(slug: "my-debate", yaml: "name: ...") { id slug version } }` |
| `submitComputeJob` | `mutation { submitComputeJob(input: { image: "marc27/lammps", inputs: "{}", gpuType: "A100-80GB" }) { id status } }` |
| `cancelComputeJob` | `mutation { cancelComputeJob(id: "...") }` |
| `submitIngestJob` | `mutation { submitIngestJob(input: { corpusId: "...", sourceUrl: "..." }) { id status } }` |

---

## Command Families

### 1. `prism research`

```
POST /api/v1/knowledge/research/query
{"query": "Find materials containing nickel", "depth": 0}
```

**Response:** SSE stream

| Event | Data |
|-------|------|
| `started` | session_id, mode, max_depth |
| `reasoning` | LLM thought process + code blocks |
| `repl_exec` | code block being executed |
| `repl_result` | stdout/stderr from execution |
| `answer` | final answer text |
| `complete` | metrics: cost, graph_queries, llm_calls |

**Depth:** 0=free (graph only), 1+=costs money (web search). Use depth:0 for smoke tests.

### 2. `prism discourse`

```
POST /api/v1/discourse/specs          — Create YAML spec (auto-versioned)
GET  /api/v1/discourse/specs          — List specs
GET  /api/v1/discourse/specs/{id}     — Get spec + YAML
POST /api/v1/discourse/run/{spec_id}  — Run (SSE stream)
GET  /api/v1/discourse/{instance_id}  — Status + results
GET  /api/v1/discourse/{instance_id}/turns — All agent turns
```

**YAML Format:**
```yaml
name: alloy-debate
agents:
  - id: metallurgist
    role: "Materials scientist"
    model: default
    tools: [graph_search]  # enables RLM
  - id: theorist
    role: "DFT expert"
discourse_tree:
  - round: 1
    type: open_discussion
    agents: [metallurgist, theorist]
    prompt: "What are the key properties of $params.alloy?"
  - round: 2
    type: consensus
    threshold: 0.8
parameters:
  - name: alloy
    required: true
gates:
  - round: 2
    metric: consensus_confidence
    op: greater_than
    threshold: 0.75
    on_fail: abort
```

Round types: `open_discussion`, `claim_challenge`, `consensus`, `ab_test`, `synthesis`

### 3. `prism deploy`

```
POST   /api/v1/compute/deployments          — Create
GET    /api/v1/compute/deployments          — List (?status=running)
GET    /api/v1/compute/deployments/{id}     — Detail
DELETE /api/v1/compute/deployments/{id}     — Stop
GET    /api/v1/compute/deployments/{id}/health — Health check
```

**Create request:**
```json
{"name": "mace-prod", "image": "marc27/mace:latest", "target": "prism_node",
 "gpu_type": "A100-80GB", "budget_max_usd": 50.0}
```

Or by marketplace slug: `{"name": "mace", "resource_slug": "mace-mp-0"}`

**Runtime generic deploy:**
```
POST /deploy on runtime
{"deployment_id": "...", "weights_source": "hf://mace-foundations/mace-mp-0",
 "framework": "auto", "port": 8080, "gpu": true}
```

Weight sources: `hf://org/model`, `r2://path`, `https://url`, `/local/path`

**WebSocket Protocol (node side):**
- Receive `DeployModel` → pull image → start → send `DeploymentReady {endpoint_url}`
- Periodically send `DeploymentHealthUpdate {healthy, message}`
- On stop/crash: send `DeploymentStopped {reason}`

### 4. `prism models`

```
GET /api/v1/projects/{project_id}/llm/models
```

Returns 519 models with: model_id, display_name, provider, tier, context_window, input_price, output_price. Also available via GraphQL `llmModels`.

### 5. `prism ingest`

Pipeline: PDF → extract text (runtime /run) → chunk → embed (KS /embed/bulk) → extract entities (LLM) → graph (KS /graph/ingest)

```
# Runtime text extraction (returns per-page text)
POST /run on runtime
{"model": "pymupdf", "input": {"r2_path": "corpora/nasa-propulsion/pdfs/123.pdf"}}
→ {"text": "...", "pages_text": [{"page": 1, "text": "...", "chars": N}], ...}

# Bulk embedding
POST /embed/bulk on knowledge service
{"corpus_id": "uuid", "documents": [{"doc_id": "...", "content": "...", "metadata": {...}}]}

# Entity ingestion
POST /graph/ingest on knowledge service
{"entities": [{"name": "X", "entity_type": "MAT", "label": "Material"}],
 "relationships": [{"from_name": "X", "to_name": "Y", "rel_type": "HAS_PROPERTY"}]}
```

**On-the-fly (REPL):** `ingest_paper("https://arxiv.org/pdf/2401.12345")` — downloads, extracts, embeds, returns {chunks, pages}.

### 6. `prism marketplace`

Via GraphQL (preferred):
```graphql
{ marketplace(limit: 10) { name slug resourceType description author contentMarkdown citations tags } }
{ resource(slug: "mace-mp-0") { name contentMarkdown deployConfig metadata } }
```

### 7. Node Registration

**WebSocket:**
```
WS /api/v1/nodes/connect?token=<jwt_or_api_key>
→ Send: {"type": "register", "name": "my-node", "capabilities": {...}}
← Recv: {"type": "registered", "node_id": "uuid", "heartbeat_interval_secs": 30}
→ Send: {"type": "heartbeat", "cpu_load": 0.3, ...} every 30s
```

**REST (alternative):**
```
POST /api/v1/nodes/register
GET  /api/v1/nodes
GET  /api/v1/nodes/{id}
POST /api/v1/nodes/{id}/heartbeat
```

**E2EE Key Exchange:**
```
GET  /api/v1/nodes/{id}/public-key     — Get node's X25519 public key
POST /api/v1/nodes/{id}/exchange-key   — Exchange public keys for shared secret
```

---

## Agent Discovery

```
GET /api/v1/agent/capabilities  (no auth required)
```

Returns full platform map: all REST endpoints, GraphQL schema with examples, auth methods, CLI quickstart. Use as system prompt for any LLM agent.

## Error Handling

All endpoints return structured errors:
```json
{"error": {"code": "not_found", "message": "..."}, "suggestions": [...]}
```

Smart 404s suggest correct endpoints. Read `suggestions` array on 404.

---

## Known Issues

| Issue | Status |
|-------|--------|
| LLM service instability | Needs investigation — goes down periodically |
| Rate limit: 100/window | Bypassed for X-Service-Secret calls |
| OpenRouter model routing | Some models 404 (routing fix in progress) |
| Marketplace empty | Migration checksum fixed, re-deploying |
| context_window INT4 | Fixed in GraphQL (was i64, now i32) |

## Cost Control

| Action | Cost |
|--------|------|
| Research depth:0 | Free |
| Research depth:1+ | LLM calls (use Gemini Flash $0.075/M) |
| Discourse run | 1 LLM call per agent per round |
| Entity extraction | Use GLM-4.5-Air (free) or Gemini Flash |
| Embedding | Gemini text-embedding-004 |

**Rule:** ONE smoke test proves the pipeline. Don't run 5 variations.
