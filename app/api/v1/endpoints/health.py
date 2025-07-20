from datetime import datetime
import logging

from fastapi import APIRouter, Depends
from redis.asyncio import Redis

from ....core.dependencies import health_check_dependencies, get_redis_dependency
from ....schemas import HealthCheckResponse, QueueStats
from ....services.connectors.redis_connector import get_job_queue

logger = logging.getLogger(__name__)
router = APIRouter()


@router.get("/", response_model=HealthCheckResponse)
async def health_check(
    health_status: dict = Depends(health_check_dependencies)
):
    """
    Health check endpoint to verify service status.
    """
    return HealthCheckResponse(
        status=health_status["status"],
        services=health_status["services"],
        timestamp=datetime.utcnow()
    )


@router.get("/liveness")
async def liveness_check():
    """
    Kubernetes liveness probe endpoint.
    """
    return {"status": "alive", "timestamp": datetime.utcnow()}


@router.get("/readiness")
async def readiness_check(
    health_status: dict = Depends(health_check_dependencies)
):
    """
    Kubernetes readiness probe endpoint.
    """
    if health_status["status"] == "healthy":
        return {"status": "ready", "timestamp": datetime.utcnow()}
    else:
        from fastapi import HTTPException, status
        raise HTTPException(
            status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
            detail="Service not ready"
        )


@router.get("/queue", response_model=QueueStats)
async def get_queue_stats(
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Get job queue statistics.
    """
    try:
        job_queue = await get_job_queue()
        stats = await job_queue.get_queue_stats()
        return QueueStats(**stats)
    except Exception as e:
        logger.error(f"Failed to get queue stats: {e}")
        from fastapi import HTTPException, status
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail="Failed to retrieve queue statistics"
        )
