"""Card widgets for the PRISM TUI output stream."""
from textual.widget import Widget
from rich.text import Text
from rich.panel import Panel
from rich.markdown import Markdown
from rich.table import Table
from app.tui.theme import CARD_BORDERS, TEXT_DIM, SUCCESS, ERROR, WARNING, INFO


def detect_card_type(result: dict) -> str:
    """Determine card type from a tool result dict shape."""
    if "error" in result:
        return "error"
    if "metrics" in result and "algorithm" in result:
        return "metrics"
    if "phases_present" in result and "gibbs_energy" in result:
        return "calphad"
    if "findings" in result and "quality_score" in result:
        return "validation"
    if isinstance(result.get("filename"), str) and result["filename"].endswith(".png"):
        return "plot"
    if isinstance(result.get("results"), list) and len(result["results"]) > 3:
        return "results_table"
    return "tool"


class InputCard(Widget):
    """Displays a user input message."""
    DEFAULT_CSS = "InputCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, message: str, **kwargs):
        super().__init__(**kwargs)
        self.message = message

    def render(self) -> Panel:
        return Panel(
            Text(self.message),
            title="input", title_align="left",
            border_style=CARD_BORDERS["input"], padding=(0, 1),
        )


class OutputCard(Widget):
    """Displays agent text output with optional truncation."""
    DEFAULT_CSS = "OutputCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, content: str, truncation_lines: int = 6, **kwargs):
        super().__init__(**kwargs)
        self.full_content = content
        self.truncation_lines = truncation_lines
        lines = content.split("\n")
        self.is_truncated = len(lines) > truncation_lines
        if self.is_truncated:
            self._display = "\n".join(lines[:truncation_lines]) + "\n..."
        else:
            self._display = content

    def render(self) -> Panel:
        title_text = Text()
        title_text.append("output", style=TEXT_DIM)
        footer = Text("Ctrl+O", style=TEXT_DIM) if self.is_truncated else None
        return Panel(
            Markdown(self._display),
            title=title_text, title_align="left",
            subtitle=footer, subtitle_align="right",
            border_style=CARD_BORDERS["output"], padding=(0, 1),
        )


class ToolCard(Widget):
    """Displays a tool execution result."""
    DEFAULT_CSS = "ToolCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float, summary: str,
                 result: dict, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.summary = summary
        self.result = result
        self.is_error = "error" in result

    def _format_elapsed(self) -> str:
        if self.elapsed_ms >= 1000:
            return f"{self.elapsed_ms / 1000:.1f}s"
        return f"{self.elapsed_ms:.0f}ms"

    def render(self) -> Panel:
        card_type = detect_card_type(self.result)
        border = CARD_BORDERS.get(card_type, CARD_BORDERS["tool"])
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {self._format_elapsed()} ", style=TEXT_DIM)
        body = Text()
        if self.is_error:
            body.append("\u2717 ", style=f"bold {ERROR}")
            body.append(str(self.result.get("error", ""))[:200], style=TEXT_DIM)
        else:
            body.append("\u2714 ", style=f"bold {SUCCESS}")
            body.append(self.summary or "completed", style=TEXT_DIM)
        return Panel(body, title=title, title_align="left",
                     border_style=border, padding=(0, 1))


class ApprovalCard(Widget):
    """Displays a tool approval request."""
    DEFAULT_CSS = "ApprovalCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, tool_args: dict, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.tool_args = tool_args

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(" approval ", style=f"bold {WARNING}")
        body = Text()
        args_preview = ", ".join(f"{k}={v!r}" for k, v in list(self.tool_args.items())[:3])
        body.append(f"\u26a0 Requires approval\n", style=WARNING)
        body.append(f"  {args_preview}\n", style=TEXT_DIM)
        body.append("\n  [y] approve  [n] deny  [a] always", style=TEXT_DIM)
        return Panel(body, title=title, title_align="left",
                     border_style=CARD_BORDERS["approval"], padding=(0, 1))


class PlanCard(Widget):
    """Displays an agent plan for user confirmation."""
    DEFAULT_CSS = "PlanCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, plan_text: str, **kwargs):
        super().__init__(**kwargs)
        self.plan_text = plan_text

    def render(self) -> Panel:
        return Panel(
            Markdown(self.plan_text),
            title=Text(" plan ", style=f"bold {INFO}"),
            title_align="left",
            subtitle=Text("[y] execute  [n] reject", style=TEXT_DIM),
            subtitle_align="right",
            border_style=CARD_BORDERS["plan"], padding=(1, 2),
        )


class ErrorRetryCard(Widget):
    """Displays a failed or partially failed tool execution with retry option."""
    DEFAULT_CSS = "ErrorRetryCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float,
                 succeeded: list[str], failed: dict[str, str],
                 partial_result: dict | None = None, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.succeeded = succeeded
        self.failed = failed
        self.partial_result = partial_result
        self.is_partial = len(succeeded) > 0

    def _format_elapsed(self) -> str:
        if self.elapsed_ms >= 1000:
            return f"{self.elapsed_ms / 1000:.1f}s"
        return f"{self.elapsed_ms:.0f}ms"

    def render(self) -> Panel:
        border = CARD_BORDERS["error_partial"] if self.is_partial else CARD_BORDERS["error"]
        label = "PARTIAL" if self.is_partial else "FAILED"

        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {label} ", style=f"bold {border}")
        title.append(f" {self._format_elapsed()} ", style=TEXT_DIM)

        body = Text()
        for name, err in self.failed.items():
            body.append(f"  \u2717 {name}: {err[:60]}\n", style=ERROR)
        if self.succeeded:
            n = len(self.succeeded)
            count = self.partial_result.get("count", "?") if self.partial_result else 0
            body.append(f"  \u2714 {n} succeeded ({count} results)\n", style=SUCCESS)
        body.append(f"\n  [r] Retry failed  [s] Skip  [Ctrl+O] Details", style=TEXT_DIM)

        return Panel(
            body,
            title=title, title_align="left",
            border_style=border, padding=(0, 1),
        )


class MetricsCard(Widget):
    """Displays ML training metrics."""
    DEFAULT_CSS = "MetricsCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float,
                 property_name: str, algorithm: str,
                 metrics: dict, plot_path: str = "", **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.property_name = property_name
        self.algorithm = algorithm
        self.metrics = metrics
        self.plot_path = plot_path

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {self.algorithm} ", style=TEXT_DIM)

        table = Table(show_header=False, box=None, padding=(0, 1))
        table.add_column(style="bold")
        table.add_column()
        table.add_row("Property", self.property_name)
        for k, v in self.metrics.items():
            label = k.upper() if k in ("mae", "rmse", "r2") else k
            table.add_row(label, f"{v:.4f}" if isinstance(v, float) else str(v))
        if self.plot_path:
            table.add_row("Plot", self.plot_path)

        return Panel(table, title=title, title_align="left",
                     border_style=CARD_BORDERS["metrics"], padding=(0, 1))


class CalphadCard(Widget):
    """Displays CALPHAD equilibrium/phase results."""
    DEFAULT_CSS = "CalphadCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float,
                 system: str, conditions: str,
                 phases: dict, gibbs_energy: float, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.system = system
        self.conditions = conditions
        self.phases = phases
        self.gibbs_energy = gibbs_energy

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")

        body = Text()
        body.append(f"System: {self.system}  {self.conditions}\n", style="bold")
        for phase, frac in self.phases.items():
            body.append(f"  {phase}: {frac:.2f}\n")
        body.append(f"\u0394G = {self.gibbs_energy:.1f} J/mol\n", style=TEXT_DIM)

        return Panel(body, title=title, title_align="left",
                     border_style=CARD_BORDERS["calphad"], padding=(0, 1))


class ValidationCard(Widget):
    """Displays validation/review findings with severity colors."""
    DEFAULT_CSS = "ValidationCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float,
                 quality_score: float, findings: dict, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.quality_score = quality_score
        self.findings = findings

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" Quality: {self.quality_score:.2f} ", style=TEXT_DIM)

        body = Text()
        colors = {"critical": ERROR, "warning": WARNING, "info": INFO}
        icons = {"critical": "\u25cf", "warning": "\u25cf", "info": "\u25cf"}
        for severity in ("critical", "warning", "info"):
            items = self.findings.get(severity, [])
            if items:
                body.append(f"  {icons[severity]} {len(items)} {severity.upper()}\n",
                            style=f"bold {colors[severity]}")
                for item in items[:2]:
                    body.append(f"    {item.get('msg', '')}\n", style=TEXT_DIM)
                if len(items) > 2:
                    body.append(f"    ... +{len(items) - 2} more\n", style=TEXT_DIM)

        return Panel(body, title=title, title_align="left",
                     subtitle=Text("Ctrl+O", style=TEXT_DIM),
                     subtitle_align="right",
                     border_style=CARD_BORDERS["validation_warning"],
                     padding=(0, 1))


class ResultsTableCard(Widget):
    """Displays tabular results with preview and export option."""
    DEFAULT_CSS = "ResultsTableCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float,
                 rows: list[dict], total_count: int,
                 preview_count: int = 3, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.rows = rows
        self.total_count = total_count
        self.preview_rows = rows[:preview_count]

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")
        title.append(f" {self.total_count} entries ", style=TEXT_DIM)

        if self.preview_rows:
            cols = list(self.preview_rows[0].keys())[:4]
            table = Table(box=None, padding=(0, 1))
            for c in cols:
                table.add_column(c, style=TEXT_DIM)
            for row in self.preview_rows:
                table.add_row(*(str(row.get(c, ""))[:20] for c in cols))
        else:
            table = Text("No preview available", style=TEXT_DIM)

        remaining = self.total_count - len(self.preview_rows)
        footer = Text()
        footer.append(f"+{remaining} more  ", style=TEXT_DIM)
        footer.append("[Ctrl+O] Full table  [e] Export", style=TEXT_DIM)

        return Panel(table, title=title, title_align="left",
                     subtitle=footer, subtitle_align="right",
                     border_style=CARD_BORDERS["results"], padding=(0, 1))


class PlotCard(Widget):
    """Displays a visualization output with file path."""
    DEFAULT_CSS = "PlotCard { height: auto; margin: 0 0 1 0; }"

    def __init__(self, tool_name: str, elapsed_ms: float,
                 description: str, file_path: str, **kwargs):
        super().__init__(**kwargs)
        self.tool_name = tool_name
        self.elapsed_ms = elapsed_ms
        self.description = description
        self.file_path = file_path

    def render(self) -> Panel:
        title = Text()
        title.append(f" {self.tool_name} ", style="bold magenta")

        body = Text()
        body.append(f"\u2714 {self.description}\n", style=SUCCESS)
        body.append(f"  {self.file_path}\n", style=TEXT_DIM)

        return Panel(body, title=title, title_align="left",
                     subtitle=Text("Ctrl+O Preview", style=TEXT_DIM),
                     subtitle_align="right",
                     border_style=CARD_BORDERS["plot"], padding=(0, 1))
