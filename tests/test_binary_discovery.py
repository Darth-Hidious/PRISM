"""Test binary discovery for the compiled Ink TUI."""
from unittest.mock import patch
from pathlib import Path


def test_has_tui_binary_returns_false_when_missing():
    from app.cli._binary import has_tui_binary
    with patch("app.cli._binary.tui_binary_path", return_value=None):
        assert has_tui_binary() is False


def test_has_tui_binary_returns_true_when_present(tmp_path):
    binary = tmp_path / "prism-tui"
    binary.write_text("#!/bin/sh\necho hi")
    binary.chmod(0o755)
    from app.cli._binary import has_tui_binary
    with patch("app.cli._binary._bin_dir", return_value=tmp_path):
        assert has_tui_binary() is True


def test_tui_binary_path_checks_user_override(tmp_path):
    binary = tmp_path / "prism-tui"
    binary.write_text("#!/bin/sh")
    binary.chmod(0o755)
    from app.cli._binary import tui_binary_path
    with patch("app.cli._binary._user_bin_dir", return_value=tmp_path), \
         patch("app.cli._binary._bin_dir", return_value=Path("/nonexistent")):
        result = tui_binary_path()
        assert result == binary
