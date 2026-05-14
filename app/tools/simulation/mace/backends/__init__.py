"""Compute backends — Fake, Local, HF Jobs, Platform (marc27 ml_predict).

Backend choice is exposed as a parameter on every primitive
(``backend: "auto" | "fake" | "local" | "hf_jobs" | "platform"``) so the
calling LLM (PRISM) can override the default heuristic.

The ``platform`` backend is the production path: it submits jobs to the
marc27 platform as ``job_type=ml_predict`` with ``model=mace-mh-1`` (or
``mace-mp-0`` per-call override). Lands when ``marc27/uip-mace-mh1:latest``
is published to the container registry and ``PRISM_PROJECT_ID`` is set.
"""

from .base import Backend, BackendJob, ProgressCb, select_backend
from .fake import FakeBackend
from .local import LocalBackend
from .platform import PlatformBackend

__all__ = [
    "Backend",
    "BackendJob",
    "ProgressCb",
    "select_backend",
    "FakeBackend",
    "LocalBackend",
    "PlatformBackend",
]
