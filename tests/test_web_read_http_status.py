"""The basic-fetch read path must not present an HTTP error page as content.

`_web_read`'s httpx fallback never looked at the status code, so a refusal
arrived in the ordinary read shape. Measured against the live web before the
fix: `https://httpbin.org/status/403` came back as
``content: "", content_length: 0, truncated: false`` — indistinguishable from a
genuinely empty page — and a 404 came back as a readable "Page not found"
article the model could quote. The `web` tool's own description warns that
repositories 403 this User-Agent, so that is precisely the case that must not
read as success.
"""

import httpx

from app.tools import web


class _Resp:
    def __init__(self, status_code: int, text: str = "") -> None:
        self.status_code = status_code
        self.text = text


def test_an_empty_403_is_an_error_not_an_empty_page(monkeypatch):
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(httpx, "get", lambda *a, **k: _Resp(403, ""))

    out = web._web(action="read", url="https://example.invalid/blocked")

    assert "error" in out, f"an HTTP 403 must not read as a successful page: {out}"
    assert out["status_code"] == 403
    assert "403" in out["error"]
    # No read shape at all: an error must not look like a successful window.
    assert "content" not in out
    assert "content_length" not in out
    assert "truncated" not in out


def test_a_404_body_is_never_returned_as_content(monkeypatch):
    body = (
        "<html><head><title>Page not found</title></head>"
        "<body>This page does not exist.</body></html>"
    )
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(httpx, "get", lambda *a, **k: _Resp(404, body))

    out = web._web(action="read", url="https://example.invalid/missing")

    assert "error" in out, f"an HTTP 404 body must not be returned as content: {out}"
    assert out["status_code"] == 404
    assert "404" in out["error"]
    assert "does not exist" not in str(out.get("content", ""))
    assert "Page not found" not in str(out.get("title", ""))


def test_a_2xx_page_still_reads_normally(monkeypatch):
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(
        httpx, "get", lambda *a, **k: _Resp(200, "<html><body>real content</body></html>")
    )

    out = web._web(action="read", url="https://example.invalid/ok")

    assert "error" not in out
    assert out["content"] == "real content"
    assert out["truncated"] is False
