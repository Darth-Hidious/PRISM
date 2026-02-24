"""Tests for error retry cards."""


def test_error_retry_card_partial():
    from app.tui.widgets.cards import ErrorRetryCard
    card = ErrorRetryCard(
        tool_name="search_optimade",
        elapsed_ms=17000,
        succeeded=["MP", "OQMD", "COD", "JARVIS"],
        failed={"AFLOW": "500 Internal Server Error", "MaterialsCloud": "404"},
        partial_result={"count": 49, "results": [{"id": "mp-1"}]},
    )
    assert card.is_partial is True
    assert len(card.failed) == 2
    assert card.partial_result["count"] == 49


def test_error_retry_card_total_failure():
    from app.tui.widgets.cards import ErrorRetryCard
    card = ErrorRetryCard(
        tool_name="query_materials_project",
        elapsed_ms=2000,
        succeeded=[],
        failed={"MP": "API key not set"},
        partial_result=None,
    )
    assert card.is_partial is False
