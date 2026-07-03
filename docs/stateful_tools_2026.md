# Stateful Tools — Architecture Spec

**Status:** Active build  
**Owner:** PRISM agent loop  
**Author:** Sid + Claude (2026-05-08)  
**Supersedes:** the "tool calling = stateless" pattern that's existed since v0.1

## Problem

Today PRISM is mostly stateless. A user who searches Ti alloys at turn 3 and asks
"which had the highest density?" at turn 8 — the LLM has to find the data again
by re-reading transcript text or re-running the search. There is no structured
handle on prior tool outputs.

Concretely, the gaps:

| Layer | State today |
|---|---|
| Tool calls recorded | ✅ scratchpad logs every call linearly (text only) |
| Tool RESULTS persisted across turns | ⚠️ in scratchpad as flat text — LLM has to re-scan |
| Tool results indexed for semantic recall | ❌ none |
| Knowledge persists across sessions | ⚠️ MARC27 KG persists but agent doesn't write to it |
| Agent learns from past sessions | ❌ |

## Goals

1. Tool outputs persist as queryable artifacts across turns and across sessions.
2. **Provenance is mandatory** — every recalled result tracks origin (tool, args, timestamp, session_id).
3. **Local-first.** Works offline. Promotion to MARC27 KG is opt-in.
4. Composes with existing MARC27 server-side persistence (`research_sessions` table, R2 artifact storage, embedded answers) — does not duplicate it.
5. Single embedding model — the EmbeddingGemma instance already loaded for Stage 2.1 retrieval also powers artifact indexing. One model, two directions (input-side tool selection + output-side artifact recall).
6. Verbatim storage for tool RESULTS (Memex-style — never overwrite). Evolution of *ideas* is a separate future concern (A-MEM-style); not in this scope.

## Non-Goals

- A-MEM-style evolving idea graph (where new memories rewrite old ones). Separate future PR.
- Cross-machine sync. Local-only. Cross-machine = MARC27 KG via promotion.
- Adding `research()` to the marc27-sdk Python package. Use `BaseAPI` directly; upstream the SDK method later.
- Caching/deduplication of identical recent queries. Out of scope; can add later if measurement says so.

## Architecture: Two Layers

```
┌─────────────────────────────────────────────────────────────────┐
│                         AGENT LOOP                              │
│                                                                 │
│  Tool.execute(args) → result, plus _artifact_id                 │
│                              │                                  │
│                              ▼                                  │
│                  ┌──────────────────────┐                       │
│                  │ ArtifactStore.record │  (local, SQLite)      │
│                  │  + EmbeddingGemma    │                       │
│                  └──────────────────────┘                       │
│                              │                                  │
│   recall("…") ◄──────────────┘     promote ─────► MARC27 KG     │
│                                                                 │
│   research(question) ───────────────────► /research/query SSE   │
│                                              (server-side state)│
└─────────────────────────────────────────────────────────────────┘
```

### Layer 1: Local Artifact Store (Memex-style, this PR)

For every tool output that meets the `should_record` heuristic.

**Storage:** `~/.prism/sessions/<session_id>/artifacts.db` (SQLite, WAL mode).

**Schema:**

```sql
CREATE TABLE artifacts (
    id              TEXT PRIMARY KEY,        -- stable: art_<base32(8)>
    session_id      TEXT NOT NULL,
    tool_name       TEXT NOT NULL,
    args_json       TEXT NOT NULL,           -- canonicalized for reproducibility
    result_json     TEXT NOT NULL,           -- verbatim
    summary         TEXT NOT NULL,           -- compact human-readable
    embedding       BLOB,                    -- f32[768], EmbeddingGemma vector
    record_count    INTEGER,                 -- if list-shaped, items count
    bytes_size      INTEGER NOT NULL,
    created_at      TEXT NOT NULL,           -- ISO8601 UTC
    promoted_to_kg  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE artifact_records (
    artifact_id     TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    record_idx      INTEGER NOT NULL,
    record_json     TEXT NOT NULL,
    record_summary  TEXT NOT NULL,
    embedding       BLOB,
    PRIMARY KEY (artifact_id, record_idx)
);

CREATE INDEX idx_artifacts_session ON artifacts(session_id);
CREATE INDEX idx_artifacts_tool    ON artifacts(tool_name);
CREATE INDEX idx_artifacts_created ON artifacts(created_at);
```

**Why two tables:** for list-shaped results (e.g. `materials_search` returns 50
materials), we want per-record embeddings so `recall("ones with band gap > 2 eV")`
can hit the right rows directly, not a single coarse summary. Per-record vectors
also enable filtered recall (`recall(query="…", from_artifact=art_xyz)`).

**Atomic record-or-fail.** Every `ArtifactStore.record()` call is one SQLite
transaction. On any error mid-record, the entire artifact is rolled back. The
tool's return path is unaffected — failure to record falls through with a logged
warning, not a raised exception. The agent never breaks on storage failures.

### Layer 2: Server-side via MARC27 research API (separate PR)

For research-shaped queries (open-ended scientific questions: "find Ti alloys
with high creep resistance and explain why"). marc27-core already implements
this end-to-end at `crates/core/src/research/`:

- `POST /api/v1/knowledge/research/query` (SSE stream)
- Persists to `research_sessions` Postgres table (steps + cost + provenance + R2 prefix)
- Auto-embeds the final answer for future semantic retrieval
- Hierarchical sub-sessions (RLM-style decomposition, depth-bounded)
- Tenant isolation + RBAC

**PRISM exposes this as the `research(question, depth=1)` tool.** The local
artifact store records the returned `session_id` + summary so it shows up in
`recall()` like any other tool output.

## New Tools

### `recall(query, scope='session'|'all', tool=None, limit=10)`
Semantic search over local artifacts.

- `scope='session'` (default): search current session only — fast, focused.
- `scope='all'`: search across all sessions in `~/.prism/sessions/`.
- `tool='materials_search'`: filter by tool that produced the artifact.
- Returns: `[{artifact_id, summary, score, tool, created_at, _record_idx?}]`
  ordered by cosine similarity. If a list-record matched, includes `_record_idx`.

### `fetch_artifact(artifact_id, record_idx=None)`
Fetch the full verbatim data — no truncation, no summary.

- `record_idx=None` returns the full artifact result.
- `record_idx=N` returns one specific record from a list-shaped artifact.

### `list_artifacts(session=None, tool=None, since=None, limit=20)`
Non-semantic listing — by tool, by time, by session. Useful when the LLM wants
to see "what have we done recently?" without a query.

### `research(question, depth=1)`
Calls MARC27 `/research/query` SSE endpoint. Streams progress to the agent
context, persists the session_id as a local artifact when complete. Returns
`{session_id, answer, cost_usd, entities_created, embeddings_created}`.

### `knowledge(action='promote_artifact', artifact_id=...)` (new action on existing tool)
Promotes a local artifact to the MARC27 KG. Calls `/api/v1/knowledge/ingest-job`
with `source={"type": "artifact", "data": <result_json>}`. Returns the ingest
`job_id`. Marks the local artifact `promoted_to_kg=1`.

## Tool.execute Middleware

Implemented in `app/tools/base.py`:

```python
class Tool:
    def execute(self, **kwargs) -> dict:
        raw = self.func(**kwargs)
        if not _should_record(raw, tool=self):
            return raw
        try:
            artifact_id = _artifact_store.record(
                tool_name=self.name,
                args=kwargs,
                result=raw,
                session_id=_current_session_id(),
            )
            return _augment(raw, artifact_id)
        except Exception as e:
            logger.warning("artifact record failed for %s: %s", self.name, e)
            return raw  # graceful: storage failure does NOT break the tool
```

### `_should_record` heuristic
- Skip if `tool.record_artifacts is False` (set on `recall`, `list_artifacts`,
  `fetch_artifact`, `show_scratchpad`).
- Skip if `result` contains `"error"` and only error-shaped fields.
- Skip if `bytes_size < 512` (too small to be worth a vector).
- Skip if `result` has no list-shaped or large-text content (heuristic: at least
  one of `results`, `data`, `papers`, `materials`, `entities`, `content`, …).
- Otherwise: record.

### `_augment(result, artifact_id)`
Returns a shallow copy with `_artifact_id` injected, plus `_record_count` if
list-shaped. The LLM sees these in the tool output and can call `recall` /
`fetch_artifact` later.

## Embedding Strategy

- One **artifact-level** embedding per artifact (over `summary`).
- For list-shaped results: one **record-level** embedding per item (over
  `record_summary`, which is a single-line description of that record).
- Embeddings are stored as raw `f32[768]` bytes (LE) in SQLite BLOB columns.
- Cosine similarity is computed in Python with NumPy on top-K candidate set
  (no native sqlite-vss dependency — keep it simple, optimize if needed).
- The EmbeddingGemma instance is loaded ONCE at PRISM startup and shared
  between Stage 2.1 (input-side tool selection) and the artifact store
  (output-side recall). One model, two directions.

## Provenance Contract

Every artifact carries:
- `id` — stable across the artifact's lifetime
- `session_id` — which session created it
- `tool_name` — exact tool that ran
- `args_json` — canonicalized arguments (sorted keys, no whitespace) → reproducible
- `created_at` — ISO8601 UTC
- `promoted_to_kg` — bool, true iff uploaded to MARC27 KG

When the LLM cites a recalled result in its answer, it includes the
`artifact_id`. The user can run `prism artifact show art_xyz` (CLI surface,
later PR) to inspect the exact origin.

## Cross-Session Recall

`recall(scope='all')` opens each `~/.prism/sessions/<id>/artifacts.db` as
read-only and merges results by score. With ~hundreds of sessions and
~thousands of artifacts per session, this is fine for laptop scale. If we
ever exceed ~100K artifacts total, add a top-level index
`~/.prism/artifacts_index.db` that mirrors all per-session DBs. Don't
premature-optimize.

## What This Does NOT Build (Layer 1 scope)

| Out | Why |
|---|---|
| `research()` tool wired to MARC27 SSE | Layer 2, separate PR (~200 LOC + auth flow + SSE handling) |
| `promote_artifact` action on `knowledge` tool | Layer 2, depends on artifact store being shipped first |
| A-MEM-style idea evolution | Future, separate concern |
| Native `prism artifact …` CLI subcommand | Future, after the tool surface is stable |
| sqlite-vss / Faiss native vector index | Premature; NumPy cosine on top-K is fine until profiling says otherwise |

## Implementation Order

1. **`app/tools/memory/` module skeleton** — `__init__.py`, `store.py`, `embedder.py`, `tool.py`.
2. **`store.py` — schema + atomic record + cosine search** with unit tests.
3. **`embedder.py` — shared EmbeddingGemma loader**, lazy-init, single instance.
4. **`recall`, `fetch_artifact`, `list_artifacts` tools** — `tool.py`.
5. **`Tool.execute` middleware** in `app/tools/base.py` — wired to the artifact store.
6. **`bootstrap.py` integration** — register the 3 new tools, init the store at startup.
7. **End-to-end test** — call `materials_search`, then `recall("…")`, verify the result includes provenance.
8. **Docs** — this file (you're reading it) + a short user-facing one in README.

## Risks + Mitigations

- **Storage growth** — every tool call writes to disk. Mitigation: `bytes_size <
  512` filter + `record_artifacts=False` opt-out. Add `prism artifacts gc
  --older-than 90d` later if needed.
- **Embedding latency** — EmbeddingGemma is fast (<50ms for 1024 tokens) but
  list-shaped results with 50 items × per-record embed = up to 2.5s added. We
  embed in a background thread so the tool's return path is not blocked. The
  artifact_id is returned immediately; embeddings backfill async. `recall`
  considers an artifact unsearchable until its embedding is committed (clean
  failure, not stale data).
- **SQLite concurrency** — multiple tool calls concurrent in the same session
  share one DB file. WAL mode + per-connection BEGIN IMMEDIATE handles this.
- **Test fragility** — embedding outputs are non-deterministic across model
  versions. Tests use a stub embedder that returns deterministic vectors based
  on input hash, not the real EmbeddingGemma.

## Success Criteria

PR is mergeable when:
1. `recall(query)` returns relevant prior tool outputs from the current session
2. `fetch_artifact(id)` returns full verbatim data
3. Tool outputs in the agent context include `_artifact_id`
4. Storage failure does NOT break tool execution (graceful degrade)
5. Hybrid retrieval (BM25 + vector + RRF fusion) returns most-relevant first
6. `tests/test_memory_store.py` passes (≥15 unit tests)
7. `tests/test_memory_integration.py` passes (≥3 end-to-end scenarios)
8. Existing test suite doesn't regress (40 tools loaded, all `test_*.py` pass)
9. Architecture doc (this file) merged

---

## Revisions — corrections from senior-architect research validation

After the v1 modules were drafted, a structured research pass against 2026
production patterns (Corvus, agent-memory, sqlite-memory-mcp, OpenClaw
hybrid memory) and a code trace of PRISM's actual tool execution path
surfaced four design corrections that landed in the final implementation.

### Correction 1 — Single DB, not per-session

**v1 plan:** one SQLite file per session at `~/.prism/sessions/<id>/artifacts.db`.
**Final:** one file at `~/.prism/artifacts.db`, with `session_id` as a column.

Why: production agent memories (Corvus, agent-memory, sqlite-memory-mcp) all
use single-DB. SQLite WAL mode handles 10+ concurrent agent processes
cleanly. Cross-session recall becomes a single SELECT instead of
opening N DBs. Single file is easier to back up. GC of old sessions
becomes a row-level DELETE (still cheap). The contention argument that
motivated per-session is theoretical at laptop scale; WAL + busy_timeout
is the canonical answer.

### Correction 2 — Missing production PRAGMAs

**v1 plan:** `journal_mode=WAL`, `synchronous=NORMAL`. Done.
**Final:** all five production PRAGMAs.

```sql
PRAGMA journal_mode  = WAL;
PRAGMA synchronous   = NORMAL;
PRAGMA busy_timeout  = 5000;     -- CRITICAL: default 0 = coin flip
PRAGMA mmap_size     = 8388608;  -- 8MB mmap for hot reads
PRAGMA cache_size    = -2000;    -- 2MB page cache
PRAGMA temp_store    = MEMORY;   -- temp tables in RAM
PRAGMA foreign_keys  = ON;
```

`busy_timeout` is the most important — without it, every concurrent
write returns SQLITE_BUSY immediately instead of waiting up to 5s.

### Correction 3 — Hybrid retrieval, not vector-only

**v1 plan:** cosine over EmbeddingGemma vectors only.
**Final:** **hybrid** — FTS5 BM25 + vector + Reciprocal Rank Fusion.

Why: every production agent memory in 2026 (Corvus 52ms hybrid vs 45ms
vector-only, OpenClaw, agent-memory, sqlite-memory-mcp) uses both.
BM25 catches exact-name matches that semantic embeddings dilute;
vector catches conceptual paraphrase that BM25 misses. RRF fusion
sidesteps score-calibration headaches: each retriever returns ranked
results, RRF merges by `1/(k+rank)` per item, sums across retrievers.
FTS5 is in the SQLite stdlib — zero new dependency.

```python
# RRF fusion (k=60 is the canonical constant from the original paper)
def rrf(rankings: list[list[str]], k: int = 60) -> list[tuple[str, float]]:
    scores: dict[str, float] = {}
    for ranking in rankings:
        for rank, doc_id in enumerate(ranking, start=1):
            scores[doc_id] = scores.get(doc_id, 0.0) + 1.0 / (k + rank)
    return sorted(scores.items(), key=lambda t: t[1], reverse=True)
```

### Correction 4 — Tool.execute direct modification, not monkey-patch

**v1 plan:** monkey-patch `Tool.execute` at bootstrap to wrap with recording.
**Final:** modify `Tool.execute` in `app/tools/base.py` directly. Add a
`record_artifacts: bool = True` field to the dataclass. The execute
method always checks if recording is enabled (global memory config) and
the tool opts in.

Why this matters: `app/mcp_server.py:74` captures `execute_fn = tool.execute`
as a **bound method at registration time**. Bound methods cache
`(func, instance)` at creation. If we monkey-patch the class
afterwards, those captured bound methods STILL POINT to the old
unwrapped function. The MCP path would silently bypass recording.

By making the recording call a regular part of `Tool.execute`, every
caller — `tool_server.py:50`, `mcp_server.py:74`, future callers —
reaches the recorder through the same code path. No magic, no
ordering dependency on bootstrap, no surprise bypass for already-bound
methods. Verified by `mcp__codebase-memory-mcp.search_code` —
`tool.func` has zero external callers.

---

## Final module layout

```
app/tools/memory/
├── __init__.py          # public API + re-exports
├── store.py             # SQLite + FTS5 + vector hybrid recall
├── embedder.py          # ST + hash backend, singleton
├── recorder.py          # called from Tool.execute; replaces middleware.py
└── tool.py              # recall / fetch_artifact / list_artifacts

app/tools/base.py        # Tool.execute now calls recorder.record_if_enabled
```

`recorder.record_if_enabled(tool, args, result)` returns either the
augmented result (with `_artifact_id`) or the original result. Storage
failure is logged-and-swallowed, never raised — tool execution must
not break because of a memory subsystem issue.
