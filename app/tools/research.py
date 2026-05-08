"""Research tool — wraps the MARC27 RLM research engine.

POST /api/v1/knowledge/research/query is a server-side Recursive Language
Model implementation (arxiv:2512.24601v2). The server's research engine
runs its OWN LLM in a Python REPL, recursively decomposing the question,
querying graph + vector + web sources, and streaming SSE events back.

Two LLMs at different abstraction levels:

    User → [chat LLM] decides "I'll call research(...)"
                 ↓
                 HTTP → [server LLM] runs the recursive REPL loop
                          ↓
                 SSE ← progress events + final answer
                 ↓
            [chat LLM] receives one structured result and continues

The chat LLM never recurses; the server-side LLM does. This tool's
job is to (a) submit the question, (b) consume the SSE stream, (c)
return a clean structured result, and (d) cost-cap the request via
the harness approval gate (it's a money-spending action).

Server-side persistence: every completed session is written to the
`research_sessions` Postgres table with full provenance (steps, tenant,
cost, metrics) AND the answer is auto-embedded into the vector store
under doc_id `research-session-{uuid}`. So future agents in any session
can find this answer via `knowledge(action='semantic')` or `recall()`.

See docs/stateful_tools_2026.md for the broader stateful architecture.
"""
from __future__ import annotations

import json
import logging
import os
from typing import Any, Optional

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# SSE event handling
# ---------------------------------------------------------------------------

# Events emitted by the server's research engine. We surface a curated
# subset to the user via the progress log; the rest are silent.
_USER_VISIBLE_STEPS: frozenset[str] = frozenset({
    "started",
    "decompose",
    "sub_session_start",
    "sub_session_complete",
    "answer",
    "complete",
    "error",
})


def _format_progress_line(step: str, data: dict) -> str:
    """One-liner suitable for streaming back to the agent's scratchpad."""
    if step == "started":
        sid = data.get("session_id", "?")
        q = data.get("question", "")
        return f"[research started] session={sid[:12]} question={q[:80]}"
    if step == "decompose":
        return f"[research decompose] {data.get('reason', '')[:120]}"
    if step == "sub_session_start":
        sub_q = data.get("sub_query", data.get("question", ""))
        return f"[research sub-session] {sub_q[:120]}"
    if step == "sub_session_complete":
        return f"[research sub-session done] cost=${data.get('cost_usd', 0):.4f}"
    if step == "answer":
        text = data.get("text", "")
        turns = data.get("turns", "?")
        return f"[research answer ready] {turns} turns, {len(text)} chars"
    if step == "complete":
        return f"[research complete] session={data.get('session_id', '?')[:12]}"
    if step == "error":
        return f"[research error] {data.get('message', '')[:200]}"
    return f"[research {step}]"


def _parse_sse_line(line: str) -> Optional[dict]:
    """Parse one `data: {...}` line. Returns None for keepalives / empty."""
    line = line.strip()
    if not line or not line.startswith("data:"):
        return None
    payload = line[len("data:"):].strip()
    if not payload or payload == "[DONE]":
        return None
    try:
        return json.loads(payload)
    except json.JSONDecodeError:
        return None


# ---------------------------------------------------------------------------
# Implementation
# ---------------------------------------------------------------------------

def _research(**kwargs) -> dict:
    """Run a research-mode query against the MARC27 RLM engine.

    Args:
        question (str, required): Natural-language research question.
        depth (int, optional, default=1): Recursion depth.
            0 = local sources only, 1+ = web search + sub-session decomposition.
            The server caps at MAX_RESEARCH_DEPTH (default 10).
        timeout_seconds (int, optional, default=600): SSE stream timeout.
            Long research sessions can take 5–10 minutes legitimately.

    Returns:
        Either a result dict with the structure documented below, or an
        error dict with `error` key. Storage failure (ingest log unable
        to persist on the server) does not block the answer return.

        Result shape:
          {
            "session_id": "<uuid>",
            "answer": "<full text>",
            "cost_usd": <float>,
            "turns": <int>,
            "metrics": {
                "graph_queries": <int>,
                "vector_queries": <int>,
                "web_searches": <int>,
                "papers_fetched": <int>,
                "papers_ingested": <int>,
                "entities_created": <int>,
                "embeddings_created": <int>,
                "llm_calls": <int>,
            },
            "progress_log": ["...", "...", ...],   # high-level steps only
            "source": "marc27_research_rlm",
          }
    """
    question = kwargs.get("question")
    if not question or not question.strip():
        return {"error": "`question` is required"}

    depth = int(kwargs.get("depth", 1))
    if depth < 0:
        return {"error": "`depth` must be >= 0"}
    if depth > 10:
        # Server enforces this anyway; rejecting client-side gives a clearer error
        return {"error": "`depth` cannot exceed 10 (server max)"}

    timeout_seconds = int(kwargs.get("timeout_seconds", 600))

    # ---------------------- HTTP call setup ----------------------
    api_url, api_key = _resolve_credentials()
    if not api_key:
        return {
            "error": (
                "MARC27 platform not connected. Run `prism login` or set "
                "MARC27_API_KEY before calling research()."
            )
        }

    try:
        import requests
    except ImportError:
        return {"error": "requests library not available"}

    url = f"{api_url}/knowledge/research/query"
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Accept": "text/event-stream",
        "Content-Type": "application/json",
    }
    body = {"question": question, "depth": depth}

    progress: list[str] = []
    session_id: Optional[str] = None
    answer: Optional[str] = None
    turns: Optional[int] = None
    final_metrics: Optional[dict] = None
    error_message: Optional[str] = None

    try:
        # stream=True keeps the connection open for SSE
        resp = requests.post(
            url,
            headers=headers,
            json=body,
            stream=True,
            timeout=(30, timeout_seconds),  # (connect, read)
        )
    except requests.exceptions.RequestException as e:
        return {"error": f"failed to reach research endpoint: {e}"}

    if resp.status_code != 200:
        try:
            err_body = resp.text[:500]
        except Exception:
            err_body = "<unreadable>"
        return {
            "error": (
                f"research endpoint returned HTTP {resp.status_code}: {err_body}"
            )
        }

    # ---------------------- Consume the SSE stream ----------------------
    try:
        for raw_line in resp.iter_lines(decode_unicode=True):
            if raw_line is None:
                continue
            event = _parse_sse_line(raw_line)
            if event is None:
                continue

            step = event.get("step")
            data = event.get("data") or {}

            if step in _USER_VISIBLE_STEPS:
                progress.append(_format_progress_line(step, data))

            if step == "started":
                session_id = data.get("session_id")
            elif step == "answer":
                answer = data.get("text")
                turns = data.get("turns")
            elif step == "complete":
                # Final metrics summary
                if "metrics" in data:
                    final_metrics = data["metrics"]
                if not session_id and "session_id" in data:
                    session_id = data["session_id"]
                # The server has finished; no more events expected
                break
            elif step == "error":
                error_message = data.get("message", "research session failed")
                break
    except requests.exceptions.RequestException as e:
        return {
            "error": f"SSE stream interrupted: {e}",
            "session_id": session_id,
            "partial_progress": progress,
        }
    finally:
        try:
            resp.close()
        except Exception:
            pass

    if error_message:
        return {
            "error": error_message,
            "session_id": session_id,
            "progress_log": progress,
        }

    if answer is None:
        return {
            "error": "research session ended without producing an answer",
            "session_id": session_id,
            "progress_log": progress,
        }

    cost_usd = 0.0
    if final_metrics and "cost_usd" in final_metrics:
        cost_usd = float(final_metrics["cost_usd"])
    elif "cost_usd" in (final_metrics or {}):
        cost_usd = float(final_metrics["cost_usd"])

    return {
        "session_id": session_id,
        "answer": answer,
        "cost_usd": cost_usd,
        "turns": turns,
        "metrics": final_metrics or {},
        "progress_log": progress,
        "source": "marc27_research_rlm",
    }


def _resolve_credentials() -> tuple[str, str]:
    """Return (api_url, api_key). Reads MARC27_API_KEY env or ~/.prism/credentials.json."""
    api_url = os.environ.get(
        "MARC27_API_URL", "https://api.marc27.com/api/v1"
    ).rstrip("/")
    api_key = os.environ.get("MARC27_API_KEY", "")

    if not api_key:
        # Try ~/.prism/credentials.json (the prism login output)
        try:
            from pathlib import Path
            creds_path = Path.home() / ".prism" / "credentials.json"
            if creds_path.exists():
                creds = json.loads(creds_path.read_text())
                api_key = creds.get("access_token", "")
                # Override URL if credentials say so
                if creds.get("platform_url"):
                    api_url = creds["platform_url"].rstrip("/")
                    # platform_url might be the bare host; ensure /api/v1 suffix
                    if not api_url.endswith("/api/v1"):
                        api_url = api_url + "/api/v1"
        except Exception:
            pass

    return api_url, api_key


# ---------------------------------------------------------------------------
# Tool description + schema
# ---------------------------------------------------------------------------

_DESCRIPTION = (
    "Run a deep research query against the MARC27 RLM (Recursive Language "
    "Model) engine. ONE call dispatches a server-side specialist LLM that "
    "recursively explores the knowledge graph + 6K+ embedded documents + "
    "academic web (Semantic Scholar / arXiv / PubMed / OpenAlex), writing "
    "Python code in a sandboxed REPL to slice, filter, and summarize "
    "evidence. Returns a structured answer with provenance.\n"
    "\n"
    "WHEN TO USE THIS over knowledge(action='search') or web_search:\n"
    "  • Question is OPEN-ENDED and requires synthesis ('compare creep "
    "performance of nickel superalloys for turbine blades').\n"
    "  • Answer requires combining literature + structured data + reasoning.\n"
    "  • The user wants a written report, not a list of hits.\n"
    "\n"
    "When NOT to use it:\n"
    "  • Single-fact lookup → use knowledge(action='entity') or web_search.\n"
    "  • Structure search by formula/elements → use materials_search.\n"
    "  • Already in the agent's recent context → use recall().\n"
    "\n"
    "COST: REAL-MONEY ACTION. Each call runs a server LLM for several "
    "turns; cost typically ~$0.01–$2 depending on `depth`. Results stream "
    "back as SSE; the call blocks until completion (5 sec to 10 min).\n"
    "\n"
    "Result is auto-persisted server-side (research_sessions Postgres "
    "row + answer auto-embedded) and locally as an artifact, so future "
    "agents can recall it without re-running."
)

_SCHEMA = {
    "type": "object",
    "properties": {
        "question": {
            "type": "string",
            "description": (
                "Natural-language research question. Be specific — the "
                "RLM produces better answers from concrete questions "
                "('what alloy systems show solid-solution strengthening "
                "above 1000°C?') than vague ones ('alloys?')."
            ),
        },
        "depth": {
            "type": "integer",
            "default": 1,
            "minimum": 0,
            "maximum": 10,
            "description": (
                "Recursion depth budget. 0 = local KB only (cheap, fast). "
                "1 (default) = local + web search. 2+ = sub-session "
                "decomposition for hard multi-part questions. Each "
                "additional level multiplies cost ~3-5x."
            ),
        },
        "timeout_seconds": {
            "type": "integer",
            "default": 600,
            "minimum": 30,
            "maximum": 1800,
            "description": (
                "How long to wait for the SSE stream before giving up. "
                "Default 10 minutes. Deep sessions (depth 2+) may need "
                "the full 30-minute cap."
            ),
        },
    },
    "required": ["question"],
    "additionalProperties": False,
}


def create_research_tools(registry: ToolRegistry) -> None:
    """Register the `research` tool.

    `requires_approval=True` because this is a real-money action — every
    call spends server LLM budget. The harness must show the user
    "agent wants to run research(question='…', depth=2). Approve?"
    before each invocation.
    """
    registry.register(Tool(
        name="research",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_research,
        requires_approval=True,
    ))
