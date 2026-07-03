# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""Shared platform credential resolution for PRISM agent tools.

Single source of truth: ``~/.prism/credentials.json`` — the one file the Rust
CLI writes through on every login and refresh (see ``crates/runtime``
``save_credentials`` / ``save_cli_state``). Tools must NOT read tokens from
anywhere else.

Auth precedence mirrors the marc27-core server, which checks ``X-API-Key``
before ``Authorization``:

1. A stable ``m27_*`` API key (``MARC27_API_KEY`` env, or ``api_key`` in the
   credentials file) -> sent as ``X-API-Key``. It never rotates and isn't
   single-use, so a long-running research cycle can't have its credential
   reset out from under it; it is also project-scoped, so it carries the org
   tenant that the user JWT does not.
2. The rotating user JWT (``access_token``) -> ``Bearer`` (legacy fallback).

Use :func:`resolve_platform_auth`; the legacy ``(api_url, token)`` shape is
available via :func:`resolve_credentials` for callers not yet migrated.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

_DEFAULT_API_URL = "https://api.marc27.com/api/v1"


def _load_creds() -> dict:
    try:
        path = Path.home() / ".prism" / "credentials.json"
        return json.loads(path.read_text()) if path.exists() else {}
    except Exception:
        return {}


def _normalise_url(url: str) -> str:
    url = url.rstrip("/")
    if not url.endswith("/api/v1"):
        url = url + "/api/v1"
    return url


def resolve_platform_auth() -> tuple[str, dict]:
    """Return ``(api_url, headers)``.

    ``headers`` is empty (``{}``) when not authenticated — callers should treat
    that as "not connected" and tell the user to run ``prism login``.
    """
    creds = _load_creds()
    api_url = (
        os.environ.get("MARC27_API_URL") or creds.get("platform_url") or _DEFAULT_API_URL
    )
    api_url = _normalise_url(api_url)

    api_key = os.environ.get("MARC27_API_KEY") or creds.get("api_key")
    if api_key:
        return api_url, {"X-API-Key": api_key}

    token = creds.get("access_token", "")
    if token:
        return api_url, {"Authorization": f"Bearer {token}"}
    return api_url, {}


def resolve_credentials() -> tuple[str, str]:
    """Legacy shape: ``(api_url, token)`` where ``token`` is the raw key/JWT
    (prefers the stable ``m27_*`` key). Pair with :func:`header_for` so the
    token is sent with the right header.

    Prefer :func:`resolve_platform_auth` for new code — it returns headers.
    """
    creds = _load_creds()
    api_url = (
        os.environ.get("MARC27_API_URL") or creds.get("platform_url") or _DEFAULT_API_URL
    )
    api_url = _normalise_url(api_url)
    token = (
        os.environ.get("MARC27_API_KEY")
        or creds.get("api_key")
        or creds.get("access_token", "")
    )
    return api_url, token


def header_for(token: str) -> dict:
    """Auth header for a raw token: ``m27_*`` keys go in ``X-API-Key`` (the
    server checks it first and it carries the org tenant), everything else is a
    ``Bearer`` JWT."""
    if token.startswith("m27_"):
        return {"X-API-Key": token}
    return {"Authorization": f"Bearer {token}"}
