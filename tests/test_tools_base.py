"""Tests for Tool base class and ToolRegistry."""
import pytest
from app.tools.base import Tool, ToolRegistry


class TestTool:
    def test_tool_creation(self):
        def my_func(**kwargs):
            return {"result": kwargs.get("x", 0) + 1}

        tool = Tool(
            name="test_tool",
            description="A test tool",
            input_schema={
                "type": "object",
                "properties": {"x": {"type": "integer"}},
                "required": ["x"],
            },
            func=my_func,
        )
        assert tool.name == "test_tool"
        assert tool.description == "A test tool"

    def test_tool_execute(self):
        def add(**kwargs):
            return {"sum": kwargs["a"] + kwargs["b"]}

        tool = Tool(
            name="add",
            description="Add two numbers",
            input_schema={
                "type": "object",
                "properties": {
                    "a": {"type": "integer"},
                    "b": {"type": "integer"},
                },
                "required": ["a", "b"],
            },
            func=add,
        )
        result = tool.execute(a=2, b=3)
        assert result == {"sum": 5}


class TestToolRegistry:
    def test_register_and_get(self):
        registry = ToolRegistry()
        tool = Tool(
            name="my_tool",
            description="desc",
            input_schema={"type": "object", "properties": {}},
            func=lambda **kw: {},
        )
        registry.register(tool)
        assert registry.get("my_tool") is tool

    def test_get_unknown_raises(self):
        registry = ToolRegistry()
        with pytest.raises(KeyError):
            registry.get("nonexistent")

    def test_list_tools(self):
        registry = ToolRegistry()
        t1 = Tool(name="a", description="A", input_schema={}, func=lambda **kw: {})
        t2 = Tool(name="b", description="B", input_schema={}, func=lambda **kw: {})
        registry.register(t1)
        registry.register(t2)
        assert len(registry.list_tools()) == 2

    def test_to_anthropic_format(self):
        registry = ToolRegistry()
        tool = Tool(
            name="search",
            description="Search materials",
            input_schema={"type": "object", "properties": {"q": {"type": "string"}}},
            func=lambda **kw: {},
        )
        registry.register(tool)
        fmt = registry.to_anthropic_format()
        assert len(fmt) == 1
        assert fmt[0]["name"] == "search"
        assert "input_schema" in fmt[0]

    def test_to_openai_format(self):
        registry = ToolRegistry()
        tool = Tool(
            name="search",
            description="Search materials",
            input_schema={"type": "object", "properties": {"q": {"type": "string"}}},
            func=lambda **kw: {},
        )
        registry.register(tool)
        fmt = registry.to_openai_format()
        assert len(fmt) == 1
        assert fmt[0]["type"] == "function"
        assert fmt[0]["function"]["name"] == "search"
        assert "parameters" in fmt[0]["function"]
