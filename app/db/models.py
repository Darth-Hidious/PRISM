from datetime import datetime
from typing import Optional
from uuid import uuid4

from sqlalchemy import Column, String, DateTime, Text, Boolean, Integer, JSON, Float, ForeignKey
from sqlalchemy.dialects.postgresql import UUID
from sqlalchemy.orm import relationship

from .database import Base


class DataIngestionJob(Base):
    """Enhanced data ingestion job model."""
    
    __tablename__ = "data_ingestion_jobs"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    job_type = Column(String(50), nullable=False, default="data_ingestion", index=True)
    source_type = Column(String(50), nullable=False, index=True)
    source_config = Column(JSON, nullable=False)
    destination_type = Column(String(50), nullable=False)
    destination_config = Column(JSON, nullable=True)
    status = Column(String(20), nullable=False, default="pending", index=True)
    progress = Column(Integer, default=0)
    total_records = Column(Integer, default=0)
    processed_records = Column(Integer, default=0)
    error_count = Column(Integer, default=0)
    error_message = Column(Text, nullable=True)
    
    # Enhanced fields
    batch_size = Column(Integer, default=100)
    retry_count = Column(Integer, default=3)
    current_retry = Column(Integer, default=0)
    processing_rate = Column(Float, nullable=True)  # items per second
    estimated_completion = Column(DateTime, nullable=True)
    dependencies = Column(JSON, nullable=True)  # List of job IDs
    schedule_config = Column(JSON, nullable=True)
    priority = Column(Integer, default=5, index=True)
    
    # Timestamps
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)
    started_at = Column(DateTime, nullable=True)
    completed_at = Column(DateTime, nullable=True)
    next_run_at = Column(DateTime, nullable=True)  # For scheduled jobs
    
    # Metadata
    created_by = Column(String(100), nullable=True)
    job_metadata = Column(JSON, nullable=True)


class DataSource(Base):
    """Data source configuration model."""
    
    __tablename__ = "data_sources"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    name = Column(String(100), nullable=False, unique=True, index=True)
    description = Column(Text, nullable=True)
    source_type = Column(String(50), nullable=False, index=True)
    connection_config = Column(JSON, nullable=False)
    is_active = Column(Boolean, default=True, nullable=False)
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)
    created_by = Column(String(100), nullable=True)
    tags = Column(JSON, nullable=True)


class DataDestination(Base):
    """Data destination configuration model."""
    
    __tablename__ = "data_destinations"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    name = Column(String(100), nullable=False, unique=True, index=True)
    description = Column(Text, nullable=True)
    destination_type = Column(String(50), nullable=False, index=True)
    connection_config = Column(JSON, nullable=False)
    is_active = Column(Boolean, default=True, nullable=False)
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)
    created_by = Column(String(100), nullable=True)
    tags = Column(JSON, nullable=True)


class JobLog(Base):
    """Job execution log model."""
    
    __tablename__ = "job_logs"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    job_id = Column(UUID(as_uuid=True), nullable=False, index=True)
    level = Column(String(10), nullable=False, index=True)
    message = Column(Text, nullable=False)
    timestamp = Column(DateTime, default=datetime.utcnow, nullable=False)
    log_metadata = Column(JSON, nullable=True)


class RawMaterialsData(Base):
    """Raw materials data storage model."""
    
    __tablename__ = "raw_materials_data"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    job_id = Column(UUID(as_uuid=True), nullable=False, index=True)
    source_db = Column(String(50), nullable=False, index=True)
    source_id = Column(String(100), nullable=False, index=True)
    material_formula = Column(String(200), nullable=True, index=True)
    raw_data = Column(JSON, nullable=False)  # Original API response
    standardized_data = Column(JSON, nullable=True)  # Processed/standardized data
    processing_status = Column(String(20), default="raw", index=True)  # raw, processed, failed
    processing_error = Column(Text, nullable=True)
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)
    material_metadata = Column(JSON, nullable=True)


class JobDependency(Base):
    """Job dependency tracking model."""
    
    __tablename__ = "job_dependencies"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    dependent_job_id = Column(UUID(as_uuid=True), nullable=False, index=True)
    dependency_job_id = Column(UUID(as_uuid=True), nullable=False, index=True)
    required_status = Column(String(20), default="completed", nullable=False)
    timeout_minutes = Column(Integer, default=60)
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    resolved_at = Column(DateTime, nullable=True)
    is_resolved = Column(Boolean, default=False, index=True)


class ScheduledJob(Base):
    """Scheduled/recurring job model."""
    
    __tablename__ = "scheduled_jobs"
    
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    name = Column(String(200), nullable=False)
    job_template = Column(JSON, nullable=False)  # JobCreate template
    schedule_config = Column(JSON, nullable=False)  # Cron or interval config
    is_active = Column(Boolean, default=True, index=True)
    last_run_at = Column(DateTime, nullable=True)
    next_run_at = Column(DateTime, nullable=True, index=True)
    run_count = Column(Integer, default=0)
    max_runs = Column(Integer, nullable=True)
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)
    created_by = Column(String(100), nullable=True)
    schedule_metadata = Column(JSON, nullable=True)
