"""Tests for the modal overlay screen."""


def test_overlay_stores_content():
    from app.tui.screens.overlay import FullContentScreen
    screen = FullContentScreen("Full content here", title="search_optimade")
    assert screen.content == "Full content here"
    assert screen.title_text == "search_optimade"


def test_overlay_stores_empty_title():
    from app.tui.screens.overlay import FullContentScreen
    screen = FullContentScreen("some content")
    assert screen.title_text == ""


def test_overlay_has_escape_binding():
    from app.tui.screens.overlay import FullContentScreen
    screen = FullContentScreen("content")
    binding_keys = [b.key if hasattr(b, "key") else b[0] for b in screen.BINDINGS]
    assert "escape" in binding_keys
