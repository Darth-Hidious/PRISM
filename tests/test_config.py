"""
Test configuration for running JARVIS connector tests without dependencies.
"""

import os
import sys
from unittest.mock import MagicMock

# Mock the settings to avoid configuration issues during testing
def mock_get_settings():
    """Return a mock settings object for testing."""
    mock_settings = MagicMock()
    mock_settings.redis_url = "redis://localhost:6379/0"
    mock_settings.redis_decode_responses = True
    return mock_settings

# Patch the config module before importing the connector
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# Mock the config module
import app.core.config
app.core.config.get_settings = mock_get_settings
