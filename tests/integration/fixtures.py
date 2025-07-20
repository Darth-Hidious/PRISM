"""
Fixtures and utilities for integration tests.
Provides common test data, mock configurations, and helper functions.
"""

import asyncio
import json
import pytest
from datetime import datetime, timedelta
from typing import Dict, Any, List, Optional, Callable
from unittest.mock import AsyncMock, MagicMock
from uuid import uuid4

import httpx
from sqlalchemy.ext.asyncio import AsyncSession

from app.db.models import Job, RawMaterialsData, JobLog
from app.schemas import JobType, JobStatus, JobPriority
from app.services.connectors.base_connector import StandardizedMaterial


class MockAPIServer:
    """Mock API server for testing different response scenarios."""
    
    def __init__(self):
        self.responses: Dict[str, Dict] = {}
        self.call_log: List[Dict] = []
        self.delay: float = 0.0
        self.failure_rate: float = 0.0
        self.rate_limit_calls: int = 0
        
    def configure_endpoint(
        self, 
        endpoint: str, 
        response_data: Dict[str, Any],
        status_code: int = 200,
        delay: float = 0.0
    ):
        """Configure response for specific endpoint."""
        self.responses[endpoint] = {
            "data": response_data,
            "status_code": status_code,
            "delay": delay
        }
    
    def set_failure_rate(self, rate: float):
        """Set random failure rate (0.0 to 1.0)."""
        self.failure_rate = rate
    
    def enable_rate_limiting(self, limit_after_calls: int = 3):
        """Enable rate limiting after N calls."""
        self.rate_limit_calls = limit_after_calls
    
    async def handle_request(self, method: str, url: str, **kwargs) -> httpx.Response:
        """Handle mock request and return configured response."""
        import random
        
        # Log the call
        self.call_log.append({
            "method": method,
            "url": str(url),
            "timestamp": datetime.utcnow(),
            "kwargs": kwargs
        })
        
        # Add delay if configured
        if self.delay > 0:
            await asyncio.sleep(self.delay)
        
        # Check rate limiting
        if self.rate_limit_calls > 0 and len(self.call_log) <= self.rate_limit_calls:
            return self._create_response(
                {"error": "Rate limited"}, 
                429
            )
        
        # Check random failures
        if self.failure_rate > 0 and random.random() < self.failure_rate:
            raise httpx.ConnectError("Simulated network error")
        
        # Find matching endpoint
        for endpoint, config in self.responses.items():
            if endpoint in str(url):
                if config["delay"] > 0:
                    await asyncio.sleep(config["delay"])
                
                return self._create_response(
                    config["data"], 
                    config["status_code"]
                )
        
        # Default 404 response
        return self._create_response({"error": "Not found"}, 404)
    
    def _create_response(self, data: Dict, status_code: int) -> httpx.Response:
        """Create mock httpx response."""
        response = MagicMock()
        response.status_code = status_code
        response.headers = {"content-type": "application/json"}
        response.json = AsyncMock(return_value=data)
        response.text = json.dumps(data)
        response.raise_for_status = MagicMock()
        
        if status_code >= 400:
            response.raise_for_status.side_effect = httpx.HTTPStatusError(
                f"HTTP {status_code}",
                request=MagicMock(),
                response=response
            )
        
        return response
    
    def get_call_count(self, endpoint: str = None) -> int:
        """Get number of calls made to endpoint."""
        if endpoint is None:
            return len(self.call_log)
        
        return sum(1 for call in self.call_log if endpoint in call["url"])
    
    def reset(self):
        """Reset all state."""
        self.call_log.clear()
        self.rate_limit_calls = 0
        self.failure_rate = 0.0
        self.delay = 0.0


@pytest.fixture
def mock_api_server():
    """Create mock API server for testing."""
    return MockAPIServer()


@pytest.fixture
def mock_httpx_with_server(mock_api_server):
    """Create httpx client that uses mock API server."""
    async def mock_request(method: str, url: str, **kwargs):
        return await mock_api_server.handle_request(method, url, **kwargs)
    
    client = AsyncMock()
    client.get = lambda url, **kwargs: mock_request("GET", url, **kwargs)
    client.post = lambda url, **kwargs: mock_request("POST", url, **kwargs)
    client.put = lambda url, **kwargs: mock_request("PUT", url, **kwargs)
    client.delete = lambda url, **kwargs: mock_request("DELETE", url, **kwargs)
    
    return client


class DatabaseTestHelper:
    """Helper class for database operations in tests."""
    
    def __init__(self, session: AsyncSession):
        self.session = session
    
    async def create_test_job(
        self,
        job_type: JobType = JobType.FETCH_SINGLE_MATERIAL,
        source_type: str = "jarvis",
        status: JobStatus = JobStatus.PENDING,
        parameters: Dict[str, Any] = None,
        **kwargs
    ) -> Job:
        """Create test job in database."""
        if parameters is None:
            parameters = {"material_id": "test-id"}
        
        job = Job(
            id=uuid4(),
            job_type=job_type,
            source_type=source_type,
            status=status,
            priority=JobPriority.NORMAL,
            parameters=parameters,
            created_at=datetime.utcnow(),
            **kwargs
        )
        
        self.session.add(job)
        await self.session.commit()
        await self.session.refresh(job)
        
        return job
    
    async def create_test_material_data(
        self,
        job_id: str,
        source_db: str = "jarvis",
        source_id: str = "test-id",
        raw_data: Dict[str, Any] = None,
        **kwargs
    ) -> RawMaterialsData:
        """Create test material data in database."""
        if raw_data is None:
            raw_data = {
                "formula": "Si",
                "formation_energy": -5.425,
                "bandgap": 0.6
            }
        
        material_data = RawMaterialsData(
            id=uuid4(),
            job_id=job_id,
            source_db=source_db,
            source_id=source_id,
            raw_data=raw_data,
            processed_at=datetime.utcnow(),
            **kwargs
        )
        
        self.session.add(material_data)
        await self.session.commit()
        await self.session.refresh(material_data)
        
        return material_data
    
    async def create_test_job_log(
        self,
        job_id: str,
        level: str = "INFO",
        message: str = "Test log message",
        **kwargs
    ) -> JobLog:
        """Create test job log entry."""
        log_entry = JobLog(
            id=uuid4(),
            job_id=job_id,
            timestamp=datetime.utcnow(),
            level=level,
            message=message,
            **kwargs
        )
        
        self.session.add(log_entry)
        await self.session.commit()
        await self.session.refresh(log_entry)
        
        return log_entry
    
    async def count_jobs(self, status: JobStatus = None) -> int:
        """Count jobs in database."""
        from sqlalchemy import select, func
        
        query = select(func.count(Job.id))
        if status:
            query = query.where(Job.status == status)
        
        result = await self.session.execute(query)
        return result.scalar()
    
    async def count_materials(self, job_id: str = None) -> int:
        """Count material data entries."""
        from sqlalchemy import select, func
        
        query = select(func.count(RawMaterialsData.id))
        if job_id:
            query = query.where(RawMaterialsData.job_id == job_id)
        
        result = await self.session.execute(query)
        return result.scalar()


@pytest.fixture
def db_helper(test_db_session):
    """Create database test helper."""
    return DatabaseTestHelper(test_db_session)


class ConnectorTestHelper:
    """Helper class for testing connectors."""
    
    @staticmethod
    def create_jarvis_material(
        jid: str = "JVASP-1002",
        formula: str = "Si",
        formation_energy: float = -5.425,
        **kwargs
    ) -> Dict[str, Any]:
        """Create sample JARVIS material data."""
        base_data = {
            "jid": jid,
            "formula": formula,
            "atoms": {
                "lattice": [[3.839, 0, 0], [0, 3.839, 0], [0, 0, 3.839]],
                "coords": [[0.0, 0.0, 0.0], [0.25, 0.25, 0.25]],
                "elements": ["Si", "Si"],
                "cart_coords": [[0.0, 0.0, 0.0], [0.9598, 0.9598, 0.9598]]
            },
            "formation_energy_peratom": formation_energy,
            "e_hull": 0.0,
            "spacegroup": "Fd-3m",
            "bandgap": 0.6,
            "bulk_modulus": 98.8,
            "shear_modulus": 79.9
        }
        base_data.update(kwargs)
        return base_data
    
    @staticmethod
    def create_nomad_material(
        entry_id: str = "test-entry-123",
        formula: str = "Si2",
        band_gap: float = 0.6,
        **kwargs
    ) -> Dict[str, Any]:
        """Create sample NOMAD material data."""
        base_data = {
            "data": [{
                "entry_id": entry_id,
                "results": {
                    "material": {
                        "formula": formula,
                        "elements": formula.replace("2", "").replace("3", "").replace("4", ""),
                        "n_elements": 1,
                        "symmetry": {
                            "space_group_symbol": "Fd-3m",
                            "crystal_system": "cubic"
                        }
                    },
                    "properties": {
                        "electronic": {
                            "band_gap": [{"value": band_gap}]
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
        if kwargs:
            base_data["data"][0].update(kwargs)
        return base_data
    
    @staticmethod
    def create_bulk_response(
        database: str,
        count: int,
        base_data: Dict[str, Any] = None
    ) -> List[Dict[str, Any]]:
        """Create bulk response data for testing."""
        if database == "jarvis":
            if base_data is None:
                base_data = ConnectorTestHelper.create_jarvis_material()
            return [
                {**base_data, "jid": f"JVASP-{i}", "formula": f"Si{i}"}
                for i in range(1, count + 1)
            ]
        elif database == "nomad":
            if base_data is None:
                base_data = ConnectorTestHelper.create_nomad_material()
            
            materials = []
            for i in range(1, count + 1):
                material = {
                    **base_data["data"][0],
                    "entry_id": f"test-entry-{i}",
                }
                material["results"]["material"]["formula"] = f"Si{i}"
                materials.append(material)
            
            return {
                "data": materials,
                "pagination": {
                    "total": count,
                    "page_size": min(count, 10),
                    "page": 1
                }
            }
        else:
            raise ValueError(f"Unknown database: {database}")


@pytest.fixture
def connector_helper():
    """Create connector test helper."""
    return ConnectorTestHelper()


class PerformanceMonitor:
    """Monitor performance metrics during tests."""
    
    def __init__(self):
        self.start_time: Optional[datetime] = None
        self.end_time: Optional[datetime] = None
        self.memory_usage: List[float] = []
        self.call_counts: Dict[str, int] = {}
    
    def start(self):
        """Start performance monitoring."""
        self.start_time = datetime.utcnow()
        self._record_memory()
    
    def stop(self):
        """Stop performance monitoring."""
        self.end_time = datetime.utcnow()
        self._record_memory()
    
    def record_call(self, operation: str):
        """Record operation call."""
        self.call_counts[operation] = self.call_counts.get(operation, 0) + 1
    
    def _record_memory(self):
        """Record current memory usage."""
        try:
            import psutil
            import os
            process = psutil.Process(os.getpid())
            memory_mb = process.memory_info().rss / 1024 / 1024
            self.memory_usage.append(memory_mb)
        except ImportError:
            # psutil not available, skip memory monitoring
            pass
    
    @property
    def elapsed_time(self) -> Optional[timedelta]:
        """Get elapsed time."""
        if self.start_time and self.end_time:
            return self.end_time - self.start_time
        return None
    
    @property
    def memory_increase(self) -> Optional[float]:
        """Get memory increase in MB."""
        if len(self.memory_usage) >= 2:
            return self.memory_usage[-1] - self.memory_usage[0]
        return None
    
    @property
    def peak_memory(self) -> Optional[float]:
        """Get peak memory usage in MB."""
        if self.memory_usage:
            return max(self.memory_usage)
        return None


@pytest.fixture
def performance_monitor():
    """Create performance monitor for tests."""
    return PerformanceMonitor()


def assert_material_data_complete(material: StandardizedMaterial):
    """Assert that material data is complete and valid."""
    assert material.source_db is not None
    assert material.source_id is not None
    assert material.formula is not None
    
    if material.structure:
        assert material.structure.lattice_vectors is not None
        assert material.structure.atomic_positions is not None
        assert len(material.structure.lattice_vectors) == 3
        assert len(material.structure.lattice_vectors[0]) == 3
    
    if material.properties:
        # At least one property should be present
        properties_count = sum([
            1 for prop in [
                material.properties.formation_energy,
                material.properties.band_gap,
                material.properties.bulk_modulus,
                material.properties.e_hull
            ] if prop is not None
        ])
        assert properties_count > 0
    
    assert material.metadata is not None
    assert material.metadata.created_at is not None


def assert_job_completed_successfully(job: Job):
    """Assert that job completed successfully."""
    assert job.status == JobStatus.COMPLETED
    assert job.completed_at is not None
    assert job.error_message is None
    assert job.progress is not None
    assert job.progress.get("completed", 0) > 0
