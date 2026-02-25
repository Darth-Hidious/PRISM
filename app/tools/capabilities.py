"""Capability discovery tool — unified view of everything available.

The agent calls this once at session start (or when asked) to learn what
databases, models, services, providers, and plugins are available. This
replaces calling 5+ separate list tools.
"""
import logging
from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)


def discover_capabilities(**kwargs) -> dict:
    """Aggregate capabilities across all PRISM subsystems.

    Returns a single dict with sections for each subsystem:
    search providers, datasets, trained models, pre-trained models,
    CALPHAD databases, simulation status, lab subscriptions, and plugins.
    """
    caps = {}

    # 1. Search providers
    try:
        from app.search.providers.registry import build_registry
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
        from app.data.store import DataStore
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
        from app.ml.registry import ModelRegistry
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
        from app.ml.pretrained import list_pretrained_models
        caps["pretrained_models"] = [
            {"name": m["name"], "property": m["property"], "installed": m["installed"]}
            for m in list_pretrained_models()
        ]
    except Exception:
        caps["pretrained_models"] = []

    # 5. Feature backend
    try:
        from app.ml.features import get_feature_backend
        caps["feature_backend"] = get_feature_backend()
    except Exception:
        caps["feature_backend"] = "unknown"

    # 6. CALPHAD databases
    try:
        from app.simulation.calphad_bridge import check_calphad_available
        caps["calphad"] = {"available": check_calphad_available(), "databases": []}
        if caps["calphad"]["available"]:
            from app.simulation.calphad_bridge import get_calphad_bridge
            bridge = get_calphad_bridge()
            caps["calphad"]["databases"] = [
                {"name": db["name"], "size_kb": db["size_kb"]}
                for db in bridge.databases.list_databases()
            ]
    except Exception:
        caps["calphad"] = {"available": False, "databases": []}

    # 7. Simulation (pyiron)
    try:
        from app.simulation.bridge import check_pyiron_available
        caps["simulation"] = {"available": check_pyiron_available()}
        if caps["simulation"]["available"]:
            from app.simulation.bridge import get_bridge
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

    return caps


def capabilities_summary() -> str:
    """Generate a concise text summary for system prompt injection.

    Returns a short string describing what's available, suitable for
    appending to the agent's system prompt.
    """
    caps = discover_capabilities()
    lines = []

    # Search providers
    providers = caps.get("search_providers", [])
    if providers:
        names = [p["id"] for p in providers]
        lines.append(f"Search providers: {', '.join(names)}")

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

    return "\n".join(lines)


def create_capabilities_tools(registry: ToolRegistry) -> None:
    """Register the capability discovery tool."""

    registry.register(Tool(
        name="discover_capabilities",
        description=(
            "Discover all available PRISM capabilities: search providers, datasets, "
            "trained models, pre-trained GNNs, CALPHAD databases, simulation status, "
            "lab subscriptions, and loaded plugins. Call this to understand what "
            "resources are available before planning a workflow."
        ),
        input_schema={"type": "object", "properties": {}},
        func=discover_capabilities,
    ))
