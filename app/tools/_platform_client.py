# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""THE MARC27 platform HTTP client for PRISM agent tools.

Every tool that talks to api.marc27.com goes through this module — no tool
hand-rolls ``requests`` + auth headers. That rule exists because the hand-
rolled copies kept getting auth wrong (``m27_*`` API keys were sent as
``Bearer``, which the server rejects; ``X-API-Key`` is required — see
``_platform_creds``).

Agent-friendly contract: methods NEVER raise. Failures come back as
``{"error": ..., "path": ...}`` dicts, which the tool layer can hand straight
to the model. A 401 triggers ONE credential re-read + retry, because the Rust
CLI refreshes ``~/.prism/credentials.json`` behind our back.

Usage::

    from app.tools._platform_client import platform

    result = platform().get("/knowledge/graph/stats")
    if "error" in result:
        return result  # agent-readable as-is
"""
from __future__ import annotations

import os
from typing import Any, Optional

import requests

from app.tools._platform_creds import resolve_platform_auth

_TIMEOUT_SECS = 30


class PlatformClient:
    """Thin, stateless-ish wrapper: auth + base URL + uniform errors."""

    def __init__(self) -> None:
        self._api_url, self._headers = resolve_platform_auth()
        self._session = requests.Session()

    @property
    def authenticated(self) -> bool:
        return bool(self._headers)

    @property
    def api_url(self) -> str:
        return self._api_url

    def request(
        self,
        method: str,
        path: str,
        *,
        params: Optional[dict] = None,
        json: Optional[dict] = None,
        timeout: float = _TIMEOUT_SECS,
    ) -> dict[str, Any]:
        if os.environ.get("PRISM_OFFLINE") == "1":
            # `prism --offline` — the CLI exports PRISM_OFFLINE to this
            # process so platform tools degrade cleanly instead of hitting
            # the network behind the user's back.
            return {
                "error": "offline mode: platform call blocked by --offline",
                "path": path,
            }
        if not self._headers:
            return {
                "error": "not connected to the MARC27 platform — run `prism login`",
                "path": path,
            }
        resp = self._send(method, path, params, json, timeout)
        if isinstance(resp, dict):  # transport error already shaped
            return resp
        if resp.status_code == 401:
            # The CLI may have rotated the token since we read the file.
            self._api_url, self._headers = resolve_platform_auth()
            resp = self._send(method, path, params, json, timeout)
            if isinstance(resp, dict):
                return resp
        if resp.status_code >= 400:
            return {
                "error": f"platform returned HTTP {resp.status_code}",
                "detail": resp.text[:500],
                "path": path,
            }
        try:
            return resp.json()
        except ValueError:
            return {
                "error": "platform returned non-JSON",
                "detail": resp.text[:200],
                "path": path,
            }

    def _send(self, method, path, params, json, timeout):
        try:
            return self._session.request(
                method,
                f"{self._api_url}{path}",
                headers=self._headers,
                params=params,
                json=json,
                timeout=timeout,
            )
        except requests.RequestException as exc:
            return {"error": f"platform request failed: {exc}", "path": path}

    def get(self, path: str, params: Optional[dict] = None, **kw) -> dict[str, Any]:
        return self.request("GET", path, params=params, **kw)

    def post(self, path: str, json: Optional[dict] = None, **kw) -> dict[str, Any]:
        return self.request("POST", path, json=json, **kw)

    def put(self, path: str, json: Optional[dict] = None, **kw) -> dict[str, Any]:
        return self.request("PUT", path, json=json, **kw)

    def delete(self, path: str, **kw) -> dict[str, Any]:
        return self.request("DELETE", path, **kw)


_CLIENT: Optional[PlatformClient] = None


def platform() -> PlatformClient:
    """Module-level client. One Session, shared by all tools in a process."""
    global _CLIENT
    if _CLIENT is None:
        _CLIENT = PlatformClient()
    return _CLIENT
