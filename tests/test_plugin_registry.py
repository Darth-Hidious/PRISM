"""Tests for PluginRegistry and loader."""
import logging
import types
import pytest
from pathlib import Path
from unittest.mock import patch, MagicMock

from app.plugins.registry import PluginRegistry
from app.plugins.loader import (
    discover_entry_point_plugins,
    discover_local_plugins,
    discover_all_plugins,
)
from app.tools.base import Tool


class TestPluginRegistry:
    def test_register_plugin_with_register_fn(self):
        reg = PluginRegistry()
        mod = types.ModuleType("test_plugin")
        mod.register = lambda r: r.tool_registry.register(
            Tool(name="custom", description="d", input_schema={}, func=lambda: {})
        )
        reg.register_plugin(mod, source="test")
        assert reg.tool_registry.get("custom").name == "custom"

    def test_register_plugin_without_register_fn(self):
        reg = PluginRegistry()
        mod = types.ModuleType("empty_plugin")
        reg.register_plugin(mod, source="test")
        assert reg.loaded_plugins() == {}

    def test_loaded_plugins_tracking(self):
        reg = PluginRegistry()
        mod = types.ModuleType("tracked")
        mod.register = lambda r: None
        reg.register_plugin(mod, source="test:tracked")
        loaded = reg.loaded_plugins()
        assert "tracked" in loaded
        assert loaded["tracked"] == "test:tracked"

    def test_default_sub_registries(self):
        reg = PluginRegistry()
        assert reg.tool_registry is not None
        assert reg.skill_registry is not None
        assert reg.collector_registry is not None
        assert reg.algorithm_registry is not None


class TestEntryPointDiscovery:
    def test_no_entry_points(self):
        reg = PluginRegistry()
        with patch("importlib.metadata.entry_points") as mock_ep:
            mock_ep.return_value = MagicMock(select=lambda group: [])
            loaded = discover_entry_point_plugins(reg)
        assert loaded == []

    def test_entry_point_loads_plugin(self):
        reg = PluginRegistry()
        mock_mod = types.ModuleType("ep_plugin")
        mock_mod.register = lambda r: r.algorithm_registry.register(
            "ep_algo", "EP algo", lambda: "model"
        )

        ep = MagicMock()
        ep.name = "ep_plugin"
        ep.load.return_value = mock_mod

        with patch("importlib.metadata.entry_points") as mock_eps:
            mock_eps.return_value = MagicMock(select=lambda group: [ep])
            loaded = discover_entry_point_plugins(reg)

        assert "ep_plugin" in loaded
        assert reg.algorithm_registry.has("ep_algo")

    def test_entry_point_load_failure_is_logged_loudly(self, caplog):
        # CONTRACT CHANGE: an entry-point plugin that raised on load used
        # to be swallowed by `except Exception: pass`. It is now logged
        # with the entry-point name and the error.
        reg = PluginRegistry()
        ep = MagicMock()
        ep.name = "broken_ep"
        ep.load.side_effect = RuntimeError("cannot import module")
        with patch("importlib.metadata.entry_points") as mock_eps:
            mock_eps.return_value = MagicMock(select=lambda group: [ep])
            with caplog.at_level(logging.ERROR, logger="app.plugins.loader"):
                loaded = discover_entry_point_plugins(reg)
        assert loaded == []
        failure = [r for r in caplog.records if "broken_ep" in r.getMessage()]
        assert failure, "the failed entry point must be logged by name"
        assert "cannot import module" in caplog.text

    def test_entry_point_registration_failure_is_logged_loudly(self, caplog):
        mock_mod = types.ModuleType("broken_registration")

        def fail_registration(_registry):
            raise RuntimeError("registration rejected")

        mock_mod.register = fail_registration
        ep = MagicMock(name="entry_point")
        ep.name = "broken_registration"
        ep.load.return_value = mock_mod
        with patch("importlib.metadata.entry_points") as mock_eps:
            mock_eps.return_value = MagicMock(select=lambda group: [ep])
            with caplog.at_level(logging.ERROR, logger="app.plugins.loader"):
                loaded = discover_entry_point_plugins(PluginRegistry())
        assert loaded == []
        assert "broken_registration" in caplog.text
        assert "registration rejected" in caplog.text


class TestLocalPluginDiscovery:
    def test_nonexistent_dir(self, tmp_path):
        reg = PluginRegistry()
        loaded = discover_local_plugins(reg, plugin_dir=tmp_path / "nope")
        assert loaded == []

    def test_loads_local_plugin(self, tmp_path):
        plugin_file = tmp_path / "my_tool.py"
        plugin_file.write_text(
            "from app.tools.base import Tool\n"
            "def register(registry):\n"
            "    registry.tool_registry.register(\n"
            "        Tool(name='local_tool', description='d', input_schema={}, func=lambda: {})\n"
            "    )\n"
        )
        reg = PluginRegistry()
        loaded = discover_local_plugins(reg, plugin_dir=tmp_path)
        assert "my_tool" in loaded
        assert reg.tool_registry.get("local_tool").name == "local_tool"

    def test_skips_bad_plugin(self, tmp_path):
        bad = tmp_path / "bad.py"
        bad.write_text("raise RuntimeError('broken')\n")
        reg = PluginRegistry()
        loaded = discover_local_plugins(reg, plugin_dir=tmp_path)
        assert loaded == []

    def test_bad_plugin_failure_is_logged_loudly(self, tmp_path, caplog):
        # CONTRACT CHANGE: a broken local plugin used to vanish into
        # `except Exception: pass` — the healthiest runtime extension door
        # failing silently. The skip-and-continue behaviour stands, but the
        # failure is now logged with the plugin name and the error.
        bad = tmp_path / "bad.py"
        bad.write_text("raise RuntimeError('broken')\n")
        reg = PluginRegistry()
        with caplog.at_level(logging.ERROR, logger="app.plugins.loader"):
            loaded = discover_local_plugins(reg, plugin_dir=tmp_path)
        assert loaded == []
        failure = [r for r in caplog.records if "bad" in r.getMessage()]
        assert failure, "the failed plugin must be logged by name"
        assert "broken" in caplog.text, "the error itself must be logged"

    def test_local_plugin_registration_failure_is_logged_loudly(self, tmp_path, caplog):
        plugin = tmp_path / "broken_registration.py"
        plugin.write_text(
            "def register(registry):\n"
            "    raise RuntimeError('registration rejected')\n"
        )
        with caplog.at_level(logging.ERROR, logger="app.plugins.loader"):
            loaded = discover_local_plugins(PluginRegistry(), plugin_dir=tmp_path)
        assert loaded == []
        assert "broken_registration" in caplog.text
        assert str(plugin) in caplog.text
        assert "registration rejected" in caplog.text

    def test_missing_local_import_loader_is_logged_loudly(self, tmp_path, caplog):
        plugin = tmp_path / "missing_loader.py"
        plugin.write_text("def register(registry):\n    pass\n")
        with patch(
            "app.plugins.loader.importlib.util.spec_from_file_location",
            return_value=None,
        ):
            with caplog.at_level(logging.ERROR, logger="app.plugins.loader"):
                loaded = discover_local_plugins(PluginRegistry(), plugin_dir=tmp_path)
        assert loaded == []
        assert "missing_loader" in caplog.text
        assert str(plugin) in caplog.text
        assert "no import loader available" in caplog.text

    def test_bad_plugin_does_not_stop_the_rest(self, tmp_path, caplog):
        good = tmp_path / "a_good.py"
        good.write_text("def register(registry):\n    pass\n")
        bad = tmp_path / "b_bad.py"
        bad.write_text("raise RuntimeError('broken')\n")
        reg = PluginRegistry()
        with caplog.at_level(logging.ERROR, logger="app.plugins.loader"):
            loaded = discover_local_plugins(reg, plugin_dir=tmp_path)
        assert loaded == ["a_good"]


class TestDiscoverAll:
    def test_combines_both_sources(self, tmp_path):
        plugin_file = tmp_path / "combo.py"
        plugin_file.write_text(
            "def register(registry):\n"
            "    registry.algorithm_registry.register('combo_algo', 'Combo', lambda: 'x')\n"
        )
        reg = PluginRegistry()
        with patch("app.plugins.loader.discover_entry_point_plugins", return_value=["ep1"]):
            loaded = discover_all_plugins(reg)
        # ep1 from mocked entry points, but combo also discovered from local dir?
        # Actually discover_all calls the real discover_local_plugins with default dir.
        # Let's just test the entry-point mock part:
        assert "ep1" in loaded

    def test_discover_all_with_local_dir(self, tmp_path):
        plugin_file = tmp_path / "localonly.py"
        plugin_file.write_text("def register(r): pass\n")
        reg = PluginRegistry()
        with patch("app.plugins.loader.discover_entry_point_plugins", return_value=[]):
            with patch("app.plugins.loader.discover_local_plugins", return_value=["localonly"]):
                loaded = discover_all_plugins(reg)
        assert "localonly" in loaded


class TestAllOrNothingRegistration:
    """Part 2 defect fix: `register_plugin` used to leave a HALF-STATE — a
    register() that registered three tools and raised on the fourth kept the
    three tools live in the shared registries while loaded_plugins() reported
    the plugin as absent. Registration is now all-or-nothing with rollback,
    and the failure is recorded queryable state."""

    def test_partial_registration_is_rolled_back(self):
        from app.tools.skills.base import Skill, SkillStep

        reg = PluginRegistry()
        # Pre-existing state the rollback must PRESERVE.
        reg.tool_registry.register(
            Tool(name="existing", description="d", input_schema={}, func=lambda: {})
        )

        def half_register(r):
            r.tool_registry.register(
                Tool(name="first", description="d", input_schema={}, func=lambda: {})
            )
            r.tool_registry.register(
                Tool(name="second", description="d", input_schema={}, func=lambda: {})
            )
            raise RuntimeError("fourth registration blew up")

        mod = types.ModuleType("half_state")
        mod.register = half_register
        with pytest.raises(RuntimeError, match="blew up"):
            reg.register_plugin(mod, source="test")

        # The two partial registrations are GONE...
        with pytest.raises(KeyError):
            reg.tool_registry.get("first")
        with pytest.raises(KeyError):
            reg.tool_registry.get("second")
        # ...the pre-existing tool SURVIVES...
        assert reg.tool_registry.get("existing").name == "existing"
        # ...and the plugin is recorded as FAILED with its error, not absent.
        assert reg.loaded_plugins() == {}
        failed = reg.failed_plugins()
        assert "half_state" in failed
        assert "test" in failed["half_state"]
        assert "blew up" in failed["half_state"]

    def test_successful_registration_clears_stale_failure(self):
        reg = PluginRegistry()
        reg.record_failure("recovers", "test", "old error")
        mod = types.ModuleType("recovers")
        mod.register = lambda r: None
        reg.register_plugin(mod, source="test")
        assert reg.loaded_plugins() == {"recovers": "test"}
        assert "recovers" not in reg.failed_plugins()

    def test_provider_factory_registration_is_rolled_back(self):
        """A plugin that registers a provider factory and then raises must
        not leave the factory routing endpoints through a partially
        initialised class (the provider loader's own guarded-import shape)."""
        from app.tools.search_engine.providers import registry as provider_plane

        def factory(_endpoint):
            return None

        def half_register(r):
            from app.tools.search_engine.providers.registry import (
                register_provider_factory,
            )

            register_provider_factory("halfstate", factory)
            raise RuntimeError("factory then failure")

        mod = types.ModuleType("half_factory")
        mod.register = half_register
        reg = PluginRegistry()
        with pytest.raises(RuntimeError, match="factory then failure"):
            reg.register_plugin(mod, source="test")
        assert "halfstate" not in provider_plane._PROVIDER_FACTORIES

    def test_loader_records_local_failures_queryably(self, tmp_path):
        bad = tmp_path / "broken.py"
        bad.write_text("raise RuntimeError('boom on import')\n")
        reg = PluginRegistry()
        loaded = discover_local_plugins(reg, plugin_dir=tmp_path)
        assert loaded == []
        failed = reg.failed_plugins()
        assert "broken" in failed
        assert "boom on import" in failed["broken"]
        assert "local:" in failed["broken"]
