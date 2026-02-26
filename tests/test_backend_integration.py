"""Integration test: full JSON-RPC roundtrip over StdioServer.

Unlike the unit tests (test_backend_server.py, test_ui_emitter.py) which mock
at the emitter or server boundary, these tests exercise the FULL path:

    JSON input -> StdioServer.handle_message -> UIEmitter -> protocol events -> JSON output

Only external dependencies (LLM backend, plugin bootstrap, AgentCore) are mocked.
"""
import json
from io import StringIO
from unittest.mock import MagicMock, patch

from app.agent.events import TextDelta, TurnComplete, UsageInfo


def test_full_roundtrip():
    """init -> input.message -> verify protocol events on output."""
    from app.backend.server import StdioServer

    server = StdioServer()
    output = StringIO()

    # Mock the backend creation — patches target the ORIGINAL module paths
    # because server.py uses lazy imports inside _handle_init
    with patch("app.agent.factory.create_backend"), \
         patch("app.agent.core.AgentCore") as MockAgent, \
         patch("app.plugins.bootstrap.build_full_registry") as mock_reg, \
         patch("app.backend.ui_emitter.build_status", return_value={
             "llm": {"connected": True, "provider": "Claude"},
             "plugins": {"count": 0, "available": False, "names": []},
             "commands": {"tools": [], "total": 1, "healthy_providers": 0, "total_providers": 0},
             "skills": {"count": 0, "names": []},
         }):

        mock_tools = MagicMock()
        mock_tools.list_tools.return_value = [MagicMock(name="t1")]
        mock_reg.return_value = (mock_tools, None, None)

        mock_agent = MagicMock()
        mock_agent.tools = mock_tools
        mock_agent.history = []
        mock_agent.auto_approve = False
        mock_agent.process_stream.return_value = iter([
            TextDelta(text="The answer is 42."),
            TurnComplete(
                text="The answer is 42.",
                usage=UsageInfo(500, 100),
                estimated_cost=0.005,
            ),
        ])
        MockAgent.return_value = mock_agent

        # 1. Init
        init = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
        server.handle_message(init, output)

        # 2. Send a message
        msg = json.dumps({
            "jsonrpc": "2.0", "method": "input.message",
            "params": {"text": "What is 6*7?"}, "id": 2,
        })
        server.handle_message(msg, output)

    # Parse all output
    lines = [l for l in output.getvalue().strip().split("\n") if l]
    events = [json.loads(l) for l in lines]

    # Verify init response
    init_resp = events[0]
    assert init_resp["id"] == 1
    assert init_resp["result"]["ok"] is True

    # Verify welcome
    welcome = events[1]
    assert welcome["method"] == "ui.welcome"
    assert welcome["params"]["version"]  # non-empty version string
    assert welcome["params"]["status"]["llm"]["provider"] == "Claude"

    # Find streaming events (everything after init + welcome)
    methods = [e.get("method") for e in events[2:]]
    assert "ui.text.delta" in methods
    assert "ui.cost" in methods
    assert "ui.turn.complete" in methods

    # Verify text delta content
    deltas = [e for e in events if e.get("method") == "ui.text.delta"]
    assert deltas[0]["params"]["text"] == "The answer is 42."

    # Verify cost event
    cost_event = [e for e in events if e.get("method") == "ui.cost"][0]
    assert cost_event["params"]["input_tokens"] == 500
    assert cost_event["params"]["output_tokens"] == 100
    assert cost_event["params"]["turn_cost"] == 0.005
    assert cost_event["params"]["session_cost"] == 0.005


def test_command_roundtrip():
    """init -> /help -> verify info card."""
    from app.backend.server import StdioServer

    server = StdioServer()
    output = StringIO()

    with patch("app.agent.factory.create_backend"), \
         patch("app.agent.core.AgentCore") as MockAgent, \
         patch("app.plugins.bootstrap.build_full_registry") as mock_reg, \
         patch("app.backend.ui_emitter.build_status", return_value={
             "llm": {"connected": True, "provider": "mock"},
             "plugins": {"count": 0, "available": False, "names": []},
             "commands": {"tools": [], "total": 0, "healthy_providers": 0, "total_providers": 0},
             "skills": {"count": 0, "names": []},
         }):

        mock_tools = MagicMock()
        mock_tools.list_tools.return_value = []
        mock_reg.return_value = (mock_tools, None, None)
        mock_agent = MagicMock()
        mock_agent.tools = mock_tools
        mock_agent.history = []
        MockAgent.return_value = mock_agent

        init = json.dumps({"jsonrpc": "2.0", "method": "init", "params": {}, "id": 1})
        server.handle_message(init, output)

        cmd = json.dumps({
            "jsonrpc": "2.0", "method": "input.command",
            "params": {"command": "/help"}, "id": 2,
        })
        server.handle_message(cmd, output)

    lines = [l for l in output.getvalue().strip().split("\n") if l]
    events = [json.loads(l) for l in lines]

    # Find the help card
    cards = [e for e in events if e.get("method") == "ui.card"]
    assert len(cards) >= 1
    assert cards[0]["params"]["card_type"] == "info"
    # The help card should contain at least one command name
    assert "/help" in cards[0]["params"]["content"] or "/tools" in cards[0]["params"]["content"]
