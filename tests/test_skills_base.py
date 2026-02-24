"""Tests for Skill base classes."""

import pytest
from app.skills.base import Skill, SkillRegistry, SkillStep
from app.tools.base import ToolRegistry


def _dummy_func(**kwargs):
    return {"result": "ok"}


def _make_skill(name="test_skill"):
    return Skill(
        name=name,
        description="A test skill",
        steps=[
            SkillStep(name="step1", description="First step", tool_name="tool_a"),
            SkillStep(
                name="step2",
                description="Second step",
                tool_name="tool_b",
                optional=True,
            ),
        ],
        input_schema={
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "required": ["x"],
        },
        func=_dummy_func,
        category="test",
    )


class TestSkillStep:
    def test_creation(self):
        step = SkillStep(name="s1", description="desc", tool_name="tool_a")
        assert step.name == "s1"
        assert step.optional is False

    def test_optional_step(self):
        step = SkillStep(
            name="s1", description="desc", tool_name="tool_a", optional=True
        )
        assert step.optional is True


class TestSkill:
    def test_creation(self):
        skill = _make_skill()
        assert skill.name == "test_skill"
        assert len(skill.steps) == 2
        assert skill.category == "test"

    def test_to_tool(self):
        skill = _make_skill()
        tool = skill.to_tool()
        assert tool.name == "test_skill"
        assert tool.description == "A test skill"
        assert tool.input_schema == skill.input_schema
        assert tool.func is skill.func

    def test_to_tool_execute(self):
        skill = _make_skill()
        tool = skill.to_tool()
        result = tool.execute(x="hello")
        assert result == {"result": "ok"}

    def test_default_category(self):
        skill = Skill(
            name="s",
            description="d",
            steps=[],
            input_schema={"type": "object", "properties": {}},
            func=_dummy_func,
        )
        assert skill.category == "skill"


class TestSkillRegistry:
    def test_register_and_get(self):
        reg = SkillRegistry()
        skill = _make_skill()
        reg.register(skill)
        assert reg.get("test_skill") is skill

    def test_get_missing_raises(self):
        reg = SkillRegistry()
        with pytest.raises(KeyError):
            reg.get("nonexistent")

    def test_list_skills(self):
        reg = SkillRegistry()
        reg.register(_make_skill("a"))
        reg.register(_make_skill("b"))
        names = [s.name for s in reg.list_skills()]
        assert "a" in names
        assert "b" in names

    def test_register_all_as_tools(self):
        reg = SkillRegistry()
        reg.register(_make_skill("skill_a"))
        reg.register(_make_skill("skill_b"))

        tool_reg = ToolRegistry()
        reg.register_all_as_tools(tool_reg)

        assert tool_reg.get("skill_a").name == "skill_a"
        assert tool_reg.get("skill_b").name == "skill_b"
        assert len(tool_reg.list_tools()) == 2

    def test_register_overwrites(self):
        reg = SkillRegistry()
        s1 = _make_skill("x")
        s2 = _make_skill("x")
        s2.description = "updated"
        reg.register(s1)
        reg.register(s2)
        assert reg.get("x").description == "updated"
        assert len(reg.list_skills()) == 1
