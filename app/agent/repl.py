"""Interactive REPL for the PRISM agent."""
from typing import Optional
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text
from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.events import TextDelta, ToolCallStart, ToolCallResult, TurnComplete
from app.agent.memory import SessionMemory
from app.tools.base import ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools


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
}


class AgentREPL:
    """Interactive REPL for conversational agent interaction."""

    def __init__(self, backend: Backend, system_prompt: Optional[str] = None,
                 tools: Optional[ToolRegistry] = None, enable_mcp: bool = True):
        self.console = Console()
        self.memory = SessionMemory()
        self._mcp_tools: list[str] = []
        if tools is None:
            tools = ToolRegistry()
            create_system_tools(tools)
            create_data_tools(tools)
            create_visualization_tools(tools)
            try:
                from app.simulation.bridge import check_pyiron_available
                if check_pyiron_available():
                    from app.tools.simulation import create_simulation_tools
                    create_simulation_tools(tools)
            except Exception:
                pass
        # Optionally discover and register tools from external MCP servers
        if enable_mcp:
            try:
                from app.mcp_client import discover_and_register_mcp_tools
                self._mcp_tools = discover_and_register_mcp_tools(tools)
                if self._mcp_tools:
                    self.console.print(f"[dim]Loaded {len(self._mcp_tools)} tools from MCP servers[/dim]")
            except Exception:
                pass  # MCP client not available or no config
        self.agent = AgentCore(backend=backend, tools=tools, system_prompt=system_prompt)

    def _load_session(self, session_id: str):
        """Restore a saved session into the agent."""
        self.memory.load(session_id)
        self.agent.history = list(self.memory.get_history())

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
        with Live("", console=self.console, refresh_per_second=15, vertical_overflow="visible") as live:
            for event in self.agent.process_stream(user_input):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text
                    live.update(Text(accumulated_text))
                elif isinstance(event, ToolCallStart):
                    # Print tool panel permanently, then reset live display
                    live.update("")
                    self.console.print(Panel(
                        f"[dim]Calling...[/dim]",
                        title=f"[bold yellow]{event.tool_name}[/bold yellow]",
                        border_style="yellow",
                        expand=False,
                    ))
                    accumulated_text = ""
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
        else:
            self.console.print(f"[yellow]Unknown command: {base_cmd}. Type /help.[/yellow]")
        return False

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
