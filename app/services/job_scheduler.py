import asyncio
import logging
from datetime import datetime, timedelta
from typing import Dict, Any, Optional, List
from uuid import UUID

from croniter import croniter
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, update, and_
from redis.asyncio import Redis

from ..core.dependencies import get_database_session, get_redis_dependency
from ..db.models import DataIngestionJob, ScheduledJob
from ..schemas import JobCreate, JobType, ScheduleConfig


logger = logging.getLogger(__name__)


class JobScheduler:
    """Job scheduler for handling recurring and scheduled jobs."""
    
    def __init__(self, db: AsyncSession, redis: Redis):
        self.db = db
        self.redis = redis
        self.is_running = False
        self._scheduler_lock_key = "job_scheduler:lock"
        self._lock_timeout = 60  # seconds
    
    async def start(self):
        """Start the job scheduler."""
        self.is_running = True
        logger.info("Job scheduler started")
        
        while self.is_running:
            try:
                # Acquire distributed lock
                if await self._acquire_lock():
                    try:
                        await self._process_scheduled_jobs()
                    finally:
                        await self._release_lock()
                
                # Wait before next check (every minute)
                await asyncio.sleep(60)
                
            except Exception as e:
                logger.error(f"Error in job scheduler main loop: {e}")
                await asyncio.sleep(60)
    
    async def stop(self):
        """Stop the job scheduler."""
        self.is_running = False
        await self._release_lock()
        logger.info("Job scheduler stopped")
    
    async def create_scheduled_job(
        self,
        name: str,
        job_template: JobCreate,
        schedule_config: ScheduleConfig,
        created_by: Optional[str] = None
    ) -> UUID:
        """Create a new scheduled job."""
        try:
            # Calculate next run time
            next_run = self._calculate_next_run(schedule_config)
            
            scheduled_job = ScheduledJob(
                name=name,
                job_template=job_template.dict(),
                schedule_config=schedule_config.dict(),
                next_run_at=next_run,
                created_by=created_by
            )
            
            self.db.add(scheduled_job)
            await self.db.commit()
            await self.db.refresh(scheduled_job)
            
            logger.info(f"Created scheduled job '{name}' with ID {scheduled_job.id}")
            return scheduled_job.id
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error creating scheduled job: {e}")
            raise
    
    async def update_scheduled_job(
        self,
        job_id: UUID,
        schedule_config: Optional[ScheduleConfig] = None,
        job_template: Optional[JobCreate] = None,
        is_active: Optional[bool] = None
    ):
        """Update a scheduled job."""
        try:
            update_values = {"updated_at": datetime.utcnow()}
            
            if schedule_config:
                update_values["schedule_config"] = schedule_config.dict()
                update_values["next_run_at"] = self._calculate_next_run(schedule_config)
            
            if job_template:
                update_values["job_template"] = job_template.dict()
            
            if is_active is not None:
                update_values["is_active"] = is_active
            
            await self.db.execute(
                update(ScheduledJob)
                .where(ScheduledJob.id == job_id)
                .values(**update_values)
            )
            await self.db.commit()
            
            logger.info(f"Updated scheduled job {job_id}")
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error updating scheduled job {job_id}: {e}")
            raise
    
    async def delete_scheduled_job(self, job_id: UUID):
        """Delete a scheduled job."""
        try:
            await self.db.execute(
                update(ScheduledJob)
                .where(ScheduledJob.id == job_id)
                .values(is_active=False, updated_at=datetime.utcnow())
            )
            await self.db.commit()
            
            logger.info(f"Deleted scheduled job {job_id}")
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error deleting scheduled job {job_id}: {e}")
            raise
    
    async def get_scheduled_jobs(
        self,
        active_only: bool = True,
        limit: int = 100
    ) -> List[ScheduledJob]:
        """Get scheduled jobs."""
        try:
            query = select(ScheduledJob).order_by(ScheduledJob.next_run_at.asc())
            
            if active_only:
                query = query.where(ScheduledJob.is_active == True)
            
            query = query.limit(limit)
            
            result = await self.db.execute(query)
            return result.scalars().all()
            
        except Exception as e:
            logger.error(f"Error getting scheduled jobs: {e}")
            return []
    
    async def _acquire_lock(self) -> bool:
        """Acquire distributed lock for scheduler."""
        try:
            # Use Redis SET with NX (not exists) and EX (expiration)
            return await self.redis.set(
                self._scheduler_lock_key,
                datetime.utcnow().isoformat(),
                nx=True,
                ex=self._lock_timeout
            )
        except Exception as e:
            logger.error(f"Error acquiring scheduler lock: {e}")
            return False
    
    async def _release_lock(self):
        """Release distributed lock."""
        try:
            await self.redis.delete(self._scheduler_lock_key)
        except Exception as e:
            logger.error(f"Error releasing scheduler lock: {e}")
    
    async def _process_scheduled_jobs(self):
        """Process scheduled jobs that are due."""
        try:
            now = datetime.utcnow()
            
            # Get jobs due for execution
            query = select(ScheduledJob).where(
                and_(
                    ScheduledJob.is_active == True,
                    ScheduledJob.next_run_at <= now
                )
            ).order_by(ScheduledJob.next_run_at.asc())
            
            result = await self.db.execute(query)
            due_jobs = result.scalars().all()
            
            for scheduled_job in due_jobs:
                try:
                    await self._execute_scheduled_job(scheduled_job)
                except Exception as e:
                    logger.error(f"Error executing scheduled job {scheduled_job.id}: {e}")
            
        except Exception as e:
            logger.error(f"Error processing scheduled jobs: {e}")
    
    async def _execute_scheduled_job(self, scheduled_job: ScheduledJob):
        """Execute a scheduled job."""
        try:
            # Create job from template
            job_template = JobCreate(**scheduled_job.job_template)
            
            # Create data ingestion job
            job = DataIngestionJob(
                job_type=job_template.job_type,
                source_type=job_template.source_type,
                source_config=job_template.source_config,
                destination_type=job_template.destination_type,
                destination_config=job_template.destination_config,
                batch_size=job_template.batch_size,
                retry_count=job_template.retry_count,
                priority=job_template.priority,
                dependencies=job_template.dependencies,
                metadata={
                    "scheduled_job_id": str(scheduled_job.id),
                    "scheduled_job_name": scheduled_job.name,
                    "scheduled_run": True
                },
                status="queued"
            )
            
            self.db.add(job)
            
            # Update scheduled job
            schedule_config = ScheduleConfig(**scheduled_job.schedule_config)
            next_run = self._calculate_next_run(schedule_config)
            run_count = scheduled_job.run_count + 1
            
            # Check if max runs reached
            is_active = scheduled_job.is_active
            if schedule_config.max_runs and run_count >= schedule_config.max_runs:
                is_active = False
                next_run = None
            
            await self.db.execute(
                update(ScheduledJob)
                .where(ScheduledJob.id == scheduled_job.id)
                .values(
                    last_run_at=datetime.utcnow(),
                    next_run_at=next_run,
                    run_count=run_count,
                    is_active=is_active,
                    updated_at=datetime.utcnow()
                )
            )
            
            await self.db.commit()
            
            logger.info(
                f"Executed scheduled job '{scheduled_job.name}' "
                f"(run {run_count}), created job {job.id}"
            )
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error executing scheduled job {scheduled_job.id}: {e}")
            raise
    
    def _calculate_next_run(self, schedule_config: ScheduleConfig) -> Optional[datetime]:
        """Calculate next run time based on schedule configuration."""
        if not schedule_config.enabled:
            return None
        
        now = datetime.utcnow()
        
        if schedule_config.cron_expression:
            # Use cron expression
            try:
                cron = croniter(schedule_config.cron_expression, now)
                return cron.get_next(datetime)
            except Exception as e:
                logger.error(f"Error parsing cron expression '{schedule_config.cron_expression}': {e}")
                return None
        
        elif schedule_config.interval_seconds:
            # Use interval
            return now + timedelta(seconds=schedule_config.interval_seconds)
        
        elif schedule_config.next_run:
            # Use specific next run time
            return schedule_config.next_run
        
        return None


# Global scheduler instance
_job_scheduler: Optional[JobScheduler] = None


async def get_job_scheduler() -> JobScheduler:
    """Get job scheduler instance."""
    global _job_scheduler
    
    if _job_scheduler is None:
        db = next(get_database_session())
        redis = await get_redis_dependency()
        _job_scheduler = JobScheduler(db, redis)
    
    return _job_scheduler


async def start_job_scheduler():
    """Start the job scheduler background task."""
    scheduler = await get_job_scheduler()
    return asyncio.create_task(scheduler.start())


async def stop_job_scheduler():
    """Stop the job scheduler."""
    global _job_scheduler
    if _job_scheduler:
        await _job_scheduler.stop()
        _job_scheduler = None
