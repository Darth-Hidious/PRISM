"""Version update checker for PRISM."""

import json
import shutil
import time
from pathlib import Path
from typing import Optional

from packaging.version import Version

PRISM_DIR = Path.home() / ".prism"
CACHE_PATH = PRISM_DIR / ".update_check"
_DEFAULT_CACHE_TTL = 86400  # 24 hours


def _get_cache_ttl() -> int:
    """Get cache TTL from settings, falling back to 24 hours."""
    try:
        from app.config.settings_schema import get_settings
        return get_settings().updates.cache_ttl_hours * 3600
    except Exception:
        return _DEFAULT_CACHE_TTL

PYPI_URL = "https://pypi.org/pypi/prism-platform/json"
GITHUB_URL = "https://api.github.com/repos/Darth-Hidious/PRISM/releases/latest"


def _read_cache() -> Optional[dict]:
    """Read cached version check result if still valid."""
    try:
        if not CACHE_PATH.exists():
            return None
        data = json.loads(CACHE_PATH.read_text())
        if time.time() - data.get("checked_at", 0) < _get_cache_ttl():
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


def detect_install_method() -> str:
    """Detect how PRISM was installed. Returns: uv, pipx, pip, or unknown."""
    # Check uv
    if shutil.which("uv"):
        try:
            import subprocess
            result = subprocess.run(
                ["uv", "tool", "list"],
                capture_output=True, text=True, timeout=5,
            )
            if "prism-platform" in result.stdout:
                return "uv"
        except Exception:
            pass

    # Check pipx
    if shutil.which("pipx"):
        try:
            import subprocess
            result = subprocess.run(
                ["pipx", "list", "--short"],
                capture_output=True, text=True, timeout=5,
            )
            if "prism-platform" in result.stdout:
                return "pipx"
        except Exception:
            pass

    # Fallback: pip
    try:
        import importlib.metadata
        importlib.metadata.distribution("prism-platform")
        return "pip"
    except Exception:
        pass

    return "unknown"


def upgrade_command(method: Optional[str] = None) -> str:
    """Return the appropriate upgrade command for the installation method."""
    method = method or detect_install_method()
    if method == "uv":
        return "uv tool upgrade prism-platform"
    elif method == "pipx":
        return "pipx upgrade prism-platform"
    elif method == "pip":
        return "pip install --upgrade prism-platform"
    else:
        return "curl -fsSL https://prism.marc27.com/install.sh | bash -s -- --upgrade"


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

        method = detect_install_method()
        return {
            "latest": latest,
            "current": current_version,
            "install_method": method,
            "upgrade_cmd": upgrade_command(method),
        }
    except Exception:
        return None
