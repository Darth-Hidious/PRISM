"""Search tools: literature and patent search."""
from app.tools.base import Tool, ToolRegistry


def _literature_search(**kwargs) -> dict:
    """Search scientific literature on arXiv and Semantic Scholar."""
    from app.data.literature_collector import LiteratureCollector
    collector = LiteratureCollector()
    results = collector.collect(**kwargs)
    return {
        "results": results,
        "count": len(results),
        "query": kwargs.get("query", ""),
    }


def _patent_search(**kwargs) -> dict:
    """Search patents via Lens.org."""
    from app.data.patent_collector import PatentCollector
    collector = PatentCollector()
    results = collector.collect(**kwargs)
    return {
        "results": results,
        "count": len(results),
        "query": kwargs.get("query", ""),
    }


def create_search_tools(registry: ToolRegistry) -> None:
    """Register literature and patent search tools."""
    registry.register(Tool(
        name="literature_search",
        description=(
            "Search scientific literature (arXiv, Semantic Scholar) for papers "
            "related to materials science topics. Returns titles, authors, "
            "abstracts, and citation counts."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query, e.g. 'tungsten rhenium alloy phase stability'",
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max papers to return (default 20)",
                    "default": 20,
                },
                "sources": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Sources to search: arxiv, semantic_scholar (default both)",
                },
            },
            "required": ["query"],
        },
        func=_literature_search,
    ))
    registry.register(Tool(
        name="patent_search",
        description=(
            "Search patents via Lens.org for materials-related patents. "
            "Requires LENS_API_TOKEN env var. Returns titles, abstracts, "
            "inventors, and applicants."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Patent search query, e.g. 'high entropy alloy coating'",
                },
                "max_results": {
                    "type": "integer",
                    "description": "Max patents to return (default 20)",
                    "default": 20,
                },
            },
            "required": ["query"],
        },
        func=_patent_search,
    ))
