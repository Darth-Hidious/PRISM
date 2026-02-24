"""Tests for LiteratureCollector."""
import pytest
from unittest.mock import patch, MagicMock
from app.data.literature_collector import LiteratureCollector


ARXIV_XML = """<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.00001</id>
    <title>Phase Stability of W-Rh Alloys</title>
    <summary>We study the phase stability of tungsten-rhenium alloys.</summary>
    <published>2024-01-15T00:00:00Z</published>
    <author><name>Alice Smith</name></author>
    <author><name>Bob Jones</name></author>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2401.00002</id>
    <title>High Entropy Alloys Review</title>
    <summary>A comprehensive review of HEA properties.</summary>
    <published>2024-02-01T00:00:00Z</published>
    <author><name>Carol Lee</name></author>
  </entry>
</feed>"""

S2_JSON = {
    "data": [
        {
            "paperId": "abc123",
            "title": "Tungsten Alloy Properties",
            "authors": [{"name": "Dave Kim"}, {"name": "Eve Chen"}],
            "abstract": "Study of tungsten alloy mechanical properties.",
            "year": 2024,
            "url": "https://www.semanticscholar.org/paper/abc123",
            "citationCount": 15,
            "externalIds": {"DOI": "10.1234/example"},
        }
    ]
}


def _mock_arxiv_response():
    resp = MagicMock()
    resp.text = ARXIV_XML
    resp.raise_for_status = MagicMock()
    return resp


def _mock_s2_response():
    resp = MagicMock()
    resp.json.return_value = S2_JSON
    resp.raise_for_status = MagicMock()
    return resp


class TestLiteratureCollector:
    def test_name(self):
        c = LiteratureCollector()
        assert c.name == "literature"

    def test_supported_params(self):
        c = LiteratureCollector()
        assert set(c.supported_params()) == {"query", "max_results", "sources"}

    def test_collect_empty_query(self):
        c = LiteratureCollector()
        assert c.collect(query="") == []

    @patch("app.data.literature_collector.requests")
    def test_collect_arxiv_only(self, mock_requests):
        mock_requests.get.return_value = _mock_arxiv_response()
        c = LiteratureCollector()
        results = c.collect(query="tungsten alloy", sources=["arxiv"])
        assert len(results) == 2
        assert results[0]["source"] == "arxiv"
        assert results[0]["title"] == "Phase Stability of W-Rh Alloys"
        assert results[0]["authors"] == ["Alice Smith", "Bob Jones"]
        assert results[0]["type"] == "paper"

    @patch("app.data.literature_collector.requests")
    def test_collect_s2_only(self, mock_requests):
        mock_requests.get.return_value = _mock_s2_response()
        c = LiteratureCollector()
        results = c.collect(query="tungsten alloy", sources=["semantic_scholar"])
        assert len(results) == 1
        assert results[0]["source"] == "semantic_scholar"
        assert results[0]["title"] == "Tungsten Alloy Properties"
        assert results[0]["citations"] == 15

    @patch("app.data.literature_collector.requests")
    def test_collect_both_sources(self, mock_requests):
        # First call: arxiv, second call: S2
        mock_requests.get.side_effect = [_mock_arxiv_response(), _mock_s2_response()]
        c = LiteratureCollector()
        results = c.collect(query="tungsten alloy", max_results=20)
        assert len(results) == 3  # 2 arxiv + 1 S2

    @patch("app.data.literature_collector.requests")
    def test_collect_max_results_limits(self, mock_requests):
        mock_requests.get.side_effect = [_mock_arxiv_response(), _mock_s2_response()]
        c = LiteratureCollector()
        results = c.collect(query="tungsten alloy", max_results=2)
        assert len(results) == 2

    @patch("app.data.literature_collector.requests")
    def test_collect_arxiv_error(self, mock_requests):
        mock_requests.get.side_effect = Exception("Network error")
        c = LiteratureCollector()
        results = c.collect(query="test", sources=["arxiv"])
        assert results == []

    @patch("app.data.literature_collector.requests")
    def test_collect_s2_error(self, mock_requests):
        mock_requests.get.side_effect = Exception("Network error")
        c = LiteratureCollector()
        results = c.collect(query="test", sources=["semantic_scholar"])
        assert results == []

    def test_parse_arxiv_xml(self):
        c = LiteratureCollector()
        results = c._parse_arxiv_xml(ARXIV_XML)
        assert len(results) == 2
        assert results[0]["source_id"] == "http://arxiv.org/abs/2401.00001"
        assert results[1]["title"] == "High Entropy Alloys Review"

    def test_parse_arxiv_xml_empty(self):
        c = LiteratureCollector()
        empty_xml = '<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"></feed>'
        results = c._parse_arxiv_xml(empty_xml)
        assert results == []

    @patch("app.data.literature_collector.requests")
    def test_search_s2_empty_data(self, mock_requests):
        resp = MagicMock()
        resp.json.return_value = {"data": []}
        resp.raise_for_status = MagicMock()
        mock_requests.get.return_value = resp
        c = LiteratureCollector()
        results = c._search_s2("test", 10)
        assert results == []

    @patch("app.data.literature_collector.requests")
    def test_search_s2_missing_authors(self, mock_requests):
        resp = MagicMock()
        resp.json.return_value = {
            "data": [{"paperId": "x", "title": "T", "authors": None,
                       "abstract": "", "year": None, "url": "",
                       "citationCount": 0}]
        }
        resp.raise_for_status = MagicMock()
        mock_requests.get.return_value = resp
        c = LiteratureCollector()
        results = c._search_s2("test", 10)
        assert len(results) == 1
        assert results[0]["authors"] == []
