"""Tests for the bounded transient retry (resilience/retries.py, OPTIMADE S4)."""
import asyncio

import httpx
import pytest

from app.tools.search_engine.resilience.retries import (
    with_transient_retry,
    _backoff_delay,
    _is_transient_status,
)


def _status_error(status: int, headers: dict | None = None) -> httpx.HTTPStatusError:
    """Build an HTTPStatusError for a given status code (with optional headers)."""
    req = httpx.Request("GET", "https://example/x")
    resp = httpx.Response(status, request=req, headers=headers or {})
    return httpx.HTTPStatusError("err", request=req, response=resp)


def test_backoff_is_exponential_and_capped():
    assert _backoff_delay(0) == 0.5
    assert _backoff_delay(1) == 1.0
    assert _backoff_delay(2) == 2.0
    assert _backoff_delay(10) == 2.0  # capped


def test_transient_status_classification():
    # 429 and 503 are transient
    transient, _ = _is_transient_status(_status_error(429))
    assert transient is True
    transient, _ = _is_transient_status(_status_error(503))
    assert transient is True
    # 400/404/500 are NOT transient — retrying them wastes time
    for code in (400, 404, 500):
        transient, _ = _is_transient_status(_status_error(code))
        assert transient is False, f"{code} must not retry"


def test_retry_after_header_honored_but_capped():
    transient, delay = _is_transient_status(
        _status_error(429, headers={"Retry-After": "10"})
    )
    assert transient is True
    assert delay == 5.0  # capped at 5s even though server said 10


def test_429_retries_then_succeeds():
    """A transient 429 on the first call is retried and the second succeeds."""
    calls = {"n": 0}

    async def factory():
        calls["n"] += 1
        if calls["n"] == 1:
            raise _status_error(429)
        return "ok"

    result = asyncio.run(with_transient_retry(factory, provider_id="t"))
    assert result == "ok"
    assert calls["n"] == 2  # one retry


def test_500_never_retries():
    """A 500 raises immediately — retrying risks hammering a broken server."""
    calls = {"n": 0}

    async def factory():
        calls["n"] += 1
        raise _status_error(500)

    with pytest.raises(httpx.HTTPStatusError):
        asyncio.run(with_transient_retry(factory, provider_id="t"))
    assert calls["n"] == 1  # no retry


def test_400_never_retries():
    """A 400 (bad filter) raises immediately — the query is wrong, not transient."""
    calls = {"n": 0}

    async def factory():
        calls["n"] += 1
        raise _status_error(400)

    with pytest.raises(httpx.HTTPStatusError):
        asyncio.run(with_transient_retry(factory, provider_id="t"))
    assert calls["n"] == 1


def test_connect_error_retries():
    """A transient ConnectError is retried (connection blip)."""
    calls = {"n": 0}

    async def factory():
        calls["n"] += 1
        if calls["n"] == 1:
            raise httpx.ConnectError("blip")
        return "ok"

    result = asyncio.run(with_transient_retry(factory, provider_id="t"))
    assert result == "ok"
    assert calls["n"] == 2


def test_timeout_not_retried():
    """asyncio.TimeoutError is NOT retried — the call already cost its full budget."""
    calls = {"n": 0}

    async def factory():
        calls["n"] += 1
        raise asyncio.TimeoutError()

    with pytest.raises(asyncio.TimeoutError):
        asyncio.run(with_transient_retry(factory, provider_id="t"))
    assert calls["n"] == 1


def test_max_retries_exhausted_raises():
    """After max_retries transient failures, the last exception is raised."""
    calls = {"n": 0}

    async def factory():
        calls["n"] += 1
        raise _status_error(429)

    with pytest.raises(httpx.HTTPStatusError):
        asyncio.run(with_transient_retry(factory, provider_id="t", max_retries=1))
    # 1 initial + 1 retry = 2 calls
    assert calls["n"] == 2
