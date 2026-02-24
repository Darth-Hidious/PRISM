"""Status bar with agent step spinner and task tracker."""
from textual.widget import Widget
from textual.reactive import reactive
from rich.text import Text
from app.tui.theme import TEXT_DIM, SUCCESS, WARNING, ACCENT_MAGENTA

BRAILLE_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]


class StatusBar(Widget):
    """Bottom status area: agent step + task tracker."""

    DEFAULT_CSS = """
    StatusBar {
        dock: bottom;
        height: auto;
        max-height: 12;
        padding: 0 1;
    }
    """

    current_step = reactive("")
    next_step = reactive("")
    spinner_frame = reactive(0)
    is_thinking = reactive(False)

    def __init__(self, max_visible_tasks: int = 5, **kwargs):
        super().__init__(**kwargs)
        self.max_visible_tasks = max_visible_tasks
        self.tasks: list[dict] = []

    def update_agent_step(self, step: str) -> None:
        self.current_step = step
        self.is_thinking = True

    def update_next_step(self, step: str) -> None:
        self.next_step = step

    def stop_thinking(self) -> None:
        self.is_thinking = False
        self.current_step = ""
        self.next_step = ""

    def add_task(self, label: str) -> int:
        idx = len(self.tasks)
        self.tasks.append({"label": label, "status": "pending"})
        self.refresh()
        return idx

    def start_task(self, idx: int) -> None:
        if 0 <= idx < len(self.tasks):
            self.tasks[idx]["status"] = "running"
            self.refresh()

    def complete_task(self, idx: int) -> None:
        if 0 <= idx < len(self.tasks):
            self.tasks[idx]["status"] = "done"
            self.refresh()

    def get_display_tasks(self, max_visible: int | None = None) -> list[dict]:
        """Return tasks sorted: running -> pending -> done, truncated."""
        order = {"running": 0, "pending": 1, "done": 2}
        sorted_tasks = sorted(self.tasks, key=lambda t: order.get(t["status"], 3))
        limit = max_visible or self.max_visible_tasks
        return sorted_tasks[:limit]

    def advance_spinner(self) -> None:
        self.spinner_frame = (self.spinner_frame + 1) % len(BRAILLE_FRAMES)

    def render(self) -> Text:
        t = Text()
        if self.is_thinking:
            frame = BRAILLE_FRAMES[self.spinner_frame]
            t.append(f"  {frame} ", style=f"bold {ACCENT_MAGENTA}")
            t.append(self.current_step or "Thinking...", style="italic")
            if self.current_step:
                t.append(f"\n     └── {self.current_step}", style=TEXT_DIM)
                t.append("  [Ctrl+O]", style=TEXT_DIM)
            if self.next_step:
                t.append(f"\n     └── Next: {self.next_step}", style=TEXT_DIM)
            t.append("\n")

        if self.tasks:
            done = sum(1 for t_ in self.tasks if t_["status"] == "done")
            total = len(self.tasks)
            t.append(f"\n  ◉ Tasks [{done}/{total}]", style="bold")
            t.append("  [Ctrl+Q]\n", style=TEXT_DIM)
            for task in self.get_display_tasks():
                if task["status"] == "done":
                    t.append(f"    ✔ {task['label']}\n", style=SUCCESS)
                elif task["status"] == "running":
                    t.append(f"    ▸ {task['label']}...\n", style=WARNING)
                else:
                    t.append(f"    ○ {task['label']}\n", style=TEXT_DIM)
            remaining = total - len(self.get_display_tasks())
            if remaining > 0:
                t.append(f"    ... +{remaining} more\n", style=TEXT_DIM)
        return t
