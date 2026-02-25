"""Streaming event handler — bridges AgentCore events to card renderers."""

import time
from prompt_toolkit import PromptSession
from rich.console import Console
from rich.markdown import Markdown

from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest,
)
from app.agent.scratchpad import Scratchpad
from app.cli.tui.cards import (
    render_input_card, render_output_card,
    render_plan_card, render_tool_result,
)
from app.cli.tui.prompt import ask_plan_confirmation
from app.cli.tui.spinner import Spinner


def handle_streaming_response(
    console: Console,
    agent,
    user_input: str,
    session: PromptSession,
    scratchpad: Scratchpad | None = None,
):
    """Process an agent stream, rendering events as Rich cards."""
    render_input_card(console, user_input)

    accumulated_text = ""
    plan_buffer = ""
    in_plan = False
    tool_start_time = None
    current_tool_name = None
    spinner = Spinner(console=console)

    for event in agent.process_stream(user_input):
        if isinstance(event, TextDelta):
            accumulated_text += event.text
            if "<plan>" in accumulated_text and not in_plan:
                in_plan = True
                plan_buffer = accumulated_text.split("<plan>", 1)[1]
                pre = accumulated_text.split("<plan>", 1)[0].strip()
                if pre:
                    console.print(Markdown(pre))
                accumulated_text = ""
            elif in_plan:
                if "</plan>" in event.text:
                    plan_buffer += event.text.split("</plan>")[0]
                    in_plan = False
                    render_plan_card(console, plan_buffer.strip())
                    if scratchpad:
                        scratchpad.log(
                            "plan",
                            summary="Plan proposed",
                            data={"plan": plan_buffer.strip()},
                        )
                    if not ask_plan_confirmation(session):
                        console.print("[dim]Cancelled.[/dim]")
                        return
                    remainder = (
                        event.text.split("</plan>", 1)[1]
                        if "</plan>" in event.text
                        else ""
                    )
                    accumulated_text = remainder
                else:
                    plan_buffer += event.text
                continue

        elif isinstance(event, ToolCallStart):
            if accumulated_text.strip():
                render_output_card(console, accumulated_text.strip())
                accumulated_text = ""
            tool_start_time = time.monotonic()
            current_tool_name = event.tool_name
            verb = spinner.verb_for_tool(event.tool_name)
            spinner.start(verb)

        elif isinstance(event, ToolApprovalRequest):
            pass  # Handled by approval_callback

        elif isinstance(event, ToolCallResult):
            spinner.stop()
            elapsed_ms = 0.0
            if tool_start_time:
                elapsed_ms = (time.monotonic() - tool_start_time) * 1000
                tool_start_time = None
            result = event.result if isinstance(event.result, dict) else {}
            render_tool_result(
                console, event.tool_name, event.summary, elapsed_ms, result,
            )
            current_tool_name = None

        elif isinstance(event, TurnComplete):
            spinner.stop()
            tool_start_time = None

    # Flush remaining text
    if accumulated_text.strip():
        render_output_card(console, accumulated_text.strip())
