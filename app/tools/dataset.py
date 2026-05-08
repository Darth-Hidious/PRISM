"""Dataset tool — unified validate / review / visualize dispatcher.

Consolidates three previously-separate Skills into one Tool:

  validate_dataset   → dataset(action='validate', ...)
  review_dataset     → dataset(action='review', ...)
  visualize_dataset  → dataset(action='visualize', ...)

WHY ABOVE-SKILL ABSTRACTION

The originals were `Skill` objects (procedural workflows with named
steps). The Skill abstraction is internal: the LLM agent only sees the
generated Tool. Per-Skill `steps` metadata didn't reach the agent's
view of the tool surface; it was used by some internal logging /
audit paths only. Collapsing into a Tool drops that internal metadata
but doesn't change the agent-visible behavior.

Dataset operations have a coherent shape — they all operate on a
DataStore-registered dataset by name and return a structured report.
The unified tool gives Stage 2.1 retrieval a single rich-description
target instead of three near-duplicate ones.

KEEPING SKILL FILES INTACT

The validate.py / review.py / visualize.py files remain — they hold
the actual implementations. We just delete the Skill registration
side and route through this dataset Tool. If the harness gains true
Skill-aware UX (showing per-step progress), we can re-introduce
Skill registration without changing the implementations.
"""
from app.tools.base import Tool, ToolRegistry


# Lazy imports keep the Tool module light at import time
def _act_validate(**kwargs) -> dict:
    from app.tools.skills.validation import _validate_dataset
    return _validate_dataset(**kwargs)


def _act_review(**kwargs) -> dict:
    from app.tools.skills.review import _review_dataset
    return _review_dataset(**kwargs)


def _act_visualize(**kwargs) -> dict:
    from app.tools.skills.visualization import _visualize_dataset
    return _visualize_dataset(**kwargs)


_DISPATCH = {
    "validate":  _act_validate,
    "review":    _act_review,
    "visualize": _act_visualize,
}


def _dataset(**kwargs) -> dict:
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_DISPATCH.keys())}",
            "hint": (
                "dataset(action='validate', dataset_name='...') / "
                "dataset(action='review', dataset_name='...') / "
                "dataset(action='visualize', dataset_name='...', kind='...')"
            ),
        }
    handler = _DISPATCH.get(action)
    if not handler:
        return {"error": f"Unknown action '{action}'. Valid: {list(_DISPATCH.keys())}"}
    if not kwargs.get("dataset_name"):
        return {"error": f"Action '{action}' requires `dataset_name`"}
    try:
        return handler(**kwargs)
    except Exception as e:
        return {"error": str(e), "action": action}


_DESCRIPTION = (
    "Operate on a stored dataset (in the PRISM DataStore — use "
    "`import_dataset` first if your data lives in a CSV). ONE tool, "
    "three actions:\n"
    "  • action='validate' — quality audit. Three checks combined: "
    "(a) Z-score outlier flagging per column (threshold 3.0 default — "
    "tighten to 2.0 for curated data, loosen to 4.0+ for wide-scan); "
    "(b) physical-constraint checks (negative band gaps, density-out-"
    "of-range, stoichiometric impossibilities); (c) per-column "
    "completeness scoring. Returns a structured report + a human-"
    "readable summary. Use before training a model or making a decision "
    "off the data.\n"
    "  • action='review' — peer-review-style assessment. Looks for "
    "structural issues, missing metadata, statistical anomalies. "
    "Heavier than 'validate'; use when the user asks 'is this dataset "
    "publishable / ready for analysis?'.\n"
    "  • action='visualize' — generate visual summaries. See the "
    "underlying skill for supported chart types. Use when the user "
    "wants 'show me this dataset's distribution / relationships'.\n"
    "All three require `dataset_name`. NOT for ad-hoc plots of arbitrary "
    "data (use `plot` for that) and NOT for searching materials databases "
    "(use materials_search)."
)


_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which dataset operation to perform.",
        },
        "dataset_name": {
            "type": "string",
            "description": (
                "Name of the dataset registered in the PRISM DataStore. "
                "Use `import_dataset` first if it lives in a CSV. Required "
                "for all actions."
            ),
        },
        "z_threshold": {
            "type": "number",
            "description": (
                "Z-score threshold for outlier flagging in action='validate'. "
                "Default 3.0 (≈ outside 99.7%). Tighten to 2.0 for stricter "
                "review of curated datasets; loosen to 4.0+ for wide-scan "
                "exploration data."
            ),
            "default": 3.0,
        },
        "kind": {
            "type": "string",
            "description": (
                "Visualization kind for action='visualize'. See the "
                "visualization skill for supported values."
            ),
        },
    },
    "required": ["action", "dataset_name"],
    "additionalProperties": False,
}


def create_dataset_tool(registry: ToolRegistry) -> None:
    """Register the unified `dataset` tool.

    Replaces the previously-separate VALIDATE_SKILL / REVIEW_SKILL /
    VISUALIZE_SKILL registrations. Those Skill objects remain in the
    skills/ directory for future Skill-aware harness UX, but they
    no longer register themselves as Tools.
    """
    registry.register(Tool(
        name="dataset",
        description=_DESCRIPTION,
        input_schema=_SCHEMA,
        func=_dataset,
    ))
