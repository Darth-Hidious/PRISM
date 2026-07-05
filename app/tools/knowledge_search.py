"""Knowledge-read tool — search the MARC27 platform's embedded corpora + graph.

The write half exists (`knowledge_write`); the READ half was lost in a
refactor (the old `research.py` this module replaces). Without it the agent
cannot touch the platform's embedded corpora — NASA propulsion reports,
Materials Project, MatKG, alloy datasheets, AM datasets — and falls back to
hammering external literature APIs for questions the platform can answer
from its own knowledge. This closes that gap.

ONE dispatcher tool, three read actions:

  • action='semantic' → POST /knowledge/search        (corpus chunks by meaning)
  • action='graph'    → GET  /knowledge/graph/search  (knowledge-graph entities)
  • action='recall'   → POST /knowledge/recall        (facts + chunks for a query)

Read-only — no approval gate. Auth mirrors `knowledge_write.py`:
`MARC27_API_KEY` env var with `~/.prism/credentials.json` fallback.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

import requests

from app.tools.base import Tool, ToolRegistry


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
                        api_url = f"{api_url}/api/v1"
        except Exception:
            pass

    return api_url, api_key


def _compact(text: Any, limit: int = 500) -> str:
    """Chunk contents can be whole datasheet pages — trim so a top_k=5 result
    fits the agent's tool-result budget without truncation."""
    text = str(text or "").strip()
    return text if len(text) <= limit else text[:limit].rsplit(" ", 1)[0] + "…"


def _knowledge_search(**kwargs: Any) -> dict:
    action = (kwargs.get("action") or "semantic").lower()
    query = kwargs.get("query", "")
    if not query:
        return {"error": "query is required"}

    api_url, token = _resolve_credentials()
    if not token:
        return {
            "error": "no platform credentials — set MARC27_API_KEY or run `prism login`"
        }
    headers = {"Authorization": f"Bearer {token}"}
    top_k = int(kwargs.get("top_k", 5))

    try:
        if action == "semantic":
            body: dict[str, Any] = {"query": query, "top_k": top_k}
            if kwargs.get("corpus_id"):
                body["corpus_id"] = kwargs["corpus_id"]
            resp = requests.post(
                f"{api_url}/knowledge/search", json=body, headers=headers, timeout=60
            )
            resp.raise_for_status()
            hits = resp.json()
            return {
                "action": "semantic",
                "query": query,
                "count": len(hits),
                "chunks": [
                    {
                        "doc_id": h.get("doc_id"),
                        "chunk_idx": h.get("chunk_idx"),
                        "content": _compact(h.get("content")),
                        "score": h.get("score"),
                    }
                    for h in hits
                ],
            }

        if action == "graph":
            resp = requests.get(
                f"{api_url}/knowledge/graph/search",
                params={"q": query, "limit": top_k},
                headers=headers,
                timeout=60,
            )
            resp.raise_for_status()
            return {"action": "graph", "query": query, "results": resp.json()}

        if action == "recall":
            resp = requests.post(
                f"{api_url}/knowledge/recall",
                json={"query": query},
                headers=headers,
                timeout=60,
            )
            resp.raise_for_status()
            data = resp.json()
            for chunk in data.get("chunks", []) or []:
                chunk["content"] = _compact(chunk.get("content"))
            return {"action": "recall", "query": query, **data}

        return {
            "error": f"unknown action '{action}' — use semantic, graph, or recall"
        }
    except requests.HTTPError as exc:
        body = ""
        if exc.response is not None:
            body = exc.response.text[:300]
        return {"error": f"platform returned {exc}", "detail": body}
    except Exception as exc:  # honest failure, never a silent empty result
        return {"error": f"{type(exc).__name__}: {exc}"}


def create_knowledge_search_tool(registry: ToolRegistry) -> None:
    registry.register(Tool(
        name="knowledge_search",
        description=(
            "Search the MARC27 platform's OWN knowledge base: embedded "
            "corpora (NASA propulsion technical reports, Materials Project, "
            "MatKG, alloy/superalloy datasheets, additive-manufacturing and "
            "fatigue datasets) plus the materials knowledge graph. Prefer "
            "this BEFORE external literature searches for materials, alloy, "
            "propulsion, and manufacturing questions — the platform often "
            "already holds the answer with provenance. action='semantic' "
            "(default) searches corpus chunks by meaning; action='graph' "
            "finds knowledge-graph entities; action='recall' pulls stored "
            "facts + chunks for a query."
        ),
        parameters={
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to look for, phrased naturally.",
                },
                "action": {
                    "type": "string",
                    "enum": ["semantic", "graph", "recall"],
                    "description": "Which read mechanism to use (default semantic).",
                },
                "top_k": {
                    "type": "integer",
                    "description": "Max results (default 5).",
                },
                "corpus_id": {
                    "type": "string",
                    "description": "Optional corpus UUID to scope a semantic search.",
                },
            },
            "required": ["query"],
        },
        func=_knowledge_search,
    ))
