"""Tests for the version update checker."""

import json
import tarfile
import time
from pathlib import Path
from unittest.mock import patch, MagicMock

import pytest

from app.update import check_for_updates, _read_cache, _write_cache, CACHE_PATH, download_tui_binary


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
    with patch("app.update._check_pypi", return_value="2.1.0"), \
         patch("app.update.detect_install_method", return_value="pip"):
        result = check_for_updates("2.0.0")
    assert result is not None
    assert result["latest"] == "2.1.0"
    assert result["current"] == "2.0.0"
    assert "upgrade" in result["upgrade_cmd"]


def test_pypi_failure_falls_back_to_github(clean_cache):
    """When PyPI is unreachable, GitHub releases are tried."""
    with patch("app.update._check_pypi", side_effect=Exception("timeout")), \
         patch("app.update._check_github", return_value="2.2.0"), \
         patch("app.update.detect_install_method", return_value="pip"):
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
    with patch("app.update._check_pypi", return_value="2.1.0"), \
         patch("app.update.detect_install_method", return_value="pip"):
        first = check_for_updates("2.0.0")
    assert first is not None

    # Second call should use cache — mock should NOT be called
    with patch("app.update._check_pypi", side_effect=AssertionError("should not be called")), \
         patch("app.update._check_github", side_effect=AssertionError("should not be called")), \
         patch("app.update.detect_install_method", return_value="pip"):
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


def test_download_tui_binary_prefers_direct_release_asset(clean_cache, monkeypatch):
    """When a standalone prism-tui asset exists, download it directly."""
    monkeypatch.setattr("app.update._tui_binary_name", lambda: "prism-tui-darwin-arm64")
    monkeypatch.setattr("app.update._platform_archive_name", lambda: "prism-macos-aarch64.tar.gz")
    monkeypatch.setattr(
        "app.update._latest_release_metadata",
        lambda: {
            "tag_name": "v9.9.9",
            "assets": [
                {
                    "name": "prism-tui-darwin-arm64",
                    "browser_download_url": "https://example.test/prism-tui-darwin-arm64",
                }
            ],
        },
    )

    def fake_urlretrieve(url, dest):
        Path(dest).write_bytes(b"direct-binary")
        return dest, None

    monkeypatch.setattr("urllib.request.urlretrieve", fake_urlretrieve)

    path = download_tui_binary()
    assert path is not None
    assert Path(path).read_bytes() == b"direct-binary"


def test_download_tui_binary_falls_back_to_archive(clean_cache, monkeypatch):
    """If the standalone asset is absent, extract prism-tui from the platform archive."""
    monkeypatch.setattr("app.update._tui_binary_name", lambda: "prism-tui-linux-x64")
    monkeypatch.setattr("app.update._platform_archive_name", lambda: "prism-linux-x86_64.tar.gz")
    monkeypatch.setattr(
        "app.update._latest_release_metadata",
        lambda: {
            "tag_name": "v9.9.9",
            "assets": [
                {
                    "name": "prism-linux-x86_64.tar.gz",
                    "browser_download_url": "https://example.test/prism-linux-x86_64.tar.gz",
                }
            ],
        },
    )

    def fake_urlretrieve(url, dest):
        if url.endswith(".tar.gz"):
            with tarfile.open(dest, "w:gz") as archive:
                payload = clean_cache.parent / "prism-tui"
                payload.write_bytes(b"archive-binary")
                archive.add(payload, arcname="prism-tui")
        else:
            raise RuntimeError("direct asset should not be used in archive fallback test")
        return dest, None

    monkeypatch.setattr("urllib.request.urlretrieve", fake_urlretrieve)

    path = download_tui_binary()
    assert path is not None
    assert Path(path).read_bytes() == b"archive-binary"
