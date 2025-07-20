import logging
from uuid import UUID, uuid4
from typing import List, Optional, Dict, Any
from datetime import datetime, timedelta

from fastapi import APIRouter, Depends, HTTPException, status, Query, BackgroundTasks
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, update, and_, func, desc
from redis.asyncio import Redis

from ....core.dependencies import get_database_session, get_redis_dependency
from ....db.models import DataIngestionJob, JobLog, RawMaterialsData, ScheduledJob
from ....schemas import (
    JobCreate, JobResponse, JobStatus, JobProgress, JobType, JobPriority,
    JobLogResponse, PaginatedResponse, QueueStats, JobStats, ScheduleConfig
)
from ....services.connectors.redis_connector import get_job_queue
from ....services.job_scheduler import get_job_scheduler

logger = logging.getLogger(__name__)
router = APIRouter()


@router.post("/", response_model=JobResponse, status_code=status.HTTP_201_CREATED)
async def create_job(
    job_data: JobCreate,
    background_tasks: BackgroundTasks,
    db: AsyncSession = Depends(get_database_session),
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Create a new enhanced data ingestion job.
    """
    try:
        # Validate dependencies if provided
        if job_data.dependencies:
            for dep_id in job_data.dependencies:
                dep_result = await db.execute(
                    select(DataIngestionJob).where(DataIngestionJob.id == dep_id)
                )
                if not dep_result.scalar_one_or_none():
                    raise HTTPException(
                        status_code=status.HTTP_400_BAD_REQUEST,
                        detail=f"Dependency job {dep_id} not found"
                    )
        
        # Create enhanced job in database
        job = DataIngestionJob(
            id=uuid4(),
            job_type=job_data.job_type,
            source_type=job_data.source_type,
            source_config=job_data.source_config,
            destination_type=job_data.destination_type,
            destination_config=job_data.destination_config,
            batch_size=job_data.batch_size,
            retry_count=job_data.retry_count,
            priority=job_data.priority,
            dependencies=job_data.dependencies,
            schedule_config=job_data.schedule_config,
            metadata=job_data.metadata,
            status="created"
        )
        
        db.add(job)
        await db.commit()
        await db.refresh(job)
        
        # Handle scheduled job
        if job_data.schedule_config:
            scheduler = await get_job_scheduler()
            schedule_config = ScheduleConfig(**job_data.schedule_config)
            await scheduler.create_scheduled_job(
                name=f"Scheduled {job_data.job_type} - {job.id}",
                job_template=job_data,
                schedule_config=schedule_config
            )
            logger.info(f"Created scheduled job for {job.id}")
        else:
            # Enqueue job for immediate processing
            job_queue = await get_job_queue()
            success = await job_queue.enqueue_job(
                job_id=str(job.id),
                job_type=job_data.job_type,
                payload={
                    "job_type": job_data.job_type,
                    "source_type": job_data.source_type,
                    "source_config": job_data.source_config,
                    "destination_type": job_data.destination_type,
                    "destination_config": job_data.destination_config,
                    "batch_size": job_data.batch_size,
                    "metadata": job_data.metadata
                },
                priority=job_data.priority
            )
            
            if success:
                # Update job status to queued
                await db.execute(
                    update(DataIngestionJob)
                    .where(DataIngestionJob.id == job.id)
                    .values(status="queued")
                )
                await db.commit()
                job.status = "queued"
        
        logger.info(f"Created job {job.id} with type {job_data.job_type} and status {job.status}")
        return JobResponse.from_orm(job)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to create job: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to create job: {str(e)}"
        )


@router.get("/", response_model=List[JobResponse])
async def list_jobs(
    skip: int = Query(0, ge=0),
    limit: int = Query(100, ge=1, le=1000),
    status_filter: Optional[str] = Query(None, description="Filter by job status"),
    job_type_filter: Optional[JobType] = Query(None, description="Filter by job type"),
    source_type_filter: Optional[str] = Query(None, description="Filter by source type"),
    priority_filter: Optional[JobPriority] = Query(None, description="Filter by priority"),
    db: AsyncSession = Depends(get_database_session)
):
    """
    List data ingestion jobs with enhanced filtering.
    """
    try:
        query = select(DataIngestionJob)
        
        # Apply filters
        if status_filter:
            query = query.where(DataIngestionJob.status == status_filter)
        
        if job_type_filter:
            query = query.where(DataIngestionJob.job_type == job_type_filter)
        
        if source_type_filter:
            query = query.where(DataIngestionJob.source_type == source_type_filter)
        
        if priority_filter:
            query = query.where(DataIngestionJob.priority == priority_filter)
        
        query = query.offset(skip).limit(limit).order_by(DataIngestionJob.created_at.desc())
        
        result = await db.execute(query)
        jobs = result.scalars().all()
        
        return [JobResponse.from_orm(job) for job in jobs]
        
    except Exception as e:
        logger.error(f"Failed to list jobs: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to list jobs: {str(e)}"
        )


@router.get("/{job_id}", response_model=JobResponse)
async def get_job(
    job_id: UUID,
    db: AsyncSession = Depends(get_database_session)
):
    """
    Get a specific job by ID.
    """
    try:
        result = await db.execute(
            select(DataIngestionJob).where(DataIngestionJob.id == job_id)
        )
        job = result.scalar_one_or_none()
        
        if not job:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Job {job_id} not found"
            )
        
        return JobResponse.from_orm(job)
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get job {job_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get job: {str(e)}"
        )


@router.get("/{job_id}/status", response_model=JobStatus)
async def get_job_status(
    job_id: UUID,
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Get real-time job status from Redis.
    """
    try:
        job_queue = await get_job_queue()
        status_data = await job_queue.get_job_status(str(job_id))
        
        if not status_data:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Job status for {job_id} not found"
            )
        
        return JobStatus(
            job_id=job_id,
            status=status_data.get("status", "unknown"),
            progress=int(status_data.get("progress", 0)),
            message=status_data.get("message"),
            updated_at=status_data.get("updated_at")
        )
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get job status {job_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get job status: {str(e)}"
        )


@router.put("/{job_id}/progress")
async def update_job_progress(
    job_id: UUID,
    progress_data: JobProgress,
    db: AsyncSession = Depends(get_database_session),
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Update job progress with enhanced tracking.
    """
    try:
        # Update database
        update_values = {
            "processed_records": progress_data.processed_records,
            "total_records": progress_data.total_records,
            "error_count": progress_data.error_count,
            "updated_at": datetime.utcnow()
        }
        
        if progress_data.status:
            update_values["status"] = progress_data.status
        
        if progress_data.processing_rate:
            update_values["processing_rate"] = progress_data.processing_rate
        
        if progress_data.estimated_completion:
            update_values["estimated_completion"] = progress_data.estimated_completion
        
        # Calculate progress percentage
        if progress_data.total_records > 0:
            progress_pct = int((progress_data.processed_records / progress_data.total_records) * 100)
            update_values["progress"] = progress_pct
        
        await db.execute(
            update(DataIngestionJob)
            .where(DataIngestionJob.id == job_id)
            .values(**update_values)
        )
        await db.commit()
        
        # Update Redis status
        job_queue = await get_job_queue()
        await job_queue.set_job_status(
            str(job_id),
            progress_data.status or "processing",
            {
                "progress": progress_pct if progress_data.total_records > 0 else 0,
                "message": progress_data.message,
                "processed_records": progress_data.processed_records,
                "total_records": progress_data.total_records,
                "error_count": progress_data.error_count,
                "processing_rate": progress_data.processing_rate,
                "estimated_completion": progress_data.estimated_completion.isoformat() if progress_data.estimated_completion else None,
                "current_batch": progress_data.current_batch
            }
        )
        
        return {"status": "updated", "job_id": job_id}
        
    except Exception as e:
        logger.error(f"Failed to update job progress {job_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to update job progress: {str(e)}"
        )


@router.get("/{job_id}/logs", response_model=List[JobLogResponse])
async def get_job_logs(
    job_id: UUID,
    skip: int = Query(0, ge=0),
    limit: int = Query(100, ge=1, le=1000),
    level: Optional[str] = Query(None, description="Filter by log level"),
    db: AsyncSession = Depends(get_database_session)
):
    """
    Get logs for a specific job.
    """
    try:
        query = select(JobLog).where(JobLog.job_id == job_id)
        
        if level:
            query = query.where(JobLog.level == level.upper())
        
        query = query.offset(skip).limit(limit).order_by(JobLog.timestamp.desc())
        
        result = await db.execute(query)
        logs = result.scalars().all()
        
        return [JobLogResponse.from_orm(log) for log in logs]
        
    except Exception as e:
        logger.error(f"Failed to get job logs {job_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get job logs: {str(e)}"
        )


@router.delete("/{job_id}", status_code=status.HTTP_204_NO_CONTENT)
async def cancel_job(
    job_id: UUID,
    db: AsyncSession = Depends(get_database_session),
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Cancel a job.
    """
    try:
        # Update job status in database
        result = await db.execute(
            update(DataIngestionJob)
            .where(
                and_(
                    DataIngestionJob.id == job_id,
                    DataIngestionJob.status.in_(["created", "queued", "processing"])
                )
            )
            .values(status="cancelled")
        )
        
        if result.rowcount == 0:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND,
                detail=f"Job {job_id} not found or cannot be cancelled"
            )
        
        await db.commit()
        
        # Update Redis status
        job_queue = await get_job_queue()
        await job_queue.set_job_status(
            str(job_id),
            "cancelled",
            {"message": "Job cancelled by user"}
        )
        
        logger.info(f"Cancelled job {job_id}")
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to cancel job {job_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to cancel job: {str(e)}"
        )


@router.get("/stats", response_model=JobStats)
async def get_job_statistics(
    hours: int = Query(24, ge=1, le=168, description="Hours to look back for statistics"),
    db: AsyncSession = Depends(get_database_session)
):
    """
    Get job execution statistics.
    """
    try:
        since = datetime.utcnow() - timedelta(hours=hours)
        
        # Count jobs by status
        status_query = select(
            DataIngestionJob.status,
            func.count(DataIngestionJob.id).label("count")
        ).where(
            DataIngestionJob.created_at >= since
        ).group_by(DataIngestionJob.status)
        
        result = await db.execute(status_query)
        status_counts = {row.status: row.count for row in result}
        
        # Calculate success rate
        completed = status_counts.get("completed", 0)
        failed = status_counts.get("failed", 0)
        total_finished = completed + failed
        success_rate = (completed / total_finished * 100) if total_finished > 0 else None
        
        # Calculate average processing time
        avg_time_query = select(
            func.avg(
                func.extract('epoch', DataIngestionJob.completed_at - DataIngestionJob.started_at)
            ).label("avg_seconds")
        ).where(
            and_(
                DataIngestionJob.status == "completed",
                DataIngestionJob.started_at.isnot(None),
                DataIngestionJob.completed_at.isnot(None),
                DataIngestionJob.created_at >= since
            )
        )
        
        avg_result = await db.execute(avg_time_query)
        avg_processing_time = avg_result.scalar()
        
        return JobStats(
            total_jobs=sum(status_counts.values()),
            queued_jobs=status_counts.get("queued", 0),
            processing_jobs=status_counts.get("processing", 0),
            completed_jobs=status_counts.get("completed", 0),
            failed_jobs=status_counts.get("failed", 0),
            cancelled_jobs=status_counts.get("cancelled", 0),
            avg_processing_time=avg_processing_time,
            success_rate=success_rate
        )
        
    except Exception as e:
        logger.error(f"Failed to get job statistics: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get job statistics: {str(e)}"
        )


@router.get("/{job_id}/materials", response_model=List[Dict])
async def get_job_materials(
    job_id: UUID,
    skip: int = Query(0, ge=0),
    limit: int = Query(100, ge=1, le=1000),
    processing_status: Optional[str] = Query(None, description="Filter by processing status"),
    db: AsyncSession = Depends(get_database_session)
):
    """
    Get materials data fetched by a job.
    """
    try:
        query = select(RawMaterialsData).where(RawMaterialsData.job_id == job_id)
        
        if processing_status:
            query = query.where(RawMaterialsData.processing_status == processing_status)
        
        query = query.offset(skip).limit(limit).order_by(RawMaterialsData.created_at.desc())
        
        result = await db.execute(query)
        materials = result.scalars().all()
        
        return [
            {
                "id": str(material.id),
                "source_db": material.source_db,
                "source_id": material.source_id,
                "material_formula": material.material_formula,
                "processing_status": material.processing_status,
                "created_at": material.created_at,
                "standardized_data": material.standardized_data,
                "metadata": material.metadata
            }
            for material in materials
        ]
        
    except Exception as e:
        logger.error(f"Failed to get job materials {job_id}: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to get job materials: {str(e)}"
        )


@router.post("/bulk/create", response_model=List[JobResponse])
async def create_bulk_jobs(
    jobs_data: List[JobCreate],
    background_tasks: BackgroundTasks,
    db: AsyncSession = Depends(get_database_session),
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Create multiple jobs in bulk.
    """
    try:
        if len(jobs_data) > 100:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Cannot create more than 100 jobs at once"
            )
        
        created_jobs = []
        job_queue = await get_job_queue()
        
        for job_data in jobs_data:
            # Create job
            job = DataIngestionJob(
                id=uuid4(),
                job_type=job_data.job_type,
                source_type=job_data.source_type,
                source_config=job_data.source_config,
                destination_type=job_data.destination_type,
                destination_config=job_data.destination_config,
                batch_size=job_data.batch_size,
                retry_count=job_data.retry_count,
                priority=job_data.priority,
                dependencies=job_data.dependencies,
                metadata=job_data.metadata,
                status="created"
            )
            
            db.add(job)
            created_jobs.append(job)
        
        await db.commit()
        
        # Enqueue jobs
        for job in created_jobs:
            if not job.schedule_config:  # Only enqueue non-scheduled jobs
                await job_queue.enqueue_job(
                    job_id=str(job.id),
                    job_type=job.job_type,
                    payload={
                        "job_type": job.job_type,
                        "source_type": job.source_type,
                        "source_config": job.source_config,
                        "destination_type": job.destination_type,
                        "destination_config": job.destination_config,
                        "batch_size": job.batch_size,
                        "metadata": job.metadata
                    },
                    priority=job.priority
                )
                job.status = "queued"
        
        # Update queued jobs
        queued_ids = [job.id for job in created_jobs if job.status == "queued"]
        if queued_ids:
            await db.execute(
                update(DataIngestionJob)
                .where(DataIngestionJob.id.in_(queued_ids))
                .values(status="queued")
            )
            await db.commit()
        
        logger.info(f"Created {len(created_jobs)} jobs in bulk")
        return [JobResponse.from_orm(job) for job in created_jobs]
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to create bulk jobs: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to create bulk jobs: {str(e)}"
        )


@router.post("/bulk/cancel")
async def cancel_bulk_jobs(
    job_ids: List[UUID],
    db: AsyncSession = Depends(get_database_session),
    redis: Redis = Depends(get_redis_dependency)
):
    """
    Cancel multiple jobs in bulk.
    """
    try:
        if len(job_ids) > 100:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Cannot cancel more than 100 jobs at once"
            )
        
        # Update job statuses
        result = await db.execute(
            update(DataIngestionJob)
            .where(
                and_(
                    DataIngestionJob.id.in_(job_ids),
                    DataIngestionJob.status.in_(["created", "queued", "processing"])
                )
            )
            .values(status="cancelled", updated_at=datetime.utcnow())
        )
        
        cancelled_count = result.rowcount
        await db.commit()
        
        # Update Redis status
        job_queue = await get_job_queue()
        for job_id in job_ids:
            await job_queue.set_job_status(
                str(job_id),
                "cancelled",
                {"message": "Job cancelled by bulk operation"}
            )
        
        logger.info(f"Cancelled {cancelled_count} jobs in bulk")
        return {
            "status": "completed",
            "cancelled_count": cancelled_count,
            "requested_count": len(job_ids)
        }
        
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to cancel bulk jobs: {e}")
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to cancel bulk jobs: {str(e)}"
        )
