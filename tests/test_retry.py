"""Tests for retry with exponential backoff."""
import pytest
from unittest.mock import MagicMock, patch
from app.agent.backends.base import Backend
from app.agent.events import AgentResponse


class FakeAPIError(Exception):
    def __init__(self, status_code, headers=None):
        self.status_code = status_code
        self.headers = headers or {}
        super().__init__(f"HTTP {status_code}")


class ConcreteBackend(Backend):
    _retryable_exceptions = (FakeAPIError,)
    def complete(self, messages, tools, system_prompt=None):
        return AgentResponse(text="ok")


class TestRetryAPICall:
    def test_success_no_retry(self):
        backend = ConcreteBackend()
        fn = MagicMock(return_value="ok")
        result = backend._retry_api_call(fn)
        assert result == "ok"
        assert fn.call_count == 1

    def test_retry_on_429(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=[FakeAPIError(429), FakeAPIError(429), "ok"])
        with patch("time.sleep"):
            result = backend._retry_api_call(fn)
        assert result == "ok"
        assert fn.call_count == 3

    def test_retry_on_500(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=[FakeAPIError(500), "ok"])
        with patch("time.sleep"):
            result = backend._retry_api_call(fn)
        assert result == "ok"

    def test_retry_on_502(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=[FakeAPIError(502), "ok"])
        with patch("time.sleep"):
            result = backend._retry_api_call(fn)
        assert result == "ok"

    def test_retry_on_503(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=[FakeAPIError(503), "ok"])
        with patch("time.sleep"):
            result = backend._retry_api_call(fn)
        assert result == "ok"

    def test_no_retry_on_400(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=FakeAPIError(400))
        with pytest.raises(FakeAPIError):
            backend._retry_api_call(fn)
        assert fn.call_count == 1

    def test_no_retry_on_401(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=FakeAPIError(401))
        with pytest.raises(FakeAPIError):
            backend._retry_api_call(fn)
        assert fn.call_count == 1

    def test_max_retries_exhausted(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=FakeAPIError(429))
        with patch("time.sleep"):
            with pytest.raises(FakeAPIError):
                backend._retry_api_call(fn, max_retries=3)
        assert fn.call_count == 4

    def test_exponential_backoff_delays(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=[FakeAPIError(429), FakeAPIError(429), FakeAPIError(429), "ok"])
        with patch("time.sleep") as mock_sleep:
            backend._retry_api_call(fn)
        delays = [call.args[0] for call in mock_sleep.call_args_list]
        assert delays == [1, 2, 4]

    def test_retry_after_header_respected(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=[FakeAPIError(429, headers={"Retry-After": "10"}), "ok"])
        with patch("time.sleep") as mock_sleep:
            backend._retry_api_call(fn)
        assert mock_sleep.call_args.args[0] >= 10

    def test_non_api_exceptions_not_retried(self):
        backend = ConcreteBackend()
        fn = MagicMock(side_effect=ValueError("bad"))
        with pytest.raises(ValueError):
            backend._retry_api_call(fn)
        assert fn.call_count == 1
