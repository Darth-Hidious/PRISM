"""Patent collector: cache-first, and never silently empty.

The two properties worth guarding are cost and honesty. Prior-art search is
called repeatedly inside one investigation, so a repeat must not reach
BigQuery at all; and a failed lookup must raise, because an empty list here
reads as "nobody has patented this" — a business conclusion nothing may guess.
"""

from unittest.mock import MagicMock, patch

import pytest

from app.tools.data_collectors.base_collector import CollectorConfigError
from app.tools.data_collectors.patent_collector import PatentCollector

ROW = {
    "publication_number": "US-2024300018-A1",
    "country_code": "US",
    "grant_date": 20240912,
    "filing_date": 20230301,
    "title": "Process for manufacturing an aluminum alloy part by laser powder bed fusion",
    "abstract": "A process for manufacturing an aluminium alloy part.",
    "assignees": ["C-TEC CONSTELLIUM TECH CENTER"],
    "inventors": ["JANE DOE"],
}


@pytest.fixture
def cache_dir(tmp_path, monkeypatch):
    """Every test gets its own cache, so one test's warm hit is not another's."""
    monkeypatch.setenv("PRISM_PATENT_CACHE", str(tmp_path))
    monkeypatch.delenv("PRISM_PATENT_TABLE", raising=False)
    monkeypatch.delenv("PRISM_PATENT_BACKEND", raising=False)
    return tmp_path


def _client_returning(rows):
    client = MagicMock()
    client.query.return_value.result.return_value = iter(rows)
    return client


class TestPatentCollector:
    def test_name(self):
        assert PatentCollector().name == "patents"

    def test_supported_params(self):
        assert PatentCollector().supported_params() == ["query", "max_results"]

    def test_empty_query_asks_nothing(self, cache_dir):
        with patch("google.cloud.bigquery.Client") as client:
            assert PatentCollector().collect("") == []
        client.assert_not_called()

    def test_result_carries_provenance_and_claim_status(self, cache_dir):
        with patch("google.cloud.bigquery.Client", return_value=_client_returning([ROW])):
            results = PatentCollector().collect("laser powder bed fusion")
        assert len(results) == 1
        hit = results[0]
        assert hit["source_id"] == "US-2024300018-A1".join(("patent:", ""))
        assert hit["jurisdiction"] == "US"
        assert hit["applicants"] == ["C-TEC CONSTELLIUM TECH CENTER"]
        assert hit["url"].endswith("US-2024300018-A1")
        # A patent is evidence of a CLAIM, never of a measurement; ranking
        # downstream depends on that distinction.
        assert hit["evidence_kind"] == "claim"

    def test_repeat_search_never_reaches_bigquery(self, cache_dir):
        """The whole point: the second identical call costs nothing."""
        with patch(
            "google.cloud.bigquery.Client", return_value=_client_returning([ROW])
        ) as client:
            first = PatentCollector().collect("high entropy alloy")
            assert client.call_count == 1
            second = PatentCollector().collect("high entropy alloy")
            assert client.call_count == 1, "a cached query must not be re-issued"
        assert first == second

    def test_expired_cache_is_refetched(self, cache_dir, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_CACHE_TTL", "0")
        with patch(
            "google.cloud.bigquery.Client", return_value=_client_returning([ROW])
        ) as client:
            PatentCollector().collect("titanium")
            PatentCollector().collect("titanium")
            assert client.call_count == 2, "a stale entry must not be served"

    def test_a_different_query_is_a_different_key(self, cache_dir):
        with patch(
            "google.cloud.bigquery.Client", return_value=_client_returning([ROW])
        ) as client:
            PatentCollector().collect("nickel superalloy")
            PatentCollector().collect("cobalt superalloy")
            assert client.call_count == 2

    def test_failure_raises_rather_than_reporting_no_prior_art(self, cache_dir):
        """An empty list would be read as "nothing has been patented"."""
        client = MagicMock()
        client.query.side_effect = Exception("permission denied")
        with patch("google.cloud.bigquery.Client", return_value=client):
            with pytest.raises(CollectorConfigError, match="BigQuery patent search failed"):
                PatentCollector().collect("graphene")

    def test_failure_is_not_cached(self, cache_dir):
        """A transient outage must not poison later searches."""
        broken = MagicMock()
        broken.query.side_effect = Exception("transient")
        with patch("google.cloud.bigquery.Client", return_value=broken):
            with pytest.raises(CollectorConfigError):
                PatentCollector().collect("inconel")
        with patch("google.cloud.bigquery.Client", return_value=_client_returning([ROW])):
            assert len(PatentCollector().collect("inconel")) == 1

    def test_operator_table_is_used_when_configured(self, cache_dir, monkeypatch):
        """PRISM names no operator's project; it uses one only if told to."""
        monkeypatch.setenv("PRISM_PATENT_TABLE", "some-project.some_ds.some_table")
        client = _client_returning([ROW])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("alloy")
        sql = client.query.call_args[0][0]
        assert "some-project.some_ds.some_table" in sql
        assert "patents-public-data" not in sql

    def test_public_dataset_is_the_default(self, cache_dir):
        client = _client_returning([ROW])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("alloy")
        sql = client.query.call_args[0][0]
        assert "patents-public-data.patents.publications" in sql

    def test_query_text_is_parameterised_not_interpolated(self, cache_dir):
        """The term is model-authored text and must never reach SQL directly."""
        client = _client_returning([])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("'; DROP TABLE x --")
        sql = client.query.call_args[0][0]
        assert "DROP TABLE" not in sql


class TestBackendsAreSwappable:
    """Nobody is locked into the service PRISM sells."""

    def test_bigquery_is_the_default_because_it_needs_no_purchase(self, cache_dir):
        client = _client_returning([ROW])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("alloy")
        assert client.query.called

    def test_an_operator_replaces_the_whole_collector_to_bring_their_own(self, cache_dir):
        """The extension path is the EXISTING one: the collector registry is
        keyed by name, so a site's own "patents" collector replaces this one.
        No second plugin system for one source."""
        from app.tools.data_collectors.base_collector import (
            CollectorRegistry,
            DataCollector,
        )

        class InHousePatents(DataCollector):
            name = "patents"

            def collect(self, query="", max_results=20, **kwargs):
                return [{"source": "in_house", "source_id": "patent:INT-1"}]

            def supported_params(self):
                return ["query", "max_results"]

        reg = CollectorRegistry()
        reg.register(PatentCollector())
        reg.register(InHousePatents())
        assert reg.get("patents").collect(query="alloy")[0]["source"] == "in_house"

    def test_lens_backend_demands_its_token_rather_than_returning_empty(
        self, cache_dir, monkeypatch
    ):
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "lens")
        monkeypatch.delenv("LENS_API_TOKEN", raising=False)
        with pytest.raises(CollectorConfigError, match="LENS_API_TOKEN"):
            PatentCollector().collect("alloy")

    def test_platform_backend_names_the_free_alternative_when_unconfigured(
        self, cache_dir, monkeypatch
    ):
        """A billed backend that is not set up must not dead-end the user."""
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "platform")
        monkeypatch.delenv("PRISM_PLATFORM_URL", raising=False)
        monkeypatch.delenv("PRISM_PLATFORM_TOKEN", raising=False)
        with pytest.raises(CollectorConfigError, match="PRISM_PATENT_BACKEND=bigquery"):
            PatentCollector().collect("alloy")

    def test_unknown_backend_lists_what_is_available(self, cache_dir, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "nope")
        with pytest.raises(CollectorConfigError, match="unknown patent backend"):
            PatentCollector().collect("alloy")

    def test_backends_do_not_share_a_cache_entry(self, cache_dir, monkeypatch):
        """Two services disagree; serving one's answer for the other would
        misreport prior art."""
        monkeypatch.setenv("LENS_API_TOKEN", "t")
        lens_rows = {"data": [{"lens_id": "L-1", "title": "lens hit"}]}
        resp = MagicMock()
        resp.json.return_value = lens_rows
        resp.raise_for_status = MagicMock()

        monkeypatch.setenv("PRISM_PATENT_BACKEND", "bigquery")
        with patch("google.cloud.bigquery.Client", return_value=_client_returning([ROW])):
            first = PatentCollector().collect("same query")

        monkeypatch.setenv("PRISM_PATENT_BACKEND", "lens")
        with patch("requests.post", return_value=resp):
            second = PatentCollector().collect("same query")

        assert first[0]["source_id"] == "patent:US-2024300018-A1"
        assert second[0]["source_id"] == "patent:L-1"


class TestAMalformedResponseIsNotAClearField:
    """Found by adversarial review: a 200 whose body lacks the results key
    returned [] AND cached it for 30 days — the exact "nobody has patented
    this" lie the module's docstring warns against."""

    def test_lens_missing_data_key_raises(self, cache_dir, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "lens")
        monkeypatch.setenv("LENS_API_TOKEN", "t")
        resp = MagicMock()
        resp.json.return_value = {"status": "throttled", "message": "slow down"}
        resp.raise_for_status = MagicMock()
        with patch("requests.post", return_value=resp):
            with pytest.raises(CollectorConfigError, match="no `data` array"):
                PatentCollector().collect("alloy")

    def test_platform_missing_results_key_raises(self, cache_dir, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "platform")
        monkeypatch.setenv("PRISM_PLATFORM_URL", "https://example.invalid")
        monkeypatch.setenv("PRISM_PLATFORM_TOKEN", "t")
        resp = MagicMock()
        resp.json.return_value = {"detail": "degraded"}
        resp.raise_for_status = MagicMock()
        with patch("requests.get", return_value=resp):
            with pytest.raises(CollectorConfigError, match="no `results` array"):
                PatentCollector().collect("alloy")

    def test_a_genuinely_empty_field_is_still_allowed(self, cache_dir, monkeypatch):
        """Zero hits is a real answer; only a MISSING key is a failure."""
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "lens")
        monkeypatch.setenv("LENS_API_TOKEN", "t")
        resp = MagicMock()
        resp.json.return_value = {"data": []}
        resp.raise_for_status = MagicMock()
        with patch("requests.post", return_value=resp):
            assert PatentCollector().collect("nothing matches this") == []

    def test_a_broken_response_is_never_cached(self, cache_dir, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "lens")
        monkeypatch.setenv("LENS_API_TOKEN", "t")
        bad = MagicMock()
        bad.json.return_value = {"status": "throttled"}
        bad.raise_for_status = MagicMock()
        with patch("requests.post", return_value=bad):
            with pytest.raises(CollectorConfigError):
                PatentCollector().collect("alloy")
        good = MagicMock()
        good.json.return_value = {"data": [{"lens_id": "L-9", "title": "real hit"}]}
        good.raise_for_status = MagicMock()
        with patch("requests.post", return_value=good):
            assert len(PatentCollector().collect("alloy")) == 1

    def test_a_different_account_does_not_read_another_tenants_cache(
        self, cache_dir, monkeypatch
    ):
        """The docstring recommends SHARED cache storage, so the key must carry
        whose view of the corpus produced it — a private CPC extract must not
        answer a deployment that never had access to it."""
        monkeypatch.setenv("PRISM_PATENT_TABLE", "tenant-a.private.extract")
        with patch("google.cloud.bigquery.Client", return_value=_client_returning([ROW])):
            first = PatentCollector().collect("shared term")
        assert len(first) == 1

        monkeypatch.setenv("PRISM_PATENT_TABLE", "tenant-b.other.extract")
        other = MagicMock()
        other.query.return_value.result.return_value = iter([])
        with patch("google.cloud.bigquery.Client", return_value=other) as client:
            second = PatentCollector().collect("shared term")
        assert client.called, "tenant B must not be served tenant A's cached rows"
        assert second == []
