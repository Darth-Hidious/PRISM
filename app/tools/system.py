"""System tools: file I/O, web search, user interaction."""
from pathlib import Path
from app.tools.base import Tool, ToolRegistry


def _read_file(**kwargs) -> dict:
    path = kwargs["path"]
    try:
        content = Path(path).read_text()
        return {"content": content}
    except Exception as e:
        return {"error": str(e)}


def _write_file(**kwargs) -> dict:
    path = kwargs["path"]
    content = kwargs["content"]
    try:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        Path(path).write_text(content)
        return {"success": True, "path": path}
    except Exception as e:
        return {"error": str(e)}


def _web_search(**kwargs) -> dict:
    query = kwargs["query"]
    try:
        import requests
        resp = requests.get("https://api.duckduckgo.com/", params={"q": query, "format": "json", "no_html": 1}, timeout=10)
        data = resp.json()
        results = []
        if data.get("AbstractText"):
            results.append({"title": data.get("Heading", ""), "text": data["AbstractText"]})
        for item in data.get("RelatedTopics", [])[:5]:
            if isinstance(item, dict) and "Text" in item:
                results.append({"text": item["Text"], "url": item.get("FirstURL", "")})
        return {"results": results, "query": query}
    except Exception as e:
        return {"error": str(e), "query": query}


def create_system_tools(registry: ToolRegistry) -> None:
    registry.register(Tool(
        name="read_file", description="Read the contents of a file at the given path.",
        input_schema={"type": "object", "properties": {"path": {"type": "string", "description": "File path to read"}}, "required": ["path"]},
        func=_read_file))
    registry.register(Tool(
        name="write_file", description="Write content to a file at the given path.",
        input_schema={"type": "object", "properties": {"path": {"type": "string", "description": "File path to write"}, "content": {"type": "string", "description": "Content to write"}}, "required": ["path", "content"]},
        func=_write_file))
    registry.register(Tool(
        name="web_search", description="Search the web for information. Returns relevant results.",
        input_schema={"type": "object", "properties": {"query": {"type": "string", "description": "Search query"}}, "required": ["query"]},
        func=_web_search))
