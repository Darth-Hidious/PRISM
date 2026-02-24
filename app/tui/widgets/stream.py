"""Scrollable stream view for the card-based output."""
from textual.containers import VerticalScroll
from textual.widget import Widget


class StreamView(VerticalScroll):
    """Scrollable container for output cards.

    Auto-scrolls to bottom on new cards. Pauses auto-scroll
    when user scrolls up. Resumes on scroll-to-bottom or new input.
    """

    DEFAULT_CSS = """
    StreamView {
        height: 1fr;
        padding: 0 1;
    }
    """

    auto_scroll = True

    def add_card(self, card: Widget) -> None:
        """Mount a card and optionally scroll to it."""
        self.mount(card)
        if self.auto_scroll:
            card.scroll_visible()

    def on_scroll_up(self) -> None:
        """Pause auto-scroll when user scrolls up."""
        self.auto_scroll = False

    def resume_auto_scroll(self) -> None:
        """Resume auto-scroll (called on new user input)."""
        self.auto_scroll = True
        self.scroll_end(animate=False)
