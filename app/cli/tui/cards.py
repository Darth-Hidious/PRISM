"""Card renderers for every result type in the PRISM TUI.

Each function takes a Rich Console and renders a bordered panel.
No class state — pure functions.

Visual design inspired by OpenCode's typed output panels.
"""

from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel
from rich.table import Table
from rich.text import Text
from rich import box

from app.cli.tui.theme import (
    PRIMARY, SECONDARY,
    SUCCESS, WARNING, ERROR, INFO,
    ACCENT, ACCENT_CYAN, ACCENT_MAGENTA,
    DIM, MUTED, TEXT,
    BORDERS, ICONS, TRUNCATION_LINES,
)


# ── Helpers ───────────────────────────────────────────────────────────

def format_elapsed(ms: float) -> str:
    if ms >= 2000:
        return f"{ms / 1000:.1f}s"
    if ms > 0:
        return f"{ms:.0f}ms"
    return ""


def make_title(card_type: str, tool_name: str = "",
               elapsed_ms: float = 0, label: str = "") -> Text:
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


# ── Card renderers ────────────────────────────────────────────────────

def render_input_card(console: Console, text: str):
    """Teal-bordered input card with ❯ icon."""
    console.print()
    title = Text()
    title.append(f" {ICONS['input']} ", style=f"bold {ACCENT_CYAN}")
    title.append("input ", style=DIM)
    console.print(Panel(
        Text(text, style=TEXT),
        title=title,
        title_align="left",
        border_style=BORDERS["input"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_output_card(console: Console, text: str):
    """Muted-bordered output card with truncation."""
    lines = text.split("\n")
    if len(lines) > TRUNCATION_LINES:
        truncated = "\n".join(lines[:TRUNCATION_LINES])
        remaining = len(lines) - TRUNCATION_LINES
        content = Markdown(truncated)
        subtitle = Text(f" +{remaining} more lines ", style=MUTED)
    else:
        content = Markdown(text)
        subtitle = None

    title = Text()
    title.append(f" {ICONS['output']} ", style=MUTED)
    title.append("output ", style=DIM)
    console.print(Panel(
        content,
        title=title,
        title_align="left",
        border_style=BORDERS["output"], box=box.ROUNDED,
        padding=(0, 1),
        subtitle=subtitle, subtitle_align="right",
    ))


def render_plan_card(console: Console, plan_text: str):
    """Magenta-bordered plan panel with execute/reject subtitle."""
    console.print()
    title = Text()
    title.append(f" {ICONS['plan']} ", style=f"bold {ACCENT_MAGENTA}")
    title.append("plan ", style=DIM)
    console.print(Panel(
        Markdown(plan_text),
        title=title,
        title_align="left",
        subtitle=Text(" [y] execute  [n] reject ", style=MUTED),
        subtitle_align="right",
        border_style=BORDERS["plan"], box=box.ROUNDED,
        padding=(1, 2),
    ))


def render_tool_result(console: Console, tool_name: str, summary: str,
                       elapsed_ms: float, result: dict):
    """Dispatch to the right card renderer based on result shape."""
    result_type = detect_result_type(result)
    if result_type == "error":
        render_error_card(console, tool_name, elapsed_ms, result)
    elif result_type == "metrics":
        render_metrics_card(console, tool_name, elapsed_ms, result)
    elif result_type == "calphad":
        render_calphad_card(console, tool_name, elapsed_ms, result)
    elif result_type == "validation":
        render_validation_card(console, tool_name, elapsed_ms, result)
    elif result_type == "results":
        render_results_card(console, tool_name, summary, elapsed_ms, result)
    elif result_type == "plot":
        render_plot_card(console, tool_name, elapsed_ms, result)
    else:
        render_success_card(console, tool_name, summary, elapsed_ms)


def render_success_card(console: Console, tool_name: str,
                        summary: str, elapsed_ms: float):
    title = make_title("tool", tool_name, elapsed_ms)
    body = Text()
    body.append(f" {ICONS['success']} ", style=f"bold {SUCCESS}")
    body.append(summary or "completed", style=DIM)
    console.print(Panel(
        body, title=title, title_align="left",
        border_style=BORDERS["tool"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_error_card(console: Console, tool_name: str,
                      elapsed_ms: float, result: dict):
    error_msg = str(result.get("error", "Unknown error"))
    succeeded = result.get("succeeded", [])
    failed = result.get("failed", {})

    if succeeded:
        title = make_title("error", tool_name, elapsed_ms, "PARTIAL")
        body = Text()
        if isinstance(failed, dict):
            for name, err in list(failed.items())[:3]:
                body.append(f"  {ICONS['error']} {name}: {str(err)[:60]}\n", style=ERROR)
        n = len(succeeded)
        count = result.get("count", "?")
        body.append(f"  {ICONS['success']} {n} succeeded ({count} results)\n", style=SUCCESS)
        body.append(f"\n  [r] Retry failed  [s] Skip", style=MUTED)
        border = BORDERS["error_partial"]
    else:
        title = make_title("error", tool_name, elapsed_ms, "FAILED")
        body = Text()
        body.append(f" {ICONS['error']} ", style=f"bold {ERROR}")
        if len(error_msg) > 200:
            error_msg = error_msg[:200] + "\u2026"
        body.append(error_msg, style=DIM)
        border = BORDERS["error"]

    console.print(Panel(
        body, title=title, title_align="left",
        border_style=border, box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_metrics_card(console: Console, tool_name: str,
                        elapsed_ms: float, result: dict):
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
    if result.get("filename"):
        table.add_row("Plot", result["filename"])

    console.print(Panel(
        table, title=title, title_align="left",
        border_style=BORDERS["metrics"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_calphad_card(console: Console, tool_name: str,
                        elapsed_ms: float, result: dict):
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
    elif isinstance(phases, list):
        body.append(f"  Phases: {', '.join(str(p) for p in phases)}\n")
    gibbs = result.get("gibbs_energy")
    if gibbs is not None:
        body.append(f"  \u0394G = {gibbs:.1f} J/mol\n", style=MUTED)

    console.print(Panel(
        body, title=title, title_align="left",
        border_style=BORDERS["calphad"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_validation_card(console: Console, tool_name: str,
                           elapsed_ms: float, result: dict):
    quality = result.get("quality_score", "")
    findings = result.get("findings", {})

    subtitle = Text()
    if quality:
        subtitle.append(f"Quality: {quality:.2f}", style=MUTED)
    title = make_title("validation", tool_name, elapsed_ms)

    body = Text()
    severity_styles = {"critical": ERROR, "warning": WARNING, "info": INFO}
    for severity in ("critical", "warning", "info"):
        items = findings.get(severity, [])
        if items:
            body.append(
                f"  {ICONS['validation']} {len(items)} {severity.upper()}\n",
                style=f"bold {severity_styles[severity]}",
            )
            for item in items[:2]:
                msg = item.get("msg", item) if isinstance(item, dict) else str(item)
                body.append(f"    {msg}\n", style=MUTED)
            if len(items) > 2:
                body.append(f"    \u2026 +{len(items) - 2} more\n", style=MUTED)

    border_key = (
        "validation_critical" if findings.get("critical")
        else "validation_warning" if findings.get("warning")
        else "validation_info"
    )
    console.print(Panel(
        body, title=title, title_align="left",
        subtitle=subtitle, subtitle_align="right",
        border_style=BORDERS[border_key], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_results_card(console: Console, tool_name: str, summary: str,
                        elapsed_ms: float, result: dict):
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

    remaining = total - len(preview)
    subtitle = Text()
    if remaining > 0:
        subtitle.append(f"+{remaining} more  ", style=MUTED)
    subtitle.append("/export to save", style=MUTED)

    console.print(Panel(
        table, title=title, title_align="left",
        subtitle=subtitle, subtitle_align="right",
        border_style=BORDERS["results"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_plot_card(console: Console, tool_name: str,
                     elapsed_ms: float, result: dict):
    title = make_title("plot", tool_name, elapsed_ms)
    body = Text()
    body.append(f" {ICONS['success']} ", style=f"bold {SUCCESS}")
    desc = result.get("description", "Plot saved")
    body.append(f"{desc}\n", style=DIM)
    body.append(f"  {ICONS['plot']} {result.get('filename', '')}", style=MUTED)
    console.print(Panel(
        body, title=title, title_align="left",
        border_style=BORDERS["plot"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def render_approval_card(console: Console, tool_name: str, tool_args: dict):
    """Render the approval request panel (card only, no input)."""
    args_summary = ", ".join(
        f"{k}={v!r}" for k, v in list(tool_args.items())[:3]
    )
    title = make_title("approval", tool_name)
    body = Text()
    body.append(f"{ICONS['approval']} Requires approval\n", style=WARNING)
    body.append(f"  {args_summary}\n", style=MUTED)
    body.append("\n  [y] approve  [n] deny  [a] always approve", style=MUTED)
    console.print(Panel(
        body, title=title, title_align="left",
        border_style=BORDERS["approval"], box=box.ROUNDED,
        padding=(0, 1),
    ))


def format_tokens(n: int) -> str:
    """Format token count: 500 -> '500', 2100 -> '2.1k'."""
    if n >= 1000:
        return f"{n / 1000:.1f}k"
    return str(n)


def render_cost_line(console: Console, usage, turn_cost: float | None = None,
                     session_cost: float = 0.0):
    """Print a dim cost/token summary line after a turn."""
    parts = [
        f"{format_tokens(usage.input_tokens)} in",
        f"{format_tokens(usage.output_tokens)} out",
    ]
    if turn_cost is not None:
        parts.append(f"${turn_cost:.4f}")
        parts.append(f"total: ${session_cost:.4f}")
    line = " \u00b7 ".join(parts)
    console.print(f" [dim]\u2500 {line} \u2500[/dim]")
