from datetime import datetime
from typing import Optional
from uuid import uuid4

from sqlalchemy import Column, String, DateTime, Text, Boolean, Integer, JSON, Float, ForeignKey, Index
from sqlalchemy.dialects.postgresql import UUID
from sqlalchemy.orm import relationship

from .database import Base


class Job(Base):
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


class MaterialEntry(Base):
    """Standardized materials database table."""
    
    __tablename__ = "materials"
    
    # Primary identification
    id = Column(UUID(as_uuid=True), primary_key=True, default=uuid4, index=True)
    material_id = Column(String(100), nullable=False, unique=True, index=True)  # Unique across all sources
    origin = Column(String(50), nullable=False, index=True)  # Source database (NOMAD, JARVIS, etc.)
    source_id = Column(String(100), nullable=False, index=True)  # Original ID from source database
    
    # Composition and structure
    composition = Column(String(500), nullable=False, index=True)  # Alphabetically-ordered composition
    reduced_formula = Column(String(200), nullable=False, index=True)  # Reduced chemical formula
    elements = Column(JSON, nullable=False)  # List of chemical elements
    nsites = Column(Integer, nullable=True, index=True)  # Number of atoms
    
    # Physical properties
    volume = Column(Float, nullable=True)  # Volume in Å³
    density = Column(Float, nullable=True)  # Density in Å³/atom
    
    # Symmetry properties
    point_group = Column(String(20), nullable=True, index=True)
    space_group = Column(String(50), nullable=True, index=True)
    space_group_number = Column(Integer, nullable=True, index=True)
    crystal_system = Column(String(20), nullable=True, index=True)
    
    # Energy properties
    uncorrected_energy = Column(Float, nullable=True)
    corrected_energy = Column(Float, nullable=True)  # MP2020 corrections
    formation_energy_per_atom = Column(Float, nullable=True, index=True)
    decomposition_energy_per_atom = Column(Float, nullable=True, index=True)
    decomposition_energy_per_atom_all = Column(Float, nullable=True)
    decomposition_energy_per_atom_relative = Column(Float, nullable=True)
    decomposition_energy_per_atom_mp = Column(Float, nullable=True)
    decomposition_energy_per_atom_mp_oqmd = Column(Float, nullable=True)
    
    # Electronic properties
    bandgap = Column(Float, nullable=True, index=True)
    
    # Classification and ML
    dimensionality_cheon = Column(Integer, nullable=True, index=True)  # Cheon et al. 2017
    is_train = Column(Boolean, default=False, index=True)  # Training set flag
    
    # Additional properties (stored as JSON for flexibility)
    structure_data = Column(JSON, nullable=True)  # Crystal structure details
    properties_data = Column(JSON, nullable=True)  # Additional calculated properties
    source_metadata = Column(JSON, nullable=True)  # Source-specific metadata
    
    # Data management
    job_id = Column(UUID(as_uuid=True), ForeignKey('data_ingestion_jobs.id'), nullable=True, index=True)
    processing_status = Column(String(20), default="raw", index=True)  # raw, processed, validated, failed
    data_quality_score = Column(Float, nullable=True)  # Quality assessment score
    last_validated = Column(DateTime, nullable=True)
    
    # Timestamps
    created_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)
    fetched_at = Column(DateTime, default=datetime.utcnow, nullable=False)
    
    # Relationships
    job = relationship("Job", back_populates="materials")
    
    # Indexes for performance
    __table_args__ = (
        Index('idx_materials_origin_source_id', 'origin', 'source_id'),
        Index('idx_materials_composition_elements', 'composition', 'elements'),
        Index('idx_materials_formation_energy', 'formation_energy_per_atom'),
        Index('idx_materials_bandgap_range', 'bandgap'),
        Index('idx_materials_space_group_system', 'space_group_number', 'crystal_system'),
    )


# Add relationship to Job model
Job.materials = relationship("MaterialEntry", back_populates="job")


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
