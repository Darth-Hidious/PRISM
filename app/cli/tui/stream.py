"""Streaming event handler — bridges UIEmitter protocol events to Rich renderers.

This module is a "dumb renderer": it consumes ui.* protocol events from
UIEmitter and dispatches each to the appropriate Rich card/widget.
All presentation logic (plan detection, text accumulation, cost tracking)
lives in UIEmitter; this module only renders.
"""

from prompt_toolkit import PromptSession
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown

from app.agent.events import UsageInfo
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
    """Process a UIEmitter event stream, rendering events as Rich cards.

    Returns the updated session_cost (accumulated).
    """
    render_input_card(console, user_input)

    from app.backend.ui_emitter import UIEmitter  # lazy to avoid circular import
    emitter = UIEmitter(agent)
    emitter.session_cost = session_cost

    accumulated_text = ""
    spinner = Spinner(console=console)

    with Live("", console=console, refresh_per_second=15,
              transient=True, vertical_overflow="visible") as live:

        for event in emitter.process(user_input):
            method = event["method"]
            params = event["params"]

            if method == "ui.text.delta":
                accumulated_text += params["text"]
                live.update(Markdown(accumulated_text))

            elif method == "ui.text.flush":
                _flush_live(live, console, params["text"])
                accumulated_text = ""

            elif method == "ui.tool.start":
                # Flush any locally accumulated text before spinner
                if accumulated_text.strip():
                    _flush_live(live, console, accumulated_text)
                    accumulated_text = ""
                spinner.start(params["verb"])

            elif method == "ui.card":
                spinner.stop()
                if params["card_type"] == "plan":
                    live.update("")
                    render_plan_card(console, params["content"])
                    if scratchpad:
                        scratchpad.log(
                            "plan",
                            summary="Plan proposed",
                            data={"plan": params["content"]},
                        )
                    if not ask_plan_confirmation(session):
                        console.print("[dim]Cancelled.[/dim]")
                        return emitter.session_cost
                else:
                    render_tool_result(
                        console,
                        params["tool_name"],
                        params["content"],
                        params["elapsed_ms"],
                        params["data"],
                    )

            elif method == "ui.cost":
                usage = UsageInfo(
                    input_tokens=params["input_tokens"],
                    output_tokens=params["output_tokens"],
                )
                render_cost_line(
                    console, usage,
                    params["turn_cost"],
                    params["session_cost"],
                )

            elif method == "ui.turn.complete":
                spinner.stop()
                _flush_live(live, console, accumulated_text)
                accumulated_text = ""

            elif method == "ui.prompt":
                pass  # Approval handled by approval_callback in AgentCore

    # Flush any remaining text (safety net)
    if accumulated_text.strip():
        console.print(Markdown(accumulated_text))

    return emitter.session_cost
