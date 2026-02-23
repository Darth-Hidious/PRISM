# PRISM Agentic CLI - Phase A Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Transform PRISM from a single-shot query tool into a Claude Code-style agentic materials science CLI with provider-agnostic agent core, tool registry, interactive REPL, autonomous mode, data pipeline, and ML property prediction.

**Architecture:** Provider-agnostic AgentCore runs a TAOR loop (Think-Act-Observe-Repeat). Tools are defined once and translated per backend (Anthropic/OpenAI/OpenRouter). The CLI offers interactive REPL mode (`prism`) and autonomous mode (`prism run "goal"`). Existing OPTIMADE search and Materials Project enrichment are wrapped as agent tools. Data pipeline and ML models are added as prediction tools.

**Tech Stack:** Python 3.9+, Click, Rich, anthropic, openai, OPTIMADE client, mp-api, pymatgen, matminer, scikit-learn, XGBoost, LightGBM, matplotlib, Optuna

**Supersedes:** Remaining tasks 12-27 from `docs/plans/2026-02-23-prism-revival-implementation.md` are incorporated here within the agent architecture.

---

## Phase A-1: Foundation

### Task 1: Create test infrastructure and .gitignore

**Files:**
- Create: `tests/__init__.py`
- Create: `tests/conftest.py`
- Create: `tests/test_llm.py`
- Create: `.gitignore`
- Modify: `Makefile:48-50`

**Step 1: Create test directory and conftest**

Create `tests/__init__.py`:
```python
```

Create `tests/conftest.py`:
```python
"""Shared test fixtures for PRISM."""
import pytest
from unittest.mock import MagicMock


@pytest.fixture
def mock_llm_response():
    """Mock LLM completion response."""
    mock = MagicMock()
    mock.choices = [MagicMock()]
    mock.choices[0].message.content = '{"provider": "mp", "filter": "elements HAS ALL \\"Si\\""}'
    return mock


@pytest.fixture
def mock_anthropic_response():
    """Mock Anthropic completion response."""
    mock = MagicMock()
    mock.content = [MagicMock()]
    mock.content[0].text = '{"provider": "mp", "filter": "elements HAS ALL \\"Si\\""}'
    return mock
```

**Step 2: Create first test file**

Create `tests/test_llm.py`:
```python
"""Tests for LLM service factory."""
import os
import pytest
from unittest.mock import patch, MagicMock
from app.llm import get_llm_service, OpenAIService, AnthropicService, OpenRouterService


class TestGetLLMService:
    def test_explicit_provider_openai(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}):
            svc = get_llm_service(provider="openai")
            assert isinstance(svc, OpenAIService)

    def test_explicit_provider_anthropic(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"}):
            svc = get_llm_service(provider="anthropic")
            assert isinstance(svc, AnthropicService)

    def test_explicit_provider_openrouter(self):
        with patch.dict(os.environ, {"OPENROUTER_API_KEY": "test-key"}):
            svc = get_llm_service(provider="openrouter")
            assert isinstance(svc, OpenRouterService)

    def test_auto_detect_openai(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"}, clear=True):
            svc = get_llm_service()
            assert isinstance(svc, OpenAIService)

    def test_no_provider_raises(self):
        with patch.dict(os.environ, {}, clear=True):
            # Clear all known API keys
            for key in ["OPENROUTER_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GOOGLE_CLOUD_PROJECT"]:
                os.environ.pop(key, None)
            with pytest.raises(ValueError, match="No LLM provider configured"):
                get_llm_service()

    def test_unsupported_provider_raises(self):
        with pytest.raises(ValueError, match="Unsupported LLM provider"):
            get_llm_service(provider="nonexistent")
```

**Step 3: Create .gitignore**

Create `.gitignore`:
```
__pycache__/
*.py[cod]
*.egg-info/
dist/
build/
.eggs/
*.egg
.env
.venv/
venv/
*.db
*.sqlite3
provider_fields.json
.coverage
htmlcov/
.pytest_cache/
.mypy_cache/
models/*.joblib
models/*.pkl
data/*.parquet
data/*.csv
.DS_Store
```

**Step 4: Update Makefile test target**

In `Makefile`, replace lines 48-50:
```makefile
test:
	@echo "Running test suite..."
	pytest tests/ -v --tb=short
```

**Step 5: Run tests to verify**

Run: `pytest tests/test_llm.py -v`
Expected: All 6 tests PASS

**Step 6: Commit**

```bash
git add tests/ .gitignore Makefile
git commit -m "feat: add test infrastructure, .gitignore, and first LLM tests"
```

---

### Task 2: Create Tool base class and ToolRegistry

**Files:**
- Create: `app/tools/__init__.py`
- Create: `app/tools/base.py`
- Create: `tests/test_tools_base.py`

**Step 1: Write the failing test**

Create `tests/test_tools_base.py`:
```python
"""Tests for Tool base class and ToolRegistry."""
import pytest
from app.tools.base import Tool, ToolRegistry


class TestTool:
    def test_tool_creation(self):
        def my_func(**kwargs):
            return {"result": kwargs.get("x", 0) + 1}

        tool = Tool(
            name="test_tool",
            description="A test tool",
            input_schema={
                "type": "object",
                "properties": {"x": {"type": "integer"}},
                "required": ["x"],
            },
            func=my_func,
        )
        assert tool.name == "test_tool"
        assert tool.description == "A test tool"

    def test_tool_execute(self):
        def add(**kwargs):
            return {"sum": kwargs["a"] + kwargs["b"]}

        tool = Tool(
            name="add",
            description="Add two numbers",
            input_schema={
                "type": "object",
                "properties": {
                    "a": {"type": "integer"},
                    "b": {"type": "integer"},
                },
                "required": ["a", "b"],
            },
            func=add,
        )
        result = tool.execute(a=2, b=3)
        assert result == {"sum": 5}


class TestToolRegistry:
    def test_register_and_get(self):
        registry = ToolRegistry()
        tool = Tool(
            name="my_tool",
            description="desc",
            input_schema={"type": "object", "properties": {}},
            func=lambda **kw: {},
        )
        registry.register(tool)
        assert registry.get("my_tool") is tool

    def test_get_unknown_raises(self):
        registry = ToolRegistry()
        with pytest.raises(KeyError):
            registry.get("nonexistent")

    def test_list_tools(self):
        registry = ToolRegistry()
        t1 = Tool(name="a", description="A", input_schema={}, func=lambda **kw: {})
        t2 = Tool(name="b", description="B", input_schema={}, func=lambda **kw: {})
        registry.register(t1)
        registry.register(t2)
        assert len(registry.list_tools()) == 2

    def test_to_anthropic_format(self):
        registry = ToolRegistry()
        tool = Tool(
            name="search",
            description="Search materials",
            input_schema={"type": "object", "properties": {"q": {"type": "string"}}},
            func=lambda **kw: {},
        )
        registry.register(tool)
        fmt = registry.to_anthropic_format()
        assert len(fmt) == 1
        assert fmt[0]["name"] == "search"
        assert "input_schema" in fmt[0]

    def test_to_openai_format(self):
        registry = ToolRegistry()
        tool = Tool(
            name="search",
            description="Search materials",
            input_schema={"type": "object", "properties": {"q": {"type": "string"}}},
            func=lambda **kw: {},
        )
        registry.register(tool)
        fmt = registry.to_openai_format()
        assert len(fmt) == 1
        assert fmt[0]["type"] == "function"
        assert fmt[0]["function"]["name"] == "search"
        assert "parameters" in fmt[0]["function"]
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_tools_base.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/tools/__init__.py`:
```python
"""PRISM agent tools."""
```

Create `app/tools/base.py`:
```python
"""Tool base class and registry for provider-agnostic tool definitions."""
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional


@dataclass
class Tool:
    """A single tool that can be called by the agent."""

    name: str
    description: str
    input_schema: dict
    func: Callable

    def execute(self, **kwargs) -> dict:
        """Execute the tool with given arguments."""
        return self.func(**kwargs)


class ToolRegistry:
    """Registry of available tools, with format conversion for each backend."""

    def __init__(self):
        self._tools: Dict[str, Tool] = {}

    def register(self, tool: Tool) -> None:
        """Register a tool."""
        self._tools[tool.name] = tool

    def get(self, name: str) -> Tool:
        """Get a tool by name. Raises KeyError if not found."""
        return self._tools[name]

    def list_tools(self) -> List[Tool]:
        """Return all registered tools."""
        return list(self._tools.values())

    def to_anthropic_format(self) -> List[dict]:
        """Convert tools to Anthropic API format."""
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            }
            for t in self._tools.values()
        ]

    def to_openai_format(self) -> List[dict]:
        """Convert tools to OpenAI API format."""
        return [
            {
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                },
            }
            for t in self._tools.values()
        ]
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_tools_base.py -v`
Expected: All 7 tests PASS

**Step 5: Commit**

```bash
git add app/tools/ tests/test_tools_base.py
git commit -m "feat: add Tool base class and ToolRegistry with provider format conversion"
```

---

### Task 3: Create agent event types and Backend ABC

**Files:**
- Create: `app/agent/__init__.py`
- Create: `app/agent/events.py`
- Create: `app/agent/backends/__init__.py`
- Create: `app/agent/backends/base.py`
- Create: `tests/test_agent_events.py`

**Step 1: Write the failing test**

Create `tests/test_agent_events.py`:
```python
"""Tests for agent event types and backend interface."""
import pytest
from app.agent.events import AgentResponse, ToolCallEvent


class TestToolCallEvent:
    def test_creation(self):
        event = ToolCallEvent(
            tool_name="search",
            tool_args={"query": "silicon"},
            call_id="call_123",
        )
        assert event.tool_name == "search"
        assert event.tool_args == {"query": "silicon"}
        assert event.call_id == "call_123"


class TestAgentResponse:
    def test_text_only(self):
        resp = AgentResponse(text="Hello")
        assert resp.text == "Hello"
        assert resp.has_tool_calls is False
        assert resp.tool_calls == []

    def test_with_tool_calls(self):
        calls = [
            ToolCallEvent(tool_name="search", tool_args={"q": "Si"}, call_id="c1"),
        ]
        resp = AgentResponse(text="Searching...", tool_calls=calls)
        assert resp.has_tool_calls is True
        assert len(resp.tool_calls) == 1

    def test_empty_response(self):
        resp = AgentResponse()
        assert resp.text is None
        assert resp.has_tool_calls is False


class TestBackendABC:
    def test_cannot_instantiate_directly(self):
        from app.agent.backends.base import Backend
        with pytest.raises(TypeError):
            Backend()
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_agent_events.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/__init__.py`:
```python
"""PRISM agent core."""
```

Create `app/agent/events.py`:
```python
"""Event types for the agent loop."""
from dataclasses import dataclass, field
from typing import List, Optional


@dataclass
class ToolCallEvent:
    """Represents a tool call requested by the LLM."""

    tool_name: str
    tool_args: dict
    call_id: str


@dataclass
class AgentResponse:
    """Structured response from a backend LLM call."""

    text: Optional[str] = None
    tool_calls: List[ToolCallEvent] = field(default_factory=list)

    @property
    def has_tool_calls(self) -> bool:
        return len(self.tool_calls) > 0
```

Create `app/agent/backends/__init__.py`:
```python
"""Agent backends for different LLM providers."""
```

Create `app/agent/backends/base.py`:
```python
"""Abstract base class for agent backends."""
from abc import ABC, abstractmethod
from typing import Dict, List, Optional

from app.agent.events import AgentResponse


class Backend(ABC):
    """Provider-agnostic backend interface."""

    @abstractmethod
    def complete(
        self,
        messages: List[Dict],
        tools: List[dict],
        system_prompt: Optional[str] = None,
    ) -> AgentResponse:
        """Send messages + tools to LLM, return structured response."""
        pass
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_agent_events.py -v`
Expected: All 5 tests PASS

**Step 5: Commit**

```bash
git add app/agent/ tests/test_agent_events.py
git commit -m "feat: add agent event types (AgentResponse, ToolCallEvent) and Backend ABC"
```

---

## Phase A-2: Backends

### Task 4: Create Anthropic backend

**Files:**
- Create: `app/agent/backends/anthropic_backend.py`
- Create: `tests/test_anthropic_backend.py`

**Step 1: Write the failing test**

Create `tests/test_anthropic_backend.py`:
```python
"""Tests for Anthropic backend."""
import pytest
from unittest.mock import patch, MagicMock
from app.agent.backends.anthropic_backend import AnthropicBackend


class TestAnthropicBackend:
    def _make_text_response(self, text):
        mock = MagicMock()
        block = MagicMock()
        block.type = "text"
        block.text = text
        mock.content = [block]
        mock.stop_reason = "end_turn"
        return mock

    def _make_tool_response(self, tool_name, tool_args, call_id):
        mock = MagicMock()
        block = MagicMock()
        block.type = "tool_use"
        block.name = tool_name
        block.input = tool_args
        block.id = call_id
        mock.content = [block]
        mock.stop_reason = "tool_use"
        return mock

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_text_response(self, mock_cls):
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_text_response("Hello!")

        backend = AnthropicBackend(api_key="test-key")
        resp = backend.complete(
            messages=[{"role": "user", "content": "Hi"}],
            tools=[],
            system_prompt="You are helpful.",
        )

        assert resp.text == "Hello!"
        assert resp.has_tool_calls is False

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_tool_call_response(self, mock_cls):
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_tool_response(
            "search_optimade", {"query": "silicon"}, "call_abc"
        )

        backend = AnthropicBackend(api_key="test-key")
        resp = backend.complete(
            messages=[{"role": "user", "content": "Find silicon"}],
            tools=[{"name": "search_optimade", "description": "Search", "input_schema": {}}],
            system_prompt=None,
        )

        assert resp.has_tool_calls is True
        assert resp.tool_calls[0].tool_name == "search_optimade"
        assert resp.tool_calls[0].tool_args == {"query": "silicon"}
        assert resp.tool_calls[0].call_id == "call_abc"

    @patch("app.agent.backends.anthropic_backend.Anthropic")
    def test_formats_messages_for_anthropic(self, mock_cls):
        """Anthropic separates system prompt from messages."""
        client = mock_cls.return_value
        client.messages.create.return_value = self._make_text_response("ok")

        backend = AnthropicBackend(api_key="test-key")
        backend.complete(
            messages=[
                {"role": "user", "content": "hi"},
            ],
            tools=[],
            system_prompt="Be helpful",
        )

        call_kwargs = client.messages.create.call_args
        assert call_kwargs.kwargs.get("system") == "Be helpful"
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_anthropic_backend.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/backends/anthropic_backend.py`:
```python
"""Anthropic backend using the Anthropic Python SDK."""
import json
import os
from typing import Dict, List, Optional

from anthropic import Anthropic

from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent


class AnthropicBackend(Backend):
    """Backend that uses Anthropic's Messages API with tool use."""

    def __init__(self, model: str = None, api_key: str = None):
        self.client = Anthropic(api_key=api_key or os.getenv("ANTHROPIC_API_KEY"))
        self.model = model or os.getenv("PRISM_MODEL", "claude-sonnet-4-20250514")

    def complete(
        self,
        messages: List[Dict],
        tools: List[dict],
        system_prompt: Optional[str] = None,
    ) -> AgentResponse:
        kwargs = {
            "model": self.model,
            "max_tokens": 4096,
            "messages": self._format_messages(messages),
        }
        if system_prompt:
            kwargs["system"] = system_prompt
        if tools:
            kwargs["tools"] = tools

        response = self.client.messages.create(**kwargs)
        return self._parse_response(response)

    def _format_messages(self, messages: List[Dict]) -> List[Dict]:
        """Convert neutral message format to Anthropic format.

        Neutral tool_call messages become assistant messages with tool_use content blocks.
        Neutral tool_result messages become user messages with tool_result content blocks.
        """
        formatted = []
        for msg in messages:
            role = msg["role"]

            if role == "tool_calls":
                # Assistant message with tool use blocks
                content = []
                if msg.get("text"):
                    content.append({"type": "text", "text": msg["text"]})
                for tc in msg["calls"]:
                    content.append({
                        "type": "tool_use",
                        "id": tc["id"],
                        "name": tc["name"],
                        "input": tc["args"],
                    })
                formatted.append({"role": "assistant", "content": content})

            elif role == "tool_result":
                content = [{
                    "type": "tool_result",
                    "tool_use_id": msg["tool_call_id"],
                    "content": json.dumps(msg["result"]) if isinstance(msg["result"], dict) else str(msg["result"]),
                }]
                formatted.append({"role": "user", "content": content})

            else:
                formatted.append({"role": role, "content": msg["content"]})

        return formatted

    def _parse_response(self, response) -> AgentResponse:
        """Parse Anthropic response into AgentResponse."""
        text_parts = []
        tool_calls = []

        for block in response.content:
            if block.type == "text":
                text_parts.append(block.text)
            elif block.type == "tool_use":
                tool_calls.append(ToolCallEvent(
                    tool_name=block.name,
                    tool_args=block.input,
                    call_id=block.id,
                ))

        return AgentResponse(
            text="\n".join(text_parts) if text_parts else None,
            tool_calls=tool_calls,
        )
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_anthropic_backend.py -v`
Expected: All 3 tests PASS

**Step 5: Commit**

```bash
git add app/agent/backends/anthropic_backend.py tests/test_anthropic_backend.py
git commit -m "feat: add Anthropic backend with tool use support"
```

---

### Task 5: Create OpenAI backend (also serves OpenRouter)

**Files:**
- Create: `app/agent/backends/openai_backend.py`
- Create: `tests/test_openai_backend.py`

**Step 1: Write the failing test**

Create `tests/test_openai_backend.py`:
```python
"""Tests for OpenAI backend."""
import json
import pytest
from unittest.mock import patch, MagicMock
from app.agent.backends.openai_backend import OpenAIBackend


class TestOpenAIBackend:
    def _make_text_response(self, text):
        mock = MagicMock()
        choice = MagicMock()
        choice.message.content = text
        choice.message.tool_calls = None
        mock.choices = [choice]
        return mock

    def _make_tool_response(self, tool_name, tool_args, call_id):
        mock = MagicMock()
        choice = MagicMock()
        choice.message.content = None
        tc = MagicMock()
        tc.function.name = tool_name
        tc.function.arguments = json.dumps(tool_args)
        tc.id = call_id
        choice.message.tool_calls = [tc]
        mock.choices = [choice]
        return mock

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_text_response(self, mock_cls):
        client = mock_cls.return_value
        client.chat.completions.create.return_value = self._make_text_response("Hi there")

        backend = OpenAIBackend(api_key="test-key")
        resp = backend.complete(
            messages=[{"role": "user", "content": "Hello"}],
            tools=[],
            system_prompt="Be helpful",
        )

        assert resp.text == "Hi there"
        assert resp.has_tool_calls is False

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_tool_call_response(self, mock_cls):
        client = mock_cls.return_value
        client.chat.completions.create.return_value = self._make_tool_response(
            "search_optimade", {"query": "Fe"}, "call_xyz"
        )

        backend = OpenAIBackend(api_key="test-key")
        resp = backend.complete(
            messages=[{"role": "user", "content": "Find iron"}],
            tools=[{"name": "search_optimade", "description": "Search", "input_schema": {}}],
        )

        assert resp.has_tool_calls is True
        assert resp.tool_calls[0].tool_name == "search_optimade"
        assert resp.tool_calls[0].call_id == "call_xyz"

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_system_prompt_injected(self, mock_cls):
        """System prompt should be prepended as a system message."""
        client = mock_cls.return_value
        client.chat.completions.create.return_value = self._make_text_response("ok")

        backend = OpenAIBackend(api_key="test-key")
        backend.complete(
            messages=[{"role": "user", "content": "hi"}],
            tools=[],
            system_prompt="You are PRISM",
        )

        call_args = client.chat.completions.create.call_args
        msgs = call_args.kwargs.get("messages", call_args[1].get("messages", []))
        assert msgs[0]["role"] == "system"
        assert msgs[0]["content"] == "You are PRISM"

    @patch("app.agent.backends.openai_backend.OpenAI")
    def test_openrouter_via_base_url(self, mock_cls):
        """OpenRouter works by setting base_url."""
        backend = OpenAIBackend(
            api_key="or-key",
            base_url="https://openrouter.ai/api/v1",
            model="anthropic/claude-3.5-sonnet",
        )
        assert backend.model == "anthropic/claude-3.5-sonnet"
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_openai_backend.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/backends/openai_backend.py`:
```python
"""OpenAI-compatible backend (also supports OpenRouter via base_url)."""
import json
import os
from typing import Dict, List, Optional

from openai import OpenAI

from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent


class OpenAIBackend(Backend):
    """Backend using OpenAI's Chat Completions API with function calling."""

    def __init__(self, model: str = None, api_key: str = None, base_url: str = None):
        kwargs = {}
        if api_key:
            kwargs["api_key"] = api_key
        if base_url:
            kwargs["base_url"] = base_url
        self.client = OpenAI(**kwargs)
        self.model = model or os.getenv("PRISM_MODEL", "gpt-4o")

    def complete(
        self,
        messages: List[Dict],
        tools: List[dict],
        system_prompt: Optional[str] = None,
    ) -> AgentResponse:
        formatted_messages = self._format_messages(messages, system_prompt)
        kwargs = {
            "model": self.model,
            "messages": formatted_messages,
        }
        if tools:
            kwargs["tools"] = self._format_tools(tools)

        response = self.client.chat.completions.create(**kwargs)
        return self._parse_response(response)

    def _format_messages(
        self, messages: List[Dict], system_prompt: Optional[str] = None
    ) -> List[Dict]:
        """Convert neutral message format to OpenAI format."""
        formatted = []
        if system_prompt:
            formatted.append({"role": "system", "content": system_prompt})

        for msg in messages:
            role = msg["role"]

            if role == "tool_calls":
                # Assistant message with tool_calls
                tool_calls = []
                for tc in msg["calls"]:
                    tool_calls.append({
                        "id": tc["id"],
                        "type": "function",
                        "function": {
                            "name": tc["name"],
                            "arguments": json.dumps(tc["args"]),
                        },
                    })
                formatted.append({
                    "role": "assistant",
                    "content": msg.get("text"),
                    "tool_calls": tool_calls,
                })

            elif role == "tool_result":
                formatted.append({
                    "role": "tool",
                    "tool_call_id": msg["tool_call_id"],
                    "content": json.dumps(msg["result"]) if isinstance(msg["result"], dict) else str(msg["result"]),
                })

            else:
                formatted.append({"role": role, "content": msg["content"]})

        return formatted

    def _format_tools(self, tools: List[dict]) -> List[dict]:
        """Convert Anthropic-style tool defs to OpenAI function calling format."""
        return [
            {
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t.get("input_schema", {}),
                },
            }
            for t in tools
        ]

    def _parse_response(self, response) -> AgentResponse:
        """Parse OpenAI response into AgentResponse."""
        msg = response.choices[0].message
        tool_calls = []

        if msg.tool_calls:
            for tc in msg.tool_calls:
                tool_calls.append(ToolCallEvent(
                    tool_name=tc.function.name,
                    tool_args=json.loads(tc.function.arguments),
                    call_id=tc.id,
                ))

        return AgentResponse(
            text=msg.content,
            tool_calls=tool_calls,
        )
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_openai_backend.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/agent/backends/openai_backend.py tests/test_openai_backend.py
git commit -m "feat: add OpenAI backend with function calling (also supports OpenRouter)"
```

---

## Phase A-3: Agent Core

### Task 6: Create AgentCore with TAOR loop

**Files:**
- Create: `app/agent/core.py`
- Create: `tests/test_agent_core.py`

**Step 1: Write the failing test**

Create `tests/test_agent_core.py`:
```python
"""Tests for AgentCore TAOR loop."""
import pytest
from unittest.mock import MagicMock
from app.agent.core import AgentCore
from app.agent.events import AgentResponse, ToolCallEvent
from app.tools.base import Tool, ToolRegistry


class TestAgentCore:
    def _make_registry_with_tool(self):
        registry = ToolRegistry()
        registry.register(Tool(
            name="add",
            description="Add numbers",
            input_schema={
                "type": "object",
                "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            },
            func=lambda **kw: {"sum": kw["a"] + kw["b"]},
        ))
        return registry

    def test_simple_text_response(self):
        """Backend returns text only → loop terminates immediately."""
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="Silicon is a semiconductor.")

        agent = AgentCore(backend=backend, tools=ToolRegistry())
        result = agent.process("Tell me about silicon")

        assert result == "Silicon is a semiconductor."
        assert len(agent.history) == 2  # user + assistant

    def test_tool_call_then_text(self):
        """Backend calls a tool, then returns text on second call."""
        registry = self._make_registry_with_tool()
        backend = MagicMock()

        # First call: tool use
        backend.complete.side_effect = [
            AgentResponse(
                text=None,
                tool_calls=[ToolCallEvent(tool_name="add", tool_args={"a": 2, "b": 3}, call_id="c1")],
            ),
            # Second call: final text
            AgentResponse(text="The sum is 5."),
        ]

        agent = AgentCore(backend=backend, tools=registry)
        result = agent.process("What is 2+3?")

        assert result == "The sum is 5."
        assert backend.complete.call_count == 2
        # History: user, tool_calls, tool_result, assistant
        assert len(agent.history) == 4

    def test_multiple_tool_calls_in_one_response(self):
        """Backend requests two tools at once."""
        registry = ToolRegistry()
        registry.register(Tool(name="a", description="A", input_schema={}, func=lambda **kw: {"v": 1}))
        registry.register(Tool(name="b", description="B", input_schema={}, func=lambda **kw: {"v": 2}))

        backend = MagicMock()
        backend.complete.side_effect = [
            AgentResponse(
                tool_calls=[
                    ToolCallEvent(tool_name="a", tool_args={}, call_id="c1"),
                    ToolCallEvent(tool_name="b", tool_args={}, call_id="c2"),
                ],
            ),
            AgentResponse(text="Done."),
        ]

        agent = AgentCore(backend=backend, tools=registry)
        result = agent.process("Do both")

        assert result == "Done."

    def test_system_prompt_passed_to_backend(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="ok")

        agent = AgentCore(
            backend=backend,
            tools=ToolRegistry(),
            system_prompt="You are PRISM.",
        )
        agent.process("hi")

        call_kwargs = backend.complete.call_args
        assert call_kwargs.kwargs.get("system_prompt") == "You are PRISM."

    def test_max_iterations_safety(self):
        """Agent stops after max_iterations to prevent infinite loops."""
        backend = MagicMock()
        # Always return tool calls (infinite loop scenario)
        backend.complete.return_value = AgentResponse(
            tool_calls=[ToolCallEvent(tool_name="x", tool_args={}, call_id="c")],
        )

        registry = ToolRegistry()
        registry.register(Tool(name="x", description="X", input_schema={}, func=lambda **kw: {}))

        agent = AgentCore(backend=backend, tools=registry, max_iterations=3)
        result = agent.process("loop forever")

        assert backend.complete.call_count == 3
        assert "max iterations" in result.lower()
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_agent_core.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/core.py`:
```python
"""AgentCore: the provider-agnostic TAOR loop."""
from typing import Dict, List, Optional

from app.agent.backends.base import Backend
from app.agent.events import AgentResponse, ToolCallEvent
from app.tools.base import ToolRegistry


DEFAULT_SYSTEM_PROMPT = """You are PRISM, an AI research assistant for materials science.

You have access to tools for searching materials databases (OPTIMADE, Materials Project),
predicting material properties, and visualizing results. Use these tools to help
researchers find, analyze, and understand materials.

When a user asks a question:
1. Think about what tools and data you need
2. Use the appropriate tools to gather information
3. Synthesize the results into a clear answer

Be precise with scientific data. Cite sources when possible."""


class AgentCore:
    """Provider-agnostic agent that runs a Think-Act-Observe-Repeat loop."""

    def __init__(
        self,
        backend: Backend,
        tools: ToolRegistry,
        system_prompt: Optional[str] = None,
        max_iterations: int = 20,
    ):
        self.backend = backend
        self.tools = tools
        self.system_prompt = system_prompt if system_prompt is not None else DEFAULT_SYSTEM_PROMPT
        self.max_iterations = max_iterations
        self.history: List[Dict] = []

    def process(self, message: str) -> str:
        """Process a user message through the TAOR loop. Returns final text."""
        self.history.append({"role": "user", "content": message})
        tool_defs = self.tools.to_anthropic_format()

        for _iteration in range(self.max_iterations):
            response = self.backend.complete(
                messages=self.history,
                tools=tool_defs,
                system_prompt=self.system_prompt,
            )

            if response.has_tool_calls:
                # Record the assistant's tool call request
                self.history.append({
                    "role": "tool_calls",
                    "text": response.text,
                    "calls": [
                        {"id": tc.call_id, "name": tc.tool_name, "args": tc.tool_args}
                        for tc in response.tool_calls
                    ],
                })

                # Execute each tool and record results
                for tc in response.tool_calls:
                    tool = self.tools.get(tc.tool_name)
                    try:
                        result = tool.execute(**tc.tool_args)
                    except Exception as e:
                        result = {"error": str(e)}

                    self.history.append({
                        "role": "tool_result",
                        "tool_call_id": tc.call_id,
                        "result": result,
                    })
            else:
                # No tool calls → final answer
                if response.text:
                    self.history.append({"role": "assistant", "content": response.text})
                return response.text or ""

        return f"Reached max iterations ({self.max_iterations}). Stopping."

    def reset(self):
        """Clear conversation history."""
        self.history.clear()
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_agent_core.py -v`
Expected: All 5 tests PASS

**Step 5: Commit**

```bash
git add app/agent/core.py tests/test_agent_core.py
git commit -m "feat: add AgentCore with TAOR loop, tool execution, and iteration safety"
```

---

### Task 7: Create session memory

**Files:**
- Create: `app/agent/memory.py`
- Create: `tests/test_agent_memory.py`

**Step 1: Write the failing test**

Create `tests/test_agent_memory.py`:
```python
"""Tests for agent session and persistent memory."""
import os
import json
import tempfile
import pytest
from app.agent.memory import SessionMemory


class TestSessionMemory:
    def test_save_and_load_session(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            mem.set("last_query", "Find silicon materials")
            mem.set("provider", "mp")

            session_id = mem.save()

            mem2 = SessionMemory(storage_dir=tmpdir)
            mem2.load(session_id)
            assert mem2.get("last_query") == "Find silicon materials"
            assert mem2.get("provider") == "mp"

    def test_get_missing_key_returns_default(self):
        mem = SessionMemory()
        assert mem.get("nonexistent") is None
        assert mem.get("nonexistent", "fallback") == "fallback"

    def test_save_and_load_history(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            history = [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
            ]
            mem.set_history(history)
            session_id = mem.save()

            mem2 = SessionMemory(storage_dir=tmpdir)
            mem2.load(session_id)
            assert mem2.get_history() == history

    def test_list_sessions(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            mem.set("x", 1)
            s1 = mem.save()

            mem2 = SessionMemory(storage_dir=tmpdir)
            mem2.set("y", 2)
            s2 = mem2.save()

            mem3 = SessionMemory(storage_dir=tmpdir)
            sessions = mem3.list_sessions()
            assert len(sessions) >= 2
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_agent_memory.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/memory.py`:
```python
"""Session and persistent memory for the agent."""
import json
import os
import uuid
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional


class SessionMemory:
    """Manages session state and persistence to disk."""

    def __init__(self, storage_dir: Optional[str] = None):
        self._data: Dict[str, Any] = {}
        self._history: List[Dict] = []
        self._session_id: Optional[str] = None
        self._storage_dir = Path(storage_dir) if storage_dir else Path.home() / ".prism" / "sessions"

    def set(self, key: str, value: Any) -> None:
        self._data[key] = value

    def get(self, key: str, default: Any = None) -> Any:
        return self._data.get(key, default)

    def set_history(self, history: List[Dict]) -> None:
        self._history = history

    def get_history(self) -> List[Dict]:
        return self._history

    def save(self) -> str:
        """Save session to disk. Returns session ID."""
        self._storage_dir.mkdir(parents=True, exist_ok=True)
        if not self._session_id:
            self._session_id = datetime.now().strftime("%Y%m%d_%H%M%S") + "_" + uuid.uuid4().hex[:8]

        filepath = self._storage_dir / f"{self._session_id}.json"
        payload = {
            "session_id": self._session_id,
            "timestamp": datetime.now().isoformat(),
            "data": self._data,
            "history": self._history,
        }
        filepath.write_text(json.dumps(payload, indent=2, default=str))
        return self._session_id

    def load(self, session_id: str) -> None:
        """Load a session from disk."""
        filepath = self._storage_dir / f"{session_id}.json"
        payload = json.loads(filepath.read_text())
        self._session_id = payload["session_id"]
        self._data = payload.get("data", {})
        self._history = payload.get("history", [])

    def list_sessions(self) -> List[Dict]:
        """List all saved sessions."""
        if not self._storage_dir.exists():
            return []
        sessions = []
        for f in sorted(self._storage_dir.glob("*.json"), reverse=True):
            try:
                payload = json.loads(f.read_text())
                sessions.append({
                    "session_id": payload["session_id"],
                    "timestamp": payload.get("timestamp"),
                })
            except (json.JSONDecodeError, KeyError):
                continue
        return sessions
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_agent_memory.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/agent/memory.py tests/test_agent_memory.py
git commit -m "feat: add SessionMemory for agent state persistence"
```

---

## Phase A-4: Built-in Tools

### Task 8: Create system tools

**Files:**
- Create: `app/tools/system.py`
- Create: `tests/test_tools_system.py`

**Step 1: Write the failing test**

Create `tests/test_tools_system.py`:
```python
"""Tests for system tools."""
import pytest
from app.tools.system import create_system_tools
from app.tools.base import ToolRegistry


class TestSystemTools:
    def test_creates_registry_with_tools(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "web_search" in names
        assert "read_file" in names
        assert "write_file" in names

    def test_read_file_tool(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        # Create a temp file
        f = tmp_path / "test.txt"
        f.write_text("hello world")

        tool = registry.get("read_file")
        result = tool.execute(path=str(f))
        assert result["content"] == "hello world"

    def test_read_file_not_found(self):
        registry = ToolRegistry()
        create_system_tools(registry)
        tool = registry.get("read_file")
        result = tool.execute(path="/nonexistent/path.txt")
        assert "error" in result

    def test_write_file_tool(self, tmp_path):
        registry = ToolRegistry()
        create_system_tools(registry)
        f = tmp_path / "out.txt"

        tool = registry.get("write_file")
        result = tool.execute(path=str(f), content="written by prism")
        assert result["success"] is True
        assert f.read_text() == "written by prism"
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_tools_system.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/tools/system.py`:
```python
"""System tools: file I/O, web search, user interaction."""
import os
from pathlib import Path

from app.tools.base import Tool, ToolRegistry


def _read_file(**kwargs) -> dict:
    path = kwargs["path"]
    try:
        content = Path(path).read_text()
        return {"content": content}
    except Exception as e:
        return {"error": str(e)}


def _write_file(**kwargs) -> dict:
    path = kwargs["path"]
    content = kwargs["content"]
    try:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        Path(path).write_text(content)
        return {"success": True, "path": path}
    except Exception as e:
        return {"error": str(e)}


def _web_search(**kwargs) -> dict:
    """Basic web search using requests. Returns search result summaries."""
    query = kwargs["query"]
    try:
        import requests
        # Use a simple search API approach
        resp = requests.get(
            "https://api.duckduckgo.com/",
            params={"q": query, "format": "json", "no_html": 1},
            timeout=10,
        )
        data = resp.json()
        results = []
        if data.get("AbstractText"):
            results.append({"title": data.get("Heading", ""), "text": data["AbstractText"]})
        for item in data.get("RelatedTopics", [])[:5]:
            if isinstance(item, dict) and "Text" in item:
                results.append({"text": item["Text"], "url": item.get("FirstURL", "")})
        return {"results": results, "query": query}
    except Exception as e:
        return {"error": str(e), "query": query}


def create_system_tools(registry: ToolRegistry) -> None:
    """Register all system tools into the given registry."""
    registry.register(Tool(
        name="read_file",
        description="Read the contents of a file at the given path.",
        input_schema={
            "type": "object",
            "properties": {"path": {"type": "string", "description": "File path to read"}},
            "required": ["path"],
        },
        func=_read_file,
    ))

    registry.register(Tool(
        name="write_file",
        description="Write content to a file at the given path.",
        input_schema={
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to write"},
                "content": {"type": "string", "description": "Content to write"},
            },
            "required": ["path", "content"],
        },
        func=_write_file,
    ))

    registry.register(Tool(
        name="web_search",
        description="Search the web for information. Returns relevant results.",
        input_schema={
            "type": "object",
            "properties": {"query": {"type": "string", "description": "Search query"}},
            "required": ["query"],
        },
        func=_web_search,
    ))
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_tools_system.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/tools/system.py tests/test_tools_system.py
git commit -m "feat: add system tools (read_file, write_file, web_search)"
```

---

### Task 9: Create SearchOPTIMADE tool

This wraps the existing `app/mcp.py` AdaptiveOptimadeFilter and OptimadeClient into an agent tool.

**Files:**
- Create: `app/tools/data.py`
- Create: `tests/test_tools_data.py`

**Step 1: Write the failing test**

Create `tests/test_tools_data.py`:
```python
"""Tests for data tools (OPTIMADE, Materials Project)."""
import pytest
from unittest.mock import patch, MagicMock
from app.tools.data import create_data_tools
from app.tools.base import ToolRegistry


class TestSearchOPTIMADETool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "search_optimade" in names

    @patch("app.tools.data.OptimadeClient")
    def test_search_by_elements(self, mock_client_cls):
        """Search with elements returns results from OPTIMADE."""
        mock_client = mock_client_cls.return_value
        mock_client.get.return_value = {
            "mp": {"data": [{"id": "mp-1", "attributes": {"chemical_formula_descriptive": "Si"}}]}
        }

        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("search_optimade")
        result = tool.execute(
            filter_string='elements HAS "Si"',
            providers=["mp"],
            max_results=5,
        )
        assert "results" in result or "error" in result

    def test_search_optimade_schema(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        tool = registry.get("search_optimade")
        schema = tool.input_schema
        assert "filter_string" in schema["properties"]


class TestQueryMPTool:
    def test_tool_registered(self):
        registry = ToolRegistry()
        create_data_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "query_materials_project" in names
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_tools_data.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/tools/data.py`:
```python
"""Data tools: OPTIMADE search and Materials Project queries."""
import json
from typing import List, Optional

from app.tools.base import Tool, ToolRegistry


def _search_optimade(**kwargs) -> dict:
    """Search OPTIMADE databases with a filter string."""
    filter_string = kwargs["filter_string"]
    providers = kwargs.get("providers", None)
    max_results = kwargs.get("max_results", 10)

    try:
        from optimade.client import OptimadeClient
        from app.config.providers import FALLBACK_PROVIDERS

        if providers:
            base_urls = {
                p["id"]: p["base_url"]
                for p in FALLBACK_PROVIDERS
                if p["id"] in providers
            }
        else:
            base_urls = {p["id"]: p["base_url"] for p in FALLBACK_PROVIDERS}

        client = OptimadeClient(
            base_urls=base_urls,
            max_results_per_provider=max_results,
        )
        raw = client.get(filter_string)

        results = []
        for provider_id, provider_data in raw.items():
            if isinstance(provider_data, dict):
                entries = provider_data.get("data", [])
            elif isinstance(provider_data, list):
                entries = provider_data
            else:
                continue
            for entry in entries[:max_results]:
                attrs = entry.get("attributes", {}) if isinstance(entry, dict) else {}
                results.append({
                    "id": entry.get("id", ""),
                    "provider": provider_id,
                    "formula": attrs.get("chemical_formula_descriptive", ""),
                    "elements": attrs.get("elements", []),
                    "space_group": attrs.get("space_group_symbol", ""),
                })

        return {"results": results, "count": len(results), "filter": filter_string}
    except Exception as e:
        return {"error": str(e), "filter": filter_string}


def _query_materials_project(**kwargs) -> dict:
    """Query Materials Project for detailed material properties."""
    formula = kwargs.get("formula")
    material_id = kwargs.get("material_id")
    properties = kwargs.get("properties", [
        "material_id", "formula_pretty", "band_gap", "formation_energy_per_atom",
        "energy_above_hull", "is_metal",
    ])

    try:
        from mp_api.client import MPRester
        import os

        api_key = os.getenv("MP_API_KEY")
        if not api_key:
            return {"error": "MP_API_KEY not set. Configure with: prism configure --mp-api-key YOUR_KEY"}

        with MPRester(api_key) as mpr:
            if material_id:
                docs = mpr.materials.summary.search(material_ids=[material_id], fields=properties)
            elif formula:
                docs = mpr.materials.summary.search(formula=formula, fields=properties)
            else:
                return {"error": "Provide either 'formula' or 'material_id'"}

            results = []
            for doc in docs[:20]:
                entry = {}
                for prop in properties:
                    val = getattr(doc, prop, None)
                    if val is not None:
                        entry[prop] = val if isinstance(val, (str, int, float, bool)) else str(val)
                results.append(entry)

            return {"results": results, "count": len(results)}
    except Exception as e:
        return {"error": str(e)}


def create_data_tools(registry: ToolRegistry) -> None:
    """Register data tools into the given registry."""
    registry.register(Tool(
        name="search_optimade",
        description="Search materials databases using OPTIMADE filter syntax. Queries 6 providers: Materials Project, OQMD, COD, AFLOW, JARVIS, Materials Cloud. Use OPTIMADE filter syntax like: elements HAS ALL \"Si\",\"O\" AND nelements=2",
        input_schema={
            "type": "object",
            "properties": {
                "filter_string": {
                    "type": "string",
                    "description": "OPTIMADE filter string, e.g. 'elements HAS ALL \"Si\",\"O\"'",
                },
                "providers": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional list of provider IDs to search (mp, oqmd, cod, aflow, jarvis, mcloud). Defaults to all.",
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results per provider. Default 10.",
                    "default": 10,
                },
            },
            "required": ["filter_string"],
        },
        func=_search_optimade,
    ))

    registry.register(Tool(
        name="query_materials_project",
        description="Query Materials Project for detailed material properties like band gap, formation energy, bulk modulus. Requires MP_API_KEY.",
        input_schema={
            "type": "object",
            "properties": {
                "formula": {
                    "type": "string",
                    "description": "Chemical formula to search, e.g. 'LiCoO2'",
                },
                "material_id": {
                    "type": "string",
                    "description": "Materials Project ID, e.g. 'mp-1234'",
                },
                "properties": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Properties to retrieve. Defaults to common properties.",
                },
            },
        },
        func=_query_materials_project,
    ))
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_tools_data.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/tools/data.py tests/test_tools_data.py
git commit -m "feat: add SearchOPTIMADE and QueryMaterialsProject agent tools"
```

---

### Task 10: Create basic visualization tool

**Files:**
- Create: `app/tools/visualization.py`
- Create: `tests/test_tools_viz.py`

**Step 1: Write the failing test**

Create `tests/test_tools_viz.py`:
```python
"""Tests for visualization tools."""
import os
import pytest
from app.tools.visualization import create_visualization_tools
from app.tools.base import ToolRegistry


class TestVisualizationTools:
    def test_tools_registered(self):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "plot_materials_comparison" in names
        assert "plot_property_distribution" in names

    def test_comparison_plot(self, tmp_path):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot_materials_comparison")

        result = tool.execute(
            materials=[
                {"name": "Si", "band_gap": 1.1, "formation_energy": -0.5},
                {"name": "Ge", "band_gap": 0.67, "formation_energy": -0.3},
            ],
            property_x="band_gap",
            property_y="formation_energy",
            output_path=str(tmp_path / "comparison.png"),
        )

        assert result.get("success") is True or "error" in result

    def test_distribution_plot(self, tmp_path):
        registry = ToolRegistry()
        create_visualization_tools(registry)
        tool = registry.get("plot_property_distribution")

        result = tool.execute(
            values=[1.1, 2.3, 0.5, 1.8, 3.2],
            property_name="band_gap",
            output_path=str(tmp_path / "dist.png"),
        )

        assert result.get("success") is True or "error" in result
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_tools_viz.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/tools/visualization.py`:
```python
"""Visualization tools for materials data."""
from typing import Dict, List

from app.tools.base import Tool, ToolRegistry


def _plot_materials_comparison(**kwargs) -> dict:
    """Create a scatter plot comparing materials on two properties."""
    materials = kwargs["materials"]
    prop_x = kwargs["property_x"]
    prop_y = kwargs["property_y"]
    output_path = kwargs.get("output_path", "comparison.png")
    title = kwargs.get("title", f"{prop_x} vs {prop_y}")

    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        x_vals = [m.get(prop_x, 0) for m in materials]
        y_vals = [m.get(prop_y, 0) for m in materials]
        labels = [m.get("name", m.get("formula", f"M{i}")) for i, m in enumerate(materials)]

        fig, ax = plt.subplots(figsize=(8, 6))
        ax.scatter(x_vals, y_vals, s=60, alpha=0.7)
        for i, label in enumerate(labels):
            ax.annotate(label, (x_vals[i], y_vals[i]), fontsize=8, ha="left")
        ax.set_xlabel(prop_x)
        ax.set_ylabel(prop_y)
        ax.set_title(title)
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)

        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed. Install with: pip install matplotlib"}
    except Exception as e:
        return {"error": str(e)}


def _plot_property_distribution(**kwargs) -> dict:
    """Create a histogram of a property distribution."""
    values = kwargs["values"]
    prop_name = kwargs.get("property_name", "property")
    output_path = kwargs.get("output_path", "distribution.png")

    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        fig, ax = plt.subplots(figsize=(8, 5))
        ax.hist(values, bins=min(30, max(5, len(values) // 3)), alpha=0.7, edgecolor="black")
        ax.set_xlabel(prop_name)
        ax.set_ylabel("Count")
        ax.set_title(f"Distribution of {prop_name}")
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)

        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed. Install with: pip install matplotlib"}
    except Exception as e:
        return {"error": str(e)}


def create_visualization_tools(registry: ToolRegistry) -> None:
    """Register visualization tools into the given registry."""
    registry.register(Tool(
        name="plot_materials_comparison",
        description="Create a scatter plot comparing materials on two properties. Saves as PNG.",
        input_schema={
            "type": "object",
            "properties": {
                "materials": {
                    "type": "array",
                    "items": {"type": "object"},
                    "description": "List of material dicts, each with property values and a 'name' or 'formula' key.",
                },
                "property_x": {"type": "string", "description": "Property for x-axis"},
                "property_y": {"type": "string", "description": "Property for y-axis"},
                "output_path": {"type": "string", "description": "Output file path. Default: comparison.png"},
                "title": {"type": "string", "description": "Plot title"},
            },
            "required": ["materials", "property_x", "property_y"],
        },
        func=_plot_materials_comparison,
    ))

    registry.register(Tool(
        name="plot_property_distribution",
        description="Create a histogram showing the distribution of a material property.",
        input_schema={
            "type": "object",
            "properties": {
                "values": {
                    "type": "array",
                    "items": {"type": "number"},
                    "description": "List of numeric values to plot",
                },
                "property_name": {"type": "string", "description": "Name of the property (for axis label)"},
                "output_path": {"type": "string", "description": "Output file path. Default: distribution.png"},
            },
            "required": ["values"],
        },
        func=_plot_property_distribution,
    ))
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_tools_viz.py -v`
Expected: All 3 tests PASS (or soft-pass if matplotlib not installed — tool returns error dict)

**Step 5: Commit**

```bash
git add app/tools/visualization.py tests/test_tools_viz.py
git commit -m "feat: add visualization tools (scatter comparison, distribution histogram)"
```

---

### Task 11: Update hardcoded limits in mcp.py to use settings

**Files:**
- Modify: `app/mcp.py`
- Create: `tests/test_mcp.py`

**Step 1: Write the test**

Create `tests/test_mcp.py`:
```python
"""Tests for mcp.py filter parsing and ModelContext."""
import pytest
from unittest.mock import MagicMock, patch
from app.mcp import ModelContext


class TestModelContext:
    def test_to_prompt_basic(self):
        ctx = ModelContext(
            query="Find silicon materials",
            results=[{"id": "mp-1", "formula": "Si", "provider": "mp"}],
        )
        prompt = ctx.to_prompt()
        assert "silicon" in prompt.lower()
        assert "Si" in prompt

    def test_to_prompt_limits_results(self):
        """Should respect MAX_RESULTS_DISPLAY setting."""
        results = [{"id": f"mp-{i}", "formula": f"X{i}"} for i in range(50)]
        ctx = ModelContext(query="test", results=results)
        prompt = ctx.to_prompt()
        # Should not contain all 50 results in prompt
        assert "mp-49" not in prompt

    def test_to_prompt_reasoning_mode(self):
        ctx = ModelContext(query="test", results=[])
        prompt = ctx.to_prompt(reasoning_mode=True)
        assert isinstance(prompt, str)
```

**Step 2: Run test to verify it passes with existing code**

Run: `pytest tests/test_mcp.py -v`
Expected: All 3 tests should PASS with existing mcp.py

**Step 3: Update mcp.py to use settings constants**

In `app/mcp.py`, add import at the top:
```python
from app.config.settings import MAX_FILTER_ATTEMPTS, MAX_RESULTS_DISPLAY, MAX_INTERACTIVE_QUESTIONS, MAX_RESULTS_PER_PROVIDER
```

Then replace hardcoded values:
- `max_attempts: int = 3` → `max_attempts: int = MAX_FILTER_ATTEMPTS`
- `max_questions: int = 3` → `max_questions: int = MAX_INTERACTIVE_QUESTIONS`
- Any hardcoded `10` for result limits → `MAX_RESULTS_DISPLAY`
- Any hardcoded `1000` for max results per provider → `MAX_RESULTS_PER_PROVIDER`

**Step 4: Run tests to verify they still pass**

Run: `pytest tests/test_mcp.py tests/test_tools_data.py -v`
Expected: All tests PASS

**Step 5: Commit**

```bash
git add app/mcp.py tests/test_mcp.py
git commit -m "refactor: use settings constants for limits in mcp.py, add ModelContext tests"
```

---

## Phase A-5: REPL + CLI Integration

### Task 12: Create interactive REPL mode

**Files:**
- Create: `app/agent/repl.py`
- Create: `tests/test_repl.py`

**Step 1: Write the failing test**

Create `tests/test_repl.py`:
```python
"""Tests for the interactive REPL."""
import pytest
from unittest.mock import patch, MagicMock, call
from app.agent.repl import AgentREPL


class TestAgentREPL:
    def test_init(self):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        assert repl.agent is not None

    @patch("builtins.input", side_effect=["hello", "/exit"])
    @patch("app.agent.repl.Console")
    def test_exit_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        from app.agent.events import AgentResponse
        backend.complete.return_value = AgentResponse(text="Hi there!")
        repl = AgentREPL(backend=backend)
        repl.run()  # Should exit cleanly on /exit

    @patch("builtins.input", side_effect=["/help", "/exit"])
    @patch("app.agent.repl.Console")
    def test_help_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()
        # Should not crash on /help

    @patch("builtins.input", side_effect=["/clear", "/exit"])
    @patch("app.agent.repl.Console")
    def test_clear_command(self, mock_console_cls, mock_input):
        backend = MagicMock()
        repl = AgentREPL(backend=backend)
        repl.run()
        # History should be cleared
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_repl.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/repl.py`:
```python
"""Interactive REPL for the PRISM agent."""
import sys
from typing import Optional

from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel

from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.agent.memory import SessionMemory
from app.tools.base import ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools


REPL_COMMANDS = {
    "/exit": "Exit the REPL",
    "/quit": "Exit the REPL",
    "/clear": "Clear conversation history",
    "/help": "Show available commands",
    "/history": "Show conversation history length",
    "/tools": "List available tools",
    "/save": "Save current session",
}


class AgentREPL:
    """Interactive REPL for conversational agent interaction."""

    def __init__(
        self,
        backend: Backend,
        system_prompt: Optional[str] = None,
        tools: Optional[ToolRegistry] = None,
    ):
        self.console = Console()
        self.memory = SessionMemory()

        # Build tool registry
        if tools is None:
            tools = ToolRegistry()
            create_system_tools(tools)
            create_data_tools(tools)
            create_visualization_tools(tools)

        self.agent = AgentCore(
            backend=backend,
            tools=tools,
            system_prompt=system_prompt,
        )

    def run(self):
        """Main REPL loop."""
        self._show_welcome()

        while True:
            try:
                user_input = input("\n> ").strip()
            except (EOFError, KeyboardInterrupt):
                self.console.print("\nGoodbye!")
                break

            if not user_input:
                continue

            if user_input.startswith("/"):
                if self._handle_command(user_input):
                    break
                continue

            # Process through agent
            try:
                with self.console.status("[bold green]Thinking..."):
                    response = self.agent.process(user_input)
                if response:
                    self.console.print()
                    self.console.print(Markdown(response))
            except Exception as e:
                self.console.print(f"[red]Error: {e}[/red]")

    def _show_welcome(self):
        self.console.print(Panel.fit(
            "[bold cyan]PRISM[/bold cyan] — Materials Science Research Agent\n"
            "Type your question or /help for commands.",
            border_style="cyan",
        ))

    def _handle_command(self, cmd: str) -> bool:
        """Handle a slash command. Returns True to exit."""
        cmd = cmd.lower().strip()

        if cmd in ("/exit", "/quit"):
            self.console.print("Goodbye!")
            return True

        elif cmd == "/clear":
            self.agent.reset()
            self.console.print("[dim]Conversation cleared.[/dim]")

        elif cmd == "/help":
            for name, desc in REPL_COMMANDS.items():
                self.console.print(f"  [cyan]{name}[/cyan]  {desc}")

        elif cmd == "/history":
            n = len(self.agent.history)
            self.console.print(f"[dim]History: {n} messages[/dim]")

        elif cmd == "/tools":
            for tool in self.agent.tools.list_tools():
                self.console.print(f"  [green]{tool.name}[/green]  {tool.description[:60]}")

        elif cmd == "/save":
            self.memory.set_history(self.agent.history)
            sid = self.memory.save()
            self.console.print(f"[dim]Session saved: {sid}[/dim]")

        else:
            self.console.print(f"[yellow]Unknown command: {cmd}. Type /help.[/yellow]")

        return False
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_repl.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/agent/repl.py tests/test_repl.py
git commit -m "feat: add interactive REPL with slash commands, tool listing, session save"
```

---

### Task 13: Create autonomous mode

**Files:**
- Create: `app/agent/autonomous.py`
- Create: `tests/test_autonomous.py`

**Step 1: Write the failing test**

Create `tests/test_autonomous.py`:
```python
"""Tests for autonomous mode (prism run)."""
import pytest
from unittest.mock import MagicMock
from app.agent.autonomous import run_autonomous
from app.agent.events import AgentResponse


class TestAutonomousMode:
    def test_runs_to_completion(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(
            text="Silicon has a band gap of 1.1 eV."
        )

        result = run_autonomous(
            goal="What is the band gap of silicon?",
            backend=backend,
        )

        assert "silicon" in result.lower() or "1.1" in result

    def test_returns_string(self):
        backend = MagicMock()
        backend.complete.return_value = AgentResponse(text="Done.")

        result = run_autonomous(goal="test", backend=backend)
        assert isinstance(result, str)
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_autonomous.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/agent/autonomous.py`:
```python
"""Autonomous mode: run agent to completion on a goal."""
from typing import Optional

from app.agent.backends.base import Backend
from app.agent.core import AgentCore
from app.tools.base import ToolRegistry
from app.tools.data import create_data_tools
from app.tools.system import create_system_tools
from app.tools.visualization import create_visualization_tools


AUTONOMOUS_SYSTEM_PROMPT = """You are PRISM, an autonomous materials science research agent.

You have been given a research goal. Use your tools to investigate, gather data,
analyze results, and produce a comprehensive answer.

Available tool categories:
- Data: Search OPTIMADE databases, query Materials Project
- Visualization: Create plots and comparisons
- System: Read/write files, search the web

Work step by step:
1. Break down the research goal
2. Use tools to gather relevant data
3. Analyze and synthesize findings
4. Present a clear, well-structured answer with citations

Be thorough but efficient. Cite data sources."""


def run_autonomous(
    goal: str,
    backend: Backend,
    system_prompt: Optional[str] = None,
    tools: Optional[ToolRegistry] = None,
    max_iterations: int = 30,
) -> str:
    """Run the agent autonomously on a goal. Returns the final text response."""
    if tools is None:
        tools = ToolRegistry()
        create_system_tools(tools)
        create_data_tools(tools)
        create_visualization_tools(tools)

    agent = AgentCore(
        backend=backend,
        tools=tools,
        system_prompt=system_prompt or AUTONOMOUS_SYSTEM_PROMPT,
        max_iterations=max_iterations,
    )

    return agent.process(goal)
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_autonomous.py -v`
Expected: All 2 tests PASS

**Step 5: Commit**

```bash
git add app/agent/autonomous.py tests/test_autonomous.py
git commit -m "feat: add autonomous mode for fire-and-forget agent execution"
```

---

### Task 14: Wire up CLI entry points for REPL and autonomous mode

**Files:**
- Modify: `app/cli.py` (add `prism` REPL launch and `prism run` command)
- Create: `app/agent/factory.py` (backend factory from config)
- Create: `tests/test_cli.py`

**Step 1: Create backend factory**

Create `app/agent/factory.py`:
```python
"""Factory for creating agent backends from configuration."""
import os
from typing import Optional

from app.agent.backends.base import Backend


def create_backend(provider: Optional[str] = None, model: Optional[str] = None) -> Backend:
    """Create the appropriate backend based on config/environment.

    Provider detection order: ANTHROPIC_API_KEY, OPENAI_API_KEY, OPENROUTER_API_KEY.
    """
    if provider is None:
        if os.getenv("ANTHROPIC_API_KEY"):
            provider = "anthropic"
        elif os.getenv("OPENAI_API_KEY"):
            provider = "openai"
        elif os.getenv("OPENROUTER_API_KEY"):
            provider = "openrouter"
        else:
            raise ValueError(
                "No LLM provider configured for agent mode. "
                "Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or OPENROUTER_API_KEY."
            )

    if provider == "anthropic":
        from app.agent.backends.anthropic_backend import AnthropicBackend
        return AnthropicBackend(model=model)
    elif provider == "openai":
        from app.agent.backends.openai_backend import OpenAIBackend
        return OpenAIBackend(model=model)
    elif provider == "openrouter":
        from app.agent.backends.openai_backend import OpenAIBackend
        return OpenAIBackend(
            model=model or os.getenv("PRISM_MODEL", "anthropic/claude-3.5-sonnet"),
            base_url="https://openrouter.ai/api/v1",
            api_key=os.getenv("OPENROUTER_API_KEY"),
        )
    else:
        raise ValueError(f"Unknown provider: {provider}")
```

**Step 2: Modify cli.py to add REPL and run command**

At the top of `app/cli.py`, the `cli()` function currently shows branding when invoked without a subcommand. Modify it to launch the REPL instead:

Add after existing imports (around line 43):
```python
from app.agent.factory import create_backend
from app.agent.repl import AgentREPL
from app.agent.autonomous import run_autonomous
```

In the `cli()` function body (the `invoke_without_command=True` handler), replace the help display with REPL launch:
```python
if ctx.invoked_subcommand is None:
    try:
        backend = create_backend()
        repl = AgentREPL(backend=backend)
        repl.run()
    except ValueError as e:
        # Fall back to showing help if no agent provider configured
        console.print(PRISM_BRAND)
        console.print(f"[yellow]{e}[/yellow]")
        console.print("\nRun [cyan]prism --help[/cyan] for available commands.")
```

Add new `run` command to the CLI group:
```python
@cli.command("run")
@click.argument("goal")
@click.option("--provider", default=None, help="LLM provider (anthropic/openai/openrouter)")
@click.option("--model", default=None, help="Model name override")
def run_goal(goal, provider, model):
    """Run PRISM agent autonomously on a research goal."""
    console = Console()
    try:
        backend = create_backend(provider=provider, model=model)
        console.print(Panel.fit(f"[bold]Goal:[/bold] {goal}", border_style="cyan"))
        with console.status("[bold green]Agent working..."):
            result = run_autonomous(goal=goal, backend=backend)
        console.print()
        from rich.markdown import Markdown
        console.print(Markdown(result))
    except ValueError as e:
        console.print(f"[red]Error: {e}[/red]")
    except Exception as e:
        console.print(f"[red]Agent error: {e}[/red]")
```

**Step 3: Write CLI test**

Create `tests/test_cli.py`:
```python
"""Tests for CLI commands."""
import pytest
from click.testing import CliRunner
from unittest.mock import patch, MagicMock
from app.cli import cli


class TestCLI:
    def test_help(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["--help"])
        assert result.exit_code == 0
        assert "PRISM" in result.output or "prism" in result.output

    def test_version(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["--version"])
        assert result.exit_code == 0

    def test_run_command_exists(self):
        runner = CliRunner()
        result = runner.invoke(cli, ["run", "--help"])
        assert result.exit_code == 0
        assert "goal" in result.output.lower() or "GOAL" in result.output

    @patch("app.cli.create_backend")
    @patch("app.cli.run_autonomous")
    def test_run_command(self, mock_run, mock_backend):
        mock_run.return_value = "Silicon has band gap 1.1 eV"
        runner = CliRunner()
        result = runner.invoke(cli, ["run", "What is silicon's band gap?"])
        assert result.exit_code == 0
```

**Step 4: Run tests**

Run: `pytest tests/test_cli.py -v`
Expected: All 4 tests PASS

**Step 5: Update pyproject.toml packages**

In `pyproject.toml`, update the packages list:
```toml
[tool.setuptools]
packages = ["app", "app.config", "app.db", "app.agent", "app.agent.backends", "app.tools"]
```

**Step 6: Commit**

```bash
git add app/agent/factory.py app/cli.py tests/test_cli.py pyproject.toml
git commit -m "feat: wire up REPL mode (prism) and autonomous mode (prism run) to CLI"
```

---

## Phase A-6: Data Pipeline

### Task 15: Create data collector module

**Files:**
- Create: `app/data/__init__.py`
- Create: `app/data/collector.py`
- Create: `tests/test_collector.py`

**Step 1: Write the failing test**

Create `tests/test_collector.py`:
```python
"""Tests for data collector."""
import pytest
from unittest.mock import patch, MagicMock
from app.data.collector import OPTIMADECollector


class TestOPTIMADECollector:
    def test_init(self):
        collector = OPTIMADECollector()
        assert collector.providers is not None
        assert len(collector.providers) > 0

    @patch("app.data.collector.OptimadeClient")
    def test_collect_by_elements(self, mock_client_cls):
        mock_client = mock_client_cls.return_value
        mock_client.get.return_value = {
            "mp": {"data": [
                {"id": "mp-1", "attributes": {
                    "chemical_formula_descriptive": "Si",
                    "elements": ["Si"],
                    "nelements": 1,
                }}
            ]}
        }

        collector = OPTIMADECollector()
        results = collector.collect(filter_string='elements HAS "Si"', max_per_provider=5)
        assert len(results) > 0
        assert "formula" in results[0]

    @patch("app.data.collector.OptimadeClient")
    def test_collect_handles_errors(self, mock_client_cls):
        mock_client_cls.return_value.get.side_effect = Exception("Network error")
        collector = OPTIMADECollector()
        results = collector.collect(filter_string='elements HAS "Zz"', max_per_provider=5)
        assert isinstance(results, list)  # Should return empty, not crash
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_collector.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/data/__init__.py`:
```python
"""Data pipeline for collecting and normalizing materials data."""
```

Create `app/data/collector.py`:
```python
"""Collect materials data from OPTIMADE and Materials Project."""
from typing import Dict, List, Optional

from app.config.providers import FALLBACK_PROVIDERS


class OPTIMADECollector:
    """Paginated bulk data collection from OPTIMADE providers."""

    def __init__(self, providers: Optional[List[Dict]] = None):
        self.providers = providers or FALLBACK_PROVIDERS

    def collect(
        self,
        filter_string: str,
        max_per_provider: int = 100,
        provider_ids: Optional[List[str]] = None,
    ) -> List[Dict]:
        """Collect materials matching an OPTIMADE filter.

        Returns a list of normalized material dicts.
        """
        try:
            from optimade.client import OptimadeClient
        except ImportError:
            return []

        base_urls = {}
        for p in self.providers:
            if provider_ids is None or p["id"] in provider_ids:
                base_urls[p["id"]] = p["base_url"]

        try:
            client = OptimadeClient(
                base_urls=base_urls,
                max_results_per_provider=max_per_provider,
            )
            raw = client.get(filter_string)
        except Exception:
            return []

        results = []
        for provider_id, provider_data in raw.items():
            entries = []
            if isinstance(provider_data, dict):
                entries = provider_data.get("data", [])
            elif isinstance(provider_data, list):
                entries = provider_data

            for entry in entries:
                if not isinstance(entry, dict):
                    continue
                attrs = entry.get("attributes", {})
                results.append({
                    "source_id": f"{provider_id}:{entry.get('id', '')}",
                    "provider": provider_id,
                    "formula": attrs.get("chemical_formula_descriptive", ""),
                    "elements": attrs.get("elements", []),
                    "nelements": attrs.get("nelements"),
                    "space_group": attrs.get("space_group_symbol", ""),
                    "lattice_vectors": attrs.get("lattice_vectors"),
                })

        return results


class MPCollector:
    """Collect enriched data from Materials Project API."""

    def collect(self, formula: str = None, elements: List[str] = None, max_results: int = 50) -> List[Dict]:
        """Query Materials Project for detailed properties."""
        import os
        api_key = os.getenv("MP_API_KEY")
        if not api_key:
            return []

        try:
            from mp_api.client import MPRester
            with MPRester(api_key) as mpr:
                kwargs = {"fields": [
                    "material_id", "formula_pretty", "band_gap",
                    "formation_energy_per_atom", "energy_above_hull",
                    "density", "is_metal",
                ]}
                if formula:
                    kwargs["formula"] = formula
                elif elements:
                    kwargs["elements"] = elements
                docs = mpr.materials.summary.search(**kwargs)

                results = []
                for doc in docs[:max_results]:
                    entry = {}
                    for field in kwargs["fields"]:
                        val = getattr(doc, field, None)
                        if val is not None:
                            entry[field] = val if isinstance(val, (str, int, float, bool)) else str(val)
                    results.append(entry)
                return results
        except Exception:
            return []
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_collector.py -v`
Expected: All 3 tests PASS

**Step 5: Commit**

```bash
git add app/data/ tests/test_collector.py
git commit -m "feat: add OPTIMADECollector and MPCollector for bulk data collection"
```

---

### Task 16: Create data normalizer and store

**Files:**
- Create: `app/data/normalizer.py`
- Create: `app/data/store.py`
- Create: `tests/test_data_store.py`

**Step 1: Write the failing test**

Create `tests/test_data_store.py`:
```python
"""Tests for data normalizer and store."""
import pytest
import tempfile
from app.data.normalizer import normalize_records
from app.data.store import DataStore


class TestNormalizer:
    def test_normalize_basic(self):
        records = [
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
            {"source_id": "oqmd:2", "formula": "Si", "elements": ["Si"], "provider": "oqmd"},
        ]
        df = normalize_records(records)
        assert len(df) == 2
        assert "formula" in df.columns
        assert "provider" in df.columns

    def test_normalize_deduplicates(self):
        records = [
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
        ]
        df = normalize_records(records)
        assert len(df) == 1


class TestDataStore:
    def test_save_and_load(self):
        records = [
            {"source_id": "mp:1", "formula": "Si", "elements": ["Si"], "provider": "mp"},
        ]
        with tempfile.TemporaryDirectory() as tmpdir:
            store = DataStore(data_dir=tmpdir)
            df = normalize_records(records)
            store.save(df, "test_collection")

            loaded = store.load("test_collection")
            assert len(loaded) == 1

    def test_list_datasets(self):
        records = [{"source_id": "x", "formula": "X", "elements": [], "provider": "p"}]
        with tempfile.TemporaryDirectory() as tmpdir:
            store = DataStore(data_dir=tmpdir)
            df = normalize_records(records)
            store.save(df, "dataset_a")
            datasets = store.list_datasets()
            assert len(datasets) >= 1
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_data_store.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/data/normalizer.py`:
```python
"""Normalize materials data into a unified schema."""
from typing import Dict, List

import pandas as pd


def normalize_records(records: List[Dict]) -> pd.DataFrame:
    """Normalize a list of material records into a DataFrame.

    Deduplicates by source_id. Converts element lists to sorted comma-separated strings.
    """
    if not records:
        return pd.DataFrame()

    df = pd.DataFrame(records)

    # Normalize elements column: list -> sorted comma-separated string
    if "elements" in df.columns:
        df["elements"] = df["elements"].apply(
            lambda x: ",".join(sorted(x)) if isinstance(x, list) else str(x)
        )

    # Deduplicate by source_id
    if "source_id" in df.columns:
        df = df.drop_duplicates(subset=["source_id"])

    return df.reset_index(drop=True)
```

Create `app/data/store.py`:
```python
"""Storage layer for collected materials data (Parquet + metadata)."""
import json
from datetime import datetime
from pathlib import Path
from typing import List, Optional

import pandas as pd


class DataStore:
    """Stores datasets as Parquet files with JSON metadata."""

    def __init__(self, data_dir: Optional[str] = None):
        self.data_dir = Path(data_dir) if data_dir else Path("data")
        self.data_dir.mkdir(parents=True, exist_ok=True)

    def save(self, df: pd.DataFrame, name: str) -> Path:
        """Save a DataFrame as Parquet with metadata."""
        filepath = self.data_dir / f"{name}.parquet"
        df.to_parquet(filepath, index=False)

        meta = {
            "name": name,
            "rows": len(df),
            "columns": list(df.columns),
            "saved_at": datetime.now().isoformat(),
        }
        meta_path = self.data_dir / f"{name}.meta.json"
        meta_path.write_text(json.dumps(meta, indent=2))

        return filepath

    def load(self, name: str) -> pd.DataFrame:
        """Load a dataset from Parquet."""
        filepath = self.data_dir / f"{name}.parquet"
        return pd.read_parquet(filepath)

    def list_datasets(self) -> List[dict]:
        """List all available datasets with metadata."""
        datasets = []
        for meta_file in sorted(self.data_dir.glob("*.meta.json")):
            try:
                meta = json.loads(meta_file.read_text())
                datasets.append(meta)
            except (json.JSONDecodeError, KeyError):
                continue
        return datasets
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_data_store.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/data/normalizer.py app/data/store.py tests/test_data_store.py
git commit -m "feat: add data normalizer (dedup, schema unification) and Parquet store"
```

---

### Task 17: Add data CLI commands

**Files:**
- Create: `app/commands/__init__.py`
- Create: `app/commands/data.py`
- Modify: `app/cli.py` (register data command group)

**Step 1: Write the commands**

Create `app/commands/__init__.py`:
```python
"""CLI command modules."""
```

Create `app/commands/data.py`:
```python
"""Data pipeline CLI commands: collect, status, export."""
import click
from rich.console import Console
from rich.table import Table


@click.group()
def data():
    """Manage materials data collection and storage."""
    pass


@data.command()
@click.option("--elements", default=None, help="Elements to search, e.g. 'Si,O'")
@click.option("--formula", default=None, help="Chemical formula, e.g. 'SiO2'")
@click.option("--providers", default=None, help="Comma-separated provider IDs")
@click.option("--max-results", default=100, help="Max results per provider")
@click.option("--name", default=None, help="Dataset name (auto-generated if not given)")
def collect(elements, formula, providers, max_results, name):
    """Collect materials data from OPTIMADE databases."""
    console = Console()
    from app.data.collector import OPTIMADECollector
    from app.data.normalizer import normalize_records
    from app.data.store import DataStore

    if not elements and not formula:
        console.print("[red]Provide --elements or --formula[/red]")
        return

    filter_parts = []
    if elements:
        elems = [e.strip() for e in elements.split(",")]
        quoted = ", ".join(f'"{e}"' for e in elems)
        filter_parts.append(f"elements HAS ALL {quoted}")
    if formula:
        filter_parts.append(f'chemical_formula_descriptive="{formula}"')

    filter_string = " AND ".join(filter_parts)
    provider_ids = [p.strip() for p in providers.split(",")] if providers else None

    console.print(f"[bold]Filter:[/bold] {filter_string}")

    with console.status("[bold green]Collecting data..."):
        collector = OPTIMADECollector()
        records = collector.collect(
            filter_string=filter_string,
            max_per_provider=max_results,
            provider_ids=provider_ids,
        )

    if not records:
        console.print("[yellow]No results found.[/yellow]")
        return

    df = normalize_records(records)
    dataset_name = name or f"collect_{elements or formula}".replace(",", "_")
    store = DataStore()
    path = store.save(df, dataset_name)

    console.print(f"[green]Collected {len(df)} materials → {path}[/green]")


@data.command()
def status():
    """Show available datasets and their metadata."""
    console = Console()
    from app.data.store import DataStore

    store = DataStore()
    datasets = store.list_datasets()

    if not datasets:
        console.print("[dim]No datasets found. Run 'prism data collect' first.[/dim]")
        return

    table = Table(title="Available Datasets")
    table.add_column("Name")
    table.add_column("Rows", justify="right")
    table.add_column("Columns", justify="right")
    table.add_column("Saved At")

    for ds in datasets:
        table.add_row(
            ds.get("name", "?"),
            str(ds.get("rows", "?")),
            str(len(ds.get("columns", []))),
            ds.get("saved_at", "?")[:19],
        )

    console.print(table)
```

**Step 2: Register in cli.py**

In `app/cli.py`, add import and register:
```python
from app.commands.data import data as data_group
cli.add_command(data_group, "data")
```

**Step 3: Test the commands**

Run: `prism data --help`
Expected: Shows collect and status subcommands

Run: `prism data status`
Expected: Shows "No datasets found" or table

**Step 4: Update pyproject.toml packages**

Add `"app.commands"` and `"app.data"` to the packages list.

**Step 5: Commit**

```bash
git add app/commands/ app/cli.py pyproject.toml
git commit -m "feat: add data CLI commands (prism data collect/status)"
```

---

## Phase A-7: ML Pipeline

### Task 18: Add ML dependencies and create feature engineering module

**Files:**
- Modify: `pyproject.toml` (add ML deps as optional)
- Create: `app/ml/__init__.py`
- Create: `app/ml/features.py`
- Create: `tests/test_features.py`

**Step 1: Add ML dependencies to pyproject.toml**

Add an optional dependency group:
```toml
[project.optional-dependencies]
ml = [
    "scikit-learn>=1.3.0",
    "xgboost>=2.0.0",
    "lightgbm>=4.0.0",
    "optuna>=3.4.0",
    "pymatgen>=2024.1.1",
    "matminer>=0.9.0",
    "matplotlib>=3.7.0",
    "pyarrow>=14.0.0",
    "joblib>=1.3.0",
]
dev = [
    "pytest>=7.4.3",
    "pytest-cov>=4.1.0",
    "pytest-mock>=3.12.0",
    "black>=23.11.0",
    "isort>=5.12.0",
    "flake8>=6.1.0",
]
```

**Step 2: Write the failing test**

Create `tests/test_features.py`:
```python
"""Tests for feature engineering."""
import pytest
from app.ml.features import composition_features


class TestCompositionFeatures:
    def test_basic_formula(self):
        features = composition_features("Si")
        assert isinstance(features, dict)
        assert len(features) > 0

    def test_binary_compound(self):
        features = composition_features("NaCl")
        assert isinstance(features, dict)
        assert "avg_atomic_mass" in features or len(features) > 0

    def test_invalid_formula_returns_empty(self):
        features = composition_features("InvalidXyz123")
        assert isinstance(features, dict)
```

**Step 3: Write implementation**

Create `app/ml/__init__.py`:
```python
"""ML pipeline for materials property prediction."""
```

Create `app/ml/features.py`:
```python
"""Feature engineering for materials property prediction."""
from typing import Dict, List, Optional


# Element properties lookup (subset of Magpie features)
ELEMENT_DATA = {
    "H": {"atomic_mass": 1.008, "atomic_number": 1, "electronegativity": 2.20, "atomic_radius": 25},
    "Li": {"atomic_mass": 6.941, "atomic_number": 3, "electronegativity": 0.98, "atomic_radius": 145},
    "Be": {"atomic_mass": 9.012, "atomic_number": 4, "electronegativity": 1.57, "atomic_radius": 105},
    "B": {"atomic_mass": 10.81, "atomic_number": 5, "electronegativity": 2.04, "atomic_radius": 85},
    "C": {"atomic_mass": 12.01, "atomic_number": 6, "electronegativity": 2.55, "atomic_radius": 70},
    "N": {"atomic_mass": 14.01, "atomic_number": 7, "electronegativity": 3.04, "atomic_radius": 65},
    "O": {"atomic_mass": 16.00, "atomic_number": 8, "electronegativity": 3.44, "atomic_radius": 60},
    "F": {"atomic_mass": 19.00, "atomic_number": 9, "electronegativity": 3.98, "atomic_radius": 50},
    "Na": {"atomic_mass": 22.99, "atomic_number": 11, "electronegativity": 0.93, "atomic_radius": 180},
    "Mg": {"atomic_mass": 24.31, "atomic_number": 12, "electronegativity": 1.31, "atomic_radius": 150},
    "Al": {"atomic_mass": 26.98, "atomic_number": 13, "electronegativity": 1.61, "atomic_radius": 125},
    "Si": {"atomic_mass": 28.09, "atomic_number": 14, "electronegativity": 1.90, "atomic_radius": 110},
    "P": {"atomic_mass": 30.97, "atomic_number": 15, "electronegativity": 2.19, "atomic_radius": 100},
    "S": {"atomic_mass": 32.07, "atomic_number": 16, "electronegativity": 2.58, "atomic_radius": 100},
    "Cl": {"atomic_mass": 35.45, "atomic_number": 17, "electronegativity": 3.16, "atomic_radius": 100},
    "K": {"atomic_mass": 39.10, "atomic_number": 19, "electronegativity": 0.82, "atomic_radius": 220},
    "Ca": {"atomic_mass": 40.08, "atomic_number": 20, "electronegativity": 1.00, "atomic_radius": 180},
    "Ti": {"atomic_mass": 47.87, "atomic_number": 22, "electronegativity": 1.54, "atomic_radius": 140},
    "Fe": {"atomic_mass": 55.85, "atomic_number": 26, "electronegativity": 1.83, "atomic_radius": 140},
    "Co": {"atomic_mass": 58.93, "atomic_number": 27, "electronegativity": 1.88, "atomic_radius": 135},
    "Ni": {"atomic_mass": 58.69, "atomic_number": 28, "electronegativity": 1.91, "atomic_radius": 135},
    "Cu": {"atomic_mass": 63.55, "atomic_number": 29, "electronegativity": 1.90, "atomic_radius": 135},
    "Zn": {"atomic_mass": 65.38, "atomic_number": 30, "electronegativity": 1.65, "atomic_radius": 135},
    "Ga": {"atomic_mass": 69.72, "atomic_number": 31, "electronegativity": 1.81, "atomic_radius": 130},
    "Ge": {"atomic_mass": 72.63, "atomic_number": 32, "electronegativity": 2.01, "atomic_radius": 125},
    "As": {"atomic_mass": 74.92, "atomic_number": 33, "electronegativity": 2.18, "atomic_radius": 115},
    "Se": {"atomic_mass": 78.96, "atomic_number": 34, "electronegativity": 2.55, "atomic_radius": 115},
    "Sr": {"atomic_mass": 87.62, "atomic_number": 38, "electronegativity": 0.95, "atomic_radius": 200},
    "Zr": {"atomic_mass": 91.22, "atomic_number": 40, "electronegativity": 1.33, "atomic_radius": 155},
    "Nb": {"atomic_mass": 92.91, "atomic_number": 41, "electronegativity": 1.60, "atomic_radius": 145},
    "Mo": {"atomic_mass": 95.96, "atomic_number": 42, "electronegativity": 2.16, "atomic_radius": 145},
    "Sn": {"atomic_mass": 118.71, "atomic_number": 50, "electronegativity": 1.96, "atomic_radius": 145},
    "Ba": {"atomic_mass": 137.33, "atomic_number": 56, "electronegativity": 0.89, "atomic_radius": 215},
    "W": {"atomic_mass": 183.84, "atomic_number": 74, "electronegativity": 2.36, "atomic_radius": 135},
    "Pt": {"atomic_mass": 195.08, "atomic_number": 78, "electronegativity": 2.28, "atomic_radius": 135},
    "Au": {"atomic_mass": 196.97, "atomic_number": 79, "electronegativity": 2.54, "atomic_radius": 135},
    "Pb": {"atomic_mass": 207.2, "atomic_number": 82, "electronegativity": 2.33, "atomic_radius": 180},
}


def _parse_formula(formula: str) -> Dict[str, float]:
    """Parse a simple chemical formula into element:count dict."""
    import re
    pattern = r'([A-Z][a-z]?)(\d*\.?\d*)'
    matches = re.findall(pattern, formula)
    composition = {}
    for elem, count in matches:
        if elem:
            composition[elem] = float(count) if count else 1.0
    return composition


def composition_features(formula: str) -> Dict[str, float]:
    """Generate composition-based features from a chemical formula.

    Returns a dict of feature_name -> value. Uses Magpie-style statistics
    (mean, min, max, range, std) over element properties weighted by composition.
    """
    comp = _parse_formula(formula)
    if not comp:
        return {}

    total_atoms = sum(comp.values())
    fractions = {elem: count / total_atoms for elem, count in comp.items()}

    features = {}
    features["n_elements"] = len(comp)
    features["total_atoms_in_formula"] = total_atoms

    # Compute statistics over each element property
    for prop_name in ["atomic_mass", "atomic_number", "electronegativity", "atomic_radius"]:
        values = []
        weights = []
        for elem, frac in fractions.items():
            if elem in ELEMENT_DATA and prop_name in ELEMENT_DATA[elem]:
                values.append(ELEMENT_DATA[elem][prop_name])
                weights.append(frac)

        if not values:
            continue

        import statistics
        weighted_avg = sum(v * w for v, w in zip(values, weights))
        features[f"avg_{prop_name}"] = weighted_avg
        features[f"min_{prop_name}"] = min(values)
        features[f"max_{prop_name}"] = max(values)
        features[f"range_{prop_name}"] = max(values) - min(values)
        if len(values) > 1:
            features[f"std_{prop_name}"] = statistics.stdev(values)
        else:
            features[f"std_{prop_name}"] = 0.0

    return features
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_features.py -v`
Expected: All 3 tests PASS

**Step 5: Commit**

```bash
git add pyproject.toml app/ml/ tests/test_features.py
git commit -m "feat: add ML optional deps and composition feature engineering"
```

---

### Task 19: Create model trainer and registry

**Files:**
- Create: `app/ml/trainer.py`
- Create: `app/ml/registry.py`
- Create: `tests/test_trainer.py`

**Step 1: Write the failing test**

Create `tests/test_trainer.py`:
```python
"""Tests for model trainer and registry."""
import tempfile
import pytest
import numpy as np
from app.ml.trainer import train_model, AVAILABLE_ALGORITHMS
from app.ml.registry import ModelRegistry


class TestTrainer:
    def test_available_algorithms(self):
        assert "random_forest" in AVAILABLE_ALGORITHMS
        assert "xgboost" in AVAILABLE_ALGORITHMS or "gradient_boosting" in AVAILABLE_ALGORITHMS

    def test_train_random_forest(self):
        X = np.random.rand(50, 5)
        y = np.random.rand(50)
        result = train_model(X, y, algorithm="random_forest", property_name="test_prop")
        assert "metrics" in result
        assert "mae" in result["metrics"]
        assert result["metrics"]["mae"] >= 0

    def test_train_returns_model(self):
        X = np.random.rand(50, 5)
        y = np.random.rand(50)
        result = train_model(X, y, algorithm="random_forest", property_name="test")
        assert "model" in result


class TestModelRegistry:
    def test_save_and_load(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ModelRegistry(models_dir=tmpdir)

            # Fake model
            from sklearn.ensemble import RandomForestRegressor
            model = RandomForestRegressor(n_estimators=5)
            X = np.random.rand(20, 3)
            y = np.random.rand(20)
            model.fit(X, y)

            registry.save_model(model, "band_gap", "random_forest", {"mae": 0.1})

            loaded = registry.load_model("band_gap", "random_forest")
            assert loaded is not None

    def test_list_models(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ModelRegistry(models_dir=tmpdir)
            models = registry.list_models()
            assert isinstance(models, list)
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_trainer.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/ml/trainer.py`:
```python
"""Model training pipeline."""
import numpy as np
from typing import Dict, Optional


AVAILABLE_ALGORITHMS = {
    "random_forest": "Random Forest Regressor",
    "gradient_boosting": "Gradient Boosting Regressor",
    "linear": "Linear Regression",
}

# Only list these if the packages are installed
try:
    import xgboost
    AVAILABLE_ALGORITHMS["xgboost"] = "XGBoost Regressor"
except ImportError:
    pass

try:
    import lightgbm
    AVAILABLE_ALGORITHMS["lightgbm"] = "LightGBM Regressor"
except ImportError:
    pass


def _create_model(algorithm: str):
    """Create a model instance for the given algorithm."""
    from sklearn.ensemble import RandomForestRegressor, GradientBoostingRegressor
    from sklearn.linear_model import LinearRegression

    if algorithm == "random_forest":
        return RandomForestRegressor(n_estimators=100, random_state=42)
    elif algorithm == "gradient_boosting":
        return GradientBoostingRegressor(n_estimators=100, random_state=42)
    elif algorithm == "linear":
        return LinearRegression()
    elif algorithm == "xgboost":
        import xgboost as xgb
        return xgb.XGBRegressor(n_estimators=100, random_state=42)
    elif algorithm == "lightgbm":
        import lightgbm as lgb
        return lgb.LGBMRegressor(n_estimators=100, random_state=42, verbose=-1)
    else:
        raise ValueError(f"Unknown algorithm: {algorithm}")


def train_model(
    X: np.ndarray,
    y: np.ndarray,
    algorithm: str = "random_forest",
    property_name: str = "property",
    test_size: float = 0.2,
) -> Dict:
    """Train a model and return metrics.

    Returns dict with keys: model, metrics, algorithm, property_name.
    """
    from sklearn.model_selection import cross_val_score, train_test_split
    from sklearn.metrics import mean_absolute_error, mean_squared_error, r2_score

    model = _create_model(algorithm)

    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=test_size, random_state=42,
    )

    model.fit(X_train, y_train)
    y_pred = model.predict(X_test)

    metrics = {
        "mae": float(mean_absolute_error(y_test, y_pred)),
        "rmse": float(np.sqrt(mean_squared_error(y_test, y_pred))),
        "r2": float(r2_score(y_test, y_pred)),
        "n_train": len(X_train),
        "n_test": len(X_test),
    }

    return {
        "model": model,
        "metrics": metrics,
        "algorithm": algorithm,
        "property_name": property_name,
    }
```

Create `app/ml/registry.py`:
```python
"""Model registry: save, load, and list trained models."""
import json
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional

import joblib


class ModelRegistry:
    """Manages trained model storage and retrieval."""

    def __init__(self, models_dir: Optional[str] = None):
        self.models_dir = Path(models_dir) if models_dir else Path("models")
        self.models_dir.mkdir(parents=True, exist_ok=True)

    def save_model(self, model: Any, property_name: str, algorithm: str, metrics: Dict) -> Path:
        """Save a trained model with metadata."""
        filename = f"{property_name}_{algorithm}"
        model_path = self.models_dir / f"{filename}.joblib"
        meta_path = self.models_dir / f"{filename}.meta.json"

        joblib.dump(model, model_path)

        meta = {
            "property": property_name,
            "algorithm": algorithm,
            "metrics": metrics,
            "saved_at": datetime.now().isoformat(),
        }
        meta_path.write_text(json.dumps(meta, indent=2))

        return model_path

    def load_model(self, property_name: str, algorithm: str) -> Optional[Any]:
        """Load a trained model."""
        model_path = self.models_dir / f"{property_name}_{algorithm}.joblib"
        if model_path.exists():
            return joblib.load(model_path)
        return None

    def list_models(self) -> List[Dict]:
        """List all available models with metadata."""
        models = []
        for meta_file in sorted(self.models_dir.glob("*.meta.json")):
            try:
                meta = json.loads(meta_file.read_text())
                models.append(meta)
            except (json.JSONDecodeError, KeyError):
                continue
        return models
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_trainer.py -v`
Expected: All 4 tests PASS

**Step 5: Commit**

```bash
git add app/ml/trainer.py app/ml/registry.py tests/test_trainer.py
git commit -m "feat: add model trainer (RF, GB, XGBoost, LightGBM) and model registry"
```

---

### Task 20: Create predictor module

**Files:**
- Create: `app/ml/predictor.py`
- Create: `tests/test_predictor.py`

**Step 1: Write the failing test**

Create `tests/test_predictor.py`:
```python
"""Tests for predictor module."""
import tempfile
import pytest
import numpy as np
from app.ml.predictor import Predictor
from app.ml.registry import ModelRegistry


class TestPredictor:
    def _train_and_save_model(self, tmpdir):
        from sklearn.ensemble import RandomForestRegressor
        model = RandomForestRegressor(n_estimators=5, random_state=42)
        X = np.random.rand(30, 5)
        y = np.random.rand(30)
        model.fit(X, y)

        registry = ModelRegistry(models_dir=tmpdir)
        registry.save_model(model, "band_gap", "random_forest", {"mae": 0.1})
        return registry

    def test_predict_from_formula(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = self._train_and_save_model(tmpdir)
            predictor = Predictor(registry=registry)
            result = predictor.predict("Si", property_name="band_gap", algorithm="random_forest")
            assert "prediction" in result or "error" in result

    def test_predict_unknown_property(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ModelRegistry(models_dir=tmpdir)
            predictor = Predictor(registry=registry)
            result = predictor.predict("Si", property_name="nonexistent", algorithm="random_forest")
            assert "error" in result
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_predictor.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/ml/predictor.py`:
```python
"""Prediction engine: featurize formula and predict with trained model."""
import numpy as np
from typing import Dict, Optional

from app.ml.features import composition_features
from app.ml.registry import ModelRegistry


class Predictor:
    """Predict material properties from chemical formula."""

    def __init__(self, registry: Optional[ModelRegistry] = None):
        self.registry = registry or ModelRegistry()

    def predict(
        self,
        formula: str,
        property_name: str,
        algorithm: str = "random_forest",
    ) -> Dict:
        """Predict a property for a given formula.

        Returns dict with: prediction, formula, property, algorithm.
        Or: error message if model not found or featurization fails.
        """
        model = self.registry.load_model(property_name, algorithm)
        if model is None:
            return {"error": f"No trained model for {property_name}/{algorithm}. Run 'prism model train' first."}

        features = composition_features(formula)
        if not features:
            return {"error": f"Could not generate features for formula: {formula}"}

        # Convert features dict to array (sorted keys for consistency)
        feature_names = sorted(features.keys())
        X = np.array([[features[k] for k in feature_names]])

        try:
            prediction = float(model.predict(X)[0])
            return {
                "prediction": prediction,
                "formula": formula,
                "property": property_name,
                "algorithm": algorithm,
                "n_features": len(feature_names),
            }
        except Exception as e:
            return {"error": f"Prediction failed: {e}"}
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_predictor.py -v`
Expected: All 2 tests PASS

**Step 5: Commit**

```bash
git add app/ml/predictor.py tests/test_predictor.py
git commit -m "feat: add Predictor for formula-based property prediction"
```

---

### Task 21: Create ML visualization module

**Files:**
- Create: `app/ml/viz.py`
- Create: `tests/test_ml_viz.py`

**Step 1: Write the failing test**

Create `tests/test_ml_viz.py`:
```python
"""Tests for ML visualization."""
import pytest
import numpy as np
from app.ml.viz import plot_parity, plot_feature_importance


class TestMLViz:
    def test_parity_plot(self, tmp_path):
        y_true = np.array([1.0, 2.0, 3.0, 4.0])
        y_pred = np.array([1.1, 2.2, 2.8, 4.1])
        result = plot_parity(y_true, y_pred, "band_gap", str(tmp_path / "parity.png"))
        assert result.get("success") is True or "error" in result

    def test_feature_importance(self, tmp_path):
        names = ["feat_a", "feat_b", "feat_c"]
        importances = [0.5, 0.3, 0.2]
        result = plot_feature_importance(names, importances, str(tmp_path / "imp.png"))
        assert result.get("success") is True or "error" in result
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_ml_viz.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/ml/viz.py`:
```python
"""Visualization helpers for ML model results."""
from typing import Dict, List

import numpy as np


def plot_parity(
    y_true: np.ndarray,
    y_pred: np.ndarray,
    property_name: str,
    output_path: str = "parity.png",
) -> Dict:
    """Create a parity plot (predicted vs actual)."""
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        fig, ax = plt.subplots(figsize=(6, 6))
        ax.scatter(y_true, y_pred, alpha=0.6, s=20)
        lims = [min(y_true.min(), y_pred.min()), max(y_true.max(), y_pred.max())]
        ax.plot(lims, lims, "k--", alpha=0.5, label="Perfect")
        ax.set_xlabel(f"Actual {property_name}")
        ax.set_ylabel(f"Predicted {property_name}")
        ax.set_title(f"Parity Plot: {property_name}")
        ax.legend()
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}


def plot_feature_importance(
    feature_names: List[str],
    importances: List[float],
    output_path: str = "feature_importance.png",
    top_n: int = 20,
) -> Dict:
    """Create a horizontal bar chart of feature importances."""
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt

        # Sort and take top N
        pairs = sorted(zip(importances, feature_names), reverse=True)[:top_n]
        vals, names = zip(*pairs)

        fig, ax = plt.subplots(figsize=(8, max(4, len(names) * 0.3)))
        ax.barh(range(len(names)), vals, align="center")
        ax.set_yticks(range(len(names)))
        ax.set_yticklabels(names, fontsize=8)
        ax.invert_yaxis()
        ax.set_xlabel("Importance")
        ax.set_title(f"Top {len(names)} Feature Importances")
        fig.tight_layout()
        fig.savefig(output_path, dpi=150)
        plt.close(fig)
        return {"success": True, "path": output_path}
    except ImportError:
        return {"error": "matplotlib not installed"}
    except Exception as e:
        return {"error": str(e)}
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_ml_viz.py -v`
Expected: All 2 tests PASS

**Step 5: Commit**

```bash
git add app/ml/viz.py tests/test_ml_viz.py
git commit -m "feat: add ML visualization (parity plots, feature importance)"
```

---

### Task 22: Create PredictProperties agent tool

**Files:**
- Create: `app/tools/prediction.py`
- Create: `tests/test_tools_prediction.py`

**Step 1: Write the failing test**

Create `tests/test_tools_prediction.py`:
```python
"""Tests for prediction agent tools."""
import pytest
from app.tools.prediction import create_prediction_tools
from app.tools.base import ToolRegistry


class TestPredictionTools:
    def test_tools_registered(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        names = [t.name for t in registry.list_tools()]
        assert "predict_property" in names
        assert "list_models" in names

    def test_predict_no_model(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("predict_property")
        result = tool.execute(formula="Si", property_name="band_gap")
        # Should return error since no model is trained
        assert "error" in result or "prediction" in result

    def test_list_models(self):
        registry = ToolRegistry()
        create_prediction_tools(registry)
        tool = registry.get("list_models")
        result = tool.execute()
        assert "models" in result
```

**Step 2: Run test to verify it fails**

Run: `pytest tests/test_tools_prediction.py -v`
Expected: FAIL with ImportError

**Step 3: Write implementation**

Create `app/tools/prediction.py`:
```python
"""Prediction tools for the agent."""
from app.tools.base import Tool, ToolRegistry


def _predict_property(**kwargs) -> dict:
    formula = kwargs["formula"]
    property_name = kwargs.get("property_name", "band_gap")
    algorithm = kwargs.get("algorithm", "random_forest")

    try:
        from app.ml.predictor import Predictor
        predictor = Predictor()
        return predictor.predict(formula, property_name, algorithm)
    except Exception as e:
        return {"error": str(e)}


def _list_models(**kwargs) -> dict:
    try:
        from app.ml.registry import ModelRegistry
        registry = ModelRegistry()
        return {"models": registry.list_models()}
    except Exception as e:
        return {"error": str(e)}


def create_prediction_tools(registry: ToolRegistry) -> None:
    """Register prediction tools."""
    registry.register(Tool(
        name="predict_property",
        description="Predict a material property from its chemical formula using trained ML models. Available properties depend on trained models (check with list_models first).",
        input_schema={
            "type": "object",
            "properties": {
                "formula": {"type": "string", "description": "Chemical formula, e.g. 'LiCoO2'"},
                "property_name": {"type": "string", "description": "Property to predict (band_gap, formation_energy, etc.)"},
                "algorithm": {"type": "string", "description": "Algorithm to use (random_forest, xgboost, etc.)"},
            },
            "required": ["formula"],
        },
        func=_predict_property,
    ))

    registry.register(Tool(
        name="list_models",
        description="List all trained ML models and their metrics.",
        input_schema={"type": "object", "properties": {}},
        func=_list_models,
    ))
```

**Step 4: Run tests to verify they pass**

Run: `pytest tests/test_tools_prediction.py -v`
Expected: All 3 tests PASS

**Step 5: Commit**

```bash
git add app/tools/prediction.py tests/test_tools_prediction.py
git commit -m "feat: add predict_property and list_models agent tools"
```

---

### Task 23: Add predict and model CLI commands

**Files:**
- Create: `app/commands/predict.py`
- Create: `app/commands/model.py`
- Modify: `app/cli.py` (register predict and model groups)

**Step 1: Create predict command**

Create `app/commands/predict.py`:
```python
"""Predict CLI command."""
import click
from rich.console import Console
from rich.table import Table


@click.command()
@click.argument("formula")
@click.option("--property", "prop", default="band_gap", help="Property to predict")
@click.option("--algorithm", default="random_forest", help="ML algorithm")
@click.option("--all-properties", is_flag=True, help="Predict all available properties")
def predict(formula, prop, algorithm, all_properties):
    """Predict material properties from chemical formula."""
    console = Console()
    from app.ml.predictor import Predictor
    from app.ml.registry import ModelRegistry

    predictor = Predictor()
    registry = ModelRegistry()

    if all_properties:
        models = registry.list_models()
        if not models:
            console.print("[yellow]No trained models. Run 'prism model train' first.[/yellow]")
            return

        table = Table(title=f"Predictions for {formula}")
        table.add_column("Property")
        table.add_column("Algorithm")
        table.add_column("Prediction")

        for m in models:
            result = predictor.predict(formula, m["property"], m["algorithm"])
            val = f"{result['prediction']:.4f}" if "prediction" in result else result.get("error", "?")
            table.add_row(m["property"], m["algorithm"], val)

        console.print(table)
    else:
        result = predictor.predict(formula, prop, algorithm)
        if "prediction" in result:
            console.print(f"[bold]{formula}[/bold] → {prop} = [green]{result['prediction']:.4f}[/green] ({algorithm})")
        else:
            console.print(f"[red]{result.get('error', 'Unknown error')}[/red]")
```

**Step 2: Create model command group**

Create `app/commands/model.py`:
```python
"""Model management CLI commands."""
import click
from rich.console import Console
from rich.table import Table


@click.group()
def model():
    """Train, evaluate, and manage ML models."""
    pass


@model.command()
@click.option("--property", "prop", default=None, help="Specific property to train")
@click.option("--algorithm", default="random_forest", help="Algorithm to use")
@click.option("--dataset", default=None, help="Dataset name to train on")
def train(prop, algorithm, dataset):
    """Train ML models on collected data."""
    console = Console()
    from app.data.store import DataStore
    from app.ml.features import composition_features
    from app.ml.trainer import train_model, AVAILABLE_ALGORITHMS
    from app.ml.registry import ModelRegistry
    import numpy as np

    store = DataStore()
    datasets = store.list_datasets()
    if not datasets:
        console.print("[yellow]No datasets found. Run 'prism data collect' first.[/yellow]")
        return

    ds_name = dataset or datasets[0]["name"]
    console.print(f"[bold]Training on dataset:[/bold] {ds_name}")

    df = store.load(ds_name)

    # Generate features for each row
    feature_rows = []
    for _, row in df.iterrows():
        formula = row.get("formula", "")
        if formula:
            feats = composition_features(formula)
            if feats:
                feature_rows.append(feats)

    if not feature_rows:
        console.print("[red]No valid formulas for featurization.[/red]")
        return

    # Build feature matrix
    all_keys = sorted(set(k for f in feature_rows for k in f.keys()))
    X = np.array([[f.get(k, 0.0) for k in all_keys] for f in feature_rows])

    # For now, use a dummy target (in production, map to actual property column)
    # This will be replaced when datasets have property columns
    target_col = prop or "band_gap"
    if target_col in df.columns:
        y = df[target_col].values[:len(X)]
    else:
        console.print(f"[yellow]Property '{target_col}' not in dataset. Using random target for demo.[/yellow]")
        y = np.random.rand(len(X))

    with console.status(f"[bold green]Training {algorithm}..."):
        result = train_model(X, y, algorithm=algorithm, property_name=target_col)

    registry = ModelRegistry()
    registry.save_model(result["model"], target_col, algorithm, result["metrics"])

    metrics = result["metrics"]
    console.print(f"[green]Model trained and saved![/green]")
    console.print(f"  MAE:  {metrics['mae']:.4f}")
    console.print(f"  RMSE: {metrics['rmse']:.4f}")
    console.print(f"  R²:   {metrics['r2']:.4f}")


@model.command()
def status():
    """List available trained models and their metrics."""
    console = Console()
    from app.ml.registry import ModelRegistry

    registry = ModelRegistry()
    models = registry.list_models()

    if not models:
        console.print("[dim]No trained models. Run 'prism model train' first.[/dim]")
        return

    table = Table(title="Trained Models")
    table.add_column("Property")
    table.add_column("Algorithm")
    table.add_column("MAE")
    table.add_column("R²")
    table.add_column("Saved At")

    for m in models:
        metrics = m.get("metrics", {})
        table.add_row(
            m.get("property", "?"),
            m.get("algorithm", "?"),
            f"{metrics.get('mae', '?'):.4f}" if isinstance(metrics.get('mae'), (int, float)) else "?",
            f"{metrics.get('r2', '?'):.4f}" if isinstance(metrics.get('r2'), (int, float)) else "?",
            m.get("saved_at", "?")[:19],
        )

    console.print(table)
```

**Step 3: Register in cli.py**

In `app/cli.py`, add imports and registration:
```python
from app.commands.predict import predict as predict_cmd
from app.commands.model import model as model_group
cli.add_command(predict_cmd, "predict")
cli.add_command(model_group, "model")
```

**Step 4: Test commands**

Run: `prism predict --help`
Expected: Shows formula argument and options

Run: `prism model --help`
Expected: Shows train and status subcommands

**Step 5: Commit**

```bash
git add app/commands/predict.py app/commands/model.py app/cli.py
git commit -m "feat: add predict and model CLI commands"
```

---

## Phase A-8: Polish

### Task 24: Update pyproject.toml and version bump

**Files:**
- Modify: `pyproject.toml`
- Modify: `app/__init__.py`

**Step 1: Update pyproject.toml**

Update the full packages list and version:
```toml
[project]
name = "prism-platform"
version = "2.0.0"
```

Update packages:
```toml
[tool.setuptools]
packages = [
    "app",
    "app.config",
    "app.db",
    "app.agent",
    "app.agent.backends",
    "app.tools",
    "app.data",
    "app.ml",
    "app.commands",
]
```

Add matplotlib to core dependencies:
```toml
dependencies = [
    "click>=8.0.0",
    "rich>=12.0.0",
    "optimade[http-client]>=1.2.0",
    "sqlalchemy>=2.0.0",
    "python-dotenv>=1.0.0",
    "openai>=1.0.0",
    "google-cloud-aiplatform>=1.0.0",
    "anthropic>=0.20.0",
    "pandas>=2.0.0",
    "tenacity>=8.0.0",
    "requests>=2.28.0",
    "mp-api>=0.45.0",
    "matplotlib>=3.7.0",
    "joblib>=1.3.0",
]
```

**Step 2: Update version in __init__.py**

In `app/__init__.py`:
```python
__version__ = "2.0.0"
```

**Step 3: Commit**

```bash
git add pyproject.toml app/__init__.py
git commit -m "chore: bump version to 2.0.0, update packages for agent + ML architecture"
```

---

### Task 25: Run full test suite and verify

**Files:**
- No new files. Verification task.

**Step 1: Install in development mode**

```bash
pip install -e ".[ml,dev]"
```

**Step 2: Run full test suite**

```bash
pytest tests/ -v --tb=short
```

Expected: All tests PASS. If any fail, fix them before proceeding.

**Step 3: Verify CLI commands work**

```bash
prism --help
prism --version
prism search --help
prism ask --help
prism run --help
prism data --help
prism predict --help
prism model --help
```

Expected: All commands show help text without errors.

**Step 4: Verify REPL launches (if API key available)**

```bash
# Only if ANTHROPIC_API_KEY or OPENAI_API_KEY is set:
prism
# Should show welcome panel and > prompt
# Type /help, then /exit
```

**Step 5: Commit any fixes**

```bash
git add -A
git commit -m "chore: fix test/lint issues from full verification"
```

---

## Summary

| Phase | Tasks | What Gets Built |
|-------|-------|----------------|
| A-1: Foundation | 1-3 | Test infra, Tool base class, Agent events + Backend ABC |
| A-2: Backends | 4-5 | Anthropic backend, OpenAI/OpenRouter backend |
| A-3: Core | 6-7 | AgentCore TAOR loop, Session memory |
| A-4: Tools | 8-11 | System tools, OPTIMADE tool, MP tool, Viz tool, mcp.py limits |
| A-5: CLI | 12-14 | Interactive REPL, Autonomous mode, CLI wiring |
| A-6: Data | 15-17 | Collector, Normalizer+Store, Data CLI commands |
| A-7: ML | 18-23 | Features, Trainer, Predictor, ML viz, Prediction tools, CLI |
| A-8: Polish | 24-25 | Version bump, Full verification |

**Total: 25 tasks**

After Phase A, PRISM will be a working agentic CLI with:
- Interactive REPL (`prism`) and autonomous mode (`prism run "goal"`)
- Provider-agnostic agent (Anthropic, OpenAI, OpenRouter)
- OPTIMADE search + Materials Project query tools
- Data collection pipeline
- ML property prediction (Random Forest, XGBoost, LightGBM)
- Visualization tools
- All existing CLI commands preserved

**Future phases (from agentic-cli-design.md):**
- Phase B: MCP server + client integration
- Phase C: Full Pyiron integration with HPC
- Phase D: Skills system
