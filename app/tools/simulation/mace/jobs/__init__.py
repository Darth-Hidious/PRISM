"""Job state machine + optional async runner.

The durable store is usable without MACE's optional ULID/torch dependency.
Keep the runner import lazy so filesystem checks and cached job inspection do
not fail merely because the science extra is absent.
"""

from .store import JobStore

__all__ = ["JobStore", "JobRunner"]


def __getattr__(name: str):
    if name == "JobRunner":
        from .runner import JobRunner

        return JobRunner
    raise AttributeError(name)
