"""Tests for TUI card widgets."""

def test_input_card_stores_message():
    from app.tui.widgets.cards import InputCard
    card = InputCard("Find W-Rh alloys")
    assert card.message == "Find W-Rh alloys"

def test_output_card_truncation():
    from app.tui.widgets.cards import OutputCard
    long_text = "\n".join(f"Line {i}" for i in range(20))
    card = OutputCard(long_text, truncation_lines=6)
    assert card.is_truncated is True
    assert card.full_content == long_text

def test_output_card_no_truncation_when_short():
    from app.tui.widgets.cards import OutputCard
    card = OutputCard("Short text", truncation_lines=6)
    assert card.is_truncated is False

def test_tool_card_success():
    from app.tui.widgets.cards import ToolCard
    card = ToolCard(tool_name="search_optimade", elapsed_ms=17000,
                    summary="49 results", result={"count": 49, "results": []})
    assert card.tool_name == "search_optimade"
    assert card.is_error is False

def test_tool_card_error():
    from app.tui.widgets.cards import ToolCard
    card = ToolCard(tool_name="search_optimade", elapsed_ms=5000,
                    summary="", result={"error": "Connection failed"})
    assert card.is_error is True

def test_card_type_detection_metrics():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"metrics": {"mae": 0.04}, "algorithm": "rf"}) == "metrics"

def test_card_type_detection_calphad():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"phases_present": ["BCC"], "gibbs_energy": -1234.5}) == "calphad"

def test_card_type_detection_validation():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"findings": [], "quality_score": 0.9}) == "validation"

def test_card_type_detection_results_table():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"results": [{"a": 1}] * 5, "count": 5}) == "results_table"

def test_card_type_detection_plot():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"filename": "plot.png"}) == "plot"

def test_card_type_detection_error():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"error": "something broke"}) == "error"

def test_card_type_detection_default():
    from app.tui.widgets.cards import detect_card_type
    assert detect_card_type({"success": True}) == "tool"

def test_approval_card():
    from app.tui.widgets.cards import ApprovalCard
    card = ApprovalCard(tool_name="run_calphad", tool_args={"system": "W-Rh"})
    assert card.tool_name == "run_calphad"

def test_plan_card():
    from app.tui.widgets.cards import PlanCard
    card = PlanCard("1. Search\n2. Analyze")
    assert card.plan_text == "1. Search\n2. Analyze"
