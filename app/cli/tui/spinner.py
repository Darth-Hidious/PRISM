"""Animated spinner for the PRISM REPL.

Uses Rich Status for clean single-line animation.
"""

from rich.console import Console
from app.cli.tui.theme import PRIMARY

TOOL_VERBS = {
    "search_optimade": "Searching OPTIMADE databases\u2026",
    "query_materials_project": "Querying Materials Project\u2026",
    "predict_property": "Training ML model\u2026",
    "calculate_phase_diagram": "Computing phase diagram\u2026",
    "calculate_equilibrium": "Computing equilibrium\u2026",
    "calculate_gibbs_energy": "Computing Gibbs energy\u2026",
    "literature_search": "Searching literature\u2026",
    "patent_search": "Searching patents\u2026",
    "run_simulation": "Running simulation\u2026",
    "submit_hpc_job": "Submitting HPC job\u2026",
    "validate_dataset": "Validating dataset\u2026",
    "review_dataset": "Reviewing dataset\u2026",
    "export_results_csv": "Exporting results\u2026",
    "import_local_data": "Importing data\u2026",
    "list_predictable_properties": "Analyzing properties\u2026",
}


class Spinner:
    """Animated spinner using Rich Status (no thread leaking)."""

    def __init__(self, console: Console):
        self._console = console
        self._status = None

    def verb_for_tool(self, tool_name: str) -> str:
        return TOOL_VERBS.get(tool_name, "Thinking\u2026")

    def start(self, verb: str = "Thinking\u2026"):
        self.stop()
        self._status = self._console.status(
            verb, spinner="dots", spinner_style=f"bold {PRIMARY}",
        )
        self._status.start()

    def update(self, verb: str):
        if self._status is not None:
            self._status.update(verb)

    def stop(self):
        if self._status is not None:
            self._status.stop()
            self._status = None
