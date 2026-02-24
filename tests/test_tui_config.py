"""Tests for TUI configuration."""

def test_default_truncation_lines():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.truncation_lines == 6

def test_default_max_status_tasks():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.max_status_tasks == 5

def test_default_auto_scroll():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.auto_scroll is True

def test_default_image_preview():
    from app.tui.config import TUIConfig
    cfg = TUIConfig()
    assert cfg.image_preview == "system"

def test_config_override():
    from app.tui.config import TUIConfig
    cfg = TUIConfig(truncation_lines=10, image_preview="none")
    assert cfg.truncation_lines == 10
    assert cfg.image_preview == "none"

def test_theme_colors_exist():
    from app.tui.theme import SURFACE, TEXT_PRIMARY, SUCCESS, ERROR, RAINBOW, CARD_BORDERS
    assert SURFACE.startswith("#")
    assert len(RAINBOW) == 10
    assert "input" in CARD_BORDERS
    assert "tool" in CARD_BORDERS
