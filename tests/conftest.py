"""Shared test fixtures for PRISM."""
import pytest
from unittest.mock import MagicMock


@pytest.fixture
def mock_llm_response():
    """Mock LLM completion response."""
    mock = MagicMock()
    mock.choices = [MagicMock()]
    mock.choices[0].message.content = '{"provider": "mp", "filter": "elements HAS ALL \\"Si\\""}'
    return mock


@pytest.fixture
def mock_anthropic_response():
    """Mock Anthropic completion response."""
    mock = MagicMock()
    mock.content = [MagicMock()]
    mock.content[0].text = '{"provider": "mp", "filter": "elements HAS ALL \\"Si\\""}'
    return mock
