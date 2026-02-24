"""Key binding definitions for the PRISM TUI.

Global bindings apply everywhere. Card actions apply when a
specific card type is focused.
"""

# Global key bindings: key -> action name
KEYMAP = {
    "ctrl+o": "expand_content",
    "ctrl+q": "view_task_queue",
    "ctrl+s": "save_session",
    "ctrl+l": "clear_stream",
    "ctrl+p": "toggle_plan_mode",
    "ctrl+t": "list_tools",
    "ctrl+c": "cancel_operation",
    "ctrl+d": "exit_app",
    "escape": "dismiss_modal",
}

# Card-local actions: key -> action name
CARD_ACTIONS = {
    "r": "retry_failed",
    "s": "skip_failed",
    "y": "approve_tool",
    "n": "deny_tool",
    "a": "always_approve_tool",
    "e": "export_csv",
}

# Human-readable descriptions for /help display
BINDING_DESCRIPTIONS = {
    "ctrl+o": "View full content",
    "ctrl+q": "View task queue",
    "ctrl+s": "Save session",
    "ctrl+l": "Clear output",
    "ctrl+p": "Toggle plan mode",
    "ctrl+t": "List tools",
    "ctrl+c": "Cancel",
    "ctrl+d": "Exit",
    "escape": "Dismiss / back",
}
