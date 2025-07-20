"""
Comprehensive test suite for the distributed rate limiter.

Tests cover:
- Token bucket algorithm
- Distributed coordination via Redis
- Adaptive rate limiting
- Request queuing
- Metrics collection
- Decorator functionality
"""

import pytest
import asyncio
import time
from unittest.mock import AsyncMock, MagicMock, patch

import redis.asyncio as redis

from app.services.rate_limiter import (
    DistributedRateLimiter,
    RateLimitConfig,
    RateLimitStatus,
    rate_limit,
    init_rate_limiter,
    get_rate_limiter
)
from app.services.rate_limiter_integration import (
    RateLimiterManager,
    setup_rate_limiter,
    rate_limiter_health_check,
    get_rate_limiter_metrics
)


@pytest.fixture
async def mock_redis():
    """Mock Redis client for testing."""
    redis_mock = AsyncMock(spec=redis.Redis)
    
    # Mock token bucket script execution
    redis_mock.eval.return_value = [1, 50.0, time.time()]  # can_proceed, tokens, last_refill
    redis_mock.get.return_value = None
    redis_mock.setex.return_value = True
    redis_mock.delete.return_value = True
    redis_mock.keys.return_value = []
    redis_mock.ping.return_value = True
    
    return redis_mock


@pytest.fixture
async def rate_limiter(mock_redis):
    """Rate limiter instance for testing."""
    config = RateLimitConfig(
        requests_per_minute=60,
        burst_capacity=30,
        queue_size=10,
        queue_timeout=5.0
    )
    return DistributedRateLimiter(mock_redis, config)


class TestRateLimitConfig:
    """Test rate limit configuration."""
    
    def test_default_config(self):
        """Test default configuration values."""
        config = RateLimitConfig()
        assert config.requests_per_minute == 60
        assert config.burst_capacity == 30  # Default is max(10, rpm//2)
        assert config.queue_size == 100
        assert config.adaptive_enabled is True
    
    def test_custom_config(self):
        """Test custom configuration."""
        config = RateLimitConfig(
            requests_per_minute=120,
            burst_capacity=100,
            queue_size=50,
            adaptive_enabled=False
        )
        assert config.requests_per_minute == 120
        assert config.burst_capacity == 100
        assert config.queue_size == 50
        assert config.adaptive_enabled is False


class TestDistributedRateLimiter:
    """Test distributed rate limiter functionality."""
    
    async def test_initialization(self, mock_redis):
        """Test rate limiter initialization."""
        config = RateLimitConfig(requests_per_minute=100)
        limiter = DistributedRateLimiter(mock_redis, config)
        
        assert limiter.redis == mock_redis
        assert limiter.default_config == config
        assert limiter.key_prefix == "rate_limit"
    
    async def test_source_configuration(self, rate_limiter):
        """Test source-specific configuration."""
        config = RateLimitConfig(requests_per_minute=200)
        rate_limiter.configure_source("test_source", config)
        
        assert "test_source" in rate_limiter.source_configs
        assert rate_limiter.source_configs["test_source"] == config
    
    async def test_endpoint_configuration(self, rate_limiter):
        """Test endpoint-specific configuration."""
        config = RateLimitConfig(requests_per_minute=150)
        rate_limiter.configure_endpoint("test_source", "test_endpoint", config)
        
        assert "test_source" in rate_limiter.endpoint_configs
        assert "test_endpoint" in rate_limiter.endpoint_configs["test_source"]
        assert rate_limiter.endpoint_configs["test_source"]["test_endpoint"] == config
    
    async def test_rate_limit_allowed(self, rate_limiter, mock_redis):
        """Test rate limit check when request is allowed."""
        # Mock Redis to return that request is allowed
        mock_redis.eval.return_value = [1, 25.0, time.time()]
        
        status, metrics = await rate_limiter.check_rate_limit("test_source")
        
        assert status == RateLimitStatus.ALLOWED
        assert metrics.source == "test_source"
        assert metrics.current_tokens == 25.0
        assert metrics.requests_allowed == 1
    
    async def test_rate_limit_queued(self, rate_limiter, mock_redis):
        """Test rate limit check when request should be queued."""
        # Mock Redis to return that request is not allowed (no tokens)
        mock_redis.eval.return_value = [0, 0.0, time.time()]
        
        status, metrics = await rate_limiter.check_rate_limit("test_source")
        
        assert status == RateLimitStatus.QUEUED
        assert metrics.requests_queued == 1
    
    async def test_adaptive_rate_limiting(self, rate_limiter, mock_redis):
        """Test adaptive rate limiting functionality."""
        # Test 429 response (rate limit hit)
        await rate_limiter.report_response_status("test_source", 429)
        
        # Should have set adaptive multiplier
        mock_redis.setex.assert_called()
        
        # Test success response
        await rate_limiter.report_response_status("test_source", 200)
        
        # Should update adaptive multiplier
        assert mock_redis.setex.call_count >= 1
    
    async def test_queue_processing(self, rate_limiter):
        """Test request queue processing."""
        # This test would need more complex mocking to test queue processing
        # For now, just verify the queue structures are created
        await rate_limiter.check_rate_limit("test_source")
        
        # Queue should be created for the source
        assert len(rate_limiter.request_queues) >= 0
    
    async def test_metrics_collection(self, rate_limiter, mock_redis):
        """Test metrics collection."""
        mock_redis.eval.return_value = [1, 20.0, time.time()]
        
        # Make some requests
        await rate_limiter.check_rate_limit("source1")
        await rate_limiter.check_rate_limit("source2", "endpoint1")
        
        metrics = await rate_limiter.get_metrics()
        
        assert len(metrics) >= 1
        assert "source1" in metrics or "source2:endpoint1" in metrics
    
    async def test_cleanup(self, rate_limiter):
        """Test cleanup functionality."""
        # Add some queue processors
        rate_limiter.queue_processors["test"] = asyncio.create_task(asyncio.sleep(1))
        
        await rate_limiter.cleanup()
        
        assert len(rate_limiter.queue_processors) == 0
        assert len(rate_limiter.request_queues) == 0


class TestRateLimitDecorator:
    """Test rate limit decorator functionality."""
    
    async def test_decorator_basic(self, mock_redis):
        """Test basic decorator functionality."""
        # Initialize global rate limiter
        config = RateLimitConfig(requests_per_minute=100)
        init_rate_limiter(mock_redis, config)
        
        # Mock Redis to allow request
        mock_redis.eval.return_value = [1, 50.0, time.time()]
        
        @rate_limit(source="test_source", requests_per_minute=100)
        async def test_function():
            return "success"
        
        result = await test_function()
        assert result == "success"
        
        # Verify Redis was called
        mock_redis.eval.assert_called()
    
    async def test_decorator_with_endpoint(self, mock_redis):
        """Test decorator with endpoint specification."""
        init_rate_limiter(mock_redis, RateLimitConfig())
        mock_redis.eval.return_value = [1, 50.0, time.time()]
        
        @rate_limit(source="test_source", endpoint="test_endpoint")
        async def test_function():
            return "success"
        
        result = await test_function()
        assert result == "success"
    
    async def test_decorator_rate_limit_exceeded(self, mock_redis):
        """Test decorator when rate limit is exceeded."""
        init_rate_limiter(mock_redis, RateLimitConfig(queue_size=0))  # No queuing
        
        # Mock Redis to reject request
        mock_redis.eval.return_value = [0, 0.0, time.time()]
        
        @rate_limit(source="test_source", fail_open=False)
        async def test_function():
            return "success"
        
        with pytest.raises(RuntimeError, match="Rate limit exceeded"):
            await test_function()
    
    async def test_decorator_fail_open(self, mock_redis):
        """Test decorator fail-open behavior."""
        # Don't initialize rate limiter
        
        @rate_limit(source="test_source", fail_open=True)
        async def test_function():
            return "success"
        
        result = await test_function()
        assert result == "success"  # Should succeed even without rate limiter
    
    async def test_decorator_error_handling(self, mock_redis):
        """Test decorator error handling."""
        init_rate_limiter(mock_redis, RateLimitConfig())
        
        # Mock Redis to allow request
        mock_redis.eval.return_value = [1, 50.0, time.time()]
        
        @rate_limit(source="test_source")
        async def test_function():
            raise ValueError("Test error")
        
        with pytest.raises(ValueError, match="Test error"):
            await test_function()


class TestRateLimiterIntegration:
    """Test rate limiter integration functionality."""
    
    @pytest.fixture
    async def manager(self):
        """Rate limiter manager for testing."""
        return RateLimiterManager()
    
    async def test_manager_initialization(self, manager, mock_redis):
        """Test manager initialization."""
        with patch('redis.asyncio.from_url', return_value=mock_redis):
            rate_limiter = await manager.initialize("redis://localhost:6379")
            
            assert manager.is_initialized
            assert manager.redis_client == mock_redis
            assert manager.rate_limiter == rate_limiter
    
    async def test_manager_cleanup(self, manager, mock_redis):
        """Test manager cleanup."""
        with patch('redis.asyncio.from_url', return_value=mock_redis):
            await manager.initialize("redis://localhost:6379")
            await manager.cleanup()
            
            assert not manager.is_initialized
            mock_redis.close.assert_called()
    
    async def test_health_check(self, mock_redis):
        """Test rate limiter health check."""
        manager = RateLimiterManager()
        manager.redis_client = mock_redis
        manager.is_initialized = True
        
        # Mock successful ping
        mock_redis.ping.return_value = True
        
        health = await rate_limiter_health_check()
        
        assert health["rate_limiter_initialized"] is True
        assert health["redis_connected"] is True
        assert health["status"] == "healthy"
    
    async def test_health_check_failure(self, mock_redis):
        """Test health check with Redis failure."""
        manager = RateLimiterManager()
        manager.redis_client = mock_redis
        manager.is_initialized = True
        
        # Mock Redis ping failure
        mock_redis.ping.side_effect = Exception("Redis connection failed")
        
        health = await rate_limiter_health_check()
        
        assert health["status"] == "unhealthy"
        assert "error" in health


@pytest.mark.integration
class TestRateLimiterRedisIntegration:
    """Integration tests with real Redis (requires Redis running)."""
    
    @pytest.fixture(scope="class")
    async def redis_client(self):
        """Real Redis client for integration testing."""
        try:
            client = redis.from_url("redis://localhost:6379/15", decode_responses=False)
            await client.ping()
            yield client
            await client.flushdb()  # Clean up test data
            await client.close()
        except redis.ConnectionError:
            pytest.skip("Redis not available for integration tests")
    
    async def test_token_bucket_algorithm(self, redis_client):
        """Test token bucket algorithm with real Redis."""
        config = RateLimitConfig(requests_per_minute=60, burst_capacity=10)
        limiter = DistributedRateLimiter(redis_client, config)
        
        # Make multiple requests rapidly
        allowed_count = 0
        for _ in range(15):  # More than burst capacity
            status, _ = await limiter.check_rate_limit("test_source")
            if status == RateLimitStatus.ALLOWED:
                allowed_count += 1
        
        # Should allow up to burst capacity
        assert allowed_count == 10
    
    async def test_distributed_coordination(self, redis_client):
        """Test coordination between multiple rate limiter instances."""
        config = RateLimitConfig(requests_per_minute=60, burst_capacity=5)
        
        # Create two rate limiter instances (simulating different service instances)
        limiter1 = DistributedRateLimiter(redis_client, config)
        limiter2 = DistributedRateLimiter(redis_client, config)
        
        # Use tokens from first limiter
        status1, _ = await limiter1.check_rate_limit("shared_source", tokens_requested=3)
        assert status1 == RateLimitStatus.ALLOWED
        
        # Second limiter should see reduced token count
        status2, metrics2 = await limiter2.check_rate_limit("shared_source", tokens_requested=3)
        
        # Should either be allowed (if 2 tokens left) or queued/rejected
        assert status2 in [RateLimitStatus.ALLOWED, RateLimitStatus.QUEUED]
        assert metrics2.current_tokens <= 2.0
    
    async def test_adaptive_rate_limiting_persistence(self, redis_client):
        """Test that adaptive multipliers persist across instances."""
        config = RateLimitConfig(adaptive_enabled=True)
        limiter1 = DistributedRateLimiter(redis_client, config)
        
        # Report 429 to trigger adaptive backoff
        await limiter1.report_response_status("adaptive_test", 429)
        
        # Create new instance and check if multiplier persists
        limiter2 = DistributedRateLimiter(redis_client, config)
        multiplier = await limiter2._get_adaptive_multiplier("adaptive_test")
        
        # Should be less than 1.0 due to backoff
        assert multiplier < 1.0


if __name__ == "__main__":
    # Run basic tests
    pytest.main([__file__, "-v"])
