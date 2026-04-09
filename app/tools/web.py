"""Web browsing tools — Firecrawl (bundled) + DuckDuckGo for search & scraping.

These are TOOLS the LLM calls, not embedded services. The agent
decides when to browse, what to read, and what to extract.

Firecrawl: bundled with PRISM — fast, clean text extraction (local or API)
DuckDuckGo: free search fallback via duckduckgo-search library
"""
import logging
import os

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)

# Firecrawl config — prefers local self-hosted instance (bundled with PRISM),
# falls back to cloud API if key is set.
FIRECRAWL_KEY = os.environ.get("FIRECRAWL_API_KEY", "")
FIRECRAWL_LOCAL_URL = os.environ.get("FIRECRAWL_LOCAL_URL", "http://localhost:3002")
FIRECRAWL_API_URL = os.environ.get("FIRECRAWL_API_URL", "https://api.firecrawl.dev/v1")


def _firecrawl_available() -> bool:
    """Check if local Firecrawl instance is running."""
    try:
        import httpx

        r = httpx.get(f"{FIRECRAWL_LOCAL_URL}/", timeout=2)
        return r.status_code < 500
    except Exception:
        return False


# Cache the check at import time so we don't hit it on every call
_LOCAL_FIRECRAWL = _firecrawl_available()

# Resolve which Firecrawl URL and key to use
if _LOCAL_FIRECRAWL:
    # Local instance — no API key needed
    FIRECRAWL_URL = FIRECRAWL_LOCAL_URL
    FIRECRAWL_ACTIVE = True
    logger.info(f"Firecrawl: using local instance at {FIRECRAWL_URL}")
elif FIRECRAWL_KEY:
    # Cloud API with key
    FIRECRAWL_URL = FIRECRAWL_API_URL
    FIRECRAWL_ACTIVE = True
    logger.info("Firecrawl: using cloud API")
else:
    FIRECRAWL_URL = ""
    FIRECRAWL_ACTIVE = False
    logger.info("Firecrawl: not available, using DuckDuckGo fallback")


def _web_read(**kwargs) -> dict:
    """Read a web page and return clean text content.

    Uses Firecrawl (bundled) if configured, falls back to
    httpx + BeautifulSoup for basic extraction.
    """
    url = kwargs.get("url", "")
    if not url:
        return {"error": "url is required"}

    # Try Firecrawl first (best quality — handles JS, returns markdown)
    if FIRECRAWL_ACTIVE:
        try:
            from firecrawl import FirecrawlApp

            app = FirecrawlApp(
                api_key=FIRECRAWL_KEY or "local",
                api_url=FIRECRAWL_URL,
            )
            result = app.scrape_url(url, params={"formats": ["markdown"]})
            content = result.get("markdown", "") if isinstance(result, dict) else str(result)
            title = ""
            if isinstance(result, dict):
                title = result.get("metadata", {}).get("title", "")
            return {
                "url": url,
                "title": title,
                "content": content[:15000],
                "source": "firecrawl",
                "content_length": len(content),
            }
        except Exception as e:
            logger.warning(f"Firecrawl failed: {e}, falling back to basic fetch")

    # Fallback: httpx + BeautifulSoup
    try:
        import httpx

        r = httpx.get(
            url,
            timeout=15,
            follow_redirects=True,
            headers={
                "User-Agent": "PRISM/2.7 (materials science research; +https://marc27.com)"
            },
        )

        try:
            from bs4 import BeautifulSoup

            soup = BeautifulSoup(r.text, "html.parser")
            # Remove script/style
            for tag in soup(["script", "style", "nav", "footer", "header"]):
                tag.decompose()
            title = soup.title.string if soup.title else ""
            text = soup.get_text(separator="\n", strip=True)
        except ImportError:
            # bs4 not available — basic regex fallback
            import re

            title = ""
            text = re.sub(r"<script[^>]*>.*?</script>", "", r.text, flags=re.DOTALL)
            text = re.sub(r"<style[^>]*>.*?</style>", "", text, flags=re.DOTALL)
            text = re.sub(r"<[^>]+>", " ", text)
            text = re.sub(r"\s+", " ", text).strip()

        if len(text) > 10000:
            text = text[:10000] + "... [truncated]"

        return {
            "url": url,
            "title": title,
            "content": text,
            "source": "basic_fetch",
            "content_length": len(text),
        }
    except Exception as e:
        return {"error": f"Failed to read URL: {e}"}


def _web_search(**kwargs) -> dict:
    """Search the web and return results.

    Uses Firecrawl search if available, falls back to DuckDuckGo
    via the bundled duckduckgo-search library.
    """
    query = kwargs.get("query", "")
    limit = kwargs.get("limit", 5)
    if not query:
        return {"error": "query is required"}

    # Try Firecrawl search first (local or cloud)
    if FIRECRAWL_ACTIVE:
        try:
            from firecrawl import FirecrawlApp

            app = FirecrawlApp(
                api_key=FIRECRAWL_KEY or "local",
                api_url=FIRECRAWL_URL,
            )
            results = app.search(query, params={"limit": limit})
            items = results if isinstance(results, list) else results.get("data", [])
            return {
                "query": query,
                "results": [
                    {
                        "title": r.get("metadata", {}).get("title", "")
                        if isinstance(r, dict)
                        else "",
                        "url": r.get("url", "") if isinstance(r, dict) else "",
                        "snippet": (r.get("markdown", "") if isinstance(r, dict) else str(r))[
                            :200
                        ],
                    }
                    for r in items[:limit]
                ],
                "count": len(items),
                "source": "firecrawl",
            }
        except Exception as e:
            logger.warning(f"Firecrawl search failed: {e}, falling back to DDG")

    # Fallback: DuckDuckGo via bundled duckduckgo-search library
    try:
        from duckduckgo_search import DDGS

        with DDGS() as ddgs:
            raw = list(ddgs.text(query, max_results=limit))

        results = [
            {
                "title": r.get("title", ""),
                "url": r.get("href", ""),
                "snippet": r.get("body", "")[:200],
            }
            for r in raw
        ]
        return {
            "query": query,
            "results": results,
            "count": len(results),
            "source": "duckduckgo",
        }
    except Exception as e:
        logger.warning(f"DDG search failed: {e}, trying raw HTTP")

    # Last resort: raw HTTP to DDG (may get rate limited)
    try:
        import httpx

        r = httpx.get(
            "https://html.duckduckgo.com/html/",
            params={"q": query},
            headers={
                "User-Agent": "PRISM/2.7 (materials science research; +https://marc27.com)"
            },
            timeout=10,
        )

        try:
            from bs4 import BeautifulSoup

            soup = BeautifulSoup(r.text, "html.parser")
            results = []
            for a in soup.select("a.result__a"):
                results.append({
                    "title": a.get_text(strip=True),
                    "url": a.get("href", ""),
                    "snippet": "",
                })
                if len(results) >= limit:
                    break
        except ImportError:
            import re

            results = []
            for match in re.finditer(
                r'class="result__a"[^>]*href="([^"]+)"[^>]*>([^<]+)', r.text
            ):
                url_match, title = match.groups()
                results.append({"title": title.strip(), "url": url_match, "snippet": ""})
                if len(results) >= limit:
                    break

        return {
            "query": query,
            "results": results,
            "count": len(results),
            "source": "duckduckgo_html",
        }
    except Exception as e:
        return {"error": f"All search methods failed: {e}"}


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
                "Searches via Firecrawl (if configured) or DuckDuckGo."
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
