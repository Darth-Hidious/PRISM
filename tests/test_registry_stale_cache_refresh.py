"""A stale OPTIMADE registry cache must actually be re-discovered.

Regression for a defect measured 2026-08-24: `build_registry` computed
`is_cache_fresh`, found the cache stale, wrote one `logger.debug` that
nothing reads, and scheduled the refresh its own comment promised in no way
at all. The cache on that machine was 33 days old against a 7-day window,
and 4 of 5 spot-checked endpoints were dead — the federation had moved while
PRISM held a frozen list.

These tests never touch the network: discovery is substituted, and what is
asserted is that it gets CALLED and that its result is persisted.
"""

import json
import time

import pytest

from app.tools.search_engine.providers import registry as registry_mod


@pytest.fixture
def cache_file(tmp_path):
    return tmp_path / "discovered_registry.json"


def _write_cache(path, endpoints, age_days):
    path.write_text(
        json.dumps(
            {
                "version": "2.0.0",
                "cached_at": time.time() - age_days * 86400,
                "endpoints": endpoints,
            }
        )
    )


def _endpoint(pid, url):
    return {"id": pid, "name": pid, "base_url": url, "enabled": True}


def test_stale_cache_triggers_rediscovery_and_is_persisted(cache_file, monkeypatch):
    _write_cache(cache_file, [_endpoint("old", "https://old.example/optimade")], age_days=33)

    fresh = [_endpoint("new", "https://new.example/optimade")]
    called = {"n": 0}

    async def fake_discover(*_a, **_k):
        called["n"] += 1
        return fresh

    monkeypatch.setattr(
        "app.tools.search_engine.providers.discovery.discover_providers", fake_discover
    )

    registry_mod.build_registry(cache_path=cache_file)

    assert called["n"] == 1, "a stale cache must re-discover, not just log about it"
    written = json.loads(cache_file.read_text())
    assert [e["id"] for e in written["endpoints"]] == ["new"]
    assert (time.time() - written["cached_at"]) < 60, "cache must be re-stamped"


def test_fresh_cache_does_not_hit_the_network(cache_file, monkeypatch):
    _write_cache(cache_file, [_endpoint("kept", "https://kept.example/optimade")], age_days=1)

    async def fail_discover(*_a, **_k):
        raise AssertionError("a fresh cache must not re-discover")

    monkeypatch.setattr(
        "app.tools.search_engine.providers.discovery.discover_providers", fail_discover
    )
    registry_mod.build_registry(cache_path=cache_file)
    assert [e["id"] for e in json.loads(cache_file.read_text())["endpoints"]] == ["kept"]


def test_failed_rediscovery_keeps_the_stale_list_rather_than_returning_nothing(
    cache_file, monkeypatch, caplog
):
    """Degrade, but say so. A dead federation must not silently become zero
    providers, and must not pass as a healthy result either."""
    _write_cache(cache_file, [_endpoint("old", "https://old.example/optimade")], age_days=99)

    async def broken_discover(*_a, **_k):
        raise RuntimeError("federation unreachable")

    monkeypatch.setattr(
        "app.tools.search_engine.providers.discovery.discover_providers", broken_discover
    )

    with caplog.at_level("WARNING"):
        registry_mod.build_registry(cache_path=cache_file)

    assert [e["id"] for e in json.loads(cache_file.read_text())["endpoints"]] == ["old"]
    assert any("STALE" in r.message or "stale" in r.message.lower() for r in caplog.records), (
        "falling back to a stale list must be reported, not absorbed"
    )
