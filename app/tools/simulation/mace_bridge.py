"""Singleton bridge that wires the MACE primitive subpackage into PRISM tools.

Mirrors the pattern of `app/tools/simulation/calphad_bridge.py`: a thin layer
that holds the lazily-initialised `JobRunner`, backend dict, and cache store
so every Tool wrapper in `app/tools/mace.py` sees the same state.

Framework note: MACE-MH-1 is PyTorch-only as of 2026 (mace-jax doesn't yet
support the multi-head MH-1 architecture). The local backend therefore loads
the model via `mace.calculators.mace_mp` (mace-torch). The platform backend
submits `job_type=ml_predict` with `model=mace-mh-1` (or `mace-mp-0`) to the
marc27 platform compute path. Fake backend is for tests — no torch required.
"""

from __future__ import annotations

import logging
import os
from typing import TYPE_CHECKING, Any

logger = logging.getLogger(__name__)

if TYPE_CHECKING:  # pragma: no cover
    from app.tools.simulation.mace.backends.base import Backend
    from app.tools.simulation.mace.cache.store import CacheStore
    from app.tools.simulation.mace.jobs.runner import JobRunner


# ---------------------------------------------------------------------------
# Availability check (used by _guard in app/tools/mace.py)
# ---------------------------------------------------------------------------

def check_mace_available() -> bool:
    """Return True iff the mace subpackage's hard dependencies are importable.

    The bare-minimum surface is:
      * `mace.calculators` — model weights loading (mace-torch)
      * `ase`              — structure I/O + ASE calculator interface
      * `ulid`             — deterministic job IDs

    Each is gated by its own try/except so the error message can point the
    user at the right missing piece if they only installed a subset.
    """
    missing: list[str] = []
    try:
        import mace.calculators  # noqa: F401
    except ImportError:
        missing.append("mace-torch")
    try:
        import ase  # noqa: F401
    except ImportError:
        missing.append("ase")
    try:
        import ulid  # noqa: F401
    except ImportError:
        missing.append("python-ulid")
    if missing:
        logger.debug("mace bridge unavailable; missing: %s", missing)
        return False
    return True


def _mace_missing_error() -> dict[str, Any]:
    """Stable, agent-readable error dict for use when MACE isn't installed."""
    return {
        "error": "MACE foundation interatomic potential not available in this PRISM install.",
        "install_hint": "pip install 'prism-platform[mace]'",
        "rationale": (
            "MACE-MH-1 is PyTorch-only as of 2026 (mace-jax does not yet support "
            "the multi-head MH-1 architecture). Installing the `[mace]` extra adds "
            "mace-torch + torch + ase + python-ulid. PRISM's broader stack remains "
            "JAX-native; MACE is an explicit PyTorch holdout pinned by upstream."
        ),
    }


# ---------------------------------------------------------------------------
# Singleton bridge
# ---------------------------------------------------------------------------

class MaceBridge:
    """Lazy wiring of JobRunner + backends + cache store.

    Constructing this attempts no heavy imports beyond what's needed for
    runtime dispatch. The actual MACE model is only loaded when a primitive
    runs against the LocalBackend (and the platform / hf_jobs / fake backends
    don't need MACE in-process at all).
    """

    def __init__(
        self,
        *,
        cache_dir: str | None = None,
        sqlite_path: str | None = None,
        results_repo: str | None = None,
    ) -> None:
        from app.tools.simulation.mace.auth import (
            get_cache_dir as _default_cache_dir,
        )
        from app.tools.simulation.mace.backends.fake import FakeBackend
        from app.tools.simulation.mace.backends.local import LocalBackend
        from app.tools.simulation.mace.backends.platform import PlatformBackend
        from app.tools.simulation.mace.cache.store import CacheStore
        from app.tools.simulation.mace.jobs.runner import JobRunner
        from app.tools.simulation.mace.jobs.store import JobStore

        resolved_cache_dir = cache_dir or _default_cache_dir()
        sqlite_path = sqlite_path or os.path.join(resolved_cache_dir, "mace_jobs.db")

        self.cache: "CacheStore" = CacheStore(cache_dir=resolved_cache_dir)
        self.job_store = JobStore(db_path=sqlite_path)

        # Build the default backend set.
        #
        # Always-on: fake (tests), local (laptop / dev), platform
        # (marc27 ml_predict). Platform is registered unconditionally —
        # the runtime check inside `PlatformBackend.execute` raises a
        # clear error if `PRISM_PROJECT_ID` isn't set, so the agent
        # picking `platform` without auth gets an actionable error
        # instead of a silent fallback.
        #
        # Optional: hf_jobs (only if HF_TOKEN is in env, to avoid the
        # huggingface_hub transitive import for users without one).
        backends: dict[str, "Backend"] = {
            "fake": FakeBackend(),
            "local": LocalBackend(),
            "platform": PlatformBackend(),
        }

        # Lazy-import hf_jobs so users without HF_TOKEN don't pay for the
        # transitive imports (huggingface_hub etc).
        if os.environ.get("HF_TOKEN"):
            try:
                from app.tools.simulation.mace.backends.hf_jobs import HfJobsBackend
                backends["hf_jobs"] = HfJobsBackend()
            except Exception:  # noqa: BLE001 — broad on purpose: never break on optional backend
                logger.exception("hf_jobs backend failed to initialise; continuing without it")

        self.backends: dict[str, "Backend"] = backends

        self.runner: "JobRunner" = JobRunner(
            cache=self.cache,
            job_store=self.job_store,
            backends=self.backends,
            results_repo=results_repo,
        )

        logger.info(
            "MACE bridge initialised: backends=%s, cache_dir=%s",
            sorted(self.backends.keys()),
            resolved_cache_dir,
        )


_BRIDGE: MaceBridge | None = None


def get_mace_bridge() -> MaceBridge:
    """Return the process-wide MaceBridge, constructing it on first use.

    Singleton because JobRunner owns a background thread pool + sqlite handle;
    re-instantiating per tool call would leak resources and corrupt the
    cache database on concurrent writes.
    """
    global _BRIDGE
    if _BRIDGE is None:
        _BRIDGE = MaceBridge()
    return _BRIDGE


def reset_mace_bridge() -> None:
    """Test-only: drop the cached bridge so the next get_mace_bridge rebuilds.

    Used by `tests/test_mace_tools.py` to provide each test with a fresh
    sqlite + cache directory via monkeypatching the bridge constructor.
    """
    global _BRIDGE
    _BRIDGE = None
