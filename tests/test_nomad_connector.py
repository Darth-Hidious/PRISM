import pytest
import asyncio
from unittest.mock import AsyncMock, MagicMock, patch
from datetime import datetime
from typing import Dict, Any

import httpx

from app.services.connectors.nomad_connector import (
    NOMADConnector, 
    NOMADQueryBuilder, 
    create_nomad_query
)
from app.services.connectors.base_connector import StandardizedMaterial


class TestNOMADQueryBuilder:
    """Test NOMAD query builder functionality."""
    
    def test_elements_filter(self):
        """Test element filtering."""
        builder = NOMADQueryBuilder()
        builder.elements(["Fe", "Ni"], "HAS ANY")
        
        query = builder.build()
        assert "results.material.elements HAS ANY" in query["query"]
        assert '"Fe"' in query["query"]
        assert '"Ni"' in query["query"]
    
    def test_elements_has_all(self):
        """Test HAS ALL element filter."""
        builder = NOMADQueryBuilder()
        builder.elements(["Fe", "O"], "HAS ALL")
        
        query = builder.build()
        assert "results.material.elements HAS ALL" in query["query"]
    
    def test_element_count_filter(self):
        """Test element count filtering."""
        builder = NOMADQueryBuilder()
        builder.element_count(4, "gte")
        
        query = builder.build()
        assert "results.material.n_elements:gte: 4" in query["query"]
    
    def test_formula_filter(self):
        """Test formula filtering."""
        builder = NOMADQueryBuilder()
        builder.formula("Fe2O3")
        
        query = builder.build()
        assert 'results.material.chemical_formula_reduced:"Fe2O3"' in query["query"]
    
    def test_formula_contains(self):
        """Test partial formula matching."""
        builder = NOMADQueryBuilder()
        builder.formula_contains("Fe")
        
        query = builder.build()
        assert "results.material.chemical_formula_reduced:*Fe*" in query["query"]
    
    def test_space_group_number(self):
        """Test space group number filter."""
        builder = NOMADQueryBuilder()
        builder.space_group(225)
        
        query = builder.build()
        assert "results.material.symmetry.space_group_number:225" in query["query"]
    
    def test_space_group_symbol(self):
        """Test space group symbol filter."""
        builder = NOMADQueryBuilder()
        builder.space_group("Fm-3m")
        
        query = builder.build()
        assert 'results.material.symmetry.space_group_symbol:"Fm-3m"' in query["query"]
    
    def test_property_range(self):
        """Test property range filtering."""
        builder = NOMADQueryBuilder()
        builder.property_range("results.properties.electronic.band_gap.value", 1.0, 3.0)
        
        query = builder.build()
        assert "results.properties.electronic.band_gap.value:gte:1.0" in query["query"]
        assert "results.properties.electronic.band_gap.value:lte:3.0" in query["query"]
    
    def test_band_gap_range(self):
        """Test band gap range helper."""
        builder = NOMADQueryBuilder()
        builder.band_gap_range(0.5, 2.0)
        
        query = builder.build()
        assert "results.properties.electronic.band_gap.value:gte:0.5" in query["query"]
        assert "results.properties.electronic.band_gap.value:lte:2.0" in query["query"]
    
    def test_formation_energy_range(self):
        """Test formation energy range helper."""
        builder = NOMADQueryBuilder()
        builder.formation_energy_range(-2.0, 0.0)
        
        query = builder.build()
        assert "results.properties.thermodynamic.formation_energy_per_atom.value:gte:-2.0" in query["query"]
        assert "results.properties.thermodynamic.formation_energy_per_atom.value:lte:0.0" in query["query"]
    
    def test_sections(self):
        """Test required sections."""
        builder = NOMADQueryBuilder()
        builder.add_section("results").add_section("run")
        
        query = builder.build()
        assert query["required"] == "results,run"
    
    def test_pagination(self):
        """Test pagination parameters."""
        builder = NOMADQueryBuilder()
        builder.paginate(page_size=50, page_offset=100)
        
        query = builder.build()
        assert query["page_size"] == 50
        assert query["page_offset"] == 100
    
    def test_complex_query(self):
        """Test complex query building."""
        builder = NOMADQueryBuilder()
        query = (builder
                .elements(["Fe", "O"])
                .element_count(3, "gte")
                .band_gap_range(1.0, 3.0)
                .add_section("results")
                .paginate(page_size=200)
                .build())
        
        assert len(query["query"].split(" AND ")) == 4  # 4 query parts
        assert query["page_size"] == 200
        assert query["required"] == "results"
    
    def test_invalid_operator(self):
        """Test invalid operator handling."""
        builder = NOMADQueryBuilder()
        
        with pytest.raises(ValueError):
            builder.elements(["Fe"], "INVALID")
        
        with pytest.raises(ValueError):
            builder.element_count(4, "invalid")


class TestNOMADConnector:
    """Test NOMAD connector functionality."""
    
    @pytest.fixture
    def nomad_config(self):
        """NOMAD connector configuration."""
        return {
            "base_url": "https://nomad-lab.eu/prod/v1/api/v1",
            "timeout": 30.0,
            "stream_threshold": 100
        }
    
    @pytest.fixture
    def mock_nomad_response(self):
        """Mock NOMAD API response."""
        return {
            "data": [
                {
                    "entry_id": "test-entry-1",
                    "results": {
                        "material": {
                            "chemical_formula_reduced": "Fe2O3",
                            "elements": ["Fe", "O"],
                            "n_elements": 2,
                            "symmetry": {
                                "space_group_symbol": "R-3c",
                                "space_group_number": 167,
                                "crystal_system": "trigonal"
                            }
                        },
                        "properties": {
                            "electronic": {
                                "band_gap": {"value": 2.1}
                            },
                            "thermodynamic": {
                                "formation_energy_per_atom": {"value": -2.5}
                            },
                            "mechanical": {
                                "bulk_modulus": {"value": 150.0}
                            }
                        }
                    },
                    "run": [{
                        "time_run": "2024-01-15T10:30:00Z",
                        "program": {"name": "VASP"}
                    }],
                    "system": [{
                        "atoms": {
                            "lattice_vectors": [
                                [5.038, -2.910, 0.000],
                                [0.000, 5.820, 0.000],
                                [0.000, 0.000, 13.772]
                            ],
                            "positions": [
                                [0.0, 0.0, 0.355],
                                [0.0, 0.0, 0.645],
                                [0.306, 0.0, 0.25]
                            ],
                            "labels": ["Fe", "Fe", "O"],
                            "cell": {"volume": 394.2}
                        }
                    }]
                }
            ],
            "pagination": {
                "total": 1,
                "page_size": 100,
                "page_offset": 0
            }
        }
    
    @pytest.fixture
    def nomad_connector(self, nomad_config):
        """Create NOMAD connector instance."""
        return NOMADConnector(nomad_config)
    
    @pytest.mark.asyncio
    async def test_connect_success(self, nomad_connector):
        """Test successful connection to NOMAD API."""
        with patch.object(httpx.AsyncClient, 'get') as mock_get:
            mock_response = MagicMock()
            mock_response.raise_for_status.return_value = None
            mock_response.json.return_value = {"data": []}
            mock_get.return_value = mock_response
            
            result = await nomad_connector.connect()
            assert result is True
            assert nomad_connector.client is not None
    
    @pytest.mark.asyncio
    async def test_connect_failure(self, nomad_connector):
        """Test connection failure."""
        with patch.object(httpx.AsyncClient, 'get') as mock_get:
            mock_get.side_effect = httpx.RequestError("Connection failed")
            
            result = await nomad_connector.connect()
            assert result is False
    
    @pytest.mark.asyncio
    async def test_disconnect(self, nomad_connector):
        """Test disconnection."""
        nomad_connector.client = AsyncMock()
        
        result = await nomad_connector.disconnect()
        assert result is True
        nomad_connector.client.aclose.assert_called_once()
    
    @pytest.mark.asyncio
    async def test_search_materials_simple(self, nomad_connector, mock_nomad_response):
        """Test simple material search."""
        with patch.object(nomad_connector, '_get_total_count', return_value=1):
            with patch.object(nomad_connector, '_fetch_paginated_materials') as mock_fetch:
                mock_fetch.return_value = [
                    StandardizedMaterial(
                        source_db="nomad",
                        source_id="test-entry-1",
                        formula="Fe2O3",
                        structure=MagicMock(),
                        properties=MagicMock(),
                        metadata=MagicMock()
                    )
                ]
                
                nomad_connector.client = AsyncMock()
                
                materials = await nomad_connector.search_materials(formula="Fe2O3")
                assert len(materials) == 1
                assert materials[0].formula == "Fe2O3"
    
    @pytest.mark.asyncio
    async def test_search_materials_with_query_builder(self, nomad_connector):
        """Test material search with query builder."""
        query_builder = (NOMADQueryBuilder()
                        .elements(["Fe", "O"])
                        .band_gap_range(1.0, 3.0))
        
        with patch.object(nomad_connector, '_get_total_count', return_value=50):
            with patch.object(nomad_connector, '_fetch_paginated_materials') as mock_fetch:
                mock_fetch.return_value = []
                nomad_connector.client = AsyncMock()
                
                await nomad_connector.search_materials(query_builder=query_builder)
                mock_fetch.assert_called_once()
    
    @pytest.mark.asyncio
    async def test_streaming_large_dataset(self, nomad_connector):
        """Test streaming for large datasets."""
        with patch.object(nomad_connector, '_get_total_count', return_value=5000):
            with patch.object(nomad_connector, '_stream_materials') as mock_stream:
                mock_stream.return_value = iter([])  # Empty iterator
                nomad_connector.client = AsyncMock()
                
                await nomad_connector.search_materials(formula="Fe2O3")
                mock_stream.assert_called_once()
    
    @pytest.mark.asyncio
    async def test_get_material_by_id_success(self, nomad_connector, mock_nomad_response):
        """Test getting material by ID successfully."""
        with patch.object(httpx.AsyncClient, 'get') as mock_get:
            mock_response = MagicMock()
            mock_response.status_code = 200
            mock_response.raise_for_status.return_value = None
            mock_response.json.return_value = mock_nomad_response["data"][0]
            mock_get.return_value = mock_response
            
            nomad_connector.client = AsyncMock()
            nomad_connector.client.get = mock_get
            
            material = await nomad_connector.get_material_by_id("test-entry-1")
            assert material is not None
            assert material.source_id == "test-entry-1"
            assert material.formula == "Fe2O3"
    
    @pytest.mark.asyncio
    async def test_get_material_by_id_not_found(self, nomad_connector):
        """Test getting material by ID when not found."""
        with patch.object(httpx.AsyncClient, 'get') as mock_get:
            mock_response = MagicMock()
            mock_response.status_code = 404
            mock_get.return_value = mock_response
            
            nomad_connector.client = AsyncMock()
            nomad_connector.client.get = mock_get
            
            material = await nomad_connector.get_material_by_id("nonexistent")
            assert material is None
    
    @pytest.mark.asyncio
    async def test_fetch_bulk_materials(self, nomad_connector):
        """Test bulk material fetching."""
        with patch.object(nomad_connector, 'search_materials') as mock_search:
            mock_search.return_value = []
            nomad_connector.client = AsyncMock()
            
            await nomad_connector.fetch_bulk_materials(
                elements=["Fe", "O"],
                limit=1000
            )
            mock_search.assert_called_once()
    
    def test_convert_to_standard_material(self, nomad_connector, mock_nomad_response):
        """Test conversion to standardized material format."""
        nomad_entry = mock_nomad_response["data"][0]
        
        material = nomad_connector._convert_to_standard_material(nomad_entry, "test-entry-1")
        
        assert material.source_db == "nomad"
        assert material.source_id == "test-entry-1"
        assert material.formula == "Fe2O3"
        assert material.structure.space_group == "R-3c"
        assert material.properties.band_gap == 2.1
        assert material.properties.formation_energy == -2.5
        assert material.properties.bulk_modulus == 150.0
    
    def test_extract_structure(self, nomad_connector, mock_nomad_response):
        """Test structure extraction."""
        entry = mock_nomad_response["data"][0]
        system_info = entry["system"][0]
        material_info = entry["results"]["material"]
        
        structure = nomad_connector._extract_structure(system_info, material_info)
        
        assert len(structure.lattice_parameters) == 3
        assert len(structure.atomic_positions) == 3
        assert len(structure.atomic_species) == 3
        assert structure.space_group == "R-3c"
        assert structure.crystal_system == "trigonal"
        assert structure.volume == 394.2
    
    def test_extract_properties(self, nomad_connector, mock_nomad_response):
        """Test property extraction."""
        results = mock_nomad_response["data"][0]["results"]
        
        properties = nomad_connector._extract_properties(results)
        
        assert properties.band_gap == 2.1
        assert properties.formation_energy == -2.5
        assert properties.bulk_modulus == 150.0
    
    def test_build_simple_query(self, nomad_connector):
        """Test simple query building from kwargs."""
        query_params = nomad_connector._build_simple_query(
            formula="Fe2O3",
            elements=["Fe", "O"],
            min_elements=2,
            band_gap_min=1.0,
            band_gap_max=3.0,
            limit=50
        )
        
        assert "Fe2O3" in query_params["query"]
        assert "HAS ANY" in query_params["query"]
        assert "n_elements:gte: 2" in query_params["query"]
        assert "band_gap.value:gte:1.0" in query_params["query"]
        assert "band_gap.value:lte:3.0" in query_params["query"]
        assert query_params["page_size"] == 50
    
    def test_parse_nomad_date(self, nomad_connector):
        """Test NOMAD date parsing."""
        # Test ISO format with Z
        date1 = nomad_connector._parse_nomad_date("2024-01-15T10:30:00Z")
        assert date1 is not None
        
        # Test ISO format without Z
        date2 = nomad_connector._parse_nomad_date("2024-01-15T10:30:00")
        assert date2 is not None
        
        # Test invalid format
        date3 = nomad_connector._parse_nomad_date("invalid-date")
        assert date3 is None
    
    def test_is_experimental(self, nomad_connector):
        """Test experimental data detection."""
        # Computational data
        comp_data = {
            "run": [{"program": {"name": "VASP"}}]
        }
        assert nomad_connector._is_experimental(comp_data) is False
        
        # Experimental data
        exp_data = {
            "run": [{"program": {"name": "experimental_xrd"}}]
        }
        assert nomad_connector._is_experimental(exp_data) is True
    
    def test_validate_response(self, nomad_connector, mock_nomad_response):
        """Test response validation."""
        # Valid response
        assert nomad_connector.validate_response(mock_nomad_response) is True
        
        # Invalid response - missing data
        invalid_response = {"pagination": {}}
        assert nomad_connector.validate_response(invalid_response) is False


class TestNOMADIntegration:
    """Test NOMAD connector integration features."""
    
    @pytest.mark.asyncio
    async def test_rate_limiting_integration(self):
        """Test rate limiter integration."""
        from app.services.rate_limiter_integration import RateLimiterManager
        
        config = {"base_url": "https://nomad-lab.eu/prod/v1/api/v1"}
        
        # Mock rate limiter
        mock_rate_limiter = AsyncMock()
        
        connector = NOMADConnector(config, rate_limiter=mock_rate_limiter)
        
        # Rate limiter should be set
        assert connector.rate_limiter is mock_rate_limiter
    
    @pytest.mark.asyncio 
    async def test_streaming_with_pagination(self):
        """Test streaming with proper pagination."""
        config = {"stream_threshold": 10}
        connector = NOMADConnector(config)
        connector.client = AsyncMock()
        
        # Mock paginated responses
        responses = [
            {
                "data": [{"entry_id": f"entry_{i}"} for i in range(10)],
                "pagination": {"total": 25, "page_size": 10, "page_offset": 0}
            },
            {
                "data": [{"entry_id": f"entry_{i}"} for i in range(10, 20)],
                "pagination": {"total": 25, "page_size": 10, "page_offset": 10}
            },
            {
                "data": [{"entry_id": f"entry_{i}"} for i in range(20, 25)],
                "pagination": {"total": 25, "page_size": 10, "page_offset": 20}
            }
        ]
        
        call_count = 0
        async def mock_get(*args, **kwargs):
            nonlocal call_count
            mock_response = MagicMock()
            mock_response.raise_for_status.return_value = None
            mock_response.json.return_value = responses[call_count]
            call_count += 1
            return mock_response
        
        connector.client.get = mock_get
        
        # Count materials from streaming
        materials = []
        async for material in connector._stream_materials({"page_size": 10}):
            materials.append(material)
        
        # Should have streamed all 25 materials
        assert len(materials) == 25


def test_create_nomad_query():
    """Test convenience function for creating query builder."""
    builder = create_nomad_query()
    assert isinstance(builder, NOMADQueryBuilder)
    
    # Test chaining
    query = (builder
            .elements(["Fe"])
            .element_count(2, "gte")
            .build())
    
    assert "HAS ANY" in query["query"]
    assert "n_elements:gte: 2" in query["query"]
