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

from app.cli.tui.theme import PRIMARY, ACCENT_MAGENTA, WARNING
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


def get_user_input(session: PromptSession) -> str:
    """Get input using the styled PRISM prompt."""
    return session.prompt(
        HTML(f'<style fg="{PRIMARY}"><b>\u276f </b></style>'),
    ).strip()


def ask_approval(session: PromptSession, console: Console,
                 tool_name: str, tool_args: dict,
                 auto_approve_tools: set) -> bool:
    """Ask for tool approval using prompt_toolkit (not bare input()).

    Returns True if approved. Mutates auto_approve_tools on 'a'.
    """
    if tool_name in auto_approve_tools:
        return True

    render_approval_card(console, tool_name, tool_args)

    try:
        answer = session.prompt(
            HTML(f'<style fg="{WARNING}">  \u203a </style>'),
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
