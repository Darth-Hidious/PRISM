import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock

from app.cli import cli
from app.llm import OpenAIService, VertexAIService, AnthropicService

@pytest.fixture
def runner():
    """Fixture for invoking command-line interfaces."""
    return CliRunner()

@patch('app.cli.get_llm_service')
@patch('app.cli.OptimadeClient')
def test_ask_command_openai(mock_optimade_client, mock_get_llm_service, runner):
    """Test the ask command with the OpenAI service."""
    # Mock the LLM service
    mock_llm_service = MagicMock(spec=OpenAIService)
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

@patch('app.cli.get_llm_service')
@patch('app.cli.OptimadeClient')
def test_ask_command_vertexai(mock_optimade_client, mock_get_llm_service, runner):
    """Test the ask command with the VertexAI service."""
    # Mock the LLM service
    mock_llm_service = MagicMock(spec=VertexAIService)
    mock_get_llm_service.return_value = mock_llm_service

    # Mock the LLM responses
    class MockResponse:
        def __init__(self, text):
            self.text = text
            self.choices = [MagicMock(delta=MagicMock(content=text))]

    mock_llm_service.get_completion.side_effect = [
        MockResponse('elements HAS "Si"'),
        [MockResponse("Silicon is a semiconductor.")]
    ]

    # Mock the OptimadeClient
    mock_optimade_client.return_value.get.return_value = {
        "data": [{"attributes": {"chemical_formula_descriptive": "Si"}}]
    }

    result = runner.invoke(cli, ['ask', 'What is silicon?'])
    
    assert result.exit_code == 0
    assert "Generated OPTIMADE filter" in result.output
    assert "Silicon is a semiconductor" in result.output 

@patch('app.cli.get_llm_service')
@patch('app.cli.OptimadeClient')
def test_ask_command_anthropic(mock_optimade_client, mock_get_llm_service, runner):
    """Test the ask command with the Anthropic service."""
    # Mock the LLM service
    mock_llm_service = MagicMock(spec=AnthropicService)
    mock_get_llm_service.return_value = mock_llm_service

    # Mock the LLM responses
    class MockAnthropicResponse:
        def __init__(self, text):
            self.content = [MagicMock(text=text)]

    class MockAnthropicStreamEvent:
        def __init__(self, text):
            self.type = "content_block_delta"
            self.delta = MagicMock(text=text)

    class MockAnthropicStream:
        def __enter__(self):
            return self
        
        def __exit__(self, exc_type, exc_val, exc_tb):
            pass

        def __iter__(self):
            yield MockAnthropicStreamEvent("Silicon is a semiconductor.")

    mock_llm_service.get_completion.side_effect = [
        MockAnthropicResponse('elements HAS "Si"'),
        MockAnthropicStream()
    ]

    # Mock the OptimadeClient
    mock_optimade_client.return_value.get.return_value = {
        "data": [{"attributes": {"chemical_formula_descriptive": "Si"}}]
    }

    result = runner.invoke(cli, ['ask', 'What is silicon?'])
    
    assert result.exit_code == 0
    assert "Generated OPTIMADE filter" in result.output
    assert "Silicon is a semiconductor" in result.output 