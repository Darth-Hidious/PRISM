"""Session context builder — KAG-style running knowledge that survives compaction.

Problem: When the Rust agent loop compacts history at turn 20, structured
data (discovered compositions, property values, tool sequences) is lost.
The agent gets a text summary but can't query specific results.

Solution: Maintain a running structured knowledge base in Python that
the agent builds incrementally. This survives compaction because it's
stored as a local artifact, not in the chat history.

The agent calls `session_context` to:
1. RECORD: After each tool call, record what was learned
2. RECALL: Before making decisions, query the running context
3. COMPACT: Summarize the context into a compact block for the LLM

## KAG alignment (arXiv:2409.13731)

This implements the KAG "LLM-friendly knowledge representation" and
"knowledge alignment" layers:
- Data: raw tool outputs (formulas, energies, counts)
- Information: organized by element system / objective
- Knowledge: relationships (which compositions are Pareto-optimal)
- Wisdom: recommendations (which compositions to evaluate next)

## Idle-time usage

The agent should call `session_context(action='compact')` during idle
moments — e.g., while waiting for a long tool to finish, or between
discovery rounds. This keeps the running context fresh without waiting
for the Rust-side compaction cliff.
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path
from typing import Any

from app.tools.base import Tool, ToolRegistry


# ── Session state (persisted to disk) ────────────────────────────────

SESSION_DIR = Path.home() / ".prism" / "sessions"
_CURRENT_SESSION: dict | None = None


def _session_path(session_id: str | None = None) -> Path:
    """Path for the session context file."""
    sid = session_id or os.environ.get("PRISM_SESSION_ID", "default")
    return SESSION_DIR / f"{sid}.json"


def _load_session() -> dict:
    """Load the current session context from disk (cached in memory)."""
    global _CURRENT_SESSION
    if _CURRENT_SESSION is not None:
        return _CURRENT_SESSION

    path = _session_path()
    if path.exists():
        try:
            _CURRENT_SESSION = json.loads(path.read_text())
        except Exception:
            _CURRENT_SESSION = _fresh_session()
    else:
        _CURRENT_SESSION = _fresh_session()
    return _CURRENT_SESSION


def _save_session(session: dict) -> None:
    """Persist session context to disk."""
    global _CURRENT_SESSION
    _CURRENT_SESSION = session
    path = _session_path()
    SESSION_DIR.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(session, indent=2, default=str))


def _fresh_session() -> dict:
    """Create a new empty session context."""
    return {
        "session_id": os.environ.get("PRISM_SESSION_ID", "default"),
        "created_at": time.time(),
        "last_updated": time.time(),
        # DIKW hierarchy (KAG alignment)
        "data": {
            "compositions_evaluated": [],     # list of {formula, properties, source}
            "materials_searched": [],          # list of {query, results_count, source}
            "discoveries": [],                 # list of {campaign_id, pareto_set}
            "tool_calls": [],                  # list of {tool, args_hash, success, elapsed}
        },
        "information": {
            "element_systems": {},             # {elements_key: {explored, best_results}}
            "objectives_seen": set(),          # set of objective names
            "verifiers_used": set(),           # set of verifier names
        },
        "knowledge": {
            "pareto_fronts": {},               # {campaign_id: [compositions]}
            "best_per_objective": {},          # {objective_name: {value, formula}}
            "tool_sequences": [],              # successful tool sequences
            "literature_refs": [],             # reference values for comparison
        },
        "wisdom": {
            "recommendations": [],             # what to do next
            "open_questions": [],              # unanswered questions
            "constraints": [],                 # discovered constraints
        },
        # Running summary for LLM injection (compact form)
        "running_summary": "",
        "summary_token_estimate": 0,
    }


# ── Recording actions ────────────────────────────────────────────────


def _record_evaluation(formula: str, result: dict, tool: str) -> None:
    """Record a composition evaluation (alpha_predict, gfn_evaluate)."""
    session = _load_session()

    entry = {
        "formula": formula,
        "properties": {},
        "tool": tool,
        "timestamp": time.time(),
    }

    # Extract properties from result
    verifiers = result.get("verifiers", {})
    for vname, vdata in verifiers.items():
        if isinstance(vdata, dict):
            for k, v in vdata.items():
                if isinstance(v, (int, float)) and k not in ("n_atoms",):
                    entry["properties"][f"{vname}.{k}"] = v
                    session["information"]["objectives_seen"].add(k)
            session["information"]["verifiers_used"].add(vname)

    # Check for best values
    for prop, value in entry["properties"].items():
        obj_key = prop.split(".")[-1]
        current_best = session["knowledge"]["best_per_objective"].get(obj_key)
        if current_best is None or value > current_best.get("value", float("-inf")):
            session["knowledge"]["best_per_objective"][obj_key] = {
                "value": value, "formula": formula, "property": prop,
            }

    session["data"]["compositions_evaluated"].append(entry)

    # Track element system
    elements = _extract_elements(formula)
    if elements:
        key = "-".join(sorted(elements))
        sys_entry = session["information"]["element_systems"].setdefault(key, {
            "explored": True,
            "n_evaluated": 0,
            "best_delta": None,
            "best_entropy": None,
        })
        sys_entry["n_evaluated"] += 1
        for prop, value in entry["properties"].items():
            if "delta" in prop:
                if sys_entry["best_delta"] is None or value < sys_entry["best_delta"]:
                    sys_entry["best_delta"] = value
            if "entropy" in prop:
                if sys_entry["best_entropy"] is None or value > sys_entry["best_entropy"]:
                    sys_entry["best_entropy"] = value

    session["last_updated"] = time.time()
    _save_session(session)


def _record_discovery(result: dict) -> None:
    """Record a discovery campaign result (alpha_discover, alloy_discover)."""
    session = _load_session()

    pareto = result.get("pareto_set", [])
    campaign_id = f"campaign_{len(session['data']['discoveries'])}"

    discovery = {
        "campaign_id": campaign_id,
        "elements": result.get("elements", []),
        "objectives": result.get("objectives", []),
        "n_pareto": len(pareto),
        "n_evaluated": result.get("n_total_evaluated", 0),
        "pareto_set": [{"formula": p.get("formula", ""),
                        "objectives": p.get("objectives", {})}
                       for p in pareto[:20]],
        "timestamp": time.time(),
    }

    session["data"]["discoveries"].append(discovery)
    session["knowledge"]["pareto_fronts"][campaign_id] = discovery["pareto_set"]

    # Record each Pareto composition as evaluated
    for p in pareto:
        formula = p.get("formula", "")
        if formula:
            _record_evaluation(formula, p, "alpha_discover")

    session["last_updated"] = time.time()
    _save_session(session)


def _record_tool_call(tool: str, args: dict, result: dict, elapsed: float) -> None:
    """Record a tool call for pattern learning."""
    session = _load_session()

    success = "error" not in result if isinstance(result, dict) else True

    session["data"]["tool_calls"].append({
        "tool": tool,
        "args_keys": list(args.keys()),
        "success": success,
        "elapsed_s": round(elapsed, 2),
        "timestamp": time.time(),
    })

    # Keep only last 100 tool calls to bound memory
    if len(session["data"]["tool_calls"]) > 100:
        session["data"]["tool_calls"] = session["data"]["tool_calls"][-100:]

    session["last_updated"] = time.time()
    _save_session(session)


def _extract_elements(formula: str) -> list[str]:
    """Extract element symbols from a composition formula."""
    import re
    return list(set(re.findall(r'[A-Z][a-z]?', formula)))


# ── Compaction / summarization ───────────────────────────────────────


def _compact_context() -> str:
    """Build a compact, LLM-friendly summary of the current session.

    This is the 'wisdom' layer — distilled knowledge the LLM can
    inject into its reasoning. Designed to be <500 tokens so it
    can fit in the system prompt or be appended to messages.
    """
    session = _load_session()
    parts = []

    # Session overview
    data = session["data"]
    parts.append(
        f"Session knowledge: {len(data['compositions_evaluated'])} compositions "
        f"evaluated, {len(data['discoveries'])} discovery campaigns, "
        f"{len(data['tool_calls'])} tool calls."
    )

    # Element systems explored
    systems = session["information"]["element_systems"]
    if systems:
        parts.append("Element systems explored:")
        for sys_key, sys_data in sorted(systems.items()):
            delta_str = f"δ={sys_data['best_delta']:.2f}%" if sys_data.get("best_delta") else ""
            entropy_str = f"S={sys_data['best_entropy']:.1f}" if sys_data.get("best_entropy") else ""
            parts.append(f"  {sys_key} ({sys_data['n_evaluated']} evals): {delta_str} {entropy_str}")

    # Best results per objective
    best = session["knowledge"]["best_per_objective"]
    if best:
        parts.append("Best results found:")
        for obj, data in sorted(best.items()):
            parts.append(f"  {obj}: {data['value']:.4f} ({data['formula']})")

    # Pareto fronts
    fronts = session["knowledge"]["pareto_fronts"]
    if fronts:
        parts.append(f"Pareto fronts: {len(fronts)} campaigns")
        for cid, front in list(fronts.items())[-3:]:  # last 3
            if front:
                formulas = [f["formula"][:30] for f in front[:5]]
                parts.append(f"  {cid}: {', '.join(formulas)}...")

    # Verifiers used
    verifiers = session["information"]["verifiers_used"]
    if verifiers:
        parts.append(f"Models used: {', '.join(sorted(verifiers))}")

    # Recommendations
    recs = session["wisdom"]["recommendations"]
    if recs:
        parts.append("Recommendations:")
        for rec in recs[-3:]:  # last 3
            parts.append(f"  - {rec}")

    summary = "\n".join(parts)

    # Estimate tokens (~4 chars per token)
    session["running_summary"] = summary
    session["summary_token_estimate"] = len(summary) // 4
    _save_session(session)

    return summary


# ── Tool function ────────────────────────────────────────────────────


def _session_context(**kwargs) -> dict:
    """Session context builder — KAG-style running knowledge.

    Actions:
    - record: Record a tool result into the session knowledge base
    - compact: Build a compact summary for LLM injection
    - query: Query the session knowledge base
    - reset: Clear the session (start fresh)
    - status: Show session statistics
    """
    action = kwargs.get("action", "status")
    session = _load_session()

    if action == "record":
        tool = kwargs.get("tool", "")
        result_str = kwargs.get("result", "{}")
        args_str = kwargs.get("args", "{}")
        elapsed = kwargs.get("elapsed_s", 0.0)

        try:
            result = json.loads(result_str) if isinstance(result_str, str) else result_str
            args = json.loads(args_str) if isinstance(args_str, str) else args_str
        except (json.JSONDecodeError, TypeError):
            result = {"raw": result_str}
            args = {}

        formula = args.get("formula", "")

        if tool in ("alpha_predict", "gfn_evaluate") and formula:
            _record_evaluation(formula, result, tool)
        elif tool in ("alpha_discover", "alloy_discover"):
            _record_discovery(result)

        _record_tool_call(tool, args, result, elapsed)

        return {"status": "recorded", "tool": tool, "n_total_evaluated":
                len(session["data"]["compositions_evaluated"])}

    elif action == "compact":
        summary = _compact_context()
        return {
            "summary": summary,
            "token_estimate": len(summary) // 4,
            "n_compositions": len(session["data"]["compositions_evaluated"]),
            "n_discoveries": len(session["data"]["discoveries"]),
        }

    elif action == "query":
        key = kwargs.get("key", "")
        if key == "compositions":
            return {"compositions": session["data"]["compositions_evaluated"][-20:]}
        elif key == "best":
            return {"best_per_objective": session["knowledge"]["best_per_objective"]}
        elif key == "element_systems":
            return {"element_systems": session["information"]["element_systems"]}
        elif key == "pareto_fronts":
            return {"pareto_fronts": session["knowledge"]["pareto_fronts"]}
        elif key == "tool_calls":
            return {"tool_calls": session["data"]["tool_calls"][-20:]}
        else:
            return {
                "available_keys": [
                    "compositions", "best", "element_systems",
                    "pareto_fronts", "tool_calls",
                ],
                "hint": "Specify key=one_of_the_above to query specific data.",
            }

    elif action == "reset":
        global _CURRENT_SESSION
        _CURRENT_SESSION = _fresh_session()
        _save_session(_CURRENT_SESSION)
        return {"status": "reset", "message": "Session context cleared."}

    elif action == "status":
        return {
            "session_id": session["session_id"],
            "n_compositions": len(session["data"]["compositions_evaluated"]),
            "n_discoveries": len(session["data"]["discoveries"]),
            "n_tool_calls": len(session["data"]["tool_calls"]),
            "element_systems": list(session["information"]["element_systems"].keys()),
            "verifiers_used": list(session["information"]["verifiers_used"]),
            "objectives_seen": list(session["information"]["objectives_seen"]),
            "running_summary_tokens": session.get("summary_token_estimate", 0),
        }

    else:
        return {"error": f"Unknown action: {action}. Use: record, compact, query, reset, status."}


# ── Tool description ─────────────────────────────────────────────────

_SESSION_CONTEXT_DESCRIPTION = (
    "Session context builder — maintains a running structured knowledge "
    "base that survives chat history compaction. Records compositions "
    "evaluated, discovery results, best values per objective, and tool "
    "call patterns. Builds compact summaries the agent can inject into "
    "its reasoning.\n\n"
    "Call this tool:\n"
    "  - After alpha_predict/alpha_discover results: action='record'\n"
    "  - Between major steps: action='compact' to refresh the summary\n"
    "  - Before decisions: action='query' to recall prior results\n"
    "  - At session start: action='status' to see accumulated knowledge\n\n"
    "The context persists to disk (~/.prism/sessions/) so it survives "
    "history compaction. This is the KAG knowledge representation layer — "
    "structured Data → Information → Knowledge → Wisdom that doesn't "
    "get lost when the chat history is summarized.\n\n"
    "Actions:\n"
    "  • record: store a tool result (args: tool, result, args, elapsed_s)\n"
    "  • compact: build compact summary for LLM injection\n"
    "  • query: retrieve specific data (key: compositions|best|element_systems|pareto_fronts|tool_calls)\n"
    "  • status: show session statistics\n"
    "  • reset: clear session context"
)


def create_session_context_tool(registry: ToolRegistry) -> None:
    """Register the session context builder tool."""
    registry.register(Tool(
        name="session_context",
        description=_SESSION_CONTEXT_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["record", "compact", "query", "status", "reset"],
                    "description": "What to do with the session context.",
                    "default": "status",
                },
                "tool": {
                    "type": "string",
                    "description": "Tool name (for action='record').",
                },
                "result": {
                    "description": "Tool result JSON (for action='record').",
                },
                "args": {
                    "description": "Tool args JSON (for action='record').",
                },
                "key": {
                    "type": "string",
                    "description": "Data key to query (for action='query').",
                },
                "elapsed_s": {
                    "type": "number",
                    "description": "Tool execution time (for action='record').",
                },
            },
            "required": [],
            "additionalProperties": True,
        },
        func=_session_context,
        requires_approval=False,
        source="builtin",
        source_detail="KAG session context builder (arXiv:2409.13731)",
    ))