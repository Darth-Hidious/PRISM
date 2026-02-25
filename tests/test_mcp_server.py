"""Tests for MCP server."""
import asyncio
import json
import pytest
from fastmcp import Client
from app.mcp_server import create_mcp_server


class TestMCPServer:
    def test_server_creation(self):
        server = create_mcp_server()
        assert server is not None
        assert server.name == "prism"

    def test_server_has_tools(self):
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                tools = await client.list_tools()
                return [t.name for t in tools]

        tool_names = asyncio.run(run())
        assert "search_optimade" in tool_names
        assert "query_materials_project" in tool_names
        assert "export_results_csv" in tool_names
        assert "predict_property" in tool_names
        assert "list_models" in tool_names
        assert len(tool_names) >= 10

    def test_server_has_resources(self):
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                resources = await client.list_resources()
                return [str(r.uri) for r in resources]

        uris = asyncio.run(run())
        assert any("sessions" in u for u in uris)
        assert any("tools" in u for u in uris)

    def test_tool_schema_has_descriptions(self):
        """Tool schemas should include parameter descriptions from input_schema."""
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                tools = await client.list_tools()
                return {t.name: t.inputSchema for t in tools}

        schemas = asyncio.run(run())
        # search_optimade has filter_string with a description
        search_schema = schemas["search_optimade"]
        assert "filter_string" in search_schema["properties"]
        assert search_schema["properties"]["filter_string"]["type"] == "string"
        # Required fields should be marked
        assert "filter_string" in search_schema.get("required", [])

    def test_tool_schema_required_optional(self):
        """Required and optional params should be correctly marked in schema."""
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                tools = await client.list_tools()
                return {t.name: t.inputSchema for t in tools}

        schemas = asyncio.run(run())
        # predict_property: formula is required, property_name and algorithm are optional
        predict_schema = schemas["predict_property"]
        assert "formula" in predict_schema.get("required", [])
        assert "property_name" not in predict_schema.get("required", [])
        assert predict_schema["properties"]["property_name"]["default"] is None

    def test_tools_resource_returns_json(self):
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                result = await client.read_resource("prism://tools")
                return result

        result = asyncio.run(run())
        # Result should be parseable JSON with tool names
        text = str(result)
        assert "search_optimade" in text
        assert "list_models" in text

    def test_server_has_dataset_and_model_resources(self):
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                resources = await client.list_resources()
                return [str(r.uri) for r in resources]

        uris = asyncio.run(run())
        assert any("datasets" in u for u in uris)
        assert any("models" in u for u in uris)

    def test_server_has_dataset_template_resource(self):
        server = create_mcp_server()
        client = Client(server)

        async def run():
            async with client:
                templates = await client.list_resource_templates()
                return [str(t.uriTemplate) for t in templates]

        templates = asyncio.run(run())
        assert any("datasets" in t for t in templates)
