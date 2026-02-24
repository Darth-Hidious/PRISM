"""Braille dot spinner for the PRISM REPL."""

import sys
import threading
import time
from rich.console import Console

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
    """Animated braille spinner with context-aware verbs.

    Uses raw sys.stdout writes so carriage-return overwrites work
    correctly (Rich console.print mangles \\r/\\033[K escape codes).
    """

    def __init__(self, console: Console):
        self._console = console
        self._verb = "Thinking..."
        self._running = False
        self._thread = None
        self._frame_idx = 0

    def verb_for_tool(self, tool_name: str) -> str:
        return TOOL_VERBS.get(tool_name, "Thinking...")

    def start(self, verb: str = "Thinking..."):
        self.stop()
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
            self._thread.join(timeout=0.3)
            self._thread = None
        # Clear the spinner line
        sys.stdout.write("\r\033[K")
        sys.stdout.flush()

    def _animate(self):
        while self._running:
            frame = BRAILLE_FRAMES[self._frame_idx % len(BRAILLE_FRAMES)]
            sys.stdout.write(f"\r\033[K {frame} \033[2;3m{self._verb}\033[0m")
            sys.stdout.flush()
            self._frame_idx += 1
            time.sleep(0.08)
