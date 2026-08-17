"""Plugin discovery: entry points and local directory."""
import importlib
import importlib.util
import logging
import sys
from pathlib import Path
from typing import Optional

from app.plugins.registry import PluginRegistry

logger = logging.getLogger(__name__)

# Module-level record of loaded plugins (for capability discovery)
_loaded_plugins: list[str] = []


def discover_entry_point_plugins(registry: PluginRegistry) -> list[str]:
    """Discover plugins registered via pip entry points (group='prism.plugins')."""
    loaded = []
    try:
        from importlib.metadata import entry_points

        eps = entry_points()
        # Python 3.12+ returns SelectableGroups; earlier returns dict
        if hasattr(eps, "select"):
            plugin_eps = eps.select(group="prism.plugins")
        else:
            plugin_eps = eps.get("prism.plugins", [])

        for ep in plugin_eps:
            try:
                module = ep.load()
                registry.register_plugin(module, source=f"entrypoint:{ep.name}")
                loaded.append(ep.name)
            except Exception as error:
                # A broken plugin must not vanish silently: name it and the
                # error, keep loading the rest, and RECORD the failure so
                # `prism plugins list` / `failed_plugins()` can show it.
                registry.record_failure(ep.name, f"entrypoint:{ep.name}", error)
                logger.exception(
                    "plugin %r failed to load from entry point: %s", ep.name, error
                )
    except Exception as error:
        logger.exception("entry-point plugin discovery failed entirely: %s", error)
    return loaded


def discover_local_plugins(
    registry: PluginRegistry,
    plugin_dir: Optional[Path] = None,
) -> list[str]:
    """Import *.py files from ~/.prism/plugins/ and call register()."""
    if plugin_dir is None:
        plugin_dir = Path.home() / ".prism" / "plugins"
    if not plugin_dir.is_dir():
        return []

    loaded = []
    for py_file in sorted(plugin_dir.glob("*.py")):
        module_name = f"prism_plugin_{py_file.stem}"
        try:
            spec = importlib.util.spec_from_file_location(module_name, py_file)
            if spec is None or spec.loader is None:
                raise ImportError(f"no import loader available for {py_file}")
            module = importlib.util.module_from_spec(spec)
            sys.modules[module_name] = module
            spec.loader.exec_module(module)
            registry.register_plugin(module, source=f"local:{py_file.name}")
            loaded.append(py_file.stem)
        except Exception as error:
            # A broken plugin must not vanish silently: name it and the
            # error, keep loading the rest, and RECORD the failure so
            # `prism plugins list` / `failed_plugins()` can show it.
            registry.record_failure(py_file.stem, f"local:{py_file.name}", error)
            logger.exception(
                "plugin %r (%s) failed to load: %s",
                py_file.stem,
                py_file,
                error,
            )
            # Do not leave a half-executed module registered. `module` is put
            # into sys.modules BEFORE exec_module so the module can import
            # itself; if exec_module then raised, that entry is a corpse, and
            # any later `import prism_plugin_<stem>` would silently get the
            # broken object instead of re-importing or failing.
            sys.modules.pop(module_name, None)
    return loaded


def discover_all_plugins(registry: PluginRegistry) -> list[str]:
    """Run both entry-point and local discovery."""
    global _loaded_plugins
    loaded = discover_entry_point_plugins(registry)
    loaded.extend(discover_local_plugins(registry))
    _loaded_plugins = loaded
    return loaded
