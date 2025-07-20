"""
Rate Limiter Integration for Data Ingestion Microservice.

This module provides easy integration of the distributed rate limiter
with the FastAPI application and Redis connection.
"""

import logging
from typing import Optional, Dict, Any

import redis.asyncio as redis
from fastapi import FastAPI

from .rate_limiter import (
    DistributedRateLimiter,
    RateLimitConfig,
    init_rate_limiter,
    get_rate_limiter
)


logger = logging.getLogger(__name__)


class RateLimiterManager:
    """Manager for rate limiter integration with FastAPI app."""
    
    def __init__(self):
        self.redis_client: Optional[redis.Redis] = None
        self.rate_limiter: Optional[DistributedRateLimiter] = None
        self.is_initialized = False
    
    async def initialize(
        self,
        redis_url: str,
        default_config: Optional[RateLimitConfig] = None,
        source_configs: Optional[Dict[str, RateLimitConfig]] = None,
        endpoint_configs: Optional[Dict[str, Dict[str, RateLimitConfig]]] = None
    ) -> DistributedRateLimiter:
        """
        Initialize rate limiter with Redis connection.
        
        Args:
            redis_url: Redis connection URL
            default_config: Default rate limiting configuration
            source_configs: Per-source rate limiting configurations
            endpoint_configs: Per-endpoint rate limiting configurations
            
        Returns:
            Initialized rate limiter instance
        """
        try:
            # Create Redis connection
            self.redis_client = redis.from_url(
                redis_url,
                decode_responses=False,
                health_check_interval=30,
                retry_on_timeout=True
            )
            
            # Test connection
            await self.redis_client.ping()
            logger.info("Redis connection established for rate limiter")
            
            # Initialize rate limiter
            default_config = default_config or RateLimitConfig(
                requests_per_minute=100,
                burst_capacity=50,
                queue_size=100,
                queue_timeout=30.0,
                adaptive_enabled=True
            )
            
            self.rate_limiter = init_rate_limiter(self.redis_client, default_config)
            
            # Configure sources
            if source_configs:
                for source, config in source_configs.items():
                    self.rate_limiter.configure_source(source, config)
            
            # Configure endpoints
            if endpoint_configs:
                for source, endpoints in endpoint_configs.items():
                    for endpoint, config in endpoints.items():
                        self.rate_limiter.configure_endpoint(source, endpoint, config)
            
            # Set up default database source configurations
            await self._setup_default_configurations()
            
            self.is_initialized = True
            logger.info("Rate limiter initialized successfully")
            
            return self.rate_limiter
            
        except Exception as e:
            logger.error(f"Failed to initialize rate limiter: {e}")
            raise
    
    async def _setup_default_configurations(self) -> None:
        """Set up default rate limiting configurations for common databases."""
        if not self.rate_limiter:
            return
        
        # JARVIS-DFT configurations
        self.rate_limiter.configure_source(
            "jarvis",
            RateLimitConfig(
                requests_per_minute=120,  # Conservative limit
                burst_capacity=60,
                queue_size=200,
                queue_timeout=60.0,
                adaptive_enabled=True,
                adaptive_backoff_factor=0.3,
                adaptive_recovery_factor=1.05
            )
        )
        
        # JARVIS endpoint-specific limits
        jarvis_endpoints = {
            "dft_3d": RateLimitConfig(requests_per_minute=100, burst_capacity=50),
            "dft_2d": RateLimitConfig(requests_per_minute=80, burst_capacity=40),
            "ml": RateLimitConfig(requests_per_minute=150, burst_capacity=75),
            "search": RateLimitConfig(requests_per_minute=60, burst_capacity=30),
            "bulk": RateLimitConfig(requests_per_minute=30, burst_capacity=15)
        }
        
        for endpoint, config in jarvis_endpoints.items():
            self.rate_limiter.configure_endpoint("jarvis", endpoint, config)
        
        # Materials Project configurations
        self.rate_limiter.configure_source(
            "materials_project",
            RateLimitConfig(
                requests_per_minute=1000,  # Higher limit for MP
                burst_capacity=200,
                queue_size=500,
                adaptive_enabled=True
            )
        )
        
        # AFLOW configurations
        self.rate_limiter.configure_source(
            "aflow",
            RateLimitConfig(
                requests_per_minute=60,
                burst_capacity=30,
                queue_size=100,
                adaptive_enabled=True
            )
        )
        
        # OQMD configurations
        self.rate_limiter.configure_source(
            "oqmd",
            RateLimitConfig(
                requests_per_minute=100,
                burst_capacity=50,
                adaptive_enabled=True
            )
        )
        
        # Crystallography Open Database (COD)
        self.rate_limiter.configure_source(
            "cod",
            RateLimitConfig(
                requests_per_minute=200,
                burst_capacity=100,
                adaptive_enabled=True
            )
        )
        
        logger.info("Default database rate limiting configurations applied")
    
    async def cleanup(self) -> None:
        """Clean up rate limiter resources."""
        if self.rate_limiter:
            await self.rate_limiter.cleanup()
        
        if self.redis_client:
            await self.redis_client.close()
        
        self.is_initialized = False
        logger.info("Rate limiter cleanup completed")
    
    def get_metrics(self) -> Dict[str, Any]:
        """Get rate limiting metrics for monitoring."""
        if not self.rate_limiter:
            return {}
        
        # This would typically be called from a monitoring endpoint
        # In a real implementation, you'd gather metrics asynchronously
        return {
            "rate_limiter_initialized": self.is_initialized,
            "redis_connected": self.redis_client is not None,
            # Add more metrics as needed
        }


# Global manager instance
_manager = RateLimiterManager()


async def setup_rate_limiter(
    app: FastAPI,
    redis_url: str,
    **kwargs
) -> DistributedRateLimiter:
    """
    Set up rate limiter for FastAPI application.
    
    Args:
        app: FastAPI application instance
        redis_url: Redis connection URL
        **kwargs: Additional configuration options
        
    Returns:
        Initialized rate limiter
    """
    rate_limiter = await _manager.initialize(redis_url, **kwargs)
    
    # Add cleanup on app shutdown
    @app.on_event("shutdown")
    async def shutdown_rate_limiter():
        await _manager.cleanup()
    
    return rate_limiter


def get_rate_limiter_manager() -> RateLimiterManager:
    """Get the global rate limiter manager."""
    return _manager


# Example usage functions for common patterns

async def with_jarvis_rate_limit(func, *args, **kwargs):
    """Execute function with JARVIS rate limiting."""
    from .rate_limiter import rate_limit
    
    @rate_limit(source="jarvis")
    async def _execute():
        return await func(*args, **kwargs)
    
    return await _execute()


async def with_materials_project_rate_limit(func, *args, **kwargs):
    """Execute function with Materials Project rate limiting."""
    from .rate_limiter import rate_limit
    
    @rate_limit(source="materials_project")
    async def _execute():
        return await func(*args, **kwargs)
    
    return await _execute()


# Monitoring and health check functions

async def rate_limiter_health_check() -> Dict[str, Any]:
    """Health check for rate limiter."""
    manager = get_rate_limiter_manager()
    rate_limiter = get_rate_limiter()
    
    health = {
        "rate_limiter_initialized": manager.is_initialized,
        "redis_connected": False,
        "status": "unknown"
    }
    
    try:
        if manager.redis_client:
            await manager.redis_client.ping()
            health["redis_connected"] = True
        
        if rate_limiter and manager.is_initialized:
            health["status"] = "healthy"
        else:
            health["status"] = "unhealthy"
            
    except Exception as e:
        health["status"] = "unhealthy"
        health["error"] = str(e)
    
    return health


async def get_rate_limiter_metrics() -> Dict[str, Any]:
    """Get comprehensive rate limiter metrics."""
    rate_limiter = get_rate_limiter()
    if not rate_limiter:
        return {"error": "Rate limiter not initialized"}
    
    try:
        # Get all metrics
        all_metrics = await rate_limiter.get_metrics()
        
        # Convert to serializable format
        metrics_data = {}
        for key, metrics in all_metrics.items():
            metrics_data[key] = metrics.to_dict()
        
        return {
            "timestamp": logger.time.time(),
            "metrics": metrics_data,
            "total_sources": len(set(m.source for m in all_metrics.values())),
            "health": await rate_limiter_health_check()
        }
        
    except Exception as e:
        logger.error(f"Failed to get rate limiter metrics: {e}")
        return {"error": str(e)}
