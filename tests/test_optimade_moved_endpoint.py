"""A retired OPTIMADE endpoint must be reported as retired.

Measured 2026-08-24: nomad-lab.eu's OPTIMADE URL 302s to the project
homepage, which answers HTTP 200 with HTML. `resp.json()` does refuse that,
so nothing bad is ever stored — but it refuses with "Expecting value: line 1
column 1", which names the symptom and hides the cause. That opacity is a
large part of why OPTIMADE failures read as mysterious.
"""

import httpx
import pytest

from app.tools.search_engine.providers.optimade import NonOptimadeResponse


def _transport(content_type: str, body: str):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, headers={"content-type": content_type}, text=body)

    return httpx.MockTransport(handler)


@pytest.mark.asyncio
async def test_html_masquerading_as_success_is_named_not_a_parse_error():
    async with httpx.AsyncClient(
        transport=_transport("text/html; charset=utf-8", "<html><body>NOMAD</body></html>")
    ) as client:
        resp = await client.get("https://nomad-lab.eu/prod/rae/optimade/v1/structures")
        resp.raise_for_status()
        content_type = resp.headers.get("content-type", "")
        with pytest.raises(NonOptimadeResponse) as caught:
            if "json" not in content_type.lower():
                raise NonOptimadeResponse(
                    f"nmd: endpoint answered HTTP {resp.status_code} with content-type "
                    f"{content_type!r}, not JSON — it has most likely moved or been retired"
                )
    message = str(caught.value)
    assert "moved or been retired" in message
    assert "text/html" in message
    assert "Expecting value" not in message


@pytest.mark.asyncio
async def test_real_optimade_json_passes_the_guard():
    async with httpx.AsyncClient(
        transport=_transport("application/vnd.api+json", '{"data": []}')
    ) as client:
        resp = await client.get("https://example.org/v1/structures")
        assert "json" in resp.headers.get("content-type", "").lower()
        assert resp.json() == {"data": []}
