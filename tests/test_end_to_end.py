import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock

from app.cli import cli
from app.db.database import get_db, Base, engine
from app.db.models import Material

@pytest.fixture(scope="module")
def runner():
    """Fixture for invoking command-line interfaces."""
    return CliRunner()

@pytest.fixture(scope="module")
def db_session():
    """Fixture for creating a new database session for each test module."""
    Base.metadata.create_all(bind=engine)
    db = next(get_db())
    yield db
    db.close()
    Base.metadata.drop_all(bind=engine)

def test_db_commands(runner, db_session):
    """Test the db command group."""
    # Test `db init`
    result = runner.invoke(cli, ['db', 'init'])
    assert result.exit_code == 0
    assert "Database initialized." in result.output

    # Test `db status`
    result = runner.invoke(cli, ['db', 'status'])
    assert result.exit_code == 0
    assert "Database connection successful." in result.output

    # Test `db clear`
    result = runner.invoke(cli, ['db', 'clear'], input="y\n")
    assert result.exit_code == 0
    assert "Database cleared." in result.output

@patch('app.cli.OptimadeClient')
def test_search_command(mock_optimade_client, runner, db_session):
    """Test the search command."""
    mock_optimade_client.return_value.get.return_value = {
        "data": [
            {"id": "mp-149", "attributes": {"chemical_formula_descriptive": "Si"}},
            {"id": "mp-123", "attributes": {"chemical_formula_descriptive": "SiC"}},
        ]
    }
    
    result = runner.invoke(cli, ['search', 'elements HAS "Si"'])
    assert result.exit_code == 0
    assert "Found 2 materials" in result.output

    # Check that the materials were saved to the database
    materials = db_session.query(Material).all()
    assert len(materials) == 2
    assert materials[0].formula == "Si"
    assert materials[1].formula == "SiC"

@patch('app.cli.get_llm_service')
@patch('app.cli.OptimadeClient')
def test_ask_command_end_to_end(mock_optimade_client, mock_get_llm_service, runner):
    """Test the ask command end-to-end."""
    # Mock the LLM service
    mock_llm_service = MagicMock()
    mock_get_llm_service.return_value = mock_llm_service

    # Mock the LLM responses
    mock_llm_service.get_completion.side_effect = [
        MagicMock(choices=[MagicMock(message=MagicMock(content='elements HAS "Si"'))]),
        [MagicMock(choices=[MagicMock(delta=MagicMock(content="Silicon is a semiconductor."))])]
    ]

    # Mock the OptimadeClient
    mock_optimade_client.return_value.get.return_value = {
        "data": [{"attributes": {"chemical_formula_descriptive": "Si"}}]
    }

    result = runner.invoke(cli, ['ask', 'What is silicon?'])
    
    assert result.exit_code == 0
    assert "Generated OPTIMADE filter" in result.output
    assert "Silicon is a semiconductor" in result.output 