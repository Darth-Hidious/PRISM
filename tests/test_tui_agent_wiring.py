"""Tests for agent wiring in PrismApp."""
from unittest.mock import MagicMock


def test_prism_app_creates_agent_on_first_message():
    from app.tui.app import PrismApp
    mock_backend = MagicMock()
    app = PrismApp(backend=mock_backend)
    # Agent is lazy — not created until first message
    assert app._agent is None


def test_prism_app_init_agent():
    from app.tui.app import PrismApp
    mock_backend = MagicMock()
    app = PrismApp(backend=mock_backend)
    app._init_agent()
    assert app._agent is not None
