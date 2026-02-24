"""Full-content modal overlay screen."""
from textual.screen import ModalScreen
from textual.containers import VerticalScroll
from textual.widgets import Static
from rich.markdown import Markdown
from rich.panel import Panel
from app.tui.theme import TEXT_DIM, ACCENT_MAGENTA


class FullContentScreen(ModalScreen):
    """Modal overlay showing full content of a truncated card.

    Press Escape to dismiss.
    """

    BINDINGS = [("escape", "dismiss", "Close")]

    DEFAULT_CSS = """
    FullContentScreen {
        align: center middle;
    }

    #overlay-container {
        width: 90%;
        height: 85%;
        background: $surface;
        border: round $accent;
        padding: 1 2;
        overflow-y: auto;
    }
    """

    def __init__(self, content: str, title: str = "", **kwargs):
        super().__init__(**kwargs)
        self.content = content
        self.title_text = title

    def compose(self):
        with VerticalScroll(id="overlay-container"):
            yield Static(
                Panel(
                    Markdown(self.content),
                    title=self.title_text,
                    title_align="left",
                    border_style=ACCENT_MAGENTA,
                    subtitle="Escape to close",
                    subtitle_align="right",
                    padding=(1, 2),
                )
            )

    def action_dismiss(self) -> None:
        self.app.pop_screen()
