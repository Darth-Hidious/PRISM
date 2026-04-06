"""Capability discovery tool — unified view of everything available.

The agent calls this once at session start (or when asked) to learn what
databases, models, services, providers, and plugins are available. This
replaces calling 5+ separate list tools.
"""
import json
import logging
import os
from pathlib import Path

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def _get_platform_client():
    """Return the MARC27 PlatformClient if the SDK/auth context is available."""
    try:
        from marc27 import PlatformClient
        return PlatformClient()
    except Exception:
        return None


def _read_platform_json(client, path: str):
    """Read a raw JSON payload from the public platform API."""
    from marc27.api.base import BaseAPI

    base: BaseAPI = client._base
    resp = base.get(path)
    return resp.json()


def _load_project_context() -> dict:
    """Resolve the active project context for project-scoped platform endpoints."""
    project_id = os.getenv("MARC27_PROJECT_ID")
    project_name = None

    if project_id:
        return {"project_id": project_id, "project_name": None}

    # PRISM's native CLI persists the active org/project here.
    cli_state_path = (
        Path.home()
        / "Library"
        / "Application Support"
        / "com.marc27.prism"
        / "cli-state.json"
    )
    if cli_state_path.exists():
        try:
            data = json.loads(cli_state_path.read_text())
            creds = data.get("credentials") or {}
            project_id = creds.get("project_id")
            project_name = creds.get("project_name")
            if project_id:
                return {"project_id": project_id, "project_name": project_name}
        except Exception:
            pass

    # Older Python-side auth flow stores a simpler credentials blob here.
    legacy_path = Path.home() / ".prism" / "credentials.json"
    if legacy_path.exists():
        try:
            creds = json.loads(legacy_path.read_text())
            project_id = creds.get("project_id")
            project_name = creds.get("project_name")
            if project_id:
                return {"project_id": project_id, "project_name": project_name}
        except Exception:
            pass

    return {"project_id": None, "project_name": None}


def discover_capabilities(**kwargs) -> dict:
    """Aggregate capabilities across all PRISM subsystems.

    Returns a single dict with sections for each subsystem:
    search providers, datasets, trained models, pre-trained models,
    CALPHAD databases, simulation status, lab subscriptions, and plugins.
    """
    caps = {}

    # 1. Search providers
    try:
        from app.tools.search_engine.providers.registry import build_registry
        provider_reg = build_registry(skip_network=True)
        providers = provider_reg.get_all()
        caps["search_providers"] = [
            {"id": p.id, "name": getattr(p, "name", p.id), "type": getattr(getattr(p, "endpoint", None), "api_type", "unknown")}
            for p in providers
        ]
    except Exception:
        caps["search_providers"] = []

    # 2. Datasets
    try:
        from app.tools.data_collectors.store import DataStore
        store = DataStore()
        datasets = store.list_datasets()
        caps["datasets"] = [
            {"name": d["name"], "rows": d.get("rows", "?"), "columns": d.get("columns", [])}
            for d in datasets
        ]
    except Exception:
        caps["datasets"] = []

    # 3. Trained ML models
    try:
        from app.tools.ml.registry import ModelRegistry
        registry = ModelRegistry()
        models = registry.list_models()
        caps["trained_models"] = [
            {"property": m.get("property"), "algorithm": m.get("algorithm"),
             "r2": m.get("metrics", {}).get("r2")}
            for m in models
        ]
    except Exception:
        caps["trained_models"] = []

    # 4. Pre-trained GNNs
    try:
        from app.tools.ml.pretrained import list_pretrained_models
        caps["pretrained_models"] = [
            {"name": m["name"], "property": m["property"], "installed": m["installed"]}
            for m in list_pretrained_models()
        ]
    except Exception:
        caps["pretrained_models"] = []

    # 5. Feature backend
    try:
        from app.tools.ml.features import get_feature_backend
        caps["feature_backend"] = get_feature_backend()
    except Exception:
        caps["feature_backend"] = "unknown"

    # 6. CALPHAD databases
    try:
        from app.tools.simulation.calphad_bridge import check_calphad_available
        caps["calphad"] = {"available": check_calphad_available(), "databases": []}
        if caps["calphad"]["available"]:
            from app.tools.simulation.calphad_bridge import get_calphad_bridge
            bridge = get_calphad_bridge()
            caps["calphad"]["databases"] = [
                {"name": db["name"], "size_kb": db["size_kb"]}
                for db in bridge.databases.list_databases()
            ]
    except Exception:
        caps["calphad"] = {"available": False, "databases": []}

    # 7. Simulation (pyiron)
    try:
        from app.tools.simulation.bridge import check_pyiron_available
        caps["simulation"] = {"available": check_pyiron_available()}
        if caps["simulation"]["available"]:
            from app.tools.simulation.bridge import get_bridge
            bridge = get_bridge()
            caps["simulation"]["structures"] = len(bridge.structures.to_summary_list())
            caps["simulation"]["jobs"] = len(bridge.jobs.to_summary_list())
    except Exception:
        caps["simulation"] = {"available": False}

    # 8. Lab subscriptions
    try:
        from app.tools.labs import _load_subscriptions
        subs = _load_subscriptions()
        caps["lab_subscriptions"] = [
            {"service": s.get("service"), "name": s.get("name"), "has_key": bool(s.get("api_key"))}
            for s in subs
        ]
    except Exception:
        caps["lab_subscriptions"] = []

    # 9. Loaded plugins
    try:
        from app.plugins.loader import _loaded_plugins
        caps["plugins"] = list(_loaded_plugins) if _loaded_plugins else []
    except Exception:
        caps["plugins"] = []

    # 10. MARC27 Knowledge Plane
    try:
        client = _get_platform_client()
        if not client:
            raise RuntimeError("MARC27 platform not connected.")
        stats = client.knowledge.graph_stats()
        caps["marc27_knowledge"] = {
            "connected": True,
            "nodes": stats.get("nodes", 0),
            "edges": stats.get("edges", 0),
            "entity_types": stats.get("entity_types", 0),
        }
        embed_stats = client.knowledge.embedding_stats()
        caps["marc27_embeddings"] = {
            "count": embed_stats.get("embeddings", 0),
            "dimensions": 3072,
        }
    except Exception:
        caps["marc27_knowledge"] = {"connected": False}
        caps["marc27_embeddings"] = {"count": 0}

    # 11. Dynamic platform capability catalog + hosted model discovery
    try:
        client = _get_platform_client()
        if not client:
            raise RuntimeError("MARC27 platform not connected.")

        agent_capabilities = _read_platform_json(client, "/agent/capabilities")
        knowledge_capabilities = _read_platform_json(client, "/knowledge/capabilities")
        project_ctx = _load_project_context()

        hosted_models = []
        if project_ctx["project_id"]:
            hosted_models = _read_platform_json(
                client,
                f"/projects/{project_ctx['project_id']}/llm/models",
            )

        provider_names = sorted(
            {
                model.get("provider")
                for model in hosted_models
                if isinstance(model, dict) and model.get("provider")
            }
        )

        caps["marc27_platform"] = {
            "connected": True,
            "project_id": project_ctx["project_id"],
            "project_name": project_ctx["project_name"],
            "agent_capabilities": agent_capabilities,
            "knowledge_capabilities": knowledge_capabilities,
            # Keep the full dynamic model list here so discovery does not need a
            # second bespoke command path for hosted model inventory.
            "hosted_models": hosted_models,
            "hosted_model_count": len(hosted_models),
            "hosted_model_providers": provider_names,
        }
    except Exception as e:
        caps["marc27_platform"] = {
            "connected": False,
            "error": str(e),
            "project_id": None,
            "project_name": None,
            "hosted_models": [],
            "hosted_model_count": 0,
            "hosted_model_providers": [],
        }

    return caps


def _load_provider_descriptions() -> dict:
    """Load provider descriptions from overrides JSON."""
    try:
        import json
        from pathlib import Path
        overrides_path = Path(__file__).parent.parent / "search" / "providers" / "provider_overrides.json"
        if overrides_path.exists():
            data = json.loads(overrides_path.read_text())
            return {
                pid: pdata.get("description", "")
                for pid, pdata in data.get("overrides", {}).items()
                if pdata.get("enabled", True) and pdata.get("description")
            }
    except Exception:
        pass
    return {}


def capabilities_summary() -> str:
    """Generate a concise text summary for system prompt injection.

    Returns a short string describing what's available, suitable for
    appending to the agent's system prompt.
    """
    caps = discover_capabilities()
    lines = []

    # Search providers — include descriptions so agent knows what each has
    providers = caps.get("search_providers", [])
    if providers:
        descriptions = _load_provider_descriptions()
        provider_lines = []
        for p in providers:
            pid = p["id"]
            desc = descriptions.get(pid, "")
            if desc:
                provider_lines.append(f"  {pid}: {desc}")
            else:
                provider_lines.append(f"  {pid}")
        lines.append("Search providers:\n" + "\n".join(provider_lines))

    # Datasets
    datasets = caps.get("datasets", [])
    if datasets:
        ds_names = [d["name"] for d in datasets]
        lines.append(f"Datasets loaded: {', '.join(ds_names)}")

    # Models
    trained = caps.get("trained_models", [])
    if trained:
        descs = [f"{m['property']}/{m['algorithm']}" for m in trained]
        lines.append(f"Trained models: {', '.join(descs)}")

    pretrained = caps.get("pretrained_models", [])
    installed = [m["name"] for m in pretrained if m.get("installed")]
    if installed:
        lines.append(f"Pre-trained GNNs (installed): {', '.join(installed)}")

    lines.append(f"Feature backend: {caps.get('feature_backend', 'unknown')}")

    # CALPHAD
    calphad = caps.get("calphad", {})
    if calphad.get("available"):
        dbs = [d["name"] for d in calphad.get("databases", [])]
        if dbs:
            lines.append(f"CALPHAD databases: {', '.join(dbs)}")
        else:
            lines.append("CALPHAD: installed (no databases imported)")
    else:
        lines.append("CALPHAD: not installed")

    # Simulation
    sim = caps.get("simulation", {})
    if sim.get("available"):
        lines.append(f"Simulation (pyiron): available ({sim.get('structures', 0)} structures, {sim.get('jobs', 0)} jobs)")
    else:
        lines.append("Simulation (pyiron): not installed")

    # Labs
    subs = caps.get("lab_subscriptions", [])
    if subs:
        sub_names = [s["name"] for s in subs]
        lines.append(f"Lab subscriptions: {', '.join(sub_names)}")

    # Plugins
    plugins = caps.get("plugins", [])
    if plugins:
        lines.append(f"Plugins loaded: {', '.join(plugins)}")

    platform = caps.get("marc27_platform", {})
    if platform.get("connected"):
        provider_list = platform.get("hosted_model_providers", [])
        provider_text = ", ".join(provider_list) if provider_list else "unknown providers"
        lines.append(
            "Platform LLM models: "
            f"{platform.get('hosted_model_count', 0)} discovered across {provider_text}"
        )

    return "\n".join(lines)


def create_capabilities_tools(registry: ToolRegistry) -> None:
    """Register the capability discovery tool."""

    registry.register(Tool(
        name="discover_capabilities",
        description=(
            "Discover all available PRISM capabilities: search providers, datasets, "
            "trained models, pre-trained GNNs, CALPHAD databases, simulation status, "
            "lab subscriptions, loaded plugins, dynamic platform capability catalogs, "
            "and project-scoped hosted LLM models. Call this to understand what "
            "resources are available before planning a workflow."
        ),
        input_schema={"type": "object", "properties": {}},
        func=discover_capabilities,
    ))
