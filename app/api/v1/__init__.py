from fastapi import APIRouter
from .endpoints import health, jobs, sources, destinations

api_router = APIRouter()

# Include all endpoint routers
api_router.include_router(health.router, prefix="/health", tags=["health"])
api_router.include_router(jobs.router, prefix="/jobs", tags=["jobs"])
api_router.include_router(sources.router, prefix="/sources", tags=["sources"])
api_router.include_router(destinations.router, prefix="/destinations", tags=["destinations"])
