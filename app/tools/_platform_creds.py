# Copyright (c) 2025-2026 Mirdyne. Licensed under MIT License.
"""Shared platform credential resolution for PRISM agent tools.

Single source of truth: ``~/.prism/credentials.json`` — the one file the Rust
CLI writes through on every login and refresh (see ``crates/runtime``
``save_credentials`` / ``save_cli_state``). Tools must NOT read tokens from
anywhere else.

Auth precedence is defined by PRISM and uses provider-neutral names first:

1. A stable API key (``PRISM_API_KEY`` env, its deprecated
   ``MARC27_API_KEY`` alias, or ``api_key`` in the credentials file) -> sent
   as ``X-API-Key``. It never rotates and isn't
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
import sys
from pathlib import Path
from typing import Optional

_WARNED_ALIASES: set[str] = set()
_MARC27_PROVIDER_API_URL = "https://api.marc27.com/api/v1"


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


def resolve_env_family_with_source(
    *pairs: tuple[str, str]
) -> tuple[Optional[str], Optional[str]]:
    """Resolve a family and retain the exact variable that supplied it."""
    for preferred, _alias in pairs:
        native = os.environ.get(preferred, "").strip()
        if native:
            return native, preferred
    for preferred, alias in pairs:
        legacy = os.environ.get(alias, "").strip()
        if legacy:
            if alias not in _WARNED_ALIASES:
                _WARNED_ALIASES.add(alias)
                print(
                    f"warning: {alias} is deprecated; use {preferred} instead.",
                    file=sys.stderr,
                )
            return legacy, alias
    return None, None


def resolve_env_family(*pairs: tuple[str, str]) -> Optional[str]:
    """Resolve equivalent settings with every PRISM name before every alias.

    Blank values are absent. A used alias emits one process-wide stderr
    notice; a shadowed alias does not, because it did not supply the value.
    """
    return resolve_env_family_with_source(*pairs)[0]


def resolve_env_alias(preferred: str, alias: str) -> Optional[str]:
    """Resolve one PRISM setting and its deprecated provider alias."""
    return resolve_env_family((preferred, alias))


def _selected_env_credential() -> tuple[Optional[str], Optional[str]]:
    return resolve_env_family_with_source(
        ("PRISM_API_KEY", "MARC27_API_KEY"),
        ("PRISM_TOKEN", "MARC27_TOKEN"),
        ("PRISM_API_TOKEN", "MARC27_API_TOKEN"),
    )


def _resolve_api_url(
    creds: dict, selected_credential_source: Optional[str] = None
) -> Optional[str]:
    value, _source = resolve_env_family_with_source(
        ("PRISM_API_URL", "MARC27_API_URL"),
        ("PRISM_PLATFORM_URL", "MARC27_PLATFORM_URL"),
    )
    if value:
        return _normalise_url(value)
    stored = str(creds.get("platform_url") or "").strip()
    if stored:
        return _normalise_url(stored)

    provider = resolve_env_alias(
        "PRISM_PLATFORM_PROVIDER", "MARC27_PLATFORM_PROVIDER"
    )
    if not provider and "platform_provider" in creds:
        provider = str(creds.get("platform_provider") or "").strip()

    # A selected historical credential is explicit provider choice, not an
    # implicit default. Old Python credential files predate provider metadata;
    # those files could only have been issued by MARC27, so grandfather them.
    old_provider_file = "platform_provider" not in creds and bool(
        creds.get("api_key") or creds.get("access_token")
    )
    if (
        str(provider or "").strip().lower() == "marc27"
        or str(selected_credential_source or "").startswith("MARC27_")
        or old_provider_file
    ):
        return _MARC27_PROVIDER_API_URL
    return None


def resolve_platform_auth() -> tuple[Optional[str], dict]:
    """Return ``(api_url, headers)``.

    ``headers`` is empty (``{}``) when not authenticated — callers should treat
    that as "not connected" and tell the user to run ``prism login``.
    """
    creds = _load_creds()
    env_credential, source = _selected_env_credential()
    api_url = _resolve_api_url(creds, source)

    if env_credential:
        if source in {"PRISM_API_KEY", "MARC27_API_KEY"} or env_credential.startswith(
            "m27_"
        ):
            return api_url, {"X-API-Key": env_credential}
        return api_url, {"Authorization": f"Bearer {env_credential}"}

    api_key = creds.get("api_key")
    if api_key:
        return api_url, {"X-API-Key": api_key}

    token = creds.get("access_token", "")
    if token:
        return api_url, {"Authorization": f"Bearer {token}"}
    return api_url, {}


def resolve_credentials() -> tuple[Optional[str], str]:
    """Legacy shape: ``(api_url, token)`` where ``token`` is the raw key/JWT
    (prefers the stable provider API key). Pair with :func:`header_for` so the
    token is sent with the right header.

    Prefer :func:`resolve_platform_auth` for new code — it returns headers.
    """
    creds = _load_creds()
    env_credential, source = _selected_env_credential()
    api_url = _resolve_api_url(creds, source)
    token = (
        env_credential
        or creds.get("api_key")
        or creds.get("access_token", "")
    )
    return api_url, token


def header_for(token: str) -> dict:
    """Choose the wire header for the legacy raw-token API.

    The frozen ``m27_*`` shape remains an API key. A provider-neutral key is
    recognized by its configured source instead of imposing MARC27's prefix on
    every provider; all other values retain the historical Bearer behavior.
    """
    creds = _load_creds()
    env_credential, source = _selected_env_credential()
    configured_api_key = (
        env_credential
        if source in {"PRISM_API_KEY", "MARC27_API_KEY"}
        else None
    ) or creds.get("api_key")
    if token.startswith("m27_") or (configured_api_key and token == configured_api_key):
        return {"X-API-Key": token}
    return {"Authorization": f"Bearer {token}"}
