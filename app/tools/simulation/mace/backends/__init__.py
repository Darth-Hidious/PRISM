"""Compute backends — Fake, Local, HF Jobs.

Backend choice is exposed as a parameter on every primitive
(``backend: "auto" | "fake" | "local" | "hf_jobs"``) so the calling LLM
(PRISM) can override the default heuristic.
"""

from .base import Backend, BackendJob, ProgressCb, select_backend
from .fake import FakeBackend
from .local import LocalBackend

__all__ = [
    "Backend",
    "BackendJob",
    "ProgressCb",
    "select_backend",
    "FakeBackend",
    "LocalBackend",
]
