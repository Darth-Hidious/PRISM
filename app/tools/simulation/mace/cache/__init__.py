"""Content-addressed cache for MACE results."""

from .hashing import cache_key, canonical_structure_repr
from .store import CacheStore

__all__ = ["cache_key", "canonical_structure_repr", "CacheStore"]
