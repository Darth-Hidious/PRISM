"""
Integration tests for PRISM data ingestion connectors.

Tests the full flow from job creation to data storage with mocked external APIs.
Covers all major scenarios including error handling, rate limiting, and concurrent processing.
"""

import asyncio
import json
import pytest
import pytest_asyncio
from datetime import datetime, timedelta
from typing import Dict, Any, List, Optional
from unittest.mock import AsyncMock, MagicMock, patch
from uuid import uuid4

import httpx
import redis.asyncio as redis
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

# Import application components
from app.core.config import Settings
from app.db.models import (
    Base, DataIngestionJob, JobLog, RawMaterialsData, 
    JobDependency, ScheduledJob
)
from app.schemas import (
    JobType, JobStatus, JobPriority, JobProgress,
    DataSourceCreate, JobCreate
)
from app.services.job_processor import JobProcessor, ConnectorRegistry
from app.services.job_scheduler import JobScheduler
from app.services.rate_limiter_integration import RateLimiterManager
from app.services.connectors.jarvis_connector import JarvisConnector
from app.services.connectors.nomad_connector import NOMADConnector
from app.services.connectors.base_connector import (
    DatabaseConnector, StandardizedMaterial, MaterialStructure,
    MaterialProperties, MaterialMetadata, ConnectorException
)


# ============================================================================
# Test Configuration and Fixtures
# ============================================================================

@pytest.fixture(scope="session")
def test_settings():
    """Test application settings."""
    return Settings(
        database_url="sqlite+aiosqlite:///:memory:",
        redis_url="redis://localhost:6379/15",  # Use test database
        cors_allowed_origins=["http://localhost:3000"],
        log_level="DEBUG",
        environment="test"
    )


@pytest_asyncio.fixture(scope="session")
async def test_db_engine(test_settings):
    """Create test database engine."""
    engine = create_async_engine(
        test_settings.database_url,
        echo=False,
        poolclass=StaticPool,
        connect_args={"check_same_thread": False}
    )
    
    # Create all tables
    async with engine.begin() as conn:
        await conn.run_sync(Base.metadata.create_all)
    
    yield engine
    
    # Cleanup
    async with engine.begin() as conn:
        await conn.run_sync(Base.metadata.drop_all)
    await engine.dispose()


@pytest_asyncio.fixture
async def test_db_session(test_db_engine):
    """Create test database session."""
    async_session = sessionmaker(
        test_db_engine, class_=AsyncSession, expire_on_commit=False
    )
    
    async with async_session() as session:
        yield session


@pytest_asyncio.fixture
async def test_redis():
    """Create test Redis connection."""
    try:
        redis_client = redis.Redis.from_url(
            "redis://localhost:6379/15",
            decode_responses=True
        )
        
        # Clear test database
        await redis_client.flushdb()
        yield redis_client
        
        # Cleanup
        await redis_client.flushdb()
        await redis_client.close()
    except Exception:
        # If Redis is not available, use a mock
        mock_redis = AsyncMock()
        mock_redis.get.return_value = None
        mock_redis.setex.return_value = True
        mock_redis.exists.return_value = False
        mock_redis.eval.return_value = [1, 10]  # [allowed, remaining]
        yield mock_redis


@pytest_asyncio.fixture
async def rate_limiter_manager(test_redis):
    """Create rate limiter manager for testing."""
    return RateLimiterManager(test_redis)


@pytest_asyncio.fixture
async def job_processor(test_db_session, test_redis, rate_limiter_manager):
    """Create job processor for testing."""
    return JobProcessor(test_db_session, test_redis, rate_limiter_manager)


@pytest_asyncio.fixture
async def job_scheduler(test_db_session, test_redis, rate_limiter_manager):
    """Create job scheduler for testing."""
    return JobScheduler(test_db_session, test_redis, rate_limiter_manager)


# ============================================================================
# Sample Data Fixtures
# ============================================================================

@pytest.fixture
def sample_jarvis_response():
    """Sample JARVIS API response."""
    return {
        "jid": "JVASP-1002",
        "formula": "Si",
        "atoms": {
            "lattice": [[3.839, 0, 0], [0, 3.839, 0], [0, 0, 3.839]],
            "coords": [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]],
            "elements": ["Si", "Si"],
            "cart_coords": [[0.0, 0.0, 0.0], [0.9598, 0.9598, 0.9598]]
        },
        "formation_energy_peratom": -5.425,
        "e_hull": 0.0,
        "spacegroup": "Fd-3m",
        "bandgap": 0.6,
        "bulk_modulus": 98.8,
        "shear_modulus": 79.9
    }


@pytest.fixture
def sample_nomad_response():
    """Sample NOMAD API response."""
    return {
        "data": [{
            "entry_id": "test-entry-123",
            "results": {
                "material": {
                    "formula": "Si2",
                    "elements": ["Si"],
                    "n_elements": 1,
                    "symmetry": {
                        "space_group_symbol": "Fd-3m",
                        "crystal_system": "cubic"
                    }
                },
                "properties": {
                    "electronic": {
                        "band_gap": [{"value": 0.6}]
                    },
                    "thermodynamic": {
                        "formation_energy": [{"value": -5.425}]
                    }
                },
                "eln": {
                    "lab_ids": ["test-lab"],
                    "sections": ["TestSection"],
                    "methods": ["DFT"]
                }
            },
            "archive": {
                "run": [{
                    "system": [{
                        "atoms": {
                            "lattice_vectors": [
                                [3.839, 0, 0],
                                [0, 3.839, 0],
                                [0, 0, 3.839]
                            ],
                            "positions": [
                                [0.0, 0.0, 0.0],
                                [0.25, 0.25, 0.25]
                            ],
                            "labels": ["Si", "Si"]
                        }
                    }]
                }]
            }
        }],
        "pagination": {
            "total": 1,
            "page_size": 10,
            "page": 1
        }
    }


@pytest.fixture
def sample_standardized_material():
    """Sample standardized material data."""
    return StandardizedMaterial(
        source_db="jarvis",
        source_id="JVASP-1002",
        formula="Si2",
        structure=MaterialStructure(
            lattice_vectors=[[3.839, 0, 0], [0, 3.839, 0], [0, 0, 3.839]],
            atomic_positions=[[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]],
            atomic_numbers=[14, 14],
            space_group="Fd-3m"
        ),
        properties=MaterialProperties(
            formation_energy=-5.425,
            e_hull=0.0,
            band_gap=0.6,
            bulk_modulus=98.8,
            shear_modulus=79.9
        ),
        metadata=MaterialMetadata(
            created_at=datetime.utcnow(),
            version="1.0",
            confidence_score=0.95
        )
    )


# ============================================================================
# Mock HTTP Responses
# ============================================================================

class MockResponse:
    """Mock HTTP response."""
    
    def __init__(self, json_data: Dict[Any, Any], status_code: int = 200):
        self.json_data = json_data
        self.status_code = status_code
        self.headers = {"content-type": "application/json"}
    
    async def json(self):
        return self.json_data
    
    def raise_for_status(self):
        if self.status_code >= 400:
            raise httpx.HTTPStatusError(
                f"HTTP {self.status_code}", 
                request=MagicMock(), 
                response=self
            )


@pytest.fixture
def mock_httpx_client():
    """Mock httpx client with configurable responses."""
    client = AsyncMock()
    
    def configure_response(url_pattern: str, response_data: Dict, status_code: int = 200):
        """Configure mock response for URL pattern."""
        mock_response = MockResponse(response_data, status_code)
        
        async def mock_get(*args, **kwargs):
            if url_pattern in str(args[0]) if args else url_pattern in str(kwargs.get('url', '')):
                return mock_response
            return MockResponse({}, 404)
        
        client.get = mock_get
        return mock_response
    
    client.configure_response = configure_response
    return client


# ============================================================================
# Integration Test Classes
# ============================================================================

class TestConnectorIntegration:
    """Integration tests for database connectors."""
    
    @pytest.mark.asyncio
    async def test_successful_jarvis_data_fetch(
        self, 
        mock_httpx_client, 
        sample_jarvis_response,
        rate_limiter_manager
    ):
        """Test successful data fetch from JARVIS."""
        # Configure mock response
        mock_httpx_client.configure_response(
            "jarvis", 
            sample_jarvis_response
        )
        
        # Create connector with mocked client
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            connector = JarvisConnector(rate_limiter_manager)
            await connector.connect()
            
            # Test material fetch
            material = await connector.get_material_by_id("JVASP-1002")
            
            assert material is not None
            assert material.source_db == "jarvis"
            assert material.source_id == "JVASP-1002"
            assert material.formula == "Si"
            assert material.properties.formation_energy == -5.425
            
            await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_successful_nomad_data_fetch(
        self, 
        mock_httpx_client, 
        sample_nomad_response,
        rate_limiter_manager
    ):
        """Test successful data fetch from NOMAD."""
        # Configure mock response
        mock_httpx_client.configure_response(
            "nomad", 
            sample_nomad_response
        )
        
        # Create connector with mocked client
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            connector = NOMADConnector(rate_limiter_manager)
            await connector.connect()
            
            # Test material search
            materials = await connector.search_materials(
                formula="Si2", 
                limit=10
            )
            
            assert len(materials) == 1
            assert materials[0].source_db == "nomad"
            assert materials[0].formula == "Si2"
            assert materials[0].properties.band_gap == 0.6
            
            await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_rate_limit_handling(
        self, 
        mock_httpx_client, 
        rate_limiter_manager
    ):
        """Test rate limit handling with 429 responses."""
        # Configure rate limit response first, then success
        call_count = 0
        
        async def mock_get(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            
            if call_count == 1:
                return MockResponse({"error": "Rate limited"}, 429)
            else:
                return MockResponse({"jid": "test", "formula": "H2"}, 200)
        
        mock_httpx_client.get = mock_get
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            with patch('asyncio.sleep', new_callable=AsyncMock):  # Speed up test
                connector = JarvisConnector(rate_limiter_manager)
                await connector.connect()
                
                # This should retry after rate limit and succeed
                material = await connector.get_material_by_id("test-id")
                
                assert material is not None
                assert call_count == 2  # First call rate limited, second succeeded
                
                await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_network_error_retries(
        self, 
        mock_httpx_client, 
        rate_limiter_manager
    ):
        """Test network error handling with retries."""
        call_count = 0
        
        async def mock_get(*args, **kwargs):
            nonlocal call_count
            call_count += 1
            
            if call_count <= 2:
                raise httpx.ConnectError("Network error")
            else:
                return MockResponse({"jid": "test", "formula": "H2"}, 200)
        
        mock_httpx_client.get = mock_get
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            with patch('asyncio.sleep', new_callable=AsyncMock):  # Speed up test
                connector = JarvisConnector(rate_limiter_manager)
                await connector.connect()
                
                # Should succeed after retries
                material = await connector.get_material_by_id("test-id")
                
                assert material is not None
                assert call_count == 3  # Two failures, one success
                
                await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_data_validation_errors(
        self, 
        mock_httpx_client, 
        rate_limiter_manager
    ):
        """Test handling of invalid response data."""
        # Configure invalid response
        mock_httpx_client.configure_response(
            "jarvis", 
            {"invalid": "data", "missing": "required_fields"}
        )
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            connector = JarvisConnector(rate_limiter_manager)
            await connector.connect()
            
            # Should handle validation errors gracefully
            with pytest.raises(ConnectorException):
                await connector.get_material_by_id("invalid-id")
            
            await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_concurrent_requests(
        self, 
        mock_httpx_client, 
        sample_jarvis_response,
        rate_limiter_manager
    ):
        """Test concurrent request handling."""
        # Configure mock to track concurrent calls
        call_times = []
        
        async def mock_get(*args, **kwargs):
            call_times.append(datetime.utcnow())
            await asyncio.sleep(0.1)  # Simulate network delay
            return MockResponse(sample_jarvis_response)
        
        mock_httpx_client.get = mock_get
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            connector = JarvisConnector(rate_limiter_manager)
            await connector.connect()
            
            # Make concurrent requests
            tasks = [
                connector.get_material_by_id(f"test-{i}") 
                for i in range(5)
            ]
            
            results = await asyncio.gather(*tasks)
            
            # Verify all requests succeeded
            assert len(results) == 5
            assert all(r is not None for r in results)
            
            # Verify requests were handled concurrently
            assert len(call_times) == 5
            time_span = (max(call_times) - min(call_times)).total_seconds()
            assert time_span < 0.5  # Should complete within reasonable time
            
            await connector.disconnect()


class TestJobSystemIntegration:
    """Integration tests for the job processing system."""
    
    @pytest.mark.asyncio
    async def test_full_job_flow_jarvis(
        self, 
        job_processor, 
        test_db_session,
        mock_httpx_client, 
        sample_jarvis_response
    ):
        """Test complete job flow: create -> process -> store -> retrieve."""
        # Configure mock response
        mock_httpx_client.configure_response("jarvis", sample_jarvis_response)
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            # 1. Create job
            job = DataIngestionJob(
                id=uuid4(),
                job_type=JobType.FETCH_SINGLE_MATERIAL,
                source_type="jarvis",
                status=JobStatus.PENDING,
                priority=JobPriority.NORMAL,
                parameters={"material_id": "JVASP-1002"},
                created_at=datetime.utcnow()
            )
            
            test_db_session.add(job)
            await test_db_session.commit()
            
            # 2. Process job
            result = await job_processor.process_job(job.id)
            
            # 3. Verify job was processed successfully
            await test_db_session.refresh(job)
            assert job.status == JobStatus.COMPLETED
            assert job.completed_at is not None
            assert result is True
            
            # 4. Verify data was stored
            from sqlalchemy import select
            query = select(RawMaterialsData).where(
                RawMaterialsData.job_id == job.id
            )
            result = await test_db_session.execute(query)
            stored_materials = result.scalars().all()
            
            assert len(stored_materials) == 1
            stored_material = stored_materials[0]
            assert stored_material.source_db == "jarvis"
            assert stored_material.source_id == "JVASP-1002"
            assert "formation_energy" in stored_material.raw_data
    
    @pytest.mark.asyncio
    async def test_bulk_fetch_job_processing(
        self, 
        job_processor, 
        test_db_session,
        mock_httpx_client, 
        sample_jarvis_response
    ):
        """Test bulk fetch job processing."""
        # Configure mock to return multiple materials
        bulk_response = [
            {**sample_jarvis_response, "jid": f"JVASP-{i}", "formula": f"Si{i}"}
            for i in range(1, 6)
        ]
        mock_httpx_client.configure_response("jarvis", bulk_response)
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            # Create bulk fetch job
            job = DataIngestionJob(
                id=uuid4(),
                job_type=JobType.BULK_FETCH_BY_FORMULA,
                source_type="jarvis",
                status=JobStatus.PENDING,
                priority=JobPriority.NORMAL,
                parameters={
                    "formula": "Si*", 
                    "limit": 5,
                    "batch_size": 2
                },
                created_at=datetime.utcnow()
            )
            
            test_db_session.add(job)
            await test_db_session.commit()
            
            # Process job
            result = await job_processor.process_job(job.id)
            
            # Verify job completion
            await test_db_session.refresh(job)
            assert job.status == JobStatus.COMPLETED
            assert result is True
            
            # Verify all materials were stored
            from sqlalchemy import select
            query = select(RawMaterialsData).where(
                RawMaterialsData.job_id == job.id
            )
            result = await test_db_session.execute(query)
            stored_materials = result.scalars().all()
            
            assert len(stored_materials) == 5
            assert all(m.source_db == "jarvis" for m in stored_materials)
    
    @pytest.mark.asyncio
    async def test_job_error_handling(
        self, 
        job_processor, 
        test_db_session,
        mock_httpx_client
    ):
        """Test job error handling and retry logic."""
        # Configure mock to always fail
        async def mock_get(*args, **kwargs):
            raise httpx.ConnectError("Network error")
        
        mock_httpx_client.get = mock_get
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            with patch('asyncio.sleep', new_callable=AsyncMock):  # Speed up test
                # Create job
                job = DataIngestionJob(
                    id=uuid4(),
                    job_type=JobType.FETCH_SINGLE_MATERIAL,
                    source_type="jarvis",
                    status=JobStatus.PENDING,
                    priority=JobPriority.NORMAL,
                    parameters={"material_id": "JVASP-1002"},
                    retry_count=0,
                    max_retries=2,
                    created_at=datetime.utcnow()
                )
                
                test_db_session.add(job)
                await test_db_session.commit()
                
                # Process job (should fail)
                result = await job_processor.process_job(job.id)
                
                # Verify job failed after retries
                await test_db_session.refresh(job)
                assert job.status == JobStatus.FAILED
                assert job.retry_count == 2
                assert job.error_message is not None
                assert result is False
    
    @pytest.mark.asyncio
    async def test_job_dependency_resolution(
        self, 
        job_processor, 
        test_db_session,
        mock_httpx_client, 
        sample_jarvis_response
    ):
        """Test job dependency resolution."""
        mock_httpx_client.configure_response("jarvis", sample_jarvis_response)
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            # Create parent job
            parent_job = DataIngestionJob(
                id=uuid4(),
                job_type=JobType.FETCH_SINGLE_MATERIAL,
                source_type="jarvis",
                status=JobStatus.PENDING,
                priority=JobPriority.NORMAL,
                parameters={"material_id": "JVASP-1002"},
                created_at=datetime.utcnow()
            )
            
            # Create child job with dependency
            child_job = DataIngestionJob(
                id=uuid4(),
                job_type=JobType.BULK_FETCH_BY_FORMULA,
                source_type="jarvis",
                status=JobStatus.PENDING,
                priority=JobPriority.NORMAL,
                parameters={"formula": "Si", "limit": 1},
                created_at=datetime.utcnow()
            )
            
            # Create dependency relationship
            dependency = JobDependency(
                job_id=child_job.id,
                depends_on_job_id=parent_job.id,
                created_at=datetime.utcnow()
            )
            
            test_db_session.add_all([parent_job, child_job, dependency])
            await test_db_session.commit()
            
            # Try to process child job (should wait for parent)
            can_process = await job_processor._check_job_dependencies(child_job.id)
            assert can_process is False
            
            # Process parent job
            await job_processor.process_job(parent_job.id)
            
            # Now child job should be processable
            can_process = await job_processor._check_job_dependencies(child_job.id)
            assert can_process is True
            
            # Process child job
            result = await job_processor.process_job(child_job.id)
            assert result is True


class TestSchedulerIntegration:
    """Integration tests for job scheduler."""
    
    @pytest.mark.asyncio
    async def test_scheduled_job_creation(
        self, 
        job_scheduler, 
        test_db_session
    ):
        """Test scheduled job creation and processing."""
        # Create scheduled job
        scheduled_job = ScheduledJob(
            id=uuid4(),
            name="Daily JARVIS Sync",
            job_type=JobType.SYNC_DATABASE,
            source_type="jarvis",
            schedule_expression="0 0 * * *",  # Daily at midnight
            parameters={"limit": 100},
            is_active=True,
            max_runs=None,
            run_count=0,
            created_at=datetime.utcnow()
        )
        
        test_db_session.add(scheduled_job)
        await test_db_session.commit()
        
        # Test job creation from schedule
        job_id = await job_scheduler.create_job_from_schedule(scheduled_job.id)
        
        assert job_id is not None
        
        # Verify job was created
        from sqlalchemy import select
        query = select(DataIngestionJob).where(
            DataIngestionJob.id == job_id
        )
        result = await test_db_session.execute(query)
        created_job = result.scalar_one_or_none()
        
        assert created_job is not None
        assert created_job.job_type == JobType.SYNC_DATABASE
        assert created_job.source_type == "jarvis"
        assert created_job.status == JobStatus.PENDING


class TestConnectorRegistry:
    """Test connector registry functionality."""
    
    def test_connector_registration(self):
        """Test connector registration and retrieval."""
        # Test existing connectors
        jarvis_class = ConnectorRegistry.get_connector_class("jarvis")
        assert jarvis_class == JarvisConnector
        
        nomad_class = ConnectorRegistry.get_connector_class("nomad")
        assert nomad_class == NOMADConnector
        
        # Test case insensitivity
        jarvis_class_upper = ConnectorRegistry.get_connector_class("JARVIS")
        assert jarvis_class_upper == JarvisConnector
        
        # Test unknown connector
        unknown_class = ConnectorRegistry.get_connector_class("unknown")
        assert unknown_class is None
        
        # Test registering new connector
        class TestConnector(DatabaseConnector):
            async def connect(self): pass
            async def disconnect(self): pass
            async def search_materials(self, **kwargs): pass
            async def get_material_by_id(self, material_id: str): pass
            async def fetch_bulk_materials(self, **kwargs): pass
            async def validate_response(self, response): pass
            async def standardize_data(self, data): pass
        
        ConnectorRegistry.register_connector("test", TestConnector)
        test_class = ConnectorRegistry.get_connector_class("test")
        assert test_class == TestConnector


# ============================================================================
# Performance and Load Tests
# ============================================================================

class TestPerformanceIntegration:
    """Performance and load testing for connectors."""
    
    @pytest.mark.asyncio
    async def test_high_concurrency_processing(
        self, 
        job_processor, 
        test_db_session,
        mock_httpx_client, 
        sample_jarvis_response
    ):
        """Test high concurrency job processing."""
        mock_httpx_client.configure_response("jarvis", sample_jarvis_response)
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            # Create multiple jobs
            jobs = []
            for i in range(20):
                job = DataIngestionJob(
                    id=uuid4(),
                    job_type=JobType.FETCH_SINGLE_MATERIAL,
                    source_type="jarvis",
                    status=JobStatus.PENDING,
                    priority=JobPriority.NORMAL,
                    parameters={"material_id": f"JVASP-{i}"},
                    created_at=datetime.utcnow()
                )
                jobs.append(job)
                test_db_session.add(job)
            
            await test_db_session.commit()
            
            # Process jobs concurrently
            start_time = datetime.utcnow()
            
            tasks = [
                job_processor.process_job(job.id) 
                for job in jobs
            ]
            
            results = await asyncio.gather(*tasks, return_exceptions=True)
            
            end_time = datetime.utcnow()
            
            # Verify results
            successful_jobs = sum(1 for r in results if r is True)
            assert successful_jobs >= 15  # Allow some failures in high concurrency
            
            # Verify processing time is reasonable
            processing_time = (end_time - start_time).total_seconds()
            assert processing_time < 30  # Should complete within 30 seconds
    
    @pytest.mark.asyncio
    async def test_memory_usage_bulk_processing(
        self, 
        job_processor, 
        test_db_session,
        mock_httpx_client, 
        sample_jarvis_response
    ):
        """Test memory usage during bulk processing."""
        # Create large dataset response
        large_response = [
            {**sample_jarvis_response, "jid": f"JVASP-{i}"}
            for i in range(1000)
        ]
        mock_httpx_client.configure_response("jarvis", large_response)
        
        with patch('httpx.AsyncClient', return_value=mock_httpx_client):
            # Create bulk job
            job = DataIngestionJob(
                id=uuid4(),
                job_type=JobType.BULK_FETCH_BY_FORMULA,
                source_type="jarvis",
                status=JobStatus.PENDING,
                priority=JobPriority.NORMAL,
                parameters={
                    "formula": "Si", 
                    "limit": 1000,
                    "batch_size": 50  # Process in smaller batches
                },
                created_at=datetime.utcnow()
            )
            
            test_db_session.add(job)
            await test_db_session.commit()
            
            # Process job and monitor memory usage
            import psutil
            import os
            
            process = psutil.Process(os.getpid())
            initial_memory = process.memory_info().rss / 1024 / 1024  # MB
            
            result = await job_processor.process_job(job.id)
            
            final_memory = process.memory_info().rss / 1024 / 1024  # MB
            memory_increase = final_memory - initial_memory
            
            # Verify job completed successfully
            assert result is True
            
            # Verify memory usage is reasonable (< 100MB increase)
            assert memory_increase < 100


# ============================================================================
# Conftest for pytest configuration
# ============================================================================

def pytest_configure(config):
    """Configure pytest for integration tests."""
    config.addinivalue_line(
        "markers", "integration: mark test as integration test"
    )
    config.addinivalue_line(
        "markers", "slow: mark test as slow running"
    )


# Mark all tests in this module as integration tests
pytestmark = pytest.mark.integration
