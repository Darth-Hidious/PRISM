"""A JavaScript-heavy page that the plain fetch cannot render escalates to
Obscura, and the result says so.

The owner asked for this (2026-09-06): Obscura is part of PRISM, not a
separate tool call — `web action=read` falls through to it automatically
when a page arrives as an unrendered JS shell, names Obscura in the result,
and the model can force it with render='obscura'."""
import app.tools.web as web


SHELL = '<html><head><title>Loading</title></head><body><div id="__next"></div>' \
        '<noscript>You need to enable JavaScript to run this app.</noscript>' \
        '<script src="/_next/static/chunks/main.js"></script></body></html>'
ARTICLE = "<html><head><title>Real Paper</title></head><body><article>" \
          + ("The oxidation of niobium in high pressure oxygen proceeds by pesting. " * 60) \
          + "</article></body></html>"


def test_a_js_shell_is_recognised_and_a_real_article_is_not():
    assert web._looks_under_rendered(SHELL, web._html_to_text(SHELL)[1]) is True
    assert web._looks_under_rendered(ARTICLE, web._html_to_text(ARTICLE)[1]) is False


def _stub_fetch(monkeypatch, html, status=200):
    class R:
        status_code = status
        text = html
    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False, raising=False)
    import httpx
    monkeypatch.setattr(httpx, "get", lambda *a, **k: R())


def test_an_unrendered_page_escalates_to_obscura_and_says_so(monkeypatch):
    _stub_fetch(monkeypatch, SHELL)
    monkeypatch.setenv("PRISM_OBSCURA_CMD", "printf %s <rendered>the real rendered text about niobium oxidation</rendered>")
    out = web._web_read(url="https://spa.example.com/paper")
    assert out.get("source") == "obscura", out
    assert out.get("rendered_with") == "obscura" and out.get("escalated") is True, out
    assert "niobium" in out.get("content", ""), out
    assert "PRISM_OBSCURA_CMD" not in out.get("content", ""), "the command must not leak into content"
    assert out["obscura"]["command"], out["obscura"]


def test_without_obscura_the_page_is_kept_but_the_gap_is_named(monkeypatch):
    _stub_fetch(monkeypatch, SHELL)
    monkeypatch.delenv("PRISM_OBSCURA_CMD", raising=False)
    monkeypatch.setattr(web.shutil, "which", lambda name: None)
    out = web._web_read(url="https://spa.example.com/paper")
    assert out.get("rendered_with") == "plain", out
    assert out["obscura"]["available"] is False, out
    assert "obscura" in out["obscura"]["how"].lower() and "PRISM_OBSCURA" in out["obscura"]["how"], out


def test_render_obscura_forces_it_even_on_a_plain_page(monkeypatch):
    _stub_fetch(monkeypatch, ARTICLE)
    monkeypatch.setenv("PRISM_OBSCURA_CMD", "printf %s forced-render")
    out = web._web_read(url="https://example.com/article", render="obscura")
    assert out.get("source") == "obscura" and out.get("content") == "forced-render", out


def test_render_plain_never_escalates(monkeypatch):
    _stub_fetch(monkeypatch, SHELL)
    monkeypatch.setenv("PRISM_OBSCURA_CMD", "printf %s should-not-run")
    out = web._web_read(url="https://spa.example.com/paper", render="plain")
    assert out.get("rendered_with") == "plain" and out.get("source") == "basic_fetch", out
    assert "should-not-run" not in out.get("content", ""), out
