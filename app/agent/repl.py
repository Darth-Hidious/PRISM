"""Interactive REPL for the PRISM agent — Claude Code-style interface."""
import os
import sys
from typing import Optional
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.panel import Panel
from rich.prompt import Confirm
from rich.text import Text
from rich.spinner import Spinner
from rich.columns import Columns
from rich.rule import Rule
from rich.style import Style
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
from app.tools.base import ToolRegistry


REPL_COMMANDS = {
    "/exit": "Exit the REPL",
    "/quit": "Exit the REPL",
    "/clear": "Clear conversation history",
    "/help": "Show available commands",
    "/history": "Show conversation history length",
    "/tools": "List available tools",
    "/mcp": "Show connected MCP servers and their tools",
    "/save": "Save current session",
    "/export": "Export last results to CSV — /export [filename]",
    "/sessions": "List saved sessions",
    "/load": "Load a saved session — /load SESSION_ID",
    "/skill": "List skills or show details — /skill [name]",
    "/plan": "Ask which skills apply to a goal — /plan <goal>",
    "/scratchpad": "Show the agent's execution log",
    "/approve-all": "Auto-approve all tool calls (skip consent prompts)",
}


class AgentREPL:
    """Interactive REPL for conversational agent interaction — Claude Code style."""

    def __init__(self, backend: Backend, system_prompt: Optional[str] = None,
                 tools: Optional[ToolRegistry] = None, enable_mcp: bool = True,
                 auto_approve: bool = False):
        self.console = Console()
        self.memory = SessionMemory()
        self.scratchpad = Scratchpad()
        self._mcp_tools: list[str] = []
        self._auto_approve = auto_approve
        if tools is None:
            from app.plugins.bootstrap import build_full_registry
            tools = build_full_registry(enable_mcp=enable_mcp)
        self.agent = AgentCore(
            backend=backend, tools=tools, system_prompt=system_prompt,
            approval_callback=self._approval_callback,
            auto_approve=auto_approve,
        )
        self.agent.scratchpad = self.scratchpad

        # Set up prompt_toolkit session with history
        history_path = os.path.expanduser("~/.prism/repl_history")
        os.makedirs(os.path.dirname(history_path), exist_ok=True)
        self._session = PromptSession(
            history=FileHistory(history_path),
            auto_suggest=AutoSuggestFromHistory(),
            multiline=False,
            enable_history_search=True,
        )

    def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
        """Ask the user for approval before running an expensive tool."""
        args_summary = ", ".join(f"{k}={v!r}" for k, v in list(tool_args.items())[:3])
        self.console.print(f"  [dim yellow]Tool requires approval:[/dim yellow] [bold]{tool_name}[/bold]({args_summary})")
        return Confirm.ask("  [yellow]Approve?[/yellow]", default=True)

    def _load_session(self, session_id: str):
        """Restore a saved session into the agent."""
        self.memory.load(session_id)
        self.agent.history = list(self.memory.get_history())
        entries = self.memory.get_scratchpad_entries()
        if entries:
            self.scratchpad = Scratchpad.from_dict(entries)
            self.agent.scratchpad = self.scratchpad

    def run(self):
        """Main REPL loop."""
        self._show_welcome()
        while True:
            try:
                user_input = self._session.prompt(
                    HTML('<style fg="ansibrightcyan"><b>&gt; </b></style>'),
                ).strip()
            except (EOFError, KeyboardInterrupt):
                self._prompt_save_on_exit()
                self.console.print("\n[dim]Goodbye![/dim]")
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
                self.console.print(f"[red]Error: {e}[/red]")

    def _handle_streaming_response(self, user_input: str):
        """Stream agent response with Claude Code-style display."""
        accumulated_text = ""
        plan_buffer = ""
        in_plan = False
        current_tool = None

        for event in self.agent.process_stream(user_input):
            if isinstance(event, TextDelta):
                accumulated_text += event.text
                # Detect <plan> blocks for plan-then-execute gating
                if "<plan>" in accumulated_text and not in_plan:
                    in_plan = True
                    plan_buffer = accumulated_text.split("<plan>", 1)[1]
                    # Print any text before <plan>
                    pre_plan = accumulated_text.split("<plan>", 1)[0].strip()
                    if pre_plan:
                        self.console.print(Markdown(pre_plan))
                    accumulated_text = ""
                elif in_plan:
                    if "</plan>" in event.text:
                        plan_buffer += event.text.split("</plan>")[0]
                        in_plan = False
                        self.console.print()
                        self.console.print(Panel(
                            Markdown(plan_buffer.strip()),
                            title="[bold cyan]Proposed Plan[/bold cyan]",
                            border_style="cyan",
                            padding=(1, 2),
                        ))
                        if self.scratchpad:
                            self.scratchpad.log("plan", summary="Plan proposed", data={"plan": plan_buffer.strip()})
                        if not Confirm.ask("  Execute this plan?", default=True):
                            self.console.print("[dim yellow]Plan rejected.[/dim yellow]")
                            return
                        remainder = event.text.split("</plan>", 1)[1] if "</plan>" in event.text else ""
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue
                # Print streaming text character by character for smooth output
                sys.stdout.write(event.text)
                sys.stdout.flush()

            elif isinstance(event, ToolCallStart):
                # Flush any accumulated text
                if accumulated_text.strip():
                    sys.stdout.write("\n")
                    sys.stdout.flush()
                    accumulated_text = ""
                current_tool = event.tool_name
                # Claude Code-style tool indicator: compact inline
                self.console.print(f"\n  [dim]>[/dim] [bold yellow]{event.tool_name}[/bold yellow] [dim]...[/dim]", end="")

            elif isinstance(event, ToolApprovalRequest):
                # Approval handled by callback
                pass

            elif isinstance(event, ToolCallResult):
                # Compact result indicator
                summary = event.summary if hasattr(event, 'summary') else "done"
                # Clear the "..." and show result
                self.console.print(f" [green]{summary}[/green]")
                current_tool = None

            elif isinstance(event, TurnComplete):
                if current_tool:
                    self.console.print()  # Close any pending tool line
                    current_tool = None

        # Final newline + render accumulated text as markdown
        if accumulated_text.strip():
            sys.stdout.write("\n\n")
            sys.stdout.flush()

    def _prompt_save_on_exit(self):
        """Prompt to save session if there's history."""
        if not self.agent.history:
            return
        try:
            answer = input("\n  Save this session? (y/N): ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return
        if answer == "y":
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"  [dim]Session saved: {sid}[/dim]")

    def _show_welcome(self):
        """Claude Code-style compact welcome."""
        from app import __version__
        self.console.print()
        self.console.print(f"  [bold cyan]PRISM[/bold cyan] [dim]v{__version__}[/dim]  [dim]— AI-Native Materials Discovery[/dim]")
        self.console.print(f"  [dim]Type your query or /help for commands. Ctrl+C to exit.[/dim]")
        self.console.print()

        # Show tool count
        tool_count = len(self.agent.tools.list_tools())
        self.console.print(f"  [dim]{tool_count} tools loaded[/dim]", end="")
        try:
            from app.skills.registry import load_builtin_skills
            skill_count = len(load_builtin_skills().list_skills())
            self.console.print(f"[dim] · {skill_count} skills[/dim]", end="")
        except Exception:
            pass
        if self._auto_approve:
            self.console.print(f" [dim]·[/dim] [yellow]auto-approve ON[/yellow]", end="")
        self.console.print()
        self.console.print()

    def _handle_command(self, cmd: str) -> bool:
        """Handle a slash command. Returns True to exit."""
        parts = cmd.strip().split(maxsplit=1)
        base_cmd = parts[0].lower()
        arg = parts[1].strip() if len(parts) > 1 else ""

        if base_cmd in ("/exit", "/quit"):
            self._prompt_save_on_exit()
            self.console.print("[dim]Goodbye![/dim]")
            return True
        elif base_cmd == "/clear":
            self.agent.reset()
            self.scratchpad = Scratchpad()
            self.agent.scratchpad = self.scratchpad
            self.console.print("[dim]Conversation cleared.[/dim]")
        elif base_cmd == "/help":
            self.console.print()
            for name, desc in REPL_COMMANDS.items():
                self.console.print(f"  [cyan]{name:<16}[/cyan] [dim]{desc}[/dim]")
            self.console.print()
        elif base_cmd == "/history":
            self.console.print(f"  [dim]{len(self.agent.history)} messages in history[/dim]")
        elif base_cmd == "/tools":
            self.console.print()
            for tool in self.agent.tools.list_tools():
                approval = " [yellow]*[/yellow]" if tool.requires_approval else ""
                self.console.print(f"  [green]{tool.name:<30}[/green] [dim]{tool.description[:50]}[/dim]{approval}")
            self.console.print(f"\n  [dim][yellow]*[/yellow] requires approval[/dim]")
        elif base_cmd == "/mcp":
            self._handle_mcp_status()
        elif base_cmd == "/save":
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"  [dim]Session saved: {sid}[/dim]")
        elif base_cmd == "/export":
            self._handle_export(arg if arg else None)
        elif base_cmd == "/sessions":
            self._handle_sessions()
        elif base_cmd == "/load":
            if not arg:
                self.console.print("[dim yellow]Usage: /load SESSION_ID[/dim yellow]")
            else:
                self._handle_load(arg)
        elif base_cmd == "/skill":
            self._handle_skill(arg if arg else None)
        elif base_cmd == "/plan":
            if not arg:
                self.console.print("[dim yellow]Usage: /plan <goal>[/dim yellow]")
            else:
                self._handle_plan(arg)
        elif base_cmd == "/scratchpad":
            self._handle_scratchpad()
        elif base_cmd == "/approve-all":
            self.agent.auto_approve = True
            self._auto_approve = True
            self.console.print("  [yellow]Auto-approve enabled. All tools will run without consent prompts.[/yellow]")
        else:
            self.console.print(f"  [dim yellow]Unknown command: {base_cmd}. Type /help.[/dim yellow]")
        return False

    def _handle_scratchpad(self):
        """Display the scratchpad execution log."""
        if not self.scratchpad or not self.scratchpad.entries:
            self.console.print("  [dim]Scratchpad is empty.[/dim]")
            return
        self.console.print()
        for i, entry in enumerate(self.scratchpad.entries, 1):
            tool_str = f" [green]{entry.tool_name}[/green]" if entry.tool_name else ""
            self.console.print(f"  [dim]{i}.[/dim]{tool_str} {entry.summary} [dim]{entry.timestamp}[/dim]")
        self.console.print()

    def _handle_skill(self, name: Optional[str] = None):
        """List skills or show details for a specific skill."""
        try:
            from app.skills.registry import load_builtin_skills
            skills = load_builtin_skills()
        except Exception:
            self.console.print("  [dim]No skills available.[/dim]")
            return

        self.console.print()
        if name:
            try:
                skill = skills.get(name)
            except KeyError:
                self.console.print(f"  [dim yellow]Skill not found: {name}[/dim yellow]")
                return
            self.console.print(f"  [bold cyan]{skill.name}[/bold cyan]  [dim]({skill.category})[/dim]")
            self.console.print(f"  {skill.description}")
            self.console.print()
            for i, step in enumerate(skill.steps, 1):
                opt = " [dim](optional)[/dim]" if step.optional else ""
                self.console.print(f"    {i}. [green]{step.name}[/green] — [dim]{step.description}[/dim]{opt}")
        else:
            for skill in skills.list_skills():
                self.console.print(f"  [green]{skill.name:<25}[/green] [dim]{skill.description[:55]}[/dim]")
        self.console.print()

    def _handle_plan(self, goal: str):
        """Send a planning prompt to the LLM to suggest skills."""
        prompt = (
            f"The user wants to accomplish: {goal}\n\n"
            "Available PRISM skills:\n"
        )
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

    def _handle_mcp_status(self):
        """Show MCP server connection status and tools."""
        from app.mcp_client import load_mcp_config
        config = load_mcp_config()
        self.console.print()
        self.console.print(f"  [dim]Config: {config.config_path}[/dim]")
        if not config.servers:
            self.console.print("  [dim]No MCP servers configured.[/dim]")
        else:
            self.console.print(f"  [cyan]Configured servers:[/cyan] {len(config.servers)}")
            for name in config.servers:
                self.console.print(f"    [green]{name}[/green]")
        if self._mcp_tools:
            self.console.print(f"  [cyan]Loaded MCP tools:[/cyan] {len(self._mcp_tools)}")
            for tname in self._mcp_tools:
                self.console.print(f"    [green]{tname}[/green]")
        else:
            self.console.print("  [dim]No MCP tools loaded.[/dim]")
        self.console.print()

    def _handle_export(self, filename: Optional[str] = None):
        """Find the most recent tool result with a 'results' array and export to CSV."""
        results = None
        for msg in reversed(self.agent.history):
            if msg.get("role") == "tool_result" and isinstance(msg.get("result"), dict):
                r = msg["result"]
                if isinstance(r.get("results"), list) and r["results"]:
                    results = r["results"]
                    break
        if not results:
            self.console.print("  [dim yellow]No exportable results found in conversation history.[/dim yellow]")
            return
        export_tool = self.agent.tools.get("export_results_csv")
        if export_tool is None:
            self.console.print("  [dim yellow]export_results_csv tool not available.[/dim yellow]")
            return
        kwargs = {"results": results}
        if filename:
            kwargs["filename"] = filename
        out = export_tool.execute(**kwargs)
        if "error" in out:
            self.console.print(f"  [red]Export error: {out['error']}[/red]")
        else:
            self.console.print(f"  [green]Exported {out['rows']} rows to {out['filename']}[/green]")

    def _handle_sessions(self):
        """List saved sessions."""
        sessions = self.memory.list_sessions()
        if not sessions:
            self.console.print("  [dim]No saved sessions.[/dim]")
            return
        self.console.print()
        for s in sessions[:20]:
            summary = s.get("summary", "")
            ts = s.get("timestamp", "")[:19]
            count = s.get("message_count", 0)
            self.console.print(f"  [cyan]{s['session_id']}[/cyan]  [dim]{ts}  ({count} msgs)[/dim]  {summary}")
        self.console.print()

    def _handle_load(self, session_id: str):
        """Load a saved session."""
        try:
            self._load_session(session_id)
            self.console.print(f"  [green]Session loaded: {session_id} ({len(self.agent.history)} messages)[/green]")
        except FileNotFoundError:
            self.console.print(f"  [red]Session not found: {session_id}[/red]")
        except Exception as e:
            self.console.print(f"  [red]Error loading session: {e}[/red]")
