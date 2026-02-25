# Plugins & Marketplace

PRISM's plugin system lets anyone extend the platform with new tools, skills,
data providers, ML algorithms, collectors, and agent configurations.

## How Plugins Become Tool Calls

Every plugin ultimately registers into shared registries. The agent's LLM
sees all registered tools and can call them by name.

```
Plugin (entry point or local .py)
  └─ register(PluginRegistry)
       ├─ tool_registry.register(Tool(...))      → agent can call it
       ├─ skill_registry.register(Skill(...))     → converted to Tool via .to_tool()
       ├─ provider_registry.register(Provider(...)) → used by search_materials
       ├─ agent_registry.register(AgentConfig(...)) → named agent profiles
       ├─ algorithm_registry.register(...)          → used by predict tools
       └─ collector_registry.register(...)          → used by data collectors
```

**Tools and skills** are directly callable by the agent LLM.
**Providers, algorithms, and collectors** are used internally by existing tools.

## Plugin Types

| Type | Registry | Agent-Callable? | Purpose |
|------|----------|-----------------|---------|
| `tool` | ToolRegistry | Yes | Single-action capability |
| `skill` | SkillRegistry | Yes (as Tool) | Multi-step workflow |
| `provider` | ProviderRegistry | No (used by search) | Materials data source |
| `agent` | AgentRegistry | No (configuration) | Pre-configured agent profile |
| `algorithm` | AlgorithmRegistry | No (used by predict) | ML model factory |
| `collector` | CollectorRegistry | No (used by data) | Raw data collector |
| `bundle` | Catalog only | No (meta-package) | Groups multiple components |

## Writing a Plugin

### Minimal Example

Create `~/.prism/plugins/my_tool.py`:

```python
from app.tools.base import Tool

def _my_function(**kwargs) -> dict:
    """Your tool implementation."""
    query = kwargs["query"]
    return {"result": f"Processed: {query}"}

def register(registry):
    """Called by PRISM plugin loader."""
    registry.tool_registry.register(Tool(
        name="my_custom_tool",
        description="Does something useful",
        input_schema={
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Input query"},
            },
            "required": ["query"],
        },
        func=_my_function,
    ))
```

Drop the file in `~/.prism/plugins/` and it's automatically discovered on next launch.

### pip-Installable Plugin

In your package's `pyproject.toml`:

```toml
[project.entry-points."prism.plugins"]
my_plugin = "my_package.plugin:register"
```

PRISM discovers all `prism.plugins` entry points at startup.

### Conditional Registration

Plugins can guard on optional dependencies:

```python
def register(registry):
    try:
        import some_library
    except ImportError:
        return  # Skip silently if not installed

    registry.tool_registry.register(Tool(...))
```

See `app/plugins/thermocalc.py` for a real example.

## Catalog (`catalog.json`)

The unified plugin catalog at `app/plugins/catalog.json` defines platform-managed
plugins including data providers, agent profiles, and bundles.

```json
{
  "plugins": {
    "mp_native": {
      "type": "provider",
      "name": "Materials Project (native API)",
      "api_type": "mp_native",
      "base_url": "https://api.materialsproject.org",
      "enabled": true
    },
    "phase_stability_agent": {
      "type": "agent",
      "name": "Phase Stability Agent",
      "system_prompt": "You are a phase stability specialist...",
      "tools": ["search_materials", "calculate_phase_diagram"],
      "skills": ["analyze_phases"],
      "enabled": false
    }
  }
}
```

### Provider Entries

Providers in the catalog are loaded as Layer 3 of the search registry:

- **Layer 1**: OPTIMADE auto-discovery (cached from consortium endpoint)
- **Layer 2**: Bundled overrides (fixes and tier metadata)
- **Layer 3**: Catalog providers (Materials Project native, AFLOW native, OMAT24, etc.)

### Agent Entries

Agent configs define pre-built specialist profiles with curated tool/skill sets
and custom system prompts. Use via `prism run --agent phase_stability_agent "goal"`.

## Discovery Order

```
build_full_registry()
  1. Built-in tools (app/tools/*.py)
  2. Optional tools (pyiron, pycalphad — if installed)
  3. Built-in skills → converted to tools
  4. Provider registry (3-layer: OPTIMADE + overrides + catalog)
  5. Agent configs from catalog.json
  6. Entry-point plugins (pip-installed)
  7. Local plugins (~/.prism/plugins/*.py)
  8. MCP server tools (~/.prism/mcp_servers.json)
```

All tools from steps 1-8 are available to the agent in a single `ToolRegistry`.

## MCP Integration

External MCP servers are also registered as tools, namespaced by server name:

```json
// ~/.prism/mcp_servers.json
{
  "servers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"]
    }
  }
}
```

MCP tools appear as `github_search_repos`, `github_create_issue`, etc.

## CLI

```bash
prism plugin list         # Show all loaded plugins
prism plugin info <name>  # Details about a plugin
prism plugin init         # Scaffold a new plugin
```

## Related

- [Data command](data.md) — uses providers and collectors
- [Run command](run.md) — agent calls all registered tools
- [Serve command](serve.md) — exposes tools via MCP
- [ACKNOWLEDGMENTS](../ACKNOWLEDGMENTS.md) — third-party credits
