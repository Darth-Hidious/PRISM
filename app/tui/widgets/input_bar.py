"""Input bar widget pinned to the bottom of the screen."""
from textual.widgets import Input


class InputBar(Input):
    """Text input bar for user messages.

    Pinned at the very bottom. On submit, the message is sent to
    PrismApp which creates an InputCard in the stream and clears
    this widget.
    """

    DEFAULT_CSS = """
    InputBar {
        dock: bottom;
        height: 3;
        padding: 0 1;
    }
    """

    def __init__(self, **kwargs):
        super().__init__(
            placeholder="Ask PRISM anything...",
            **kwargs,
        )
