"""Server-side auth and environment loading.

HF_TOKEN must NEVER cross the MCP wire. It is loaded only here from one of:

  1. The OS environment (highest priority — useful for tests / CI).
  2. ``$MACE_MCP_ENV_FILE`` (if set).
  3. ``~/.config/mace-mcp/.env`` (default).

The MCP client cannot read or send the token; it is consumed only by the
``HfJobsBackend`` and the dataset-push step of the provenance writer.

All token-bearing strings pass through :func:`scrub_token` before they ever
hit a log or a provenance JSON file.
"""

from __future__ import annotations

import os
from pathlib import Path

from dotenv import dotenv_values

DEFAULT_ENV_PATH = Path("~/.config/mace-mcp/.env").expanduser()


class AuthError(RuntimeError):
    pass


def _env_path() -> Path:
    custom = os.environ.get("MACE_MCP_ENV_FILE")
    return Path(custom).expanduser() if custom else DEFAULT_ENV_PATH


_cache: dict[str, str] | None = None


def load_env(force_reload: bool = False) -> dict[str, str]:
    """Read and cache env variables from disk + OS env. Never returns None."""
    global _cache
    if _cache is not None and not force_reload:
        return _cache
    merged: dict[str, str] = {}
    path = _env_path()
    if path.exists():
        merged.update({k: v for k, v in dotenv_values(path).items() if v is not None})
    # OS env wins
    for k in (
        "HF_TOKEN",
        "MACE_MCP_RESULTS_REPO",
        "MACE_MCP_BACKEND",
        "MACE_MCP_CACHE_DIR",
        "MACE_MCP_STATE_DIR",
        "MACE_MCP_LIVE",
    ):
        if k in os.environ:
            merged[k] = os.environ[k]
    _cache = merged
    return _cache


def get_hf_token() -> str:
    """Return the HF_TOKEN. Raises :class:`AuthError` if absent."""
    env = load_env()
    tok = env.get("HF_TOKEN")
    if not tok:
        raise AuthError(
            "HF_TOKEN not set. Put it in ~/.config/mace-mcp/.env or the OS env."
        )
    return tok


def get_results_repo() -> str | None:
    """Return the HF Dataset repo id for provenance push (None disables push)."""
    return load_env().get("MACE_MCP_RESULTS_REPO")


def get_state_dir() -> Path:
    """Return ~/.local/state/mace-mcp/ (or MACE_MCP_STATE_DIR override)."""
    custom = load_env().get("MACE_MCP_STATE_DIR")
    if custom:
        p = Path(custom).expanduser()
    else:
        p = Path("~/.local/state/mace-mcp").expanduser()
    p.mkdir(parents=True, exist_ok=True)
    return p


def get_cache_dir() -> Path:
    """Return the content-addressed cache root."""
    custom = load_env().get("MACE_MCP_CACHE_DIR")
    if custom:
        p = Path(custom).expanduser()
    else:
        p = get_state_dir() / "cache"
    p.mkdir(parents=True, exist_ok=True)
    return p


def get_backend_override() -> str | None:
    """Force a specific backend irrespective of tool/N_atoms heuristic.

    Values: ``"fake"`` | ``"local"`` | ``"hf_jobs"``. None means use the
    heuristic.
    """
    return load_env().get("MACE_MCP_BACKEND")


def scrub_token(text: str) -> str:
    """Replace the live HF_TOKEN (if any) with a placeholder in ``text``."""
    env = load_env()
    tok = env.get("HF_TOKEN")
    if tok and tok in text:
        text = text.replace(tok, "***HF_TOKEN***")
    return text


def reset_cache_for_tests() -> None:
    """Force-reload on next access. Use in test fixtures only."""
    global _cache
    _cache = None
