"""Main PRISM Textual application."""
import asyncio
import json
import threading
import time

from textual.app import App, ComposeResult
from textual.binding import Binding

from app.tui.widgets.header import HeaderWidget
from app.tui.widgets.stream import StreamView
from app.tui.widgets.cards import (
    InputCard, OutputCard, ToolCard, ApprovalCard, PlanCard, detect_card_type,
)
from app.tui.widgets.status_bar import StatusBar
from app.tui.widgets.input_bar import InputBar
from app.tui.screens.overlay import FullContentScreen
from app.tui.keymap import KEYMAP, BINDING_DESCRIPTIONS
from app.tui.config import TUIConfig
from app.tui.theme import SURFACE


class PrismApp(App):
    """PRISM AI Materials Discovery — Textual TUI."""

    CSS = f"""
    Screen {{
        background: {SURFACE};
    }}
    """

    BINDINGS = [
        Binding("ctrl+o", "expand_content", BINDING_DESCRIPTIONS.get("ctrl+o", "")),
        Binding("ctrl+q", "view_task_queue", BINDING_DESCRIPTIONS.get("ctrl+q", "")),
        Binding("ctrl+s", "save_session", BINDING_DESCRIPTIONS.get("ctrl+s", "")),
        Binding("ctrl+l", "clear_stream", BINDING_DESCRIPTIONS.get("ctrl+l", "")),
        Binding("ctrl+p", "toggle_plan_mode", BINDING_DESCRIPTIONS.get("ctrl+p", "")),
        Binding("ctrl+t", "list_tools", BINDING_DESCRIPTIONS.get("ctrl+t", "")),
        Binding("ctrl+d", "exit_app", BINDING_DESCRIPTIONS.get("ctrl+d", "")),
    ]

    def __init__(self, backend=None, enable_mcp: bool = True,
                 auto_approve: bool = False, config: TUIConfig | None = None,
                 **kwargs):
        super().__init__(**kwargs)
        self.config = config or TUIConfig()
        self._backend = backend
        self._enable_mcp = enable_mcp
        self._auto_approve = auto_approve
        self._agent = None  # Lazy init — needs backend
        self._approval_event = threading.Event()
        self._approval_result = False

    def compose(self) -> ComposeResult:
        yield HeaderWidget()
        yield StreamView()
        yield StatusBar(max_visible_tasks=self.config.max_status_tasks)
        yield InputBar()

    def on_mount(self) -> None:
        """Focus the input bar on startup."""
        self.query_one(InputBar).focus()
        # Start spinner timer (12.5 fps)
        self.set_interval(0.08, self._tick_spinner)

    def _tick_spinner(self) -> None:
        """Advance the spinner animation frame."""
        status = self.query_one(StatusBar)
        if status.is_thinking:
            status.advance_spinner()

    def _init_agent(self) -> None:
        """Lazily initialize AgentCore with tool registry and approval callback."""
        if self._agent is not None:
            return
        from app.plugins.bootstrap import build_full_registry
        from app.agent.core import AgentCore
        from app.agent.scratchpad import Scratchpad
        from app.agent.memory import SessionMemory

        tools = build_full_registry(enable_mcp=self._enable_mcp)
        self._memory = SessionMemory()
        self._scratchpad = Scratchpad()
        self._agent = AgentCore(
            backend=self._backend,
            tools=tools,
            approval_callback=self._approval_callback,
            auto_approve=self._auto_approve,
        )
        self._agent.scratchpad = self._scratchpad

    async def on_input_submitted(self, event: InputBar.Submitted) -> None:
        """Handle user input submission."""
        message = event.value.strip()
        if not message:
            return

        input_bar = self.query_one(InputBar)
        input_bar.value = ""

        stream = self.query_one(StreamView)
        stream.resume_auto_scroll()

        # Handle / commands
        if message.startswith("/"):
            await self._handle_command(message)
            return

        # Add input card to stream
        stream.add_card(InputCard(message))

        # Process with agent (in worker to avoid blocking)
        if self._backend:
            self.run_worker(self._process_agent_message(message))

    async def _process_agent_message(self, message: str) -> None:
        """Run agent.process_stream() and map events to cards."""
        from app.agent.events import (
            TextDelta, ToolCallStart, ToolCallResult,
            TurnComplete, ToolApprovalRequest,
        )
        from app.agent.spinner import TOOL_VERBS

        self._init_agent()

        stream = self.query_one(StreamView)
        status = self.query_one(StatusBar)
        accumulated_text = ""
        tool_start_time = 0.0

        try:
            status.update_agent_step("Thinking...")
            for event in self._agent.process_stream(message):
                if isinstance(event, TextDelta):
                    accumulated_text += event.text

                elif isinstance(event, ToolCallStart):
                    # Flush accumulated text
                    if accumulated_text.strip():
                        stream.add_card(OutputCard(
                            accumulated_text.strip(),
                            truncation_lines=self.config.truncation_lines,
                        ))
                        accumulated_text = ""
                    # Update agent status
                    verb = TOOL_VERBS.get(event.tool_name, "Thinking...")
                    status.update_agent_step(verb)
                    tool_start_time = time.time()

                elif isinstance(event, ToolCallResult):
                    elapsed = (time.time() - tool_start_time) * 1000
                    status.stop_thinking()
                    stream.add_card(ToolCard(
                        tool_name=event.tool_name,
                        elapsed_ms=elapsed,
                        summary=event.summary,
                        result=event.result if isinstance(event.result, dict) else {},
                    ))

                elif isinstance(event, ToolApprovalRequest):
                    stream.add_card(ApprovalCard(
                        tool_name=event.tool_name,
                        tool_args=event.tool_args,
                    ))

                elif isinstance(event, TurnComplete):
                    if accumulated_text.strip():
                        stream.add_card(OutputCard(
                            accumulated_text.strip(),
                            truncation_lines=self.config.truncation_lines,
                        ))
                        accumulated_text = ""
                    status.stop_thinking()

        except Exception as e:
            stream.add_card(OutputCard(f"Error: {e}"))
            status.stop_thinking()

    def _approval_callback(self, tool_name: str, tool_args: dict) -> bool:
        """Called by AgentCore (from worker thread) when a tool needs approval."""
        self._approval_event.clear()
        self._approval_result = False
        # Post card to main thread
        self.call_from_thread(self._show_approval_card, tool_name, tool_args)
        # Block worker until user responds
        self._approval_event.wait()
        return self._approval_result

    def _show_approval_card(self, tool_name: str, tool_args: dict) -> None:
        stream = self.query_one(StreamView)
        card = ApprovalCard(tool_name=tool_name, tool_args=tool_args)
        stream.add_card(card)
        card.focus()

    def _resolve_approval(self, approved: bool) -> None:
        self._approval_result = approved
        self._approval_event.set()

    async def _handle_command(self, command: str) -> None:
        """Handle / commands."""
        stream = self.query_one(StreamView)
        cmd = command.split()[0].lower()

        if cmd in ("/exit", "/quit"):
            self.exit()
        elif cmd == "/clear":
            stream.remove_children()
        elif cmd == "/help":
            from app.agent.repl import REPL_COMMANDS
            help_text = "**Commands:**\n"
            for c, desc in REPL_COMMANDS.items():
                help_text += f"- `{c}` — {desc}\n"
            help_text += "\n**Key Bindings:**\n"
            for key, desc in BINDING_DESCRIPTIONS.items():
                help_text += f"- `{key}` — {desc}\n"
            stream.add_card(OutputCard(help_text))
        else:
            stream.add_card(OutputCard(f"Unknown command: {cmd}"))

    # --- Actions ---

    def action_expand_content(self) -> None:
        """Open modal overlay for focused card."""
        focused = self.focused
        content = ""
        title = ""
        if hasattr(focused, "full_content"):
            content = focused.full_content
            title = "output"
        elif hasattr(focused, "result"):
            content = json.dumps(focused.result, indent=2, default=str)
            title = getattr(focused, "tool_name", "details")
        elif hasattr(focused, "plan_text"):
            content = focused.plan_text
            title = "plan"
        if content:
            self.push_screen(FullContentScreen(content, title=title))

    def action_view_task_queue(self) -> None:
        status = self.query_one(StatusBar)
        lines = []
        for task in status.tasks:
            icon = {"done": "\u2714", "running": "\u25b8", "pending": "\u25cb"}[task["status"]]
            lines.append(f"{icon} {task['label']}")
        content = "\n".join(lines) if lines else "No tasks."
        self.push_screen(FullContentScreen(content, title="Task Queue"))

    def action_save_session(self) -> None:
        pass  # Wired in Task 11

    def action_clear_stream(self) -> None:
        self.query_one(StreamView).remove_children()

    def action_toggle_plan_mode(self) -> None:
        pass  # Wired in Task 11

    def action_list_tools(self) -> None:
        pass  # Wired in Task 11

    def action_exit_app(self) -> None:
        self.exit()
