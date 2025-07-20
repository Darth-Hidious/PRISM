"""
Enhanced unit tests for NOMAD connector with real API integration.
Tests specific material properties and validates data consistency.
"""

import pytest
import pytest_asyncio
import asyncio
from unittest.mock import patch, MagicMock
from datetime import datetime
import json

from app.services.connectors.nomad_connector import NOMADConnector
from app.services.enhanced_nomad_connector import EnhancedNOMADConnector
from app.core.config import get_settings


class TestNOMADConnectorReal:
    """Test NOMAD connector with real API calls for specific materials."""
    
    @pytest.fixture
    def nomad_config(self):
        """NOMAD connector configuration."""
        return {
            "base_url": "https://nomad-lab.eu/prod/rae/api/v1",
            "timeout": 30.0,
            "max_retries": 3,
            "requests_per_second": 2.0  # Conservative rate limiting
        }
    
    @pytest_asyncio.fixture
    async def nomad_connector(self, nomad_config):
        """Create and connect basic NOMAD connector for testing."""
        connector = NOMADConnector(nomad_config)
        await connector.connect()
        yield connector
        await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_silicon_search_real_api(self, nomad_connector):
        """Test real API search for Silicon materials."""
        print("\n🔍 Testing NOMAD API with Silicon search...")
        
        # Search for Silicon materials
        materials = await nomad_connector.search_materials(
            elements="Si",
            limit=5
        )
        
        # Validate results
        assert len(materials) > 0, "Should find Silicon materials"
        print(f"✅ Found {len(materials)} Silicon materials")
        
        # Check each material
        for i, material in enumerate(materials):
            print(f"\nMaterial {i+1}:")
            print(f"  ID: {material.source_id}")
            print(f"  Formula: {material.formula}")
            print(f"  Database: {material.source_db}")
            
            # Validate required fields
            assert material.source_id is not None, "Material should have source_id"
            assert material.formula is not None, "Material should have formula"
            assert material.source_db == "NOMAD", "Should be from NOMAD"
            
            # Check if Silicon is in the formula
            assert "Si" in material.formula, f"Formula should contain Si: {material.formula}"
            
            # Test formation energy if available
            if material.properties and material.properties.formation_energy is not None:
                print(f"  Formation energy: {material.properties.formation_energy} eV/atom")
                assert isinstance(material.properties.formation_energy, (int, float)), \
                    "Formation energy should be numeric"
            
            # Test band gap if available
            if material.properties and material.properties.band_gap is not None:
                print(f"  Band gap: {material.properties.band_gap} eV")
                assert material.properties.band_gap >= 0, "Band gap should be non-negative"
                
                # Silicon should have small band gap (around 1.1 eV)
                if material.formula == "Si":
                    assert material.properties.band_gap < 2.0, "Pure Silicon should have small band gap"
            
            # Test structure if available
            if material.structure:
                print(f"  Structure:")
                print(f"    Elements: {material.structure.elements}")
                print(f"    Atom count: {material.structure.atom_count}")
                
                if material.structure.elements:
                    assert "Si" in material.structure.elements, "Structure should contain Silicon"
                
                if material.structure.atom_count:
                    assert material.structure.atom_count > 0, "Should have positive atom count"
    
    @pytest.mark.asyncio
    async def test_sio2_search_real_api(self, nomad_connector):
        """Test real API search for SiO2 materials."""
        print("\n🔍 Testing NOMAD API with SiO2 search...")
        
        # Search for Silicon dioxide materials
        materials = await nomad_connector.search_materials(
            elements="Si,O",
            limit=5
        )
        
        if len(materials) > 0:
            print(f"✅ Found {len(materials)} Si-O materials")
            
            for material in materials:
                print(f"  - {material.formula} (ID: {material.source_id})")
                
                # Check if it contains both Si and O
                formula = material.formula
                if "Si" in formula and "O" in formula:
                    print(f"    ✅ Contains both Si and O")
                    
                    # Check specific SiO2 properties if it's pure silica
                    if formula in ["SiO2", "Si1O2"]:
                        print(f"    🔍 Pure SiO2 found")
                        
                        # SiO2 should have wide band gap (around 9 eV)
                        if material.properties and material.properties.band_gap:
                            print(f"    Band gap: {material.properties.band_gap} eV")
                            assert material.properties.band_gap > 5.0, "SiO2 should have wide band gap"
        else:
            print("⚠️  No Si-O materials found")
    
    @pytest.mark.asyncio
    async def test_health_check_real(self, nomad_connector):
        """Test health check with real API."""
        print("\n🔍 Testing NOMAD health check...")
        
        is_healthy = await nomad_connector.health_check()
        print(f"NOMAD health check result: {is_healthy}")
        
        assert isinstance(is_healthy, bool), "Health check should return boolean"
        assert is_healthy, "NOMAD should be healthy"


class TestNOMADConnectorEnhanced:
    """Test enhanced NOMAD connector with database integration."""
    
    @pytest.fixture
    def enhanced_config(self):
        """Enhanced NOMAD connector configuration."""
        return {
            "base_url": "https://nomad-lab.eu/prod/rae/api/v1",
            "timeout": 30.0,
            "batch_size": 3,
            "progress_callback": lambda processed, total, current: print(f"Progress: {processed}/{total} - {current}")
        }
    
    @pytest.mark.asyncio
    async def test_search_and_store_integration(self, enhanced_config):
        """Test search and store functionality with database integration."""
        print("\n🔍 Testing NOMAD search and store integration...")
        
        # Mock the database to avoid actual database operations in tests
        with patch('app.services.enhanced_nomad_connector.MaterialsService') as mock_service:
            mock_service_instance = MagicMock()
            mock_service.return_value = mock_service_instance
            mock_service_instance.store_materials.return_value = (2, 1, [])
            
            connector = EnhancedNOMADConnector(enhanced_config)
            await connector.connect()
            
            try:
                # Test search and store with small batch
                stored, updated, errors = await connector.search_and_store_materials(
                    elements="Si",
                    limit=3,
                    job_id="test-job-123"
                )
                
                print(f"✅ Search and store completed:")
                print(f"  Stored: {stored}")
                print(f"  Updated: {updated}")
                print(f"  Errors: {len(errors) if errors else 0}")
                
                # Validate results
                assert isinstance(stored, int), "Stored count should be integer"
                assert isinstance(updated, int), "Updated count should be integer"
                assert isinstance(errors, list), "Errors should be list"
                
                # Verify database service was called
                mock_service_instance.store_materials.assert_called()
                
            finally:
                await connector.disconnect()


class TestNOMADErrorHandling:
    """Test NOMAD connector error handling and edge cases."""
    
    @pytest.fixture
    def nomad_config(self):
        return {
            "base_url": "https://nomad-lab.eu/prod/rae/api/v1",
            "timeout": 10.0,
            "max_retries": 2
        }
    
    @pytest.mark.asyncio
    async def test_empty_search_results(self, nomad_config):
        """Test handling of search with no results."""
        print("\n🔍 Testing NOMAD empty search results...")
        
        connector = EnhancedNOMADConnector(nomad_config)
        
        try:
            await connector.connect()
            
            # Search for something that likely doesn't exist
            materials = await connector.search_materials(
                elements="Unobtainium",  # Non-existent element
                limit=5
            )
            
            # Should return empty list, not error
            assert isinstance(materials, list), "Should return list"
            assert len(materials) == 0, "Should return empty list for non-existent elements"
            print(f"✅ Empty search returned {len(materials)} results")
            
        finally:
            await connector.disconnect()
    
    @pytest.mark.asyncio
    async def test_invalid_parameters(self, nomad_config):
        """Test handling of invalid search parameters."""
        print("\n🔍 Testing NOMAD invalid parameters...")
        
        connector = EnhancedNOMADConnector(nomad_config)
        
        try:
            await connector.connect()
            
            # Test with invalid limit
            materials = await connector.search_materials(
                elements="Si",
                limit=0  # Invalid limit
            )
            
            # Should handle gracefully
            assert isinstance(materials, list), "Should return list even with invalid limit"
            print(f"✅ Invalid limit handled gracefully")
            
        except ValueError as e:
            # Some implementations might raise ValueError for invalid params
            print(f"✅ ValueError correctly raised for invalid parameters: {e}")
            assert "limit" in str(e).lower(), "Error should mention limit parameter"
        
        finally:
            await connector.disconnect()


@pytest.mark.integration
class TestNOMADIntegration:
    """Integration tests for NOMAD connector."""
    
    @pytest.mark.asyncio
    async def test_full_workflow_silicon(self):
        """Test complete workflow for Silicon materials via NOMAD."""
        print("\n🔍 Testing full NOMAD Silicon workflow...")
        
        config = {
            "base_url": "https://nomad-lab.eu/prod/rae/api/v1",
            "timeout": 30.0,
            "requests_per_second": 1.0  # Be gentle with API
        }
        
        connector = EnhancedNOMADConnector(config)
        
        try:
            # Step 1: Connect
            success = await connector.connect()
            assert success, "Should connect to NOMAD"
            print("✅ Connected to NOMAD")
            
            # Step 2: Health check
            is_healthy = await connector.health_check()
            assert is_healthy, "NOMAD should be healthy"
            print("✅ NOMAD health check passed")
            
            # Step 3: Search for Silicon
            materials = await connector.search_materials(
                elements="Si",
                limit=5
            )
            
            assert len(materials) > 0, "Should find Silicon materials"
            print(f"✅ Found {len(materials)} Silicon materials")
            
            # Step 4: Validate each material
            for i, material in enumerate(materials):
                print(f"\nValidating material {i+1}: {material.formula}")
                
                # Test basic properties
                assert material.source_id, "Should have material source_id"
                assert material.formula, "Should have chemical formula"
                assert material.metadata, "Should have metadata"
                assert material.source_db == "NOMAD", "Should be from NOMAD"
                
                # Test NOMAD ID format (should be UUID-like)
                assert len(material.source_id) > 10, "NOMAD source_id should be substantial length"
                
                # Test Silicon presence
                assert "Si" in material.formula, "Should contain Silicon"
                
                # Test structure if available
                if material.structure:
                    assert material.structure.elements, "Should have elements list"
                    assert "Si" in material.structure.elements, "Should contain Silicon in structure"
                    
                    if material.structure.atom_count:
                        assert material.structure.atom_count > 0, "Should have positive atom count"
                
                # Test properties if available
                if material.properties:
                    if material.properties.formation_energy is not None:
                        assert isinstance(material.properties.formation_energy, (int, float)), \
                            "Formation energy should be numeric"
                    
                    if material.properties.band_gap is not None:
                        assert material.properties.band_gap >= 0, "Band gap should be non-negative"
                        
                        # For pure Silicon, expect small band gap
                        if material.formula == "Si":
                            assert material.properties.band_gap < 3.0, "Pure Si should have small band gap"
                
                print(f"  ✅ Material {i+1} validation passed")
            
            print(f"✅ Full NOMAD workflow completed successfully")
            
        finally:
            await connector.disconnect()


if __name__ == "__main__":
    # Run specific tests
    pytest.main([__file__ + "::TestNOMADConnectorReal::test_silicon_search_real_api", "-v", "-s"])
