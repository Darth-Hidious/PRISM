"""Streaming event handler — bridges AgentCore events to card renderers."""

import time
from prompt_toolkit import PromptSession
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown

from app.agent.events import (
    TextDelta, ToolCallStart, ToolCallResult, TurnComplete,
    ToolApprovalRequest,
)
from app.agent.scratchpad import Scratchpad
from app.cli.tui.cards import (
    render_input_card,
    render_plan_card, render_tool_result, render_cost_line,
)
from app.cli.tui.prompt import ask_plan_confirmation
from app.cli.tui.spinner import Spinner


def _flush_live(live: Live, console: Console, text: str):
    """Freeze streamed text: stop Live update, print permanently."""
    live.update("")
    if text.strip():
        console.print(Markdown(text))


def handle_streaming_response(
    console: Console,
    agent,
    user_input: str,
    session: PromptSession,
    scratchpad: Scratchpad | None = None,
    session_cost: float = 0.0,
) -> float:
    """Process an agent stream, rendering events as Rich cards.

    Returns the updated session_cost (accumulated).
    """
    render_input_card(console, user_input)

    accumulated_text = ""
    plan_buffer = ""
    in_plan = False
    tool_start_time = None
    spinner = Spinner(console=console)

    with Live("", console=console, refresh_per_second=15,
              vertical_overflow="visible") as live:

        for event in agent.process_stream(user_input):
            if isinstance(event, TextDelta):
                accumulated_text += event.text

                # ── Plan tag detection ──
                if "<plan>" in accumulated_text and not in_plan:
                    in_plan = True
                    plan_buffer = accumulated_text.split("<plan>", 1)[1]
                    pre = accumulated_text.split("<plan>", 1)[0].strip()
                    if pre:
                        _flush_live(live, console, pre)
                    accumulated_text = ""
                    continue
                elif in_plan:
                    if "</plan>" in event.text:
                        plan_buffer += event.text.split("</plan>")[0]
                        in_plan = False
                        live.update("")
                        render_plan_card(console, plan_buffer.strip())
                        if scratchpad:
                            scratchpad.log(
                                "plan",
                                summary="Plan proposed",
                                data={"plan": plan_buffer.strip()},
                            )
                        if not ask_plan_confirmation(session):
                            console.print("[dim]Cancelled.[/dim]")
                            return session_cost
                        remainder = (
                            event.text.split("</plan>", 1)[1]
                            if "</plan>" in event.text
                            else ""
                        )
                        accumulated_text = remainder
                    else:
                        plan_buffer += event.text
                    continue

                # ── Live-update streaming text ──
                if not in_plan:
                    live.update(Markdown(accumulated_text))

            elif isinstance(event, ToolCallStart):
                _flush_live(live, console, accumulated_text)
                accumulated_text = ""
                tool_start_time = time.monotonic()
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

            elif isinstance(event, TurnComplete):
                spinner.stop()
                _flush_live(live, console, accumulated_text)
                accumulated_text = ""
                tool_start_time = None
                # Cost line
                if event.usage:
                    turn_cost = event.estimated_cost
                    if turn_cost is not None:
                        session_cost += turn_cost
                    render_cost_line(console, event.usage, turn_cost, session_cost)

    # Flush any remaining text (safety net)
    if accumulated_text.strip():
        console.print(Markdown(accumulated_text))

    return session_cost
