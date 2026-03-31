"""Shared tool metadata, result classification, and Rich rendering helpers.

Extracted from app/cli/tui/ so the backend and autonomous mode can use
these without depending on the full Rich REPL stack.
"""

import hashlib
import json
import os

# Rich imports deferred to functions that use them (saves ~45ms on backend startup).
# The constants (TOOL_VERBS, BORDERS, ICONS, detect_result_type) don't need Rich.


# ── Theme constants (used across backend + CLI) ────────────────────────

PRIMARY = "#fab283"
SECONDARY = "#5c9cf5"
ACCENT_MAGENTA = "#bb86fc"
ACCENT_CYAN = "#56b6c2"

SUCCESS = "#7fd88f"
WARNING = "#e5c07b"
ERROR = "#e06c75"
INFO = "#61afef"
ACCENT = f"bold {PRIMARY}"
DIM = "dim"
TEXT = "#e0e0e0"
MUTED = "#808080"

BORDERS = {
    "input": ACCENT_CYAN,
    "output": MUTED,
    "tool": SUCCESS,
    "error": ERROR,
    "error_partial": WARNING,
    "metrics": INFO,
    "calphad": SECONDARY,
    "labs": ACCENT_MAGENTA,
    "validation_critical": ERROR,
    "validation_warning": WARNING,
    "validation_info": INFO,
    "results": MUTED,
    "plot": SUCCESS,
    "approval": WARNING,
    "plan": ACCENT_MAGENTA,
}

ICONS = {
    "input": "\u276f",
    "output": "\u25cb",
    "tool": "\u2699",
    "error": "\u2717",
    "success": "\u2714",
    "metrics": "\u25a0",
    "calphad": "\u2206",
    "labs": "\u2726",
    "validation": "\u25cf",
    "results": "\u2261",
    "plot": "\u25a3",
    "approval": "\u26a0",
    "plan": "\u25b7",
    "pending": "\u223c",
}

TRUNCATION_LINES = 6
TRUNCATION_CHARS = 50_000


# ── Tool verbs (used by UIEmitter for spinner text) ─────────────────────

TOOL_VERBS = {
    "search_materials": "Searching materials databases\u2026",
    "query_materials_project": "Querying Materials Project\u2026",
    "predict_property": "Training ML model\u2026",
    "calculate_phase_diagram": "Computing phase diagram\u2026",
    "calculate_equilibrium": "Computing equilibrium\u2026",
    "calculate_gibbs_energy": "Computing Gibbs energy\u2026",
    "literature_search": "Searching literature\u2026",
    "patent_search": "Searching patents\u2026",
    "run_simulation": "Running simulation\u2026",
    "submit_hpc_job": "Submitting HPC job\u2026",
    "validate_dataset": "Validating dataset\u2026",
    "review_dataset": "Reviewing dataset\u2026",
    "export_results_csv": "Exporting results\u2026",
    "import_local_data": "Importing data\u2026",
}


# ── Result classification ────────────────────────────────────────────────

def detect_result_type(result: dict) -> str:
    """Determine the display type from a tool result dict shape."""
    if "error" in result:
        return "error"
    if "metrics" in result and "algorithm" in result:
        return "metrics"
    if "phases_present" in result or ("phases" in result and "gibbs_energy" in result):
        return "calphad"
    if "findings" in result and "quality_score" in result:
        return "validation"
    if isinstance(result.get("filename"), str) and result["filename"].endswith(".png"):
        return "plot"
    if isinstance(result.get("results"), list) and len(result["results"]) > 3:
        return "results"
    return "tool"


# ── Capability detection (used by slash handlers) ────────────────────────

def detect_capabilities() -> dict:
    """Legacy capability detection."""
    from app.backend.status import detect_llm, detect_commands
    llm = detect_llm()
    cmds = detect_commands()
    return {
        "LLM": llm["connected"],
        "search": any(t["healthy"] for t in cmds["tools"] if t["name"] == "search_materials"),
    }


# ── Rich rendering helpers (used by autonomous mode) ─────────────────────

def format_elapsed(ms: float) -> str:
    if ms >= 2000:
        return f"{ms / 1000:.1f}s"
    if ms > 0:
        return f"{ms:.0f}ms"
    return ""


def format_tokens(n: int) -> str:
    if n >= 1000:
        return f"{n / 1000:.1f}k"
    return str(n)


def make_title(card_type: str, tool_name: str = "",
               elapsed_ms: float = 0, label: str = ""):
    from rich.text import Text
    icon = ICONS.get(card_type, "")
    title = Text()
    if icon:
        title.append(f" {icon} ", style=MUTED)
    title.append(f"{card_type} ", style=DIM)
    if tool_name:
        title.append(f"{tool_name} ", style=ACCENT)
    if label:
        title.append(f" {label} ", style=f"bold {WARNING}")
    elapsed = format_elapsed(elapsed_ms)
    if elapsed:
        title.append(f" {elapsed} ", style=MUTED)
    return title


def render_tool_result(console, tool_name: str, summary: str,
                       elapsed_ms: float, result: dict):
    """Dispatch to the right card renderer based on result shape."""
    result_type = detect_result_type(result)
    if result_type == "error":
        _render_error_card(console, tool_name, elapsed_ms, result)
    elif result_type == "metrics":
        _render_metrics_card(console, tool_name, elapsed_ms, result)
    elif result_type == "calphad":
        _render_calphad_card(console, tool_name, elapsed_ms, result)
    elif result_type == "results":
        _render_results_card(console, tool_name, summary, elapsed_ms, result)
    else:
        _render_success_card(console, tool_name, summary, elapsed_ms)
    _check_large_result(console, result)


def render_cost_line(console, usage, turn_cost: float | None = None,
                     session_cost: float = 0.0):
    parts = [
        f"{format_tokens(usage.input_tokens)} in",
        f"{format_tokens(usage.output_tokens)} out",
    ]
    if turn_cost is not None:
        parts.append(f"${turn_cost:.4f}")
        parts.append(f"total: ${session_cost:.4f}")
    line = " \u00b7 ".join(parts)
    console.print(f" [dim]\u2500 {line} \u2500[/dim]")


# ── Spinner (used by autonomous mode) ────────────────────────────────────

class Spinner:
    """Animated spinner using Rich Status."""

    def __init__(self, console):
        self._console = console
        self._status = None

    def verb_for_tool(self, tool_name: str) -> str:
        return TOOL_VERBS.get(tool_name, "Thinking\u2026")

    def start(self, verb: str = "Thinking\u2026"):
        self.stop()
        self._status = self._console.status(
            verb, spinner="dots", spinner_style=f"bold {PRIMARY}",
        )
        self._status.start()

    def update(self, verb: str):
        if self._status is not None:
            self._status.update(verb)

    def stop(self):
        if self._status is not None:
            self._status.stop()
            self._status = None


# ── Private card renderers ───────────────────────────────────────────────

def _rich():
    """Lazy-load Rich modules. Called only when Rich rendering is needed."""
    from rich.text import Text
    from rich.panel import Panel
    from rich.table import Table
    from rich import box
    return Text, Panel, Table, box


def _render_success_card(console, tool_name, summary, elapsed_ms):
    Text, Panel, _, box = _rich()
    title = make_title("tool", tool_name, elapsed_ms)
    body = Text()
    body.append(f" {ICONS['success']} ", style=f"bold {SUCCESS}")
    body.append(summary or "completed", style=DIM)
    console.print(Panel(body, title=title, title_align="left",
                        border_style=BORDERS["tool"], box=box.ROUNDED, padding=(0, 1)))


def _render_error_card(console, tool_name, elapsed_ms, result):
    Text, Panel, _, box = _rich()
    error_msg = str(result.get("error", "Unknown error"))
    title = make_title("error", tool_name, elapsed_ms, "FAILED")
    body = Text()
    body.append(f" {ICONS['error']} ", style=f"bold {ERROR}")
    if len(error_msg) > 200:
        error_msg = error_msg[:200] + "\u2026"
    body.append(error_msg, style=DIM)
    console.print(Panel(body, title=title, title_align="left",
                        border_style=BORDERS["error"], box=box.ROUNDED, padding=(0, 1)))


def _render_metrics_card(console, tool_name, elapsed_ms, result):
    _, Panel, Table, box = _rich()
    title = make_title("metrics", tool_name, elapsed_ms)
    metrics = result.get("metrics", {})
    algorithm = result.get("algorithm", "")
    table = Table(show_header=False, box=None, padding=(0, 1))
    table.add_column(style="bold")
    table.add_column()
    if algorithm:
        table.add_row("Algorithm", algorithm)
    for k, v in metrics.items():
        label = k.upper() if k in ("mae", "rmse", "r2") else k
        val = f"{v:.4f}" if isinstance(v, float) else str(v)
        table.add_row(label, val)
    console.print(Panel(table, title=title, title_align="left",
                        border_style=BORDERS["metrics"], box=box.ROUNDED, padding=(0, 1)))


def _render_calphad_card(console, tool_name, elapsed_ms, result):
    Text, Panel, _, box = _rich()
    title = make_title("calphad", tool_name, elapsed_ms)
    body = Text()
    system = result.get("system", "")
    if system:
        body.append(f"  System: {system}\n", style="bold")
    phases = result.get("phases_present", result.get("phases", {}))
    if isinstance(phases, dict):
        for phase, frac in phases.items():
            if isinstance(frac, (int, float)):
                body.append(f"  {phase}: {frac:.2f}\n")
            else:
                body.append(f"  {phase}: {frac}\n")
    console.print(Panel(body, title=title, title_align="left",
                        border_style=BORDERS["calphad"], box=box.ROUNDED, padding=(0, 1)))


def _render_results_card(console, tool_name, summary, elapsed_ms, result):
    Text, Panel, Table, box = _rich()
    rows = result.get("results", [])
    total = result.get("count", len(rows))
    title = make_title("results", tool_name, elapsed_ms)
    preview = rows[:3]
    if preview:
        cols = list(preview[0].keys())[:4]
        table = Table(box=box.SIMPLE, padding=(0, 1))
        for c in cols:
            table.add_column(c, style=MUTED)
        for row in preview:
            table.add_row(*(str(row.get(c, ""))[:25] for c in cols))
    else:
        table = Text(summary or "No preview", style=MUTED)
    console.print(Panel(table, title=title, title_align="left",
                        border_style=BORDERS["results"], box=box.ROUNDED, padding=(0, 1)))


def _check_large_result(console, result):
    try:
        serialized = json.dumps(result, default=str)
    except (TypeError, ValueError):
        return
    if len(serialized) <= TRUNCATION_CHARS:
        return
    result_id = hashlib.md5(serialized[:1000].encode()).hexdigest()[:8]
    cache_dir = os.path.join(
        os.environ.get("PRISM_CACHE_DIR", os.path.expanduser("~/.prism/cache")),
        "results",
    )
    os.makedirs(cache_dir, exist_ok=True)
    path = os.path.join(cache_dir, f"{result_id}.json")
    try:
        with open(path, "w") as f:
            f.write(serialized)
    except OSError:
        pass
    console.print(
        f" [dim]\u2500 {len(serialized):,} chars truncated \u00b7 "
        f"stored as {result_id} \u00b7 "
        f'use peek_result("{result_id}") \u2500[/dim]'
    )
