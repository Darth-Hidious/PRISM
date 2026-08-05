# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Network-free tests for the HuggingFace PULL tool.

All HTTP is mocked at the two seams (``app.tools.hf._request_json`` and
``app.tools.hf._download_file``) and the matcher/licence logic is tested via
the pure :mod:`app.tools.hf_index`. No test requires network — a filter that
matches 0 tests would be reported as a failure, not a pass.
"""
from __future__ import annotations

import importlib

import pytest

import app.tools.hf as hf_mod
from app.tools import hf_index


# ---------------------------------------------------------------------------
# Licence classifier (pure)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "lic,commercial",
    [
        ("cc-by-4.0", True),
        ("mit", True),
        ("apache-2.0", True),
        ("bsd-3-clause", True),
        ("cc-by-nc-4.0", False),
        ("cc-by-nc-sa-4.0", False),
        ("other", None),  # HF 'other' = custom licence → cannot classify
        (None, None),     # no licence declared → unknown
        ("", None),
    ],
)
def test_classify_license_commercial_usability(lic, commercial):
    out = hf_index.classify_license(lic)
    assert out["license_commercial"] is commercial
    # Unknown is the honest default: never silently guessed as True.
    if commercial is None:
        assert "NOT" in out["license_note"] or "not" in out["license_note"].lower() or "no licence" in out["license_note"]


def test_classify_license_never_lies_about_other():
    """'other' must NOT be reported as commercially usable (brief: unknown stays unknown)."""
    out = hf_index.classify_license("other")
    assert out["license_commercial"] is None
    assert out["license"] == "other"


# ---------------------------------------------------------------------------
# Curated index resolves queries that plain keyword search fails
# ---------------------------------------------------------------------------


def test_curated_index_finds_mace_interatomic_potential():
    """The headline contrast: 'MACE interatomic potential' should resolve to
    MACE checkpoints via the curated index. The mocked raw Hub API (below)
    returns Macedonian-language NLP noise for the same query — exactly the
    live behaviour observed 2026-08-05."""
    hits = hf_index.match_index("MACE interatomic potential for inorganic crystals")
    repos = [h["id"] for h in hits]
    assert any(r.startswith("ACEtools/mace") for r in repos), repos
    # And the community MIT checkpoint should surface too.
    assert "jorgemunozl/mace_omat_medium" in repos


def test_curated_index_finds_omat24_dft_training_data():
    hits = hf_index.match_index("DFT relaxation training data for an interatomic potential")
    repos = [h["id"] for h in hits]
    assert "fairchem/OMAT24" in repos, repos


def test_curated_index_returns_empty_for_unrelated_query():
    # 'for'/'materials' stopwords and 'macedonian' must NOT pull in materials
    # entries (the substring bug where keyword 'mace' matched 'macedonian').
    assert hf_index.match_index("sentiment analysis for Macedonian tweets") == []
    assert hf_index.match_index("macedonian") == []


def test_anonymous_blocked_entries_keep_unknown_licence():
    """ACEtools entries are anonymous-API-401; their licence MUST be unknown,
    never guessed (brief: 'unknown stays unknown')."""
    ace = hf_index.index_entry_for("ACEtools/mace-mp-0")
    assert ace is not None
    presented = hf_index.present_index_entry(ace)
    assert presented["gated"] == "anonymous-blocked"
    assert presented["license"] is None
    assert presented["license_commercial"] is None


def test_verified_entries_carry_real_licence():
    omat = hf_index.present_index_entry(hf_index.index_entry_for("fairchem/OMAT24"))
    assert omat["license"] == "cc-by-4.0"
    assert omat["license_commercial"] is True  # CC-BY-4.0 permits commercial use
    assert omat["gated"] is False


# ---------------------------------------------------------------------------
# search() — blends index + API, tags sources, demonstrates the contrast
# ---------------------------------------------------------------------------


def _macedonian_noise_api(path, params=None):
    """Mock the live Hub API: a search for 'mace' returns Macedonian-language
    NLP models — the real, observed failure mode of plain keyword search."""
    if path.startswith("/api/models") and "search" in (params or {}):
        q = (params or {}).get("search", "").lower()
        if "mace" in q:
            return [
                {"id": "anon-submission-mk/bert-base-macedonian-cased",
                 "cardData": {}, "downloads": 12, "likes": 0, "gated": False, "tags": []},
            ]
        return []
    return []


def test_search_contrast_index_beats_keyword_api(monkeypatch):
    """DEFINITION-OF-DONE: the curated index resolves a query that plain Hub
    keyword search fails. Here the mocked API returns a Macedonian-language
    model for 'mace'; the index returns the real MACE checkpoints. We assert
    BOTH directions: index hits are relevant, API hit is the noise."""
    monkeypatch.setattr(hf_mod, "_request_json", _macedonian_noise_api)

    out = hf_mod._search("MACE interatomic potential", kind="model")
    assert "error" not in out

    index_hits = [r for r in out["results"] if r["source"].startswith("curated_index")]
    api_hits = [r for r in out["results"] if r["source"] == "huggingface_api"]

    # Index found the real MACE checkpoints.
    assert any(r["repo"].startswith("ACEtools/mace") or r["repo"] == "jorgemunozl/mace_omat_medium"
               for r in index_hits), [r["repo"] for r in index_hits]
    # Raw keyword search returned the Macedonian NLP noise — the contrast.
    assert any("macedonian" in r["repo"] for r in api_hits), [r["repo"] for r in api_hits]


def test_search_surfaces_licence_and_gated_per_result(monkeypatch):
    """DEFINITION-OF-DONE: licence + gated status surfaced at search time."""
    monkeypatch.setattr(hf_mod, "_request_json", _macedonian_noise_api)
    out = hf_mod._search("OMat24", kind="dataset")
    assert "error" not in out
    for r in out["results"]:
        # Every result must carry a licence verdict and a gated field.
        assert "license" in r
        assert "license_commercial" in r
        assert "gated" in r


def test_search_requires_query():
    out = hf_mod._search("")
    assert out == {"error": "query is required"}


def test_search_handles_api_network_error_honestly(monkeypatch):
    """If the live API is unreachable, search still returns index results and
    records the API error — it does not pretend success."""
    def boom(path, params=None):
        raise hf_mod._HFError(0, "network error")

    monkeypatch.setattr(hf_mod, "_request_json", boom)
    out = hf_mod._search("interatomic potential")
    assert out["api_errors"], "expected the API failure to be recorded"
    assert out["results"], "index results should still come through"


# ---------------------------------------------------------------------------
# details() — live licence/gated, 401 anonymous-blocked, 404 fallthrough
# ---------------------------------------------------------------------------


def test_details_returns_live_licence_for_readable_dataset(monkeypatch):
    payload = {
        "id": "fairchem/OMAT24",
        "cardData": {"license": "cc-by-4.0"},
        "gated": False,
        "downloads": 281,
        "likes": 75,
        "tags": ["license:cc-by-4.0"],
        "siblings": [{"rfilename": "README.md"}, {"rfilename": "train.parquet"}],
    }
    monkeypatch.setattr(hf_mod, "_request_json", lambda path, params=None: payload)
    out = hf_mod._details("fairchem/OMAT24", kind="dataset")
    assert out["license"] == "cc-by-4.0"
    assert out["license_commercial"] is True
    assert out["gated"] is False
    assert out["file_count"] == 2
    assert "train.parquet" in out["sample_files"]


def test_details_401_reports_anonymous_blocked_with_unknown_licence(monkeypatch):
    """A repo the anonymous API refuses (HTTP 401) must be reported as
    anonymous-blocked with unknown licence — never guessed."""
    def req(path, params=None):
        raise hf_mod._HFError(401, "anonymous access blocked")

    monkeypatch.setattr(hf_mod, "_request_json", req)
    out = hf_mod._details("ACEtools/mace-mp-0", kind="model")
    assert out["gated"] == "anonymous-blocked"
    assert out["license_commercial"] is None
    assert out["repo"] == "ACEtools/mace-mp-0"


def test_details_manual_gated_model_reports_non_fetchable(monkeypatch):
    payload = {
        "id": "facebook/OMAT24",
        "cardData": {"license": "other"},
        "gated": "manual",
        "downloads": 0, "likes": 102, "tags": ["license:other"],
        "siblings": [],
    }
    monkeypatch.setattr(hf_mod, "_request_json", lambda path, params=None: payload)
    out = hf_mod._details("facebook/OMAT24", kind="model")
    assert out["gated"] == "manual"
    # 'other' licence → commercial usability unknown (not guessed).
    assert out["license_commercial"] is None


def test_details_auto_kind_falls_through_404(monkeypatch):
    calls = []

    def req(path, params=None):
        calls.append(path)
        if "/api/models/" in path:
            raise hf_mod._HFError(404, "not found")
        return {"id": "x/y", "cardData": {"license": "mit"}, "gated": False,
                "tags": [], "siblings": [], "downloads": 0, "likes": 0}

    monkeypatch.setattr(hf_mod, "_request_json", req)
    out = hf_mod._details("some/repo", kind="auto")
    assert out["kind"] == "dataset"  # fell through models→datasets
    assert calls == ["/api/models/some/repo", "/api/datasets/some/repo"]


def test_details_requires_repo():
    assert hf_mod._details("")["error"]


# ---------------------------------------------------------------------------
# pull() — anonymous download, gated refusal, caps
# ---------------------------------------------------------------------------


def test_pull_downloads_files_and_returns_manifest(monkeypatch, tmp_path):
    details = {
        "id": "atomind/alexandria",
        "cardData": {"license": "cc-by-4.0"},
        "gated": False,
        "tags": [], "downloads": 0, "likes": 0,
        "siblings": [{"rfilename": "README.md"}, {"rfilename": "data.json.bz2"}],
    }
    monkeypatch.setattr(hf_mod, "_request_json", lambda path, params=None: details)
    monkeypatch.setattr(hf_mod, "_download_file", lambda url, dest: {"path": str(dest), "bytes": 42})

    out = hf_mod._pull("atomind/alexandria", kind="dataset", target=str(tmp_path))
    assert out["file_count"] == 2
    assert out["bytes"] == 84
    assert not out["truncated"]
    assert all("path" in f for f in out["files"])


def test_pull_refuses_gated_repo_before_downloading(monkeypatch):
    """DEFINITION-OF-DONE: a gated repo is refused at search/pull time, not
    after someone builds on it. And _download_file must never be called."""
    details = {"id": "facebook/OMAT24", "cardData": {"license": "other"},
               "gated": "manual", "tags": [], "siblings": [{"rfilename": "x"}]}

    def no_download(url, dest):  # would fail the test if reached
        raise AssertionError("must not download a gated repo")

    monkeypatch.setattr(hf_mod, "_request_json", lambda path, params=None: details)
    monkeypatch.setattr(hf_mod, "_download_file", no_download)
    out = hf_mod._pull("facebook/OMAT24", kind="model")
    assert "error" in out
    assert "gated" in out
    assert out["gated"] == "manual"


def test_pull_refuses_anonymous_blocked_repo(monkeypatch):
    def req(path, params=None):
        raise hf_mod._HFError(401, "anonymous access blocked")

    monkeypatch.setattr(hf_mod, "_request_json", req)
    out = hf_mod._pull("ACEtools/mace-mp-0", kind="model")
    assert "error" in out
    assert out["gated"] == "anonymous-blocked"


def test_pull_byte_cap_truncates(monkeypatch, tmp_path):
    details = {"id": "x/y", "cardData": {}, "gated": False, "tags": [],
               "siblings": [{"rfilename": f"f{i}"} for i in range(100)]}
    monkeypatch.setattr(hf_mod, "_request_json", lambda path, params=None: details)
    # Each file is 1 MB; cap at 3 MB → truncated after ~3 files.
    monkeypatch.setattr(hf_mod, "_download_file", lambda url, dest: {"path": str(dest), "bytes": 1024 * 1024})
    out = hf_mod._pull("x/y", kind="model", target=str(tmp_path), max_bytes_mb=3)
    assert out["truncated"] is True
    assert out["file_count"] <= 4


# ---------------------------------------------------------------------------
# dispatcher + registration
# ---------------------------------------------------------------------------


def test_dispatcher_missing_action_returns_help():
    out = hf_mod._hf()
    assert "error" in out and "Valid" in out["error"]
    assert "search" in out["hint"] and "details" in out["hint"] and "pull" in out["hint"]


def test_dispatcher_unknown_action():
    assert "Unknown action" in hf_mod._hf(action="frobnicate")["error"]


def test_dispatcher_routes_actions(monkeypatch):
    seen = {}

    def fake_search(**kw):
        seen["search"] = kw
        return {"ok": True}

    monkeypatch.setattr(hf_mod, "_search", fake_search)
    hf_mod._hf(action="search", query="q", limit=3)
    assert seen["search"]["query"] == "q"
    assert seen["search"]["limit"] == 3


def test_tool_registered_in_full_registry():
    """DEFINITION-OF-DONE: the agent can actually call `hf`. Verifies the tool
    is wired into build_full_registry under the name 'hf'."""
    from app.plugins.bootstrap import build_full_registry

    registry, _providers, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
    tools = {t.name for t in registry.list_tools()}
    assert "hf" in tools, f"'hf' not registered; got {sorted(tools)[:20]}..."

    tool = registry.get("hf")
    assert "search" in tool.description and "pull" in tool.description
    # Smoke-execute through the registry (no network: action missing → help).
    out = tool.execute()
    assert "error" in out
