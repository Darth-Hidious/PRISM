"""AgentConfig and AgentRegistry -- configurable agent profiles."""
from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class AgentConfig:
    """A named agent configuration for the TAOR loop."""

    id: str
    name: str
    description: str = ""
    system_prompt: str = ""
    tools: list[str] | None = None
    skills: list[str] | None = None
    runtime: str = "local"  # local | remote | managed
    remote_endpoint: str | None = None
    max_iterations: int = 20
    enabled: bool = True


class AgentRegistry:
    """Registry of available agent configurations."""

    def __init__(self):
        self._agents: dict[str, AgentConfig] = {}

    def register(self, config: AgentConfig) -> None:
        self._agents[config.id] = config

    def get(self, agent_id: str) -> AgentConfig | None:
        return self._agents.get(agent_id)

    def get_all(self) -> list[AgentConfig]:
        return list(self._agents.values())
