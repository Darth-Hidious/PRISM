"""Tests for MCP client configuration."""
import json
import tempfile
import pytest
from app.mcp_client import MCPClientConfig, load_mcp_config


class TestMCPClientConfig:
    def test_load_empty_config(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump({"mcpServers": {}}, f)
            f.flush()
            config = load_mcp_config(f.name)
        assert config.servers == {}

    def test_load_config_with_servers(self):
        cfg = {
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@anthropic/mcp-filesystem"],
                },
                "weather": {
                    "url": "https://weather.example.com/mcp",
                },
            }
        }
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump(cfg, f)
            f.flush()
            config = load_mcp_config(f.name)
        assert "filesystem" in config.servers
        assert "weather" in config.servers
        assert config.servers["filesystem"]["command"] == "npx"

    def test_default_config_path(self):
        config = MCPClientConfig()
        assert config.config_path.name == "mcp_servers.json"

    def test_load_nonexistent_config(self):
        config = load_mcp_config("/nonexistent/path.json")
        assert config.servers == {}
