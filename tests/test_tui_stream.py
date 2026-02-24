"""Tests for the stream view widget."""


def test_stream_view_instantiates():
    from app.tui.widgets.stream import StreamView
    sv = StreamView()
    assert sv is not None


def test_stream_view_auto_scroll_default():
    from app.tui.widgets.stream import StreamView
    sv = StreamView()
    assert sv.auto_scroll is True
