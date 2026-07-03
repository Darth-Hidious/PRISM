"""Tests for the built-in skill registry."""

from app.tools.skills.registry import load_builtin_skills
from app.tools.base import ToolRegistry


class TestSkillRegistry:
    """After Round 6: VALIDATE_SKILL / REVIEW_SKILL / VISUALIZE_SKILL were
    collapsed into the unified `dataset` Tool, registered separately. The
    Skill registry now only holds the materials-workflow skills."""

    def test_load_all_skills(self):
        reg = load_builtin_skills()
        skills = reg.list_skills()
        names = {s.name for s in skills}
        # Materials-workflow skills (preserved as Skills)
        assert "acquire_materials" in names
        assert "predict_properties" in names
        assert "generate_report" in names
        assert "select_materials" in names
        assert "materials_discovery" in names
        assert "plan_simulations" in names
        assert "analyze_phases" in names
        # Dataset-shaped skills GONE — collapsed into the `dataset` Tool
        assert "validate_dataset" not in names
        assert "review_dataset" not in names
        assert "visualize_dataset" not in names
        assert len(skills) == 7  # 10 - 3 (dataset trio collapsed out)

    def test_all_convert_to_tools(self):
        reg = load_builtin_skills()
        tool_reg = ToolRegistry()
        reg.register_all_as_tools(tool_reg)

        tools = tool_reg.list_tools()
        assert len(tools) == 7
        for tool in tools:
            assert tool.name
            assert tool.description
            assert tool.input_schema
            assert callable(tool.func)

    def test_get_individual_skills(self):
        reg = load_builtin_skills()
        acq = reg.get("acquire_materials")
        assert acq.category == "acquisition"

        disc = reg.get("materials_discovery")
        assert disc.category == "discovery"
