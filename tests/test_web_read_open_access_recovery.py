"""A publisher's 403 is not the work being unavailable.

Measured 2026-08-28 in a live PFAS-alternatives run. PRISM asked for two DOIs,
took the publisher's refusal, and stopped:

    HTTP 403 reading https://doi.org/10.1039/d6su00094k
    HTTP 403 reading https://doi.org/10.1039/d5ra08575f

Those are "Siloxanes: viable alternatives for PFAS in essential applications?"
and "Non-stick performance of polymethylsilsesquioxane thin films" — the two
most on-topic papers of the entire run, matching the agent's own queries almost
word for word. **Both are open access**, each with a direct PDF one metadata
call away. Having abandoned them, the run went and read a paper on sorghum root
architecture instead.

So the refusal must be recoverable, and recovery must never become invention:
no DOI, no open copy, or a failed lookup all leave the original error exactly
as it was.
"""

import httpx

from app.tools import web

SILOXANE_DOI = "https://doi.org/10.1039/d6su00094k"
OPEN_PDF = "https://pubs.rsc.org/en/content/articlepdf/2026/su/d6su00094k"


class _Resp:
    def __init__(self, status_code: int, text: str = "", payload=None) -> None:
        self.status_code = status_code
        self.text = text
        self._payload = payload

    def json(self):
        if self._payload is None:
            raise ValueError("not json")
        return self._payload


def _openalex(pdf_url=OPEN_PDF, is_oa=True, title="Siloxanes: viable alternatives for PFAS"):
    """One OpenAlex work record, shaped as the live API returns it."""
    return {
        "title": title,
        "open_access": {"is_oa": is_oa, "oa_url": pdf_url if is_oa else None},
        "best_oa_location": {"pdf_url": pdf_url} if is_oa else {},
    }


def _routed(routes):
    """Dispatch httpx.get by URL prefix; anything unrouted is a 403."""

    def fake_get(url, *args, **kwargs):
        for prefix, response in routes.items():
            if str(url).startswith(prefix):
                return response
        return _Resp(403, "")

    return fake_get


def test_an_open_access_pdf_is_named_so_the_refusal_is_actionable(monkeypatch):
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(
        httpx,
        "get",
        _routed({"https://api.openalex.org": _Resp(200, payload=_openalex())}),
    )

    out = web._web(action="read", url=SILOXANE_DOI)

    # Still an error — we did not read what was asked for, and must not pretend to.
    assert out["status_code"] == 403
    assert "content" not in out, f"a refusal must not acquire a read shape: {out}"
    # But no longer terminal.
    assert out["open_access_url"] == OPEN_PDF
    assert "papers_ingest" in out["recovery"]


def test_an_open_access_html_copy_is_actually_read(monkeypatch):
    landing = "https://example.org/open/siloxanes"
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(
        httpx,
        "get",
        _routed(
            {
                "https://api.openalex.org": _Resp(200, payload=_openalex(pdf_url=landing)),
                landing: _Resp(
                    200,
                    "<html><title>Siloxanes</title><body>PFAS alternatives.</body></html>",
                ),
            }
        ),
    )

    out = web._web(action="read", url=SILOXANE_DOI)

    assert "error" not in out, f"the open copy was readable: {out}"
    assert "PFAS alternatives" in out["content"]
    # Honest about WHICH document was read. Reporting the requested URL here
    # would attribute the open copy's text to a page that returned 403.
    assert out["url"] == landing
    assert out["requested_url"] == SILOXANE_DOI
    assert out["source"] == "open_access_fallback"


def test_a_closed_work_is_left_as_the_plain_refusal(monkeypatch):
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(
        httpx,
        "get",
        _routed({"https://api.openalex.org": _Resp(200, payload=_openalex(is_oa=False))}),
    )

    out = web._web(action="read", url=SILOXANE_DOI)

    assert out["status_code"] == 403
    assert "open_access_url" not in out, f"nothing open to offer, so offer nothing: {out}"
    assert "recovery" not in out


def test_a_failed_lookup_never_invents_a_location(monkeypatch):
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    # OpenAlex itself down: the original refusal must survive intact.
    monkeypatch.setattr(httpx, "get", _routed({"https://api.openalex.org": _Resp(503, "")}))

    out = web._web(action="read", url=SILOXANE_DOI)

    assert out["status_code"] == 403
    assert "open_access_url" not in out


def test_a_url_without_a_doi_is_not_looked_up(monkeypatch):
    calls = []

    def fake_get(url, *args, **kwargs):
        calls.append(str(url))
        return _Resp(403, "")

    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(httpx, "get", fake_get)

    out = web._web(action="read", url="https://example.invalid/blocked")

    assert out["status_code"] == 403
    assert "open_access_url" not in out
    assert not any("openalex" in call for call in calls), (
        f"no DOI means nothing to resolve; the lookup must be skipped: {calls}"
    )


def test_the_open_copy_is_not_offered_when_it_is_the_url_that_refused(monkeypatch):
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(
        httpx,
        "get",
        _routed(
            {"https://api.openalex.org": _Resp(200, payload=_openalex(pdf_url=SILOXANE_DOI))}
        ),
    )

    out = web._web(action="read", url=SILOXANE_DOI)

    assert "open_access_url" not in out, "pointing back at the refusal is not a recovery"
