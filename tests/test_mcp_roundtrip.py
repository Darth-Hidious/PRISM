"""End-to-end MCP test: server exposes tools, client calls them."""
import asyncio
import json
import pytest
from fastmcp import Client
from app.mcp_server import create_mcp_server


class TestMCPRoundTrip:
    def test_list_tools_via_client(self):
        """Client can discover all PRISM tools via MCP."""
        server = create_mcp_server()

        async def run():
            async with Client(server) as client:
                tools = await client.list_tools()
                return [t.name for t in tools]

        tool_names = asyncio.run(run())
        assert "search_optimade" in tool_names
        assert "predict_property" in tool_names
        assert "export_results_csv" in tool_names
        assert "list_models" in tool_names
        assert len(tool_names) >= 10

    def test_call_list_models_via_mcp(self):
        """Client can call list_models tool and get valid JSON back."""
        server = create_mcp_server()

        async def run():
            async with Client(server) as client:
                result = await client.call_tool("list_models", {})
                return result

        result = asyncio.run(run())
        # Result should be parseable JSON with "models" key
        text = result.content[0].text if result.content else str(result)
        data = json.loads(text)
        assert "models" in data

    def test_read_resources_via_client(self):
        """Client can read PRISM resources."""
        server = create_mcp_server()

        async def run():
            async with Client(server) as client:
                resources = await client.list_resources()
                return [str(r.uri) for r in resources]

        uris = asyncio.run(run())
        assert any("sessions" in u for u in uris)
        assert any("tools" in u for u in uris)
        assert any("datasets" in u for u in uris)
        assert any("models" in u for u in uris)

    def test_read_tools_resource_content(self):
        """Client can read the tools resource and get valid JSON."""
        server = create_mcp_server()

        async def run():
            async with Client(server) as client:
                result = await client.read_resource("prism://tools")
                # read_resource returns list of TextResourceContents
                return result[0].text

        text = asyncio.run(run())
        data = json.loads(text)
        tool_names = [t["name"] for t in data]
        assert "search_optimade" in tool_names
        assert "list_models" in tool_names
