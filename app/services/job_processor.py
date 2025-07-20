import asyncio
import logging
import traceback
from datetime import datetime, timedelta
from typing import Dict, Any, Optional, List
from uuid import UUID

from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, update, and_
from redis.asyncio import Redis

from ..core.dependencies import get_database_session, get_redis_dependency
from ..db.models import DataIngestionJob, JobLog, RawMaterialsData, JobDependency
from ..schemas import JobType, JobProgress
from ..services.connectors.base_connector import DatabaseConnector, StandardizedMaterial
from ..services.connectors.jarvis_connector import JarvisConnector
from ..services.connectors.nomad_connector import NOMADConnector
from ..services.rate_limiter_integration import RateLimiterManager


logger = logging.getLogger(__name__)


class ConnectorRegistry:
    """Registry for database connectors."""
    
    _connectors: Dict[str, type] = {
        "jarvis": JarvisConnector,
        "nomad": NOMADConnector,
        # Add other connectors here as they're implemented
        # "materials_project": MaterialsProjectConnector,
        # "aflow": AFLOWConnector,
    }
    
    @classmethod
    def get_connector_class(cls, source_type: str) -> Optional[type]:
        """Get connector class for source type."""
        return cls._connectors.get(source_type.lower())
    
    @classmethod
    def register_connector(cls, source_type: str, connector_class: type):
        """Register a new connector."""
        cls._connectors[source_type.lower()] = connector_class


class JobProcessor:
    """Enhanced job processor with batch processing, progress tracking, and error handling."""
    
    def __init__(self, db: AsyncSession, redis: Redis, rate_limiter_manager: RateLimiterManager):
        self.db = db
        self.redis = redis
        self.rate_limiter_manager = rate_limiter_manager
        self.is_running = False
        self._connectors: Dict[str, DatabaseConnector] = {}
    
    async def start(self):
        """Start the job processor."""
        self.is_running = True
        logger.info("Job processor started")
        
        while self.is_running:
            try:
                # Process scheduled jobs
                await self._process_scheduled_jobs()
                
                # Process regular jobs
                await self._process_jobs()
                
                # Wait before next iteration
                await asyncio.sleep(5)
                
            except Exception as e:
                logger.error(f"Error in job processor main loop: {e}")
                await asyncio.sleep(10)  # Wait longer on error
    
    async def stop(self):
        """Stop the job processor."""
        self.is_running = False
        
        # Close all connectors
        for connector in self._connectors.values():
            try:
                await connector.disconnect()
            except Exception as e:
                logger.error(f"Error closing connector: {e}")
        
        logger.info("Job processor stopped")
    
    async def _process_scheduled_jobs(self):
        """Process scheduled jobs that are due."""
        try:
            # This would typically query ScheduledJob table for due jobs
            # For now, we'll focus on the main job processing
            pass
        except Exception as e:
            logger.error(f"Error processing scheduled jobs: {e}")
    
    async def _process_jobs(self):
        """Process pending jobs from the database."""
        try:
            # Get next job with dependency resolution
            job = await self._get_next_job()
            if not job:
                return
            
            await self._log_job_event(job.id, "INFO", f"Starting job processing: {job.job_type}")
            
            try:
                # Update job status
                await self._update_job_status(job.id, "processing", started_at=datetime.utcnow())
                
                # Process based on job type
                if job.job_type == JobType.FETCH_SINGLE_MATERIAL:
                    await self._process_single_material_job(job)
                elif job.job_type == JobType.BULK_FETCH_BY_FORMULA:
                    await self._process_bulk_formula_job(job)
                elif job.job_type == JobType.BULK_FETCH_BY_PROPERTIES:
                    await self._process_bulk_properties_job(job)
                elif job.job_type == JobType.SYNC_DATABASE:
                    await self._process_sync_database_job(job)
                else:
                    # Legacy data ingestion job
                    await self._process_legacy_job(job)
                
                # Mark job as completed
                await self._update_job_status(
                    job.id, 
                    "completed", 
                    completed_at=datetime.utcnow(),
                    progress=100
                )
                await self._log_job_event(job.id, "INFO", "Job completed successfully")
                
            except Exception as e:
                await self._handle_job_error(job, e)
                
        except Exception as e:
            logger.error(f"Error processing jobs: {e}")
    
    async def _get_next_job(self) -> Optional[DataIngestionJob]:
        """Get the next job to process, considering dependencies."""
        try:
            # Get jobs with dependencies resolved
            query = select(DataIngestionJob).where(
                and_(
                    DataIngestionJob.status == "queued",
                    # Add dependency resolution logic here
                )
            ).order_by(
                DataIngestionJob.priority.desc(),
                DataIngestionJob.created_at.asc()
            ).limit(1)
            
            result = await self.db.execute(query)
            job = result.scalar_one_or_none()
            
            if job and await self._check_dependencies(job):
                return job
            
            return None
            
        except Exception as e:
            logger.error(f"Error getting next job: {e}")
            return None
    
    async def _check_dependencies(self, job: DataIngestionJob) -> bool:
        """Check if job dependencies are satisfied."""
        if not job.dependencies:
            return True
        
        try:
            for dep_job_id in job.dependencies:
                dep_query = select(DataIngestionJob).where(DataIngestionJob.id == dep_job_id)
                dep_result = await self.db.execute(dep_query)
                dep_job = dep_result.scalar_one_or_none()
                
                if not dep_job or dep_job.status != "completed":
                    return False
            
            return True
            
        except Exception as e:
            logger.error(f"Error checking dependencies for job {job.id}: {e}")
            return False
    
    async def _get_connector(self, source_type: str, config: Dict[str, Any]) -> DatabaseConnector:
        """Get or create connector for source type."""
        connector_key = f"{source_type}_{hash(str(config))}"
        
        if connector_key not in self._connectors:
            connector_class = ConnectorRegistry.get_connector_class(source_type)
            if not connector_class:
                raise ValueError(f"No connector found for source type: {source_type}")
            
            # Create connector with rate limiting
            rate_limiter = await self.rate_limiter_manager.get_rate_limiter(source_type)
            connector = connector_class(config, rate_limiter=rate_limiter)
            await connector.connect()
            
            self._connectors[connector_key] = connector
        
        return self._connectors[connector_key]
    
    async def _process_single_material_job(self, job: DataIngestionJob):
        """Process single material fetch job."""
        material_id = job.source_config.get("material_id")
        if not material_id:
            raise ValueError("material_id not found in source_config")
        
        connector = await self._get_connector(job.source_type, job.source_config)
        
        # Update progress
        await self._update_progress(job.id, 0, 1, "Fetching material data")
        
        # Fetch material
        material = await connector.get_material_by_id(material_id)
        if not material:
            raise ValueError(f"Material {material_id} not found")
        
        # Store in raw_materials_data
        await self._store_material_data(job.id, [material])
        
        # Update progress
        await self._update_progress(job.id, 1, 1, "Material data fetched successfully")
    
    async def _process_bulk_formula_job(self, job: DataIngestionJob):
        """Process bulk fetch by formula job."""
        formulas = job.source_config.get("formulas", [])
        formula_pattern = job.source_config.get("formula_pattern")
        
        connector = await self._get_connector(job.source_type, job.source_config)
        
        if formulas:
            total_formulas = len(formulas)
            await self._update_progress(job.id, 0, total_formulas, "Starting bulk fetch by formulas")
            
            all_materials = []
            processed = 0
            
            for i in range(0, len(formulas), job.batch_size):
                batch_formulas = formulas[i:i + job.batch_size]
                
                for formula in batch_formulas:
                    try:
                        materials = await connector.search_materials(formula=formula)
                        all_materials.extend(materials)
                        processed += 1
                        
                        # Update progress
                        rate = processed / ((datetime.utcnow() - job.started_at).total_seconds() or 1)
                        await self._update_progress(
                            job.id, 
                            processed, 
                            total_formulas, 
                            f"Processed {processed}/{total_formulas} formulas",
                            processing_rate=rate
                        )
                        
                    except Exception as e:
                        await self._log_job_event(job.id, "WARNING", f"Error fetching formula {formula}: {e}")
                
                # Store batch
                if all_materials:
                    await self._store_material_data(job.id, all_materials)
                    all_materials = []
        
        elif formula_pattern:
            # Handle pattern-based search
            materials = await connector.search_materials(formula_pattern=formula_pattern)
            await self._store_material_data(job.id, materials)
    
    async def _process_bulk_properties_job(self, job: DataIngestionJob):
        """Process bulk fetch by properties job."""
        property_filters = job.source_config.get("property_filters", {})
        
        connector = await self._get_connector(job.source_type, job.source_config)
        
        await self._update_progress(job.id, 0, 1, "Starting bulk fetch by properties")
        
        # Fetch materials with property filters
        materials = await connector.search_materials(**property_filters)
        
        if materials:
            total_materials = len(materials)
            await self._update_progress(job.id, 0, total_materials, f"Found {total_materials} materials")
            
            # Process in batches
            for i in range(0, len(materials), job.batch_size):
                batch = materials[i:i + job.batch_size]
                await self._store_material_data(job.id, batch)
                
                processed = min(i + job.batch_size, total_materials)
                rate = processed / ((datetime.utcnow() - job.started_at).total_seconds() or 1)
                await self._update_progress(
                    job.id, 
                    processed, 
                    total_materials, 
                    f"Stored {processed}/{total_materials} materials",
                    processing_rate=rate
                )
    
    async def _process_sync_database_job(self, job: DataIngestionJob):
        """Process database sync job."""
        dataset = job.source_config.get("dataset")
        if not dataset:
            raise ValueError("dataset not specified in source_config")
        
        connector = await self._get_connector(job.source_type, job.source_config)
        
        await self._update_progress(job.id, 0, 1, f"Starting database sync for {dataset}")
        
        # Fetch all materials for the dataset
        materials = await connector.fetch_bulk_materials(dataset=dataset)
        
        if materials:
            total_materials = len(materials)
            await self._update_progress(job.id, 0, total_materials, f"Syncing {total_materials} materials")
            
            # Process in batches
            processed = 0
            for i in range(0, len(materials), job.batch_size):
                batch = materials[i:i + job.batch_size]
                await self._store_material_data(job.id, batch)
                
                processed += len(batch)
                rate = processed / ((datetime.utcnow() - job.started_at).total_seconds() or 1)
                await self._update_progress(
                    job.id, 
                    processed, 
                    total_materials, 
                    f"Synced {processed}/{total_materials} materials",
                    processing_rate=rate
                )
    
    async def _process_legacy_job(self, job: DataIngestionJob):
        """Process legacy data ingestion job."""
        # Handle legacy job types for backward compatibility
        await self._log_job_event(job.id, "INFO", "Processing legacy job type")
        await self._update_progress(job.id, 1, 1, "Legacy job processed")
    
    async def _store_material_data(self, job_id: UUID, materials: List[StandardizedMaterial]):
        """Store material data in the database."""
        try:
            for material in materials:
                raw_data_entry = RawMaterialsData(
                    job_id=job_id,
                    source_db=material.source_db,
                    source_id=material.source_id,
                    material_formula=material.formula,
                    raw_data=material.raw_data if hasattr(material, 'raw_data') else {},
                    standardized_data=material.dict(),
                    processing_status="processed"
                )
                self.db.add(raw_data_entry)
            
            await self.db.commit()
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error storing material data for job {job_id}: {e}")
            raise
    
    async def _update_job_status(self, job_id: UUID, status: str, **kwargs):
        """Update job status in database."""
        try:
            update_values = {"status": status, "updated_at": datetime.utcnow()}
            update_values.update(kwargs)
            
            await self.db.execute(
                update(DataIngestionJob)
                .where(DataIngestionJob.id == job_id)
                .values(**update_values)
            )
            await self.db.commit()
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error updating job status for {job_id}: {e}")
            raise
    
    async def _update_progress(
        self, 
        job_id: UUID, 
        processed: int, 
        total: int, 
        message: str = "",
        processing_rate: Optional[float] = None
    ):
        """Update job progress with rate calculation and ETA."""
        try:
            progress = int((processed / total * 100)) if total > 0 else 0
            
            update_values = {
                "processed_records": processed,
                "total_records": total,
                "progress": progress,
                "updated_at": datetime.utcnow()
            }
            
            if processing_rate:
                update_values["processing_rate"] = processing_rate
                
                # Calculate estimated completion
                remaining = total - processed
                if remaining > 0 and processing_rate > 0:
                    eta_seconds = remaining / processing_rate
                    eta = datetime.utcnow() + timedelta(seconds=eta_seconds)
                    update_values["estimated_completion"] = eta
            
            await self.db.execute(
                update(DataIngestionJob)
                .where(DataIngestionJob.id == job_id)
                .values(**update_values)
            )
            await self.db.commit()
            
            if message:
                await self._log_job_event(job_id, "INFO", message)
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error updating progress for job {job_id}: {e}")
    
    async def _log_job_event(self, job_id: UUID, level: str, message: str, metadata: Optional[Dict[str, Any]] = None):
        """Log job event."""
        try:
            log_entry = JobLog(
                job_id=job_id,
                level=level,
                message=message,
                metadata=metadata
            )
            self.db.add(log_entry)
            await self.db.commit()
            
        except Exception as e:
            await self.db.rollback()
            logger.error(f"Error logging job event for {job_id}: {e}")
    
    async def _handle_job_error(self, job: DataIngestionJob, error: Exception):
        """Handle job processing error with retry logic."""
        try:
            error_message = str(error)
            error_traceback = traceback.format_exc()
            
            await self._log_job_event(
                job.id, 
                "ERROR", 
                f"Job processing error: {error_message}",
                {"traceback": error_traceback}
            )
            
            # Check if we should retry
            if job.current_retry < job.retry_count:
                # Increment retry count
                next_retry = job.current_retry + 1
                
                # Calculate retry delay (exponential backoff)
                delay_minutes = 2 ** next_retry  # 2, 4, 8, 16 minutes
                retry_at = datetime.utcnow() + timedelta(minutes=delay_minutes)
                
                await self.db.execute(
                    update(DataIngestionJob)
                    .where(DataIngestionJob.id == job.id)
                    .values(
                        status="queued",
                        current_retry=next_retry,
                        error_message=error_message,
                        next_run_at=retry_at,
                        updated_at=datetime.utcnow()
                    )
                )
                await self.db.commit()
                
                await self._log_job_event(
                    job.id, 
                    "INFO", 
                    f"Job queued for retry {next_retry}/{job.retry_count} at {retry_at}"
                )
            else:
                # Mark as failed
                await self.db.execute(
                    update(DataIngestionJob)
                    .where(DataIngestionJob.id == job.id)
                    .values(
                        status="failed",
                        error_message=error_message,
                        completed_at=datetime.utcnow(),
                        updated_at=datetime.utcnow()
                    )
                )
                await self.db.commit()
                
                await self._log_job_event(job.id, "ERROR", f"Job failed after {job.retry_count} retries")
            
        except Exception as e:
            logger.error(f"Error handling job error for {job.id}: {e}")


# Global job processor instance
_job_processor: Optional[JobProcessor] = None


async def get_job_processor() -> JobProcessor:
    """Get job processor instance."""
    global _job_processor
    
    if _job_processor is None:
        # Get dependencies
        db = next(get_database_session())
        redis = await get_redis_dependency()
        rate_limiter_manager = RateLimiterManager(redis)
        
        _job_processor = JobProcessor(db, redis, rate_limiter_manager)
    
    return _job_processor


async def start_job_processor():
    """Start the job processor background task."""
    processor = await get_job_processor()
    return asyncio.create_task(processor.start())


async def stop_job_processor():
    """Stop the job processor."""
    global _job_processor
    if _job_processor:
        await _job_processor.stop()
        _job_processor = None
