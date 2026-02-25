"""Backward-compatibility shim — real implementation in app.cli.tui.

All TUI code now lives under app/cli/tui/. This file re-exports the
public API so existing imports continue to work.
"""

# Re-export the REPL class
from app.cli.tui.app import AgentREPL  # noqa: F401

# Re-export card helpers used by tests
from app.cli.tui.cards import (  # noqa: F401
    detect_result_type as _detect_result_type,
    format_elapsed,
    make_title,
)

# Re-export theme constants used by tests
from app.cli.tui.theme import (  # noqa: F401
    BORDERS as _BORDERS,
    MASCOT as _MASCOT,
    CRYSTAL_OUTER_DIM as _CRYSTAL_OUTER_DIM,
    CRYSTAL_OUTER as _CRYSTAL_OUTER,
    CRYSTAL_INNER as _CRYSTAL_INNER,
    CRYSTAL_CORE as _CRYSTAL_CORE,
    TRUNCATION_LINES as _TRUNCATION_LINES,
    HEADER_COMMANDS_L as _HEADER_COMMANDS_L,
    HEADER_COMMANDS_R as _HEADER_COMMANDS_R,
)

# Re-export command registry
from app.cli.slash.registry import REPL_COMMANDS  # noqa: F401
