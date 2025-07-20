from typing import AsyncGenerator
import logging

from fastapi import Depends, HTTPException, status
from redis.asyncio import Redis
from sqlalchemy.ext.asyncio import AsyncSession

from .config import get_settings, Settings
from ..db.database import get_db_session
from ..services.connectors.redis_connector import get_redis_client


logger = logging.getLogger(__name__)


async def get_settings_dependency() -> Settings:
    """Get settings dependency."""
    return get_settings()


async def get_database_session() -> AsyncGenerator[AsyncSession, None]:
    """Get database session dependency."""
    async for session in get_db_session():
        try:
            yield session
        except Exception as e:
            logger.error(f"Database session error: {e}")
            raise HTTPException(
                status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
                detail="Database connection error"
            )


async def get_redis_dependency() -> Redis:
    """Get Redis client dependency."""
    try:
        redis_client = await get_redis_client()
        return redis_client
    except Exception as e:
        logger.error(f"Redis connection error: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Redis connection error"
        )


async def health_check_dependencies() -> dict:
    """Perform health checks on all dependencies."""
    health_status = {
        "status": "healthy",
        "services": {}
    }
    
    # Check database
    try:
        async for db in get_database_session():
            await db.execute("SELECT 1")
            health_status["services"]["database"] = "healthy"
            break
    except Exception as e:
        logger.error(f"Database health check failed: {e}")
        health_status["services"]["database"] = "unhealthy"
        health_status["status"] = "unhealthy"
    
    # Check Redis
    try:
        redis_client = await get_redis_dependency()
        await redis_client.ping()
        health_status["services"]["redis"] = "healthy"
    except Exception as e:
        logger.error(f"Redis health check failed: {e}")
        health_status["services"]["redis"] = "unhealthy"
        health_status["status"] = "unhealthy"
    
    return health_status


# Common dependencies
CommonDeps = {
    "settings": Depends(get_settings_dependency),
    "db": Depends(get_database_session),
    "redis": Depends(get_redis_dependency),
}
