"""Tests for ProviderRegistry -- build_registry, routing, custom registration."""
from unittest.mock import patch


def test_build_registry_from_cache(tmp_path):
    """build_registry loads from discovery cache + overrides."""
    from app.tools.search_engine.providers.registry import build_registry
    from app.tools.search_engine.providers.discovery import save_cache

    # Seed a fake cache
    endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"},
        {"id": "cod", "name": "COD", "base_url": "https://cod.org", "parent": "cod"},
    ]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)

    reg = build_registry(cache_path=cache_path, skip_network=True)
    providers = reg.get_all()
    assert len(providers) >= 2  # mp + cod + mp_native from overrides


def test_build_registry_includes_platform_providers(tmp_path):
    """Platform providers from catalog.json (Layer 3) are included."""
    from app.tools.search_engine.providers.registry import build_registry
    from app.tools.search_engine.providers.discovery import save_cache

    endpoints = [{"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"}]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)

    reg = build_registry(cache_path=cache_path, skip_network=True)
    ids = {p.id for p in reg.get_all()}
    # mp_native comes from catalog.json (Layer 3), not overrides
    assert "mp_native" in ids


def test_registry_get_capable():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
    from app.tools.search_engine.query import MaterialSearchQuery

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query): return []

    reg = ProviderRegistry()
    reg.register(FakeProvider())
    q = MaterialSearchQuery(elements=["Fe"])
    assert len(reg.get_capable(q)) == 1


def test_registry_register_custom():
    from app.tools.search_engine.providers.registry import ProviderRegistry
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities

    class FakeProvider(Provider):
        id = "fake"
        name = "Fake"
        capabilities = ProviderCapabilities(filterable_fields={"elements"})
        async def search(self, query): return []

    reg = ProviderRegistry()
    reg.register(FakeProvider())
    assert "fake" in {p.id for p in reg.get_all()}


def test_from_registry_json_backward_compat(tmp_path):
    """from_registry_json() now delegates to build_registry()."""
    from app.tools.search_engine.providers.registry import ProviderRegistry, build_registry
    from app.tools.search_engine.providers.discovery import save_cache

    # Seed cache so it doesn't hit network
    endpoints = [{"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"}]
    cache_path = tmp_path / "cache.json"
    save_cache(endpoints, path=cache_path)

    with patch("app.tools.search_engine.providers.registry.build_registry",
               wraps=lambda **kw: build_registry(cache_path=cache_path, skip_network=True)) as mock_build:
        # Can't easily test from_registry_json without network,
        # so just verify it calls build_registry
        reg = build_registry(cache_path=cache_path, skip_network=True)
        assert len(reg.get_all()) >= 1


def test_from_endpoints_dispatches_through_the_registered_factory_callback():
    """The open-adapter contract, pinned by IDENTITY: from_endpoints must call
    the registered factory callable and register EXACTLY the object it
    returned. An implementation that ignored the factory table (e.g. matched
    Provider.__subclasses__() against the api_type) could construct a
    same-typed provider but never THIS sentinel instance."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
    from app.tools.search_engine.providers.endpoint import ProviderEndpoint
    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        register_provider_factory,
        unregister_provider_factory,
    )

    class SentinelProvider(Provider):
        id = "spy"
        name = "Spy"
        capabilities = ProviderCapabilities()

        async def search(self, query):
            return []

    sentinel = SentinelProvider()  # built HERE, not by the implementation
    calls: list = []

    def spy_factory(endpoint):
        calls.append(endpoint)
        return sentinel

    register_provider_factory("spy_native", spy_factory)
    try:
        reg = ProviderRegistry.from_endpoints([
            {
                "id": "spy",
                "name": "Spy",
                "base_url": "https://spy.example.org",
                "api_type": "spy_native",
                "enabled": True,
            },
        ])
        # Identity, not just type: only genuine factory-callback dispatch can
        # register the exact object the spy returned.
        assert len(reg.get_all()) == 1
        assert reg.get_all()[0] is sentinel
        # And the factory received the VALIDATED endpoint model, same as
        # built-ins -- no new elif, no registry edit.
        assert len(calls) == 1
        assert isinstance(calls[0], ProviderEndpoint)
        assert calls[0].base_url == "https://spy.example.org"
    finally:
        # Supported teardown -- never pop the private dict directly.
        assert unregister_provider_factory("spy_native") is True


def test_from_endpoints_unregistered_api_type_warns_not_silent(caplog):
    """An endpoint with an unregistered api_type must be skipped WITH a
    WARNING naming the id and api_type -- the silent drop was the defect --
    and must not prevent other entries from registering."""
    import logging

    from app.tools.search_engine.providers.registry import ProviderRegistry

    with caplog.at_level(
        logging.WARNING, logger="app.tools.search_engine.providers.registry"
    ):
        reg = ProviderRegistry.from_endpoints([
            {
                "id": "ghost",
                "name": "Ghost",
                "base_url": "https://ghost.example.org",
                "api_type": "nobody_registered_this",
                "enabled": True,
            },
            {
                "id": "real_optimade",
                "name": "Real",
                "base_url": "https://real.example.org",
                "api_type": "optimade",
                "enabled": True,
            },
        ])

    # The unknown entry did not register; the known one still did.
    assert {p.id for p in reg.get_all()} == {"real_optimade"}
    warnings = [
        r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
    ]
    assert any(
        "ghost" in msg and "nobody_registered_this" in msg for msg in warnings
    ), f"expected a WARNING naming id and api_type, got: {warnings}"


def test_register_provider_factory_refusals():
    """Mirror of register_identity_plugin: invalid keys and duplicates are
    refused loudly, never absorbed."""
    import pytest

    from app.tools.search_engine.providers.registry import register_provider_factory

    with pytest.raises(ValueError, match="non-empty api_type"):
        register_provider_factory("   ", lambda ep: None)
    with pytest.raises(ValueError, match="callable"):
        register_provider_factory("x_native", "not a factory")
    # Built-ins are already registered through the same table.
    with pytest.raises(ValueError, match="already registered"):
        register_provider_factory("optimade", lambda ep: None)
    with pytest.raises(ValueError, match="already registered"):
        register_provider_factory("mp_native", lambda ep: None)


def test_register_provider_factory_lifecycle_on_a_non_builtin_key():
    """Lifecycle on a NON-built-in key (an implementation refusing only
    'optimade'/'mp_native' would pass the built-in-only test): re-registering
    the SAME factory object is an idempotent no-op (importlib.reload, test
    re-imports); a DIFFERENT factory for the taken key is refused."""
    import pytest

    from app.tools.search_engine.providers.registry import (
        register_provider_factory,
        unregister_provider_factory,
    )

    def f1(ep):
        return None

    def f2(ep):
        return None

    register_provider_factory("acme_native", f1)
    try:
        # Same object again: silent no-op, NOT a ValueError.
        register_provider_factory("acme_native", f1)
        # A different factory for the same key: refused, f1 keeps the key.
        with pytest.raises(ValueError, match="already registered"):
            register_provider_factory("acme_native", f2)
    finally:
        assert unregister_provider_factory("acme_native") is True
    # Unregistering again reports there was nothing to remove.
    assert unregister_provider_factory("acme_native") is False


def test_register_provider_factory_normalises_whitespace_in_the_key():
    """'  ws_native  ' must register under 'ws_native': endpoints match on
    the exact api_type string, so an untrimmed key would be a factory no
    endpoint could ever reach."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        register_provider_factory,
        unregister_provider_factory,
    )

    class WsProvider(Provider):
        def __init__(self, endpoint):
            self._endpoint = endpoint
            self.id = endpoint.id
            self.name = endpoint.name
            self.capabilities = ProviderCapabilities()

        async def search(self, query):
            return []

    register_provider_factory("  ws_native  ", WsProvider)
    try:
        reg = ProviderRegistry.from_endpoints([
            {
                "id": "ws",
                "name": "WS",
                "base_url": "https://ws.example.org",
                "api_type": "ws_native",
                "enabled": True,
            },
        ])
        assert {p.id for p in reg.get_all()} == {"ws"}
    finally:
        # The trimmed key is the stored one.
        assert unregister_provider_factory("ws_native") is True


def test_from_endpoints_survives_malformed_entries_with_warnings(caplog):
    """One malformed entry must never abort the whole build: the old code
    raised AttributeError on a None entry and returned NO registry. Non-
    mapping entries and an enabled entry without base_url are skipped with a
    WARNING; enabled:false stays quiet (DEBUG only -- a normal state)."""
    import logging

    from app.tools.search_engine.providers.registry import ProviderRegistry

    with caplog.at_level(
        logging.WARNING, logger="app.tools.search_engine.providers.registry"
    ):
        reg = ProviderRegistry.from_endpoints([
            None,  # the exact crash the review reproduced
            42,
            ["not", "a", "mapping"],
            {"id": "nourl", "name": "NoUrl", "api_type": "optimade", "enabled": True},
            {"id": "off", "name": "Off", "base_url": "https://off.example.org",
             "enabled": False},
            {"id": "ok", "name": "OK", "base_url": "https://ok.example.org",
             "api_type": "optimade", "enabled": True},
        ])

    assert {p.id for p in reg.get_all()} == {"ok"}
    warnings = [
        r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
    ]
    assert sum("not a mapping" in m for m in warnings) == 3, warnings
    assert any("nourl" in m and "base_url" in m for m in warnings), warnings
    # A disabled provider is a normal state, not a deployment problem.
    assert not any("'off'" in m for m in warnings), warnings


def test_from_endpoints_invalid_config_warns_naming_the_endpoint(caplog):
    """A validation failure (api_type: null, missing required field) used to
    vanish at DEBUG -- at INFO-level production logging the configured
    provider just disappeared. It must WARN naming the id and the reason."""
    import logging

    from app.tools.search_engine.providers.registry import ProviderRegistry

    with caplog.at_level(
        logging.WARNING, logger="app.tools.search_engine.providers.registry"
    ):
        reg = ProviderRegistry.from_endpoints([
            {"id": "badcfg", "name": "Bad", "base_url": "https://bad.example.org",
             "api_type": None, "enabled": True},
            {"id": "ok", "name": "OK", "base_url": "https://ok.example.org",
             "api_type": "optimade", "enabled": True},
        ])

    assert {p.id for p in reg.get_all()} == {"ok"}
    warnings = [
        r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
    ]
    assert any(
        "badcfg" in m and "invalid endpoint config" in m for m in warnings
    ), warnings


def test_from_endpoints_factory_failures_warn_and_do_not_kill_the_build(caplog):
    """A factory that raises, and one that returns a non-Provider, each skip
    THAT endpoint with a WARNING naming it and the reason; the healthy entry
    still builds. Both used to be dropped at DEBUG."""
    import logging

    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        register_provider_factory,
        unregister_provider_factory,
    )

    def exploding_factory(ep):
        raise RuntimeError("factory exploded")

    register_provider_factory("boom_native", exploding_factory)
    register_provider_factory("none_native", lambda ep: None)
    try:
        with caplog.at_level(
            logging.WARNING, logger="app.tools.search_engine.providers.registry"
        ):
            reg = ProviderRegistry.from_endpoints([
                {"id": "boom", "name": "Boom", "base_url": "https://boom.example.org",
                 "api_type": "boom_native", "enabled": True},
                {"id": "nothing", "name": "Nothing", "base_url": "https://n.example.org",
                 "api_type": "none_native", "enabled": True},
                {"id": "ok", "name": "OK", "base_url": "https://ok.example.org",
                 "api_type": "optimade", "enabled": True},
            ])
        assert {p.id for p in reg.get_all()} == {"ok"}
        warnings = [
            r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
        ]
        assert any("boom" in m and "factory exploded" in m for m in warnings), warnings
        assert any("nothing" in m and "not a Provider" in m for m in warnings), warnings
    finally:
        unregister_provider_factory("boom_native")
        unregister_provider_factory("none_native")


def test_late_registration_after_a_registry_was_built():
    """Registering an adapter AFTER a registry build must make the NEXT build
    pick it up -- dispatch reads the live factory table, not a copy frozen at
    import or first-build time."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        register_provider_factory,
        unregister_provider_factory,
    )

    entry = {"id": "late", "name": "Late", "base_url": "https://late.example.org",
             "api_type": "late_native", "enabled": True}
    # First build: adapter not yet registered -> entry skipped.
    reg1 = ProviderRegistry.from_endpoints([entry])
    assert reg1.get_all() == []

    class LateProvider(Provider):
        def __init__(self, endpoint):
            self._endpoint = endpoint
            self.id = endpoint.id
            self.name = endpoint.name
            self.capabilities = ProviderCapabilities()

        async def search(self, query):
            return []

    register_provider_factory("late_native", LateProvider)
    try:
        reg2 = ProviderRegistry.from_endpoints([entry])
        assert {p.id for p in reg2.get_all()} == {"late"}
    finally:
        assert unregister_provider_factory("late_native") is True


def test_dropped_in_package_adapter_module_is_discovered_and_activated(
    tmp_path, monkeypatch
):
    """The activation half, end to end: a NEW module dropped into providers/
    that registers a factory at import time is discovered, imported, and its
    api_type builds -- zero edits to registries, dispatch, or enums.

    Deliberately NO manual load_provider_plugins() call: from_endpoints
    itself must trigger the load (the pass-1 defect was exactly that it
    didn't). Hermetic: the drop-in lives in a tmp dir grafted onto the
    package __path__ (nothing written into the real source tree), the user
    config path points at a nonexistent tmp file, and the load latch is
    monkeypatched so it is restored afterwards."""
    import sys

    import app.tools.search_engine.providers as providers_pkg
    from app.tools.search_engine.providers import registry as registry_mod
    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        unregister_provider_factory,
    )

    mod_file = tmp_path / "zz_e2e_dropin_adapter.py"
    mod_name = f"{providers_pkg.__name__}.zz_e2e_dropin_adapter"
    mod_file.write_text(
        "from app.tools.search_engine.providers.base import Provider, ProviderCapabilities\n"
        "from app.tools.search_engine.providers.registry import register_provider_factory\n"
        "\n"
        "class ZzE2eProvider(Provider):\n"
        "    def __init__(self, endpoint):\n"
        "        self._endpoint = endpoint\n"
        "        self.id = endpoint.id\n"
        "        self.name = endpoint.name\n"
        "        self.capabilities = ProviderCapabilities()\n"
        "\n"
        "    async def search(self, query):\n"
        "        return []\n"
        "\n"
        "register_provider_factory('zz_e2e_native', ZzE2eProvider)\n"
    )
    # Graft the tmp dir onto the package search path: pkgutil.iter_modules
    # walks __path__, so the drop-in is discovered exactly like a file in the
    # real package dir -- without dirtying the working tree.
    monkeypatch.setattr(
        providers_pkg, "__path__", [*providers_pkg.__path__, str(tmp_path)]
    )
    monkeypatch.setattr(registry_mod, "_USER_CONFIG_PATH", tmp_path / "providers.yaml")
    # Reset the latch (restored by monkeypatch) so from_endpoints performs a
    # genuine load -- and MUST do so itself for the api_type to resolve.
    monkeypatch.setattr(registry_mod, "_plugins_loaded", False)
    monkeypatch.setattr(registry_mod, "_loaded_config_path", None)
    try:
        reg = ProviderRegistry.from_endpoints([
            {"id": "zz", "name": "ZZ", "base_url": "https://zz.example.org",
             "api_type": "zz_e2e_native", "enabled": True},
        ])
        assert {p.id for p in reg.get_all()} == {"zz"}
    finally:
        sys.modules.pop(mod_name, None)
        registry_mod._MODULE_REGISTRATIONS.pop(mod_name, None)
        unregister_provider_factory("zz_e2e_native")


def test_user_adapter_modules_one_broken_does_not_kill_the_others(tmp_path, caplog):
    """User plugins via adapter_modules in providers.yaml: a plugin that
    RAISES on import is skipped with a WARNING naming the module and error,
    and the healthy plugin listed after it still registers -- a broken
    third-party adapter cannot take down search."""
    import logging
    import sys

    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        load_provider_plugins,
        unregister_provider_factory,
    )

    bad = tmp_path / "bad_adapter.py"
    bad.write_text("raise RuntimeError('third-party adapter is broken')\n")
    good = tmp_path / "good_adapter.py"
    good.write_text(
        "from app.tools.search_engine.providers.base import Provider, ProviderCapabilities\n"
        "from app.tools.search_engine.providers.registry import register_provider_factory\n"
        "\n"
        "class GoodProvider(Provider):\n"
        "    def __init__(self, endpoint):\n"
        "        self._endpoint = endpoint\n"
        "        self.id = endpoint.id\n"
        "        self.name = endpoint.name\n"
        "        self.capabilities = ProviderCapabilities()\n"
        "\n"
        "    async def search(self, query):\n"
        "        return []\n"
        "\n"
        "register_provider_factory('user_good_native', GoodProvider)\n"
    )
    cfg = tmp_path / "providers.yaml"
    cfg.write_text(f"adapter_modules:\n  - {bad}\n  - {good}\n")

    try:
        with caplog.at_level(
            logging.WARNING, logger="app.tools.search_engine.providers.registry"
        ):
            load_provider_plugins(user_config_path=cfg, force=True)
        warnings = [
            r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
        ]
        # The broken one warned, naming the module and the error...
        assert any(
            "bad_adapter" in m and "third-party adapter is broken" in m
            for m in warnings
        ), warnings
        # ...and did NOT stop the good one, listed after it, from registering.
        reg = ProviderRegistry.from_endpoints([
            {"id": "usergood", "name": "UG", "base_url": "https://ug.example.org",
             "api_type": "user_good_native", "enabled": True},
        ])
        assert {p.id for p in reg.get_all()} == {"usergood"}
    finally:
        from app.tools.search_engine.providers.registry import _user_adapter_module_name

        sys.modules.pop(_user_adapter_module_name(good.resolve()), None)
        sys.modules.pop(_user_adapter_module_name(bad.resolve()), None)
        unregister_provider_factory("user_good_native")


def _user_adapter_src(api_type: str, version: str = "v1", prologue: str = "",
                      epilogue: str = "") -> str:
    """Source for a minimal user adapter registering ``api_type``. Instances
    carry ``version`` so re-execution (force=True) is observable."""
    return (
        prologue
        + "from app.tools.search_engine.providers.base import Provider, ProviderCapabilities\n"
        "from app.tools.search_engine.providers.registry import register_provider_factory\n"
        "\n"
        "class UserProvider(Provider):\n"
        "    def __init__(self, endpoint):\n"
        "        self._endpoint = endpoint\n"
        "        self.id = endpoint.id\n"
        "        self.name = endpoint.name\n"
        f"        self.version = {version!r}\n"
        "        self.capabilities = ProviderCapabilities()\n"
        "\n"
        "    async def search(self, query):\n"
        "        return []\n"
        "\n"
        f"register_provider_factory({api_type!r}, UserProvider)\n"
        + epilogue
    )


def _reset_latch(monkeypatch):
    """Clear the plugin-load latch for this test; monkeypatch restores it."""
    from app.tools.search_engine.providers import registry as registry_mod

    monkeypatch.setattr(registry_mod, "_plugins_loaded", False)
    monkeypatch.setattr(registry_mod, "_loaded_config_path", None)
    return registry_mod


def test_adapter_that_registers_then_raises_is_rolled_back(tmp_path, monkeypatch, caplog):
    """Item 1: a plugin that registers a factory and THEN raises must not
    leave that factory registered while being logged as skipped -- endpoints
    would route through a partially-initialised class. The registration is
    rolled back, and the rollback is named in a WARNING."""
    import logging

    from app.tools.search_engine.providers.registry import (
        load_provider_plugins,
        unregister_provider_factory,
    )

    _reset_latch(monkeypatch)
    plugin = tmp_path / "half_registered_adapter.py"
    plugin.write_text(_user_adapter_src(
        "half_native", epilogue="raise RuntimeError('exploded after registering')\n"
    ))
    cfg = tmp_path / "providers.yaml"
    cfg.write_text(f"adapter_modules:\n  - {plugin}\n")

    with caplog.at_level(
        logging.WARNING, logger="app.tools.search_engine.providers.registry"
    ):
        load_provider_plugins(user_config_path=cfg, force=True)

    # The partial registration did NOT survive the failed import.
    assert unregister_provider_factory("half_native") is False
    warnings = [
        r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
    ]
    assert any(
        "half_registered_adapter" in m and "exploded after registering" in m
        for m in warnings
    ), warnings
    assert any(
        "rolled back" in m and "half_native" in m for m in warnings
    ), warnings


def test_concurrent_load_blocks_until_the_first_load_finishes(tmp_path, monkeypatch):
    """Item 2: the load is guarded by a lock and the latch publishes only
    after loading completes. A second caller arriving mid-load must WAIT for
    the full factory table -- the old code set _plugins_loaded=True at the
    top, so the second caller skipped loading and permanently built a
    registry missing every provider still being imported."""
    import sys
    import threading

    from app.tools.search_engine.providers.registry import (
        _user_adapter_module_name,
        load_provider_plugins,
        unregister_provider_factory,
    )

    _reset_latch(monkeypatch)
    gate = {"entered": threading.Event(), "release": threading.Event()}
    monkeypatch.setattr(sys, "_prism_plugin_gate", gate, raising=False)
    plugin = tmp_path / "slow_gate_adapter.py"
    plugin.write_text(_user_adapter_src(
        "slowgate_native",
        prologue=(
            "import sys\n"
            "_gate = sys._prism_plugin_gate\n"
            "_gate['entered'].set()\n"
            "if not _gate['release'].wait(10):\n"
            "    raise RuntimeError('gate never released')\n"
        ),
    ))
    cfg = tmp_path / "providers.yaml"
    cfg.write_text(f"adapter_modules:\n  - {plugin}\n")

    t1 = threading.Thread(
        target=load_provider_plugins,
        kwargs={"user_config_path": cfg, "force": True},
    )
    t2 = threading.Thread(
        target=load_provider_plugins, kwargs={"user_config_path": cfg}
    )
    try:
        t1.start()
        assert gate["entered"].wait(5), "plugin import never started"
        t2.start()
        t2.join(0.5)
        # The pinned defect: t2 observed a latch published BEFORE loading
        # finished and returned immediately, missing slowgate_native.
        assert t2.is_alive(), (
            "second loader returned while the first load was still importing"
        )
    finally:
        gate["release"].set()
        t1.join(10)
        t2.join(10)
    assert not t1.is_alive() and not t2.is_alive()
    sys.modules.pop(_user_adapter_module_name(plugin.resolve()), None)
    assert unregister_provider_factory("slowgate_native") is True


def test_latch_publishes_only_after_loading_completes(tmp_path, monkeypatch):
    """Item 2: a load that dies partway must NOT leave the latch set --
    otherwise every later caller skips loading forever. (Per-plugin failures
    are contained; this simulates the loader infrastructure itself failing.)"""
    import pytest

    registry_mod = _reset_latch(monkeypatch)

    def explode(config_path, force=False):
        raise RuntimeError("load infrastructure fell over")

    monkeypatch.setattr(registry_mod, "_import_user_adapters", explode)
    with pytest.raises(RuntimeError, match="fell over"):
        registry_mod.load_provider_plugins(
            user_config_path=tmp_path / "providers.yaml", force=True
        )
    assert registry_mod._plugins_loaded is False, (
        "latch was published before loading completed"
    )


def test_force_reexecutes_a_changed_user_adapter_file(tmp_path, monkeypatch):
    """Item 3: force=True must genuinely re-execute plugin code. The old
    file-module path returned early when the synthetic name was already in
    sys.modules, so changed code never ran again."""
    import sys

    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        _user_adapter_module_name,
        load_provider_plugins,
        unregister_provider_factory,
    )

    _reset_latch(monkeypatch)
    plugin = tmp_path / "reexec_adapter.py"
    plugin.write_text(_user_adapter_src("reexec_native", version="v1"))
    cfg = tmp_path / "providers.yaml"
    cfg.write_text(f"adapter_modules:\n  - {plugin}\n")
    endpoint = {"id": "rx", "name": "RX", "base_url": "https://rx.example.org",
                "api_type": "reexec_native", "enabled": True}
    try:
        load_provider_plugins(user_config_path=cfg, force=True)
        reg = ProviderRegistry.from_endpoints([endpoint])
        assert reg.get_all()[0].version == "v1"

        # Change the code on disk (longer content, so no stale-bytecode
        # ambiguity) and force-reload: the NEW code must be what registers.
        plugin.write_text(_user_adapter_src(
            "reexec_native", version="v2",
            epilogue="# changed on disk after the first load\n",
        ))
        load_provider_plugins(user_config_path=cfg, force=True)
        reg = ProviderRegistry.from_endpoints([endpoint])
        assert reg.get_all()[0].version == "v2", (
            "force=True did not re-execute the changed adapter file"
        )
    finally:
        sys.modules.pop(_user_adapter_module_name(plugin.resolve()), None)
        unregister_provider_factory("reexec_native")


def test_force_reexecutes_a_changed_package_dropin(tmp_path, monkeypatch):
    """Item 3, package half: importlib.import_module returns the cached
    module, so force previously re-discovered names without re-executing
    changed drop-in code. A module that registered a factory is re-executed
    (its old registration unregistered first)."""
    import sys

    import app.tools.search_engine.providers as providers_pkg
    from app.tools.search_engine.providers import registry as registry_mod
    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        load_provider_plugins,
        unregister_provider_factory,
    )

    _reset_latch(monkeypatch)
    monkeypatch.setattr(
        providers_pkg, "__path__", [*providers_pkg.__path__, str(tmp_path)]
    )
    mod_file = tmp_path / "zz_pkg_reexec_adapter.py"
    mod_name = f"{providers_pkg.__name__}.zz_pkg_reexec_adapter"
    mod_file.write_text(_user_adapter_src("pkg_reexec_native", version="v1"))
    cfg = tmp_path / "providers.yaml"  # nonexistent: no user adapters
    endpoint = {"id": "pr", "name": "PR", "base_url": "https://pr.example.org",
                "api_type": "pkg_reexec_native", "enabled": True}
    try:
        load_provider_plugins(user_config_path=cfg, force=True)
        reg = ProviderRegistry.from_endpoints([endpoint])
        assert reg.get_all()[0].version == "v1"

        mod_file.write_text(_user_adapter_src(
            "pkg_reexec_native", version="v2",
            epilogue="# changed on disk after the first load\n",
        ))
        load_provider_plugins(user_config_path=cfg, force=True)
        reg = ProviderRegistry.from_endpoints([endpoint])
        assert reg.get_all()[0].version == "v2", (
            "force=True did not re-execute the changed package drop-in"
        )
    finally:
        sys.modules.pop(mod_name, None)
        registry_mod._MODULE_REGISTRATIONS.pop(mod_name, None)
        unregister_provider_factory("pkg_reexec_native")


def test_latch_is_per_config_path(tmp_path, monkeypatch):
    """Item 3: loading a test config must not mark plugins loaded GLOBALLY --
    the old latch ignored the path, so a later ordinary load (e.g.
    from_endpoints reading the real ~/.prism/providers.yaml) never ran."""
    import sys

    from app.tools.search_engine.providers.registry import (
        _user_adapter_module_name,
        load_provider_plugins,
        unregister_provider_factory,
    )

    registry_mod = _reset_latch(monkeypatch)
    plug_a = tmp_path / "cfg_a_adapter.py"
    plug_a.write_text(_user_adapter_src("cfga_native"))
    cfg_a = tmp_path / "providers_a.yaml"
    cfg_a.write_text(f"adapter_modules:\n  - {plug_a}\n")
    plug_b = tmp_path / "cfg_b_adapter.py"
    plug_b.write_text(_user_adapter_src("cfgb_native"))
    cfg_b = tmp_path / "providers_b.yaml"
    cfg_b.write_text(f"adapter_modules:\n  - {plug_b}\n")
    try:
        load_provider_plugins(user_config_path=cfg_a)
        assert "cfga_native" in registry_mod._PROVIDER_FACTORIES
        # NO force: a different config path must still load. The old
        # process-global latch made this a silent no-op.
        load_provider_plugins(user_config_path=cfg_b)
        assert "cfgb_native" in registry_mod._PROVIDER_FACTORIES, (
            "latch ignored the config path: second config never loaded"
        )
        # Same path again IS latched (no re-import, no duplicate warning).
        load_provider_plugins(user_config_path=cfg_b)
    finally:
        sys.modules.pop(_user_adapter_module_name(plug_a.resolve()), None)
        sys.modules.pop(_user_adapter_module_name(plug_b.resolve()), None)
        unregister_provider_factory("cfga_native")
        unregister_provider_factory("cfgb_native")


def test_same_stem_user_adapters_do_not_collide(tmp_path, monkeypatch):
    """Item 4: /opt/acme/adapter.py and /home/user/adapter.py used to map to
    ONE synthetic module name, so only the first ran -- silently. The name is
    now derived from the full resolved path; both must register."""
    import sys

    from app.tools.search_engine.providers.registry import (
        _user_adapter_module_name,
        load_provider_plugins,
        unregister_provider_factory,
    )

    _reset_latch(monkeypatch)
    dir_a = tmp_path / "opt_acme"
    dir_a.mkdir()
    plug_a = dir_a / "adapter.py"
    plug_a.write_text(_user_adapter_src("stem_a_native"))
    dir_b = tmp_path / "home_user"
    dir_b.mkdir()
    plug_b = dir_b / "adapter.py"  # SAME stem, different directory
    plug_b.write_text(_user_adapter_src("stem_b_native"))
    cfg = tmp_path / "providers.yaml"
    cfg.write_text(f"adapter_modules:\n  - {plug_a}\n  - {plug_b}\n")
    try:
        load_provider_plugins(user_config_path=cfg, force=True)
        removed = {
            k: unregister_provider_factory(k)
            for k in ("stem_a_native", "stem_b_native")
        }
        assert removed == {"stem_a_native": True, "stem_b_native": True}, (
            f"a same-stem adapter was silently swallowed by a name collision: {removed}"
        )
    finally:
        sys.modules.pop(_user_adapter_module_name(plug_a.resolve()), None)
        sys.modules.pop(_user_adapter_module_name(plug_b.resolve()), None)
        unregister_provider_factory("stem_a_native")
        unregister_provider_factory("stem_b_native")


def test_duplicate_user_adapter_entry_warns_when_skipped(tmp_path, monkeypatch, caplog):
    """Item 4: an entry whose module is already loaded is skipped WITH a
    warning, not dropped silently."""
    import logging
    import sys

    from app.tools.search_engine.providers.registry import (
        _user_adapter_module_name,
        load_provider_plugins,
        unregister_provider_factory,
    )

    _reset_latch(monkeypatch)
    plugin = tmp_path / "dup_adapter.py"
    plugin.write_text(_user_adapter_src("dup_native"))
    cfg = tmp_path / "providers.yaml"
    cfg.write_text(f"adapter_modules:\n  - {plugin}\n  - {plugin}\n")
    try:
        with caplog.at_level(
            logging.WARNING, logger="app.tools.search_engine.providers.registry"
        ):
            # force=False: with force, duplicates are re-executed instead.
            load_provider_plugins(user_config_path=cfg)
        warnings = [
            r.getMessage() for r in caplog.records if r.levelno >= logging.WARNING
        ]
        assert any(
            "already loaded" in m and "duplicate" in m for m in warnings
        ), f"duplicate adapter entry was dropped silently: {warnings}"
    finally:
        sys.modules.pop(_user_adapter_module_name(plugin.resolve()), None)
        unregister_provider_factory("dup_native")


def test_load_platform_providers_merges_marketplace_and_user(tmp_path):
    """load_platform_providers merges catalog + user overrides."""
    import json
    from app.tools.search_engine.providers.discovery import load_platform_providers

    catalog = {
        "_meta": {"version": "2.0.0"},
        "plugins": {
            "test_native": {
                "type": "provider",
                "name": "Test Native",
                "api_type": "test_native",
                "base_url": "https://test.org",
                "tier": 2,
                "enabled": True,
            }
        },
    }
    mp_path = tmp_path / "catalog.json"
    mp_path.write_text(json.dumps(catalog))

    # No user overrides file
    user_path = tmp_path / "providers.yaml"

    result = load_platform_providers(marketplace_path=mp_path, user_path=user_path)
    assert len(result) == 1
    assert result[0]["id"] == "test_native"
    assert result[0]["base_url"] == "https://test.org"


# ---------------------------------------------------------------------------
# Deliberate replacement -- the owner's named requirement: "I am able to
# remove OPTIMADE with my own systems later on."
# ---------------------------------------------------------------------------


def test_replace_provider_factory_swaps_out_optimade_in_production_dispatch():
    """THE requirement, proven at PRODUCTION dispatch: replace the built-in
    'optimade' factory and a real ProviderRegistry.from_endpoints -- not a
    registry built inside the test -- must construct OUR provider for an
    api_type 'optimade' endpoint. An implementation that hardcoded
    OptimadeProvider anywhere on the from_endpoints path dies here."""
    from app.tools.search_engine.providers.base import Provider, ProviderCapabilities
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    from app.tools.search_engine.providers.registry import (
        ProviderRegistry,
        replace_provider_factory,
    )

    class MyOwnImplementation(Provider):
        def __init__(self, endpoint):
            self._endpoint = endpoint
            self.id = endpoint.id
            self.name = endpoint.name
            self.capabilities = ProviderCapabilities()

        async def search(self, query):
            return []

    displaced = replace_provider_factory("optimade", MyOwnImplementation)
    try:
        # We displaced the genuine built-in, not some test residue.
        assert displaced is OptimadeProvider
        reg = ProviderRegistry.from_endpoints([
            {
                "id": "any_optimade_db",
                "name": "Any OPTIMADE DB",
                "base_url": "https://example.org/optimade",
                "api_type": "optimade",
                "enabled": True,
            },
        ])
        providers = reg.get_all()
        assert len(providers) == 1, "the endpoint must not vanish"
        assert isinstance(providers[0], MyOwnImplementation), (
            "production dispatch must construct the REPLACEMENT provider"
        )
        assert not isinstance(providers[0], OptimadeProvider)
    finally:
        # Restore the built-in for the rest of the session; replace returns
        # the displaced factory precisely so this round-trip is possible.
        assert replace_provider_factory("optimade", displaced) is MyOwnImplementation


def test_replace_provider_factory_refuses_a_free_key():
    """The strict half of the two-call contract: replace on an unregistered
    key refuses. A typo'd api_type ('optimde') must not silently ADD a second
    adapter while the built-in the caller meant to displace keeps running."""
    import pytest

    from app.tools.search_engine.providers.registry import replace_provider_factory

    with pytest.raises(ValueError, match="no provider factory registered"):
        replace_provider_factory("optimde", lambda ep: None)


def test_replace_provider_factory_logs_what_it_displaced(caplog):
    """A deliberate replacement is loud: it logs the key, the displaced
    factory, and the replacement."""
    import logging

    from app.tools.search_engine.providers.registry import (
        register_provider_factory,
        replace_provider_factory,
        unregister_provider_factory,
    )

    def original(ep):
        return None

    def substitute(ep):
        return None

    register_provider_factory("loudswap_native", original)
    try:
        with caplog.at_level(
            logging.INFO, logger="app.tools.search_engine.providers.registry"
        ):
            assert replace_provider_factory("loudswap_native", substitute) is original
        messages = [r.getMessage() for r in caplog.records]
        assert any(
            "loudswap_native" in m and "original" in m and "substitute" in m
            for m in messages
        ), f"replacement must log what it displaced, got: {messages}"
    finally:
        assert unregister_provider_factory("loudswap_native") is True


def test_endpoint_api_type_whitespace_matches_the_trimmed_factory_key():
    """Defect fix: factory keys are trimmed at registration but endpoint
    api_type was not, so a config entry '  optimade  ' matched no factory and
    the provider silently vanished. Both sides now normalise identically --
    the padded entry must construct a provider, not be skipped."""
    from app.tools.search_engine.providers.optimade import OptimadeProvider
    from app.tools.search_engine.providers.registry import ProviderRegistry

    reg = ProviderRegistry.from_endpoints([
        {
            "id": "padded",
            "name": "Padded",
            "base_url": "https://padded.example.org",
            "api_type": "  optimade  ",
            "enabled": True,
        },
    ])
    providers = reg.get_all()
    assert len(providers) == 1, "a whitespace-padded api_type must still route"
    assert isinstance(providers[0], OptimadeProvider)
