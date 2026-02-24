"""Interactive REPL for the PRISM agent."""
from typing import Optional
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.panel import Panel
from rich.prompt import Confirm
from rich.text import Text
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
    """Interactive REPL for conversational agent interaction."""

    def __init__(self, backend: Backend, system_prompt: Optional[str] = None,
                 tools: Optional[ToolRegistry] = None, enable_mcp: bool = True):
        self.console = Console()
        self.memory = SessionMemory()
        self.scratchpad = Scratchpad()
        self._mcp_tools: list[str] = []
        if tools is None:
            from app.plugins.bootstrap import build_full_registry
            tools = build_full_registry(enable_mcp=enable_mcp)
        self.agent = AgentCore(
            backend=backend, tools=tools, system_prompt=system_prompt,
            approval_callback=self._approval_callback, auto_approve=False,
        )
        self.agent.scratchpad = self.scratchpad

    def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
        """Ask the user for approval before running an expensive tool."""
        args_summary = ", ".join(f"{k}={v!r}" for k, v in list(tool_args.items())[:3])
        return Confirm.ask(f"  [yellow]Approve {tool_name}({args_summary})?[/yellow]", default=True)

    def _load_session(self, session_id: str):
        """Restore a saved session into the agent."""
        self.memory.load(session_id)
        self.agent.history = list(self.memory.get_history())
        # Restore scratchpad if saved
        entries = self.memory.get_scratchpad_entries()
        if entries:
            self.scratchpad = Scratchpad.from_dict(entries)
            self.agent.scratchpad = self.scratchpad

    def run(self):
        """Main REPL loop."""
        self._show_welcome()
        while True:
            try:
                user_input = input("\n> ").strip()
            except (EOFError, KeyboardInterrupt):
                self._prompt_save_on_exit()
                self.console.print("\nGoodbye!")
                break
            if not user_input:
                continue
            if user_input.startswith("/"):
                if self._handle_command(user_input):
                    break
                continue
            try:
                self._handle_streaming_response(user_input)
            except Exception as e:
                self.console.print(f"[red]Error: {e}[/red]")

    def _handle_streaming_response(self, user_input: str):
        """Stream agent response with Rich Live display."""
        accumulated_text = ""
        plan_buffer = ""
        in_plan = False
        with Live("", console=self.console, refresh_per_second=15, vertical_overflow="visible") as live:
            for event in self.agent.process_stream(user_input):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    # Detect <plan> blocks for plan-then-execute gating
                    if "<plan>" in accumulated_text and not in_plan:
                        in_plan = True
                        plan_buffer = accumulated_text.split("<plan>", 1)[1]
                        accumulated_text = accumulated_text.split("<plan>", 1)[0]
                    elif in_plan:
                        if "</plan>" in event.text:
                            plan_buffer += event.text.split("</plan>")[0]
                            in_plan = False
                            # Show plan and ask for approval
                            live.update("")
                            self.console.print(Panel(
                                plan_buffer.strip(),
                                title="[bold cyan]Proposed Plan[/bold cyan]",
                                border_style="cyan",
                            ))
                            if self.scratchpad:
                                self.scratchpad.log("plan", summary="Plan proposed", data={"plan": plan_buffer.strip()})
                            if not Confirm.ask("  Execute this plan?", default=True):
                                self.console.print("[yellow]Plan rejected. Stopping.[/yellow]")
                                return
                            # Continue after plan approval
                            remainder = event.text.split("</plan>", 1)[1] if "</plan>" in event.text else ""
                            accumulated_text += remainder
                        else:
                            plan_buffer += event.text
                        continue
                    live.update(Text(accumulated_text))
                elif isinstance(event, ToolCallStart):
                    live.update("")
                    self.console.print(Panel(
                        f"[dim]Calling...[/dim]",
                        title=f"[bold yellow]{event.tool_name}[/bold yellow]",
                        border_style="yellow",
                        expand=False,
                    ))
                    accumulated_text = ""
                elif isinstance(event, ToolApprovalRequest):
                    live.update("")
                    # Approval is handled by the callback in AgentCore
                elif isinstance(event, ToolCallResult):
                    self.console.print(Panel(
                        f"[green]{event.summary}[/green]",
                        title=f"[bold green]{event.tool_name}[/bold green]",
                        border_style="green",
                        expand=False,
                    ))
                elif isinstance(event, TurnComplete):
                    live.update("")
        # Render final text as Markdown
        if accumulated_text:
            self.console.print()
            self.console.print(Markdown(accumulated_text))

    def _prompt_save_on_exit(self):
        """Prompt to save session if there's history."""
        if not self.agent.history:
            return
        try:
            answer = input("Save this session? (y/N): ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            return
        if answer == "y":
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"[dim]Session saved: {sid}[/dim]")

    def _show_welcome(self):
        self.console.print(Panel.fit(
            "[bold cyan]PRISM[/bold cyan] — Materials Science Research Agent\n"
            "Type your question or /help for commands.",
            border_style="cyan"))

    def _handle_command(self, cmd: str) -> bool:
        """Handle a slash command. Returns True to exit."""
        parts = cmd.strip().split(maxsplit=1)
        base_cmd = parts[0].lower()
        arg = parts[1].strip() if len(parts) > 1 else ""

        if base_cmd in ("/exit", "/quit"):
            self._prompt_save_on_exit()
            self.console.print("Goodbye!")
            return True
        elif base_cmd == "/clear":
            self.agent.reset()
            self.console.print("[dim]Conversation cleared.[/dim]")
        elif base_cmd == "/help":
            for name, desc in REPL_COMMANDS.items():
                self.console.print(f"  [cyan]{name}[/cyan]  {desc}")
        elif base_cmd == "/history":
            self.console.print(f"[dim]History: {len(self.agent.history)} messages[/dim]")
        elif base_cmd == "/tools":
            for tool in self.agent.tools.list_tools():
                self.console.print(f"  [green]{tool.name}[/green]  {tool.description[:60]}")
        elif base_cmd == "/mcp":
            self._handle_mcp_status()
        elif base_cmd == "/save":
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"[dim]Session saved: {sid}[/dim]")
        elif base_cmd == "/export":
            self._handle_export(arg if arg else None)
        elif base_cmd == "/sessions":
            self._handle_sessions()
        elif base_cmd == "/load":
            if not arg:
                self.console.print("[yellow]Usage: /load SESSION_ID[/yellow]")
            else:
                self._handle_load(arg)
        elif base_cmd == "/skill":
            self._handle_skill(arg if arg else None)
        elif base_cmd == "/plan":
            if not arg:
                self.console.print("[yellow]Usage: /plan <goal>[/yellow]")
            else:
                self._handle_plan(arg)
        elif base_cmd == "/scratchpad":
            self._handle_scratchpad()
        elif base_cmd == "/approve-all":
            self.agent.auto_approve = True
            self.console.print("[yellow]Auto-approve enabled. All tools will run without consent prompts.[/yellow]")
        else:
            self.console.print(f"[yellow]Unknown command: {base_cmd}. Type /help.[/yellow]")
        return False

    def _handle_scratchpad(self):
        """Display the scratchpad execution log."""
        if not self.scratchpad or not self.scratchpad.entries:
            self.console.print("[dim]Scratchpad is empty.[/dim]")
            return
        md = self.scratchpad.to_markdown()
        self.console.print(Panel(Markdown(md), title="[bold cyan]Scratchpad[/bold cyan]", border_style="cyan"))

    def _handle_skill(self, name: Optional[str] = None):
        """List skills or show details for a specific skill."""
        try:
            from app.skills.registry import load_builtin_skills
            skills = load_builtin_skills()
        except Exception:
            self.console.print("[dim]No skills available.[/dim]")
            return

        if name:
            try:
                skill = skills.get(name)
            except KeyError:
                self.console.print(f"[yellow]Skill not found: {name}[/yellow]")
                return
            self.console.print(f"[bold cyan]{skill.name}[/bold cyan]  ({skill.category})")
            self.console.print(f"  {skill.description}")
            self.console.print(f"\n  [bold]Steps:[/bold]")
            for i, step in enumerate(skill.steps, 1):
                opt = " [dim](optional)[/dim]" if step.optional else ""
                self.console.print(f"    {i}. [green]{step.name}[/green] — {step.description}{opt}")
        else:
            for skill in skills.list_skills():
                self.console.print(f"  [green]{skill.name}[/green]  {skill.description[:60]}")

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
        self.console.print(f"[dim]Config: {config.config_path}[/dim]")
        if not config.servers:
            self.console.print("[dim]No MCP servers configured.[/dim]")
        else:
            self.console.print(f"[cyan]Configured servers:[/cyan] {len(config.servers)}")
            for name in config.servers:
                self.console.print(f"  [green]{name}[/green]")
        if self._mcp_tools:
            self.console.print(f"[cyan]Loaded MCP tools:[/cyan] {len(self._mcp_tools)}")
            for tname in self._mcp_tools:
                self.console.print(f"  [green]{tname}[/green]")
        else:
            self.console.print("[dim]No MCP tools loaded.[/dim]")

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
            self.console.print("[yellow]No exportable results found in conversation history.[/yellow]")
            return
        export_tool = self.agent.tools.get("export_results_csv")
        if export_tool is None:
            self.console.print("[yellow]export_results_csv tool not available.[/yellow]")
            return
        kwargs = {"results": results}
        if filename:
            kwargs["filename"] = filename
        out = export_tool.execute(**kwargs)
        if "error" in out:
            self.console.print(f"[red]Export error: {out['error']}[/red]")
        else:
            self.console.print(f"[green]Exported {out['rows']} rows to {out['filename']}[/green]")

    def _handle_sessions(self):
        """List saved sessions."""
        sessions = self.memory.list_sessions()
        if not sessions:
            self.console.print("[dim]No saved sessions.[/dim]")
            return
        for s in sessions[:20]:
            summary = s.get("summary", "")
            ts = s.get("timestamp", "")[:19]
            count = s.get("message_count", 0)
            self.console.print(f"  [cyan]{s['session_id']}[/cyan]  {ts}  ({count} msgs)  {summary}")

    def _handle_load(self, session_id: str):
        """Load a saved session."""
        try:
            self._load_session(session_id)
            self.console.print(f"[green]Session loaded: {session_id} ({len(self.agent.history)} messages)[/green]")
        except FileNotFoundError:
            self.console.print(f"[red]Session not found: {session_id}[/red]")
        except Exception as e:
            self.console.print(f"[red]Error loading session: {e}[/red]")
