"""Falsifiable tests for PRISM's provider-neutral credential boundary."""

from app.tools._platform_client import PlatformClient
from app.tools._platform_creds import header_for, resolve_platform_auth


def test_prism_native_values_win_without_deprecation_notice(monkeypatch, capsys):
    monkeypatch.setenv("PRISM_API_KEY", "native-key-shape")
    monkeypatch.setenv("MARC27_API_KEY", "m27_legacy")
    monkeypatch.setenv("PRISM_PLATFORM_URL", "https://native.example")
    monkeypatch.setenv("MARC27_API_URL", "https://legacy.example/api/v1")

    api_url, headers = resolve_platform_auth()

    assert api_url == "https://native.example/api/v1"
    assert headers == {"X-API-Key": "native-key-shape"}
    assert header_for("native-key-shape") == {"X-API-Key": "native-key-shape"}
    assert "deprecated" not in capsys.readouterr().err


def test_legacy_aliases_warn_once_and_name_replacements(monkeypatch, capsys):
    monkeypatch.setenv("MARC27_API_KEY", "m27_legacy")
    monkeypatch.setenv("MARC27_API_URL", "https://legacy.example")

    assert resolve_platform_auth()[0] == "https://legacy.example/api/v1"
    assert resolve_platform_auth()[0] == "https://legacy.example/api/v1"
    stderr = capsys.readouterr().err

    key_notice = (
        "warning: MARC27_API_KEY is deprecated; use PRISM_API_KEY instead."
    )
    url_notice = (
        "warning: MARC27_API_URL is deprecated; use PRISM_API_URL instead."
    )
    assert stderr.count(key_notice) == 1
    assert stderr.count(url_notice) == 1


def test_no_endpoint_is_a_clear_refusal_before_network(monkeypatch, platform_http):
    monkeypatch.setenv("PRISM_API_KEY", "native-key")

    client = PlatformClient()
    result = client.get("/agent/capabilities")

    assert result["error"] == "no platform configured — set PRISM_API_URL"
    assert platform_http.calls == []


def test_legacy_key_only_selects_optional_marc27_provider(monkeypatch, capsys):
    monkeypatch.setenv("MARC27_API_KEY", "m27_legacy")

    api_url, headers = resolve_platform_auth()

    assert api_url == "https://api.marc27.com/api/v1"
    assert headers == {"X-API-Key": "m27_legacy"}
    notice = "warning: MARC27_API_KEY is deprecated; use PRISM_API_KEY instead."
    assert capsys.readouterr().err.count(notice) == 1


def test_native_token_shadows_legacy_key_across_the_credential_family(
    monkeypatch, capsys
):
    monkeypatch.setenv("PRISM_TOKEN", "native-session")
    monkeypatch.setenv("MARC27_API_KEY", "m27_shadowed")

    api_url, headers = resolve_platform_auth()

    assert api_url is None
    assert headers == {"Authorization": "Bearer native-session"}
    assert "deprecated" not in capsys.readouterr().err
