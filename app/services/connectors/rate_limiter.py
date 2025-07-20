"""
Token bucket rate limiter implementation for API connectors.
"""

import asyncio
import time
from typing import Optional


class TokenBucket:
    """
    Token bucket rate limiter implementation.
    
    Allows burst requests up to capacity while maintaining average rate.
    """
    
    def __init__(self, capacity: int, refill_rate: float):
        """
        Initialize token bucket.
        
        Args:
            capacity: Maximum number of tokens in the bucket
            refill_rate: Number of tokens added per second
        """
        self.capacity = capacity
        self.refill_rate = refill_rate
        self.tokens = capacity
        self.last_refill = time.time()
        self._lock = asyncio.Lock()
    
    async def consume(self, tokens: int = 1) -> bool:
        """
        Try to consume tokens from the bucket.
        
        Args:
            tokens: Number of tokens to consume
            
        Returns:
            True if tokens were consumed, False if insufficient tokens
        """
        async with self._lock:
            self._refill()
            
            if self.tokens >= tokens:
                self.tokens -= tokens
                return True
            
            return False
    
    async def wait_for_tokens(self, tokens: int = 1) -> None:
        """
        Wait until enough tokens are available and consume them.
        
        Args:
            tokens: Number of tokens to consume
        """
        while True:
            async with self._lock:
                self._refill()
                
                if self.tokens >= tokens:
                    self.tokens -= tokens
                    return
                
                # Calculate wait time for next token
                wait_time = (tokens - self.tokens) / self.refill_rate
            
            # Wait outside the lock to allow other operations
            await asyncio.sleep(min(wait_time, 0.1))
    
    def _refill(self) -> None:
        """Refill tokens based on elapsed time."""
        now = time.time()
        elapsed = now - self.last_refill
        
        if elapsed > 0:
            new_tokens = elapsed * self.refill_rate
            self.tokens = min(self.capacity, self.tokens + new_tokens)
            self.last_refill = now
    
    @property
    def available_tokens(self) -> int:
        """Get current number of available tokens."""
        self._refill()
        return int(self.tokens)


class RateLimiter:
    """
    Rate limiter with multiple buckets for different rate limits.
    """
    
    def __init__(self):
        self.buckets: dict[str, TokenBucket] = {}
    
    def add_bucket(self, name: str, capacity: int, refill_rate: float) -> None:
        """Add a named token bucket."""
        self.buckets[name] = TokenBucket(capacity, refill_rate)
    
    async def wait_for_permit(self, bucket_name: str, tokens: int = 1) -> None:
        """Wait for permit from specified bucket."""
        if bucket_name not in self.buckets:
            return  # No rate limit configured
        
        await self.buckets[bucket_name].wait_for_tokens(tokens)
    
    async def try_acquire(self, bucket_name: str, tokens: int = 1) -> bool:
        """Try to acquire permit without waiting."""
        if bucket_name not in self.buckets:
            return True  # No rate limit configured
        
        return await self.buckets[bucket_name].consume(tokens)
