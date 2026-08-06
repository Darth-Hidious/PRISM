"""Focused tests for the prior-art search adapter's honesty reporting.

F5 regression: a fully-cached zero-hit success renders "cache (0 results)"
in source_status. The old fault check asked whether every rendered string
startswith("ok") — so the cached success was reported as "every source
failed". The honesty check itself must not lie.

These tests fake the subprocess boundary (spawn.run); no binary runs and no
network is touched.
"""

import json
import subprocess

import app.tools.search as search


def _fake_run(outcome: dict, returncode: int = 0):
    def run(argv, **kwargs):
        return subprocess.CompletedProcess(
            args=argv,
            returncode=returncode,
            stdout=json.dumps(outcome),
            stderr="",
        )

    return run


def _patched(monkeypatch, outcome: dict, returncode: int = 0):
    monkeypatch.setattr(search, "_resolve_prism_binary", lambda: "/usr/bin/true")
    monkeypatch.setattr(search.spawn, "run", _fake_run(outcome, returncode))


def test_cached_zero_hit_success_is_not_reported_as_all_sources_failed(monkeypatch):
    _patched(
        monkeypatch,
        {
            "papers": [],
            "source_status": [
                {
                    "source": "arxiv",
                    "status": "ok",
                    "count": 0,
                    "cache_hit": True,
                    "error": None,
                }
            ],
        },
    )
    out = search._literature_search_impl(query="tungsten rhenium")
    assert out["count"] == 0
    assert out["source_status"] == {"arxiv": "cache (0 results)"}
    # The old check saw "cache (0 results)", concluded every source failed,
    # and attached this error to an honest zero-hit success.
    assert "error" not in out


def test_zero_hit_success_from_the_network_is_not_a_fault_either(monkeypatch):
    _patched(
        monkeypatch,
        {
            "papers": [],
            "source_status": [
                {
                    "source": "arxiv",
                    "status": "ok",
                    "count": 0,
                    "cache_hit": False,
                    "error": None,
                }
            ],
        },
    )
    out = search._literature_search_impl(query="nothing published here")
    assert "error" not in out


def test_genuine_all_sources_failed_is_still_reported(monkeypatch):
    _patched(
        monkeypatch,
        {
            "papers": [],
            "source_status": [
                {
                    "source": "arxiv",
                    "status": "error",
                    "count": 0,
                    "cache_hit": False,
                    "error": "HTTP 500",
                },
                {
                    "source": "semantic_scholar",
                    "status": "timeout",
                    "count": 0,
                    "cache_hit": False,
                    "error": "exceeded per-source timeout of 30s",
                },
            ],
        },
    )
    out = search._literature_search_impl(query="high entropy alloy")
    assert out["count"] == 0
    assert "every source failed" in out["error"]
    assert out["source_status"]["arxiv"].startswith("error")
    assert out["source_status"]["semantic_scholar"].startswith("timeout")


def test_one_ok_source_among_failures_is_not_a_total_fault(monkeypatch):
    _patched(
        monkeypatch,
        {
            "papers": [],
            "source_status": [
                {
                    "source": "arxiv",
                    "status": "ok",
                    "count": 0,
                    "cache_hit": True,
                    "error": None,
                },
                {
                    "source": "crossref",
                    "status": "error",
                    "count": 0,
                    "cache_hit": False,
                    "error": "HTTP 503",
                },
            ],
        },
    )
    out = search._literature_search_impl(query="high entropy alloy")
    assert "error" not in out
