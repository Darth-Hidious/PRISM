"""
Distributed Rate Limiter with Redis backend for multi-instance deployments.

Features:
- Token bucket algorithm with Redis persistence
- Per-source and per-endpoint rate limiting
- Adaptive rate limiting on 429 responses
- Request queuing when rate limits exceeded
- Comprehensive monitoring metrics
- Configurable burst capacity and refill rates
"""

import asyncio
import json
import time
import logging
from typing import Dict, Optional, Any, Callable, Tuple
from functools import wraps
from datetime import datetime, timedelta
from dataclasses import dataclass, asdict
from enum import Enum

import redis.asyncio as redis
from pydantic import BaseModel


logger = logging.getLogger(__name__)


class RateLimitStatus(Enum):
    """Rate limit check status."""
    ALLOWED = "allowed"
    QUEUED = "queued"
    REJECTED = "rejected"
    ERROR = "error"


@dataclass
class RateLimitMetrics:
    """Metrics for rate limiting monitoring."""
    source: str
    endpoint: str
    current_tokens: float
    requests_allowed: int
    requests_queued: int
    requests_rejected: int
    queue_length: int
    last_refill: float
    adaptive_multiplier: float
    burst_capacity: int
    refill_rate: float
    
    def to_dict(self) -> Dict[str, Any]:
        """Convert metrics to dictionary."""
        return asdict(self)


class RateLimitConfig(BaseModel):
    """Configuration for rate limiting."""
    requests_per_minute: int = 60
    burst_capacity: Optional[int] = None
    queue_size: int = 100
    queue_timeout: float = 30.0
    adaptive_enabled: bool = True
    adaptive_backoff_factor: float = 0.5
    adaptive_recovery_factor: float = 1.1
    adaptive_min_multiplier: float = 0.1
    adaptive_max_multiplier: float = 2.0
    
    def __post_init__(self):
        """Set burst capacity if not provided."""
        if self.burst_capacity is None:
            self.burst_capacity = max(10, self.requests_per_minute // 2)


class DistributedRateLimiter:
    """
    Distributed rate limiter using Redis with token bucket algorithm.
    
    Supports:
    - Per-source rate limiting (e.g., different databases)
    - Per-endpoint rate limiting within sources
    - Adaptive rate limiting based on 429 responses
    - Request queuing with configurable timeouts
    - Comprehensive metrics collection
    """
    
    def __init__(
        self,
        redis_client: redis.Redis,
        default_config: Optional[RateLimitConfig] = None,
        key_prefix: str = "rate_limit"
    ):
        """
        Initialize distributed rate limiter.
        
        Args:
            redis_client: Redis client instance
            default_config: Default rate limit configuration
            key_prefix: Redis key prefix for rate limiting data
        """
        self.redis = redis_client
        self.default_config = default_config or RateLimitConfig()
        self.key_prefix = key_prefix
        
        # Per-source and per-endpoint configurations
        self.source_configs: Dict[str, RateLimitConfig] = {}
        self.endpoint_configs: Dict[str, Dict[str, RateLimitConfig]] = {}
        
        # Metrics tracking
        self.metrics: Dict[str, RateLimitMetrics] = {}
        
        # Request queues (in-memory per instance)
        self.request_queues: Dict[str, asyncio.Queue] = {}
        self.queue_processors: Dict[str, asyncio.Task] = {}
        
        # Lua script for atomic token bucket operations
        self.token_bucket_script = """
        local key = KEYS[1]
        local capacity = tonumber(ARGV[1])
        local refill_rate = tonumber(ARGV[2])
        local requested = tonumber(ARGV[3])
        local now = tonumber(ARGV[4])
        
        local bucket = redis.call('HMGET', key, 'tokens', 'last_refill')
        local tokens = tonumber(bucket[1]) or capacity
        local last_refill = tonumber(bucket[2]) or now
        
        -- Calculate tokens to add based on time elapsed
        local time_elapsed = math.max(0, now - last_refill)
        local tokens_to_add = time_elapsed * (refill_rate / 60.0)
        tokens = math.min(capacity, tokens + tokens_to_add)
        
        -- Check if we can fulfill the request
        local can_proceed = tokens >= requested
        if can_proceed then
            tokens = tokens - requested
        end
        
        -- Update bucket state
        redis.call('HMSET', key, 'tokens', tokens, 'last_refill', now)
        redis.call('EXPIRE', key, 3600)  -- 1 hour TTL
        
        return {can_proceed and 1 or 0, tokens, last_refill}
        """
    
    def configure_source(
        self,
        source: str,
        config: RateLimitConfig
    ) -> None:
        """Configure rate limiting for a specific source."""
        self.source_configs[source] = config
        logger.info(f"Configured rate limiting for source '{source}': {config.requests_per_minute} RPM")
    
    def configure_endpoint(
        self,
        source: str,
        endpoint: str,
        config: RateLimitConfig
    ) -> None:
        """Configure rate limiting for a specific endpoint within a source."""
        if source not in self.endpoint_configs:
            self.endpoint_configs[source] = {}
        self.endpoint_configs[source][endpoint] = config
        logger.info(
            f"Configured endpoint rate limiting for '{source}.{endpoint}': "
            f"{config.requests_per_minute} RPM"
        )
    
    def _get_config(self, source: str, endpoint: Optional[str] = None) -> RateLimitConfig:
        """Get rate limit configuration for source/endpoint."""
        if endpoint and source in self.endpoint_configs:
            if endpoint in self.endpoint_configs[source]:
                return self.endpoint_configs[source][endpoint]
        
        if source in self.source_configs:
            return self.source_configs[source]
        
        return self.default_config
    
    def _get_rate_limit_key(self, source: str, endpoint: Optional[str] = None) -> str:
        """Generate Redis key for rate limiting."""
        if endpoint:
            return f"{self.key_prefix}:bucket:{source}:{endpoint}"
        return f"{self.key_prefix}:bucket:{source}"
    
    def _get_adaptive_key(self, source: str, endpoint: Optional[str] = None) -> str:
        """Generate Redis key for adaptive rate limiting."""
        if endpoint:
            return f"{self.key_prefix}:adaptive:{source}:{endpoint}"
        return f"{self.key_prefix}:adaptive:{source}"
    
    def _get_metrics_key(self, source: str, endpoint: Optional[str] = None) -> str:
        """Generate key for metrics tracking."""
        if endpoint:
            return f"{source}:{endpoint}"
        return source
    
    async def _get_adaptive_multiplier(
        self,
        source: str,
        endpoint: Optional[str] = None
    ) -> float:
        """Get current adaptive rate multiplier."""
        key = self._get_adaptive_key(source, endpoint)
        try:
            result = await self.redis.get(key)
            if result:
                return float(result)
        except Exception as e:
            logger.warning(f"Failed to get adaptive multiplier: {e}")
        return 1.0
    
    async def _update_adaptive_multiplier(
        self,
        source: str,
        multiplier: float,
        endpoint: Optional[str] = None
    ) -> None:
        """Update adaptive rate multiplier."""
        key = self._get_adaptive_key(source, endpoint)
        try:
            await self.redis.setex(key, 3600, str(multiplier))  # 1 hour TTL
        except Exception as e:
            logger.warning(f"Failed to update adaptive multiplier: {e}")
    
    async def check_rate_limit(
        self,
        source: str,
        endpoint: Optional[str] = None,
        tokens_requested: int = 1
    ) -> Tuple[RateLimitStatus, RateLimitMetrics]:
        """
        Check if request is allowed under rate limits.
        
        Args:
            source: Data source identifier
            endpoint: Optional endpoint identifier
            tokens_requested: Number of tokens to request
            
        Returns:
            Tuple of (status, metrics)
        """
        config = self._get_config(source, endpoint)
        key = self._get_rate_limit_key(source, endpoint)
        metrics_key = self._get_metrics_key(source, endpoint)
        
        try:
            # Get adaptive multiplier
            adaptive_multiplier = await self._get_adaptive_multiplier(source, endpoint)
            
            # Calculate effective rate with adaptive multiplier
            effective_rate = config.requests_per_minute * adaptive_multiplier
            effective_capacity = int(config.burst_capacity * adaptive_multiplier)
            
            # Execute token bucket algorithm atomically
            now = time.time()
            result = await self.redis.eval(
                self.token_bucket_script,
                1,
                key,
                str(effective_capacity),
                str(effective_rate),
                str(tokens_requested),
                str(now)
            )
            
            can_proceed, current_tokens, last_refill = result
            
            # Update metrics
            if metrics_key not in self.metrics:
                self.metrics[metrics_key] = RateLimitMetrics(
                    source=source,
                    endpoint=endpoint or "",
                    current_tokens=current_tokens,
                    requests_allowed=0,
                    requests_queued=0,
                    requests_rejected=0,
                    queue_length=0,
                    last_refill=last_refill,
                    adaptive_multiplier=adaptive_multiplier,
                    burst_capacity=effective_capacity,
                    refill_rate=effective_rate
                )
            
            metrics = self.metrics[metrics_key]
            metrics.current_tokens = current_tokens
            metrics.adaptive_multiplier = adaptive_multiplier
            metrics.burst_capacity = effective_capacity
            metrics.refill_rate = effective_rate
            
            if can_proceed:
                metrics.requests_allowed += 1
                return RateLimitStatus.ALLOWED, metrics
            
            # Check if we should queue or reject
            queue_key = f"queue:{metrics_key}"
            if queue_key not in self.request_queues:
                self.request_queues[queue_key] = asyncio.Queue(maxsize=config.queue_size)
            
            queue = self.request_queues[queue_key]
            
            if queue.full():
                metrics.requests_rejected += 1
                return RateLimitStatus.REJECTED, metrics
            
            metrics.requests_queued += 1
            metrics.queue_length = queue.qsize()
            return RateLimitStatus.QUEUED, metrics
            
        except Exception as e:
            logger.error(f"Rate limit check failed for {source}.{endpoint}: {e}")
            # Fail open - allow request but log error
            return RateLimitStatus.ERROR, self.metrics.get(
                metrics_key,
                RateLimitMetrics(
                    source=source,
                    endpoint=endpoint or "",
                    current_tokens=0,
                    requests_allowed=0,
                    requests_queued=0,
                    requests_rejected=1,
                    queue_length=0,
                    last_refill=time.time(),
                    adaptive_multiplier=1.0,
                    burst_capacity=config.burst_capacity,
                    refill_rate=config.requests_per_minute
                )
            )
    
    async def report_response_status(
        self,
        source: str,
        status_code: int,
        endpoint: Optional[str] = None
    ) -> None:
        """
        Report response status for adaptive rate limiting.
        
        Args:
            source: Data source identifier
            status_code: HTTP response status code
            endpoint: Optional endpoint identifier
        """
        config = self._get_config(source, endpoint)
        if not config.adaptive_enabled:
            return
        
        try:
            current_multiplier = await self._get_adaptive_multiplier(source, endpoint)
            
            if status_code == 429:  # Too Many Requests
                # Decrease rate (slow down)
                new_multiplier = max(
                    config.adaptive_min_multiplier,
                    current_multiplier * config.adaptive_backoff_factor
                )
                logger.warning(
                    f"Rate limit hit for {source}.{endpoint}, "
                    f"reducing multiplier from {current_multiplier:.2f} to {new_multiplier:.2f}"
                )
                await self._update_adaptive_multiplier(source, new_multiplier, endpoint)
                
            elif 200 <= status_code < 300:  # Success
                # Gradually increase rate (speed up)
                new_multiplier = min(
                    config.adaptive_max_multiplier,
                    current_multiplier * config.adaptive_recovery_factor
                )
                if new_multiplier != current_multiplier:
                    await self._update_adaptive_multiplier(source, new_multiplier, endpoint)
                    
        except Exception as e:
            logger.error(f"Failed to update adaptive rate limiting: {e}")
    
    async def wait_for_rate_limit(
        self,
        source: str,
        endpoint: Optional[str] = None,
        timeout: Optional[float] = None
    ) -> bool:
        """
        Wait for rate limit to allow request.
        
        Args:
            source: Data source identifier
            endpoint: Optional endpoint identifier
            timeout: Maximum time to wait (uses config default if None)
            
        Returns:
            True if request can proceed, False if timeout
        """
        config = self._get_config(source, endpoint)
        wait_timeout = timeout or config.queue_timeout
        metrics_key = self._get_metrics_key(source, endpoint)
        queue_key = f"queue:{metrics_key}"
        
        # Ensure queue exists
        if queue_key not in self.request_queues:
            self.request_queues[queue_key] = asyncio.Queue(maxsize=config.queue_size)
        
        queue = self.request_queues[queue_key]
        
        try:
            # Put request in queue
            await asyncio.wait_for(queue.put(None), timeout=wait_timeout)
            
            # Start queue processor if not running
            if queue_key not in self.queue_processors:
                self.queue_processors[queue_key] = asyncio.create_task(
                    self._process_queue(source, endpoint, queue_key)
                )
            
            # Wait for our turn
            await asyncio.wait_for(queue.get(), timeout=wait_timeout)
            queue.task_done()
            
            return True
            
        except asyncio.TimeoutError:
            logger.warning(f"Rate limit wait timeout for {source}.{endpoint}")
            return False
        except Exception as e:
            logger.error(f"Error waiting for rate limit: {e}")
            return False
    
    async def _process_queue(
        self,
        source: str,
        endpoint: Optional[str],
        queue_key: str
    ) -> None:
        """Process queued requests for rate limiting."""
        queue = self.request_queues[queue_key]
        config = self._get_config(source, endpoint)
        
        while True:
            try:
                # Check if we can process next request
                status, _ = await self.check_rate_limit(source, endpoint)
                
                if status == RateLimitStatus.ALLOWED and not queue.empty():
                    # Process next queued request
                    queue.get_nowait()
                    queue.task_done()
                else:
                    # Wait before checking again
                    refill_interval = 60.0 / config.requests_per_minute
                    await asyncio.sleep(refill_interval)
                    
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Error processing rate limit queue: {e}")
                await asyncio.sleep(1.0)
    
    async def get_metrics(
        self,
        source: Optional[str] = None,
        endpoint: Optional[str] = None
    ) -> Dict[str, RateLimitMetrics]:
        """
        Get rate limiting metrics.
        
        Args:
            source: Optional source filter
            endpoint: Optional endpoint filter
            
        Returns:
            Dictionary of metrics by key
        """
        if source:
            metrics_key = self._get_metrics_key(source, endpoint)
            if metrics_key in self.metrics:
                return {metrics_key: self.metrics[metrics_key]}
            return {}
        
        return dict(self.metrics)
    
    async def reset_rate_limits(
        self,
        source: Optional[str] = None,
        endpoint: Optional[str] = None
    ) -> None:
        """Reset rate limits for source/endpoint."""
        if source:
            pattern = self._get_rate_limit_key(source, endpoint)
            await self.redis.delete(pattern)
            
            adaptive_key = self._get_adaptive_key(source, endpoint)
            await self.redis.delete(adaptive_key)
            
            metrics_key = self._get_metrics_key(source, endpoint)
            if metrics_key in self.metrics:
                del self.metrics[metrics_key]
        else:
            # Reset all rate limits
            pattern = f"{self.key_prefix}:*"
            keys = await self.redis.keys(pattern)
            if keys:
                await self.redis.delete(*keys)
            self.metrics.clear()
    
    async def cleanup(self) -> None:
        """Cleanup resources."""
        # Cancel queue processors
        for task in self.queue_processors.values():
            task.cancel()
        
        # Wait for tasks to complete
        if self.queue_processors:
            await asyncio.gather(
                *self.queue_processors.values(),
                return_exceptions=True
            )
        
        self.queue_processors.clear()
        self.request_queues.clear()


# Global rate limiter instance
_rate_limiter: Optional[DistributedRateLimiter] = None


def init_rate_limiter(
    redis_client: redis.Redis,
    default_config: Optional[RateLimitConfig] = None
) -> DistributedRateLimiter:
    """Initialize global rate limiter instance."""
    global _rate_limiter
    _rate_limiter = DistributedRateLimiter(redis_client, default_config)
    return _rate_limiter


def get_rate_limiter() -> Optional[DistributedRateLimiter]:
    """Get global rate limiter instance."""
    return _rate_limiter


def rate_limit(
    source: str,
    endpoint: Optional[str] = None,
    requests_per_minute: Optional[int] = None,
    burst_capacity: Optional[int] = None,
    queue_timeout: Optional[float] = None,
    tokens_requested: int = 1,
    fail_open: bool = True
):
    """
    Decorator for rate limiting function calls.
    
    Args:
        source: Data source identifier
        endpoint: Optional endpoint identifier
        requests_per_minute: Override requests per minute
        burst_capacity: Override burst capacity
        queue_timeout: Override queue timeout
        tokens_requested: Number of tokens to request
        fail_open: Allow requests if rate limiter fails
        
    Usage:
        @rate_limit(source="jarvis", requests_per_minute=100)
        async def fetch_data():
            ...
            
        @rate_limit(source="materials_project", endpoint="search", requests_per_minute=50)
        async def search_materials():
            ...
    """
    def decorator(func: Callable) -> Callable:
        @wraps(func)
        async def wrapper(*args, **kwargs):
            limiter = get_rate_limiter()
            if not limiter:
                logger.warning("Rate limiter not initialized, allowing request")
                if fail_open:
                    return await func(*args, **kwargs)
                else:
                    raise RuntimeError("Rate limiter not available")
            
            # Override configuration if specified
            if requests_per_minute or burst_capacity or queue_timeout:
                config = RateLimitConfig(
                    requests_per_minute=requests_per_minute or limiter.default_config.requests_per_minute,
                    burst_capacity=burst_capacity or limiter.default_config.burst_capacity,
                    queue_timeout=queue_timeout or limiter.default_config.queue_timeout
                )
                
                if endpoint:
                    limiter.configure_endpoint(source, endpoint, config)
                else:
                    limiter.configure_source(source, config)
            
            # Check rate limit
            status, metrics = await limiter.check_rate_limit(
                source, endpoint, tokens_requested
            )
            
            if status == RateLimitStatus.ALLOWED:
                try:
                    result = await func(*args, **kwargs)
                    # Report successful response for adaptive rate limiting
                    await limiter.report_response_status(source, 200, endpoint)
                    return result
                except Exception as e:
                    # Report error status if it's HTTP-related
                    if hasattr(e, 'status_code'):
                        await limiter.report_response_status(source, e.status_code, endpoint)
                    raise
                    
            elif status == RateLimitStatus.QUEUED:
                # Wait for rate limit to allow request
                wait_timeout = queue_timeout or limiter._get_config(source, endpoint).queue_timeout
                if await limiter.wait_for_rate_limit(source, endpoint, wait_timeout):
                    return await func(*args, **kwargs)
                else:
                    raise TimeoutError(f"Rate limit wait timeout for {source}.{endpoint}")
                    
            elif status == RateLimitStatus.REJECTED:
                raise RuntimeError(f"Rate limit exceeded for {source}.{endpoint}, queue full")
                
            else:  # ERROR
                if fail_open:
                    logger.warning(f"Rate limiter error for {source}.{endpoint}, allowing request")
                    return await func(*args, **kwargs)
                else:
                    raise RuntimeError(f"Rate limiter error for {source}.{endpoint}")
        
        return wrapper
    return decorator


# Convenience functions for common rate limiting patterns

async def jarvis_rate_limit(func: Callable, *args, **kwargs):
    """Rate limit for JARVIS-DFT API calls."""
    @rate_limit(source="jarvis", requests_per_minute=120, burst_capacity=60)
    async def _wrapped():
        return await func(*args, **kwargs)
    return await _wrapped()


async def materials_project_rate_limit(func: Callable, *args, **kwargs):
    """Rate limit for Materials Project API calls."""
    @rate_limit(source="materials_project", requests_per_minute=1000, burst_capacity=100)
    async def _wrapped():
        return await func(*args, **kwargs)
    return await _wrapped()


async def aflow_rate_limit(func: Callable, *args, **kwargs):
    """Rate limit for AFLOW API calls."""
    @rate_limit(source="aflow", requests_per_minute=60, burst_capacity=30)
    async def _wrapped():
        return await func(*args, **kwargs)
    return await _wrapped()
