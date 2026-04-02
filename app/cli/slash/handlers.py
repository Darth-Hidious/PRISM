"""Slash-command handler implementations for the PRISM REPL.

Each handler is a plain function taking the REPL app as first arg.
Definitions (names, descriptions, aliases) live in registry.py;
rendering / UI lives in cli/tui/; handler *logic* lives here.
"""

import os
from typing import Optional

from app.agent.scratchpad import Scratchpad
from app.cli.slash.registry import REPL_COMMANDS, COMMAND_ALIASES, WORKFLOW_COMMANDS
from app.backend.tool_meta import WARNING, SUCCESS, DIM


def _dot(ok: bool) -> str:
    return "[green]\u25cf[/green]" if ok else "[dim]\u25cb[/dim]"


# ── Dispatch ──────────────────────────────────────────────────────────

def handle_command(app, cmd: str) -> bool:
    """Route a slash-command string to the right handler.

    Returns True when the REPL should exit.
    """
    parts = cmd.strip().split(maxsplit=1)
    base_cmd = COMMAND_ALIASES.get(parts[0].lower(), parts[0].lower())
    arg = parts[1].strip() if len(parts) > 1 else ""

    if base_cmd in ("/exit", "/quit"):
        app.prompt_save_on_exit()
        app.console.print("[dim]Goodbye.[/dim]")
        return True
    elif base_cmd == "/clear":
        app.agent.reset()
        app.scratchpad = Scratchpad()
        app.agent.scratchpad = app.scratchpad
        app.console.print("[dim]Cleared.[/dim]")
    elif base_cmd == "/help":
        handle_help(app)
    elif base_cmd == "/history":
        app.console.print(f"[dim]{len(app.agent.history)} messages[/dim]")
    elif base_cmd == "/tools":
        handle_tools(app)
    elif base_cmd == "/skills":
        handle_skill(app, arg if arg else None)
    elif base_cmd == "/mcp":
        handle_mcp_status(app)
    elif base_cmd == "/save":
        app.memory.set_history(app.agent.history)
        if app.scratchpad:
            app.memory.set_scratchpad_entries(app.scratchpad.to_dict())
        sid = app.memory.save()
        app.console.print(f"[dim]Saved: {sid}[/dim]")
    elif base_cmd == "/export":
        handle_export(app, arg if arg else None)
    elif base_cmd == "/sessions":
        handle_sessions(app)
    elif base_cmd == "/load":
        if not arg:
            app.console.print("[dim]Usage: /load SESSION_ID[/dim]")
        else:
            handle_load(app, arg)
    elif base_cmd == "/plan":
        if not arg:
            app.console.print("[dim]Usage: /plan <goal>[/dim]")
        else:
            handle_plan(app, arg)
    elif base_cmd == "/scratchpad":
        handle_scratchpad(app)
    elif base_cmd == "/approve-all":
        app.agent.auto_approve = True
        app._auto_approve = True
        app.console.print("[yellow]Auto-approve on.[/yellow]")
    elif base_cmd == "/status":
        handle_status(app)
    elif base_cmd == "/login":
        handle_login(app)
    elif base_cmd == "/compact":
        handle_compact(app)
    elif base_cmd == "/cost":
        handle_cost(app)
    elif base_cmd == "/permissions":
        handle_permissions(app, arg if arg else None)
    elif base_cmd == "/report":
        handle_report(app, arg if arg else None)
    elif base_cmd == "/model":
        handle_model(app, arg if arg else None)
    elif base_cmd in ("/node", "/mesh", "/ingest", "/query", "/run", "/workflow"):
        handle_rust_bridge(app, base_cmd, arg)
    elif base_cmd in WORKFLOW_COMMANDS:
        handle_workflow_bridge(app, base_cmd, arg)
    else:
        # Check if it's a dynamically registered workflow
        from app.cli.slash.registry import WORKFLOW_COMMANDS
        if base_cmd in WORKFLOW_COMMANDS:
            handle_workflow_bridge(app, base_cmd, arg)
        else:
            app.console.print(
                f"[dim]Unknown: {base_cmd}  \u2014  /help for commands[/dim]"
            )
    return False


# ── Individual handlers ───────────────────────────────────────────────

def handle_help(app):
    app.console.print()
    for name, desc in REPL_COMMANDS.items():
        if name in ("/quit", "/history"):
            continue
        app.console.print(f"  [bold]{name:<16}[/bold] [dim]{desc}[/dim]")

    # Show workflow commands
    from app.cli.slash.registry import discover_workflow_commands
    discover_workflow_commands()
    if WORKFLOW_COMMANDS:
        app.console.print()
        app.console.print("  [bold]Workflows:[/bold]")
        for name, desc in WORKFLOW_COMMANDS.items():
            app.console.print(f"  [bold]{name:<16}[/bold] [dim]{desc[:55]}[/dim]")
    app.console.print()


def handle_tools(app):
    app.console.print()
    tools = app.agent.tools.list_tools()
    for tool in tools:
        name_style = f"bold {WARNING}" if tool.requires_approval else "bold"
        flag = f" [{WARNING}]\u2605[/{WARNING}]" if tool.requires_approval else ""
        app.console.print(
            f"  [{name_style}]{tool.name:<28}[/{name_style}] "
            f"[dim]{tool.description[:55]}[/dim]{flag}"
        )
    app.console.print(
        f"\n  [dim]{len(tools)} tools[/dim]  "
        f"[{WARNING}]\u2605[/{WARNING}] [dim]= requires approval[/dim]"
    )
    app.console.print()


def handle_status(app):
    from app import __version__
    from app.backend.tool_meta import detect_capabilities
    caps = detect_capabilities()

    app.console.print()
    app.console.print(f"[bold]PRISM[/bold] v{__version__}")
    app.console.print()

    provider = "not configured"
    if os.getenv("MARC27_API_KEY") or os.getenv("MARC27_TOKEN"):
        provider = "MARC27"
    else:
        try:
            from marc27.credentials import CredentialsManager

            creds = CredentialsManager().load()
            if creds and creds.access_token:
                provider = "MARC27"
        except Exception:
            pass
    if provider == "not configured" and os.getenv("ANTHROPIC_API_KEY"):
        provider = "Anthropic (Claude)"
    elif provider == "not configured" and os.getenv("OPENAI_API_KEY"):
        provider = "OpenAI"
    elif provider == "not configured" and os.getenv("OPENROUTER_API_KEY"):
        provider = "OpenRouter"
    app.console.print(
        f"  LLM          {_dot(provider != 'not configured')} {provider}"
    )

    labels = {"ML": "ML", "pyiron": "pyiron", "CALPHAD": "CALPHAD"}
    for key, ok in caps.items():
        label = labels.get(key, key)
        status = "[green]ready[/green]" if ok else "[dim]not installed[/dim]"
        app.console.print(f"  {label:<12}   {_dot(ok)} {status}")

    tool_count = len(app.agent.tools.list_tools())
    try:
        from app.skills.registry import load_builtin_skills
        skill_count = len(load_builtin_skills().list_skills())
    except Exception:
        skill_count = 0
    app.console.print(f"\n  [dim]{tool_count} tools \u00b7 {skill_count} skills[/dim]")

    missing = [n for n, a in caps.items() if not a]
    if missing:
        app.console.print(
            f"  [dim]pip install \"prism-platform[all]\" for {', '.join(missing)}[/dim]"
        )
    app.console.print()


def handle_login(app):
    from app.config.preferences import PRISM_DIR
    from rich.prompt import Prompt

    platform_url = os.getenv("MARC27_PLATFORM_URL", "https://api.marc27.com")

    app.console.print()
    app.console.print("[bold]MARC27 Login[/bold]")
    app.console.print(
        "[dim]Connect your MARC27 account for managed LLM access.[/dim]"
    )
    app.console.print()

    api_key = os.getenv("MARC27_API_KEY") or os.getenv("MARC27_TOKEN")
    if api_key:
        app.console.print(
            f"  Already configured via env. [dim](key: {api_key[:8]}...)[/dim]"
        )
        app.console.print("  [dim]To logout: unset MARC27_API_KEY/MARC27_TOKEN[/dim]")
        app.console.print()
        return

    # Native SDK login path (device flow + stored credentials)
    try:
        from marc27 import PlatformClient
        from marc27.credentials import CredentialsManager

        creds = CredentialsManager().load()
        if creds and creds.access_token:
            app.console.print("  Already logged in via marc27-sdk credentials.")
            if creds.project_id:
                app.console.print(f"  [dim]Active project: {creds.project_id}[/dim]")
            app.console.print("  [dim]Credentials: ~/.prism/credentials.json[/dim]")
            app.console.print()
            return

        app.console.print("  Starting browser login via marc27-sdk device flow...")
        client = PlatformClient(platform_url=platform_url)
        creds = client.login(open_browser=True)
        if creds.project_id:
            os.environ["MARC27_PROJECT_ID"] = str(creds.project_id)
        app.console.print("[green]Logged in to MARC27 via SDK.[/green]")
        app.console.print("[dim]Credentials saved to ~/.prism/credentials.json[/dim]")
        app.console.print()
        return
    except ImportError:
        # marc27-sdk optional dependency; fall back to legacy token mode.
        pass
    except Exception as e:
        app.console.print(f"[yellow]SDK login failed:[/yellow] {e}")
        app.console.print("[dim]Falling back to token login.[/dim]")
        app.console.print()

    app.console.print(
        "  [dim]1.[/dim] Go to [bold]https://marc27.com/account/tokens[/bold]"
    )
    app.console.print("  [dim]2.[/dim] Create a PRISM API token")
    app.console.print("  [dim]3.[/dim] Paste it below")
    app.console.print()

    try:
        token_input = Prompt.ask("  Token", password=True)
    except (EOFError, KeyboardInterrupt):
        app.console.print("\n[dim]Cancelled.[/dim]")
        return

    if not token_input.strip():
        app.console.print("[dim]No token entered.[/dim]")
        return

    token_path = PRISM_DIR / "marc27_token"
    PRISM_DIR.mkdir(parents=True, exist_ok=True)
    token_path.write_text(token_input.strip())
    token_path.chmod(0o600)
    os.environ["MARC27_TOKEN"] = token_input.strip()
    os.environ.setdefault("MARC27_API_KEY", token_input.strip())

    app.console.print("[green]Logged in to MARC27.[/green]")
    app.console.print("[dim]Token saved to ~/.prism/marc27_token (legacy fallback)[/dim]")
    app.console.print()


def handle_skill(app, name: Optional[str] = None):
    try:
        from app.skills.registry import load_builtin_skills
        skills = load_builtin_skills()
    except Exception:
        app.console.print("[dim]No skills available.[/dim]")
        return

    app.console.print()
    if name:
        try:
            skill = skills.get(name)
        except KeyError:
            app.console.print(f"[dim]Skill not found: {name}[/dim]")
            return
        app.console.print(
            f"  [bold]{skill.name}[/bold]  [dim]{skill.category}[/dim]"
        )
        app.console.print(f"  {skill.description}")
        app.console.print()
        for i, step in enumerate(skill.steps, 1):
            opt = " [dim](optional)[/dim]" if step.optional else ""
            app.console.print(
                f"    {i}. {step.name} [dim]\u2014 {step.description}[/dim]{opt}"
            )
    else:
        for skill in skills.list_skills():
            app.console.print(
                f"  {skill.name:<25} [dim]{skill.description[:55]}[/dim]"
            )
    app.console.print()


def handle_plan(app, goal: str):
    prompt = f"The user wants to accomplish: {goal}\n\nAvailable PRISM skills:\n"
    try:
        from app.skills.registry import load_builtin_skills
        for skill in load_builtin_skills().list_skills():
            prompt += f"- {skill.name}: {skill.description}\n"
    except Exception:
        pass
    prompt += "\nWhich skill(s) should be used? Explain the recommended workflow."
    try:
        # Send as a regular message — the UIEmitter/frontend handles rendering
        app.console.print(f"[dim]Sending recommendation query to agent...[/dim]")
        response = app.agent.process(prompt)
        if response:
            from rich.markdown import Markdown
            app.console.print(Markdown(response))
    except Exception as e:
        app.console.print(f"[red]Error: {e}[/red]")


def handle_scratchpad(app):
    if not app.scratchpad or not app.scratchpad.entries:
        app.console.print("[dim]Empty.[/dim]")
        return
    app.console.print()
    for i, entry in enumerate(app.scratchpad.entries, 1):
        tool = f" {entry.tool_name}" if entry.tool_name else ""
        app.console.print(
            f"  [dim]{i}.[/dim]{tool} {entry.summary} "
            f"[dim]{entry.timestamp}[/dim]"
        )
    app.console.print()


def handle_mcp_status(app):
    from app.mcp_client import load_mcp_config
    config = load_mcp_config()
    app.console.print()
    if not config.servers:
        app.console.print("  [dim]No MCP servers configured.[/dim]")
    else:
        for name in config.servers:
            app.console.print(f"  {name}")
    if app._mcp_tools:
        app.console.print(
            f"\n  [dim]{len(app._mcp_tools)} MCP tools loaded[/dim]"
        )
    app.console.print()


def handle_export(app, filename: Optional[str] = None):
    results = None
    for msg in reversed(app.agent.history):
        if msg.get("role") == "tool_result" and isinstance(
            msg.get("result"), dict
        ):
            r = msg["result"]
            if isinstance(r.get("results"), list) and r["results"]:
                results = r["results"]
                break
    if not results:
        app.console.print("[dim]No exportable results.[/dim]")
        return
    export_tool = app.agent.tools.get("export_results_csv")
    if export_tool is None:
        app.console.print("[dim]Export tool not available.[/dim]")
        return
    kwargs = {"results": results}
    if filename:
        kwargs["filename"] = filename
    out = export_tool.execute(**kwargs)
    if "error" in out:
        app.console.print(f"[red]{out['error']}[/red]")
    else:
        app.console.print(f"Exported {out['rows']} rows to {out['filename']}")


def handle_sessions(app):
    sessions = app.memory.list_sessions()
    if not sessions:
        app.console.print("[dim]No saved sessions.[/dim]")
        return
    app.console.print()
    for s in sessions[:20]:
        ts = s.get("timestamp", "")[:19]
        count = s.get("message_count", 0)
        app.console.print(
            f"  {s['session_id']}  [dim]{ts}  ({count} msgs)[/dim]"
        )
    app.console.print()


def handle_load(app, session_id: str):
    try:
        app.load_session(session_id)
        app.console.print(
            f"Loaded {session_id} ({len(app.agent.history)} messages)"
        )
    except FileNotFoundError:
        app.console.print(f"[red]Not found: {session_id}[/red]")
    except Exception as e:
        app.console.print(f"[red]Error: {e}[/red]")


# ── New handlers (v2.6) ─────────────────────────────────────────────

def handle_compact(app):
    """Trigger manual conversation compaction."""
    if hasattr(app.agent, 'transcript'):
        summary = app.agent.transcript.compact(keep_last=6)
        if summary:
            app.console.print(f"[dim]Compacted. Summary:[/dim]")
            for line in summary.split("\n"):
                app.console.print(f"  [dim]{line}[/dim]")
        else:
            app.console.print("[dim]Nothing to compact (conversation too short).[/dim]")
    else:
        # Fallback: clear old history keeping last 6 messages
        if len(app.agent.history) > 6:
            kept = app.agent.history[-6:]
            app.agent.history.clear()
            app.agent.history.extend(kept)
            app.console.print(f"[dim]Compacted to {len(kept)} messages.[/dim]")
        else:
            app.console.print("[dim]Nothing to compact.[/dim]")


def handle_cost(app):
    """Show token usage and cost."""
    usage = app.agent._total_usage
    cost = app.agent._calculate_cost(usage)
    model = getattr(app.agent.backend, 'model', 'unknown')

    app.console.print()
    app.console.print(f"  [bold]Model:[/bold]   {model}")
    app.console.print(f"  [bold]Input:[/bold]   {usage.input_tokens:,} tokens")
    app.console.print(f"  [bold]Output:[/bold]  {usage.output_tokens:,} tokens")
    if usage.cache_read_tokens:
        app.console.print(f"  [bold]Cache:[/bold]   {usage.cache_read_tokens:,} tokens (read)")
    app.console.print(f"  [bold]Cost:[/bold]    ${cost:.4f}")

    if hasattr(app.agent, 'cost') and app.agent.cost.events:
        app.console.print(f"\n  [dim]{len(app.agent.cost.events)} cost events recorded[/dim]")

    if hasattr(app.agent, 'transcript'):
        app.console.print(f"  [dim]{app.agent.transcript.turn_count} turns[/dim]")
    app.console.print()


def handle_permissions(app, mode: Optional[str] = None):
    """Show or switch permission mode."""
    from app.cli.slash.registry import PERMISSION_MODES
    from app.agent.permissions import ToolPermissionContext

    if mode is None:
        # Show current mode
        current = "full-access" if app.agent.auto_approve else "workspace-write"
        if hasattr(app.agent, 'permissions'):
            ctx = app.agent.permissions
            if ctx.deny_names or ctx.deny_prefixes:
                current = "read-only"
            elif ctx == ToolPermissionContext.accept_all():
                current = "full-access"

        app.console.print()
        for name, desc in PERMISSION_MODES.items():
            marker = "[green]\u25cf[/green]" if name == current else "[dim]\u25cb[/dim]"
            app.console.print(f"  {marker} [bold]{name:<18}[/bold] [dim]{desc}[/dim]")
        app.console.print(f"\n  [dim]Switch: /permissions <mode>[/dim]")
        app.console.print()
        return

    mode = mode.lower().strip()
    if mode not in PERMISSION_MODES:
        app.console.print(f"[red]Unknown mode: {mode}[/red]")
        app.console.print(f"[dim]Options: {', '.join(PERMISSION_MODES.keys())}[/dim]")
        return

    if mode == "read-only":
        app.agent.auto_approve = False
        app.agent.permissions = ToolPermissionContext.default().with_deny(
            prefixes=("execute_", "write_", "export_", "import_", "compute_submit"),
        )
        app.console.print("[dim]Read-only mode. Search, read, and query only.[/dim]")
    elif mode == "workspace-write":
        app.agent.auto_approve = False
        app.agent.permissions = ToolPermissionContext.default()
        app.console.print("[dim]Workspace-write mode. Approval required for risky tools.[/dim]")
    elif mode == "full-access":
        app.agent.auto_approve = True
        app.agent.permissions = ToolPermissionContext.accept_all()
        app.console.print("[yellow]Full-access mode. All tools auto-approved.[/yellow]")


def handle_model(app, model_name: Optional[str] = None):
    """Show or switch model."""
    current = getattr(app.agent.backend, 'model', 'unknown')

    if model_name is None:
        app.console.print(f"\n  [bold]Current model:[/bold] {current}")
        app.console.print(f"  [dim]Switch: /model <name>[/dim]")
        app.console.print(f"  [dim]Examples: claude-sonnet-4-20250514, gpt-4o, qwen2.5:7b[/dim]")
        app.console.print()
        return

    # Try to switch — backend must support it
    if hasattr(app.agent.backend, 'model'):
        app.agent.backend.model = model_name
        app.console.print(f"[dim]Switched to: {model_name}[/dim]")
    else:
        app.console.print(f"[dim]Backend doesn't support model switching.[/dim]")


def handle_report(app, description: Optional[str] = None):
    """File a bug report from the REPL."""
    if not description:
        app.console.print("[dim]Usage: /report <description of the issue>[/dim]")
        return

    app.console.print("[dim]Filing report...[/dim]")
    import subprocess
    result = subprocess.run(
        ["prism", "report", description, "--no-github"],
        capture_output=True, text=True, timeout=15,
    )
    if result.returncode == 0:
        app.console.print(f"[dim]{result.stdout.strip()}[/dim]")
    else:
        app.console.print(f"[red]Report failed: {result.stderr.strip()}[/red]")


def handle_rust_bridge(app, cmd: str, arg: str):
    """Bridge slash commands to Rust CLI commands.

    /node status   → prism node status
    /mesh discover → prism mesh discover
    /query "..."   → prism query "..."
    /ingest f.csv  → prism ingest f.csv
    """
    import subprocess

    # Map /command to prism subcommand
    prism_cmd = cmd.lstrip("/")
    full_cmd = ["prism", prism_cmd]
    if arg:
        full_cmd.extend(arg.split())

    app.console.print(f"[dim]$ {' '.join(full_cmd)}[/dim]")

    try:
        result = subprocess.run(
            full_cmd, capture_output=True, text=True, timeout=30,
        )
        if result.stdout.strip():
            app.console.print(result.stdout.strip())
        if result.stderr.strip() and result.returncode != 0:
            app.console.print(f"[red]{result.stderr.strip()}[/red]")
    except FileNotFoundError:
        app.console.print("[red]prism binary not found. Is it in your PATH?[/red]")
    except subprocess.TimeoutExpired:
        app.console.print("[yellow]Command timed out after 30s.[/yellow]")


def handle_workflow_bridge(app, cmd: str, arg: str):
    """Bridge slash commands to YAML workflows.

    /forge --paper arxiv:123 → prism workflow run forge --set paper=arxiv:123
    """
    import subprocess

    workflow_name = cmd.lstrip("/")
    full_cmd = ["prism", "workflow", "run", workflow_name]
    if arg:
        # Convert simple args to --set key=value format
        for part in arg.split():
            if "=" in part:
                full_cmd.extend(["--set", part])
            elif part.startswith("--"):
                full_cmd.append(part)
            else:
                full_cmd.extend(["--set", f"input={part}"])

    app.console.print(f"[dim]$ {' '.join(full_cmd)}[/dim]")

    try:
        result = subprocess.run(
            full_cmd, capture_output=True, text=True, timeout=60,
        )
        if result.stdout.strip():
            app.console.print(result.stdout.strip())
        if result.stderr.strip() and result.returncode != 0:
            app.console.print(f"[red]{result.stderr.strip()}[/red]")
    except FileNotFoundError:
        app.console.print("[red]prism binary not found.[/red]")
    except subprocess.TimeoutExpired:
        app.console.print("[yellow]Workflow timed out after 60s.[/yellow]")
