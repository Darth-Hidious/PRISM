"""Abstract base class for agent backends."""
from abc import ABC, abstractmethod
from typing import Dict, List, Optional
from app.agent.events import AgentResponse


class Backend(ABC):
    """Provider-agnostic backend interface."""
    @abstractmethod
    def complete(self, messages: List[Dict], tools: List[dict], system_prompt: Optional[str] = None) -> AgentResponse:
        pass
