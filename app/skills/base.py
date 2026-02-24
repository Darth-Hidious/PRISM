"""Skill base classes: SkillStep, Skill, and SkillRegistry."""

from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional

from app.tools.base import Tool, ToolRegistry


@dataclass
class SkillStep:
    """Metadata for a single step in a skill pipeline."""

    name: str
    description: str
    tool_name: str
    optional: bool = False


@dataclass
class Skill:
    """A multi-step orchestration that composes existing tools.

    Skills convert to Tools via to_tool() and register in the same ToolRegistry
    the LLM already uses — no new execution engine needed.
    """

    name: str
    description: str
    steps: List[SkillStep]
    input_schema: dict
    func: Callable
    category: str = "skill"

    def to_tool(self) -> Tool:
        """Convert this Skill to a Tool for ToolRegistry."""
        return Tool(
            name=self.name,
            description=self.description,
            input_schema=self.input_schema,
            func=self.func,
        )


class SkillRegistry:
    """Registry of available skills."""

    def __init__(self):
        self._skills: Dict[str, Skill] = {}

    def register(self, skill: Skill) -> None:
        """Register a skill."""
        self._skills[skill.name] = skill

    def get(self, name: str) -> Skill:
        """Get a skill by name. Raises KeyError if not found."""
        return self._skills[name]

    def list_skills(self) -> List[Skill]:
        """Return all registered skills."""
        return list(self._skills.values())

    def register_all_as_tools(self, tool_registry: ToolRegistry) -> None:
        """Convert all skills to Tools and register them in a ToolRegistry."""
        for skill in self._skills.values():
            tool_registry.register(skill.to_tool())
