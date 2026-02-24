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
