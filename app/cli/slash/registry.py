"""Unified command & flag registry for the PRISM REPL and CLI.

Slash commands bridge to three systems:
1. REPL-internal commands (/help, /clear, /status)
2. Rust CLI commands (/node, /mesh, /ingest, /query)
3. YAML workflows (/forge, or any workflow in ~/.prism/workflows/)
"""

# ── Slash commands (REPL) ────────────────────────────────────────────

REPL_COMMANDS = {
    # Session
    "/exit": "Exit",
    "/quit": "Exit",
    "/clear": "Clear conversation (requires confirmation)",
    "/save": "Save session",
    "/sessions": "List saved sessions",
    "/load": "Load session by ID",
    "/compact": "Compress conversation context",
    "/export": "Export results to CSV",

    # Agent control
    "/help": "Show all commands",
    "/status": "Platform status, model, usage, cost",
    "/cost": "Show token usage and estimated cost",
    "/model": "Show or switch LLM model",
    "/permissions": "Show or switch permission mode",
    "/approve-all": "Auto-approve all tool calls",
    "/tools": "List available tools",
    "/skills": "List available skills",
    "/plan": "Plan a multi-step goal",
    "/scratchpad": "Show execution log",

    # Platform
    "/login": "MARC27 account login",
    "/mcp": "MCP server status",
    "/report": "Report a bug (files GitHub issue + MARC27 ticket)",

    # Rust CLI bridge — these shell out to `prism <command>`
    "/node": "Node commands (status, up, down, logs, probe)",
    "/mesh": "Mesh commands (discover, peers, publish, subscribe)",
    "/ingest": "Ingest a data file",
    "/query": "Query the knowledge graph",
    "/run": "Submit a compute job",
    "/workflow": "Run a YAML workflow",

    # History
    "/history": "Message count",
}

# Aliases map to canonical command names above.
COMMAND_ALIASES = {
    "/skill": "/skills",
    "/q": "/query",
    "/n": "/node",
    "/m": "/mesh",
    "/s": "/status",
    "/h": "/help",
    "/c": "/compact",
}

# ── Workflow commands (auto-discovered) ──────────────────────────────
# These are populated at startup from ~/.prism/workflows/ and builtins.
# Each becomes a /command in the REPL: /forge, /analyze-hea, etc.

WORKFLOW_COMMANDS: dict[str, str] = {}  # populated by _discover_workflows()


def discover_workflow_commands():
    """Scan for YAML workflows and register as slash commands."""
    try:
        from app.workflows.registry import discover_workflows
        specs = discover_workflows()
        for name, spec in specs.items():
            cmd = f"/{spec.command_name}"
            WORKFLOW_COMMANDS[cmd] = spec.description
    except Exception:
        pass


# ── CLI flags (click options) ────────────────────────────────────────

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
        "help": "LLM provider (anthropic/openai/openrouter/marc27)",
    },
    "model": {
        "flag": "--model",
        "type": "str",
        "default": None,
        "help": "Model name override",
    },
    "permission_mode": {
        "flag": "--permission-mode",
        "type": "str",
        "default": None,
        "help": "Permission mode: read-only, workspace-write, full-access",
    },
}

# ── Permission modes ────────────────────────────────────────────────

PERMISSION_MODES = {
    "read-only": "Search, read, query tools only — no file writes or code execution",
    "workspace-write": "File editing, data export, code execution — no destructive ops",
    "full-access": "All tools including destructive operations — use with caution",
}

# ── Agent modes ──────────────────────────────────────────────────────

AGENT_MODES = {
    "normal": "Default conversational mode",
    "plan": "Plan-then-execute mode",
    "auto-approve": "All tool calls auto-approved",
}
