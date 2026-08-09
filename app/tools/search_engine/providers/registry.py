"""Provider registry -- discover, build, route queries."""
from __future__ import annotations

import hashlib
import logging
import threading
from collections.abc import Callable, Mapping
from pathlib import Path

from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
from app.tools.search_engine.providers.endpoint import ProviderEndpoint
from app.tools.search_engine.providers.optimade import OptimadeProvider
from app.tools.search_engine.providers.materials_project import MaterialsProjectProvider
from app.tools.search_engine.query import MaterialSearchQuery

logger = logging.getLogger(__name__)

# A factory takes the validated endpoint config and returns a ready Provider.
ProviderFactory = Callable[[ProviderEndpoint], Provider]

_PROVIDER_FACTORIES: dict[str, ProviderFactory] = {
    "optimade": OptimadeProvider,
    "mp_native": MaterialsProjectProvider,
}

# One reentrant lock guards BOTH the factory table and the plugin-load latch:
# registration is a check-then-set, and plugin imports (which call
# register_provider_factory on the same thread) run while the load holds this
# lock, so it must be reentrant.
_REGISTRY_LOCK = threading.RLock()


def register_provider_factory(api_type: str, factory: ProviderFactory) -> None:
    """Register a provider factory for an explicitly named ``api_type``.

    Registration is deliberately explicit: an endpoint whose ``api_type`` has
    no registered factory is skipped by ``from_endpoints`` with a WARNING
    naming the endpoint and api_type, instead of being silently dropped or
    guessed into the nearest adapter -- config routed at an adapter it was
    not written for would send wrong wire queries, not honest failures.

    Re-registering the SAME factory object under its api_type is a no-op so
    that ``importlib.reload``, test re-imports, and double-loading a module
    under two names all work. Registering a DIFFERENT factory for a taken
    api_type raises, because two factories for one api_type would mean one
    of them silently wins.
    """
    if not isinstance(api_type, str) or not api_type.strip():
        raise ValueError("provider factory must be registered under a non-empty api_type")
    if not callable(factory):
        raise ValueError("provider factory must be callable: (ProviderEndpoint) -> Provider")
    # Store the TRIMMED key: endpoints match on ep.api_type, which is never
    # whitespace-padded, so an untrimmed key would register a factory no
    # endpoint could ever reach.
    key = api_type.strip()
    with _REGISTRY_LOCK:  # check-then-set must be atomic across threads
        existing = _PROVIDER_FACTORIES.get(key)
        if existing is not None:
            if existing is factory:
                return  # idempotent: same factory, same key
            raise ValueError(f"provider factory already registered for api_type {key!r}")
        _PROVIDER_FACTORIES[key] = factory


def unregister_provider_factory(api_type: str) -> bool:
    """Remove a registered factory; returns True if one was removed.

    The supported teardown for tests and unloaded plugins -- do not pop
    ``_PROVIDER_FACTORIES`` directly. Removing a built-in ("optimade",
    "mp_native") disables that adapter for the rest of the process, so only
    remove keys you registered.
    """
    with _REGISTRY_LOCK:
        return _PROVIDER_FACTORIES.pop(api_type.strip(), None) is not None


# ---------------------------------------------------------------------------
# Adapter activation -- imports adapter modules so their
# register_provider_factory() calls actually run.
# ---------------------------------------------------------------------------

_USER_CONFIG_PATH = Path.home() / ".prism" / "providers.yaml"
_plugins_loaded = False
# Which config path the completed load read -- loading a test config must not
# latch out a later load of the real one (and vice versa).
_loaded_config_path: Path | None = None

# module name -> api_type keys its import registered. Lets a failed import
# roll back exactly what it added, and lets force=True unregister a plugin's
# keys before re-executing it (identity-based duplicate refusal would
# otherwise reject the re-registration of the freshly-created class).
_MODULE_REGISTRATIONS: dict[str, list[str]] = {}

# Infrastructure modules of this package -- never re-executed by force=True.
# Reloading base/endpoint would fork the Provider/ProviderEndpoint class
# identity out from under live registries; reloading registry would reset
# this very table and latch.
_NON_ADAPTER_MODULES = frozenset({"base", "discovery", "endpoint", "refresh", "registry"})


def load_provider_plugins(
    user_config_path: Path | None = None, force: bool = False
) -> None:
    """Import every provider adapter module so its registrations run.

    Plugin modules execute IN-PROCESS with the host's privileges the moment
    they are imported -- this is the requested drop-in contract, not a
    sandbox; list only adapters you would run as your own code.

    Called automatically by ``ProviderRegistry.from_endpoints`` (once per
    process and config path). Two sources, in deterministic order:

    1. Bundled: every public module in ``app/tools/search_engine/providers/``,
       alphabetically. Dropping ``foo.py`` that calls
       ``register_provider_factory("foo", FooProvider)`` at import time is the
       WHOLE integration -- no list, registry, or enum to edit. Modules whose
       names start with ``_`` or ``test`` are never imported.
    2. User: ``adapter_modules:`` in ``~/.prism/providers.yaml`` -- a list of
       either importable module names (``mycorp.prism_adapter``) or paths to
       ``.py`` files (resolved to absolute; prefer absolute paths in config,
       since relative ones depend on the process CWD), imported in listed
       order.

    ``force=True`` genuinely re-executes plugin code: package modules that
    registered a factory on a previous load, and every user adapter entry,
    are re-imported fresh (their previous registrations are unregistered
    first, then re-made by the re-executed code). Package infrastructure
    modules and already-imported package modules that never registered a
    factory are not re-executed.

    One broken adapter must not take down search: each import failure is
    logged at WARNING with the module name and error, any factory it managed
    to register before failing is rolled back, and loading continues.
    All imports happen at call time, never at module-import time, so this
    cannot create an import cycle with base/optimade/materials_project.
    """
    global _plugins_loaded, _loaded_config_path
    config_path = Path(user_config_path) if user_config_path else _USER_CONFIG_PATH
    with _REGISTRY_LOCK:
        if _plugins_loaded and not force and config_path == _loaded_config_path:
            return
        _import_package_adapters(force=force)
        _import_user_adapters(config_path, force=force)
        # Publish the latch only AFTER loading completes: a concurrent caller
        # that observed True mid-load would skip loading and permanently build
        # a registry missing every provider still being imported. (Concurrent
        # callers block on _REGISTRY_LOCK until this load is done.)
        _plugins_loaded = True
        _loaded_config_path = config_path


def _guarded_plugin_import(mod_name: str, label: str, do_import: Callable[[], None]) -> None:
    """Run one plugin import with rollback: snapshot the factory table, and on
    failure restore it so a plugin that registers and THEN raises does not
    leave a factory routing endpoints through a partially-initialised class."""
    snapshot = dict(_PROVIDER_FACTORIES)
    owned_before = _MODULE_REGISTRATIONS.get(mod_name)
    try:
        do_import()
    except Exception as e:
        added = [k for k in _PROVIDER_FACTORIES if k not in snapshot]
        # clear+update (not reassignment) keeps the shared dict object.
        _PROVIDER_FACTORIES.clear()
        _PROVIDER_FACTORIES.update(snapshot)
        # A failed force-reload popped the ownership entry before re-executing;
        # restore it so the NEXT force pass can still unregister-then-reload.
        if owned_before is not None:
            _MODULE_REGISTRATIONS[mod_name] = owned_before
        if added:
            logger.warning(
                "%s registered %r before failing; those registrations were rolled back",
                label, added,
            )
        logger.warning(
            "%s failed to import and was skipped: %s: %s",
            label, type(e).__name__, e,
        )
    else:
        added = [k for k in _PROVIDER_FACTORIES if k not in snapshot]
        if added:
            _MODULE_REGISTRATIONS[mod_name] = added


def _unregister_module_keys(mod_name: str) -> None:
    """Drop the factories a module registered, ahead of re-executing it."""
    for key in _MODULE_REGISTRATIONS.pop(mod_name, []):
        _PROVIDER_FACTORIES.pop(key, None)


def _import_package_adapters(force: bool = False) -> None:
    """Import every public sibling module of this package, alphabetically."""
    import importlib
    import pkgutil
    import sys

    from app.tools.search_engine import providers as pkg

    for info in sorted(pkgutil.iter_modules(pkg.__path__), key=lambda m: m.name):
        name = info.name
        if name.startswith("_") or name.startswith("test"):
            continue  # private modules and test files are never adapter entry points
        full_name = f"{pkg.__name__}.{name}"
        # force re-executes only modules that registered a factory before:
        # infrastructure and non-registering modules gain nothing from a
        # reload, and reloading base/endpoint/registry is actively harmful.
        if (
            force
            and name not in _NON_ADAPTER_MODULES
            and full_name in sys.modules
            and full_name in _MODULE_REGISTRATIONS
        ):
            def _reload(fn: str = full_name) -> None:
                _unregister_module_keys(fn)
                importlib.reload(sys.modules[fn])

            do_import = _reload
        else:
            def do_import(fn: str = full_name) -> None:
                importlib.import_module(fn)

        _guarded_plugin_import(
            full_name, f"Provider adapter module {name!r}", do_import
        )


def _import_user_adapters(config_path: Path, force: bool = False) -> None:
    """Import user adapter modules listed under ``adapter_modules:``."""
    import importlib
    import sys

    for entry in _read_user_adapter_modules(config_path):
        if entry.endswith(".py"):
            path = Path(entry).expanduser().resolve()
            mod_name = _user_adapter_module_name(path)
            _guarded_plugin_import(
                mod_name,
                f"User provider adapter {entry!r}",
                lambda p=path, f=force: _import_module_from_file(p, force=f),
            )
        else:
            if force and entry in sys.modules:
                def _reload(e: str = entry) -> None:
                    _unregister_module_keys(e)
                    importlib.reload(sys.modules[e])

                do_import = _reload
            else:
                def do_import(e: str = entry) -> None:
                    importlib.import_module(e)

            _guarded_plugin_import(
                entry, f"User provider adapter {entry!r}", do_import
            )


def _read_user_adapter_modules(config_path: Path) -> list[str]:
    """Read the ``adapter_modules`` list from providers.yaml, tolerantly."""
    if not config_path.exists():
        return []
    try:
        import yaml  # optional dep, same contract as discovery.load_user_providers
    except ImportError:
        logger.debug("PyYAML not installed; skipping adapter_modules in %s", config_path)
        return []
    try:
        data = yaml.safe_load(config_path.read_text())
    except Exception as e:
        logger.warning("Cannot read adapter_modules from %s: %s", config_path, e)
        return []
    if not isinstance(data, dict):
        return []
    modules = data.get("adapter_modules", [])
    if not isinstance(modules, list):
        logger.warning(
            "adapter_modules in %s must be a list of module names or .py paths, got %s",
            config_path, type(modules).__name__,
        )
        return []
    return [m for m in modules if isinstance(m, str) and m.strip()]


def _user_adapter_module_name(resolved_path: Path) -> str:
    """Synthetic module name for a user .py adapter, derived from the FULL
    resolved path: stem for readability, path digest for uniqueness --
    /opt/acme/adapter.py and /home/user/adapter.py must not collide."""
    digest = hashlib.sha256(str(resolved_path).encode()).hexdigest()[:12]
    return f"prism_user_adapter_{resolved_path.stem}_{digest}"


def _import_module_from_file(path: Path, force: bool = False) -> None:
    """Import a user .py file (already resolved) under its synthetic name."""
    import importlib.util
    import sys

    mod_name = _user_adapter_module_name(path)
    if mod_name in sys.modules:
        if not force:
            logger.warning(
                "User provider adapter %s is already loaded; duplicate entry "
                "skipped (force=True re-executes it)", path,
            )
            return
        # force: genuinely re-execute -- drop the cached module and the
        # factories its previous run registered, then run the file fresh.
        sys.modules.pop(mod_name, None)
        _unregister_module_keys(mod_name)
    spec = importlib.util.spec_from_file_location(mod_name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load a module spec from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[mod_name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(mod_name, None)
        raise


class ProviderRegistry:
    """Manages all registered providers."""

    def __init__(self):
        self._providers: dict[str, Provider] = {}

    def register(self, provider: Provider) -> None:
        self._providers[provider.id] = provider

    def get_all(self) -> list[Provider]:
        return list(self._providers.values())

    def get_capable(self, query: MaterialSearchQuery) -> list[Provider]:
        """Return only providers that can handle this query's filters."""
        capable = []
        for p in self._providers.values():
            if query.providers and p.id not in query.providers:
                continue
            if p.capabilities.can_handle(query):
                capable.append(p)
        return capable

    @classmethod
    def from_endpoints(cls, endpoints: list[dict]) -> ProviderRegistry:
        """Build registry from resolved endpoint dicts.

        Fault containment: one malformed entry, invalid config, or broken
        factory skips THAT entry and never aborts the whole build -- a bad
        element in a config array must not kill every other provider.
        Log levels are deliberate: configuration/validation problems and
        adapter/factory failures are WARNINGs naming the endpoint and reason
        (at INFO-level production logging a provider must not just vanish);
        DEBUG is reserved for genuinely uninteresting skips (enabled: false).
        """
        # Activate adapter modules (bundled drop-ins + user adapter_modules)
        # so their register_provider_factory() calls have run before dispatch.
        load_provider_plugins()
        reg = cls()
        for ep_data in endpoints:
            if not isinstance(ep_data, Mapping):
                logger.warning(
                    "Skipping malformed provider entry (not a mapping): %.120r",
                    ep_data,
                )
                continue
            if not ep_data.get("enabled", True):
                logger.debug("Skipping provider %s: disabled", ep_data.get("id"))
                continue
            if not ep_data.get("base_url"):
                logger.warning(
                    "Skipping provider %r: enabled but no base_url configured",
                    ep_data.get("id"),
                )
                continue
            try:
                ep = ProviderEndpoint.model_validate(dict(ep_data))
            except Exception as e:
                logger.warning(
                    "Skipping provider %r: invalid endpoint config: %s",
                    ep_data.get("id"), e,
                )
                continue
            factory = _PROVIDER_FACTORIES.get(ep.api_type)
            if factory is None:
                # The old closed dispatch dropped these entries in total
                # silence; an unregistered api_type is an adapter gap the
                # operator must be able to see. One bad entry must not kill
                # the registry, so warn and continue rather than raise.
                logger.warning(
                    "Skipping provider %r: no adapter registered for api_type %r "
                    "(register one with register_provider_factory)",
                    ep.id,
                    ep.api_type,
                )
                continue
            try:
                provider = factory(ep)
            except Exception as e:
                logger.warning(
                    "Skipping provider %r: factory for api_type %r raised %s: %s",
                    ep.id, ep.api_type, type(e).__name__, e,
                )
                continue
            if not isinstance(provider, Provider):
                logger.warning(
                    "Skipping provider %r: factory for api_type %r returned %s, "
                    "not a Provider",
                    ep.id, ep.api_type, type(provider).__name__,
                )
                continue
            reg.register(provider)
        return reg

    # Keep backward compat -- delegates to build_registry
    @classmethod
    def from_registry_json(cls) -> ProviderRegistry:
        """Legacy entry point -- calls build_registry()."""
        return build_registry()


def build_registry(
    cache_path=None,
    overrides_path=None,
    skip_network: bool = False,
) -> ProviderRegistry:
    """Build the provider registry from all three layers.

    Layer 1: Discovery cache (OPTIMADE auto-discovery)
    Layer 2: Bundled overrides (tiers, capabilities, URL corrections)
    Layer 3: Platform/marketplace providers + user overrides
    """
    from pathlib import Path
    from app.tools.search_engine.providers.discovery import (
        load_cache, save_cache, is_cache_fresh, discover_providers,
        load_overrides, apply_overrides, load_platform_providers,
        DEFAULT_CACHE_PATH,
    )
    import asyncio

    # --- Layer 1: OPTIMADE discovery ---
    c_path = cache_path or DEFAULT_CACHE_PATH
    cache = load_cache(c_path)

    if cache and cache.get("endpoints"):
        # Always prefer cache if it has data — re-discover in background later
        endpoints = cache["endpoints"]
        if not is_cache_fresh(cache) and not skip_network:
            # Stale cache: schedule background refresh, don't block startup
            logger.debug("Cache stale, using existing %d providers", len(endpoints))
    elif not skip_network:
        try:
            overrides_data = load_overrides(overrides_path)
            fallbacks = overrides_data.get("fallback_index_urls", {})
            endpoints = asyncio.run(discover_providers(fallback_index_urls=fallbacks))
            if endpoints:
                save_cache(endpoints, c_path)
            else:
                endpoints = []
        except Exception as e:
            logger.error("Discovery error: %s", e)
            endpoints = []
    else:
        endpoints = []

    # --- Layer 2: Bundled overrides ---
    overrides_data = load_overrides(overrides_path)
    overrides = overrides_data.get("overrides", {})
    defaults = overrides_data.get("defaults", {})
    url_corrections = overrides_data.get("url_corrections", {})
    resolved = apply_overrides(endpoints, overrides, defaults, url_corrections)

    # --- Layer 3: Platform providers + user overrides ---
    platform = load_platform_providers()
    resolved.extend(platform)

    return ProviderRegistry.from_endpoints(resolved)
