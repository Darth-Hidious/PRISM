"""PluginRegistry — aggregates all sub-registries for plugin use."""
from dataclasses import dataclass, field
from typing import Optional

from app.tools.base import ToolRegistry
from app.skills.base import SkillRegistry
from app.data.base_collector import CollectorRegistry
from app.ml.algorithm_registry import AlgorithmRegistry


@dataclass
class PluginRegistry:
    """Facade that plugin authors receive in their ``register()`` callback."""

    tool_registry: ToolRegistry = field(default_factory=ToolRegistry)
    skill_registry: SkillRegistry = field(default_factory=SkillRegistry)
    collector_registry: CollectorRegistry = field(default_factory=CollectorRegistry)
    algorithm_registry: AlgorithmRegistry = field(default_factory=AlgorithmRegistry)

    # Tracks which plugins have been loaded (name -> module path)
    _loaded: dict = field(default_factory=dict, repr=False)

    def register_plugin(self, plugin_module, source: str = "unknown") -> None:
        """Call plugin_module.register(self) if it has a register function."""
        register_fn = getattr(plugin_module, "register", None)
        if register_fn is None:
            return
        register_fn(self)
        name = getattr(plugin_module, "__name__", source)
        self._loaded[name] = source

    def loaded_plugins(self) -> dict:
        """Return dict of loaded plugin names -> source."""
        return dict(self._loaded)
