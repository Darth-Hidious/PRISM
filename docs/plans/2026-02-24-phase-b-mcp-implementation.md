# PRISM Phase B: MCP Integration — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make PRISM both an MCP server (exposing all tools to external hosts like Claude Desktop, Cursor, etc.) and an MCP client (consuming external MCP servers like filesystem, web search, databases). Uses FastMCP 3.x.

**Depends on:** Phase A complete (agent core, backends, tool registry, REPL, autonomous mode, data pipeline, ML pipeline, streaming, CSV export, session memory). All 123 tests passing.

**Key Constraint:** FastMCP requires Python >= 3.10. Update `requires-python` from `>=3.9` to `>=3.10`.

---

## Architecture

```
External MCP Hosts                    External MCP Servers
(Claude Desktop, Cursor, etc.)        (filesystem, databases, etc.)
         |                                      |
         v                                      v
   PRISM MCP Server                      PRISM MCP Client
   (FastMCP, exposes tools)              (FastMCP Client, consumes tools)
         |                                      |
         +-----------> ToolRegistry <-----------+
                            |
                        AgentCore
```

**Two independent capabilities:**

1. **MCP Server** (`prism serve`): Exposes PRISM's existing tools via MCP so external LLM hosts can call them. Uses FastMCP `@mcp.tool` decorators that delegate to our existing `Tool.execute()`. Supports stdio (Claude Desktop) and HTTP transports.

2. **MCP Client** (`prism` REPL + `prism run`): Connects to external MCP servers, discovers their tools, wraps them as PRISM `Tool` objects, and injects them into the agent's `ToolRegistry`. Configured via `~/.prism/mcp_servers.json`.

---

## Phase B-1: MCP Server Foundation

### Task 1: Add FastMCP dependency and bump Python requirement

**Files:**
- Modify: `pyproject.toml`

**Step 1: Update pyproject.toml**

Add `fastmcp>=3.0.0` to core dependencies. Update `requires-python` to `>=3.10`.

**Step 2: Install and verify**

Run: `pip install -e .`
Verify: `python3 -c "import fastmcp; print(fastmcp.__version__)"`

**Step 3: Run full test suite**

Run: `python3 -m pytest tests/ -x -q`
Expected: All 123 tests still pass.

---

### Task 2: Create MCP server module that exposes existing tools

**Files:**
- Create: `app/mcp_server.py`
- Create: `tests/test_mcp_server.py`

**Step 1: Write the failing test**

Create `tests/test_mcp_server.py`:
```python
"""Tests for MCP server."""
import pytest
from app.mcp_server import create_mcp_server


class TestMCPServer:
    def test_server_creation(self):
        server = create_mcp_server()
        assert server is not None
        assert server.name == "prism"

    def test_server_has_tools(self):
        server = create_mcp_server()
        # FastMCP registers tools on the server's local_provider
        tools = server._tool_manager.list_tools()
        tool_names = [t.name for t in tools]
        assert "search_optimade" in tool_names
        assert "query_materials_project" in tool_names
        assert "export_results_csv" in tool_names
        assert "predict_property" in tool_names
        assert "list_models" in tool_names

    def test_server_has_resources(self):
        server = create_mcp_server()
        resources = server._resource_manager.list_resources()
        # Should have at least the session list resource
        assert len(resources) >= 1
```

**Step 2: Run test to verify it fails**

Run: `python3 -m pytest tests/test_mcp_server.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/mcp_server.py`:
```python
"""PRISM MCP Server — exposes all tools to external MCP hosts."""
import json
from fastmcp import FastMCP

from app.tools.base import ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools
from app.tools.prediction import create_prediction_tools
from app.agent.memory import SessionMemory


def _build_registry() -> ToolRegistry:
    """Build a full tool registry with all PRISM tools."""
    registry = ToolRegistry()
    create_system_tools(registry)
    create_data_tools(registry)
    create_visualization_tools(registry)
    create_prediction_tools(registry)
    return registry


def create_mcp_server() -> FastMCP:
    """Create a FastMCP server exposing all PRISM tools and resources."""
    mcp = FastMCP(
        name="prism",
        instructions="PRISM: Materials science research tools. Search OPTIMADE databases, query Materials Project, predict properties with ML, visualize results, and export data.",
    )
    registry = _build_registry()

    # Register each tool from ToolRegistry as a FastMCP tool
    for tool in registry.list_tools():
        _register_tool(mcp, tool)

    # Register resources
    _register_resources(mcp)

    return mcp


def _register_tool(mcp: FastMCP, tool):
    """Register a single PRISM Tool as a FastMCP tool."""
    # Build a closure that calls tool.execute()
    def make_handler(t):
        def handler(**kwargs) -> str:
            result = t.execute(**kwargs)
            return json.dumps(result, default=str)
        handler.__name__ = t.name
        handler.__doc__ = t.description
        # Attach annotations from input_schema so FastMCP can build the schema
        # FastMCP reads type hints, but we have JSON schema — use a wrapper approach
        return handler

    handler = make_handler(tool)
    mcp.add_tool(
        handler,
        name=tool.name,
        description=tool.description,
    )


def _register_resources(mcp: FastMCP):
    """Register MCP resources for PRISM data."""

    @mcp.resource("prism://sessions")
    def list_sessions() -> str:
        """List saved PRISM sessions."""
        memory = SessionMemory()
        sessions = memory.list_sessions()
        return json.dumps(sessions, default=str)

    @mcp.resource("prism://tools")
    def list_tools() -> str:
        """List all available PRISM tools."""
        registry = _build_registry()
        tools = [{"name": t.name, "description": t.description} for t in registry.list_tools()]
        return json.dumps(tools)
```

**Step 4: Run tests to verify they pass**

Run: `python3 -m pytest tests/test_mcp_server.py -v`
Expected: All 3 tests PASS

NOTE: The test for `_tool_manager` and `_resource_manager` may need adjustment based on FastMCP 3.x internal API. If those are not the right attribute names, adapt the test to use the public API — e.g., create a Client against the server in-memory and call `list_tools()` / `list_resources()`.

---

### Task 3: Add `prism serve` CLI command

**Files:**
- Modify: `app/cli.py`
- Create: `tests/test_cli_serve.py`

**Step 1: Write the test**

Create `tests/test_cli_serve.py`:
```python
"""Tests for prism serve command."""
import pytest
from click.testing import CliRunner
from app.cli import cli


class TestServeCommand:
    def test_serve_help(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--help"])
        assert result.exit_code == 0
        assert "transport" in result.output.lower() or "stdio" in result.output.lower()

    def test_serve_command_exists(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["serve", "--help"])
        assert result.exit_code == 0
```

**Step 2: Add the serve command to cli.py**

In `app/cli.py`, add a `serve` command:

```python
@cli.command("serve")
@click.option("--transport", default="stdio", type=click.Choice(["stdio", "http"]), help="MCP transport (stdio for Claude Desktop, http for web)")
@click.option("--port", default=8000, type=int, help="HTTP port (only for http transport)")
def serve(transport, port):
    """Start PRISM as an MCP server for external LLM hosts."""
    from app.mcp_server import create_mcp_server
    console = Console()
    server = create_mcp_server()
    if transport == "http":
        console.print(f"[bold cyan]PRISM MCP Server[/bold cyan] starting on http://localhost:{port}/mcp")
    else:
        console.print("[bold cyan]PRISM MCP Server[/bold cyan] starting on stdio", err=True)
    server.run(transport=transport, port=port)
```

**Step 3: Run tests**

Run: `python3 -m pytest tests/test_cli_serve.py -v`
Expected: All 2 tests PASS

---

### Task 4: Add Claude Desktop configuration helper

**Files:**
- Modify: `app/mcp_server.py` (add `generate_claude_config()`)
- Modify: `app/cli.py` (add `--install` flag to serve)

**Step 1: Add config generator**

In `app/mcp_server.py`, add:

```python
def generate_claude_desktop_config() -> dict:
    """Generate claude_desktop_config.json entry for PRISM."""
    import sys
    return {
        "mcpServers": {
            "prism": {
                "command": sys.executable,
                "args": ["-m", "app.cli", "serve"],
            }
        }
    }
```

**Step 2: Add --install flag**

In `app/cli.py`, on the `serve` command, add an `--install` flag that prints the JSON config for the user to paste into their Claude Desktop config:

```python
@click.option("--install", is_flag=True, help="Print Claude Desktop configuration JSON and exit")
```

When `--install` is passed, print the config and exit without starting the server.

**Step 3: Test manually**

Run: `prism serve --install`
Expected: Prints JSON with `mcpServers.prism` entry.

---

## Phase B-2: MCP Client

### Task 5: Create MCP client configuration module

**Files:**
- Create: `app/mcp_client.py`
- Create: `tests/test_mcp_client.py`

**Step 1: Write the failing test**

Create `tests/test_mcp_client.py`:
```python
"""Tests for MCP client configuration."""
import json
import tempfile
import pytest
from app.mcp_client import MCPClientConfig, load_mcp_config


class TestMCPClientConfig:
    def test_load_empty_config(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump({"mcpServers": {}}, f)
            f.flush()
            config = load_mcp_config(f.name)
        assert config.servers == {}

    def test_load_config_with_servers(self):
        cfg = {
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@anthropic/mcp-filesystem"],
                },
                "weather": {
                    "url": "https://weather.example.com/mcp",
                },
            }
        }
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump(cfg, f)
            f.flush()
            config = load_mcp_config(f.name)
        assert "filesystem" in config.servers
        assert "weather" in config.servers
        assert config.servers["filesystem"]["command"] == "npx"

    def test_default_config_path(self):
        config = MCPClientConfig()
        assert config.config_path.name == "mcp_servers.json"
```

**Step 2: Write implementation**

Create `app/mcp_client.py`:
```python
"""MCP client: connect to external MCP servers and import their tools."""
import asyncio
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

from app.tools.base import Tool, ToolRegistry


@dataclass
class MCPClientConfig:
    """Configuration for external MCP server connections."""
    config_path: Path = field(default_factory=lambda: Path.home() / ".prism" / "mcp_servers.json")
    servers: Dict[str, Dict[str, Any]] = field(default_factory=dict)


def load_mcp_config(path: Optional[str] = None) -> MCPClientConfig:
    """Load MCP server configuration from JSON file."""
    config = MCPClientConfig()
    if path:
        config.config_path = Path(path)
    if config.config_path.exists():
        data = json.loads(config.config_path.read_text())
        config.servers = data.get("mcpServers", {})
    return config


async def discover_tools_from_server(server_name: str, server_config: Dict[str, Any]) -> List[Dict]:
    """Connect to an MCP server and discover its tools.

    Returns list of dicts with: name, description, input_schema, server_name.
    """
    from fastmcp import Client

    client = Client(server_config)
    tools = []
    try:
        async with client:
            mcp_tools = await client.list_tools()
            for t in mcp_tools:
                tools.append({
                    "name": f"{server_name}_{t.name}",
                    "description": f"[{server_name}] {t.description or t.name}",
                    "input_schema": t.inputSchema if hasattr(t, "inputSchema") else {},
                    "server_name": server_name,
                    "original_name": t.name,
                })
    except Exception:
        pass  # Server unavailable, skip
    return tools


async def call_mcp_tool(server_config: Dict[str, Any], tool_name: str, arguments: dict) -> dict:
    """Call a tool on an external MCP server."""
    from fastmcp import Client

    client = Client(server_config)
    try:
        async with client:
            result = await client.call_tool(tool_name, arguments)
            # Extract text content from MCP result
            if hasattr(result, "content") and result.content:
                texts = [c.text for c in result.content if hasattr(c, "text")]
                return {"result": "\n".join(texts)}
            return {"result": str(result)}
    except Exception as e:
        return {"error": str(e)}


def discover_and_register_mcp_tools(registry: ToolRegistry, config: Optional[MCPClientConfig] = None) -> List[str]:
    """Discover tools from all configured MCP servers and register them.

    Returns list of registered tool names.
    """
    if config is None:
        config = load_mcp_config()
    if not config.servers:
        return []

    registered = []
    for server_name, server_config in config.servers.items():
        try:
            tools = asyncio.run(discover_tools_from_server(server_name, server_config))
        except Exception:
            continue

        for tool_info in tools:
            # Create a closure that calls the remote MCP tool
            def make_handler(sconfig, orig_name):
                def handler(**kwargs) -> dict:
                    return asyncio.run(call_mcp_tool(sconfig, orig_name, kwargs))
                return handler

            tool = Tool(
                name=tool_info["name"],
                description=tool_info["description"],
                input_schema=tool_info["input_schema"],
                func=make_handler(server_config, tool_info["original_name"]),
            )
            registry.register(tool)
            registered.append(tool_info["name"])

    return registered
```

**Step 3: Run tests**

Run: `python3 -m pytest tests/test_mcp_client.py -v`
Expected: All 3 tests PASS

---

### Task 6: Wire MCP client into REPL and autonomous mode

**Files:**
- Modify: `app/agent/repl.py`
- Modify: `app/agent/autonomous.py`
- Modify: `app/cli.py`
- Create: `tests/test_mcp_integration.py`

**Step 1: Write the test**

Create `tests/test_mcp_integration.py`:
```python
"""Tests for MCP client integration with agent."""
import json
import tempfile
import pytest
from unittest.mock import patch, MagicMock
from app.mcp_client import load_mcp_config, discover_and_register_mcp_tools
from app.tools.base import ToolRegistry


class TestMCPIntegration:
    def test_empty_config_registers_nothing(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump({"mcpServers": {}}, f)
            f.flush()
            config = load_mcp_config(f.name)
        registry = ToolRegistry()
        registered = discover_and_register_mcp_tools(registry, config)
        assert registered == []

    def test_unavailable_server_skipped(self):
        """If an MCP server is unreachable, it's silently skipped."""
        from app.mcp_client import MCPClientConfig
        config = MCPClientConfig(servers={
            "fake": {"url": "http://localhost:99999/mcp"}
        })
        registry = ToolRegistry()
        registered = discover_and_register_mcp_tools(registry, config)
        assert registered == []
```

**Step 2: Add MCP tool loading option to REPL and autonomous mode**

In `app/agent/repl.py`, in `__init__`, after building the tool registry, optionally discover MCP tools:

```python
# After tools registry is built, try loading MCP tools
try:
    from app.mcp_client import discover_and_register_mcp_tools
    mcp_tools = discover_and_register_mcp_tools(tools)
    if mcp_tools:
        self.console.print(f"[dim]Loaded {len(mcp_tools)} tools from MCP servers[/dim]")
except Exception:
    pass  # MCP client not available or no config
```

In `app/agent/autonomous.py`, in `_make_tools()`, add the same pattern.

In `app/cli.py`, add `--no-mcp` flag to the main `cli` group to disable MCP tool loading.

**Step 3: Run tests**

Run: `python3 -m pytest tests/test_mcp_integration.py -v`
Expected: All 2 tests PASS

---

### Task 7: Add /mcp REPL command

**Files:**
- Modify: `app/agent/repl.py`

**Step 1: Add /mcp command**

Add to `REPL_COMMANDS`:
```python
"/mcp": "Show connected MCP servers and their tools",
```

Add handler in `_handle_command`:
```python
elif base_cmd == "/mcp":
    self._handle_mcp_status()
```

Implement `_handle_mcp_status()`:
- List all tools that came from MCP servers (prefixed with server name)
- Show which servers are configured and connected
- Show server config file path

**Step 2: Test manually**

Start REPL, type `/mcp`. Should show status even with no servers configured.

---

## Phase B-3: Server Refinements

### Task 8: Add dynamic tool parameters via JSON schema

**Files:**
- Modify: `app/mcp_server.py`
- Modify: `tests/test_mcp_server.py`

**Step 1: Improve tool registration**

The initial `_register_tool` uses a generic `**kwargs` handler. Improve it to pass proper JSON schema metadata to FastMCP so that MCP clients see proper parameter descriptions. Use FastMCP's ability to accept a JSON schema via the tool's metadata rather than relying on Python type hints.

**Step 2: Add test**

Add a test that creates a Client against the in-memory server, calls `list_tools()`, and verifies that the `search_optimade` tool has the correct `inputSchema` with `filter_string` as a required parameter.

**Step 3: Run tests**

Run: `python3 -m pytest tests/test_mcp_server.py -v`
Expected: All tests PASS

---

### Task 9: Add MCP resources for datasets and models

**Files:**
- Modify: `app/mcp_server.py`
- Modify: `tests/test_mcp_server.py`

**Step 1: Add dataset and model resources**

Add to `_register_resources()`:

```python
@mcp.resource("prism://datasets")
def list_datasets() -> str:
    """List collected materials datasets."""
    from app.data.store import DataStore
    store = DataStore()
    return json.dumps(store.list_datasets(), default=str)

@mcp.resource("prism://models")
def list_trained_models() -> str:
    """List trained ML models and their metrics."""
    from app.ml.registry import ModelRegistry
    registry = ModelRegistry()
    return json.dumps(registry.list_models(), default=str)

@mcp.resource("prism://datasets/{name}")
def get_dataset(name: str) -> str:
    """Get a specific dataset's metadata and preview."""
    from app.data.store import DataStore
    store = DataStore()
    df = store.load(name)
    return json.dumps({
        "name": name,
        "rows": len(df),
        "columns": list(df.columns),
        "preview": df.head(10).to_dict(orient="records"),
    }, default=str)
```

**Step 2: Add tests**

Test that the resources are registered and return valid JSON.

**Step 3: Run tests**

Run: `python3 -m pytest tests/test_mcp_server.py -v`
Expected: All tests PASS

---

### Task 10: Create `~/.prism/mcp_servers.json` template and docs

**Files:**
- Modify: `app/cli.py` (add `mcp` command group with `init` and `status` subcommands)
- Create: `tests/test_cli_mcp.py`

**Step 1: Add mcp CLI command group**

```python
@cli.group("mcp")
def mcp_group():
    """Manage MCP server connections."""
    pass

@mcp_group.command("init")
def mcp_init():
    """Create a template mcp_servers.json config file."""
    # Creates ~/.prism/mcp_servers.json with example servers commented out

@mcp_group.command("status")
def mcp_status():
    """Show MCP server configuration and connection status."""
    # Lists configured servers and tests connectivity
```

**Step 2: Add tests**

```python
class TestMCPCommands:
    def test_mcp_help(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "--help"])
        assert result.exit_code == 0

    def test_mcp_init(self, tmp_path, monkeypatch):
        monkeypatch.setenv("HOME", str(tmp_path))
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "init"])
        assert result.exit_code == 0

    def test_mcp_status(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["mcp", "status"])
        assert result.exit_code == 0
```

**Step 3: Run tests**

Run: `python3 -m pytest tests/test_cli_mcp.py -v`
Expected: All 3 tests PASS

---

## Phase B-4: End-to-End Testing

### Task 11: In-memory integration test — server + client round-trip

**Files:**
- Create: `tests/test_mcp_roundtrip.py`

**Step 1: Write the integration test**

```python
"""End-to-end MCP test: server exposes tools, client calls them."""
import asyncio
import json
import pytest
from fastmcp import Client
from app.mcp_server import create_mcp_server


class TestMCPRoundTrip:
    def test_list_tools_via_client(self):
        """Client can discover all PRISM tools via MCP."""
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                tools = await client.list_tools()
                return [t.name for t in tools]

        tool_names = asyncio.run(run())
        assert "search_optimade" in tool_names
        assert "predict_property" in tool_names
        assert "export_results_csv" in tool_names

    def test_call_list_models_via_mcp(self):
        """Client can call list_models tool and get valid JSON back."""
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                result = await client.call_tool("list_models", {})
                return result

        result = asyncio.run(run())
        # Result should be parseable JSON with "models" key
        data = json.loads(result.content[0].text if hasattr(result, "content") else str(result))
        assert "models" in data

    def test_read_resources_via_client(self):
        """Client can read PRISM resources."""
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                resources = await client.list_resources()
                return [r.uri for r in resources]

        uris = asyncio.run(run())
        assert any("sessions" in str(u) for u in uris)
        assert any("tools" in str(u) for u in uris)
```

**Step 2: Run the integration test**

Run: `python3 -m pytest tests/test_mcp_roundtrip.py -v`
Expected: All 3 tests PASS

---

### Task 12: Full test suite verification and commit

**Files:**
- No new files. Verification task.

**Step 1: Run full test suite**

Run: `python3 -m pytest tests/ -v --tb=short`
Expected: All tests PASS (123 original + ~15 new MCP tests)

**Step 2: Verify CLI commands**

Run:
```bash
prism serve --help
prism serve --install
prism mcp --help
prism mcp status
```

**Step 3: Verify the server actually starts**

Run: `prism serve --transport http --port 8765 &` (background)
Then: `curl http://localhost:8765/mcp` or use FastMCP client to connect.
Kill the background process.

---

## Summary

| Phase | Tasks | What Gets Built |
|-------|-------|----------------|
| B-1: Server Foundation | 1-4 | FastMCP dep, MCP server exposing all tools, `prism serve`, Claude Desktop config |
| B-2: MCP Client | 5-7 | Config loading, tool discovery + registration, `/mcp` REPL command |
| B-3: Server Refinements | 8-10 | JSON schema passthrough, dataset/model resources, `prism mcp init/status` |
| B-4: End-to-End Testing | 11-12 | Round-trip integration tests, full verification |

**Total: 12 tasks**

After Phase B, PRISM will be:
- Usable as an **MCP server** from Claude Desktop, Cursor, or any MCP-compatible host
- Able to **consume external MCP servers** (filesystem, databases, APIs) and use their tools in the agent loop
- Configurable via `~/.prism/mcp_servers.json`
- Launchable via `prism serve` (stdio or HTTP)

**Implementation order:**
```
Group 1 (no deps):          1
Group 2 (depends on 1):     2, 3
Group 3 (depends on 2):     4, 5
Group 4 (depends on 3-5):   6, 7
Group 5 (depends on 6):     8, 9, 10
Group 6 (final):            11, 12
```
