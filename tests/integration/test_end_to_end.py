"""
End-to-end integration tests for the complete PRISM workflow.

Tests the full pipeline from API request to data storage, including
real-world scenarios and edge cases.
"""

import asyncio
import pytest
from datetime import datetime, timedelta
from typing import List
from uuid import uuid4

from app.schemas import JobType, JobStatus, JobPriority
from app.db.models import DataIngestionJob, RawMaterialsData
from .fixtures import (
    assert_material_data_complete,
    assert_job_completed_successfully,
    ConnectorTestHelper
)


class TestEndToEndWorkflow:
    """Test complete end-to-end workflows."""
    
    @pytest.mark.asyncio
    async def test_complete_jarvis_workflow(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper,
        performance_monitor
    ):
        """Test complete workflow: API request -> job creation -> processing -> storage."""
        performance_monitor.start()
        
        # 1. Configure mock API
        jarvis_data = connector_helper.create_jarvis_material(
            jid="JVASP-12345",
            formula="TiO2",
            formation_energy=-8.2
        )
        mock_api_server.configure_endpoint("jarvis", jarvis_data)
        
        # 2. Create job through API-like process
        job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="jarvis",
            parameters={"material_id": "JVASP-12345"}
        )
        
        # 3. Process job with mocked connector
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            result = await job_processor.process_job(job.id)
        
        # 4. Verify job completion
        await db_helper.session.refresh(job)
        assert_job_completed_successfully(job)
        assert result is True
        
        # 5. Verify data storage
        material_count = await db_helper.count_materials(job.id)
        assert material_count == 1
        
        # 6. Verify API was called correctly
        assert mock_api_server.get_call_count("jarvis") == 1
        
        performance_monitor.stop()
        assert performance_monitor.elapsed_time.total_seconds() < 5.0
    
    @pytest.mark.asyncio
    async def test_bulk_processing_workflow(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper,
        performance_monitor
    ):
        """Test bulk processing workflow with progress tracking."""
        performance_monitor.start()
        
        # Configure bulk response
        bulk_data = connector_helper.create_bulk_response("jarvis", 25)
        mock_api_server.configure_endpoint("jarvis", bulk_data)
        
        # Create bulk job
        job = await db_helper.create_test_job(
            job_type=JobType.BULK_FETCH_BY_FORMULA,
            source_type="jarvis",
            parameters={
                "formula": "Ti*O*",
                "limit": 25,
                "batch_size": 5
            }
        )
        
        # Process with progress monitoring
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            result = await job_processor.process_job(job.id)
        
        # Verify results
        await db_helper.session.refresh(job)
        assert_job_completed_successfully(job)
        assert result is True
        
        # Check batch processing occurred
        material_count = await db_helper.count_materials(job.id)
        assert material_count == 25
        
        # Verify progress tracking
        assert job.progress is not None
        assert job.progress.get("completed", 0) == 25
        assert job.progress.get("total", 0) == 25
        
        performance_monitor.stop()
        assert performance_monitor.elapsed_time.total_seconds() < 10.0
    
    @pytest.mark.asyncio
    async def test_multi_source_processing(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper
    ):
        """Test processing jobs from multiple data sources."""
        # Configure responses for different sources
        jarvis_data = connector_helper.create_jarvis_material(
            jid="JVASP-111", formula="Si"
        )
        nomad_data = connector_helper.create_nomad_material(
            entry_id="nomad-222", formula="Si2"
        )
        
        mock_api_server.configure_endpoint("jarvis", jarvis_data)
        mock_api_server.configure_endpoint("nomad", nomad_data)
        
        # Create jobs for both sources
        jarvis_job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="jarvis",
            parameters={"material_id": "JVASP-111"}
        )
        
        nomad_job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="nomad",
            parameters={"material_id": "nomad-222"}
        )
        
        # Process both jobs
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            jarvis_result = await job_processor.process_job(jarvis_job.id)
            nomad_result = await job_processor.process_job(nomad_job.id)
        
        # Verify both completed successfully
        await db_helper.session.refresh(jarvis_job)
        await db_helper.session.refresh(nomad_job)
        
        assert_job_completed_successfully(jarvis_job)
        assert_job_completed_successfully(nomad_job)
        assert jarvis_result is True
        assert nomad_result is True
        
        # Verify data from both sources
        jarvis_materials = await db_helper.count_materials(jarvis_job.id)
        nomad_materials = await db_helper.count_materials(nomad_job.id)
        
        assert jarvis_materials == 1
        assert nomad_materials == 1
    
    @pytest.mark.asyncio
    async def test_error_recovery_workflow(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper
    ):
        """Test error recovery and retry mechanisms."""
        # Configure API to fail first few times
        jarvis_data = connector_helper.create_jarvis_material()
        mock_api_server.configure_endpoint("jarvis", jarvis_data)
        mock_api_server.set_failure_rate(0.7)  # 70% failure rate
        
        # Create job with retry configuration
        job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="jarvis",
            parameters={"material_id": "JVASP-test"},
            max_retries=3,
            retry_count=0
        )
        
        # Process job with retries
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            with pytest.mock.patch('asyncio.sleep', new_callable=pytest.mock.AsyncMock):
                result = await job_processor.process_job(job.id)
        
        await db_helper.session.refresh(job)
        
        # Job might succeed or fail depending on random failures
        # But should have attempted retries
        assert job.retry_count > 0
        
        # Reset failure rate and try again if it failed
        if job.status == JobStatus.FAILED:
            mock_api_server.set_failure_rate(0.0)  # No failures
            job.status = JobStatus.PENDING
            job.retry_count = 0
            await db_helper.session.commit()
            
            with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
                result = await job_processor.process_job(job.id)
            
            await db_helper.session.refresh(job)
            assert_job_completed_successfully(job)
    
    @pytest.mark.asyncio
    async def test_concurrent_job_processing(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper,
        performance_monitor
    ):
        """Test concurrent processing of multiple jobs."""
        performance_monitor.start()
        
        # Configure API responses
        materials = [
            connector_helper.create_jarvis_material(
                jid=f"JVASP-{i}", 
                formula=f"Material{i}"
            )
            for i in range(10)
        ]
        
        for i, material in enumerate(materials):
            mock_api_server.configure_endpoint(f"jarvis-{i}", material)
        
        # Create multiple jobs
        jobs = []
        for i in range(10):
            job = await db_helper.create_test_job(
                job_type=JobType.FETCH_SINGLE_MATERIAL,
                source_type="jarvis",
                parameters={"material_id": f"JVASP-{i}"}
            )
            jobs.append(job)
        
        # Process jobs concurrently
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            tasks = [
                job_processor.process_job(job.id) 
                for job in jobs
            ]
            results = await asyncio.gather(*tasks, return_exceptions=True)
        
        # Verify results
        successful_jobs = sum(1 for r in results if r is True)
        assert successful_jobs >= 8  # Allow some failures in concurrent processing
        
        # Verify database state
        completed_count = await db_helper.count_jobs(JobStatus.COMPLETED)
        assert completed_count >= 8
        
        performance_monitor.stop()
        assert performance_monitor.elapsed_time.total_seconds() < 15.0


class TestRealWorldScenarios:
    """Test real-world usage scenarios."""
    
    @pytest.mark.asyncio
    async def test_daily_sync_scenario(
        self,
        job_processor,
        job_scheduler,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper
    ):
        """Test daily database synchronization scenario."""
        # Configure large dataset response
        bulk_data = connector_helper.create_bulk_response("jarvis", 100)
        mock_api_server.configure_endpoint("jarvis", bulk_data)
        
        # Create sync job
        sync_job = await db_helper.create_test_job(
            job_type=JobType.SYNC_DATABASE,
            source_type="jarvis",
            parameters={
                "sync_type": "incremental",
                "last_sync": (datetime.utcnow() - timedelta(days=1)).isoformat(),
                "batch_size": 20
            }
        )
        
        # Process sync job
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            result = await job_processor.process_job(sync_job.id)
        
        # Verify sync completed
        await db_helper.session.refresh(sync_job)
        assert_job_completed_successfully(sync_job)
        
        # Verify data was synchronized
        material_count = await db_helper.count_materials(sync_job.id)
        assert material_count == 100
    
    @pytest.mark.asyncio
    async def test_research_query_scenario(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper
    ):
        """Test research-focused query scenario."""
        # Configure specialized search response
        research_materials = [
            connector_helper.create_jarvis_material(
                jid=f"JVASP-research-{i}",
                formula=f"Ti{i}O{i*2}",
                formation_energy=-5.0 - i * 0.5,
                bandgap=1.0 + i * 0.2
            )
            for i in range(1, 6)
        ]
        
        mock_api_server.configure_endpoint("jarvis", research_materials)
        
        # Create research query job
        research_job = await db_helper.create_test_job(
            job_type=JobType.BULK_FETCH_BY_PROPERTIES,
            source_type="jarvis",
            parameters={
                "property_filters": {
                    "bandgap": {"min": 1.0, "max": 3.0},
                    "formation_energy": {"max": -3.0}
                },
                "elements": ["Ti", "O"],
                "limit": 50
            }
        )
        
        # Process research job
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            result = await job_processor.process_job(research_job.id)
        
        # Verify research query completed
        await db_helper.session.refresh(research_job)
        assert_job_completed_successfully(research_job)
        
        # Verify materials match research criteria
        material_count = await db_helper.count_materials(research_job.id)
        assert material_count == 5
    
    @pytest.mark.asyncio
    async def test_high_throughput_scenario(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper,
        performance_monitor
    ):
        """Test high-throughput processing scenario."""
        performance_monitor.start()
        
        # Configure large dataset
        large_dataset = connector_helper.create_bulk_response("jarvis", 500)
        mock_api_server.configure_endpoint("jarvis", large_dataset)
        mock_api_server.delay = 0.01  # Small delay to simulate network
        
        # Create high-throughput job
        htp_job = await db_helper.create_test_job(
            job_type=JobType.BULK_FETCH_BY_FORMULA,
            source_type="jarvis",
            parameters={
                "formula": "*",  # All materials
                "limit": 500,
                "batch_size": 50,
                "max_concurrent": 5
            }
        )
        
        # Process with performance monitoring
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            result = await job_processor.process_job(htp_job.id)
        
        # Verify processing completed efficiently
        await db_helper.session.refresh(htp_job)
        assert_job_completed_successfully(htp_job)
        
        material_count = await db_helper.count_materials(htp_job.id)
        assert material_count == 500
        
        performance_monitor.stop()
        
        # Verify performance metrics
        assert performance_monitor.elapsed_time.total_seconds() < 60.0
        
        # Check processing rate
        if htp_job.progress and "processing_rate" in htp_job.progress:
            rate = htp_job.progress["processing_rate"]
            assert rate > 5.0  # Should process at least 5 materials/second


class TestErrorHandlingScenarios:
    """Test various error handling scenarios."""
    
    @pytest.mark.asyncio
    async def test_api_timeout_scenario(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper
    ):
        """Test handling of API timeouts."""
        # Configure slow API response
        jarvis_data = connector_helper.create_jarvis_material()
        mock_api_server.configure_endpoint("jarvis", jarvis_data, delay=10.0)
        
        job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="jarvis",
            parameters={"material_id": "slow-response"},
            max_retries=1
        )
        
        # Process with timeout
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            with pytest.mock.patch('asyncio.wait_for', side_effect=asyncio.TimeoutError):
                result = await job_processor.process_job(job.id)
        
        # Verify timeout handling
        await db_helper.session.refresh(job)
        assert job.status == JobStatus.FAILED
        assert "timeout" in job.error_message.lower()
        assert result is False
    
    @pytest.mark.asyncio
    async def test_invalid_data_scenario(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server
    ):
        """Test handling of invalid API response data."""
        # Configure invalid response
        invalid_data = {
            "invalid": "structure",
            "missing": "required_fields",
            "malformed": {"data": None}
        }
        mock_api_server.configure_endpoint("jarvis", invalid_data)
        
        job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="jarvis",
            parameters={"material_id": "invalid-data"}
        )
        
        # Process with invalid data
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            result = await job_processor.process_job(job.id)
        
        # Verify error handling
        await db_helper.session.refresh(job)
        assert job.status == JobStatus.FAILED
        assert "validation" in job.error_message.lower() or "invalid" in job.error_message.lower()
        assert result is False
    
    @pytest.mark.asyncio
    async def test_rate_limit_recovery_scenario(
        self,
        job_processor,
        db_helper,
        mock_api_server,
        mock_httpx_with_server,
        connector_helper
    ):
        """Test recovery from rate limiting."""
        # Configure rate limiting
        jarvis_data = connector_helper.create_jarvis_material()
        mock_api_server.configure_endpoint("jarvis", jarvis_data)
        mock_api_server.enable_rate_limiting(limit_after_calls=2)
        
        job = await db_helper.create_test_job(
            job_type=JobType.FETCH_SINGLE_MATERIAL,
            source_type="jarvis",
            parameters={"material_id": "rate-limited"}
        )
        
        # Process with rate limiting
        with pytest.mock.patch('httpx.AsyncClient', return_value=mock_httpx_with_server):
            with pytest.mock.patch('asyncio.sleep', new_callable=pytest.mock.AsyncMock):
                result = await job_processor.process_job(job.id)
        
        # Should eventually succeed after rate limit recovery
        await db_helper.session.refresh(job)
        
        # Either succeeded after retry or failed with rate limit error
        if job.status == JobStatus.COMPLETED:
            assert result is True
            material_count = await db_helper.count_materials(job.id)
            assert material_count == 1
        else:
            assert job.status == JobStatus.FAILED
            assert "rate" in job.error_message.lower()


# Mark all tests as integration and potentially slow
pytestmark = [pytest.mark.integration, pytest.mark.slow]
