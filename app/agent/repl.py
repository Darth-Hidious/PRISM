"""Interactive REPL for the PRISM agent."""
import os
import re
import time
from typing import Optional
from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel
from rich.prompt import Confirm
from rich.text import Text
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


class AgentREPL:
    """Interactive REPL — Claude Code-inspired minimal interface."""

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
        header.append(f" {tool_name} ", style="bold magenta")
        header.append(" approval ", style="bold #d29922")
        body = Text()
        body.append(f" {args_summary}", style="dim")
        self.console.print(Panel(
            body,
            title=header,
            title_align="left",
            border_style="#d29922",
            padding=(0, 1),
        ))
        self.console.print("  [#d29922]?[/#d29922] Allow?  [bold]y[/bold] yes  [bold]n[/bold] no  [bold]a[/bold] always")
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

    # ── Main loop ──────────────────────────────────────────────────────

    def run(self):
        self._show_welcome()
        while True:
            try:
                user_input = self._get_input()
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

    def _get_input(self) -> str:
        """Prompt for user input inside a bordered box."""
        width = min(self.console.width, 120)
        border = "\u2500" * (width - 2)
        self.console.print(f"[dim]\u256d{border}\u256e[/dim]")
        user_input = self._session.prompt(
            HTML('<style fg="#555555">\u2502</style> <ansimagenta><b>\u276f </b></ansimagenta>'),
        ).strip()
        self.console.print(f"[dim]\u2570{border}\u256f[/dim]")
        return user_input

    # ── Streaming ──────────────────────────────────────────────────────

    # ── Plan step helpers ────────────────────────────────────────────

    @staticmethod
    def _parse_plan_steps(plan_text: str) -> list:
        """Extract numbered steps from plan text."""
        steps = []
        for line in plan_text.strip().splitlines():
            m = re.match(r"\s*(\d+)[.)\s]+(.+)", line)
            if m:
                steps.append(m.group(2).strip())
        return steps

    def _render_step_progress(self, steps: list, current: int):
        """Render a live-updated progress panel for plan steps."""
        lines = Text()
        for i, step in enumerate(steps):
            if i > 0:
                lines.append("\n")
            if i < current:
                lines.append("  \u2714 ", style="bold green")
                lines.append(step, style="dim strikethrough")
            elif i == current:
                lines.append("  \u25b8 ", style="bold magenta")
                lines.append(step, style="bold")
            else:
                lines.append("  \u25cb ", style="dim")
                lines.append(step, style="dim")
        self.console.print(Panel(
            lines,
            title="[bold]Progress[/bold]",
            border_style="dim",
            padding=(0, 1),
        ))

    # ── Streaming ──────────────────────────────────────────────────────

    def _handle_streaming_response(self, user_input: str):
        accumulated_text = ""
        plan_buffer = ""
        in_plan = False
        tool_start_time = None
        current_tool_name = None
        spinner = Spinner(console=self.console)

        # Plan step tracking
        plan_steps: list = []
        step_idx = 0

        # Show thinking spinner immediately while waiting for first token
        spinner.start("Thinking...")

        for event in self.agent.process_stream(user_input):
            if isinstance(event, TextDelta):
                # Stop the initial "Thinking..." spinner on first text
                spinner.stop()
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
                        self.console.print()
                        self.console.print(Panel(
                            Markdown(plan_buffer.strip()),
                            title="[bold]Plan[/bold]",
                            border_style="dim",
                            padding=(1, 2),
                        ))
                        if self.scratchpad:
                            self.scratchpad.log("plan", summary="Plan proposed", data={"plan": plan_buffer.strip()})
                        if not Confirm.ask("  Execute?", default=True):
                            self.console.print("[dim]Cancelled.[/dim]")
                            return
                        # Parse steps for progress tracking
                        plan_steps = self._parse_plan_steps(plan_buffer)
                        step_idx = 0
                        if plan_steps:
                            self._render_step_progress(plan_steps, step_idx)
                        remainder = event.text.split("</plan>", 1)[1] if "</plan>" in event.text else ""
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue
                # Don't write raw text — we render as Markdown at breaks

            elif isinstance(event, ToolCallStart):
                spinner.stop()
                # Flush accumulated text as Markdown before tool card
                if accumulated_text.strip():
                    self.console.print()
                    self.console.print(Markdown(accumulated_text.strip()))
                    accumulated_text = ""
                tool_start_time = time.monotonic()
                current_tool_name = event.tool_name
                verb = spinner.verb_for_tool(event.tool_name)
                # Show step context in spinner if we have a plan
                if plan_steps and step_idx < len(plan_steps):
                    verb = f"[{step_idx + 1}/{len(plan_steps)}] {verb}"
                spinner.start(verb)

            elif isinstance(event, ToolApprovalRequest):
                spinner.stop()

            elif isinstance(event, ToolCallResult):
                spinner.stop()
                elapsed_str = ""
                if tool_start_time:
                    ms = (time.monotonic() - tool_start_time) * 1000
                    elapsed_str = f"{ms:.0f}ms" if ms < 2000 else f"{ms / 1000:.1f}s"
                    tool_start_time = None
                summary = event.summary if hasattr(event, "summary") else "done"
                self._render_tool_card(event.tool_name, summary, elapsed_str)
                current_tool_name = None
                # Advance plan step and show updated progress
                if plan_steps:
                    step_idx = min(step_idx + 1, len(plan_steps))
                    if step_idx < len(plan_steps):
                        self._render_step_progress(plan_steps, step_idx)

            elif isinstance(event, TurnComplete):
                spinner.stop()
                tool_start_time = None

        # Show final progress (all steps done)
        if plan_steps:
            self._render_step_progress(plan_steps, len(plan_steps))

        # Flush any remaining text as Markdown
        if accumulated_text.strip():
            self.console.print()
            self.console.print(Markdown(accumulated_text.strip()))
            self.console.print()

    def _render_tool_card(self, tool_name: str, summary: str, elapsed: str):
        """Render a bordered tool call result card."""
        header = Text()
        header.append(f" {tool_name} ", style="bold magenta")
        if elapsed:
            header.append(f" {elapsed}", style="dim")

        body = Text()
        body.append(" ✓ ", style="bold green")
        body.append(summary, style="dim")

        self.console.print(Panel(
            body,
            title=header,
            title_align="left",
            border_style="dim",
            padding=(0, 1),
        ))

    # ── Exit ───────────────────────────────────────────────────────────

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

    # ── Welcome ────────────────────────────────────────────────────────

    def _show_welcome(self):
        from app import __version__
        caps = self._detect_capabilities()

        # VIBGYOR letter colors
        colors = {
            "P": "#ff0000", "R": "#ff7700", "I": "#ffdd00",
            "S": "#00cc44", "M": "#0066ff",
        }
        # 5-line block-letter art
        art_lines = [
            " ██████  ██████  ██  ███████ ███    ███",
            " ██   ██ ██   ██ ██  ██      ████  ████",
            " ██████  ██████  ██  ███████ ██ ████ ██",
            " ██      ██   ██ ██       ██ ██  ██  ██",
            " ██      ██   ██ ██  ███████ ██      ██",
        ]
        # Character ranges for each letter in the art
        letter_spans = [
            ("P", 0, 8), ("R", 8, 16), ("I", 16, 20),
            ("S", 20, 28), ("M", 28, 40),
        ]

        self.console.print()
        for line in art_lines:
            text = Text()
            for letter, start, end in letter_spans:
                segment = line[start:end] if end <= len(line) else line[start:]
                text.append(segment, style=f"bold {colors[letter]}")
            self.console.print(text)

        # Prism triangle + rainbow bar
        self.console.print()
        triangle = Text()
        triangle.append("           ▲\n", style="dim")
        triangle.append("          ╱ ╲\n", style="dim")
        triangle.append("         ╱   ╲\n", style="dim")
        triangle.append("        ▔▔▔▔▔▔▔\n", style="dim")

        rainbow_colors = [
            "#ff0000", "#ff3300", "#ff6600", "#ff9900", "#ffcc00",
            "#ffff00", "#ccff00", "#66ff00", "#00cc44", "#00aa88",
            "#0088cc", "#0055ff", "#2200ff", "#4400cc", "#6600aa",
        ]
        bar = Text("     ")
        for c in rainbow_colors:
            bar.append("━", style=c)
        self.console.print(triangle, end="")
        self.console.print(bar)
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
        info.append(f"  v{__version__}", style="dim")
        if provider:
            info.append("  ·  ", style="dim")
            info.append(provider, style="bold magenta")
        self.console.print(info)

        # Capabilities line
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
        self.console.print("[dim]  " + " · ".join(parts) + "[/dim]")
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

    # ── Command dispatch ───────────────────────────────────────────────

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

    # ── /help ──────────────────────────────────────────────────────────

    def _handle_help(self):
        self.console.print()
        for name, desc in REPL_COMMANDS.items():
            if name == "/quit":
                continue  # skip alias
            self.console.print(f"  [bold]{name:<16}[/bold] [dim]{desc}[/dim]")
        self.console.print()

    # ── /tools ─────────────────────────────────────────────────────────

    def _handle_tools(self):
        self.console.print()
        tools = self.agent.tools.list_tools()
        for tool in tools:
            name_style = "bold #d29922" if tool.requires_approval else "bold"
            flag = " [#d29922]★[/#d29922]" if tool.requires_approval else ""
            self.console.print(f"  [{name_style}]{tool.name:<28}[/{name_style}] [dim]{tool.description[:55]}[/dim]{flag}")
        self.console.print(f"\n  [dim]{len(tools)} tools[/dim]  [#d29922]★[/#d29922] [dim]= requires approval[/dim]")
        self.console.print()

    # ── /status ────────────────────────────────────────────────────────

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

    # ── /login ─────────────────────────────────────────────────────────

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

    # ── /skills ────────────────────────────────────────────────────────

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

    # ── /plan ──────────────────────────────────────────────────────────

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

    # ── /scratchpad ────────────────────────────────────────────────────

    def _handle_scratchpad(self):
        if not self.scratchpad or not self.scratchpad.entries:
            self.console.print("[dim]Empty.[/dim]")
            return
        self.console.print()
        for i, entry in enumerate(self.scratchpad.entries, 1):
            tool = f" {entry.tool_name}" if entry.tool_name else ""
            self.console.print(f"  [dim]{i}.[/dim]{tool} {entry.summary} [dim]{entry.timestamp}[/dim]")
        self.console.print()

    # ── /mcp ───────────────────────────────────────────────────────────

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

    # ── /export ────────────────────────────────────────────────────────

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

    # ── /sessions, /load ───────────────────────────────────────────────

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
