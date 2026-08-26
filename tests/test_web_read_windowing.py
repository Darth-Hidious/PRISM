"""Guards for honest truncation reporting in the `web` read tool.

Both read paths used to misreport what they returned. The Firecrawl path
sliced to 15000 characters with no marker; the basic-fetch path marked the cut
but computed `content_length` after slicing, so every long page reported
exactly 10015 chars no matter its true size. Neither offered any way to reach
the rest of the document.
"""

from app.tools.web import _DEFAULT_READ_CHARS, _window


def test_a_short_page_is_returned_whole_and_unmarked():
    out = _window("hello world", 0, _DEFAULT_READ_CHARS)
    assert out["content"] == "hello world"
    assert out["content_length"] == 11
    assert out["truncated"] is False
    assert out["next_offset"] is None


def test_content_length_is_the_true_size_not_the_returned_size():
    # The regression this exists for: content_length must describe the
    # DOCUMENT, never the window. A 50k page reporting "10015" is the bug.
    text = "x" * 50_000
    out = _window(text, 0, 10_000)
    assert out["content_length"] == 50_000
    assert out["returned_chars"] < 50_000
    assert out["truncated"] is True


def test_a_truncated_read_says_where_to_continue():
    text = "y" * 30_000
    out = _window(text, 0, 10_000)
    assert out["next_offset"] == 10_000
    assert "offset=10000" in out["content"], "must tell the caller how to get the rest"
    assert "10000 of 30000" in out["content"]


def test_offset_walks_the_document_to_the_end():
    text = "".join(chr(97 + i % 26) for i in range(25_000))
    seen, offset, guard = "", 0, 0
    while offset is not None and guard < 20:
        out = _window(text, offset, 10_000)
        # Strip the trailing marker before reassembling.
        body = out["content"].split("\n\n... [truncated")[0]
        seen += body
        offset = out["next_offset"]
        guard += 1
    assert seen == text, "paging by next_offset must reconstruct the whole document"


def test_max_chars_zero_returns_everything():
    text = "z" * 40_000
    out = _window(text, 0, 0)
    assert out["truncated"] is False
    assert out["returned_chars"] == 40_000


def test_an_offset_past_the_end_is_clamped_not_an_error():
    out = _window("short", 9_999, 100)
    assert out["content"] == ""
    assert out["truncated"] is False
    assert out["content_length"] == 5


def test_max_chars_zero_reaches_the_window_through_the_tool(monkeypatch):
    """`max_chars=0` is documented as "0 returns the entire document".

    `_window` has always honoured it (test above), but `_web_read` coerced the
    argument with `or _DEFAULT_READ_CHARS` — and 0 is falsy, so a whole-document
    request silently became the 15000-char default AND came back flagged
    truncated. Testing `_window` alone can never catch that: the bug lives in
    the argument coercion, so this drives the tool entry point.
    """
    import httpx

    from app.tools import web

    page = "A" * (_DEFAULT_READ_CHARS * 4)

    class _Resp:
        status_code = 200
        text = f"<html><body>{page}</body></html>"

    monkeypatch.setattr(web, "FIRECRAWL_ACTIVE", False)
    monkeypatch.setattr(httpx, "get", lambda *a, **k: _Resp())

    whole = web._web(action="read", url="https://example.invalid/doc", max_chars=0)
    assert whole["truncated"] is False, "max_chars=0 must not report truncation"
    assert whole["next_offset"] is None
    assert whole["returned_chars"] == whole["content_length"] == len(page)

    # The default is untouched: omitting max_chars still windows the page.
    windowed = web._web(action="read", url="https://example.invalid/doc")
    assert windowed["truncated"] is True
    assert windowed["returned_chars"] < len(page)
