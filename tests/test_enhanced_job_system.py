import pytest
import asyncio
from datetime import datetime, timedelta
from uuid import uuid4
from typing import Dict, Any

from sqlalchemy.ext.asyncio import AsyncSession
from redis.asyncio import Redis

from app.db.models import Job, RawMaterialsData, ScheduledJob
from app.schemas import JobCreate, JobType, JobPriority, ScheduleConfig
from app.services.job_processor import JobProcessor, ConnectorRegistry
from app.services.job_scheduler import JobScheduler
from app.services.rate_limiter_integration import RateLimiterManager


class MockConnector:
    """Mock connector for testing."""
    
    def __init__(self, config: Dict[str, Any], rate_limiter=None):
        self.config = config
        self.rate_limiter = rate_limiter
        self.is_connected = False
    
    async def connect(self):
        """Mock connect."""
        self.is_connected = True
    
    async def disconnect(self):
        """Mock disconnect."""
        self.is_connected = False
    
    async def get_material_by_id(self, material_id: str):
        """Mock get material by ID."""
        from app.services.connectors.base_connector import StandardizedMaterial
        
        return StandardizedMaterial(
            source_db="mock",
            source_id=material_id,
            formula="MockFormula",
            structure=None,
            properties=None,
            metadata={}
        )
    
    async def search_materials(self, **kwargs):
        """Mock search materials."""
        from app.services.connectors.base_connector import StandardizedMaterial
        
        materials = []
        for i in range(3):  # Return 3 mock materials
            materials.append(StandardizedMaterial(
                source_db="mock",
                source_id=f"mock-{i}",
                formula=f"MockFormula{i}",
                structure=None,
                properties=None,
                metadata=kwargs
            ))
        
        return materials
    
    async def fetch_bulk_materials(self, **kwargs):
        """Mock fetch bulk materials."""
        return await self.search_materials(**kwargs)


@pytest.fixture
def mock_connector():
    """Mock connector fixture."""
    # Register mock connector
    ConnectorRegistry.register_connector("mock", MockConnector)
    yield MockConnector
    # Cleanup
    if "mock" in ConnectorRegistry._connectors:
        del ConnectorRegistry._connectors["mock"]


@pytest.fixture
async def job_processor(db_session: AsyncSession, redis_client: Redis):
    """Job processor fixture."""
    rate_limiter_manager = RateLimiterManager(redis_client)
    processor = JobProcessor(db_session, redis_client, rate_limiter_manager)
    yield processor
    await processor.stop()


@pytest.fixture
async def job_scheduler(db_session: AsyncSession, redis_client: Redis):
    """Job scheduler fixture."""
    scheduler = JobScheduler(db_session, redis_client)
    yield scheduler
    await scheduler.stop()


class TestJobProcessor:
    """Test job processor functionality."""
    
    @pytest.mark.asyncio
    async def test_single_material_job(
        self, 
        job_processor: JobProcessor, 
        db_session: AsyncSession,
        mock_connector
    ):
        """Test single material fetch job."""
        # Create job
        job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "test-123"},
            destination_type="database",
            status="queued",
            batch_size=1,
            retry_count=3
        )
        
        db_session.add(job)
        await db_session.commit()
        
        # Process job
        await job_processor._process_single_material_job(job)
        
        # Check material was stored
        result = await db_session.execute(
            "SELECT COUNT(*) FROM raw_materials_data WHERE job_id = :job_id",
            {"job_id": job.id}
        )
        count = result.scalar()
        assert count == 1
    
    @pytest.mark.asyncio
    async def test_bulk_formula_job(
        self,
        job_processor: JobProcessor,
        db_session: AsyncSession,
        mock_connector
    ):
        """Test bulk fetch by formula job."""
        # Create job
        job = Job(
            id=uuid4(),
            job_type=JobType.BULK_FETCH_BY_FORMULA,
            source_type="mock",
            source_config={"formulas": ["Si", "GaAs"]},
            destination_type="database",
            status="queued",
            batch_size=2,
            retry_count=3,
            started_at=datetime.utcnow()
        )
        
        db_session.add(job)
        await db_session.commit()
        
        # Process job
        await job_processor._process_bulk_formula_job(job)
        
        # Check materials were stored (2 formulas * 3 materials each = 6)
        result = await db_session.execute(
            "SELECT COUNT(*) FROM raw_materials_data WHERE job_id = :job_id",
            {"job_id": job.id}
        )
        count = result.scalar()
        assert count == 6
    
    @pytest.mark.asyncio
    async def test_job_error_handling(
        self,
        job_processor: JobProcessor,
        db_session: AsyncSession
    ):
        """Test job error handling and retry logic."""
        # Create job
        job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="nonexistent",
            source_config={"material_id": "test"},
            destination_type="database",
            status="processing",
            retry_count=2,
            current_retry=0
        )
        
        db_session.add(job)
        await db_session.commit()
        
        # Process job (should fail)
        error = Exception("Test error")
        await job_processor._handle_job_error(job, error)
        
        # Refresh job
        await db_session.refresh(job)
        
        # Should be queued for retry
        assert job.status == "queued"
        assert job.current_retry == 1
        assert job.error_message == "Test error"
        assert job.next_run_at is not None
    
    @pytest.mark.asyncio
    async def test_job_max_retries(
        self,
        job_processor: JobProcessor,
        db_session: AsyncSession
    ):
        """Test job failure after max retries."""
        # Create job at max retries
        job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="nonexistent",
            source_config={"material_id": "test"},
            destination_type="database",
            status="processing",
            retry_count=2,
            current_retry=2  # At max retries
        )
        
        db_session.add(job)
        await db_session.commit()
        
        # Process job (should fail permanently)
        error = Exception("Test error")
        await job_processor._handle_job_error(job, error)
        
        # Refresh job
        await db_session.refresh(job)
        
        # Should be marked as failed
        assert job.status == "failed"
        assert job.error_message == "Test error"
        assert job.completed_at is not None
    
    @pytest.mark.asyncio
    async def test_progress_tracking(
        self,
        job_processor: JobProcessor,
        db_session: AsyncSession
    ):
        """Test job progress tracking."""
        # Create job
        job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "test"},
            destination_type="database",
            status="processing"
        )
        
        db_session.add(job)
        await db_session.commit()
        
        # Update progress
        await job_processor._update_progress(
            job.id, 
            processed=50, 
            total=100, 
            message="Halfway done",
            processing_rate=10.5
        )
        
        # Refresh and check
        await db_session.refresh(job)
        assert job.processed_records == 50
        assert job.total_records == 100
        assert job.progress == 50
        assert job.processing_rate == 10.5
        assert job.estimated_completion is not None


class TestJobScheduler:
    """Test job scheduler functionality."""
    
    @pytest.mark.asyncio
    async def test_create_scheduled_job(
        self,
        job_scheduler: JobScheduler,
        db_session: AsyncSession
    ):
        """Test creating a scheduled job."""
        job_template = JobCreate(
            job_type=JobType.SYNC_DATABASE,
            source_type="mock",
            source_config={"dataset": "test"},
            destination_type="database"
        )
        
        schedule_config = ScheduleConfig(
            enabled=True,
            cron_expression="0 2 * * *",  # Daily at 2 AM
            max_runs=10
        )
        
        job_id = await job_scheduler.create_scheduled_job(
            name="Test Scheduled Job",
            job_template=job_template,
            schedule_config=schedule_config
        )
        
        # Check job was created
        result = await db_session.execute(
            "SELECT * FROM scheduled_jobs WHERE id = :id",
            {"id": job_id}
        )
        scheduled_job = result.first()
        
        assert scheduled_job is not None
        assert scheduled_job.name == "Test Scheduled Job"
        assert scheduled_job.is_active is True
        assert scheduled_job.next_run_at is not None
    
    @pytest.mark.asyncio
    async def test_interval_scheduling(self, job_scheduler: JobScheduler):
        """Test interval-based scheduling."""
        schedule_config = ScheduleConfig(
            enabled=True,
            interval_seconds=3600  # Every hour
        )
        
        next_run = job_scheduler._calculate_next_run(schedule_config)
        
        # Should be approximately 1 hour from now
        now = datetime.utcnow()
        expected = now + timedelta(hours=1)
        
        assert abs((next_run - expected).total_seconds()) < 60  # Within 1 minute
    
    @pytest.mark.asyncio
    async def test_disabled_schedule(self, job_scheduler: JobScheduler):
        """Test disabled schedule."""
        schedule_config = ScheduleConfig(
            enabled=False,
            interval_seconds=3600
        )
        
        next_run = job_scheduler._calculate_next_run(schedule_config)
        assert next_run is None


class TestJobDependencies:
    """Test job dependency functionality."""
    
    @pytest.mark.asyncio
    async def test_dependency_checking(
        self,
        job_processor: JobProcessor,
        db_session: AsyncSession
    ):
        """Test job dependency checking."""
        # Create parent job
        parent_job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "parent"},
            destination_type="database",
            status="completed"
        )
        
        # Create dependent job
        dependent_job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "child"},
            destination_type="database",
            status="queued",
            dependencies=[str(parent_job.id)]
        )
        
        db_session.add(parent_job)
        db_session.add(dependent_job)
        await db_session.commit()
        
        # Check dependencies
        can_run = await job_processor._check_dependencies(dependent_job)
        assert can_run is True
    
    @pytest.mark.asyncio
    async def test_unresolved_dependency(
        self,
        job_processor: JobProcessor,
        db_session: AsyncSession
    ):
        """Test unresolved dependency blocking."""
        # Create parent job (not completed)
        parent_job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "parent"},
            destination_type="database",
            status="processing"  # Not completed
        )
        
        # Create dependent job
        dependent_job = Job(
            id=uuid4(),
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "child"},
            destination_type="database",
            status="queued",
            dependencies=[str(parent_job.id)]
        )
        
        db_session.add(parent_job)
        db_session.add(dependent_job)
        await db_session.commit()
        
        # Check dependencies
        can_run = await job_processor._check_dependencies(dependent_job)
        assert can_run is False


class TestJobTypes:
    """Test different job types."""
    
    @pytest.mark.asyncio
    async def test_job_type_validation(self):
        """Test job type validation in schemas."""
        # Valid job type
        job_data = JobCreate(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "test"},
            destination_type="database"
        )
        assert job_data.job_type == JobType.FETCH_SINGLE_MATERIAL
        
        # Test source config validation for single material
        try:
            invalid_job = JobCreate(
                job_type=JobType.FETCH_SINGLE_MATERIAL,
                source_type="mock",
                source_config={},  # Missing material_id
                destination_type="database"
            )
            assert False, "Should have raised validation error"
        except ValueError as e:
            assert "material_id is required" in str(e)
    
    @pytest.mark.asyncio
    async def test_bulk_formula_validation(self):
        """Test bulk formula job validation."""
        # Valid with formulas
        job_data = JobCreate(
            job_type=JobType.BULK_FETCH_BY_FORMULA,
            source_type="mock",
            source_config={"formulas": ["Si", "GaAs"]},
            destination_type="database"
        )
        assert job_data.source_config["formulas"] == ["Si", "GaAs"]
        
        # Valid with formula pattern
        job_data2 = JobCreate(
            job_type=JobType.BULK_FETCH_BY_FORMULA,
            source_type="mock",
            source_config={"formula_pattern": "Si*"},
            destination_type="database"
        )
        assert job_data2.source_config["formula_pattern"] == "Si*"
    
    @pytest.mark.asyncio
    async def test_priority_levels(self):
        """Test job priority levels."""
        job_data = JobCreate(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="mock",
            source_config={"material_id": "test"},
            destination_type="database",
            priority=JobPriority.HIGH
        )
        assert job_data.priority == JobPriority.HIGH
        assert job_data.priority.value == 8


class TestRateLimiterIntegration:
    """Test rate limiter integration with jobs."""
    
    @pytest.mark.asyncio
    async def test_connector_with_rate_limiter(
        self,
        job_processor: JobProcessor,
        redis_client: Redis,
        mock_connector
    ):
        """Test connector creation with rate limiter."""
        config = {"test": "config"}
        
        # Get connector (should include rate limiter)
        connector = await job_processor._get_connector("mock", config)
        
        assert connector is not None
        assert connector.is_connected is True
        
        # Cleanup
        await connector.disconnect()


@pytest.mark.asyncio
async def test_job_queue_integration(redis_client: Redis):
    """Test job queue integration."""
    from app.services.connectors.redis_connector import JobQueue
    
    job_queue = JobQueue(redis_client)
    
    # Enqueue job
    success = await job_queue.enqueue_job(
        job_id="test-job",
        job_type=JobType.FETCH_SINGLE_MATERIAL,
        payload={"test": "data"},
        priority=5
    )
    assert success is True
    
    # Dequeue job
    job_data = await job_queue.dequeue_job()
    assert job_data is not None
    assert job_data["job_id"] == "test-job"
    assert job_data["job_type"] == JobType.FETCH_SINGLE_MATERIAL
    
    # Check status
    status = await job_queue.get_job_status("test-job")
    assert status is not None
    assert status["status"] == "processing"
