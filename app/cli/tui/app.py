"""PRISM interactive REPL — main loop.

Composes prompt, cards, status, welcome, and streaming modules.
No business logic here — just the UI event loop.
"""

from typing import Optional

from rich.console import Console

from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.memory import SessionMemory
from app.agent.scratchpad import Scratchpad
from app.tools.base import ToolRegistry

from app.cli.tui.prompt import (
    create_prompt_session, get_user_input, ask_approval, ask_save_on_exit,
)
from app.cli.tui.welcome import show_welcome
from app.cli.tui.status import render_status_line
from app.cli.tui.stream import handle_streaming_response
from app.cli.slash.handlers import handle_command


class AgentREPL:
    """Interactive REPL — Rich panels + prompt_toolkit."""

    def __init__(
        self,
        backend: Backend,
        system_prompt: Optional[str] = None,
        tools: Optional[ToolRegistry] = None,
        enable_mcp: bool = True,
        auto_approve: bool = False,
    ):
        self.console = Console(highlight=False)
        self.memory = SessionMemory()
        self.scratchpad = Scratchpad()
        self._mcp_tools: list[str] = []
        self._auto_approve = auto_approve
        self._auto_approve_tools: set = set()

        if tools is None:
            from app.plugins.bootstrap import build_full_registry
            tools, _provider_reg, _agent_reg = build_full_registry(enable_mcp=enable_mcp)

        self.session = create_prompt_session()

        self.agent = AgentCore(
            backend=backend,
            tools=tools,
            system_prompt=system_prompt,
            approval_callback=self._approval_callback,
            auto_approve=auto_approve,
        )
        self.agent.scratchpad = self.scratchpad

    # ── Approval (uses prompt_toolkit — no more bare input()) ─────

    def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
        return ask_approval(
            self.session, self.console,
            tool_name, tool_args,
            self._auto_approve_tools,
        )

    # ── Session management ────────────────────────────────────────

    def load_session(self, session_id: str):
        self.memory.load(session_id)
        self.agent.history = list(self.memory.get_history())
        entries = self.memory.get_scratchpad_entries()
        if entries:
            self.scratchpad = Scratchpad.from_dict(entries)
            self.agent.scratchpad = self.scratchpad

    def prompt_save_on_exit(self):
        if not self.agent.history:
            return
        if ask_save_on_exit(self.console, self.session):
            self.memory.set_history(self.agent.history)
            if self.scratchpad:
                self.memory.set_scratchpad_entries(self.scratchpad.to_dict())
            sid = self.memory.save()
            self.console.print(f"[dim]Saved: {sid}[/dim]")

    # ── Main loop ─────────────────────────────────────────────────

    def run(self):
        show_welcome(self.console, self.agent, self._auto_approve)

        while True:
            try:
                render_status_line(
                    self.console, self.agent, self._auto_approve,
                )
                user_input = get_user_input(self.session)
            except (EOFError, KeyboardInterrupt):
                self.prompt_save_on_exit()
                self.console.print("\n[dim]Goodbye.[/dim]")
                break

            if not user_input:
                continue

            if user_input.startswith("/"):
                if handle_command(self, user_input):
                    break
                continue

            try:
                handle_streaming_response(
                    self.console, self.agent, user_input,
                    self.session, self.scratchpad,
                )
            except KeyboardInterrupt:
                self.console.print("\n[dim]Interrupted.[/dim]")
            except Exception as e:
                self.console.print(f"\n[red]Error: {e}[/red]")
