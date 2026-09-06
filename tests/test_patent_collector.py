"""Patent collector: cost-first, and never silently empty.

The three properties worth guarding are cost, honesty and neutrality.

COST, because the failure that produced this file was measured: seventeen
`prior_art_search` calls in one afternoon billed 240.03 GB EACH — 4.3 TB, about
EUR 24 — by falling through an unconfigured default into a full scan of the
public corpus. So: nothing is defaulted, the corpus is cut once instead of
scanned per query, the cache is keyed on what a question MEANS rather than how
it was typed, and both ceilings actually bind.

HONESTY, because an empty list here reads as "nobody has patented this", a
business conclusion nothing may guess at. Every configuration, build and
ceiling failure raises.

NEUTRALITY, because PRISM's patent path must name no operator's resource.
"""

import json
import shlex
import sqlite3
import time
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from app.tools.data_collectors.base_collector import CollectorConfigError
from app.tools.data_collectors.patent_collector import (
    EXTRACT_DATASET,
    EXTRACT_TABLE,
    MATERIALS_CPC,
    MAX_EXTRACT_BUILD_BYTES_BILLED,
    MAX_PATENT_BYTES_BILLED,
    PUBLIC_TABLE,
    PatentCollector,
    _cache_path,
    _scope_fingerprint,
    resolve_backend,
)

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


#: The env vars that between them decide which backend answers. Tests start
#: from none of them set, so nothing leaks in from the developer's shell.
_ROUTING_VARS = (
    "PRISM_PATENT_BACKEND",
    "PRISM_PATENT_TABLE",
    "PRISM_PLATFORM_URL",
    "PRISM_PLATFORM_TOKEN",
    "LENS_API_TOKEN",
    "PRISM_PATENT_AUTOBUILD",
)


@pytest.fixture
def unconfigured(tmp_path, monkeypatch):
    """Own cache, no credentials: the state that used to cost 240 GB a search."""
    monkeypatch.setenv("PRISM_PATENT_CACHE", str(tmp_path))
    for var in _ROUTING_VARS:
        monkeypatch.delenv(var, raising=False)
    return monkeypatch


@pytest.fixture
def cache_dir(unconfigured, tmp_path):
    """The ordinary bigquery deployment: an operator's own flat extract.

    Configured rather than defaulted — there IS no default any more, which is
    the whole point of the change these tests cover.
    """
    unconfigured.setenv("PRISM_PATENT_TABLE", "operator-project.operator_ds.extract")
    return tmp_path


def _client_returning(rows):
    client = MagicMock()
    client.query.return_value.result.return_value = iter(rows)
    return client


def _client_over_corpus(corpus):
    """A client that ANSWERS THE QUESTION IT WAS ASKED.

    A fake returning fixed rows regardless of the needle cannot distinguish a
    correct answer from one stored for a different question — which is exactly
    how a cache-key collision reached review undetected. This one applies the
    `LIKE %needle%` the backend actually sends, over a tiny corpus.
    """

    def _query(sql, job_config=None, **_kwargs):
        needle = ""
        for param in getattr(job_config, "query_parameters", None) or []:
            if getattr(param, "name", None) == "needle":
                needle = (param.value or "").strip("%").lower()
        hits = [
            row
            for row in corpus
            if needle in (row["title"] or "").lower()
            or needle in (row["abstract"] or "").lower()
        ]
        job = MagicMock()
        job.result.return_value = iter(hits)
        return job

    client = MagicMock()
    client.query.side_effect = _query
    return client


_CORPUS = [
    {
        "publication_number": "US-1-A",
        "country_code": "US",
        "grant_date": 20200101,
        "filing_date": 20190101,
        "title": "Seals with PFAS for high voltage switchgear",
        "abstract": "A fluoropolymer seal.",
        "assignees": ["ACME"],
        "inventors": ["A"],
    },
    {
        "publication_number": "US-2-A",
        "country_code": "US",
        "grant_date": 20210101,
        "filing_date": 20200101,
        "title": "Seals without PFAS for high voltage switchgear",
        "abstract": "A hydrocarbon seal.",
        "assignees": ["ACME"],
        "inventors": ["B"],
    },
]


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

    def test_a_search_never_scans_the_public_corpus(self, cache_dir):
        """The 240 GB bill was a SEARCH against the full public table. Searches
        read the flat extract; only the one-time build touches the corpus."""
        client = _client_returning([ROW])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("alloy")
        sql = client.query.call_args[0][0]
        assert PUBLIC_TABLE not in sql
        assert "operator-project.operator_ds.extract" in sql

    def test_query_text_is_parameterised_not_interpolated(self, cache_dir):
        """The term is model-authored text and must never reach SQL directly."""
        client = _client_returning([])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("'; DROP TABLE x --")
        sql = client.query.call_args[0][0]
        assert "DROP TABLE" not in sql


class TestBackendsAreSwappable:
    """Nobody is locked into the service PRISM sells."""

    def test_bigquery_answers_when_the_operator_configured_their_own_table(
        self, cache_dir
    ):
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


class TestBackendSelectionFollowsCredentials:
    """The bug was a DEFAULT. `PRISM_PATENT_BACKEND` defaulted to "bigquery",
    so a deployment that had configured nothing at all still ran — straight
    into a full scan of the public corpus, at 240.03 GB a search."""

    def test_nothing_configured_refuses_and_names_every_route(self, unconfigured):
        with pytest.raises(CollectorConfigError) as err:
            PatentCollector().collect("alloy")
        message = str(err.value)
        # A refusal that does not say how to fix it is a dead end, so every
        # route and the credential it needs must be in the message.
        for route in ("platform", "bigquery", "lens"):
            assert route in message, f"the refusal never mentions {route}"
        for credential in (
            "PRISM_PLATFORM_URL",
            "PRISM_PLATFORM_TOKEN",
            "PRISM_PATENT_TABLE",
            "LENS_API_TOKEN",
            "PRISM_PATENT_BACKEND",
        ):
            assert credential in message, f"the refusal never names {credential}"

    def test_nothing_configured_never_reaches_bigquery(self, unconfigured):
        """The refusal has to happen BEFORE a client exists — this is the exact
        path that billed EUR 24."""
        with patch("google.cloud.bigquery.Client") as client:
            with pytest.raises(CollectorConfigError):
                PatentCollector().collect("alloy")
        client.assert_not_called()

    def test_platform_credentials_select_platform(self, unconfigured):
        unconfigured.setenv("PRISM_PLATFORM_URL", "https://patents.example.invalid")
        unconfigured.setenv("PRISM_PLATFORM_TOKEN", "t")
        assert resolve_backend() == "platform"

    def test_half_the_platform_credentials_do_not_select_it(self, unconfigured):
        """A URL with no token cannot search; falling to it would just fail
        later with a worse message."""
        unconfigured.setenv("PRISM_PLATFORM_URL", "https://patents.example.invalid")
        unconfigured.setenv("LENS_API_TOKEN", "t")
        assert resolve_backend() == "lens"

    def test_a_table_selects_bigquery(self, unconfigured):
        unconfigured.setenv("PRISM_PATENT_TABLE", "p.d.t")
        assert resolve_backend() == "bigquery"

    def test_a_lens_token_selects_lens(self, unconfigured):
        unconfigured.setenv("LENS_API_TOKEN", "t")
        assert resolve_backend() == "lens"

    def test_platform_outranks_table_and_token(self, unconfigured):
        unconfigured.setenv("PRISM_PLATFORM_URL", "https://patents.example.invalid")
        unconfigured.setenv("PRISM_PLATFORM_TOKEN", "t")
        unconfigured.setenv("PRISM_PATENT_TABLE", "p.d.t")
        unconfigured.setenv("LENS_API_TOKEN", "t")
        assert resolve_backend() == "platform"

    def test_a_table_outranks_a_lens_token(self, unconfigured):
        unconfigured.setenv("PRISM_PATENT_TABLE", "p.d.t")
        unconfigured.setenv("LENS_API_TOKEN", "t")
        assert resolve_backend() == "bigquery"

    def test_an_explicit_choice_beats_every_credential(self, unconfigured):
        """bigquery stays first-class: an operator who says so gets it, even
        with a platform subscription sitting configured next to it."""
        unconfigured.setenv("PRISM_PLATFORM_URL", "https://patents.example.invalid")
        unconfigured.setenv("PRISM_PLATFORM_TOKEN", "t")
        unconfigured.setenv("PRISM_PATENT_BACKEND", "bigquery")
        assert resolve_backend() == "bigquery"

    def test_an_explicit_choice_is_obeyed_even_unconfigured(self, unconfigured):
        """So the error names the missing credential instead of silently
        routing the search somewhere the operator did not ask for."""
        unconfigured.setenv("PRISM_PATENT_BACKEND", "lens")
        with pytest.raises(CollectorConfigError, match="LENS_API_TOKEN"):
            PatentCollector().collect("alloy")

    def test_the_answering_backend_is_recorded(self, cache_dir):
        """A zero has to be attributable to a named corpus."""
        collector = PatentCollector()
        with patch("google.cloud.bigquery.Client", return_value=_client_returning([])):
            collector.collect("alloy")
        assert collector.last_backend == "bigquery"


class _FakeBigQuery:
    """A BigQuery client that records jobs instead of billing for them.

    Deliberately not a bare MagicMock: these tests are about WHICH jobs run and
    with what ceiling, and a MagicMock answers every question yes.
    """

    def __init__(self, rows=(), table_exists=False):
        self.project = "caller-project"
        self.table_exists = table_exists
        self.rows = list(rows)
        self.jobs = []       # (sql, job_config)
        self.datasets = []

    def get_table(self, target):
        from google.api_core.exceptions import NotFound

        if not self.table_exists:
            raise NotFound(f"{target} not found")
        return MagicMock()

    def create_dataset(self, dataset, exists_ok=False):
        self.datasets.append(dataset)
        return dataset

    def query(self, sql, job_config=None):
        self.jobs.append((sql, job_config))
        job = MagicMock()
        job.job_id = f"job-{len(self.jobs)}"
        job.total_bytes_processed = 253 * 1024**3
        job.total_bytes_billed = 253 * 1024**3
        is_build = sql.strip().startswith("CREATE TABLE")
        if is_build and not getattr(job_config, "dry_run", False):
            self.table_exists = True   # the build landed the table
        job.result.return_value = iter([] if is_build else self.rows)
        return job

    # — helpers the assertions read —
    def builds(self):
        return [
            (sql, cfg) for sql, cfg in self.jobs
            if sql.strip().startswith("CREATE TABLE")
            and not getattr(cfg, "dry_run", False)
        ]

    def searches(self):
        return [
            (sql, cfg) for sql, cfg in self.jobs
            if sql.strip().startswith("SELECT")
        ]


@pytest.fixture
def autobuild(unconfigured, tmp_path):
    """bigquery chosen explicitly, with no extract configured — the case where
    PRISM cuts the corpus itself."""
    unconfigured.setenv("PRISM_PATENT_BACKEND", "bigquery")
    return unconfigured


class TestTheCorpusIsCutOnce:
    """What is expensive is the CORPUS, not the query. A per-query result cache
    was structurally incapable of helping: seventeen phrasings were seventeen
    misses and therefore seventeen 240 GB scans."""

    TARGET = f"caller-project.{EXTRACT_DATASET}.{EXTRACT_TABLE}"

    def test_the_extract_lands_in_the_callers_own_project(self, autobuild):
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("alloy")
        build_sql, _ = fake.builds()[0]
        # Read off the client at runtime — no project is written in the source.
        assert self.TARGET in build_sql

    def test_the_build_filters_to_the_materials_classes(self, autobuild):
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("alloy")
        build_sql, _ = fake.builds()[0]
        assert PUBLIC_TABLE in build_sql
        for cpc in MATERIALS_CPC:
            assert cpc in build_sql

    def test_the_build_projects_the_columns_the_search_reads(self, autobuild):
        """A build whose shape does not match the search path produces an
        extract that silently fails every query."""
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("alloy")
        build_sql, _ = fake.builds()[0]
        for column in (
            "publication_number", "country_code", "grant_date", "filing_date",
            "title", "abstract", "assignees", "inventors",
        ):
            assert column in build_sql

    def test_the_search_then_reads_the_extract(self, autobuild):
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            results = PatentCollector().collect("alloy")
        search_sql, _ = fake.searches()[0]
        assert self.TARGET in search_sql
        assert PUBLIC_TABLE not in search_sql
        assert len(results) == 1

    def test_the_build_happens_once_not_once_per_search(self, autobuild):
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("titanium powder porosity")
            assert len(fake.builds()) == 1
            PatentCollector().collect("nickel superalloy creep")
            assert len(fake.builds()) == 1, "the corpus is cut once, not per query"

    def test_an_existing_extract_is_never_rebuilt(self, autobuild):
        fake = _FakeBigQuery(rows=[ROW], table_exists=True)
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("alloy")
        assert fake.builds() == []
        assert len(fake.searches()) == 1

    def test_a_permission_error_is_not_read_as_absent(self, autobuild):
        """"Not there" would trigger a 253 GB build. A 403 must not say that."""
        from google.api_core.exceptions import Forbidden

        fake = _FakeBigQuery()
        fake.get_table = MagicMock(side_effect=Forbidden("no access"))
        with patch("google.cloud.bigquery.Client", return_value=fake):
            with pytest.raises(CollectorConfigError, match="could not resolve"):
                PatentCollector().collect("alloy")
        assert fake.jobs == [], "a permissions fault must not start a build"

    def test_autobuild_off_refuses_with_a_command_a_human_can_run(self, autobuild):
        autobuild.setenv("PRISM_PATENT_AUTOBUILD", "0")
        fake = _FakeBigQuery()
        with patch("google.cloud.bigquery.Client", return_value=fake):
            with pytest.raises(CollectorConfigError) as err:
                PatentCollector().collect("alloy")
        message = str(err.value)
        assert "PRISM_PATENT_TABLE" in message
        assert fake.jobs == [], "a refusal must not run a job"

        # The command has to SURVIVE a shell. The SQL carries single quotes
        # and backticks, so a naively quoted one would not run — and a command
        # that does not run is the same dead end as no message at all.
        command = shlex.split(message.split("\n\n", 1)[1])
        assert command[:2] == ["bq", "--location=US"]
        assert f"--maximum_bytes_billed={MAX_EXTRACT_BUILD_BYTES_BILLED}" in command
        assert command[-1].startswith("CREATE TABLE")
        assert PUBLIC_TABLE in command[-1]

    def test_the_cost_is_estimated_before_it_is_spent(self, autobuild):
        """A dry run bills nothing and is the only honest estimate."""
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("alloy")
        dry_runs = [c for _, c in fake.jobs if getattr(c, "dry_run", False)]
        assert dry_runs, "nothing estimated the build before running it"
        # …and it happened first.
        assert getattr(fake.jobs[0][1], "dry_run", False) is True


class TestBothCeilingsBind:
    """BigQuery bills bytes SCANNED, so an over-large job is paid for whether
    or not anyone reads the answer. Over the ceiling the job FAILS and costs
    nothing — the only failure mode that cannot quietly spend money."""

    def test_the_search_ceiling_binds(self, cache_dir):
        client = _client_returning([ROW])
        with patch("google.cloud.bigquery.Client", return_value=client):
            PatentCollector().collect("alloy")
        config = client.query.call_args.kwargs["job_config"]
        assert config.maximum_bytes_billed == MAX_PATENT_BYTES_BILLED

    def test_the_build_ceiling_binds(self, autobuild):
        fake = _FakeBigQuery(rows=[ROW])
        with patch("google.cloud.bigquery.Client", return_value=fake):
            PatentCollector().collect("alloy")
        _, config = fake.builds()[0]
        assert config.maximum_bytes_billed == MAX_EXTRACT_BUILD_BYTES_BILLED

    def test_the_build_ceiling_is_its_own_larger_number(self):
        """One corpus pass is 253 GB. A build sized at the per-search ceiling
        could never run; a search sized at the build ceiling is the bug."""
        assert MAX_EXTRACT_BUILD_BYTES_BILLED > MAX_PATENT_BYTES_BILLED
        assert MAX_EXTRACT_BUILD_BYTES_BILLED >= 253 * 1024**3
        assert MAX_PATENT_BYTES_BILLED < 253 * 1024**3


class TestThePatentPathNamesNoOperator:
    """PRISM's data-acquisition plane must be operator-neutral: an operator
    supplies their own table, URL and token, and PRISM ships none of them. A
    resource name baked in here would blur the IP boundary.

    `app/tools/_platform_creds.py` is deliberately out of scope — the operator
    credential/CLI surface is a separate plane and owns those names.
    """

    ROOT = Path(__file__).resolve().parents[1]
    OPERATOR_NAMES = ("marc27", "mirdyne", "kuttaka")

    #: The one BigQuery resource that may be named: Google's free public
    #: corpus, billed to the caller's own account. It is what the extract is
    #: cut from, so the build has to know it.
    ALLOWED_TABLES = {PUBLIC_TABLE}

    #: Public, keyless-or-operator-keyed endpoints. Nobody's private service.
    #: www.lens.org is the vendor's own subscription page — where the human
    #: is sent to buy access when the tool cannot — not an API endpoint.
    ALLOWED_HOSTS = {"api.lens.org", "www.lens.org", "patents.google.com"}

    def _acquisition_sources(self):
        files = sorted((self.ROOT / "app" / "tools" / "data_collectors").glob("*.py"))
        return files + [self.ROOT / "app" / "tools" / "search.py"]

    def test_no_operator_name_appears_in_the_acquisition_plane(self):
        for path in self._acquisition_sources():
            text = path.read_text().lower()
            for name in self.OPERATOR_NAMES:
                assert name not in text, f"{path.name} names the operator {name!r}"

    def _named_tables(self, path):
        import re

        return set(
            re.findall(
                r'"([a-z0-9][a-z0-9-]*\.[a-z0-9_]+\.[a-z0-9_]+)"', path.read_text()
            )
        )

    def _named_hosts(self, path):
        import re

        return set(re.findall(r"https?://([a-zA-Z0-9.-]+)", path.read_text()))

    def test_the_only_named_table_is_the_public_corpus(self):
        collector = (
            self.ROOT / "app" / "tools" / "data_collectors" / "patent_collector.py"
        )
        # The scan must be able to SEE a table, or this guard passes vacuously
        # the day someone changes how the constant is written.
        assert PUBLIC_TABLE in self._named_tables(collector)
        for path in (collector, self.ROOT / "app" / "tools" / "search.py"):
            found = self._named_tables(path)
            assert found <= self.ALLOWED_TABLES, (
                f"{path.name} hardcodes a BigQuery resource: "
                f"{sorted(found - self.ALLOWED_TABLES)}"
            )

    def test_the_patent_path_hardcodes_no_private_endpoint(self):
        collector = (
            self.ROOT / "app" / "tools" / "data_collectors" / "patent_collector.py"
        )
        assert self._named_hosts(collector), "the host scan found nothing to check"
        for path in (collector, self.ROOT / "app" / "tools" / "search.py"):
            hosts = self._named_hosts(path)
            assert hosts <= self.ALLOWED_HOSTS, (
                f"{path.name} hardcodes {sorted(hosts - self.ALLOWED_HOSTS)}; "
                "the operator supplies their own endpoint"
            )


class TestCacheAnswersTheQuestionAsked:
    """A cache hit must belong to the question that was asked.

    The withdrawn normalised key collapsed "seals with PFAS" and "seals without
    PFAS" to one entry ("without" is a stopword) while the backend matched the
    literal phrase. With empty results cached for 30 days, one phrasing served a
    false "no prior art" to every rephrasing for a month.
    """

    def test_a_negation_does_not_reuse_its_affirmation(self, tmp_path, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_CACHE", str(tmp_path))
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "bigquery")
        monkeypatch.setenv("PRISM_PATENT_TABLE", "proj.ds.extract")
        client = _client_over_corpus(_CORPUS)
        with patch("google.cloud.bigquery.Client", return_value=client):
            collector = PatentCollector()
            affirmed = collector.collect(query="Seals with PFAS", max_results=10)
            negated = collector.collect(query="Seals without PFAS", max_results=10)

        assert [r["source_id"] for r in affirmed] == ["patent:US-1-A"]
        assert [r["source_id"] for r in negated] == ["patent:US-2-A"], (
            "a negation must not be served the affirmation's cached results"
        )

    def test_the_same_question_twice_is_a_cache_hit(self, tmp_path, monkeypatch):
        monkeypatch.setenv("PRISM_PATENT_CACHE", str(tmp_path))
        monkeypatch.setenv("PRISM_PATENT_BACKEND", "bigquery")
        monkeypatch.setenv("PRISM_PATENT_TABLE", "proj.ds.extract")
        client = _client_over_corpus(_CORPUS)
        with patch("google.cloud.bigquery.Client", return_value=client):
            collector = PatentCollector()
            first = collector.collect(query="Seals with PFAS", max_results=10)
            second = collector.collect(query="  seals WITH pfas  ", max_results=10)

        assert first == second
        assert client.query.call_count == 1, (
            "the second ask must be served from cache, issuing NO BigQuery job"
        )
