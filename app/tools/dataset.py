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


def _act_import(**kwargs) -> dict:
    """Round 7 addition: load a CSV/JSON/Parquet into the DataStore."""
    from app.tools.data import _import_dataset
    return _import_dataset(**kwargs)


def _act_export(**kwargs) -> dict:
    """Round 7 addition: write a list of result dicts to a CSV file."""
    from app.tools.data import _export_results_csv
    return _export_results_csv(**kwargs)


_DISPATCH = {
    "validate":  _act_validate,
    "review":    _act_review,
    "visualize": _act_visualize,
    "import":    _act_import,
    "export":    _act_export,
}


# Actions that REQUIRE a `dataset_name` argument (the analysis ops).
# import + export use different keys (file_path / results), so they're
# excluded from this gate and validate themselves through the underlying
# impls.
_REQUIRES_DATASET_NAME = {"validate", "review", "visualize"}


def _dataset(**kwargs) -> dict:
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_DISPATCH.keys())}",
            "hint": (
                "dataset(action='validate', dataset_name='...') / "
                "dataset(action='review', dataset_name='...') / "
                "dataset(action='visualize', dataset_name='...') / "
                "dataset(action='import', file_path='./alloys.csv') / "
                "dataset(action='export', results=[{...}, ...])"
            ),
        }
    handler = _DISPATCH.get(action)
    if not handler:
        return {"error": f"Unknown action '{action}'. Valid: {list(_DISPATCH.keys())}"}
    if action in _REQUIRES_DATASET_NAME and not kwargs.get("dataset_name"):
        return {"error": f"Action '{action}' requires `dataset_name`"}
    if action == "import" and not kwargs.get("file_path"):
        return {"error": "Action 'import' requires `file_path`"}
    if action == "export" and "results" not in kwargs:
        return {"error": "Action 'export' requires `results` (list of dicts)"}
    try:
        return handler(**kwargs)
    except Exception as e:
        return {"error": str(e), "action": action}


_DESCRIPTION = (
    "Dataset I/O + analysis. ONE tool, five actions:\n"
    "\n"
    "I/O actions:\n"
    "  • action='import' — load a local CSV / JSON / Parquet file into the "
    "PRISM DataStore. Requires `file_path`. Optional: `dataset_name` "
    "(default: file stem), `file_format` (default: auto-detect from "
    "extension). Returns the resolved dataset_name + row/column counts.\n"
    "  • action='export' — write a list of result dictionaries to a CSV "
    "file. Requires `results` (list of dicts). Optional: `filename` "
    "(default: auto-generated). Use after gathering data to save results.\n"
    "\n"
    "Analysis actions (require `dataset_name` — use action='import' first "
    "if your data lives in a file):\n"
    "  • action='validate' — quality audit. Three checks combined: "
    "(a) Z-score outlier flagging per column (`z_threshold` default 3.0 — "
    "tighten to 2.0 for curated data, loosen to 4.0+ for wide-scan); "
    "(b) physical-constraint checks (negative band gaps, density "
    "out-of-range, stoichiometric impossibilities); (c) per-column "
    "completeness scoring. Returns a structured report + human-readable "
    "summary. Use before training a model or making a decision off the data.\n"
    "  • action='review' — peer-review-style assessment. Looks for "
    "structural issues, missing metadata, statistical anomalies. Heavier "
    "than 'validate'; use when the user asks 'is this dataset publishable "
    "/ ready for analysis?'.\n"
    "  • action='visualize' — generate visual summaries. See the "
    "visualization skill for supported chart kinds. Use when the user "
    "wants 'show me this dataset's distribution / relationships'.\n"
    "\n"
    "NOT for ad-hoc plots of arbitrary data (use `plot` for that) and "
    "NOT for searching materials databases (use `materials_search`)."
)


_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_DISPATCH.keys()),
            "description": "Which dataset operation to perform.",
        },
        # Analysis actions
        "dataset_name": {
            "type": "string",
            "description": (
                "Name of the dataset registered in the PRISM DataStore. "
                "Required for action='validate'/'review'/'visualize'. "
                "For action='import', this is the optional name to "
                "register the file under (defaults to file stem)."
            ),
        },
        "z_threshold": {
            "type": "number",
            "description": (
                "Z-score threshold for outlier flagging in action='validate'. "
                "Default 3.0. Tighten to 2.0 for stricter review of curated "
                "datasets; loosen to 4.0+ for wide-scan exploration data."
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
        # Import action
        "file_path": {
            "type": "string",
            "description": (
                "Source file path for action='import'. Absolute or "
                "working-directory-relative. Glob patterns NOT supported "
                "— one file per call."
            ),
        },
        "file_format": {
            "type": "string",
            "enum": ["csv", "json", "parquet"],
            "description": (
                "Format override for action='import'. Pass when the "
                "extension is wrong or absent. Default: auto-detect."
            ),
        },
        # Export action
        "results": {
            "type": "array",
            "items": {"type": "object"},
            "description": (
                "List of result dictionaries to export for action='export'. "
                "All dicts should share the same key shape; missing keys "
                "become empty cells."
            ),
        },
        "filename": {
            "type": "string",
            "description": (
                "Output CSV path for action='export'. Auto-generated under "
                "the working directory if omitted."
            ),
        },
    },
    "required": ["action"],
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
