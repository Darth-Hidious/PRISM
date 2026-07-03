"""Knowledge-write tools — wrap the MARC27 Knowledge Service WRITE endpoints.

The agent already has READ access to the knowledge graph (`research`, the
`/knowledge/graph/search`, `/graph/entity`, `/graph/paths`, `/graph/stats`
GET routes). It had no way to *contribute* — embed text, batch-embed
docs, seed the graph from CSV, ingest a structured doc, or call the
platform's web-search service. This module closes that asymmetry.

ONE dispatcher tool, five actions, mirroring `calphad_compute`:

  • action='embed'             → POST /knowledge/embed
  • action='embed_bulk'        → POST /knowledge/embed/bulk
  • action='graph_seed'        → POST /knowledge/graph/seed
  • action='graph_ingest'      → POST /knowledge/graph/ingest
  • action='research_web_search' → POST /knowledge/research/web-search

ALL of these mutate platform state and/or spend compute (embeddings cost
money, web-search hits external academic APIs, graph writes are
durable). The tool is `requires_approval=True` — the harness prompts
once per call.

Auth path mirrors `app/tools/research.py:_resolve_credentials` and
`app/tools/platform_status.py:_resolve_credentials` — `MARC27_API_KEY`
env var with `~/.prism/credentials.json` access_token fallback.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Optional

import requests

from app.tools._platform_client import platform

from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Shared auth (mirror of platform_status.py:_resolve_credentials, kept
# private here so each tool module can evolve its own auth handling later
# if needed).
# ---------------------------------------------------------------------------

def _resolve_credentials() -> tuple[str, str]:
    """Return (api_url, access_token) from env or `~/.prism/credentials.json`."""
    api_url = os.environ.get(
        "MARC27_API_URL", "https://api.marc27.com/api/v1"
    ).rstrip("/")
    api_key = os.environ.get("MARC27_API_KEY", "")

    if not api_key:
        try:
            creds_path = Path.home() / ".prism" / "credentials.json"
            if creds_path.exists():
                creds = json.loads(creds_path.read_text())
                api_key = creds.get("access_token", "")
                if creds.get("platform_url"):
                    api_url = creds["platform_url"].rstrip("/")
                    if not api_url.endswith("/api/v1"):
                        api_url = api_url + "/api/v1"
        except Exception:
            pass

    return api_url, api_key


def _get(path: str) -> dict:
    """GET helper. Returns dict with 'error' on failure, parsed JSON otherwise."""
    return platform().get(path, timeout=10)


def _post(path: str, body: dict) -> dict:
    return platform().post(path, json=body, timeout=15)


# ---------------------------------------------------------------------------
# Per-action handlers
# ---------------------------------------------------------------------------

_VALID_ACTIONS = (
    "embed",
    "embed_bulk",
    "graph_seed",
    "graph_ingest",
    "research_web_search",
)


def _do_embed(**kwargs: Any) -> dict:
    """POST /knowledge/embed — embed a single text and store the vector.

    Request body (per knowledge-service `EmbedRequest`):
      doc_id   (required, str)
      content  (required, str)
      corpus_id (optional, uuid str)
      metadata (optional, object)
    """
    doc_id = kwargs.get("doc_id")
    content = kwargs.get("content")
    if not doc_id:
        return {"error": "Action 'embed' requires `doc_id`."}
    if not content:
        return {"error": "Action 'embed' requires `content`."}
    body: dict = {"doc_id": doc_id, "content": content}
    if kwargs.get("corpus_id"):
        body["corpus_id"] = kwargs["corpus_id"]
    if kwargs.get("metadata") is not None:
        body["metadata"] = kwargs["metadata"]
    return _post("/knowledge/embed", body)


def _do_embed_bulk(**kwargs: Any) -> dict:
    """POST /knowledge/embed/bulk — kick off background batch embedding.

    Request body (per knowledge-service `BulkEmbedRequest`):
      corpus_id  (required, uuid str)
      documents  (required, list of {doc_id, content, metadata?})
    """
    corpus_id = kwargs.get("corpus_id")
    documents = kwargs.get("documents")
    if not corpus_id:
        return {"error": "Action 'embed_bulk' requires `corpus_id`."}
    if not documents or not isinstance(documents, list):
        return {
            "error": "Action 'embed_bulk' requires `documents` (non-empty list)."
        }
    body = {"corpus_id": corpus_id, "documents": documents}
    return _post("/knowledge/embed/bulk", body)


def _do_graph_seed(**kwargs: Any) -> dict:
    """POST /knowledge/graph/seed — seed the graph from nodes/edges CSV URLs.

    Admin-only on the server. The 403 surfaces here as an HTTP error.

    Request body (per knowledge-service `SeedRequest`):
      nodes_url  (required, str)
      edges_url  (required, str)
    """
    nodes_url = kwargs.get("nodes_url")
    edges_url = kwargs.get("edges_url")
    if not nodes_url:
        return {"error": "Action 'graph_seed' requires `nodes_url`."}
    if not edges_url:
        return {"error": "Action 'graph_seed' requires `edges_url`."}
    return _post(
        "/knowledge/graph/seed",
        {"nodes_url": nodes_url, "edges_url": edges_url},
    )


def _do_graph_ingest(**kwargs: Any) -> dict:
    """POST /knowledge/graph/ingest — ingest entities + relationships.

    Request body (per `marc27_core::graph::ingest::GraphIngestInput`):
      entities       (required, list of {name, entity_type, label, properties?})
      relationships  (required, list of {from_name, to_name, rel_type, properties?})

    Server overrides `tenant` from auth — caller does not need to set it.
    """
    entities = kwargs.get("entities")
    relationships = kwargs.get("relationships")
    if entities is None or not isinstance(entities, list):
        return {"error": "Action 'graph_ingest' requires `entities` (list)."}
    if relationships is None or not isinstance(relationships, list):
        return {
            "error": "Action 'graph_ingest' requires `relationships` (list, may be empty)."
        }
    return _post(
        "/knowledge/graph/ingest",
        {"entities": entities, "relationships": relationships},
    )


def _do_research_web_search(**kwargs: Any) -> dict:
    """POST /knowledge/research/web-search — platform web-search across
    Semantic Scholar / arXiv / PubMed / OpenAlex.

    Request body (per knowledge-service `WebSearchQuery`):
      query  (required, str)
      limit  (optional, int — server default 5)
    """
    query = kwargs.get("query")
    if not query:
        return {"error": "Action 'research_web_search' requires `query`."}
    body: dict = {"query": query}
    if "limit" in kwargs and kwargs["limit"] is not None:
        body["limit"] = kwargs["limit"]
    return _post("/knowledge/research/web-search", body)


# ---------------------------------------------------------------------------
# Dispatcher
# ---------------------------------------------------------------------------

_DISPATCH = {
    "embed": _do_embed,
    "embed_bulk": _do_embed_bulk,
    "graph_seed": _do_graph_seed,
    "graph_ingest": _do_graph_ingest,
    "research_web_search": _do_research_web_search,
}


def _knowledge_write(**kwargs: Any) -> dict:
    """Dispatch a knowledge-write action. Approval-gated upstream."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": (
                "Missing 'action'. Valid: "
                + ", ".join(_VALID_ACTIONS)
            ),
            "hint": (
                "knowledge_write(action='embed', doc_id='...', content='...') / "
                "knowledge_write(action='embed_bulk', corpus_id='...', documents=[...]) / "
                "knowledge_write(action='graph_seed', nodes_url='...', edges_url='...') / "
                "knowledge_write(action='graph_ingest', entities=[...], relationships=[...]) / "
                "knowledge_write(action='research_web_search', query='...')"
            ),
        }
    handler = _DISPATCH.get(action)
    if handler is None:
        return {
            "error": (
                f"Unknown action '{action}'. Valid: "
                + ", ".join(_VALID_ACTIONS)
            )
        }
    return handler(**kwargs)


# ---------------------------------------------------------------------------
# Tool description + registration
# ---------------------------------------------------------------------------

_KNOWLEDGE_WRITE_DESCRIPTION = (
    "WRITE side of the MARC27 Knowledge Service — closes the agent's "
    "read/write asymmetry. Read paths live in the `research` tool and "
    "the platform's GET endpoints. ONE tool, five actions. "
    "MUTATING/COMPUTE-SPENDING — requires_approval=True; the harness "
    "will prompt before each call.\n"
    "  • action='embed' — embed a single text and store the vector. "
    "Required: `doc_id`, `content`. Optional: `corpus_id` (uuid), "
    "`metadata` (object).\n"
    "  • action='embed_bulk' — kick off a background batch embed for a "
    "whole corpus. Required: `corpus_id` (uuid), `documents` "
    "(list of {doc_id, content, metadata?}). Returns immediately; "
    "monitor via /embeddings/stats.\n"
    "  • action='graph_seed' — admin-only. Seed the knowledge graph "
    "from CSV files via presigned URLs. Required: `nodes_url`, "
    "`edges_url`. Non-admins receive 403 from the server.\n"
    "  • action='graph_ingest' — ingest a structured set of entities "
    "and relationships into the graph. Required: `entities` "
    "(list of {name, entity_type, label, properties?}), `relationships` "
    "(list of {from_name, to_name, rel_type, properties?}). The server "
    "overrides `tenant` from auth — do not set it client-side.\n"
    "  • action='research_web_search' — query the platform's academic "
    "web-search service (Semantic Scholar, arXiv, PubMed, OpenAlex). "
    "Required: `query`. Optional: `limit` (server default 5). This is "
    "the platform-hosted variant; the local `web` tool only does "
    "single-page fetch."
)


def create_knowledge_write_tool(registry: ToolRegistry) -> None:
    """Register the unified `knowledge_write` dispatcher tool.

    Approval-gated like `calphad_compute` — every action either spends
    compute (embeddings, web-search) or persists state to the graph.
    """
    registry.register(Tool(
        name="knowledge_write",
        description=_KNOWLEDGE_WRITE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": list(_VALID_ACTIONS),
                    "description": "Which knowledge-write operation to run.",
                },
                # embed / embed_bulk
                "doc_id": {
                    "type": "string",
                    "description": "Document id for action='embed'. Required for 'embed'.",
                },
                "content": {
                    "type": "string",
                    "description": "Text to embed for action='embed'. Required for 'embed'.",
                },
                "corpus_id": {
                    "type": "string",
                    "description": (
                        "Corpus UUID. Optional for action='embed', "
                        "required for action='embed_bulk'."
                    ),
                },
                "metadata": {
                    "type": "object",
                    "description": "Optional metadata object for action='embed'.",
                    "additionalProperties": True,
                },
                "documents": {
                    "type": "array",
                    "description": (
                        "Documents for action='embed_bulk', each "
                        "{doc_id, content, metadata?}."
                    ),
                    "items": {
                        "type": "object",
                        "properties": {
                            "doc_id": {"type": "string"},
                            "content": {"type": "string"},
                            "metadata": {"type": "object", "additionalProperties": True},
                        },
                        "required": ["doc_id", "content"],
                        "additionalProperties": True,
                    },
                },
                # graph_seed
                "nodes_url": {
                    "type": "string",
                    "description": "Nodes CSV URL for action='graph_seed'.",
                },
                "edges_url": {
                    "type": "string",
                    "description": "Edges CSV URL for action='graph_seed'.",
                },
                # graph_ingest
                "entities": {
                    "type": "array",
                    "description": (
                        "Entities for action='graph_ingest'. Each item: "
                        "{name, entity_type, label, properties?}."
                    ),
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "entity_type": {"type": "string"},
                            "label": {"type": "string"},
                            "properties": {
                                "type": "object",
                                "additionalProperties": {"type": "string"},
                            },
                        },
                        "required": ["name", "entity_type", "label"],
                        "additionalProperties": True,
                    },
                },
                "relationships": {
                    "type": "array",
                    "description": (
                        "Relationships for action='graph_ingest'. Each item: "
                        "{from_name, to_name, rel_type, properties?}. May be empty."
                    ),
                    "items": {
                        "type": "object",
                        "properties": {
                            "from_name": {"type": "string"},
                            "to_name": {"type": "string"},
                            "rel_type": {"type": "string"},
                            "properties": {
                                "type": "object",
                                "additionalProperties": {"type": "string"},
                            },
                        },
                        "required": ["from_name", "to_name", "rel_type"],
                        "additionalProperties": True,
                    },
                },
                # research_web_search
                "query": {
                    "type": "string",
                    "description": "Search query for action='research_web_search'.",
                },
                "limit": {
                    "type": "integer",
                    "description": (
                        "Max results for action='research_web_search'. "
                        "Server default 5."
                    ),
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_knowledge_write,
        requires_approval=True,
    ))
