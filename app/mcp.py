from typing import List, Dict, Any
from app.prompts import SUMMARIZATION_PROMPT

class ModelContext:
    """
    The Model Context Protocol (MCP) for materials science data.

    This class encapsulates the data and metadata required by the LLM to
    understand and reason about materials science data.
    """

    def __init__(self, query: str, results: List[Dict[str, Any]]):
        self.query = query
        self.results = results

    def to_prompt(self) -> str:
        """
        Converts the model context to a prompt for the LLM.
        """
        results_str = ""
        for result in self.results:
            attributes = result.get('attributes', {})
            formula = attributes.get('chemical_formula_descriptive', 'N/A')
            elements = ', '.join(attributes.get('elements', []))
            band_gap = attributes.get('band_gap', 'N/A')
            results_str += f"- {formula} (Elements: {elements}, Band Gap: {band_gap} eV)\n"
        
        return SUMMARIZATION_PROMPT.format(query=self.query, results=results_str) 