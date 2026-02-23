"""Interactive REPL for the PRISM agent."""
from typing import Optional
from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel
from app.agent.backends.base import Backend
from app.agent.core import AgentCore
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
    "/save": "Save current session",
}


class AgentREPL:
    """Interactive REPL for conversational agent interaction."""

    def __init__(self, backend: Backend, system_prompt: Optional[str] = None, tools: Optional[ToolRegistry] = None):
        self.console = Console()
        self.memory = SessionMemory()
        if tools is None:
            tools = ToolRegistry()
            create_system_tools(tools)
            create_data_tools(tools)
            create_visualization_tools(tools)
        self.agent = AgentCore(backend=backend, tools=tools, system_prompt=system_prompt)

    def run(self):
        """Main REPL loop."""
        self._show_welcome()
        while True:
            try:
                user_input = input("\n> ").strip()
            except (EOFError, KeyboardInterrupt):
                self.console.print("\nGoodbye!")
                break
            if not user_input:
                continue
            if user_input.startswith("/"):
                if self._handle_command(user_input):
                    break
                continue
            try:
                with self.console.status("[bold green]Thinking..."):
                    response = self.agent.process(user_input)
                if response:
                    self.console.print()
                    self.console.print(Markdown(response))
            except Exception as e:
                self.console.print(f"[red]Error: {e}[/red]")

    def _show_welcome(self):
        self.console.print(Panel.fit(
            "[bold cyan]PRISM[/bold cyan] — Materials Science Research Agent\n"
            "Type your question or /help for commands.",
            border_style="cyan"))

    def _handle_command(self, cmd: str) -> bool:
        """Handle a slash command. Returns True to exit."""
        cmd = cmd.lower().strip()
        if cmd in ("/exit", "/quit"):
            self.console.print("Goodbye!")
            return True
        elif cmd == "/clear":
            self.agent.reset()
            self.console.print("[dim]Conversation cleared.[/dim]")
        elif cmd == "/help":
            for name, desc in REPL_COMMANDS.items():
                self.console.print(f"  [cyan]{name}[/cyan]  {desc}")
        elif cmd == "/history":
            self.console.print(f"[dim]History: {len(self.agent.history)} messages[/dim]")
        elif cmd == "/tools":
            for tool in self.agent.tools.list_tools():
                self.console.print(f"  [green]{tool.name}[/green]  {tool.description[:60]}")
        elif cmd == "/save":
            self.memory.set_history(self.agent.history)
            sid = self.memory.save()
            self.console.print(f"[dim]Session saved: {sid}[/dim]")
        else:
            self.console.print(f"[yellow]Unknown command: {cmd}. Type /help.[/yellow]")
        return False
