"""Interactive REPL for the PRISM agent.

Rich panels for output + prompt_toolkit for input — the same pattern
used by Aider, Open Interpreter, and other Python AI CLIs.
"""
import os
import time
from typing import Optional
from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel
from rich.prompt import Confirm
from rich.table import Table
from rich.text import Text
from rich import box
from prompt_toolkit import PromptSession
from prompt_toolkit.history import FileHistory
from prompt_toolkit.auto_suggest import AutoSuggestFromHistory
from prompt_toolkit.formatted_text import HTML
from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest,
)
from app.agent.memory import SessionMemory
from app.agent.scratchpad import Scratchpad
from app.agent.spinner import Spinner
from app.tools.base import ToolRegistry

# ── Theme constants ──────────────────────────────────────────────────

_SUCCESS = "#00cc44"
_WARNING = "#d29922"
_ERROR = "#cc555a"
_INFO = "#0088ff"
_ACCENT = "bold magenta"
_DIM = "dim"
_TEXT = "#e0e0e0"

# Card border colors by result type
_BORDERS = {
    "tool": _DIM,
    "error": _ERROR,
    "error_partial": _WARNING,
    "metrics": _INFO,
    "calphad": _INFO,
    "validation_critical": _ERROR,
    "validation_warning": _WARNING,
    "validation_info": _INFO,
    "results": _DIM,
    "plot": _SUCCESS,
    "approval": _WARNING,
    "plan": _INFO,
}

# Mascot: C4 glowing hex crystal
_MASCOT = [
    "   \u2b21 \u2b21 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "  \u2b21 \u2b22 \u2b22 \u2b22 \u2b21",
    "   \u2b21 \u2b21 \u2b21",
]

# VIBGYOR rainbow
_RAINBOW = [
    "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
    "#00cc44", "#00cccc", "#0088ff", "#5500ff", "#8b00ff",
    "#ff0000", "#ff5500", "#ff8800", "#ffcc00", "#88ff00",
]

# Default truncation for long output
_TRUNCATION_LINES = 25


REPL_COMMANDS = {
    "/exit": "Exit",
    "/quit": "Exit",
    "/clear": "Clear conversation",
    "/help": "Show commands",
    "/history": "Message count",
    "/tools": "List tools",
    "/skills": "List skills",
    "/status": "Platform status",
    "/mcp": "MCP servers",
    "/save": "Save session",
    "/export": "Export to CSV",
    "/sessions": "List sessions",
    "/load": "Load session",
    "/plan": "Plan a goal",
    "/scratchpad": "Execution log",
    "/approve-all": "Skip consent",
    "/login": "MARC27 account",
}

# Keep old aliases
_COMMAND_ALIASES = {"/skill": "/skills"}


# ── Result type detection ────────────────────────────────────────────

def _detect_result_type(result: dict) -> str:
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


class AgentREPL:
    """Interactive REPL — Rich panels + prompt_toolkit."""

    def __init__(self, backend: Backend, system_prompt: Optional[str] = None,
                 tools: Optional[ToolRegistry] = None, enable_mcp: bool = True,
                 auto_approve: bool = False):
        self.console = Console(highlight=False)
        self.memory = SessionMemory()
        self.scratchpad = Scratchpad()
        self._mcp_tools: list[str] = []
        self._auto_approve = auto_approve
        self._auto_approve_tools: set = set()
        if tools is None:
            from app.plugins.bootstrap import build_full_registry
            tools = build_full_registry(enable_mcp=enable_mcp)
        self.agent = AgentCore(
            backend=backend, tools=tools, system_prompt=system_prompt,
            approval_callback=self._approval_callback,
            auto_approve=auto_approve,
        )
        self.agent.scratchpad = self.scratchpad

        history_path = os.path.expanduser("~/.prism/repl_history")
        os.makedirs(os.path.dirname(history_path), exist_ok=True)
        self._session = PromptSession(
            history=FileHistory(history_path),
            auto_suggest=AutoSuggestFromHistory(),
            multiline=False,
            enable_history_search=True,
        )

    def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
        if tool_name in self._auto_approve_tools:
            return True
        args_summary = ", ".join(f"{k}={v!r}" for k, v in list(tool_args.items())[:3])
        header = Text()
        header.append(f" {tool_name} ", style=_ACCENT)
        header.append(" approval ", style=f"bold {_WARNING}")
        body = Text()
        body.append(f"\u26a0 Requires approval\n", style=_WARNING)
        body.append(f"  {args_summary}\n", style=_DIM)
        body.append("\n  [y] approve  [n] deny  [a] always", style=_DIM)
        self.console.print(Panel(
            body, title=header, title_align="left",
            border_style=_WARNING, box=box.ROUNDED, padding=(0, 1),
        ))
        try:
            answer = input("    ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return False
        if answer == "a":
            self._auto_approve_tools.add(tool_name)
            return True
        return answer in ("y", "yes", "")

    def _load_session(self, session_id: str):
        self.memory.load(session_id)
        self.agent.history = list(self.memory.get_history())
        entries = self.memory.get_scratchpad_entries()
        if entries:
            self.scratchpad = Scratchpad.from_dict(entries)
            self.agent.scratchpad = self.scratchpad

    # ── Main loop ────────────────────────────────────────────────────

    def run(self):
        self._show_welcome()
        while True:
            try:
                user_input = self._session.prompt(
                    HTML('<ansimagenta><b>\u276f </b></ansimagenta>'),
                ).strip()
            except (EOFError, KeyboardInterrupt):
                self._prompt_save_on_exit()
                self.console.print("\n[dim]Goodbye.[/dim]")
                break
            if not user_input:
                continue
            if user_input.startswith("/"):
                if self._handle_command(user_input):
                    break
                continue
            try:
                self._handle_streaming_response(user_input)
            except KeyboardInterrupt:
                self.console.print("\n[dim]Interrupted.[/dim]")
            except Exception as e:
                self.console.print(f"\n[red]Error: {e}[/red]")

    # ── Streaming ────────────────────────────────────────────────────

    def _handle_streaming_response(self, user_input: str):
        accumulated_text = ""
        plan_buffer = ""
        in_plan = False
        tool_start_time = None
        current_tool_name = None
        spinner = Spinner(console=self.console)

        for event in self.agent.process_stream(user_input):
            if isinstance(event, TextDelta):
                accumulated_text += event.text
                if "<plan>" in accumulated_text and not in_plan:
                    in_plan = True
                    plan_buffer = accumulated_text.split("<plan>", 1)[1]
                    pre = accumulated_text.split("<plan>", 1)[0].strip()
                    if pre:
                        self.console.print(Markdown(pre))
                    accumulated_text = ""
                elif in_plan:
                    if "</plan>" in event.text:
                        plan_buffer += event.text.split("</plan>")[0]
                        in_plan = False
                        self._render_plan_card(plan_buffer.strip())
                        if self.scratchpad:
                            self.scratchpad.log("plan", summary="Plan proposed", data={"plan": plan_buffer.strip()})
                        if not Confirm.ask("  Execute?", default=True):
                            self.console.print("[dim]Cancelled.[/dim]")
                            return
                        remainder = event.text.split("</plan>", 1)[1] if "</plan>" in event.text else ""
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue

            elif isinstance(event, ToolCallStart):
                # Flush accumulated text before tool card
                if accumulated_text.strip():
                    self._render_output(accumulated_text.strip())
                    accumulated_text = ""
                tool_start_time = time.monotonic()
                current_tool_name = event.tool_name
                verb = spinner.verb_for_tool(event.tool_name)
                spinner.start(verb)

            elif isinstance(event, ToolApprovalRequest):
                pass

            elif isinstance(event, ToolCallResult):
                spinner.stop()
                elapsed_ms = 0.0
                if tool_start_time:
                    elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                    tool_start_time = None
                result = event.result if isinstance(event.result, dict) else {}
                self._render_tool_result(event.tool_name, event.summary, elapsed_ms, result)
                current_tool_name = None

            elif isinstance(event, TurnComplete):
                spinner.stop()
                tool_start_time = None

        # Flush remaining text
        if accumulated_text.strip():
            self._render_output(accumulated_text.strip())

    # ── Output rendering ─────────────────────────────────────────────

    def _render_output(self, text: str):
        """Render agent text output with truncation for long content."""
        self.console.print()
        lines = text.split("\n")
        if len(lines) > _TRUNCATION_LINES:
            truncated = "\n".join(lines[:_TRUNCATION_LINES])
            remaining = len(lines) - _TRUNCATION_LINES
            self.console.print(Markdown(truncated))
            self.console.print(f"  [dim]... {remaining} more lines (use /scratchpad to review)[/dim]")
        else:
            self.console.print(Markdown(text))
        self.console.print()

    def _render_plan_card(self, plan_text: str):
        """Render an agent plan in a bordered panel."""
        self.console.print()
        self.console.print(Panel(
            Markdown(plan_text),
            title=Text(" plan ", style=f"bold {_INFO}"),
            title_align="left",
            subtitle=Text("[y] execute  [n] reject", style=_DIM),
            subtitle_align="right",
            border_style=_BORDERS["plan"], box=box.ROUNDED,
            padding=(1, 2),
        ))

    def _render_tool_result(self, tool_name: str, summary: str, elapsed_ms: float, result: dict):
        """Render a tool result as a typed, color-coded panel."""
        result_type = _detect_result_type(result)

        # Dispatch to specialized renderers
        if result_type == "error":
            self._render_error_card(tool_name, elapsed_ms, result)
        elif result_type == "metrics":
            self._render_metrics_card(tool_name, elapsed_ms, result)
        elif result_type == "calphad":
            self._render_calphad_card(tool_name, elapsed_ms, result)
        elif result_type == "validation":
            self._render_validation_card(tool_name, elapsed_ms, result)
        elif result_type == "results":
            self._render_results_card(tool_name, summary, elapsed_ms, result)
        elif result_type == "plot":
            self._render_plot_card(tool_name, elapsed_ms, result)
        else:
            self._render_success_card(tool_name, summary, elapsed_ms)

    def _format_elapsed(self, ms: float) -> str:
        if ms >= 2000:
            return f"{ms / 1000:.1f}s"
        if ms > 0:
            return f"{ms:.0f}ms"
        return ""

    def _make_title(self, tool_name: str, elapsed_ms: float, label: str = "") -> Text:
        title = Text()
        title.append(f" {tool_name} ", style=_ACCENT)
        if label:
            title.append(f" {label} ", style=f"bold {_WARNING}")
        elapsed = self._format_elapsed(elapsed_ms)
        if elapsed:
            title.append(f" {elapsed}", style=_DIM)
        return title

    def _render_success_card(self, tool_name: str, summary: str, elapsed_ms: float):
        """Standard success tool card."""
        title = self._make_title(tool_name, elapsed_ms)
        body = Text()
        body.append(" \u2714 ", style=f"bold {_SUCCESS}")
        body.append(summary or "completed", style=_DIM)
        self.console.print(Panel(
            body, title=title, title_align="left",
            border_style=_BORDERS["tool"], box=box.ROUNDED, padding=(0, 1),
        ))

    def _render_error_card(self, tool_name: str, elapsed_ms: float, result: dict):
        """Error card with formatted error message (not raw JSON dump)."""
        error_msg = str(result.get("error", "Unknown error"))
        # Check for partial success (some providers succeeded)
        succeeded = result.get("succeeded", [])
        failed = result.get("failed", {})

        if succeeded:
            # Partial failure
            title = self._make_title(tool_name, elapsed_ms, "PARTIAL")
            body = Text()
            if isinstance(failed, dict):
                for name, err in list(failed.items())[:3]:
                    body.append(f"  \u2717 {name}: {str(err)[:60]}\n", style=_ERROR)
            n = len(succeeded)
            count = result.get("count", "?")
            body.append(f"  \u2714 {n} succeeded ({count} results)\n", style=_SUCCESS)
            body.append(f"\n  [r] Retry failed  [s] Skip", style=_DIM)
            border = _BORDERS["error_partial"]
        else:
            # Total failure — show clean error, not raw JSON
            title = self._make_title(tool_name, elapsed_ms)
            body = Text()
            body.append(" \u2717 ", style=f"bold {_ERROR}")
            # Truncate long errors to something readable
            if len(error_msg) > 200:
                error_msg = error_msg[:200] + "..."
            body.append(error_msg, style=_DIM)
            border = _BORDERS["error"]

        self.console.print(Panel(
            body, title=title, title_align="left",
            border_style=border, box=box.ROUNDED, padding=(0, 1),
        ))

    def _render_metrics_card(self, tool_name: str, elapsed_ms: float, result: dict):
        """ML training metrics as a compact table."""
        title = self._make_title(tool_name, elapsed_ms)
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

        self.console.print(Panel(
            table, title=title, title_align="left",
            border_style=_BORDERS["metrics"], box=box.ROUNDED, padding=(0, 1),
        ))

    def _render_calphad_card(self, tool_name: str, elapsed_ms: float, result: dict):
        """CALPHAD equilibrium/phase results."""
        title = self._make_title(tool_name, elapsed_ms)
        body = Text()
        system = result.get("system", "")
        if system:
            body.append(f"  System: {system}\n", style="bold")
        phases = result.get("phases_present", result.get("phases", {}))
        if isinstance(phases, dict):
            for phase, frac in phases.items():
                body.append(f"  {phase}: {frac:.2f}\n" if isinstance(frac, (int, float)) else f"  {phase}: {frac}\n")
        elif isinstance(phases, list):
            body.append(f"  Phases: {', '.join(str(p) for p in phases)}\n")
        gibbs = result.get("gibbs_energy")
        if gibbs is not None:
            body.append(f"  \u0394G = {gibbs:.1f} J/mol\n", style=_DIM)

        self.console.print(Panel(
            body, title=title, title_align="left",
            border_style=_BORDERS["calphad"], box=box.ROUNDED, padding=(0, 1),
        ))

    def _render_validation_card(self, tool_name: str, elapsed_ms: float, result: dict):
        """Validation findings with severity colors."""
        quality = result.get("quality_score", "")
        findings = result.get("findings", {})

        subtitle = Text()
        if quality:
            subtitle.append(f"Quality: {quality:.2f}", style=_DIM)
        title = self._make_title(tool_name, elapsed_ms)

        body = Text()
        severity_styles = {"critical": _ERROR, "warning": _WARNING, "info": _INFO}
        severity_icons = {"critical": "\u25cf", "warning": "\u25cf", "info": "\u25cf"}
        for severity in ("critical", "warning", "info"):
            items = findings.get(severity, [])
            if items:
                body.append(f"  {severity_icons[severity]} {len(items)} {severity.upper()}\n",
                            style=f"bold {severity_styles[severity]}")
                for item in items[:2]:
                    msg = item.get("msg", item) if isinstance(item, dict) else str(item)
                    body.append(f"    {msg}\n", style=_DIM)
                if len(items) > 2:
                    body.append(f"    ... +{len(items) - 2} more\n", style=_DIM)

        border_key = "validation_critical" if findings.get("critical") else "validation_warning" if findings.get("warning") else "validation_info"
        self.console.print(Panel(
            body, title=title, title_align="left",
            subtitle=subtitle, subtitle_align="right",
            border_style=_BORDERS[border_key], box=box.ROUNDED, padding=(0, 1),
        ))

    def _render_results_card(self, tool_name: str, summary: str, elapsed_ms: float, result: dict):
        """Tabular results with a 3-row preview."""
        rows = result.get("results", [])
        total = result.get("count", len(rows))
        title = self._make_title(tool_name, elapsed_ms)

        preview = rows[:3]
        if preview:
            cols = list(preview[0].keys())[:4]
            table = Table(box=box.SIMPLE, padding=(0, 1))
            for c in cols:
                table.add_column(c, style=_DIM)
            for row in preview:
                table.add_row(*(str(row.get(c, ""))[:25] for c in cols))
        else:
            table = Text(summary or "No preview", style=_DIM)

        remaining = total - len(preview)
        subtitle = Text()
        if remaining > 0:
            subtitle.append(f"+{remaining} more  ", style=_DIM)
        subtitle.append("/export to save", style=_DIM)

        self.console.print(Panel(
            table, title=title, title_align="left",
            subtitle=subtitle, subtitle_align="right",
            border_style=_BORDERS["results"], box=box.ROUNDED, padding=(0, 1),
        ))

    def _render_plot_card(self, tool_name: str, elapsed_ms: float, result: dict):
        """Plot/visualization output with file path."""
        title = self._make_title(tool_name, elapsed_ms)
        body = Text()
        body.append(" \u2714 ", style=f"bold {_SUCCESS}")
        desc = result.get("description", "Plot saved")
        body.append(f"{desc}\n", style=_DIM)
        body.append(f"  \U0001f4ca {result.get('filename', '')}", style=_DIM)
        self.console.print(Panel(
            body, title=title, title_align="left",
            border_style=_BORDERS["plot"], box=box.ROUNDED, padding=(0, 1),
        ))

    # ── Exit ─────────────────────────────────────────────────────────

    def _prompt_save_on_exit(self):
        if not self.agent.history:
            return
        try:
            answer = input("\nSave session? (y/N): ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return
        if answer == "y":
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"[dim]Saved: {sid}[/dim]")

    # ── Welcome ──────────────────────────────────────────────────────

    def _show_welcome(self):
        from app import __version__
        caps = self._detect_capabilities()

        self.console.print()

        # Hex crystal mascot with rainbow rays
        crystal_colors = {"outer": "#7777aa", "core": "#ffffff"}
        for i, line in enumerate(_MASCOT):
            text = Text()
            for ch in line:
                if ch == "\u2b22":
                    text.append(ch, style=f"bold {crystal_colors['core']}")
                elif ch == "\u2b21":
                    text.append(ch, style=crystal_colors["outer"])
                else:
                    text.append(ch)
            # Add rainbow rays on middle lines
            if i in (1, 2):
                text.append("  ")
                for j in range(15):
                    text.append("\u2501", style=f"bold {_RAINBOW[j]}")
            self.console.print(text)

        self.console.print()

        # Provider
        provider = None
        if os.getenv("MARC27_TOKEN"):
            provider = "MARC27"
        elif os.getenv("ANTHROPIC_API_KEY"):
            provider = "Claude"
        elif os.getenv("OPENAI_API_KEY"):
            provider = "GPT"
        elif os.getenv("OPENROUTER_API_KEY"):
            provider = "OpenRouter"

        info = Text()
        info.append("  PRISM", style="bold")
        info.append(f" v{__version__}", style=_DIM)
        if provider:
            info.append("  \u00b7  ", style=_DIM)
            info.append(provider, style=_ACCENT)
        self.console.print(info)

        # Capabilities
        parts = []
        tool_count = len(self.agent.tools.list_tools())
        parts.append(f"{tool_count} tools")
        try:
            from app.skills.registry import load_builtin_skills
            skill_count = len(load_builtin_skills().list_skills())
            parts.append(f"{skill_count} skills")
        except Exception:
            pass
        for name, ok in caps.items():
            if ok:
                parts.append(f"[green]{name}[/green]")
            else:
                parts.append(f"[dim]{name}[/dim]")
        if self._auto_approve:
            parts.append("[yellow]auto-approve[/yellow]")
        self.console.print("[dim]  " + " \u00b7 ".join(parts) + "[/dim]")

        # Commands hint
        self.console.print(f"  [dim]/help for commands[/dim]")
        self.console.print()

    def _detect_capabilities(self) -> dict:
        caps = {}
        try:
            import sklearn  # noqa: F401
            caps["ML"] = True
        except ImportError:
            caps["ML"] = False
        try:
            from app.simulation.bridge import check_pyiron_available
            caps["pyiron"] = check_pyiron_available()
        except Exception:
            caps["pyiron"] = False
        try:
            from app.simulation.calphad_bridge import check_calphad_available
            caps["CALPHAD"] = check_calphad_available()
        except Exception:
            caps["CALPHAD"] = False
        return caps

    # ── Command dispatch ─────────────────────────────────────────────

    def _handle_command(self, cmd: str) -> bool:
        parts = cmd.strip().split(maxsplit=1)
        base_cmd = _COMMAND_ALIASES.get(parts[0].lower(), parts[0].lower())
        arg = parts[1].strip() if len(parts) > 1 else ""

        if base_cmd in ("/exit", "/quit"):
            self._prompt_save_on_exit()
            self.console.print("[dim]Goodbye.[/dim]")
            return True
        elif base_cmd == "/clear":
            self.agent.reset()
            self.scratchpad = Scratchpad()
            self.agent.scratchpad = self.scratchpad
            self.console.print("[dim]Cleared.[/dim]")
        elif base_cmd == "/help":
            self._handle_help()
        elif base_cmd == "/history":
            self.console.print(f"[dim]{len(self.agent.history)} messages[/dim]")
        elif base_cmd == "/tools":
            self._handle_tools()
        elif base_cmd == "/skills":
            self._handle_skill(arg if arg else None)
        elif base_cmd == "/mcp":
            self._handle_mcp_status()
        elif base_cmd == "/save":
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"[dim]Saved: {sid}[/dim]")
        elif base_cmd == "/export":
            self._handle_export(arg if arg else None)
        elif base_cmd == "/sessions":
            self._handle_sessions()
        elif base_cmd == "/load":
            if not arg:
                self.console.print("[dim]Usage: /load SESSION_ID[/dim]")
            else:
                self._handle_load(arg)
        elif base_cmd == "/plan":
            if not arg:
                self.console.print("[dim]Usage: /plan <goal>[/dim]")
            else:
                self._handle_plan(arg)
        elif base_cmd == "/scratchpad":
            self._handle_scratchpad()
        elif base_cmd == "/approve-all":
            self.agent.auto_approve = True
            self._auto_approve = True
            self.console.print("[yellow]Auto-approve on.[/yellow]")
        elif base_cmd == "/status":
            self._handle_status()
        elif base_cmd == "/login":
            self._handle_login()
        else:
            self.console.print(f"[dim]Unknown: {base_cmd}  \u2014  /help for commands[/dim]")
        return False

    # ── /help ────────────────────────────────────────────────────────

    def _handle_help(self):
        self.console.print()
        for name, desc in REPL_COMMANDS.items():
            if name == "/quit":
                continue  # skip alias
            self.console.print(f"  [bold]{name:<16}[/bold] [dim]{desc}[/dim]")
        self.console.print()

    # ── /tools ───────────────────────────────────────────────────────

    def _handle_tools(self):
        self.console.print()
        tools = self.agent.tools.list_tools()
        for tool in tools:
            name_style = f"bold {_WARNING}" if tool.requires_approval else "bold"
            flag = f" [{_WARNING}]\u2605[/{_WARNING}]" if tool.requires_approval else ""
            self.console.print(f"  [{name_style}]{tool.name:<28}[/{name_style}] [dim]{tool.description[:55]}[/dim]{flag}")
        self.console.print(f"\n  [dim]{len(tools)} tools[/dim]  [{_WARNING}]\u2605[/{_WARNING}] [dim]= requires approval[/dim]")
        self.console.print()

    # ── /status ──────────────────────────────────────────────────────

    def _handle_status(self):
        from app import __version__
        caps = self._detect_capabilities()

        self.console.print()
        self.console.print(f"[bold]PRISM[/bold] v{__version__}")
        self.console.print()

        # LLM
        provider = "not configured"
        if os.getenv("MARC27_TOKEN"):
            provider = "MARC27"
        elif os.getenv("ANTHROPIC_API_KEY"):
            provider = "Anthropic (Claude)"
        elif os.getenv("OPENAI_API_KEY"):
            provider = "OpenAI"
        elif os.getenv("OPENROUTER_API_KEY"):
            provider = "OpenRouter"
        self.console.print(f"  LLM          {_dot(provider != 'not configured')} {provider}")

        labels = {"ML": "ML", "pyiron": "pyiron", "CALPHAD": "CALPHAD"}
        for key, ok in caps.items():
            label = labels.get(key, key)
            status = "[green]ready[/green]" if ok else "[dim]not installed[/dim]"
            self.console.print(f"  {label:<12}   {_dot(ok)} {status}")

        tool_count = len(self.agent.tools.list_tools())
        try:
            from app.skills.registry import load_builtin_skills
            skill_count = len(load_builtin_skills().list_skills())
        except Exception:
            skill_count = 0
        self.console.print(f"\n  [dim]{tool_count} tools \u00b7 {skill_count} skills[/dim]")

        missing = [n for n, a in caps.items() if not a]
        if missing:
            self.console.print(f"  [dim]pip install \"prism-platform[all]\" for {', '.join(missing)}[/dim]")
        self.console.print()

    # ── /login ───────────────────────────────────────────────────────

    def _handle_login(self):
        from app.config.preferences import PRISM_DIR
        from rich.prompt import Prompt

        self.console.print()
        self.console.print("[bold]MARC27 Login[/bold]")
        self.console.print("[dim]Connect your MARC27 account for managed LLM access.[/dim]")
        self.console.print()

        token = os.getenv("MARC27_TOKEN")
        if token:
            self.console.print(f"  Already logged in. [dim](token: {token[:8]}...)[/dim]")
            self.console.print("  [dim]To logout: unset MARC27_TOKEN[/dim]")
            self.console.print()
            return

        self.console.print("  [dim]1.[/dim] Go to [bold]https://marc27.com/account/tokens[/bold]")
        self.console.print("  [dim]2.[/dim] Create a PRISM API token")
        self.console.print("  [dim]3.[/dim] Paste it below")
        self.console.print()

        try:
            token_input = Prompt.ask("  Token", password=True)
        except (EOFError, KeyboardInterrupt):
            self.console.print("\n[dim]Cancelled.[/dim]")
            return

        if not token_input.strip():
            self.console.print("[dim]No token entered.[/dim]")
            return

        # Save token
        token_path = PRISM_DIR / "marc27_token"
        PRISM_DIR.mkdir(parents=True, exist_ok=True)
        token_path.write_text(token_input.strip())
        token_path.chmod(0o600)
        os.environ["MARC27_TOKEN"] = token_input.strip()

        self.console.print("[green]Logged in to MARC27.[/green]")
        self.console.print("[dim]Token saved to ~/.prism/marc27_token[/dim]")
        self.console.print()

    # ── /skills ──────────────────────────────────────────────────────

    def _handle_skill(self, name: Optional[str] = None):
        try:
            from app.skills.registry import load_builtin_skills
            skills = load_builtin_skills()
        except Exception:
            self.console.print("[dim]No skills available.[/dim]")
            return

        self.console.print()
        if name:
            try:
                skill = skills.get(name)
            except KeyError:
                self.console.print(f"[dim]Skill not found: {name}[/dim]")
                return
            self.console.print(f"  [bold]{skill.name}[/bold]  [dim]{skill.category}[/dim]")
            self.console.print(f"  {skill.description}")
            self.console.print()
            for i, step in enumerate(skill.steps, 1):
                opt = " [dim](optional)[/dim]" if step.optional else ""
                self.console.print(f"    {i}. {step.name} [dim]\u2014 {step.description}[/dim]{opt}")
        else:
            for skill in skills.list_skills():
                self.console.print(f"  {skill.name:<25} [dim]{skill.description[:55]}[/dim]")
        self.console.print()

    # ── /plan ────────────────────────────────────────────────────────

    def _handle_plan(self, goal: str):
        prompt = f"The user wants to accomplish: {goal}\n\nAvailable PRISM skills:\n"
        try:
            from app.skills.registry import load_builtin_skills
            for skill in load_builtin_skills().list_skills():
                prompt += f"- {skill.name}: {skill.description}\n"
        except Exception:
            pass
        prompt += "\nWhich skill(s) should be used? Explain the recommended workflow."
        try:
            self._handle_streaming_response(prompt)
        except Exception as e:
            self.console.print(f"[red]Error: {e}[/red]")

    # ── /scratchpad ──────────────────────────────────────────────────

    def _handle_scratchpad(self):
        if not self.scratchpad or not self.scratchpad.entries:
            self.console.print("[dim]Empty.[/dim]")
            return
        self.console.print()
        for i, entry in enumerate(self.scratchpad.entries, 1):
            tool = f" {entry.tool_name}" if entry.tool_name else ""
            self.console.print(f"  [dim]{i}.[/dim]{tool} {entry.summary} [dim]{entry.timestamp}[/dim]")
        self.console.print()

    # ── /mcp ─────────────────────────────────────────────────────────

    def _handle_mcp_status(self):
        from app.mcp_client import load_mcp_config
        config = load_mcp_config()
        self.console.print()
        if not config.servers:
            self.console.print("  [dim]No MCP servers configured.[/dim]")
        else:
            for name in config.servers:
                self.console.print(f"  {name}")
        if self._mcp_tools:
            self.console.print(f"\n  [dim]{len(self._mcp_tools)} MCP tools loaded[/dim]")
        self.console.print()

    # ── /export ──────────────────────────────────────────────────────

    def _handle_export(self, filename: Optional[str] = None):
        results = None
        for msg in reversed(self.agent.history):
            if msg.get("role") == "tool_result" and isinstance(msg.get("result"), dict):
                r = msg["result"]
                if isinstance(r.get("results"), list) and r["results"]:
                    results = r["results"]
                    break
        if not results:
            self.console.print("[dim]No exportable results.[/dim]")
            return
        export_tool = self.agent.tools.get("export_results_csv")
        if export_tool is None:
            self.console.print("[dim]Export tool not available.[/dim]")
            return
        kwargs = {"results": results}
        if filename:
            kwargs["filename"] = filename
        out = export_tool.execute(**kwargs)
        if "error" in out:
            self.console.print(f"[red]{out['error']}[/red]")
        else:
            self.console.print(f"Exported {out['rows']} rows to {out['filename']}")

    # ── /sessions, /load ─────────────────────────────────────────────

    def _handle_sessions(self):
        sessions = self.memory.list_sessions()
        if not sessions:
            self.console.print("[dim]No saved sessions.[/dim]")
            return
        self.console.print()
        for s in sessions[:20]:
            ts = s.get("timestamp", "")[:19]
            count = s.get("message_count", 0)
            self.console.print(f"  {s['session_id']}  [dim]{ts}  ({count} msgs)[/dim]")
        self.console.print()

    def _handle_load(self, session_id: str):
        try:
            self._load_session(session_id)
            self.console.print(f"Loaded {session_id} ({len(self.agent.history)} messages)")
        except FileNotFoundError:
            self.console.print(f"[red]Not found: {session_id}[/red]")
        except Exception as e:
            self.console.print(f"[red]Error: {e}[/red]")


def _dot(ok: bool) -> str:
    return "[green]\u25cf[/green]" if ok else "[dim]\u25cb[/dim]"
