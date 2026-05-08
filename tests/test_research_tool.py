"""Tests for the research() tool.

The real /api/v1/knowledge/research/query endpoint costs real money per
call ($0.01–$2 each). All tests here use a mocked SSE stream. A live
integration test is gated by the env var PRISM_LIVE_RESEARCH_TEST=1
so CI never accidentally spends.
"""
import json
import os
from unittest.mock import MagicMock, patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.research import (
    create_research_tools,
    _format_progress_line,
    _parse_sse_line,
    _research,
)


# ---------------------------------------------------------------------------
# Helpers — fake SSE stream
# ---------------------------------------------------------------------------

def _sse_lines_from_events(events: list[dict]) -> list[str]:
    """Format a list of {step, data} events as SSE `data: ...` lines."""
    lines = []
    for ev in events:
        lines.append(f"data: {json.dumps(ev)}")
        lines.append("")  # blank line separates events
    return lines


def _make_mock_response(status_code: int, sse_events: list[dict]):
    """Build a requests.Response stand-in that yields SSE lines."""
    resp = MagicMock()
    resp.status_code = status_code
    resp.iter_lines = MagicMock(return_value=iter(_sse_lines_from_events(sse_events)))
    resp.text = ""
    resp.close = MagicMock()
    return resp


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

class TestRegistration:
    def test_tool_registered(self):
        reg = ToolRegistry()
        create_research_tools(reg)
        names = [t.name for t in reg.list_tools()]
        assert "research" in names

    def test_requires_approval(self):
        reg = ToolRegistry()
        create_research_tools(reg)
        tool = reg.get("research")
        # Real-money action MUST be gated
        assert tool.requires_approval is True

    def test_question_is_required_in_schema(self):
        reg = ToolRegistry()
        create_research_tools(reg)
        tool = reg.get("research")
        assert "question" in tool.input_schema["required"]

    def test_depth_bounds_in_schema(self):
        reg = ToolRegistry()
        create_research_tools(reg)
        tool = reg.get("research")
        depth = tool.input_schema["properties"]["depth"]
        assert depth["minimum"] == 0
        assert depth["maximum"] == 10


# ---------------------------------------------------------------------------
# Argument validation
# ---------------------------------------------------------------------------

class TestArgValidation:
    def test_missing_question(self):
        result = _research()
        assert "error" in result
        assert "question" in result["error"]

    def test_empty_question(self):
        result = _research(question="   ")
        assert "error" in result
        assert "question" in result["error"]

    def test_negative_depth(self):
        result = _research(question="x", depth=-1)
        assert "error" in result
        assert "depth" in result["error"]

    def test_excessive_depth(self):
        result = _research(question="x", depth=99)
        assert "error" in result
        assert "depth" in result["error"]


# ---------------------------------------------------------------------------
# Credentials
# ---------------------------------------------------------------------------

class TestCredentials:
    def test_no_credentials_returns_error(self, tmp_path, monkeypatch):
        monkeypatch.delenv("MARC27_API_KEY", raising=False)
        # Point credentials.json lookup at an empty home
        monkeypatch.setenv("HOME", str(tmp_path))
        result = _research(question="anything")
        assert "error" in result
        assert "MARC27" in result["error"] or "platform" in result["error"]


# ---------------------------------------------------------------------------
# SSE parsing
# ---------------------------------------------------------------------------

class TestSSEParsing:
    def test_parse_data_line(self):
        ev = _parse_sse_line('data: {"step": "started", "data": {"x": 1}}')
        assert ev == {"step": "started", "data": {"x": 1}}

    def test_parse_blank_line(self):
        assert _parse_sse_line("") is None

    def test_parse_done_sentinel(self):
        assert _parse_sse_line("data: [DONE]") is None

    def test_parse_non_data_line(self):
        assert _parse_sse_line(":keepalive") is None
        assert _parse_sse_line("event: anything") is None

    def test_parse_malformed_json(self):
        assert _parse_sse_line("data: {not json") is None

    def test_format_progress_started(self):
        out = _format_progress_line("started", {
            "session_id": "abc-123-uuid", "question": "what is steel?",
        })
        assert "research started" in out
        assert "abc-123-uuid"[:12] in out

    def test_format_progress_answer(self):
        out = _format_progress_line("answer", {
            "text": "Steel is...",
            "turns": 5,
        })
        assert "answer ready" in out
        assert "5 turns" in out

    def test_format_progress_error(self):
        out = _format_progress_line("error", {"message": "LLM down"})
        assert "research error" in out
        assert "LLM down" in out


# ---------------------------------------------------------------------------
# End-to-end with mocked SSE
# ---------------------------------------------------------------------------

class TestEndToEnd:
    @patch("app.tools.research._resolve_credentials")
    @patch("requests.post")
    def test_happy_path(self, mock_post, mock_creds):
        """A full research session: started → llm_response → answer → complete."""
        mock_creds.return_value = ("https://api.marc27.test/api/v1", "test-key")

        events = [
            {"step": "started", "data": {
                "session_id": "session-uuid-abc",
                "question": "test question",
                "max_depth": 1,
                "mode": "repl",
            }},
            {"step": "reasoning", "data": {"text": "Let me think..."}},
            {"step": "repl_exec", "data": {"code": "results = graph_search('Ti')"}},
            {"step": "repl_result", "data": {"output": "[{name: 'Ti'}]"}},
            {"step": "answer", "data": {
                "text": "Titanium is an element with atomic number 22.",
                "turns": 3,
            }},
            {"step": "complete", "data": {
                "session_id": "session-uuid-abc",
                "metrics": {
                    "graph_queries": 1,
                    "vector_queries": 0,
                    "web_searches": 0,
                    "papers_fetched": 0,
                    "papers_ingested": 0,
                    "entities_created": 0,
                    "embeddings_created": 0,
                    "llm_calls": 3,
                    "cost_usd": 0.0123,
                },
            }},
        ]
        mock_post.return_value = _make_mock_response(200, events)

        result = _research(question="test question", depth=1)

        assert "error" not in result, f"unexpected error: {result}"
        assert result["session_id"] == "session-uuid-abc"
        assert "Titanium" in result["answer"]
        assert result["turns"] == 3
        assert result["cost_usd"] == 0.0123
        assert result["metrics"]["llm_calls"] == 3
        assert result["source"] == "marc27_research_rlm"
        # progress_log should include the user-visible steps
        assert any("research started" in p for p in result["progress_log"])
        assert any("research complete" in p for p in result["progress_log"])

    @patch("app.tools.research._resolve_credentials")
    @patch("requests.post")
    def test_http_error(self, mock_post, mock_creds):
        mock_creds.return_value = ("https://api.marc27.test/api/v1", "test-key")
        resp = _make_mock_response(503, [])
        resp.text = "service unavailable"
        mock_post.return_value = resp

        result = _research(question="test")
        assert "error" in result
        assert "503" in result["error"]

    @patch("app.tools.research._resolve_credentials")
    @patch("requests.post")
    def test_server_error_event(self, mock_post, mock_creds):
        """Server emits {step:'error', data:{message:'...'}} mid-stream."""
        mock_creds.return_value = ("https://api.marc27.test/api/v1", "test-key")
        events = [
            {"step": "started", "data": {"session_id": "s1", "question": "q"}},
            {"step": "error", "data": {"message": "LLM service unreachable"}},
        ]
        mock_post.return_value = _make_mock_response(200, events)

        result = _research(question="anything")
        assert "error" in result
        assert "LLM service unreachable" in result["error"]
        assert result["session_id"] == "s1"

    @patch("app.tools.research._resolve_credentials")
    @patch("requests.post")
    def test_no_answer_emitted(self, mock_post, mock_creds):
        """Stream ends without an `answer` event — should fail cleanly."""
        mock_creds.return_value = ("https://api.marc27.test/api/v1", "test-key")
        events = [
            {"step": "started", "data": {"session_id": "s1", "question": "q"}},
            {"step": "complete", "data": {"session_id": "s1"}},
        ]
        mock_post.return_value = _make_mock_response(200, events)

        result = _research(question="anything")
        assert "error" in result
        assert "without producing an answer" in result["error"]
        assert result["session_id"] == "s1"

    @patch("app.tools.research._resolve_credentials")
    @patch("requests.post")
    def test_keepalive_lines_ignored(self, mock_post, mock_creds):
        """Server may emit `:keepalive` pings — must not break parsing."""
        mock_creds.return_value = ("https://api.marc27.test/api/v1", "test-key")

        # Mix in keepalive comments and blank lines
        resp = MagicMock()
        resp.status_code = 200
        resp.iter_lines = MagicMock(return_value=iter([
            ":keepalive",
            "",
            f'data: {json.dumps({"step": "started", "data": {"session_id": "s1", "question": "q"}})}',
            "",
            ":another keepalive",
            f'data: {json.dumps({"step": "answer", "data": {"text": "answer", "turns": 1}})}',
            "",
            f'data: {json.dumps({"step": "complete", "data": {"session_id": "s1", "metrics": {"cost_usd": 0.001}}})}',
            "",
        ]))
        resp.text = ""
        resp.close = MagicMock()
        mock_post.return_value = resp

        result = _research(question="test")
        assert "error" not in result
        assert result["answer"] == "answer"

    @patch("app.tools.research._resolve_credentials")
    @patch("requests.post")
    def test_request_exception_returns_error(self, mock_post, mock_creds):
        """Connection error → clean error dict, no exception bubbled up."""
        import requests
        mock_creds.return_value = ("https://api.marc27.test/api/v1", "test-key")
        mock_post.side_effect = requests.exceptions.ConnectionError("DNS lookup failed")

        result = _research(question="test")
        assert "error" in result
        assert "research endpoint" in result["error"]


# ---------------------------------------------------------------------------
# Live integration test — runs ONLY when explicitly enabled. Costs real $.
# ---------------------------------------------------------------------------

@pytest.mark.skipif(
    os.environ.get("PRISM_LIVE_RESEARCH_TEST") != "1",
    reason="Live research test costs real money; set PRISM_LIVE_RESEARCH_TEST=1 to run",
)
def test_live_research_cheap_query():
    """End-to-end against the real MARC27 platform.

    Set `PRISM_LIVE_RESEARCH_TEST=1` to run. Uses depth=0 (local-only)
    to keep cost minimal — typically <$0.05 per run.
    """
    result = _research(
        question="What is the chemical composition of Inconel 718?",
        depth=0,
        timeout_seconds=120,
    )
    assert "error" not in result, f"live research failed: {result}"
    assert result["answer"]
    assert "Inconel" in result["answer"] or "nickel" in result["answer"].lower()
    assert result["session_id"]
    print(f"\n[live test] session={result['session_id']} cost=${result['cost_usd']:.4f}")
