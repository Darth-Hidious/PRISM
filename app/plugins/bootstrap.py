"""Central bootstrap: build a fully loaded ToolRegistry in one call."""
import logging

from app.tools.base import ToolRegistry

logger = logging.getLogger(__name__)


def build_full_registry(
    enable_mcp: bool = True,
    enable_plugins: bool = True,
) -> tuple:
    """Build ToolRegistry and ProviderRegistry.

    Returns (tool_registry, provider_registry, None).
    The third element is kept as None for backward compatibility (was agent_registry).
    """
    from app.tools.data import create_data_tools
    from app.tools.system import create_system_tools
    from app.tools.visualization import create_visualization_tools
    from app.tools.prediction import create_prediction_tools
    from app.tools.search import create_search_tools
    from app.tools.property_selection import create_property_selection_tools
    from app.tools.code import create_code_tools
    from app.tools.bash import create_bash_tools
    from app.tools.capabilities import create_capabilities_tools
    from app.tools.dataset import create_dataset_tool
    from app.tools.platform_status import create_platform_status_tools
    from app.tools.agent_capabilities import create_agent_capabilities_tool
    from app.tools.knowledge_write import create_knowledge_write_tool
    from app.tools.platform_jobs import create_platform_jobs_tools
    from app.tools.platform_workflows import create_platform_workflows_tools
    from app.tools.mcp_services import create_mcp_services_tools

    registry = ToolRegistry()
    create_system_tools(registry)
    create_data_tools(registry)
    create_visualization_tools(registry)
    create_prediction_tools(registry)
    create_search_tools(registry)
    create_property_selection_tools(registry)
    create_code_tools(registry)
    create_bash_tools(registry)
    create_capabilities_tools(registry)
    # Platform-status tools (policy/usage/billing reads). Closes three
    # GAP-HIGH endpoints from the v2.7.2 endpoint-coverage audit. All
    # three are read-only — no approval gate.
    create_platform_status_tools(registry)
    # Self-discovery: GET /agent/capabilities. Read-only.
    create_agent_capabilities_tool(registry)
    # Knowledge graph WRITE side (embed/seed/ingest/research-web-search).
    # Closes the read/write asymmetry. Approval-gated as a single tool.
    create_knowledge_write_tool(registry)
    # Platform jobs — generic submit/SSE. Read+cancel in `platform_jobs`,
    # money-spending submit isolated in `platform_jobs_submit`.
    create_platform_jobs_tools(registry)
    # Platform workflows runner. Read+cancel in `platform_workflows`,
    # money-spending start + spec-register in `platform_workflows_run`.
    create_platform_workflows_tools(registry)
    # Platform-hosted MCP service discovery + invocation. Read in
    # `mcp_services`, proxy/scale state-changes in `mcp_services_invoke`.
    create_mcp_services_tools(registry)
    # Unified dataset tool — replaces VALIDATE_SKILL / REVIEW_SKILL /
    # VISUALIZE_SKILL Tool registrations. See app/tools/dataset.py.
    create_dataset_tool(registry)

    # Stateful tool memory — auto-record meaningful tool outputs as artifacts
    # and expose recall/fetch/list as tools. Opt-out via PRISM_DISABLE_MEMORY=1
    # for tests or memory-free CLI modes. The artifact store + recorder are
    # always graceful: any failure inside record_if_enabled is logged and the
    # original tool result passes through unchanged. See
    # docs/stateful_tools_2026.md for the architecture.
    import os as _os
    if _os.environ.get("PRISM_DISABLE_MEMORY", "").strip().lower() not in {"1", "true", "yes", "on"}:
        try:
            from app.tools.memory import (
                ArtifactStore,
                configure as _configure_memory,
                create_memory_tools,
                default_db_path,
            )
            _store = ArtifactStore(default_db_path())
            _configure_memory(store=_store)
            create_memory_tools(registry)
        except Exception as e:
            logger.warning("memory subsystem disabled: %s", e)

    # Web browsing tools (Firecrawl + DuckDuckGo fallback)
    try:
        from app.tools.web import create_web_tools
        create_web_tools(registry)
    except Exception:
        pass

    # MARC27 Knowledge Plane tools (graph search, semantic search, ingest)
    try:
        from app.tools.knowledge import create_knowledge_tools
        create_knowledge_tools(registry)
    except Exception:
        pass

    # MARC27 RLM Research tool — server-side recursive LLM that explores
    # the knowledge graph + vector store + academic web. Real-money tool.
    # See app/tools/research.py for the SSE protocol + cost notes.
    try:
        from app.tools.research import create_research_tools
        create_research_tools(registry)
    except Exception:
        pass

    # MARC27 Compute Broker tools (GPU dispatch, job management)
    try:
        from app.tools.compute import create_compute_tools
        create_compute_tools(registry)
    except Exception:
        pass

    # Premium labs tools (marketplace services)
    from app.tools.labs import create_labs_tools
    create_labs_tools(registry)

    # Simulation tools (optional — pyiron may not be installed)
    try:
        from app.tools.simulation.bridge import check_pyiron_available

        if check_pyiron_available():
            from app.tools.sim_tools import create_simulation_tools

            create_simulation_tools(registry)
    except Exception:
        pass

    # CALPHAD tools (optional — pycalphad may not be installed)
    try:
        from app.tools.simulation.calphad_bridge import check_calphad_available

        if check_calphad_available():
            from app.tools.calphad import create_calphad_tools

            create_calphad_tools(registry)
    except Exception:
        pass

    # Built-in skills → tools
    try:
        from app.tools.skills.registry import load_builtin_skills

        load_builtin_skills().register_all_as_tools(registry)
    except Exception:
        pass

    # Search provider registry (3-layer: discovery + overrides + catalog)
    from app.tools.search_engine.providers.registry import ProviderRegistry, build_registry
    try:
        provider_reg = build_registry()
    except Exception:
        provider_reg = ProviderRegistry()

    # Expose the federated SearchEngine as the `materials_search` MCP
    # tool. The engine + provider registry already exist; this just
    # wraps them in a shape the in-PRISM agent can call. See
    # docs/search_consolidation_2026.md for the rationale (collapse
    # 9 point-tools to 3 unified). No-op if registration fails so
    # the rest of bootstrap still completes.
    try:
        from app.tools.search_engine.tools import create_search_engine_tools

        create_search_engine_tools(registry, provider_reg)
    except Exception:
        logger.exception("materials_search tool registration failed")

    # Plugins (entry points + local — can register into ANY sub-registry)
    if enable_plugins:
        try:
            from app.tools.data_collectors.base_collector import CollectorRegistry
            from app.tools.ml.algorithm_registry import get_default_registry
            from app.tools.skills.base import SkillRegistry
            from app.plugins.registry import PluginRegistry
            from app.plugins.loader import discover_all_plugins

            plugin_reg = PluginRegistry(
                tool_registry=registry,
                skill_registry=SkillRegistry(),
                collector_registry=CollectorRegistry(),
                algorithm_registry=get_default_registry(),
                provider_registry=provider_reg,
            )
            discover_all_plugins(plugin_reg)
        except Exception:
            pass

    # Spark data processing tools (optional — pyspark may not be installed)
    try:
        from app.tools.spark import _check_spark_available

        if _check_spark_available():
            from app.tools.spark import create_spark_tools

            create_spark_tools(registry)
    except Exception:
        pass

    # Custom tools from ~/.prism/tools/*.py
    try:
        from app.tools.custom_loader import discover_custom_tools

        custom_names = discover_custom_tools(registry)
        if custom_names:
            logger.info("Loaded %d custom tools: %s", len(custom_names), ", ".join(custom_names))
    except Exception:
        pass

    # External MCP servers
    if enable_mcp:
        try:
            from app.mcp_client import discover_and_register_mcp_tools

            discover_and_register_mcp_tools(registry)
        except Exception:
            pass

    return registry, provider_reg, None
