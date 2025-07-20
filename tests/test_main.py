import pytest
import asyncio
from httpx import AsyncClient
from fastapi.testclient import TestClient

from app.main import app
from app.core.config import get_settings


class TestHealthEndpoints:
    """Test health check endpoints."""
    
    def test_root_endpoint(self):
        """Test root endpoint."""
        with TestClient(app) as client:
            response = client.get("/")
            assert response.status_code == 200
            data = response.json()
            assert "message" in data
            assert "version" in data
            assert "status" in data
    
    @pytest.mark.asyncio
    async def test_health_endpoint(self):
        """Test health check endpoint."""
        async with AsyncClient(app=app, base_url="http://test") as client:
            response = await client.get("/api/v1/health/")
            assert response.status_code in [200, 503]  # May fail if deps not available
            data = response.json()
            assert "status" in data
            assert "timestamp" in data
    
    @pytest.mark.asyncio
    async def test_liveness_endpoint(self):
        """Test liveness probe endpoint."""
        async with AsyncClient(app=app, base_url="http://test") as client:
            response = await client.get("/api/v1/health/liveness")
            assert response.status_code == 200
            data = response.json()
            assert data["status"] == "alive"
            assert "timestamp" in data


class TestConfiguration:
    """Test configuration management."""
    
    def test_settings_creation(self):
        """Test settings can be created."""
        settings = get_settings()
        assert settings.app_name is not None
        assert settings.app_version is not None
        assert isinstance(settings.debug, bool)
    
    def test_database_url_construction(self):
        """Test database URL construction."""
        settings = get_settings()
        db_url = settings.database_url
        assert db_url.startswith("postgresql+asyncpg://")
        assert settings.postgres_user in db_url
        assert settings.postgres_db in db_url
    
    def test_redis_url_construction(self):
        """Test Redis URL construction."""
        settings = get_settings()
        redis_url = settings.redis_url
        assert redis_url.startswith("redis://")
        assert str(settings.redis_port) in redis_url


if __name__ == "__main__":
    pytest.main([__file__])
