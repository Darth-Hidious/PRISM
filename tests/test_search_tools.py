"""Tests for the search tools.

After Round 6: the literature_search and patent_search Tool aliases were
removed. Both functionalities live behind prior_art_search(source=…). The
private _literature_search / _patent_search helpers are preserved for
direct testing because prior_art_search dispatches into them.
"""
import json
from unittest.mock import MagicMock, patch
from app.tools.base import ToolRegistry
from app.tools.search import (
    _literature_search,
    _patent_search,
    _prior_art_search,
    create_search_tools,
)


class TestCreateSearchTools:
    def test_registers_prior_art_search(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        names = [t.name for t in reg.list_tools()]
        # Unified entry — replaced the two aliases
        assert "prior_art_search" in names
        # Old aliases must be gone (they inflated the retrieval surface
        # without adding capability)
        assert "literature_search" not in names
        assert "patent_search" not in names

    def test_prior_art_search_schema(self):
        reg = ToolRegistry()
        create_search_tools(reg)
        tool = reg.get("prior_art_search")
        assert "query" in tool.input_schema["required"]
        assert "query" in tool.input_schema["properties"]
        assert "source" in tool.input_schema["properties"]


class TestLiteratureSearchFunc:
    """`_literature_search` delegates to the Rust retrieval engine
    (`prism papers search`) — these stub the engine process, never the
    network."""

    ENGINE_OUTCOME = json.dumps({
        "papers": [
            {"source": "arxiv", "source_id": "1", "title": "Paper 1",
             "abstract_text": "A1", "url": "u1"},
            {"source": "semantic_scholar", "source_id": "2",
             "title": "Paper 2", "abstract_text": "A2", "url": "u2"},
        ],
        "duplicates_merged": 0,
        "relevance": {
            "status": "applied",
            "candidates": 3,
            "evaluated": 3,
            "dropped": 1,
            "threshold": 0.60,
            "backend": "openai:relevance-fixture",
            "returned_unfiltered": False,
            "off_topic_examples": [{
                "source": "arxiv",
                "source_id": "peek-cv",
                "title": (
                    "PEEK: Picking Essential frames via Efficient "
                    "Knowledge distillation"
                ),
                "score": 0.21,
            }],
        },
        "source_status": [
            {"source": "arxiv", "status": "ok", "count": 2,
             "cache_hit": False, "error": None},
            {"source": "semantic_scholar", "status": "ok", "count": 1,
             "cache_hit": False, "error": None},
        ],
        "elapsed_ms": 10.0,
    })

    def _engine_proc(self):
        proc = MagicMock()
        proc.stdout = self.ENGINE_OUTCOME
        proc.stderr = ""
        proc.returncode = 0
        return proc

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary",
           return_value="/usr/local/bin/prism")
    def test_returns_results(self, _binary, mock_run):
        mock_run.return_value = self._engine_proc()
        result = _literature_search(query="tungsten alloy")
        assert result["count"] == 2
        assert result["source"] == "literature"
        assert len(result["results"]) == 2
        # Per-source outcomes must reach the caller — a thin result set with
        # a failed source is a different fact from a genuinely empty one.
        assert result["source_status"]["arxiv"] == "ok (2 results)"
        # Filtering is equally provenance-bearing: callers must see that a
        # third candidate was removed and be able to audit an example.
        assert result["relevance"]["status"] == "applied"
        assert result["relevance"]["dropped"] == 1
        assert result["relevance"]["off_topic_examples"][0]["source_id"] == "peek-cv"
        # Every record carries the literature evidence ceiling.
        assert all(r["evidence_class"] == "research" for r in result["results"])

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary",
           return_value="/usr/local/bin/prism")
    def test_prior_art_reports_when_papers_were_returned_unfiltered(
        self, _binary, mock_run,
    ):
        outcome = json.loads(self.ENGINE_OUTCOME)
        outcome["papers"].append({
            "source": "arxiv",
            "source_id": "peek-cv",
            "title": (
                "PEEK: Picking Essential frames via Efficient Knowledge "
                "distillation"
            ),
            "abstract_text": "A computer-vision frame-selection paper.",
            "url": "u3",
        })
        outcome["relevance"] = {
            "status": "unavailable",
            "candidates": 3,
            "evaluated": 0,
            "dropped": 0,
            "threshold": 0.60,
            "returned_unfiltered": True,
            "off_topic_examples": [],
            "message": (
                "no embedding backend was available; papers were returned "
                "unfiltered"
            ),
        }
        proc = self._engine_proc()
        proc.stdout = json.dumps(outcome)
        mock_run.return_value = proc

        result = _prior_art_search(
            query="PEEK dielectric breakdown strength",
            source="papers",
        )

        assert result["counts"]["papers"] == 3
        assert result["papers_relevance"] == outcome["relevance"]
        assert result["papers_relevance"]["status"] == "unavailable"
        assert result["papers_relevance"]["returned_unfiltered"] is True
        assert "returned unfiltered" in result["papers_relevance"]["message"]

    @patch("app.tools.search.spawn.run")
    @patch("app.tools.search._resolve_prism_binary",
           return_value="/usr/local/bin/prism")
    def test_empty_results(self, _binary, mock_run):
        proc = MagicMock()
        proc.stdout = json.dumps({"papers": [], "duplicates_merged": 0,
                                  "source_status": [], "elapsed_ms": 1.0})
        proc.stderr = ""
        proc.returncode = 0
        mock_run.return_value = proc
        result = _literature_search(query="")
        assert result["count"] == 0
        assert result["results"] == []


class TestPatentSearchFunc:
    @patch("app.tools.data_collectors.patent_collector.PatentCollector.collect")
    def test_returns_results(self, mock_collect):
        mock_collect.return_value = [
            {"source": "lens_patents", "title": "Patent 1"},
        ]
        result = _patent_search(query="alloy coating")
        assert result["count"] == 1
        assert result["source"] == "patents"

    @patch("app.tools.data_collectors.patent_collector.PatentCollector.collect")
    def test_a_backend_that_genuinely_found_nothing_reports_zero(self, mock_collect):
        """Zero hits from a backend that RAN is a real answer. A missing
        credential is not — that raises, and is covered below."""
        mock_collect.return_value = []
        result = _patent_search(query="alloy")
        assert result["count"] == 0
        assert result["results"] == []


class TestAPatentFailureIsNeverAnEmptyList:
    """Measured: prior_art_search(source='patents') came back with
    counts.patents = 0 and no patents_error at all. An unattributed zero is
    indistinguishable from "nobody has patented this", which is a business
    conclusion nothing may guess at.
    """

    @staticmethod
    def _unconfigured(monkeypatch, tmp_path):
        monkeypatch.setenv("PRISM_PATENT_CACHE", str(tmp_path))
        for var in (
            "PRISM_PATENT_BACKEND", "PRISM_PATENT_TABLE", "PRISM_PLATFORM_URL",
            "PRISM_PLATFORM_TOKEN", "LENS_API_TOKEN",
        ):
            monkeypatch.delenv(var, raising=False)

    def test_no_backend_configured_reports_the_routes(self, monkeypatch, tmp_path):
        self._unconfigured(monkeypatch, tmp_path)
        out = _prior_art_search(query="pfas free seals", source="patents")
        assert out["patents"] == []
        assert out["counts"]["patents"] == 0
        assert out.get("patents_error"), "an unconfigured source reported a bare zero"
        for credential in ("PRISM_PATENT_TABLE", "LENS_API_TOKEN", "PRISM_PLATFORM_URL"):
            assert credential in out["patents_error"]

    @patch("app.tools.data_collectors.patent_collector.PatentCollector.collect")
    def test_a_build_or_ceiling_failure_surfaces(self, mock_collect):
        """The byte ceiling makes an over-large job FAIL. That failure has to
        reach the agent, not be flattened into an empty array."""
        from app.tools.data_collectors.base_collector import CollectorConfigError

        mock_collect.side_effect = CollectorConfigError(
            "BigQuery patent search failed (Query exceeded limit for bytes billed)"
        )
        out = _prior_art_search(query="alloy", source="patents")
        assert out["patents"] == []
        assert "bytes billed" in out["patents_error"]

    def test_a_blank_query_is_named_rather_than_answered_with_zero(self):
        out = _prior_art_search(query="   ", source="patents")
        assert out["counts"]["patents"] == 0
        assert "non-empty query" in out["patents_error"]

    def test_a_zero_is_attributed_to_the_backend_that_produced_it(
        self, monkeypatch, tmp_path
    ):
        """Driven through the real collector — a mocked `collect` would not
        exercise the selection this asserts."""
        self._unconfigured(monkeypatch, tmp_path)
        monkeypatch.setenv("LENS_API_TOKEN", "t")
        resp = MagicMock()
        resp.json.return_value = {"data": []}   # ran fine, found nothing
        resp.raise_for_status = MagicMock()
        with patch("requests.post", return_value=resp):
            out = _prior_art_search(query="alloy", source="patents")
        assert out.get("patents_error") is None
        assert out["counts"]["patents"] == 0
        assert out["patents_backend"] == "lens"

    def test_a_patent_failure_does_not_take_the_papers_branch_down(
        self, monkeypatch, tmp_path
    ):
        self._unconfigured(monkeypatch, tmp_path)
        with patch("app.tools.search._literature_search_impl") as lit:
            lit.return_value = {"results": [{"title": "a paper"}], "count": 1}
            out = _prior_art_search(query="alloy", source="both")
        assert out["counts"]["papers"] == 1
        assert out.get("patents_error")
