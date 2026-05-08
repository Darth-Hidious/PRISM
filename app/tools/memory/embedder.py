"""Embedder — pluggable text→vector backend for the artifact store.

Supports multiple backends behind one interface:

  * `STEmbedder` — sentence-transformers, default `all-MiniLM-L6-v2` (384-d).
    Good semantic quality, ~80 MB model, ~30 ms per call.
  * `HashEmbedder` — deterministic hash-based fallback. NOT semantic.
    Used when sentence-transformers isn't installed AND for tests
    (hermetic, no model load, no network).

`get_default_embedder()` returns the best available backend, with the
choice logged at INFO so we can see which is in use.

The Rust harness can later inject a callback-based backend that calls
the EmbeddingGemma instance loaded for Stage 2.1 retrieval. The
interface stays the same; only the constructor changes.
"""
from __future__ import annotations

import abc
import hashlib
import logging
import math
import threading
from typing import Optional

logger = logging.getLogger(__name__)


class Embedder(abc.ABC):
    """Abstract embedder. Stable contract: text in, vector out."""

    @property
    @abc.abstractmethod
    def dim(self) -> int: ...

    @abc.abstractmethod
    def embed(self, text: str) -> list[float]: ...

    def embed_batch(self, texts: list[str]) -> list[list[float]]:
        """Default sequential batch — backends override for true batching."""
        return [self.embed(t) for t in texts]


class HashEmbedder(Embedder):
    """Deterministic hash-based embedder.

    NOT semantic. Used for tests (hermetic) and as a fallback when no real
    embedder is installed. Embedding shape is stable for identical input
    so tests can assert exact behavior, but two semantically-similar
    inputs will be unrelated in vector space.
    """

    def __init__(self, dim: int = 384) -> None:
        self._dim = dim

    @property
    def dim(self) -> int:
        return self._dim

    def embed(self, text: str) -> list[float]:
        # Build a vector by hashing the text into chunks. Sin transform
        # produces stable [-1, 1] values. Trivially reproducible.
        seed_hash = hashlib.sha256(text.encode("utf-8", errors="replace")).digest()
        # Stretch the 32-byte hash into `dim` floats by hashing variants
        out: list[float] = []
        i = 0
        while len(out) < self._dim:
            block = hashlib.sha256(seed_hash + i.to_bytes(2, "little")).digest()
            for j in range(0, 32, 4):
                if len(out) >= self._dim:
                    break
                v = int.from_bytes(block[j:j + 4], "little") / (2 ** 32)
                out.append(math.sin(v * 2 * math.pi))
            i += 1
        # L2-normalize so cosine == dot product
        norm = math.sqrt(sum(x * x for x in out)) or 1.0
        return [x / norm for x in out]


class STEmbedder(Embedder):
    """sentence-transformers backend. Default: `all-MiniLM-L6-v2`."""

    _model_lock = threading.Lock()
    _instances: dict[str, "STEmbedder"] = {}

    def __new__(cls, model_name: str = "all-MiniLM-L6-v2") -> "STEmbedder":
        # Singleton per model_name — model load is expensive
        with cls._model_lock:
            if model_name not in cls._instances:
                inst = super().__new__(cls)
                inst._model_name = model_name
                inst._model = None
                inst._dim_cached: Optional[int] = None
                cls._instances[model_name] = inst
        return cls._instances[model_name]

    def _ensure_loaded(self) -> None:
        if self._model is None:
            with self._model_lock:
                if self._model is None:  # double-check inside lock
                    from sentence_transformers import SentenceTransformer  # type: ignore
                    logger.info("loading sentence-transformers model: %s", self._model_name)
                    self._model = SentenceTransformer(self._model_name)
                    self._dim_cached = int(self._model.get_sentence_embedding_dimension())
                    logger.info("model loaded; dim=%d", self._dim_cached)

    @property
    def dim(self) -> int:
        self._ensure_loaded()
        assert self._dim_cached is not None
        return self._dim_cached

    def embed(self, text: str) -> list[float]:
        self._ensure_loaded()
        # convert_to_numpy=True returns ndarray; tolist for SQLite portability
        vec = self._model.encode(  # type: ignore[union-attr]
            text or "",
            convert_to_numpy=True,
            normalize_embeddings=True,
        )
        return [float(x) for x in vec]

    def embed_batch(self, texts: list[str]) -> list[list[float]]:
        self._ensure_loaded()
        if not texts:
            return []
        # Empty strings would error; replace with a single space placeholder
        clean = [t if t else " " for t in texts]
        vecs = self._model.encode(  # type: ignore[union-attr]
            clean,
            convert_to_numpy=True,
            normalize_embeddings=True,
            show_progress_bar=False,
        )
        return [[float(x) for x in v] for v in vecs]


_DEFAULT_LOCK = threading.Lock()
_DEFAULT_INSTANCE: Optional[Embedder] = None


def get_default_embedder() -> Embedder:
    """Return the best available embedder.

    Priority:
      1. sentence-transformers (real semantics) if installed
      2. HashEmbedder (deterministic fallback) otherwise

    Singleton — one instance per process, lazy-loaded.
    """
    global _DEFAULT_INSTANCE
    if _DEFAULT_INSTANCE is not None:
        return _DEFAULT_INSTANCE
    with _DEFAULT_LOCK:
        if _DEFAULT_INSTANCE is not None:
            return _DEFAULT_INSTANCE
        try:
            import sentence_transformers  # noqa: F401
            _DEFAULT_INSTANCE = STEmbedder()
            logger.info("default embedder: STEmbedder (sentence-transformers)")
        except Exception as e:
            logger.warning(
                "sentence-transformers unavailable (%s); falling back to "
                "HashEmbedder. Install with `pip install sentence-transformers` "
                "for real semantic recall.", e
            )
            _DEFAULT_INSTANCE = HashEmbedder()
        return _DEFAULT_INSTANCE


def reset_default_embedder() -> None:
    """For tests — drop the cached default so the next call re-decides."""
    global _DEFAULT_INSTANCE
    with _DEFAULT_LOCK:
        _DEFAULT_INSTANCE = None
