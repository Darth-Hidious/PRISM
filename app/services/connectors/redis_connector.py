import json
import logging
from typing import Any, Optional, Dict
from datetime import datetime, timedelta

import redis.asyncio as redis
from redis.asyncio import Redis

from ...core.config import get_settings


logger = logging.getLogger(__name__)


class RedisManager:
    """Redis connection manager."""
    
    def __init__(self):
        self.settings = get_settings()
        self._client: Optional[Redis] = None
    
    async def get_client(self) -> Redis:
        """Get or create Redis client."""
        if self._client is None:
            self._client = redis.from_url(
                self.settings.redis_url,
                decode_responses=self.settings.redis_decode_responses,
                retry_on_timeout=True,
                socket_connect_timeout=5,
                socket_timeout=5
            )
            # Test connection
            await self._client.ping()
            logger.info("Redis client connected successfully")
        
        return self._client
    
    async def close(self):
        """Close Redis connection."""
        if self._client:
            await self._client.close()
            logger.info("Redis connection closed")


class JobQueue:
    """Job queue manager using Redis."""
    
    def __init__(self, redis_client: Redis):
        self.redis = redis_client
        self.job_queue_key = "job_queue"
        self.job_status_prefix = "job_status:"
        self.job_result_prefix = "job_result:"
    
    async def enqueue_job(
        self,
        job_id: str,
        job_type: str,
        payload: Dict[str, Any],
        priority: int = 0,
        delay: Optional[timedelta] = None
    ) -> bool:
        """Enqueue a job for processing."""
        try:
            job_data = {
                "job_id": job_id,
                "job_type": job_type,
                "payload": payload,
                "priority": priority,
                "created_at": datetime.utcnow().isoformat(),
                "status": "queued"
            }
            
            if delay:
                # Schedule job for later execution
                execute_at = datetime.utcnow() + delay
                await self.redis.zadd(
                    f"{self.job_queue_key}:delayed",
                    {json.dumps(job_data): execute_at.timestamp()}
                )
            else:
                # Add to immediate queue with priority
                await self.redis.zadd(
                    self.job_queue_key,
                    {json.dumps(job_data): priority}
                )
            
            # Set job status
            await self.set_job_status(job_id, "queued", job_data)
            
            logger.info(f"Job {job_id} enqueued successfully")
            return True
            
        except Exception as e:
            logger.error(f"Failed to enqueue job {job_id}: {e}")
            return False
    
    async def dequeue_job(self) -> Optional[Dict[str, Any]]:
        """Dequeue the highest priority job."""
        try:
            # Check for delayed jobs that are ready
            await self._process_delayed_jobs()
            
            # Get highest priority job
            result = await self.redis.zpopmax(self.job_queue_key)
            if result:
                job_data_str, priority = result[0]
                job_data = json.loads(job_data_str)
                
                # Update job status
                await self.set_job_status(
                    job_data["job_id"],
                    "processing",
                    {"started_at": datetime.utcnow().isoformat()}
                )
                
                return job_data
            
            return None
            
        except Exception as e:
            logger.error(f"Failed to dequeue job: {e}")
            return None
    
    async def _process_delayed_jobs(self):
        """Move delayed jobs to main queue if they're ready."""
        now = datetime.utcnow().timestamp()
        delayed_jobs = await self.redis.zrangebyscore(
            f"{self.job_queue_key}:delayed",
            0,
            now,
            withscores=True
        )
        
        if delayed_jobs:
            pipe = self.redis.pipeline()
            for job_data_str, _ in delayed_jobs:
                job_data = json.loads(job_data_str)
                # Move to main queue
                pipe.zadd(self.job_queue_key, {job_data_str: job_data["priority"]})
                # Remove from delayed queue
                pipe.zrem(f"{self.job_queue_key}:delayed", job_data_str)
            
            await pipe.execute()
    
    async def set_job_status(
        self,
        job_id: str,
        status: str,
        metadata: Optional[Dict[str, Any]] = None
    ):
        """Set job status and metadata."""
        status_data = {
            "status": status,
            "updated_at": datetime.utcnow().isoformat()
        }
        
        if metadata:
            status_data.update(metadata)
        
        await self.redis.hset(
            f"{self.job_status_prefix}{job_id}",
            mapping=status_data
        )
        
        # Set expiration for completed/failed jobs
        if status in ["completed", "failed"]:
            await self.redis.expire(f"{self.job_status_prefix}{job_id}", 86400)  # 24 hours
    
    async def get_job_status(self, job_id: str) -> Optional[Dict[str, Any]]:
        """Get job status and metadata."""
        try:
            status_data = await self.redis.hgetall(f"{self.job_status_prefix}{job_id}")
            return status_data if status_data else None
        except Exception as e:
            logger.error(f"Failed to get job status for {job_id}: {e}")
            return None
    
    async def set_job_result(self, job_id: str, result: Dict[str, Any]):
        """Store job result."""
        await self.redis.setex(
            f"{self.job_result_prefix}{job_id}",
            86400,  # 24 hours
            json.dumps(result)
        )
    
    async def get_job_result(self, job_id: str) -> Optional[Dict[str, Any]]:
        """Get job result."""
        try:
            result_str = await self.redis.get(f"{self.job_result_prefix}{job_id}")
            return json.loads(result_str) if result_str else None
        except Exception as e:
            logger.error(f"Failed to get job result for {job_id}: {e}")
            return None
    
    async def get_queue_stats(self) -> Dict[str, int]:
        """Get queue statistics."""
        try:
            pending_count = await self.redis.zcard(self.job_queue_key)
            delayed_count = await self.redis.zcard(f"{self.job_queue_key}:delayed")
            
            return {
                "pending_jobs": pending_count,
                "delayed_jobs": delayed_count,
                "total_jobs": pending_count + delayed_count
            }
        except Exception as e:
            logger.error(f"Failed to get queue stats: {e}")
            return {"pending_jobs": 0, "delayed_jobs": 0, "total_jobs": 0}


# Global Redis manager
redis_manager = RedisManager()


async def get_redis_client() -> Redis:
    """Get Redis client."""
    return await redis_manager.get_client()


async def get_job_queue() -> JobQueue:
    """Get job queue instance."""
    redis_client = await get_redis_client()
    return JobQueue(redis_client)


async def close_redis():
    """Close Redis connections."""
    await redis_manager.close()
