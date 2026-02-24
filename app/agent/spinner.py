"""Braille dot spinner for the PRISM REPL.

Uses Rich Status for clean single-line animation that doesn't leak
into the output stream.
"""

import sys
from rich.console import Console
from rich.text import Text

BRAILLE_FRAMES = ["\u280b", "\u2819", "\u2839", "\u2838", "\u283c", "\u2834", "\u2826", "\u2827", "\u2807", "\u280f"]

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
    """Animated braille spinner using Rich Status (no thread leaking)."""

    def __init__(self, console: Console):
        self._console = console
        self._status = None

    def verb_for_tool(self, tool_name: str) -> str:
        return TOOL_VERBS.get(tool_name, "Thinking...")

    def start(self, verb: str = "Thinking..."):
        self.stop()  # Clean up any prior spinner
        self._status = self._console.status(
            verb, spinner="dots", spinner_style="bold magenta",
        )
        self._status.start()

    def update(self, verb: str):
        if self._status is not None:
            self._status.update(verb)

    def stop(self):
        if self._status is not None:
            self._status.stop()
            self._status = None
