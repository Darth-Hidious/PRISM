"""
Tests for the PRISM Command-Line Interface.

This test suite focuses on the CLI commands, particularly the `search` command,
and its integration with the OPTIMADE client.
"""

import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock
from sqlalchemy.orm import sessionmaker

from app.cli import cli
from app.db.database import engine, Base
from app.db.models import Material

# Create a new database session for testing
TestingSessionLocal = sessionmaker(autocommit=False, autoflush=False, bind=engine)

@pytest.fixture
def runner():
    """Fixture for invoking command-line interfaces."""
    return CliRunner()


@pytest.fixture(scope="function")
def test_db():
    """Fixture for creating a new database session for testing."""
    Base.metadata.create_all(bind=engine)
    yield
    Base.metadata.drop_all(bind=engine)

def get_mock_optimade_client(filter_str):
    """Creates a mock OptimadeClient for testing."""
    mock_client = MagicMock()
    
    mock_data = [
        {"id": "mp-149", "type": "structures", "attributes": {"chemical_formula_descriptive": "Si", "elements": ["Si"], "nelements": 1}},
        {"id": "mp-123", "type": "structures", "attributes": {"chemical_formula_descriptive": "SiC", "elements": ["Si", "C"], "nelements": 2}},
        {"id": "mp-456", "type": "structures", "attributes": {"chemical_formula_descriptive": "SiO2", "elements": ["Si", "O"], "nelements": 2}},
        {"id": "mp-789", "type": "structures", "attributes": {"chemical_formula_descriptive": "Si2O4", "elements": ["Si", "O"], "nelements": 2}},
        {"id": "mp-101", "type": "structures", "attributes": {"chemical_formula_descriptive": "Si2", "elements": ["Si"], "nelements": 1}},
    ]

    def parse_filter(filter_str, data):
        if 'HAS ALL' in filter_str:
            elements = filter_str.split('"')[1::2]
            return all(elem in data['elements'] for elem in elements)
        elif 'chemical_formula_descriptive' in filter_str:
            formula = filter_str.split('"')[1]
            return data['chemical_formula_descriptive'] == formula
        elif 'nelements' in filter_str:
            nelements = int(filter_str.split('=')[1])
            return data['nelements'] == nelements
        return False

    if filter_str:
        filtered_data = [d for d in mock_data if parse_filter(filter_str, d["attributes"])]
    else:
        filtered_data = mock_data

    mock_client.get.return_value = {"data": filtered_data}
    return mock_client

@patch('app.db.database.SessionLocal', new=TestingSessionLocal)
@patch('app.cli.OptimadeClient')
def test_search_by_elements(mock_optimade_constructor, runner):
    """Test the search command with the --elements option."""
    mock_client = get_mock_optimade_client(filter_str='elements HAS ALL "Si", "O"')
    mock_optimade_constructor.return_value = mock_client
    
    result = runner.invoke(cli, ['search', '--elements', 'Si,O'])
    
    assert result.exit_code == 0
    assert "Found 2 materials" in result.output

@patch('app.db.database.SessionLocal', new=TestingSessionLocal)
@patch('app.cli.OptimadeClient')
def test_search_by_formula(mock_optimade_constructor, runner):
    """Test the search command with the --formula option."""
    mock_client = get_mock_optimade_client(filter_str='chemical_formula_descriptive="Si2"')
    mock_optimade_constructor.return_value = mock_client
    
    result = runner.invoke(cli, ['search', '--formula', 'Si2'])
    
    assert result.exit_code == 0
    assert "Found 1 material" in result.output 

@patch('app.db.database.SessionLocal', new=TestingSessionLocal)
@patch('app.cli.OptimadeClient')
def test_search_by_nelements(mock_optimade_constructor, runner):
    """Test the search command with the --nelements option."""
    mock_client = get_mock_optimade_client(filter_str='nelements=1')
    mock_optimade_constructor.return_value = mock_client
    
    result = runner.invoke(cli, ['search', '--nelements', '1'])
    
    assert result.exit_code == 0
    assert "Found 2 materials" in result.output 

@patch('app.db.database.SessionLocal', new=TestingSessionLocal)
@patch('app.cli.OptimadeClient')
def test_search_no_results(mock_optimade_constructor, runner):
    """Test the search command with no results."""
    mock_client = MagicMock()
    mock_client.get.return_value = {"data": []}
    mock_optimade_constructor.return_value = mock_client
    
    result = runner.invoke(cli, ['search', '--elements', 'Xy'])
    
    assert result.exit_code == 0
    assert "No materials found for the given filter." in result.output 

@patch('app.db.database.SessionLocal', new=TestingSessionLocal)
@patch('app.cli.OptimadeClient')
def test_search_api_error(mock_optimade_constructor, runner):
    """Test the search command with an API error."""
    mock_client = MagicMock()
    mock_client.get.side_effect = Exception("API Error")
    mock_optimade_constructor.return_value = mock_client
    
    result = runner.invoke(cli, ['search', '--elements', 'Si'])
    
    assert result.exit_code == 0
    assert "An error occurred: API Error" in result.output 

@patch('app.db.database.SessionLocal', new=TestingSessionLocal)
@patch('app.cli.OptimadeClient')
def test_search_and_save(mock_optimade_constructor, runner, test_db):
    """Test the search command with the --save option."""
    mock_client = get_mock_optimade_client(filter_str='chemical_formula_descriptive="Si2"')
    mock_optimade_constructor.return_value = mock_client
    
    result = runner.invoke(cli, ['search', '--formula', 'Si2'], input='y\n')
    
    assert result.exit_code == 0
    assert "Results saved to the database." in result.output
    
    db = TestingSessionLocal()
    materials = db.query(Material).all()
    assert len(materials) == 1
    assert materials[0].formula == "Si2"
    db.close()