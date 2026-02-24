"""Tests for the main PrismApp."""
import pytest


def test_prism_app_instantiates():
    from app.tui.app import PrismApp
    app = PrismApp()
    assert app is not None


@pytest.mark.asyncio
async def test_prism_app_has_all_widgets():
    """PrismApp composes the expected widget tree."""
    from app.tui.app import PrismApp
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        from app.tui.widgets.header import HeaderWidget
        from app.tui.widgets.stream import StreamView
        from app.tui.widgets.status_bar import StatusBar
        from app.tui.widgets.input_bar import InputBar
        assert app.query_one(HeaderWidget) is not None
        assert app.query_one(StreamView) is not None
        assert app.query_one(StatusBar) is not None
        assert app.query_one(InputBar) is not None
