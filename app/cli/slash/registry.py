"""Unified command & flag registry for the PRISM REPL and CLI.

Add new slash commands and CLI flags HERE — both the TUI app and cli.py
import from this single file.
"""

# ── Slash commands (REPL) ────────────────────────────────────────────
# Each entry: "/command" -> description shown in /help.
# Handler implementations live in handlers.py.

REPL_COMMANDS = {
    "/exit": "Exit",
    "/quit": "Exit",
    "/clear": "Clear conversation",
    "/help": "Show commands",
    "/history": "Message count",
    "/tools": "List tools",
    "/skills": "List skills",
    "/status": "Platform status",
    "/mcp": "MCP servers",
    "/save": "Save session",
    "/export": "Export to CSV",
    "/sessions": "List sessions",
    "/load": "Load session",
    "/plan": "Plan a goal",
    "/scratchpad": "Execution log",
    "/approve-all": "Skip consent",
    "/login": "MARC27 account",
}

# Aliases map to canonical command names above.
COMMAND_ALIASES = {
    "/skill": "/skills",
}

# ── CLI flags (click options) ────────────────────────────────────────
# Metadata for flags used across `prism`, `prism run`, `prism serve`.
# cli.py reads these; keep flag names, types, and help text here.

CLI_FLAGS = {
    "dangerously_accept_all": {
        "flag": "--dangerously-accept-all",
        "is_flag": True,
        "help": "Auto-approve all tool calls (skip consent prompts)",
    },
    "confirm": {
        "flag": "--confirm",
        "is_flag": True,
        "help": "Require confirmation for expensive tools",
    },
    "no_mcp": {
        "flag": "--no-mcp",
        "is_flag": True,
        "help": "Disable loading tools from external MCP servers",
    },
    "resume": {
        "flag": "--resume",
        "type": "str",
        "default": None,
        "help": "Resume a saved session by SESSION_ID",
    },
    "provider": {
        "flag": "--provider",
        "type": "str",
        "default": None,
        "help": "LLM provider (anthropic/openai/openrouter)",
    },
    "model": {
        "flag": "--model",
        "type": "str",
        "default": None,
        "help": "Model name override",
    },
}

# ── Agent modes ──────────────────────────────────────────────────────
# Modes shown in the status line below the prompt.

AGENT_MODES = {
    "normal": "Default conversational mode",
    "plan": "Plan-then-execute mode",
    "auto-approve": "All tool calls auto-approved",
}
