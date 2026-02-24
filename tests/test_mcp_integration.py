"""Tests for MCP client integration with agent."""
import json
import tempfile
import pytest
from app.mcp_client import MCPClientConfig, load_mcp_config, discover_and_register_mcp_tools
from app.tools.base import ToolRegistry


class TestMCPIntegration:
    def test_empty_config_registers_nothing(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump({"mcpServers": {}}, f)
            f.flush()
            config = load_mcp_config(f.name)
        registry = ToolRegistry()
        registered = discover_and_register_mcp_tools(registry, config)
        assert registered == []

    def test_unavailable_server_skipped(self):
        """If an MCP server is unreachable, it's silently skipped."""
        config = MCPClientConfig(
            servers={"fake": {"url": "http://localhost:99999/mcp"}}
        )
        registry = ToolRegistry()
        registered = discover_and_register_mcp_tools(registry, config)
        assert registered == []
