"""Central bootstrap: build a fully loaded ToolRegistry in one call."""
from app.tools.base import ToolRegistry


def build_full_registry(
    enable_mcp: bool = True,
    enable_plugins: bool = True,
) -> ToolRegistry:
    """Build a ToolRegistry with all built-in tools, skills, and plugins.

    Consolidates the duplicated loading in repl.py, autonomous.py, and
    mcp_server.py into a single function.
    """
    from app.tools.data import create_data_tools
    from app.tools.system import create_system_tools
    from app.tools.visualization import create_visualization_tools
    from app.tools.prediction import create_prediction_tools

    registry = ToolRegistry()
    create_system_tools(registry)
    create_data_tools(registry)
    create_visualization_tools(registry)
    create_prediction_tools(registry)

    # Simulation tools (optional — pyiron may not be installed)
    try:
        from app.simulation.bridge import check_pyiron_available

        if check_pyiron_available():
            from app.tools.simulation import create_simulation_tools

            create_simulation_tools(registry)
    except Exception:
        pass

    # Built-in skills → tools
    try:
        from app.skills.registry import load_builtin_skills

        load_builtin_skills().register_all_as_tools(registry)
    except Exception:
        pass

    # Plugins (entry points + local directory)
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

    return registry
