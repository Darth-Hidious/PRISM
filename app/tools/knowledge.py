"""Knowledge Plane tools — graph search, semantic search, ingest.

These tools connect to the live MARC27 Knowledge Service API.
They give the agent access to the 200K+ node knowledge graph,
semantic search over 6K+ embeddings, and the ingest pipeline.
"""
import logging
from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


class _DirectClient:
    """Fallback HTTP client when marc27 SDK is not installed.

    Uses MARC27_API_KEY and MARC27_API_URL env vars passed by the Rust CLI.
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
            headers={"Authorization": f"Bearer {self.api_key}", "Content-Type": "application/json"},
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

    def graph_paths(self, from_entity: str, to_entity: str):
        return self._c._get("/knowledge/paths", {"from": from_entity, "to": to_entity})

    def graph_stats(self):
        return self._c._get("/knowledge/graph/stats")

    def semantic_search(self, query: str, limit: int = 10):
        return self._c._post("/knowledge/search", {"query": query, "limit": limit})

    def list_corpora(self):
        return self._c._get("/knowledge/catalog")


def _get_client():
    """Get or create a MARC27 PlatformClient.

    Tries marc27 SDK first, falls back to direct HTTP using env vars.
    """
    try:
        from marc27 import PlatformClient
        return PlatformClient()
    except Exception:
        pass

    import os
    api_key = os.environ.get("MARC27_API_KEY", "")
    api_url = os.environ.get("MARC27_API_URL", "https://api.marc27.com/api/v1")
    if api_key:
        return _DirectClient(api_url, api_key)

    logger.warning("No MARC27 auth — set MARC27_API_KEY or install marc27 SDK")
    return None


def _graph_search(**kwargs) -> dict:
    """Search the knowledge graph for entities by name."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected. Run `prism login` first."}

    term = kwargs.get("term", kwargs.get("query", ""))
    limit = kwargs.get("limit", 20)

    try:
        results = client.knowledge.graph_search(term, limit=limit)
        return {
            "results": results,
            "count": len(results),
            "query": term,
            "source": "marc27_knowledge_graph",
        }
    except Exception as e:
        return {"error": str(e)}


def _graph_entity(**kwargs) -> dict:
    """Get an entity and its neighbors from the knowledge graph."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    name = kwargs.get("name", kwargs.get("entity", ""))
    limit = kwargs.get("limit", 10)

    try:
        result = client.knowledge.graph_entity(name, limit=limit)
        return {
            "entity": result.get("entity"),
            "neighbors": result.get("neighbors", {}),
            "source": "marc27_knowledge_graph",
        }
    except Exception as e:
        return {"error": str(e)}


def _graph_paths(**kwargs) -> dict:
    """Find shortest paths between two entities in the knowledge graph."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    from_entity = kwargs.get("from_entity", kwargs.get("from", ""))
    to_entity = kwargs.get("to_entity", kwargs.get("to", ""))
    max_hops = kwargs.get("max_hops", 3)

    try:
        paths = client.knowledge.graph_paths(from_entity, to_entity, max_hops=max_hops)
        return {
            "paths": paths,
            "from": from_entity,
            "to": to_entity,
            "source": "marc27_knowledge_graph",
        }
    except Exception as e:
        return {"error": str(e)}


def _graph_stats(**kwargs) -> dict:
    """Get knowledge graph statistics."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    try:
        stats = client.knowledge.graph_stats()
        return {
            "nodes": stats.get("nodes", 0),
            "edges": stats.get("edges", 0),
            "entity_types": stats.get("entity_types", 0),
            "source": "marc27_knowledge_graph",
        }
    except Exception as e:
        return {"error": str(e)}


def _semantic_search(**kwargs) -> dict:
    """Semantic similarity search over embedded documents."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    query = kwargs.get("query", "")
    corpus_id = kwargs.get("corpus_id")
    limit = kwargs.get("limit", 10)

    try:
        results = client.knowledge.search(query, corpus_id=corpus_id, limit=limit)
        return {
            "results": results,
            "count": len(results),
            "query": query,
            "source": "marc27_pgvector",
        }
    except Exception as e:
        return {"error": str(e)}


def _list_corpora(**kwargs) -> dict:
    """List available data corpora in the Knowledge Plane."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    domain = kwargs.get("domain")
    kind = kwargs.get("kind")
    limit = kwargs.get("limit", 50)

    try:
        corpora = client.knowledge.list_corpora(domain=domain, kind=kind, limit=limit)
        return {
            "corpora": corpora,
            "count": len(corpora),
            "source": "marc27_catalog",
        }
    except Exception as e:
        return {"error": str(e)}


def _knowledge_ingest(**kwargs) -> dict:
    """Submit a knowledge ingest job (extract entities from URL/text)."""
    client = _get_client()
    if not client:
        return {"error": "MARC27 platform not connected."}

    source_url = kwargs.get("url", kwargs.get("source_url"))
    query = kwargs.get("query")
    mode = kwargs.get("mode", "full")

    try:
        from marc27.api.base import BaseAPI
        base: BaseAPI = client._base

        body = {"mode": mode}
        if source_url:
            body["source"] = {"type": "url", "url": source_url}
        elif query:
            body["source"] = {"type": "query", "query": query}
        else:
            return {"error": "Provide either 'url' or 'query'"}

        resp = base.post("/knowledge/ingest-job", json=body)
        return resp.json()
    except Exception as e:
        return {"error": str(e)}


def create_knowledge_tools(registry: ToolRegistry) -> None:
    """Register all Knowledge Plane tools."""

    registry.register(Tool(
        name="knowledge_search",
        description=(
            "Search the MARC27 knowledge graph (200K+ nodes, 6M+ edges) for "
            "materials, properties, elements, publications, authors, and topics. "
            "Returns entity names, types, and labels. Use this to find what the "
            "platform knows about a material or concept."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "term": {
                    "type": "string",
                    "description": "Search term, e.g. 'titanium', 'band gap', 'MOF'",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 20)",
                    "default": 20,
                },
            },
            "required": ["term"],
        },
        func=_graph_search,
    ))

    registry.register(Tool(
        name="knowledge_entity",
        description=(
            "Get a specific entity and its neighbors from the knowledge graph. "
            "Shows what's connected — properties, compositions, structures, "
            "authors, topics. Use after knowledge_search to explore relationships."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Entity name (exact match from knowledge_search)",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max neighbors to return (default 10)",
                    "default": 10,
                },
            },
            "required": ["name"],
        },
        func=_graph_entity,
    ))

    registry.register(Tool(
        name="knowledge_paths",
        description=(
            "Find shortest paths between two entities in the knowledge graph. "
            "Shows how materials relate to properties, methods, or applications. "
            "Example: path from 'Ti-6Al-4V' to 'Fatigue Resistance'."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "from_entity": {"type": "string", "description": "Start entity name"},
                "to_entity": {"type": "string", "description": "End entity name"},
                "max_hops": {
                    "type": "integer",
                    "description": "Max path length (default 3)",
                    "default": 3,
                },
            },
            "required": ["from_entity", "to_entity"],
        },
        func=_graph_paths,
    ))

    registry.register(Tool(
        name="knowledge_stats",
        description=(
            "Get statistics about the MARC27 knowledge graph — total nodes, "
            "edges, and entity types. Use to understand the scope of available data."
        ),
        input_schema={"type": "object", "properties": {}},
        func=_graph_stats,
    ))

    registry.register(Tool(
        name="semantic_search",
        description=(
            "Semantic similarity search over embedded materials science documents "
            "using Gemini Embedding 2 (3072-dim). Returns the most semantically "
            "similar content to your query. Better than keyword search for "
            "conceptual queries like 'high strength alloy for aerospace'."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language query",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 10)",
                    "default": 10,
                },
            },
            "required": ["query"],
        },
        func=_semantic_search,
    ))

    registry.register(Tool(
        name="list_corpora",
        description=(
            "List available data corpora in the MARC27 Knowledge Plane. "
            "Shows datasets like Materials Project (154K materials), JARVIS-DFT "
            "(80K), QMOF (20K MOFs), MatKG (3.5M triples), and more. "
            "Filter by domain (materials, chemistry) or kind (structured_db, "
            "knowledge_graph, literature)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "domain": {
                    "type": "string",
                    "description": "Filter by domain: materials, chemistry, biomedical, physics",
                },
                "kind": {
                    "type": "string",
                    "description": "Filter by kind: structured_db, knowledge_graph, literature, ontology",
                },
                "limit": {"type": "integer", "default": 50},
            },
        },
        func=_list_corpora,
    ))

    registry.register(Tool(
        name="knowledge_ingest",
        description=(
            "Submit a knowledge ingest job — extract entities from a URL or "
            "natural language query into the knowledge graph. Uses LLM-powered "
            "entity extraction. The job runs in the background; check status "
            "with the returned job_id."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to extract knowledge from (paper, dataset page, etc.)",
                },
                "query": {
                    "type": "string",
                    "description": "Natural language query to discover and ingest data",
                },
                "mode": {
                    "type": "string",
                    "description": "Extraction mode: 'graph' (entities only), 'embed' (vectors only), 'full' (both)",
                    "default": "full",
                },
            },
        },
        func=_knowledge_ingest,
    ))
