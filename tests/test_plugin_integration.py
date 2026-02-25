"""Integration tests for the plugin system end-to-end."""
import pytest
from pathlib import Path
from unittest.mock import patch

from app.plugins.bootstrap import build_full_registry
from app.plugins.registry import PluginRegistry
from app.plugins.loader import discover_local_plugins


class TestPluginIntegration:
    """End-to-end: local plugin file -> build_full_registry -> tool available."""

    def test_local_plugin_adds_tool(self, tmp_path):
        """A .py plugin in a temp dir should be discovered and its tool registered."""
        plugin_file = tmp_path / "integration_plugin.py"
        plugin_file.write_text(
            "from app.tools.base import Tool\n"
            "def register(registry):\n"
            "    registry.tool_registry.register(Tool(\n"
            "        name='integration_test_tool',\n"
            "        description='Added by integration test plugin',\n"
            "        input_schema={'type': 'object', 'properties': {}},\n"
            "        func=lambda **kw: {'status': 'ok'},\n"
            "    ))\n"
        )

        # Patch the loader's discover_local_plugins to use our tmp dir
        original_discover = discover_local_plugins

        def patched_discover(reg, plugin_dir=None):
            return original_discover(reg, plugin_dir=tmp_path)

        with patch("app.plugins.loader.discover_local_plugins", side_effect=patched_discover):
            registry = build_full_registry(enable_mcp=False, enable_plugins=True)

        names = {t.name for t in registry.list_tools()}
        assert "integration_test_tool" in names

        # Verify the tool actually works
        tool = registry.get("integration_test_tool")
        result = tool.execute()
        assert result == {"status": "ok"}

    def test_local_plugin_adds_algorithm(self, tmp_path):
        """A plugin can register a custom ML algorithm."""
        plugin_file = tmp_path / "algo_plugin.py"
        plugin_file.write_text(
            "def register(registry):\n"
            "    registry.algorithm_registry.register(\n"
            "        'custom_rf', 'Custom Random Forest',\n"
            "        lambda: type('FakeModel', (), {'fit': lambda s, X, y: None, 'predict': lambda s, X: X})(),\n"
            "    )\n"
        )

        reg = PluginRegistry()
        discover_local_plugins(reg, plugin_dir=tmp_path)
        assert reg.algorithm_registry.has("custom_rf")
        model = reg.algorithm_registry.get("custom_rf")
        assert hasattr(model, "fit")

    def test_local_plugin_adds_collector(self, tmp_path):
        """A plugin can register a custom data collector."""
        plugin_file = tmp_path / "collector_plugin.py"
        plugin_file.write_text(
            "from app.data.base_collector import DataCollector\n"
            "class MyCollector(DataCollector):\n"
            "    name = 'my_source'\n"
            "    def collect(self, **kwargs):\n"
            "        return [{'id': 'test-1', 'formula': 'H2O'}]\n"
            "def register(registry):\n"
            "    registry.collector_registry.register(MyCollector())\n"
        )

        reg = PluginRegistry()
        discover_local_plugins(reg, plugin_dir=tmp_path)
        collector = reg.collector_registry.get("my_source")
        records = collector.collect()
        assert len(records) == 1
        assert records[0]["formula"] == "H2O"

    def test_build_full_registry_without_plugins(self):
        """build_full_registry with plugins disabled still loads all built-in tools."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        names = {t.name for t in registry.list_tools()}
        # Core tools
        assert "search_materials" in names
        assert "import_dataset" in names
        # Skills as tools
        assert "acquire_materials" in names
        assert "materials_discovery" in names

    def test_build_full_registry_tool_count(self):
        """Verify we have a reasonable number of tools (no regression)."""
        registry = build_full_registry(enable_mcp=False, enable_plugins=False)
        tools = registry.list_tools()
        # At minimum: 4 data + 3 system + 2 viz + 2 prediction + 7 skills = 18
        assert len(tools) >= 18

    def test_bad_plugin_does_not_break_loading(self, tmp_path):
        """A broken plugin should not prevent other tools from loading."""
        bad_plugin = tmp_path / "broken.py"
        bad_plugin.write_text("raise RuntimeError('plugin is broken')\n")

        good_plugin = tmp_path / "good.py"
        good_plugin.write_text(
            "from app.tools.base import Tool\n"
            "def register(registry):\n"
            "    registry.tool_registry.register(Tool(\n"
            "        name='good_tool', description='works',\n"
            "        input_schema={}, func=lambda **kw: {},\n"
            "    ))\n"
        )

        reg = PluginRegistry()
        loaded = discover_local_plugins(reg, plugin_dir=tmp_path)
        # good.py loaded, broken.py skipped
        assert "good" in loaded
        assert "broken" not in loaded
        assert reg.tool_registry.get("good_tool").name == "good_tool"
