"""Tests for prism search --refresh CLI flag."""
from unittest.mock import patch, AsyncMock
from click.testing import CliRunner


def test_refresh_flag_triggers_discovery():
    from app.commands.search import search
    runner = CliRunner()

    mock_endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"},
    ]

    with patch("app.commands.search.discover_providers", new_callable=AsyncMock, return_value=mock_endpoints) as mock_discover:
        with patch("app.commands.search.save_cache") as mock_save:
            with patch("app.commands.search.load_overrides", return_value={
                "fallback_index_urls": {},
                "overrides": {},
                "defaults": {},
            }):
                result = runner.invoke(search, ["--refresh"])
                assert mock_discover.called
                assert mock_save.called
                assert result.exit_code == 0


def test_refresh_flag_shows_provider_count():
    from app.commands.search import search
    runner = CliRunner()

    mock_endpoints = [
        {"id": "mp", "name": "MP", "base_url": "https://mp.org", "parent": "mp"},
        {"id": "cod", "name": "COD", "base_url": "https://cod.org", "parent": "cod"},
    ]

    with patch("app.commands.search.discover_providers", new_callable=AsyncMock, return_value=mock_endpoints):
        with patch("app.commands.search.save_cache"):
            with patch("app.commands.search.load_overrides", return_value={
                "fallback_index_urls": {},
                "overrides": {},
                "defaults": {},
            }):
                result = runner.invoke(search, ["--refresh"])
                assert "2" in result.output  # should mention count
