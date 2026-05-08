"""Knowledge Plane — UNIFIED tool collapsing 7 actions into one entry point.

DRAFT for Round 4. Replaces app/tools/knowledge.py.

Replaces (7 tools → 1):
  knowledge_search   → action='search'        (graph entity lookup by term)
  knowledge_entity   → action='entity'        (entity + 1-hop neighbors)
  knowledge_paths    → action='paths'         (shortest paths between two entities)
  knowledge_stats    → action='stats'         (graph metrics; no args)
  knowledge_ingest   → action='ingest'        (background extract job from URL/query)
  semantic_search    → action='semantic'      (pgvector similarity over embedded docs)
  list_corpora       → action='list_corpora'  (catalog of available corpora)

KNOWN BUG (preserved from current code, NOT fixing in this collapse):
  The semantic-search direct-HTTP fallback class has method `semantic_search`
  but the call site invokes `client.knowledge.search` — only works with the
  marc27 SDK, breaks with MARC27_API_KEY-only auth. Flag for follow-up.
"""
import logging
import os
from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


class _DirectClient:
    """HTTP fallback when marc27 SDK isn't installed.

    Uses MARC27_API_KEY + MARC27_API_URL env vars.
    """

    def __init__(self, api_url: str, api_key: str):
        self.api_url = api_url.rstrip("/")
        self.api_key = api_key
        self.knowledge = _DirectKnowledge(self)

    def _get(self, path: str, params: dict | None = None) -> dict:
        import requests
        resp = requests.get(
            f"{self.api_url}{path}",
            headers={"Authorization": f"Bearer {self.api_key}"},
            params=params or {},
            timeout=15,
        )
        resp.raise_for_status()
        return resp.json()

    def _post(self, path: str, body: dict) -> dict:
        import requests
        resp = requests.post(
            f"{self.api_url}{path}",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json=body,
            timeout=15,
        )
        resp.raise_for_status()
        return resp.json()


class _DirectKnowledge:
    def __init__(self, client: _DirectClient):
        self._c = client

    def graph_search(self, term: str, limit: int = 20):
        return self._c._get("/knowledge/graph/search", {"q": term, "limit": str(limit)})

    def graph_entity(self, name: str, limit: int = 10):
        return self._c._get(f"/knowledge/entity/{name}", {"limit": str(limit)})

    def graph_paths(self, from_entity: str, to_entity: str, max_hops: int = 3):
        return self._c._get(
            "/knowledge/paths",
            {"from": from_entity, "to": to_entity, "max_hops": str(max_hops)},
        )

    def graph_stats(self):
        return self._c._get("/knowledge/graph/stats")

    def semantic_search(self, query: str, limit: int = 10, corpus_id=None):
        body = {"query": query, "limit": limit}
        if corpus_id:
            body["corpus_id"] = corpus_id
        return self._c._post("/knowledge/search", body)

    def list_corpora(self, domain=None, kind=None, limit: int = 50):
        params = {"limit": str(limit)}
        if domain:
            params["domain"] = domain
        if kind:
            params["kind"] = kind
        return self._c._get("/knowledge/catalog", params)


def _get_client():
    """Try marc27 SDK first, fall back to direct HTTP."""
    try:
        from marc27 import PlatformClient
        return PlatformClient()
    except Exception:
        pass

    api_key = os.environ.get("MARC27_API_KEY", "")
    api_url = os.environ.get("MARC27_API_URL", "https://api.marc27.com/api/v1")
    if api_key:
        return _DirectClient(api_url, api_key)

    logger.warning("No MARC27 auth — set MARC27_API_KEY or install marc27 SDK")
    return None


# --- Per-action handlers ---------------------------------------------------

def _act_search(client, **kw) -> dict:
    term = kw.get("term") or kw.get("query", "")
    if not term:
        return {"error": "Action 'search' requires `term` (or `query`)"}
    results = client.knowledge.graph_search(term, limit=kw.get("limit", 20))
    return {
        "results": results,
        "count": len(results) if hasattr(results, "__len__") else None,
        "query": term,
        "source": "marc27_knowledge_graph",
    }


def _act_entity(client, **kw) -> dict:
    name = kw.get("name") or kw.get("entity", "")
    if not name:
        return {"error": "Action 'entity' requires `name`"}
    result = client.knowledge.graph_entity(name, limit=kw.get("limit", 10))
    return {
        "entity": result.get("entity") if isinstance(result, dict) else result,
        "neighbors": result.get("neighbors", {}) if isinstance(result, dict) else {},
        "source": "marc27_knowledge_graph",
    }


def _act_paths(client, **kw) -> dict:
    from_entity = kw.get("from_entity") or kw.get("from", "")
    to_entity = kw.get("to_entity") or kw.get("to", "")
    if not from_entity or not to_entity:
        return {"error": "Action 'paths' requires `from_entity` and `to_entity`"}
    paths = client.knowledge.graph_paths(
        from_entity, to_entity, max_hops=kw.get("max_hops", 3),
    )
    return {
        "paths": paths,
        "from": from_entity,
        "to": to_entity,
        "source": "marc27_knowledge_graph",
    }


def _act_stats(client, **_) -> dict:
    stats = client.knowledge.graph_stats()
    if isinstance(stats, dict):
        return {
            "nodes": stats.get("nodes", 0),
            "edges": stats.get("edges", 0),
            "entity_types": stats.get("entity_types", 0),
            "source": "marc27_knowledge_graph",
        }
    return {"raw": stats, "source": "marc27_knowledge_graph"}


def _act_semantic(client, **kw) -> dict:
    query = kw.get("query", "")
    if not query:
        return {"error": "Action 'semantic' requires `query`"}
    # NOTE preserved from existing code: SDK path uses client.knowledge.search,
    # direct-HTTP path uses client.knowledge.semantic_search. We try both.
    if hasattr(client.knowledge, "search"):
        results = client.knowledge.search(
            query, corpus_id=kw.get("corpus_id"), limit=kw.get("limit", 10),
        )
    else:
        results = client.knowledge.semantic_search(
            query, corpus_id=kw.get("corpus_id"), limit=kw.get("limit", 10),
        )
    return {
        "results": results,
        "count": len(results) if hasattr(results, "__len__") else None,
        "query": query,
        "source": "marc27_pgvector",
    }


def _act_list_corpora(client, **kw) -> dict:
    corpora = client.knowledge.list_corpora(
        domain=kw.get("domain"), kind=kw.get("kind"), limit=kw.get("limit", 50),
    )
    return {
        "corpora": corpora,
        "count": len(corpora) if hasattr(corpora, "__len__") else None,
        "source": "marc27_catalog",
    }


def _act_ingest(client, **kw) -> dict:
    source_url = kw.get("url") or kw.get("source_url")
    query = kw.get("query")
    mode = kw.get("mode", "full")
    if not source_url and not query:
        return {"error": "Action 'ingest' requires `url` or `query`"}

    try:
        # SDK path uses base.post; direct path uses _post.
        body = {"mode": mode}
        if source_url:
            body["source"] = {"type": "url", "url": source_url}
        else:
            body["source"] = {"type": "query", "query": query}

        if hasattr(client, "_base"):
            from marc27.api.base import BaseAPI  # type: ignore
            base: BaseAPI = client._base
            resp = base.post("/knowledge/ingest-job", json=body)
            return resp.json()
        # Direct HTTP fallback
        return client._post("/knowledge/ingest-job", body)
    except Exception as e:
        return {"error": str(e)}


_DISPATCH = {
    "search":       _act_search,
    "entity":       _act_entity,
    "paths":        _act_paths,
    "stats":        _act_stats,
    "semantic":     _act_semantic,
    "list_corpora": _act_list_corpora,
    "ingest":       _act_ingest,
}


def _knowledge(**kwargs) -> dict:
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_DISPATCH.keys())}",
            "hint": "e.g. knowledge(action='search', term='titanium') or knowledge(action='stats')",
        }
    handler = _DISPATCH.get(action)
    if not handler:
        return {"error": f"Unknown action '{action}'. Valid: {list(_DISPATCH.keys())}"}

    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected. Run `prism login` first."}

    try:
        return handler(client, **kwargs)
    except Exception as e:
        return {"error": str(e), "action": action}


_DESCRIPTION = (
    "MARC27 Knowledge Plane — graph + semantic search over the live "
    "knowledge service (200K+ nodes, 6M+ edges, 6K+ vector embeddings, "
    "200+ corpora catalog). ONE tool, seven actions:\n"
    "  • action='search' — graph entity lookup by `term` (best for 'find Ti-6Al-4V')\n"
    "  • action='entity' — get one entity + 1-hop neighbors by `name`\n"
    "  • action='paths' — shortest hop-paths from `from_entity` to `to_entity`; "
    "use for 'how does X relate to Y?'\n"
    "  • action='stats' — graph metrics (nodes/edges/types). No args. Use to "
    "verify graph is populated.\n"
    "  • action='semantic' — pgvector similarity by natural-language `query` "
    "(better than 'search' for conceptual asks like 'high strength alloy for aerospace')\n"
    "  • action='list_corpora' — catalog of available datasets (Materials Project, "
    "JARVIS-DFT, QMOF, MatKG…). Filter by `domain`/`kind`.\n"
    "  • action='ingest' — submit background extraction job; requires `url` or `query`.\n"
    "NOT for raw structure DBs (use materials_search) and NOT for paper text (use prior_art_search)."
)

_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which knowledge-plane operation to perform.",
        },
        # search / entity
        "term":   {"type": "string", "description": "Search term for action='search'."},
        "name":   {"type": "string", "description": "Entity name for action='entity'."},
        # paths
        "from_entity": {"type": "string", "description": "Path start entity for action='paths'."},
        "to_entity":   {"type": "string", "description": "Path end entity for action='paths'."},
        "max_hops":    {"type": "integer", "default": 3, "minimum": 1, "maximum": 8,
                        "description": "Max path length for action='paths'."},
        # semantic
        "query":     {"type": "string", "description": "Natural-language query for action='semantic' or action='ingest'."},
        "corpus_id": {"type": "string", "description": "Restrict action='semantic' to one corpus."},
        # list_corpora
        "domain": {"type": "string", "description": "Filter for action='list_corpora': materials/chemistry/biomedical/physics."},
        "kind":   {"type": "string", "description": "Filter for action='list_corpora': structured_db/knowledge_graph/literature/ontology."},
        # ingest
        "url":  {"type": "string", "description": "Source URL for action='ingest'."},
        "mode": {"type": "string", "default": "full",
                 "description": "Extraction mode for action='ingest': graph/embed/full."},
        # shared
        "limit": {"type": "integer", "description": "Max results (default varies by action)."},
    },
    "required": ["action"],
    "additionalProperties": False,
}


def create_knowledge_tools(registry: ToolRegistry) -> None:
    """Register the unified `knowledge` tool (replaces 7 prior tools)."""
    registry.register(Tool(
        name="knowledge",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_knowledge,
    ))
