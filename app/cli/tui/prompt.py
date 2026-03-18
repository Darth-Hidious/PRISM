"""Prompt handling — all user input goes through prompt_toolkit.

This fixes the broken approval/plan prompts that used bare input()
or Rich Confirm.ask(), which fight with prompt_toolkit for stdin.
"""

import os
from prompt_toolkit import PromptSession
from prompt_toolkit.history import FileHistory
from prompt_toolkit.auto_suggest import AutoSuggestFromHistory
from prompt_toolkit.formatted_text import HTML
from rich.console import Console

from app.cli.tui.theme import PRIMARY, ACCENT_MAGENTA, WARNING, CRYSTAL_INNER, CRYSTAL_OUTER_DIM, MUTED
from app.cli.tui.cards import render_approval_card


def create_prompt_session() -> PromptSession:
    """Create a prompt_toolkit session with history."""
    history_path = os.path.expanduser("~/.prism/repl_history")
    os.makedirs(os.path.dirname(history_path), exist_ok=True)
    return PromptSession(
        history=FileHistory(history_path),
        auto_suggest=AutoSuggestFromHistory(),
        multiline=False,
        enable_history_search=True,
    )


def print_top_separator(console: Console):
    """Print crystal-themed separator line above the prompt."""
    width = console.width
    from rich.text import Text
    sep = Text()
    sep.append("\u25c8", style=CRYSTAL_INNER)
    sep.append("\u2500" * max(width - 1, 10), style=CRYSTAL_OUTER_DIM)
    console.print(sep)


def print_bottom_separator(console: Console):
    """Print subtle dotted separator line below the prompt."""
    width = console.width
    console.print(f"[{MUTED}]{chr(0x2508) * width}[/{MUTED}]")


def get_user_input(session: PromptSession, console: Console | None = None) -> str:
    """Get input using the styled PRISM prompt.

    If console is provided, prints crystal separator above the prompt.
    """
    if console:
        print_top_separator(console)
    result = session.prompt(
        HTML(f'<style fg="{PRIMARY}"><b>\u2b21 </b></style>'),
    ).strip()
    if console:
        print_bottom_separator(console)
    return result


def ask_approval(session: PromptSession, console: Console,
                 tool_name: str, tool_args: dict,
                 auto_approve_tools: set) -> bool:
    """Ask for tool approval with OpenCode-style permission prompt.

    Returns True if approved. Mutates auto_approve_tools on 'a'.
    """
    if tool_name in auto_approve_tools:
        return True

    render_approval_card(console, tool_name, tool_args)

    try:
        answer = session.prompt(
            HTML(f'<style fg="{WARNING}">\u2503 \u203a </style>'),
        ).strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False

    if answer == "a":
        auto_approve_tools.add(tool_name)
        return True
    return answer in ("y", "yes", "")


def ask_plan_confirmation(session: PromptSession) -> bool:
    """Ask whether to execute a proposed plan.

    Uses prompt_toolkit so it doesn't fight with the main session.
    """
    try:
        answer = session.prompt(
            HTML(f'<style fg="{ACCENT_MAGENTA}">  Execute? (y/n) \u203a </style>'),
        ).strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False
    return answer in ("y", "yes", "")


def ask_save_on_exit(console: Console, session: PromptSession) -> bool:
    """Ask whether to save session before exiting."""
    try:
        answer = session.prompt(
            HTML('<style fg="ansiwhite">Save session? (y/N) </style>'),
        ).strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False
    return answer == "y"
