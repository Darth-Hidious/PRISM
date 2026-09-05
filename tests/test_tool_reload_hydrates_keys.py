"""A key saved in the TUI reaches the running tool server on reload.

The TUI's API-key window writes `~/.prism/api_keys.json`; the Rust side
hydrates the environment from it at startup. The Python tool server is a
child process whose environment was fixed when it was spawned, so a key
pasted mid-session was invisible to every tool until a restart. `reload_tools`
is the moment the server rebuilds itself; it must read the keys then."""
import io
import json

from app import tool_server
from app.tools.base import ToolRegistry


def test_reload_tools_loads_saved_api_keys_into_the_environment(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    (tmp_path / ".prism").mkdir()
    (tmp_path / ".prism" / "api_keys.json").write_text(json.dumps({"LENS_API_TOKEN": "lens-123", "SEMANTIC_SCHOLAR_API_KEY": "s2-456"}))
    monkeypatch.delenv("LENS_API_TOKEN", raising=False)
    monkeypatch.setenv("SEMANTIC_SCHOLAR_API_KEY", "already-set-by-the-shell")
    monkeypatch.setattr(tool_server, "build_full_registry", lambda **kw: (ToolRegistry(), None, None))
    state = {"registry": ToolRegistry(), "enable_mcp": False, "enable_plugins": False}
    response = tool_server._handle(state, {"method": "reload_tools"})
    assert response.get("status") == "ok", response
    import os
    assert os.environ["LENS_API_TOKEN"] == "lens-123"
    assert os.environ["SEMANTIC_SCHOLAR_API_KEY"] == "already-set-by-the-shell", "a shell export outranks the saved file"
    assert response.get("keys_loaded") == ["LENS_API_TOKEN"], response
