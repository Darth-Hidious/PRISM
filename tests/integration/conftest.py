"""
Shared fixtures for integration tests.
"""

import pytest
import pytest_asyncio
from typing import AsyncGenerator

# Import all fixtures from fixtures.py
from .fixtures import (
    mock_api_server,
    mock_httpx_with_server,
    db_helper,
    connector_helper,
    performance_monitor,
    assert_material_data_complete,
    assert_job_completed_successfully
)

# Import fixtures from main test file  
from .test_connectors import (
    test_settings,
    test_db_engine,
    test_db_session,
    test_redis,
    rate_limiter_manager,
    job_processor,
    job_scheduler,
    sample_jarvis_response,
    sample_nomad_response,
    sample_standardized_material,
    mock_httpx_client
)

# Make all fixtures available
__all__ = [
    "mock_api_server",
    "mock_httpx_with_server", 
    "db_helper",
    "connector_helper",
    "performance_monitor",
    "assert_material_data_complete",
    "assert_job_completed_successfully",
    "test_settings",
    "test_db_engine",
    "test_db_session", 
    "test_redis",
    "rate_limiter_manager",
    "job_processor",
    "job_scheduler",
    "sample_jarvis_response",
    "sample_nomad_response",
    "sample_standardized_material",
    "mock_httpx_client"
]
