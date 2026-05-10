"""Tests for the `knowledge_write` dispatcher tool.

Covers the WRITE side of the MARC27 Knowledge Service. Mirrors
`tests/test_platform_status_tools.py` — registration shape, the
not-authenticated branch, action validation, and per-action endpoint
URL routing (with `requests.post` stubbed).

Network-dependent behavior is mocked at the `requests` level; the
underlying platform routes are tested in marc27-core's own suite.
"""
from unittest.mock import patch

import pytest

from app.tools.base import ToolRegistry
from app.tools.knowledge_write import (
    _knowledge_write,
    create_knowledge_write_tool,
)


@pytest.fixture(autouse=True)
def _no_credentials_env(monkeypatch, tmp_path):
    """Run each test with empty creds env + a non-existent creds file
    so the tool takes the "not authenticated" branch deterministically."""
    monkeypatch.delenv("MARC27_API_KEY", raising=False)
    monkeypatch.delenv("MARC27_API_URL", raising=False)
    # Point HOME at an empty tmpdir so credentials.json is missing.
    monkeypatch.setenv("HOME", str(tmp_path))


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

class TestRegistration:
    def test_registers_exactly_one_tool_named_knowledge_write(self):
        registry = ToolRegistry()
        create_knowledge_write_tool(registry)
        tools = registry.list_tools()
        assert len(tools) == 1
        assert tools[0].name == "knowledge_write"

    def test_requires_approval_true(self):
        """Every action mutates platform state and/or spends compute."""
        registry = ToolRegistry()
        create_knowledge_write_tool(registry)
        assert registry.get("knowledge_write").requires_approval is True


# ---------------------------------------------------------------------------
# Action validation
# ---------------------------------------------------------------------------

class TestActionValidation:
    def test_missing_action_returns_clear_error(self):
        result = _knowledge_write()
        assert "error" in result
        assert "action" in result["error"].lower()
        # Must list the valid actions in the hint.
        hint = result.get("hint", "")
        for action in (
            "embed",
            "embed_bulk",
            "graph_seed",
            "graph_ingest",
            "research_web_search",
        ):
            assert action in hint, f"hint does not mention {action!r}"

    def test_unknown_action_lists_valid_actions(self):
        result = _knowledge_write(action="delete_everything")
        assert "error" in result
        assert "Unknown action" in result["error"]
        # The error must enumerate the legal options so the agent can
        # self-correct without another round-trip.
        for action in (
            "embed",
            "embed_bulk",
            "graph_seed",
            "graph_ingest",
            "research_web_search",
        ):
            assert action in result["error"], (
                f"error does not mention valid action {action!r}"
            )


# ---------------------------------------------------------------------------
# No-credentials → login hint
# ---------------------------------------------------------------------------

class TestNoCredentials:
    """Without auth, every valid action should hit the same login hint."""

    def test_embed_no_credentials(self):
        result = _knowledge_write(
            action="embed", doc_id="d1", content="hello world"
        )
        assert "error" in result
        assert "Not authenticated" in result["error"]
        assert "prism login" in result.get("hint", "")

    def test_embed_bulk_no_credentials(self):
        result = _knowledge_write(
            action="embed_bulk",
            corpus_id="00000000-0000-4000-8000-000000000001",
            documents=[{"doc_id": "d1", "content": "x"}],
        )
        assert "error" in result
        assert "Not authenticated" in result["error"]

    def test_graph_seed_no_credentials(self):
        result = _knowledge_write(
            action="graph_seed",
            nodes_url="https://example.invalid/nodes.csv",
            edges_url="https://example.invalid/edges.csv",
        )
        assert "error" in result
        assert "Not authenticated" in result["error"]

    def test_graph_ingest_no_credentials(self):
        result = _knowledge_write(
            action="graph_ingest", entities=[], relationships=[]
        )
        assert "error" in result
        assert "Not authenticated" in result["error"]

    def test_research_web_search_no_credentials(self):
        result = _knowledge_write(action="research_web_search", query="alloy")
        assert "error" in result
        assert "Not authenticated" in result["error"]


# ---------------------------------------------------------------------------
# Endpoint URL routing — with requests.post mocked
# ---------------------------------------------------------------------------

class TestEndpointRouting:
    """Each valid action should POST to a distinct endpoint URL."""

    def _stub(self, monkeypatch):
        """Install a fake credentials env + capture every POST call."""
        called = []

        class _StubResp:
            status_code = 200

            def json(self):
                return {"ok": True}

        def _stub_post(url, **kwargs):
            called.append({"url": url, "json": kwargs.get("json")})
            return _StubResp()

        monkeypatch.setenv("MARC27_API_KEY", "fake-token")
        monkeypatch.setenv(
            "MARC27_API_URL", "https://example.invalid/api/v1"
        )
        monkeypatch.setattr(
            "app.tools.knowledge_write.requests.post", _stub_post
        )
        return called

    def test_embed_hits_knowledge_embed(self, monkeypatch):
        called = self._stub(monkeypatch)
        result = _knowledge_write(
            action="embed", doc_id="d1", content="hello"
        )
        assert result == {"ok": True}
        assert len(called) == 1
        assert called[0]["url"] == "https://example.invalid/api/v1/knowledge/embed"
        # Body shape: doc_id + content forwarded; tenant/auth headers
        # added by _post and not part of the JSON body.
        assert called[0]["json"]["doc_id"] == "d1"
        assert called[0]["json"]["content"] == "hello"

    def test_embed_bulk_hits_knowledge_embed_bulk(self, monkeypatch):
        called = self._stub(monkeypatch)
        result = _knowledge_write(
            action="embed_bulk",
            corpus_id="00000000-0000-4000-8000-000000000001",
            documents=[{"doc_id": "a", "content": "x"}],
        )
        assert result == {"ok": True}
        assert called[0]["url"] == (
            "https://example.invalid/api/v1/knowledge/embed/bulk"
        )
        assert called[0]["json"]["corpus_id"] == (
            "00000000-0000-4000-8000-000000000001"
        )
        assert called[0]["json"]["documents"] == [
            {"doc_id": "a", "content": "x"}
        ]

    def test_graph_seed_hits_knowledge_graph_seed(self, monkeypatch):
        called = self._stub(monkeypatch)
        result = _knowledge_write(
            action="graph_seed",
            nodes_url="https://example.invalid/nodes.csv",
            edges_url="https://example.invalid/edges.csv",
        )
        assert result == {"ok": True}
        assert called[0]["url"] == (
            "https://example.invalid/api/v1/knowledge/graph/seed"
        )
        assert called[0]["json"] == {
            "nodes_url": "https://example.invalid/nodes.csv",
            "edges_url": "https://example.invalid/edges.csv",
        }

    def test_graph_ingest_hits_knowledge_graph_ingest(self, monkeypatch):
        called = self._stub(monkeypatch)
        entities = [
            {"name": "Ti6Al4V", "entity_type": "Material", "label": "Material"}
        ]
        relationships = []
        result = _knowledge_write(
            action="graph_ingest",
            entities=entities,
            relationships=relationships,
        )
        assert result == {"ok": True}
        assert called[0]["url"] == (
            "https://example.invalid/api/v1/knowledge/graph/ingest"
        )
        assert called[0]["json"]["entities"] == entities
        assert called[0]["json"]["relationships"] == relationships

    def test_research_web_search_hits_knowledge_research_web_search(
        self, monkeypatch
    ):
        called = self._stub(monkeypatch)
        result = _knowledge_write(
            action="research_web_search", query="titanium aluminide", limit=10
        )
        assert result == {"ok": True}
        assert called[0]["url"] == (
            "https://example.invalid/api/v1/knowledge/research/web-search"
        )
        assert called[0]["json"] == {
            "query": "titanium aluminide",
            "limit": 10,
        }


# ---------------------------------------------------------------------------
# Per-action required-arg validation (cheap sanity, no network needed)
# ---------------------------------------------------------------------------

class TestRequiredArgs:
    """Required-arg checks should fire BEFORE the auth check so an agent
    that forgets `content` doesn't get a misleading auth error."""

    def test_embed_missing_doc_id(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(action="embed", content="x")
            assert "error" in result
            assert "doc_id" in result["error"]

    def test_embed_missing_content(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(action="embed", doc_id="d1")
            assert "error" in result
            assert "content" in result["error"]

    def test_embed_bulk_missing_corpus_id(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(
                action="embed_bulk",
                documents=[{"doc_id": "a", "content": "x"}],
            )
            assert "error" in result
            assert "corpus_id" in result["error"]

    def test_embed_bulk_missing_documents(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(
                action="embed_bulk", corpus_id="abc"
            )
            assert "error" in result
            assert "documents" in result["error"]

    def test_graph_seed_missing_urls(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(action="graph_seed")
            assert "error" in result
            assert "nodes_url" in result["error"]

    def test_graph_ingest_missing_entities(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(
                action="graph_ingest", relationships=[]
            )
            assert "error" in result
            assert "entities" in result["error"]

    def test_research_web_search_missing_query(self):
        with patch.dict("os.environ", {"MARC27_API_KEY": "fake"}):
            result = _knowledge_write(action="research_web_search")
            assert "error" in result
            assert "query" in result["error"]
