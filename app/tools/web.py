"""Web browsing tools — Firecrawl + Playwright for reading web content.

These are TOOLS the LLM calls, not embedded services. The agent
decides when to browse, what to read, and what to extract.

Firecrawl: fast, clean text extraction from URLs (API-based)
Playwright: full browser automation for JS-heavy sites (local)
"""
import logging
import os

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)

FIRECRAWL_KEY = os.environ.get("FIRECRAWL_API_KEY", "")
FIRECRAWL_URL = "https://api.firecrawl.dev/v1"


def _web_read(**kwargs) -> dict:
    """Read a web page and return clean text content.

    Uses Firecrawl if available (fast, handles JS), falls back to
    basic HTTP fetch + HTML stripping.
    """
    url = kwargs.get("url", "")
    if not url:
        return {"error": "url is required"}

    # Try Firecrawl first (best quality, handles JS rendering)
    if FIRECRAWL_KEY:
        try:
            import httpx

            r = httpx.post(
                f"{FIRECRAWL_URL}/scrape",
                headers={
                    "Authorization": f"Bearer {FIRECRAWL_KEY}",
                    "Content-Type": "application/json",
                },
                json={"url": url, "formats": ["markdown"]},
                timeout=30,
            )
            if r.status_code == 200:
                data = r.json().get("data", {})
                return {
                    "url": url,
                    "title": data.get("metadata", {}).get("title", ""),
                    "content": data.get("markdown", ""),
                    "source": "firecrawl",
                    "content_length": len(data.get("markdown", "")),
                }
        except Exception as e:
            logger.warning(f"Firecrawl failed: {e}, falling back to basic fetch")

    # Fallback: basic HTTP fetch + strip HTML
    try:
        import httpx

        r = httpx.get(
            url,
            timeout=15,
            follow_redirects=True,
            headers={"User-Agent": "PRISM/2.5 (materials science research agent; +https://marc27.com)"},
        )
        text = r.text

        # Strip HTML tags (basic)
        import re

        text = re.sub(r"<script[^>]*>.*?</script>", "", text, flags=re.DOTALL)
        text = re.sub(r"<style[^>]*>.*?</style>", "", text, flags=re.DOTALL)
        text = re.sub(r"<[^>]+>", " ", text)
        text = re.sub(r"\s+", " ", text).strip()

        # Truncate to reasonable size
        if len(text) > 10000:
            text = text[:10000] + "... [truncated]"

        return {
            "url": url,
            "title": "",
            "content": text,
            "source": "basic_fetch",
            "content_length": len(text),
        }
    except Exception as e:
        return {"error": f"Failed to read URL: {e}"}


def _web_search(**kwargs) -> dict:
    """Search the web and return results.

    Uses Firecrawl search if available, falls back to DuckDuckGo.
    """
    query = kwargs.get("query", "")
    limit = kwargs.get("limit", 5)
    if not query:
        return {"error": "query is required"}

    # Try Firecrawl search
    if FIRECRAWL_KEY:
        try:
            import httpx

            r = httpx.post(
                f"{FIRECRAWL_URL}/search",
                headers={
                    "Authorization": f"Bearer {FIRECRAWL_KEY}",
                    "Content-Type": "application/json",
                },
                json={"query": query, "limit": limit},
                timeout=15,
            )
            if r.status_code == 200:
                results = r.json().get("data", [])
                return {
                    "query": query,
                    "results": [
                        {
                            "title": r.get("metadata", {}).get("title", ""),
                            "url": r.get("url", ""),
                            "snippet": r.get("markdown", "")[:200],
                        }
                        for r in results[:limit]
                    ],
                    "count": len(results),
                    "source": "firecrawl",
                }
        except Exception as e:
            logger.warning(f"Firecrawl search failed: {e}")

    # Fallback: DuckDuckGo (no API key needed)
    try:
        import httpx

        r = httpx.get(
            "https://html.duckduckgo.com/html/",
            params={"q": query},
            headers={"User-Agent": "PRISM/2.5"},
            timeout=10,
        )
        # Basic parsing of DDG HTML results
        import re

        results = []
        for match in re.finditer(
            r'<a class="result__a" href="([^"]+)"[^>]*>([^<]+)</a>', r.text
        ):
            url, title = match.groups()
            results.append({"title": title.strip(), "url": url, "snippet": ""})
            if len(results) >= limit:
                break

        return {
            "query": query,
            "results": results,
            "count": len(results),
            "source": "duckduckgo",
        }
    except Exception as e:
        return {"error": f"Search failed: {e}"}


def create_web_tools(registry: ToolRegistry) -> None:
    """Register web browsing tools."""

    registry.register(
        Tool(
            name="web_read",
            description=(
                "Read a web page and return clean text content. Handles JavaScript-heavy "
                "sites, strips HTML, and returns markdown. Use this to read papers, docs, "
                "Wikipedia articles, or any web page. Returns clean text ready for analysis."
            ),
            input_schema={
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to read (https://...)",
                    },
                },
                "required": ["url"],
            },
            func=_web_read,
        )
    )

    registry.register(
        Tool(
            name="web_search",
            description=(
                "Search the web for information. Returns titles, URLs, and snippets. "
                "Use to find papers, datasets, documentation, or any web content. "
                "Searches via Firecrawl (if configured) or DuckDuckGo (always available)."
            ),
            input_schema={
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query",
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (default 5)",
                        "default": 5,
                    },
                },
                "required": ["query"],
            },
            func=_web_search,
        )
    )
