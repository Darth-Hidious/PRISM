"""Tests for PatentCollector."""
import pytest
from unittest.mock import patch, MagicMock
from app.data.patent_collector import PatentCollector


LENS_RESPONSE = {
    "data": [
        {
            "lens_id": "patent-001",
            "title": "High Entropy Alloy Coating Method",
            "abstract": "A method for applying HEA coatings to substrates.",
            "date_published": "2023-06-15",
            "inventor": [
                {"extracted_name": {"value": "John Doe"}},
                {"extracted_name": {"value": "Jane Smith"}},
            ],
            "applicant": [
                {"extracted_name": {"value": "AlloyTech Inc."}},
            ],
            "jurisdiction": "US",
        },
        {
            "lens_id": "patent-002",
            "title": "Tungsten Alloy Composition",
            "abstract": "Novel tungsten-based alloy composition.",
            "date_published": "2024-01-10",
            "inventor": [
                {"extracted_name": {"value": "Alice Wang"}},
            ],
            "applicant": None,
            "jurisdiction": "EP",
        },
    ]
}


def _mock_lens_response():
    resp = MagicMock()
    resp.json.return_value = LENS_RESPONSE
    resp.raise_for_status = MagicMock()
    return resp


class TestPatentCollector:
    def test_name(self):
        c = PatentCollector()
        assert c.name == "patents"

    def test_supported_params(self):
        c = PatentCollector()
        assert set(c.supported_params()) == {"query", "max_results"}

    def test_collect_empty_query(self):
        c = PatentCollector()
        assert c.collect(query="") == []

    @patch.dict("os.environ", {}, clear=True)
    def test_collect_no_token(self):
        c = PatentCollector()
        assert c.collect(query="alloy") == []

    @patch("app.data.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_collect_success(self, mock_requests):
        mock_requests.post.return_value = _mock_lens_response()
        c = PatentCollector()
        results = c.collect(query="high entropy alloy", max_results=20)
        assert len(results) == 2
        assert results[0]["source"] == "lens_patents"
        assert results[0]["source_id"] == "lens:patent-001"
        assert results[0]["title"] == "High Entropy Alloy Coating Method"
        assert results[0]["inventors"] == ["John Doe", "Jane Smith"]
        assert results[0]["applicants"] == ["AlloyTech Inc."]
        assert results[0]["jurisdiction"] == "US"
        assert results[0]["type"] == "patent"

    @patch("app.data.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_collect_second_patent(self, mock_requests):
        mock_requests.post.return_value = _mock_lens_response()
        c = PatentCollector()
        results = c.collect(query="tungsten", max_results=20)
        assert results[1]["title"] == "Tungsten Alloy Composition"
        assert results[1]["applicants"] == []  # applicant was None
        assert results[1]["jurisdiction"] == "EP"

    @patch("app.data.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_collect_api_error(self, mock_requests):
        mock_requests.post.side_effect = Exception("API error")
        c = PatentCollector()
        results = c.collect(query="alloy")
        assert results == []

    @patch("app.data.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_collect_empty_data(self, mock_requests):
        resp = MagicMock()
        resp.json.return_value = {"data": []}
        resp.raise_for_status = MagicMock()
        mock_requests.post.return_value = resp
        c = PatentCollector()
        results = c.collect(query="alloy")
        assert results == []

    @patch("app.data.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_collect_max_results_caps_at_50(self, mock_requests):
        mock_requests.post.return_value = _mock_lens_response()
        c = PatentCollector()
        c.collect(query="alloy", max_results=200)
        call_args = mock_requests.post.call_args
        body = call_args[1]["json"]
        assert body["size"] == 50  # capped

    @patch("app.data.patent_collector.requests")
    @patch.dict("os.environ", {"LENS_API_TOKEN": "test-token"})
    def test_collect_request_headers(self, mock_requests):
        mock_requests.post.return_value = _mock_lens_response()
        c = PatentCollector()
        c.collect(query="alloy")
        call_args = mock_requests.post.call_args
        headers = call_args[1]["headers"]
        assert headers["Authorization"] == "Bearer test-token"
        assert headers["Content-Type"] == "application/json"
