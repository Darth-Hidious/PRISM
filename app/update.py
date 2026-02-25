"""Version update checker for PRISM."""

import json
import time
from pathlib import Path
from typing import Optional

from packaging.version import Version

PRISM_DIR = Path.home() / ".prism"
CACHE_PATH = PRISM_DIR / ".update_check"
CACHE_TTL = 86400  # 24 hours

PYPI_URL = "https://pypi.org/pypi/prism-platform/json"
GITHUB_URL = "https://api.github.com/repos/Darth-Hidious/PRISM/releases/latest"


def _read_cache() -> Optional[dict]:
    """Read cached version check result if still valid."""
    try:
        if not CACHE_PATH.exists():
            return None
        data = json.loads(CACHE_PATH.read_text())
        if time.time() - data.get("checked_at", 0) < CACHE_TTL:
            return data
    except Exception:
        pass
    return None


def _write_cache(latest_version: str) -> None:
    """Write version check result to cache."""
    try:
        PRISM_DIR.mkdir(parents=True, exist_ok=True)
        CACHE_PATH.write_text(json.dumps({
            "checked_at": time.time(),
            "latest_version": latest_version,
        }))
    except Exception:
        pass


def _check_pypi() -> Optional[str]:
    """Query PyPI for the latest version."""
    import urllib.request

    req = urllib.request.Request(PYPI_URL, headers={"Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as resp:
        data = json.loads(resp.read())
    return data.get("info", {}).get("version")


def _check_github() -> Optional[str]:
    """Query GitHub releases for the latest version."""
    import urllib.request

    req = urllib.request.Request(GITHUB_URL, headers={"Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req, timeout=5) as resp:
        data = json.loads(resp.read())
    tag = data.get("tag_name", "")
    return tag.lstrip("v") if tag else None


def check_for_updates(current_version: str) -> Optional[dict]:
    """Check if a newer version of PRISM is available.

    Returns a dict with update info if outdated, or None if up-to-date.
    Never raises — all errors return None.
    """
    try:
        # Check cache first
        cached = _read_cache()
        if cached:
            latest = cached["latest_version"]
        else:
            # Try PyPI first, fall back to GitHub
            latest = None
            try:
                latest = _check_pypi()
            except Exception:
                pass
            if not latest:
                try:
                    latest = _check_github()
                except Exception:
                    pass
            if not latest:
                return None
            _write_cache(latest)

        if Version(latest) <= Version(current_version):
            return None

        return {
            "latest": latest,
            "current": current_version,
            "upgrade_cmd": "curl -fsSL https://prism.marc27.com/install.sh | bash -s -- --upgrade",
        }
    except Exception:
        return None
