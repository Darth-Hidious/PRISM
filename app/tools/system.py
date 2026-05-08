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


_FILE_DISPATCH = {
    "read":  _read_file,
    "write": _write_file,
    "edit":  _edit_file,
}


def _file(**kwargs) -> dict:
    """Unified file I/O dispatcher.

    Replaces the prior `read_file`, `write_file`, and `edit_file` tools.
    Each action validates its required args before touching the filesystem.
    Sandbox enforcement (paths must resolve inside PRISM project root) is
    inherited from `_is_safe_path`, applied by each handler.
    """
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": f"Missing 'action'. Valid: {list(_FILE_DISPATCH.keys())}",
            "hint": (
                "file(action='read', path='./README.md') / "
                "file(action='write', path=..., content=...) / "
                "file(action='edit', path=..., old_text=..., new_text=...)"
            ),
        }
    handler = _FILE_DISPATCH.get(action)
    if not handler:
        return {"error": f"Unknown action '{action}'. Valid: {list(_FILE_DISPATCH.keys())}"}
    # Per-action arg validation
    if action == "read":
        if not kwargs.get("path"):
            return {"error": "Action 'read' requires `path`"}
    elif action == "write":
        if not kwargs.get("path"):
            return {"error": "Action 'write' requires `path`"}
        if kwargs.get("content") is None:
            return {"error": "Action 'write' requires `content`"}
    elif action == "edit":
        if not kwargs.get("path"):
            return {"error": "Action 'edit' requires `path`"}
        if kwargs.get("old_text") is None or kwargs.get("new_text") is None:
            return {"error": "Action 'edit' requires `old_text` and `new_text`"}
    try:
        return handler(**kwargs)
    except Exception as e:
        return {"error": str(e), "action": action}


_FILE_DESCRIPTION = (
    "Text file I/O inside the current PRISM project. ONE tool, three actions:\n"
    "  • action='read' — read a text file. Requires `path`. Returns content + size.\n"
    "  • action='write' — overwrite (or create) a file. Requires `path` + "
    "`content`. Replaces existing contents completely.\n"
    "  • action='edit' — targeted in-file replace. Requires `path`, `old_text`, "
    "`new_text`. Fails if old_text matches multiple locations unless "
    "`replace_all=true`. Use this when you want a surgical change rather than "
    "rewriting the whole file with action='write'.\n"
    "Paths are resolved relative to the project root (CWD); absolute paths "
    "must still be inside the project tree (sandbox enforced). NOT for binary "
    "files and NOT for shell access (use execute_bash)."
)

_FILE_SCHEMA = {
    "type": "object",
    "properties": {
        "action": {
            "type": "string",
            "enum": list(_FILE_DISPATCH.keys()),
            "description": "Which file operation to perform.",
        },
        "path": {
            "type": "string",
            "description": (
                "Project-relative or absolute path inside the PRISM "
                "project. Required for all actions."
            ),
        },
        "content": {
            "type": "string",
            "description": (
                "Full file content for action='write'. Replaces existing contents."
            ),
        },
        "old_text": {
            "type": "string",
            "description": (
                "Exact text snippet to replace for action='edit'. Must be "
                "specific enough to match only the intended location."
            ),
        },
        "new_text": {
            "type": "string",
            "description": "Replacement text for action='edit'.",
        },
        "replace_all": {
            "type": "boolean",
            "default": False,
            "description": (
                "For action='edit': replace every match instead of failing "
                "on multiple matches."
            ),
        },
    },
    "required": ["action"],
    "additionalProperties": False,
}


def create_system_tools(registry: ToolRegistry) -> None:
    # Unified file tool replaces read_file / write_file / edit_file.
    registry.register(Tool(
        name="file",
        description=_FILE_DESCRIPTION,
        input_schema=_FILE_SCHEMA,
        func=_file,
    ))
    # NOTE: the system.py `web_search` Tool registration was removed in
    # Round 6 cleanup. It duplicated the richer Firecrawl-or-DuckDuckGo
    # implementation in app/tools/web.py, which is now exposed as
    # `web(action='search')`. The `_web_search` function below remains
    # because internal helpers (and tests) may call it directly.
    registry.register(Tool(
        name="show_scratchpad",
        description=(
            "Print the agent's execution log for this chat session — an "
            "ordered list of every tool the agent has called so far, "
            "along with the arguments and a short result summary per "
            "call. Use this when the user asks 'what have you done so "
            "far?', 'show me your work', 'what tools did you call to "
            "get this answer?', or when the agent itself needs to "
            "remind itself of prior steps before deciding the next "
            "action. Read-only; does not affect state. Returns a "
            "structured list, one entry per tool invocation."
        ),
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        func=_show_scratchpad,
    ))
