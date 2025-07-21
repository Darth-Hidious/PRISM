from typing import List, Dict, Any, Optional
from app.prompts import SUMMARIZATION_PROMPT

class ModelContext:
    """
    The Model Context Protocol (MCP) for materials science data.

    This class encapsulates the data and metadata required by the LLM to
    understand and reason about materials science data.
    """

    def __init__(self, query: str, results: List[Dict[str, Any]], rag_context: Optional[str] = None):
        self.query = query
        self.results = results
        self.rag_context = rag_context

    def to_prompt(self) -> str:
        """
        Constructs a prompt for the LLM by summarizing the search results and
        including optional RAG context.
        """
        # Truncate results to avoid exceeding token limits
        summarized_results = [
            {
                "formula": r.get("attributes", {}).get("chemical_formula_descriptive", "N/A"),
                "provider": r.get("meta", {}).get("provider", {}).get("name", "N/A"),
                "elements": r.get("attributes", {}).get("elements", [])
            }
            for r in self.results[:20] # Limit to first 20 results for brevity
        ]
        
        results_str = "\n".join([f"- {r['formula']} (from {r['provider']})" for r in summarized_results])
        
        return SUMMARIZATION_PROMPT.format(
            query=self.query,
            results=results_str,
            rag_context=self.rag_context if self.rag_context else "No additional context provided."
        ) 