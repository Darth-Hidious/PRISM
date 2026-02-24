"""Braille dot spinner for the PRISM REPL."""

import threading
import time
from rich.console import Console
from rich.text import Text

BRAILLE_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]

TOOL_VERBS = {
    "search_optimade": "Searching OPTIMADE databases...",
    "query_materials_project": "Querying Materials Project...",
    "predict_property": "Training ML model...",
    "calculate_phase_diagram": "Computing phase diagram...",
    "calculate_equilibrium": "Computing equilibrium...",
    "calculate_gibbs_energy": "Computing Gibbs energy...",
    "literature_search": "Searching literature...",
    "patent_search": "Searching patents...",
    "run_simulation": "Running simulation...",
    "submit_hpc_job": "Submitting HPC job...",
    "validate_dataset": "Validating dataset...",
    "review_dataset": "Reviewing dataset...",
    "export_results_csv": "Exporting results...",
    "import_local_data": "Importing data...",
    "list_predictable_properties": "Analyzing properties...",
}


class Spinner:
    """Animated braille spinner with context-aware verbs."""

    def __init__(self, console: Console):
        self._console = console
        self._verb = "Thinking..."
        self._running = False
        self._thread = None
        self._frame_idx = 0

    def verb_for_tool(self, tool_name: str) -> str:
        return TOOL_VERBS.get(tool_name, "Thinking...")

    def start(self, verb: str = "Thinking..."):
        self._verb = verb
        self._running = True
        self._frame_idx = 0
        self._thread = threading.Thread(target=self._animate, daemon=True)
        self._thread.start()

    def update(self, verb: str):
        self._verb = verb

    def stop(self):
        self._running = False
        if self._thread is not None:
            self._thread.join(timeout=0.2)
            self._thread = None
        # Clear the spinner line
        self._console.print("\r\033[K", end="")

    def _animate(self):
        while self._running:
            frame = BRAILLE_FRAMES[self._frame_idx % len(BRAILLE_FRAMES)]
            text = Text()
            text.append(f" {frame} ", style="bold magenta")
            text.append(self._verb, style="dim italic")
            self._console.print(f"\r\033[K", end="")
            self._console.print(text, end="")
            self._frame_idx += 1
            time.sleep(0.08)
