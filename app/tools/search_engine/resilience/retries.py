"""Bounded transient retry for HTTP calls in the federated search engine.

WHY this exists: the search engine previously had ZERO retries — a single
transient 429 (rate limit) or 503 (service unavailable) or a momentary
connection reset was a hard, unrecoverable failure that fed the circuit
breaker. The OPTIMADE redesign (docs/OPTIMADE_REDESIGN_PLAN.md S4) adds a
tightly-scoped retry that recovers ONLY transient failures.

WHAT it retries (and only this):
  - 429 Too Many Requests  — honors Retry-After when present
  - 503 Service Unavailable — transient overload
  - Connection resets / transport blips:
      httpx.ConnectError, httpx.ReadError, httpx.RemoteProtocolError,
      httpx.ReadTimeout (a *connect*-phase blip, distinct from the engine's
      whole-call timeout which is NOT retried)

WHAT it does NOT retry (a retry here wastes time or doubles cost):
  - 400 Bad Request       — a bad filter; the provider is rejecting the query
  - 404 Not Found         — the resource/endpoint doesn't exist
  - 500 Server Error      — a persistent server bug; let the circuit breaker
                            handle it (retrying risks hammering a broken server)
  - asyncio.TimeoutError  — the call already consumed its full timeout budget;
                            retrying would double the cost for ~zero gain

Policy: ONE retry (not a chain), exponential backoff capped at 2.0s. This
recovers a transient blip without ever turning one slow provider into a long
stall — the engine's global deadline (S2) still caps the whole fan-out.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Awaitable, Callable, TypeVar

import httpx

logger = logging.getLogger(__name__)

T = TypeVar("T")

# Status codes that indicate a TRANSIENT condition worth one retry.
_TRANSIENT_STATUS = frozenset({429, 503})

# httpx transport exceptions that represent a transient connection blip
# (as opposed to a permanent misconfiguration). A ConnectTimeout during the
# connect phase is transient; a ReadTimeout mid-stream is handled by the
# engine's own timeout and NOT retried here.
_TRANSIENT_TRANSPORT = (
    httpx.ConnectError,
    httpx.RemoteProtocolError,
    httpx.ReadError,
)


def _backoff_delay(attempt: int) -> float:
    """Exponential backoff for attempt n (0-indexed): 0.5, 1.0, ... capped 2.0s."""
    return min(0.5 * (2**attempt), 2.0)


def _retry_after_seconds(response: httpx.Response) -> float | None:
    """Parse a Retry-After header (seconds form only; HTTP-date not supported)."""
    val = response.headers.get("Retry-After")
    if val is None:
        return None
    try:
        return min(float(val), 5.0)  # never wait longer than 5s even if asked
    except ValueError:
        return None  # HTTP-date form — honor the default backoff instead


def _is_transient_status(exc: httpx.HTTPStatusError) -> tuple[bool, float]:
    """Return (is_transient, delay_seconds) for an HTTPStatusError."""
    status = exc.response.status_code
    if status in _TRANSIENT_STATUS:
        delay = _retry_after_seconds(exc.response)
        return True, delay if delay is not None else _backoff_delay(0)
    return False, 0.0


async def with_transient_retry(
    coro_factory: Callable[[], Awaitable[T]],
    *,
    max_retries: int = 1,
    provider_id: str = "?",
) -> T:
    """Call ``coro_factory()`` with ONE retry on a transient failure only.

    ``coro_factory`` is a zero-arg callable returning a fresh coroutine — we
    re-invoke it on retry because an awaitable can only be awaited once.

    Raises the last exception if all attempts fail or the failure is
    non-transient (non-transient failures raise immediately, no retry).
    """
    last_exc: Exception | None = None
    for attempt in range(max_retries + 1):
        try:
            return await coro_factory()
        except httpx.HTTPStatusError as e:
            transient, delay = _is_transient_status(e)
            if not transient or attempt >= max_retries:
                raise
            logger.info(
                "provider '%s': transient HTTP %d — retrying in %.1fs (attempt %d/%d)",
                provider_id,
                e.response.status_code,
                delay,
                attempt + 1,
                max_retries,
            )
            last_exc = e
            await asyncio.sleep(delay)
        except _TRANSIENT_TRANSPORT as e:
            if attempt >= max_retries:
                raise
            delay = _backoff_delay(attempt)
            logger.info(
                "provider '%s': transient %s — retrying in %.1fs (attempt %d/%d)",
                provider_id,
                type(e).__name__,
                delay,
                attempt + 1,
                max_retries,
            )
            last_exc = e
            await asyncio.sleep(delay)
        # Any other exception (400 via raise_for_status that isn't in the
        # transient set, 404, 500, asyncio.TimeoutError, etc.) falls through
        # and is NOT retried — it re-raises immediately.
    # Unreachable: the loop either returns or raises. Defensive fallback.
    assert last_exc is not None
    raise last_exc
