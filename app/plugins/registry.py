"""PluginRegistry — aggregates all sub-registries for plugin use.

STANDARD PLUGIN CONTRACT (Part 3 convergence). Every PRISM extension plane
follows the same four rules; this module is the Python plane's reference
implementation of them:

1. DECLARE — a plugin carries an id (module name or file stem) and a source
   (``entrypoint:<name>`` or ``local:<file>``), recorded when it loads.
2. DISCOVER — fixed locations only: pip entry points in group
   ``prism.plugins`` and ``~/.prism/plugins/*.py``. Missing locations mean
   zero plugins, never an error.
3. FAIL — loudly, named, isolated: one broken plugin is logged with its name
   and error, the rest keep loading, and the failure is RECORDED here
   (``failed_plugins()``), not just dropped into a log file.
4. LIST — ``loaded_plugins()`` / ``failed_plugins()`` are the queryable
   inventory; ``prism plugins list`` (CLI, TUI ``/plugins list``, agent
   ``plugins`` tool) surfaces them beside every other plane's inventory.

Registration is ALL-OR-NOTHING (Part 2 defect fix): a ``register()`` that
registers three tools and raises on the fourth leaves NO tools behind —
the sub-registries are rolled back to their pre-call state and the plugin
is recorded as failed with its error. The old behaviour kept the three
tools live in the shared registries while ``loaded_plugins()`` reported the
plugin as absent: a half-state that was neither loaded nor not-loaded.
"""
import logging
from dataclasses import dataclass, field

from app.tools.base import ToolRegistry
from app.tools.skills.base import SkillRegistry
from app.tools.data_collectors.base_collector import CollectorRegistry
from app.tools.ml.algorithm_registry import AlgorithmRegistry
from app.tools.search_engine.providers.registry import ProviderRegistry

logger = logging.getLogger(__name__)


@dataclass
class PluginRegistry:
    """Facade that plugin authors receive in their ``register()`` callback."""

    tool_registry: ToolRegistry = field(default_factory=ToolRegistry)
    skill_registry: SkillRegistry = field(default_factory=SkillRegistry)
    collector_registry: CollectorRegistry = field(default_factory=CollectorRegistry)
    algorithm_registry: AlgorithmRegistry = field(default_factory=AlgorithmRegistry)
    provider_registry: ProviderRegistry = field(default_factory=ProviderRegistry)

    # Tracks which plugins have been loaded (name -> module path)
    _loaded: dict = field(default_factory=dict, repr=False)
    # Tracks which plugins FAILED and why (name -> "source: error"), so a
    # failure is queryable state (contract rule 3), not a log line only.
    _failed: dict = field(default_factory=dict, repr=False)

    # ── Rollback support ────────────────────────────────────────────
    # The sub-registries are plain dict-backed; a plugin may register into
    # any of them directly, so all-or-nothing is enforced by snapshotting
    # their internal tables before the plugin's ``register()`` runs and
    # restoring them on failure — the same shape the search-provider
    # loader's ``_guarded_plugin_import`` uses for the factory table.

    def _snapshot(self) -> dict:
        """Copy every sub-registry's table (and the provider factory table)."""
        from app.tools.search_engine.providers import registry as provider_plane

        return {
            "tools": dict(self.tool_registry._tools),
            "skills": dict(self.skill_registry._skills),
            "collectors": dict(self.collector_registry._collectors),
            "algorithms": dict(self.algorithm_registry._algorithms),
            "providers": dict(self.provider_registry._providers),
            "provider_factories": dict(provider_plane._PROVIDER_FACTORIES),
        }

    def _restore(self, snapshot: dict) -> None:
        """Restore the tables in place (clear+update keeps shared objects)."""
        from app.tools.search_engine.providers import registry as provider_plane

        self.tool_registry._tools.clear()
        self.tool_registry._tools.update(snapshot["tools"])
        self.skill_registry._skills.clear()
        self.skill_registry._skills.update(snapshot["skills"])
        self.collector_registry._collectors.clear()
        self.collector_registry._collectors.update(snapshot["collectors"])
        self.algorithm_registry._algorithms.clear()
        self.algorithm_registry._algorithms.update(snapshot["algorithms"])
        self.provider_registry._providers.clear()
        self.provider_registry._providers.update(snapshot["providers"])
        provider_plane._PROVIDER_FACTORIES.clear()
        provider_plane._PROVIDER_FACTORIES.update(snapshot["provider_factories"])

    def register_plugin(self, plugin_module, source: str = "unknown") -> None:
        """Call plugin_module.register(self), all-or-nothing.

        Raises whatever ``register()`` raised after rolling every
        sub-registry back to its pre-call state; the caller (the loader)
        names the plugin, records the failure, and keeps loading the rest.
        """
        register_fn = getattr(plugin_module, "register", None)
        if register_fn is None:
            return
        name = getattr(plugin_module, "__name__", source)
        snapshot = self._snapshot()
        try:
            register_fn(self)
        except Exception as error:
            self._restore(snapshot)
            self.record_failure(name, source, error)
            logger.exception(
                "plugin %r (source %s) failed during register(); its partial "
                "registrations were rolled back",
                name,
                source,
            )
            raise
        self._loaded[name] = source
        self._failed.pop(name, None)

    def record_failure(self, name: str, source: str, error: BaseException | str) -> None:
        """Record a plugin that failed to load (import or register).

        Called by the loader in its per-plugin except block, so the failure
        is queryable via ``failed_plugins()`` (contract rule 3).
        """
        message = error if isinstance(error, str) else f"{type(error).__name__}: {error}"
        self._failed[name] = f"{source}: {message}"

    def loaded_plugins(self) -> dict:
        """Return dict of loaded plugin names -> source."""
        return dict(self._loaded)

    def failed_plugins(self) -> dict:
        """Return dict of failed plugin names -> "source: error"."""
        return dict(self._failed)
