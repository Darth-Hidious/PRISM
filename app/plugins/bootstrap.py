"""Central bootstrap: build a fully loaded ToolRegistry in one call."""
import json
import logging
from pathlib import Path

from app.tools.base import ToolRegistry

logger = logging.getLogger(__name__)

_CATALOG_PATH = Path(__file__).parent / "catalog.json"


def load_agent_configs(catalog_path: Path | None = None):
    """Load agent configs from the plugin catalog."""
    from app.agent.agent_registry import AgentConfig, AgentRegistry

    reg = AgentRegistry()
    p = catalog_path or _CATALOG_PATH
    if not p.exists():
        return reg
    try:
        data = json.loads(p.read_text())
        for pid, entry in data.get("plugins", {}).items():
            if entry.get("type") != "agent":
                continue
            reg.register(AgentConfig(
                id=pid,
                name=entry.get("name", pid),
                description=entry.get("description", ""),
                system_prompt=entry.get("system_prompt", ""),
                tools=entry.get("tools"),
                skills=entry.get("skills"),
                runtime=entry.get("runtime", "local"),
                remote_endpoint=entry.get("remote_endpoint"),
                max_iterations=entry.get("max_iterations", 20),
                enabled=entry.get("enabled", True),
            ))
    except Exception as e:
        logger.debug("Failed to load agent configs: %s", e)
    return reg


def build_full_registry(
    enable_mcp: bool = True,
    enable_plugins: bool = True,
) -> tuple:
    """Build ToolRegistry, ProviderRegistry, and AgentRegistry.

    Returns (tool_registry, provider_registry, agent_registry).
    """
    from app.tools.data import create_data_tools
    from app.tools.system import create_system_tools
    from app.tools.visualization import create_visualization_tools
    from app.tools.prediction import create_prediction_tools
    from app.tools.search import create_search_tools
    from app.tools.property_selection import create_property_selection_tools
    from app.tools.code import create_code_tools

    registry = ToolRegistry()
    create_system_tools(registry)
    create_data_tools(registry)
    create_visualization_tools(registry)
    create_prediction_tools(registry)
    create_search_tools(registry)
    create_property_selection_tools(registry)
    create_code_tools(registry)

    # Simulation tools (optional — pyiron may not be installed)
    try:
        from app.simulation.bridge import check_pyiron_available

        if check_pyiron_available():
            from app.tools.simulation import create_simulation_tools

            create_simulation_tools(registry)
    except Exception:
        pass

    # CALPHAD tools (optional — pycalphad may not be installed)
    try:
        from app.simulation.calphad_bridge import check_calphad_available

        if check_calphad_available():
            from app.tools.calphad import create_calphad_tools

            create_calphad_tools(registry)
    except Exception:
        pass

    # Built-in skills → tools
    try:
        from app.skills.registry import load_builtin_skills

        load_builtin_skills().register_all_as_tools(registry)
    except Exception:
        pass

    # Search provider registry (3-layer: discovery + overrides + catalog)
    from app.search.providers.registry import ProviderRegistry, build_registry
    try:
        provider_reg = build_registry()
    except Exception:
        provider_reg = ProviderRegistry()

    # Agent registry (from catalog)
    agent_reg = load_agent_configs()

    # Plugins (entry points + local — can register into ANY sub-registry)
    if enable_plugins:
        try:
            from app.data.base_collector import CollectorRegistry
            from app.ml.algorithm_registry import get_default_registry
            from app.skills.base import SkillRegistry
            from app.plugins.registry import PluginRegistry
            from app.plugins.loader import discover_all_plugins

            plugin_reg = PluginRegistry(
                tool_registry=registry,
                skill_registry=SkillRegistry(),
                collector_registry=CollectorRegistry(),
                algorithm_registry=get_default_registry(),
                provider_registry=provider_reg,
                agent_registry=agent_reg,
            )
            discover_all_plugins(plugin_reg)
        except Exception:
            pass

    # External MCP servers
    if enable_mcp:
        try:
            from app.mcp_client import discover_and_register_mcp_tools

            discover_and_register_mcp_tools(registry)
        except Exception:
            pass

    return registry, provider_reg, agent_reg
