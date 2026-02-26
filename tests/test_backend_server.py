"""Tests for the JSON-RPC stdio server."""
import json
from unittest.mock import MagicMock, patch
from io import StringIO


def test_server_handles_init():
    from app.backend.server import StdioServer
    server = StdioServer()
    output = StringIO()

    mock_status = {
        "llm": {"connected": True, "provider": "mock"},
        "plugins": {"count": 0, "available": False, "names": []},
        "commands": {"tools": [], "total": 0, "healthy_providers": 0, "total_providers": 0},
        "skills": {"count": 0, "names": []},
    }
    with patch("app.agent.factory.create_backend") as mock_cb, \
         patch("app.agent.core.AgentCore") as mock_ac, \
         patch("app.plugins.bootstrap.build_full_registry") as mock_reg, \
         patch("app.backend.ui_emitter.build_status", return_value=mock_status):
        mock_reg.return_value = (MagicMock(), None, None)
        mock_agent = MagicMock()
        mock_agent.tools.list_tools.return_value = []
        mock_ac.return_value = mock_agent

        msg = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
        server.handle_message(msg, output)

    lines = output.getvalue().strip().split("\n")
    # Should get: result for init + welcome event
    result = json.loads(lines[0])
    assert result["id"] == 1
    assert result["result"]["ok"] is True


def test_server_handles_input_message():
    from app.backend.server import StdioServer
    server = StdioServer()
    # Pre-set emitter
    mock_emitter = MagicMock()
    mock_emitter.process.return_value = iter([
        {"jsonrpc": "2.0", "method": "ui.text.delta", "params": {"text": "hi"}},
        {"jsonrpc": "2.0", "method": "ui.turn.complete", "params": {}},
    ])
    server.emitter = mock_emitter

    output = StringIO()
    msg = json.dumps({"jsonrpc": "2.0", "method": "input.message",
                       "params": {"text": "hello"}, "id": 2})
    server.handle_message(msg, output)

    lines = output.getvalue().strip().split("\n")
    events = [json.loads(l) for l in lines]
    methods = [e.get("method") for e in events]
    assert "ui.text.delta" in methods
    assert "ui.turn.complete" in methods


def test_server_handles_input_command():
    from app.backend.server import StdioServer
    server = StdioServer()
    mock_emitter = MagicMock()
    mock_emitter.handle_command.return_value = iter([
        {"jsonrpc": "2.0", "method": "ui.card",
         "params": {"card_type": "info", "content": "help text",
                    "tool_name": "", "elapsed_ms": 0, "data": {}}},
    ])
    server.emitter = mock_emitter

    output = StringIO()
    msg = json.dumps({"jsonrpc": "2.0", "method": "input.command",
                       "params": {"command": "/help"}, "id": 3})
    server.handle_message(msg, output)
    mock_emitter.handle_command.assert_called_once_with("/help")


def test_server_rejects_unknown_method():
    from app.backend.server import StdioServer
    server = StdioServer()
    output = StringIO()
    msg = json.dumps({"jsonrpc": "2.0", "method": "unknown.method",
                       "params": {}, "id": 4})
    server.handle_message(msg, output)
    result = json.loads(output.getvalue().strip())
    assert "error" in result
