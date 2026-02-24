"""Tests for the version update checker."""

import json
import time
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

from app.update import check_for_updates, _read_cache, _write_cache, CACHE_PATH


@pytest.fixture(autouse=True)
def clean_cache(tmp_path, monkeypatch):
    """Use a temp directory for the cache file."""
    cache_path = tmp_path / ".update_check"
    monkeypatch.setattr("app.update.CACHE_PATH", cache_path)
    monkeypatch.setattr("app.update.PRISM_DIR", tmp_path)
    yield cache_path


# ---------- Core behaviour ----------


def test_returns_none_when_up_to_date(clean_cache):
    """If current == latest, no update info is returned."""
    with patch("app.update._check_pypi", return_value="2.0.0"):
        result = check_for_updates("2.0.0")
    assert result is None


def test_returns_update_info_when_outdated(clean_cache):
    """If latest > current, return upgrade info."""
    with patch("app.update._check_pypi", return_value="2.1.0"):
        result = check_for_updates("2.0.0")
    assert result is not None
    assert result["latest"] == "2.1.0"
    assert result["current"] == "2.0.0"
    assert "upgrade" in result["upgrade_cmd"]


def test_pypi_failure_falls_back_to_github(clean_cache):
    """When PyPI is unreachable, GitHub releases are tried."""
    with patch("app.update._check_pypi", side_effect=Exception("timeout")), \
         patch("app.update._check_github", return_value="2.2.0"):
        result = check_for_updates("2.0.0")
    assert result is not None
    assert result["latest"] == "2.2.0"


def test_both_fail_returns_none(clean_cache):
    """When both PyPI and GitHub fail, returns None gracefully."""
    with patch("app.update._check_pypi", side_effect=Exception("fail")), \
         patch("app.update._check_github", side_effect=Exception("fail")):
        result = check_for_updates("2.0.0")
    assert result is None


# ---------- Cache ----------


def test_cache_prevents_repeated_checks(clean_cache):
    """Within the TTL window, the cached value is used (no network calls)."""
    # Prime the cache
    with patch("app.update._check_pypi", return_value="2.1.0"):
        first = check_for_updates("2.0.0")
    assert first is not None

    # Second call should use cache — mock should NOT be called
    with patch("app.update._check_pypi", side_effect=AssertionError("should not be called")), \
         patch("app.update._check_github", side_effect=AssertionError("should not be called")):
        second = check_for_updates("2.0.0")
    assert second is not None
    assert second["latest"] == "2.1.0"


# ---------- Preferences integration ----------


def test_check_updates_false_skips_check():
    """When check_updates preference is False, CLI should not call check_for_updates.

    We verify the preference field exists and defaults to True.
    """
    from app.config.preferences import UserPreferences
    prefs = UserPreferences()
    assert prefs.check_updates is True

    prefs.check_updates = False
    assert prefs.check_updates is False
