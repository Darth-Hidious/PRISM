"""Backward-compatibility shim — real handlers in app.cli.slash.handlers."""

from app.cli.slash.handlers import (  # noqa: F401
    handle_command,
    handle_help,
    handle_tools,
    handle_status,
    handle_login,
    handle_skill,
    handle_plan,
    handle_scratchpad,
    handle_mcp_status,
    handle_export,
    handle_sessions,
    handle_load,
)
