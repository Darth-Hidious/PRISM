"""System tools: file I/O, web search, user interaction."""
from pathlib import Path
from app.tools.base import Tool, ToolRegistry

# Restrict file operations to current working directory
_ALLOWED_BASE = Path.cwd().resolve()


def _resolve_project_path(path: str) -> Path:
    candidate = Path(path).expanduser()
    return (
        candidate.resolve()
        if candidate.is_absolute()
        else (_ALLOWED_BASE / candidate).resolve()
    )


def _is_safe_path(path: str) -> bool:
    """Check that path resolves within the allowed base directory."""
    try:
        resolved = _resolve_project_path(path)
        return resolved == _ALLOWED_BASE or _ALLOWED_BASE in resolved.parents
    except (ValueError, OSError):
        return False


def _read_file(**kwargs) -> dict:
    path = kwargs["path"]
    if not _is_safe_path(path):
        return {"error": f"Access denied: path must be within {_ALLOWED_BASE}"}
    try:
        resolved = _resolve_project_path(path)
        content = resolved.read_text()
        return {
            "path": str(resolved),
            "content": content,
            "size_bytes": len(content.encode("utf-8")),
        }
    except Exception as e:
        return {"error": str(e)}


def _write_file(**kwargs) -> dict:
    path = kwargs["path"]
    content = kwargs["content"]
    if not _is_safe_path(path):
        return {"error": f"Access denied: path must be within {_ALLOWED_BASE}"}
    try:
        resolved = _resolve_project_path(path)
        resolved.parent.mkdir(parents=True, exist_ok=True)
        resolved.write_text(content)
        return {
            "success": True,
            "path": str(resolved),
            "size_bytes": len(content.encode("utf-8")),
        }
    except Exception as e:
        return {"error": str(e)}


def _edit_file(**kwargs) -> dict:
    path = kwargs["path"]
    old_text = kwargs["old_text"]
    new_text = kwargs["new_text"]
    replace_all = bool(kwargs.get("replace_all", False))
    if not _is_safe_path(path):
        return {"error": f"Access denied: path must be within {_ALLOWED_BASE}"}
    if old_text == "":
        return {"error": "old_text must not be empty"}

    try:
        resolved = _resolve_project_path(path)
        content = resolved.read_text()
        match_count = content.count(old_text)
        if match_count == 0:
            return {"error": "old_text was not found in the target file"}
        if match_count > 1 and not replace_all:
            return {
                "error": (
                    "old_text matched multiple locations; rerun with replace_all=true "
                    "or choose a more specific snippet"
                ),
                "match_count": match_count,
                "path": str(resolved),
            }

        replacements = match_count if replace_all else 1
        updated = (
            content.replace(old_text, new_text)
            if replace_all
            else content.replace(old_text, new_text, 1)
        )
        resolved.write_text(updated)
        return {
            "success": True,
            "path": str(resolved),
            "replacements": replacements,
            "size_bytes": len(updated.encode("utf-8")),
        }
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


def _show_scratchpad(**kwargs) -> dict:
    """Return the agent's scratchpad as text. Requires scratchpad to be set."""
    # The scratchpad reference is injected by the caller (AgentCore)
    scratchpad = kwargs.get("_scratchpad")
    if scratchpad is None:
        return {"text": "Scratchpad is not available in this session."}
    return {"text": scratchpad.to_text()}


def create_system_tools(registry: ToolRegistry) -> None:
    # These core coding tools need procedural descriptions because the model
    # should choose them intentionally instead of falling back to shell habits.
    registry.register(Tool(
        name="read_file",
        description=(
            "Read a text file inside the current PRISM project. Use this for "
            "source files, config files, logs, and other textual project files "
            "when you need the file contents directly in the model context. Use "
            "this instead of execute_bash for straightforward file reads."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": (
                        "Project-relative or absolute path to a text file inside "
                        "the current PRISM project."
                    ),
                }
            },
            "required": ["path"],
        },
        func=_read_file))
    registry.register(Tool(
        name="write_file",
        description=(
            "Write text content to a file inside the current PRISM project. "
            "This overwrites the target file completely, so use it when you want "
            "to replace a file body or create a new text file directly."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": (
                        "Project-relative or absolute path to a file inside the "
                        "current PRISM project."
                    ),
                },
                "content": {
                    "type": "string",
                    "description": (
                        "Full text content to write. This replaces any existing "
                        "file contents."
                    ),
                },
            },
            "required": ["path", "content"],
        },
        func=_write_file))
    registry.register(Tool(
        name="edit_file",
        description=(
            "Edit a text file inside the current PRISM project by replacing an "
            "exact old_text snippet with new_text. Use this when you want a "
            "targeted in-file change instead of overwriting the whole file with "
            "write_file or shelling out through execute_bash."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": (
                        "Project-relative or absolute path to a text file inside "
                        "the current PRISM project."
                    ),
                },
                "old_text": {
                    "type": "string",
                    "description": (
                        "Exact text snippet to replace. Keep this specific enough "
                        "to match only the intended location."
                    ),
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text for the matched snippet.",
                },
                "replace_all": {
                    "type": "boolean",
                    "description": (
                        "Replace every exact match instead of failing on multiple matches."
                    ),
                },
            },
            "required": ["path", "old_text", "new_text"],
        },
        func=_edit_file))
    registry.register(Tool(
        name="web_search", description="Search the web for information. Returns relevant results.",
        input_schema={"type": "object", "properties": {"query": {"type": "string", "description": "Search query"}}, "required": ["query"]},
        func=_web_search))
    registry.register(Tool(
        name="show_scratchpad",
        description="Show the agent's execution log (scratchpad) — lists all actions taken so far in this session.",
        input_schema={"type": "object", "properties": {}},
        func=_show_scratchpad))
