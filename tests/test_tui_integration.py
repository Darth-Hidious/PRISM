"""Integration smoke test for the full TUI."""
import pytest


@pytest.mark.asyncio
async def test_full_tui_renders_all_zones():
    """PrismApp renders header, stream, status, and input."""
    from app.tui.app import PrismApp
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        from app.tui.widgets.header import HeaderWidget
        from app.tui.widgets.stream import StreamView
        from app.tui.widgets.status_bar import StatusBar
        from app.tui.widgets.input_bar import InputBar
        assert app.query_one(HeaderWidget)
        assert app.query_one(StreamView)
        assert app.query_one(StatusBar)
        assert app.query_one(InputBar)
        # Input bar has focus
        assert isinstance(app.focused, InputBar)


@pytest.mark.asyncio
async def test_submit_creates_input_card():
    """Submitting text creates an InputCard in the stream."""
    from app.tui.app import PrismApp
    from app.tui.widgets.cards import InputCard
    from app.tui.widgets.input_bar import InputBar
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        input_bar = app.query_one(InputBar)
        input_bar.value = "Find W-Rh alloys"
        await pilot.press("enter")
        await pilot.pause()
        cards = app.query(InputCard)
        assert len(cards) == 1
        assert cards[0].message == "Find W-Rh alloys"


@pytest.mark.asyncio
async def test_slash_help_command():
    """Typing /help creates an OutputCard with help text."""
    from app.tui.app import PrismApp
    from app.tui.widgets.cards import OutputCard
    from app.tui.widgets.input_bar import InputBar
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        input_bar = app.query_one(InputBar)
        input_bar.value = "/help"
        await pilot.press("enter")
        await pilot.pause()
        cards = app.query(OutputCard)
        assert len(cards) >= 1


@pytest.mark.asyncio
async def test_ctrl_l_clears_stream():
    """Ctrl+L clears the stream."""
    from app.tui.app import PrismApp
    from app.tui.widgets.stream import StreamView
    from app.tui.widgets.input_bar import InputBar
    async with PrismApp().run_test(size=(120, 40)) as pilot:
        app = pilot.app
        input_bar = app.query_one(InputBar)
        input_bar.value = "/help"
        await pilot.press("enter")
        await pilot.pause()
        await pilot.press("ctrl+l")
        await pilot.pause()
        stream = app.query_one(StreamView)
        assert len(stream.children) == 0
