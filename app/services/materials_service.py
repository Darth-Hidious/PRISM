"""
Materials database service for storing and managing materials data.
"""

import logging
from datetime import datetime, timezone
from typing import List, Optional, Dict, Any, Tuple
from uuid import uuid4
import hashlib
import json

from sqlalchemy.orm import Session
from sqlalchemy import and_, or_, func, distinct
from sqlalchemy.exc import IntegrityError

from app.db.database import get_db_session_sync
from app.db.models import MaterialEntry, Job
from app.services.connectors.base_connector import StandardizedMaterial

logger = logging.getLogger(__name__)


class MaterialsService:
    """Service for managing materials in the database."""
    
    def __init__(self):
        self.batch_size = 100
        
    def generate_material_id(self, origin: str, source_id: str, formula: str) -> str:
        """Generate a unique material ID based on source and content."""
        # Create a hash from origin, source_id, and formula to ensure uniqueness
        content = f"{origin}:{source_id}:{formula}"
        hash_obj = hashlib.md5(content.encode())
        return f"{origin}_{hash_obj.hexdigest()[:12]}"
    
    def store_materials(
        self, 
        materials: List[StandardizedMaterial], 
        job_id: Optional[str] = None,
        batch_size: Optional[int] = None
    ) -> Tuple[int, int, List[str]]:
        """
        Store a list of materials in the database.
        
        Args:
            materials: List of standardized materials
            job_id: Associated job ID
            batch_size: Number of materials to process in each batch
            
        Returns:
            Tuple of (stored_count, updated_count, error_ids)
        """
        if not materials:
            return 0, 0, []
            
        batch_size = batch_size or self.batch_size
        stored_count = 0
        updated_count = 0
        error_ids = []
        
        # Process in batches
        for i in range(0, len(materials), batch_size):
            batch = materials[i:i + batch_size]
            try:
                batch_stored, batch_updated, batch_errors = self._store_batch(batch, job_id)
                stored_count += batch_stored
                updated_count += batch_updated
                error_ids.extend(batch_errors)
                
                logger.info(f"Processed batch {i//batch_size + 1}: {batch_stored} stored, {batch_updated} updated")
                
            except Exception as e:
                logger.error(f"Error processing batch {i//batch_size + 1}: {e}")
                error_ids.extend([mat.id for mat in batch])
                
        return stored_count, updated_count, error_ids
    
    def _store_batch(
        self, 
        materials: List[StandardizedMaterial], 
        job_id: Optional[str]
    ) -> Tuple[int, int, List[str]]:
        """Store a batch of materials."""
        stored_count = 0
        updated_count = 0
        error_ids = []
        
        with get_db_session_sync() as db:
            for material in materials:
                try:
                    # Generate unique material ID
                    material_id = self.generate_material_id(
                        material.chemical_formula,
                        material.metadata.database,
                        material.id
                    )
                    
                    # Check if material already exists
                    existing = db.query(MaterialEntry).filter(
                        MaterialEntry.material_id == material_id
                    ).first()
                    
                    if existing:
                        # Update existing material
                        self._update_material_entry(existing, material, job_id)
                        updated_count += 1
                    else:
                        # Create new material
                        material_entry = self._create_material_entry(material, material_id, job_id)
                        db.add(material_entry)
                        stored_count += 1
                        
                except Exception as e:
                    logger.error(f"Error storing material {material.id}: {e}")
                    error_ids.append(material.id)
                    
            try:
                db.commit()
            except IntegrityError as e:
                logger.error(f"Integrity error in batch: {e}")
                db.rollback()
                # Add all material IDs to error list
                error_ids.extend([mat.id for mat in materials])
                stored_count = 0
                updated_count = 0
                
        return stored_count, updated_count, error_ids
    
    def _create_material_entry(
        self, 
        material: StandardizedMaterial, 
        material_id: str, 
        job_id: Optional[str]
    ) -> MaterialEntry:
        """Create a new MaterialEntry from StandardizedMaterial."""
        
        # Extract elements and create alphabetically ordered composition
        elements = []
        composition_parts = []
        
        if material.structure and material.structure.elements:
            elements = sorted(list(set(material.structure.elements)))
            # Create a simple alphabetical composition
            composition_parts = elements
        
        # Extract properties
        properties = material.properties or {}
        
        return MaterialEntry(
            material_id=material_id,
            origin=material.metadata.database,
            source_id=material.id,
            
            # Composition
            composition=" ".join(composition_parts),
            reduced_formula=material.chemical_formula,
            elements=elements,
            nsites=material.structure.atom_count if material.structure else None,
            
            # Physical properties
            volume=material.structure.volume if material.structure else None,
            density=material.structure.density if material.structure else None,
            
            # Symmetry
            space_group=material.structure.space_group if material.structure else None,
            space_group_number=None,  # Extract from space_group if needed
            crystal_system=material.structure.crystal_system if material.structure else None,
            
            # Energy properties
            formation_energy_per_atom=properties.formation_energy,
            bandgap=properties.band_gap,
            
            # Store full structure and properties as JSON
            structure_data=self._structure_to_dict(material.structure) if material.structure else None,
            properties_data=self._properties_to_dict(properties),
            source_metadata=self._metadata_to_dict(material.metadata),
            
            # Management fields
            job_id=job_id,
            processing_status="processed",
            fetched_at=material.metadata.fetched_at,
        )
    
    def _update_material_entry(
        self, 
        existing: MaterialEntry, 
        material: StandardizedMaterial, 
        job_id: Optional[str]
    ):
        """Update an existing material entry with new data."""
        # Update timestamps
        existing.updated_at = datetime.utcnow()
        existing.fetched_at = material.metadata.fetched_at
        
        # Update job association if provided
        if job_id:
            existing.job_id = job_id
            
        # Update properties if they have new values
        properties = material.properties or {}
        if properties.formation_energy is not None:
            existing.formation_energy_per_atom = properties.formation_energy
        if properties.band_gap is not None:
            existing.bandgap = properties.band_gap
            
        # Update structure data
        if material.structure:
            existing.structure_data = self._structure_to_dict(material.structure)
            if material.structure.volume:
                existing.volume = material.structure.volume
            if material.structure.density:
                existing.density = material.structure.density
                
        # Update metadata
        existing.source_metadata = self._metadata_to_dict(material.metadata)
        existing.properties_data = self._properties_to_dict(properties)
    
    def _structure_to_dict(self, structure) -> Dict[str, Any]:
        """Convert MaterialStructure to dictionary."""
        if not structure:
            return {}
            
        return {
            "lattice_parameters": structure.lattice_parameters,
            "lattice_angles": structure.lattice_angles,
            "atom_count": structure.atom_count,
            "volume": structure.volume,
            "density": structure.density,
            "elements": structure.elements,
            "space_group": structure.space_group,
            "crystal_system": structure.crystal_system,
            "atomic_positions": structure.atomic_positions,
        }
    
    def _properties_to_dict(self, properties) -> Dict[str, Any]:
        """Convert MaterialProperties to dictionary."""
        if not properties:
            return {}
            
        return {
            "formation_energy": properties.formation_energy,
            "band_gap": properties.band_gap,
            "total_energy": properties.total_energy,
            "magnetic_moment": properties.magnetic_moment,
            "calculated_properties": properties.calculated_properties or {},
        }
    
    def _metadata_to_dict(self, metadata) -> Dict[str, Any]:
        """Convert MaterialMetadata to dictionary."""
        if not metadata:
            return {}
            
        return {
            "database": metadata.database,
            "calculation_method": metadata.calculation_method,
            "source_url": metadata.source_url,
            "fetched_at": metadata.fetched_at.isoformat() if metadata.fetched_at else None,
            "additional_info": metadata.additional_info or {},
        }
    
    def search_materials(
        self,
        formula: Optional[str] = None,
        elements: Optional[List[str]] = None,
        origin: Optional[str] = None,
        min_bandgap: Optional[float] = None,
        max_bandgap: Optional[float] = None,
        min_formation_energy: Optional[float] = None,
        max_formation_energy: Optional[float] = None,
        crystal_system: Optional[str] = None,
        limit: int = 100,
        offset: int = 0
    ) -> Tuple[List[MaterialEntry], int]:
        """
        Search materials in the database.
        
        Returns:
            Tuple of (materials, total_count)
        """
        with get_db_session_sync() as db:
            query = db.query(MaterialEntry)
            
            # Apply filters
            if formula:
                query = query.filter(MaterialEntry.reduced_formula.ilike(f"%{formula}%"))
                
            if elements:
                # Filter by elements (materials that contain ANY of the specified elements)
                for element in elements:
                    query = query.filter(MaterialEntry.elements.contains([element]))
                    
            if origin:
                query = query.filter(MaterialEntry.origin == origin)
                
            if min_bandgap is not None:
                query = query.filter(MaterialEntry.bandgap >= min_bandgap)
                
            if max_bandgap is not None:
                query = query.filter(MaterialEntry.bandgap <= max_bandgap)
                
            if min_formation_energy is not None:
                query = query.filter(MaterialEntry.formation_energy_per_atom >= min_formation_energy)
                
            if max_formation_energy is not None:
                query = query.filter(MaterialEntry.formation_energy_per_atom <= max_formation_energy)
                
            if crystal_system:
                query = query.filter(MaterialEntry.crystal_system == crystal_system)
            
            # Get total count
            total_count = query.count()
            
            # Apply pagination and get results
            materials = query.offset(offset).limit(limit).all()
            
            return materials, total_count
    
    def get_statistics(self) -> Dict[str, Any]:
        """Get database statistics."""
        with get_db_session_sync() as db:
            total_materials = db.query(func.count(MaterialEntry.id)).scalar()
            
            # Count by origin
            origin_counts = db.query(
                MaterialEntry.origin,
                func.count(MaterialEntry.id)
            ).group_by(MaterialEntry.origin).all()
            
            # Count by crystal system
            crystal_system_counts = db.query(
                MaterialEntry.crystal_system,
                func.count(MaterialEntry.id)
            ).group_by(MaterialEntry.crystal_system).all()
            
            # Count by processing status
            status_counts = db.query(
                MaterialEntry.processing_status,
                func.count(MaterialEntry.id)
            ).group_by(MaterialEntry.processing_status).all()
            
            return {
                "total_materials": total_materials,
                "by_origin": dict(origin_counts),
                "by_crystal_system": dict(crystal_system_counts),
                "by_status": dict(status_counts),
            }
