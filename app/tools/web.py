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


def _firecrawl_base() -> str:
    """Firecrawl REST base ending in /v1 (local URL lacks the prefix)."""
    base = FIRECRAWL_URL.rstrip("/")
    return base if base.endswith("/v1") else f"{base}/v1"


def _firecrawl_headers() -> dict:
    return {"Authorization": f"Bearer {FIRECRAWL_KEY}"} if FIRECRAWL_KEY else {}


def _firecrawl_available() -> bool:
    """Check if a local Firecrawl instance is REALLY running.

    Probes the API route, not `/`: anything can squat the port (a Next.js
    dev server on 3002 passed the old `GET /` check, which silently broke
    every web_search/web_read primary path). Real Firecrawl answers the
    scrape route with JSON (400/401/422 for an empty body); an unrelated
    app answers 404 HTML.
    """
    try:
        import httpx

        base = FIRECRAWL_LOCAL_URL.rstrip("/")
        if not base.endswith("/v1"):
            base = f"{base}/v1"
        r = httpx.post(f"{base}/scrape", json={}, timeout=2)
        return r.status_code != 404 and "json" in r.headers.get("content-type", "")
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


# Default characters returned from one page read. Not a ceiling on what PRISM
# can see: `offset` walks the rest of the document and `max_chars` raises the
# window. The point of a default is to keep one incidental page from filling
# the context, never to put a document out of reach.
_DEFAULT_READ_CHARS = 15000


def _window(text: str, offset: int, max_chars: int) -> dict:
    """Return a slice of `text` plus an HONEST account of what was left out.

    Both read paths used to lie in different directions: the Firecrawl path
    sliced to 15000 chars with no marker at all, and the basic-fetch path
    marked the cut but computed `content_length` AFTER slicing, so it reported
    10015 for every page regardless of true size — three different Wikipedia
    articles all came back "10015 chars". Either way the caller could not tell
    how much it was missing, and had no way to ask for the rest.
    """
    total = len(text)
    offset = max(0, min(offset, total))
    body = text[offset:] if max_chars <= 0 else text[offset : offset + max_chars]
    end = offset + len(body)
    truncated = end < total
    if truncated:
        body += (
            f"\n\n... [truncated at {end} of {total} chars — "
            f"re-read this url with offset={end} for the rest]"
        )
    return {
        "content": body,
        # The TRUE size of the document, never the size of what we returned.
        "content_length": total,
        "returned_chars": len(body),
        "offset": offset,
        "truncated": truncated,
        "next_offset": end if truncated else None,
    }


def _web_read(**kwargs) -> dict:
    """Read a web page and return clean text content.

    Uses Firecrawl (bundled) if configured, falls back to
    httpx + BeautifulSoup for basic extraction.
    """
    url = kwargs.get("url", "")
    if not url:
        return {"error": "url is required"}
    offset = int(kwargs.get("offset", 0) or 0)
    # `or _DEFAULT_READ_CHARS` swallowed the documented `max_chars=0`
    # ("0 returns the entire document") because 0 is falsy, so the whole-document
    # request came back windowed to 15000 chars AND flagged truncated. Only an
    # absent/None value may fall back to the default.
    raw_max_chars = kwargs.get("max_chars")
    max_chars = _DEFAULT_READ_CHARS if raw_max_chars is None else int(raw_max_chars)

    # Try Firecrawl first (best quality — handles JS, returns markdown).
    # Direct REST call — the firecrawl-py SDK has renamed this API twice
    # (scrape_url → scrape, params → kwargs), and each rename silently
    # knocked the primary path down to the basic fallback. The HTTP API
    # is stable.
    if FIRECRAWL_ACTIVE:
        try:
            import httpx

            r = httpx.post(
                f"{_firecrawl_base()}/scrape",
                json={"url": url, "formats": ["markdown"]},
                headers=_firecrawl_headers(),
                timeout=30,
            )
            r.raise_for_status()
            data = r.json().get("data", {}) or {}
            content = data.get("markdown", "") or ""
            title = (data.get("metadata") or {}).get("title", "")
            if not content:
                raise ValueError("firecrawl returned no markdown")
            return {
                "url": url,
                "title": title,
                "source": "firecrawl",
                **_window(content, offset, max_chars),
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
        # An HTTP error page is NOT content. Without this the error body was
        # returned under the ordinary read shape and the caller could not tell a
        # refusal from a real page: measured, a 403 came back as content:"",
        # content_length:0, truncated:false (indistinguishable from a genuinely
        # empty page) and a 404 came back as a readable "Page not found" article.
        # The tool's own description warns that repositories 403 this
        # User-Agent — that is exactly the case that must not read as success.
        if r.status_code >= 400:
            # A publisher refusing a bot is not the work being unavailable.
            # See `_open_access_location`: this exact 403 lost the two most
            # relevant papers of a live run, both of them open access.
            error = {
                "error": f"HTTP {r.status_code} reading {url}",
                "url": url,
                "status_code": r.status_code,
                "source": "basic_fetch",
            }
            open_copy = _open_access_location(url)
            if not open_copy:
                return error
            if open_copy["is_pdf"]:
                # Not parsed here — `papers_ingest` is the tool that reads PDFs
                # properly and records provenance. Name it so the refusal is
                # actionable instead of terminal.
                error["open_access_url"] = open_copy["url"]
                error["recovery"] = (
                    "the publisher refused this request, but the work is open "
                    "access: read the PDF above with papers_ingest"
                )
                return error
            try:
                oa_response = httpx.get(
                    open_copy["url"],
                    timeout=15,
                    follow_redirects=True,
                    headers={
                        "User-Agent": "PRISM/2.7 (materials science research; "
                        "+https://marc27.com)"
                    },
                )
                if oa_response.status_code >= 400:
                    error["open_access_url"] = open_copy["url"]
                    return error
            except Exception:
                error["open_access_url"] = open_copy["url"]
                return error
            title, text = _html_to_text(oa_response.text)
            # Honest about what was actually read: this is the open copy of the
            # same work, NOT the URL that was asked for.
            return {
                "url": open_copy["url"],
                "requested_url": url,
                "title": title or open_copy["title"],
                "source": "open_access_fallback",
                **_window(text, offset, max_chars),
            }

        title, text = _html_to_text(r.text)

        return {
            "url": url,
            "title": title,
            "source": "basic_fetch",
            **_window(text, offset, max_chars),
        }
    except Exception as e:
        return {"error": f"Failed to read URL: {e}"}


def _html_to_text(html: str) -> tuple[str, str]:
    """Title and readable text from an HTML page."""
    try:
        from bs4 import BeautifulSoup

        soup = BeautifulSoup(html, "html.parser")
        # Remove script/style
        for tag in soup(["script", "style", "nav", "footer", "header"]):
            tag.decompose()
        title = soup.title.string if soup.title else ""
        return title, soup.get_text(separator="\n", strip=True)
    except ImportError:
        # bs4 not available — basic regex fallback
        import re

        text = re.sub(r"<script[^>]*>.*?</script>", "", html, flags=re.DOTALL)
        text = re.sub(r"<style[^>]*>.*?</style>", "", text, flags=re.DOTALL)
        text = re.sub(r"<[^>]+>", " ", text)
        return "", re.sub(r"\s+", " ", text).strip()


_DOI_PATTERN = r"10\.\d{4,9}/[^\s\"'<>&?#]+"


def _open_access_location(url: str) -> dict | None:
    """Where the same work is legally readable, when the publisher refuses us.

    Measured 2026-08-28 during a live PFAS run. PRISM asked for two DOIs, took
    the publisher's 403, and stopped:

        HTTP 403 reading https://doi.org/10.1039/d6su00094k
        HTTP 403 reading https://doi.org/10.1039/d5ra08575f

    They were "Siloxanes: viable alternatives for PFAS in essential
    applications?" and "Non-stick performance of polymethylsilsesquioxane thin
    films" — the two most on-topic papers of the entire run. **Both are open
    access**, with a direct PDF one metadata call away. The run instead read a
    paper on sorghum root architecture.

    A 403 at doi.org is the publisher's landing page refusing a bot, not the
    work being unavailable, and PRISM already talks to OpenAlex as a search
    source. So on refusal, ask where the open copy is. Returns `None` — never
    a guess — when the URL carries no DOI, when the lookup fails, when the work
    is not open, or when the open copy is the URL that just refused us.
    """
    import re

    match = re.search(_DOI_PATTERN, url)
    if not match:
        return None
    doi = match.group(0).rstrip(".")
    try:
        import httpx

        # OpenAlex asks for a contact for its polite pool. Sent only when the
        # operator has configured one; never invented from the local user.
        params = {}
        mailto = os.environ.get("PRISM_MAILTO", "").strip()
        if mailto:
            params["mailto"] = mailto
        response = httpx.get(
            f"https://api.openalex.org/works/doi:{doi}",
            params=params,
            timeout=15,
            follow_redirects=True,
        )
        if response.status_code >= 400:
            return None
        work = response.json()
        if not isinstance(work, dict):
            return None
    except Exception as error:  # network, JSON, or a stubbed client in tests
        logger.warning(f"open-access lookup failed for {doi}: {error}")
        return None

    best = work.get("best_oa_location") or {}
    open_access = work.get("open_access") or {}
    location = best.get("pdf_url") or open_access.get("oa_url") or best.get("landing_page_url")
    if not isinstance(location, str) or not location or location == url:
        return None
    return {
        "url": location,
        "title": work.get("title") or "",
        "is_pdf": location.lower().endswith(".pdf") or "articlepdf" in location.lower(),
    }


def _web_search(**kwargs) -> dict:
    """Search the web and return results.

    Uses Firecrawl search if available, falls back to DuckDuckGo
    via the bundled duckduckgo-search library.
    """
    query = kwargs.get("query", "")
    limit = kwargs.get("limit", 5)
    if not query:
        return {"error": "query is required"}

    # Try Firecrawl search first (local or cloud). Direct REST — see
    # _web_read for why the SDK is not used.
    if FIRECRAWL_ACTIVE:
        try:
            import httpx

            r = httpx.post(
                f"{_firecrawl_base()}/search",
                json={"query": query, "limit": limit},
                headers=_firecrawl_headers(),
                timeout=30,
            )
            r.raise_for_status()
            data = r.json().get("data", [])
            # v1 returns a list; v2 nests under "web".
            items = data.get("web", []) if isinstance(data, dict) else data
            if not items:
                raise ValueError("firecrawl search returned no results")
            return {
                "query": query,
                "results": [
                    {
                        "title": r.get("title", ""),
                        "url": r.get("url", ""),
                        "snippet": (r.get("description") or r.get("markdown") or "")[:200],
                    }
                    for r in items[:limit]
                    if isinstance(r, dict)
                ],
                "count": len(items),
                "source": "firecrawl",
            }
        except Exception as e:
            logger.warning(f"Firecrawl search failed: {e}, falling back to DDG")

    # Fallback: DuckDuckGo. The library was renamed duckduckgo_search →
    # ddgs; try the new name first, fall back to the legacy one.
    try:
        try:
            from ddgs import DDGS
        except ImportError:
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


def _web(**kwargs) -> dict:
    """Unified web dispatcher. Replaces web_read + web_search."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: read, search",
            "hint": (
                "web(action='read', url='https://...') — fetch one page as clean text. "
                "web(action='search', query='...') — search the web for relevant URLs."
            ),
        }
    if action == "read":
        if not kwargs.get("url"):
            return {"error": "Action 'read' requires `url`"}
        return _web_read(**kwargs)
    if action == "search":
        if not kwargs.get("query"):
            return {"error": "Action 'search' requires `query`"}
        return _web_search(**kwargs)
    return {"error": f"Unknown action '{action}'. Valid: read, search"}


_WEB_DESCRIPTION = (
    "Open-web access. ONE tool, two actions:\n"
    "  • action='read' — fetch a single URL and return clean text content. "
    "Handles JavaScript-heavy sites, strips HTML, returns markdown. Requires "
    "`url`. Use to read papers, docs, Wikipedia articles, blog posts.\n"
    "  • action='search' — query the open web; returns titles, URLs, snippets. "
    "Requires `query`. Optional `limit` (default 5). Searches via Firecrawl "
    "(if configured) or DuckDuckGo.\n"
    "Typical sequence: action='search' → pick the best URL → action='read' on "
    "that URL. NOT for scientific papers (use prior_art_search for better "
    "metadata + DOIs) and NOT for the platform KG (use query with scope=platform). "
    "KNOWN BLOCKERS: search engines and government repositories block this "
    "tool's User-Agent — do NOT call action='read' on google.com/search, "
    "bing.com/search, duckduckgo.com, osti.gov/servlets/* or osti.gov/biblio/* "
    "(every one returns robots.txt or 403; observed cost in real runs: ~15 "
    "wasted calls per question). Use prior_art_search or research instead. "
    "The CrossRef API (api.crossref.org/works) IS accessible and is the "
    "right place for DOI-based citation lookups."
)


def create_web_tools(registry: ToolRegistry) -> None:
    """Register the unified `web` tool (replaces web_read + web_search)."""
    registry.register(Tool(
        name="web",
        description=_WEB_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "search"],
                    "description": "Which web operation to perform.",
                },
                "url": {
                    "type": "string",
                    "description": "URL to read for action='read' (https://...).",
                },
                "query": {
                    "type": "string",
                    "description": "Search query for action='search'.",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results for action='search' (default 5).",
                    "default": 5,
                },
                "offset": {
                    "type": "integer",
                    "description": (
                        "action='read': start reading this many characters into the "
                        "page. A truncated result reports next_offset — pass it back "
                        "to continue through a long document."
                    ),
                    "default": 0,
                },
                "max_chars": {
                    "type": "integer",
                    "description": (
                        "action='read': characters to return in this window "
                        f"(default {_DEFAULT_READ_CHARS}). Raise it for a data table "
                        "you need whole; 0 returns the entire document."
                    ),
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_web,
    ))
