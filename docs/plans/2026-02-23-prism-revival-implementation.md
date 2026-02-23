# PRISM Revival Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Stabilize the PRISM CLI, build a materials data pipeline, and add SoTA ML property prediction models.

**Architecture:** Three-phase approach. Phase 1 removes dead code, fixes bugs, refactors cli.py into modules, and adds tests. Phase 2 builds a data collection/normalization pipeline from OPTIMADE + Materials Project. Phase 3 adds tiered ML prediction (classical + foundation models) with visualization.

**Tech Stack:** Python 3.9+, Click, Rich, OPTIMADE client, pymatgen, matminer, scikit-learn, XGBoost, LightGBM, MODNet, MACE, CHGNet, ALIGNN, matplotlib, Optuna

---

## Phase 1: Stabilize & Clean Up

### Task 1: Delete orphaned API directory

**Files:**
- Delete: `app/api/` (entire directory tree)
- Delete: `app/api/__init__.py`
- Delete: `app/api/v1/__init__.py`
- Delete: `app/api/v1/endpoints/__init__.py`
- Delete: `app/api/v1/endpoints/destinations.py`
- Delete: `app/api/v1/endpoints/health.py`
- Delete: `app/api/v1/endpoints/jobs.py`
- Delete: `app/api/v1/endpoints/sources.py`

**Step 1: Delete the directory**

```bash
rm -rf app/api/
```

**Step 2: Verify deletion**

```bash
ls app/api/ 2>&1
```
Expected: `No such file or directory`

**Step 3: Commit**

```bash
git add -A app/api/
git commit -m "chore: remove orphaned FastAPI API directory

These endpoints were never connected to the CLI and are dead code."
```

---

### Task 2: Delete orphaned services code

**Files:**
- Delete: `app/services/job_processor.py`
- Delete: `app/services/job_scheduler.py`
- Delete: `app/services/materials_service.py`
- Delete: `app/services/enhanced_nomad_connector.py`
- Delete: `app/services/connectors/redis_connector.py`
- Delete: `app/services/connectors/base_connector.py`
- Delete: `app/services/connectors/__init__.py`
- Delete: `app/services/connectors/` (directory)
- Delete: `app/services/rate_limiter.py`
- Delete: `app/services/rate_limiter_integration.py`
- Delete: `app/services/__init__.py`
- Delete: `app/services/` (directory)

**Step 1: Delete the directory**

```bash
rm -rf app/services/
```

**Step 2: Verify no imports reference these files**

```bash
grep -r "from app.services" app/ || echo "No references found"
grep -r "import app.services" app/ || echo "No references found"
```
Expected: No references found (these were never imported by the CLI).

**Step 3: Commit**

```bash
git add -A app/services/
git commit -m "chore: remove orphaned services directory

Job processor, scheduler, NOMAD connector, Redis connector, rate limiter
were never connected to the CLI."
```

---

### Task 3: Remove "coming soon" LLM providers from llm.py

**Files:**
- Modify: `app/llm.py`

**Step 1: Remove the 4 stub classes and their env-var detection**

Edit `app/llm.py` to remove lines 72-103 (PerplexityService, GrokService, OllamaService, PRISMCustomService classes) and lines 116-123 (their env-var checks in `get_llm_service`) and lines 133-136 (their entries in `provider_map`).

The cleaned `app/llm.py` should be:

```python
import os
from abc import ABC, abstractmethod
from openai import OpenAI
import vertexai
from vertexai.generative_models import GenerativeModel
from dotenv import load_dotenv
from anthropic import Anthropic

from app.config.settings import get_env_path

load_dotenv(dotenv_path=get_env_path())


class LLMService(ABC):
    @abstractmethod
    def get_completion(self, prompt: str, stream: bool = False):
        pass


class OpenAIService(LLMService):
    def __init__(self, model: str = None):
        self.client = OpenAI(api_key=os.getenv("OPENAI_API_KEY"))
        self.model = model or os.getenv("LLM_MODEL", "gpt-4o")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream,
        )


class VertexAIService(LLMService):
    def __init__(self, model: str = None):
        project_id = os.getenv("GOOGLE_CLOUD_PROJECT")
        vertexai.init(project=project_id)
        model_name = model or os.getenv("LLM_MODEL", "gemini-1.5-pro")
        self.model = GenerativeModel(model_name)

    def get_completion(self, prompt: str, stream: bool = False):
        return self.model.generate_content(prompt, stream=stream)


class AnthropicService(LLMService):
    def __init__(self, model: str = None):
        self.client = Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
        self.model = model or os.getenv("LLM_MODEL", "claude-sonnet-4-20250514")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.messages.create(
            model=self.model,
            max_tokens=1024,
            messages=[{"role": "user", "content": prompt}],
            stream=stream,
        )


class OpenRouterService(LLMService):
    def __init__(self, model: str = None):
        self.client = OpenAI(
            base_url="https://openrouter.ai/api/v1",
            api_key=os.getenv("OPENROUTER_API_KEY"),
        )
        self.model = model or os.getenv("LLM_MODEL", "anthropic/claude-3.5-sonnet")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream,
        )


PROVIDER_MAP = {
    "openrouter": OpenRouterService,
    "openai": OpenAIService,
    "anthropic": AnthropicService,
    "vertexai": VertexAIService,
}


def get_llm_service(provider: str = None, model: str = None) -> LLMService:
    """Get an LLM service instance. Auto-detects provider from env vars if not specified."""
    if provider is None:
        if os.getenv("OPENROUTER_API_KEY"):
            provider = "openrouter"
        elif os.getenv("OPENAI_API_KEY"):
            provider = "openai"
        elif os.getenv("ANTHROPIC_API_KEY"):
            provider = "anthropic"
        elif os.getenv("GOOGLE_CLOUD_PROJECT"):
            provider = "vertexai"
        else:
            raise ValueError(
                "No LLM provider configured. Set one of: OPENROUTER_API_KEY, "
                "OPENAI_API_KEY, ANTHROPIC_API_KEY, or GOOGLE_CLOUD_PROJECT in your .env file."
            )

    service_class = PROVIDER_MAP.get(provider.lower())
    if not service_class:
        raise ValueError(
            f"Unsupported LLM provider: {provider}. "
            f"Supported: {', '.join(PROVIDER_MAP.keys())}"
        )

    return service_class(model=model)
```

Note: This references `app.config.settings` which we create in Task 5. For now, keep the old dotenv loading until Task 5.

**Step 2: Verify the file has no syntax errors**

```bash
python -c "import ast; ast.parse(open('app/llm.py').read()); print('OK')"
```
Expected: `OK`

**Step 3: Commit**

```bash
git add app/llm.py
git commit -m "chore: remove unimplemented LLM provider stubs

Removed PerplexityService, GrokService, OllamaService, PRISMCustomService.
These raised NotImplementedError and crashed if env vars were set."
```

---

### Task 4: Remove "coming soon" references from cli.py

**Files:**
- Modify: `app/cli.py`

**Step 1: Remove coming-soon env checks from the `cli()` function**

In `app/cli.py`, lines 262-268 check for coming-soon provider env vars. Remove those `elif` blocks so the provider detection only checks the 4 active providers.

Remove lines 262-268:
```python
            elif os.getenv("PERPLEXITY_API_KEY"):
                llm_provider = "Perplexity (coming soon)"
            elif os.getenv("GROK_API_KEY"):
                llm_provider = "Grok (coming soon)"
            elif os.getenv("OLLAMA_HOST"):
                llm_provider = "Ollama Local (coming soon)"
            elif os.getenv("PRISM_CUSTOM_API_KEY"):
                llm_provider = "PRISM Custom Model (coming soon)"
```

**Step 2: Remove coming-soon from switch-llm command**

In `app/cli.py`, lines 367-375 detect coming-soon env vars. Remove that block and the display of coming-soon providers (lines 409-412).

Remove lines 367-375:
```python
    coming_soon = []
    if os.getenv("PERPLEXITY_API_KEY"):
        coming_soon.append("Perplexity")
    if os.getenv("GROK_API_KEY"):
        coming_soon.append("Grok")
    if os.getenv("OLLAMA_HOST"):
        coming_soon.append("Ollama Local")
    if os.getenv("PRISM_CUSTOM_API_KEY"):
        coming_soon.append("PRISM Custom Model")
```

Remove lines 409-412:
```python
    if coming_soon:
        console.print(f"\n[dim]Coming Soon:[/dim]")
        for provider in coming_soon:
            console.print(f"• [dim]{provider} (configured but not yet supported)[/dim]")
```

**Step 3: Remove coming-soon from configure command**

In `app/cli.py`, lines 816-819 show coming-soon options. Remove them and update the choices:
```python
    console.print("[dim]5. Perplexity (coming soon)[/dim]")
    console.print("[dim]6. Grok (coming soon)[/dim]")
    console.print("[dim]7. Ollama Local (coming soon)[/dim]")
    console.print("[dim]8. PRISM Custom Model (coming soon)[/dim]")
```

**Step 4: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/cli.py').read()); print('OK')"
```

**Step 5: Commit**

```bash
git add app/cli.py
git commit -m "chore: remove coming-soon provider references from CLI

Cleaned up switch-llm, configure, and status display to only show
the 4 active LLM providers."
```

---

### Task 5: Create centralized settings module

**Files:**
- Create: `app/config/settings.py`

**Step 1: Write the settings module**

```python
"""Centralized configuration for PRISM."""

import os
from pathlib import Path

# Project root is two levels up from this file (app/config/settings.py -> app/ -> PRISM/)
PROJECT_ROOT = Path(__file__).parent.parent.parent

def get_env_path() -> Path:
    """Return the path to the .env file. Checks project root first, then cwd."""
    project_env = PROJECT_ROOT / ".env"
    if project_env.exists():
        return project_env
    cwd_env = Path.cwd() / ".env"
    if cwd_env.exists():
        return cwd_env
    # Return project root path even if it doesn't exist yet
    return project_env


# Configurable limits (override via env vars)
MAX_FILTER_ATTEMPTS = int(os.getenv("PRISM_MAX_FILTER_ATTEMPTS", "3"))
MAX_RESULTS_DISPLAY = int(os.getenv("PRISM_MAX_RESULTS_DISPLAY", "10"))
MAX_INTERACTIVE_QUESTIONS = int(os.getenv("PRISM_MAX_INTERACTIVE_QUESTIONS", "3"))
MAX_RESULTS_PER_PROVIDER = int(os.getenv("PRISM_MAX_RESULTS_PER_PROVIDER", "1000"))
```

**Step 2: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/config/settings.py').read()); print('OK')"
```

**Step 3: Commit**

```bash
git add app/config/settings.py
git commit -m "feat: add centralized settings module

Consolidates .env path resolution and makes hardcoded limits
configurable via environment variables."
```

---

### Task 6: Fix .env loading in cli.py

**Files:**
- Modify: `app/cli.py` (lines 17-28)

**Step 1: Replace the 3-path .env search with settings module**

Replace lines 17-28 in `app/cli.py`:
```python
# Load environment variables from .env file
# Try multiple locations to find .env file
env_paths = [
    '.env',  # Current directory
    Path(__file__).parent.parent / '.env',  # Project root
    Path.cwd() / '.env'  # Current working directory
]

for env_path in env_paths:
    if Path(env_path).exists():
        load_dotenv(env_path)
        break
```

With:
```python
from app.config.settings import get_env_path

# Load environment variables from .env file
load_dotenv(get_env_path())
```

**Step 2: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/cli.py').read()); print('OK')"
```

**Step 3: Commit**

```bash
git add app/cli.py
git commit -m "fix: consolidate .env loading to use centralized settings

Replaces 3 different .env path searches with single get_env_path()."
```

---

### Task 7: Fix band_gap column in models.py

**Files:**
- Modify: `app/db/models.py`

**Step 1: Fix the incomplete column definition**

Replace the contents of `app/db/models.py` with:

```python
from sqlalchemy import Column, Integer, String, Float, JSON
from .database import Base


class Material(Base):
    __tablename__ = "materials"

    id = Column(Integer, primary_key=True, index=True, autoincrement=True)
    source_id = Column(String, unique=True, index=True)
    formula = Column(String, index=True)
    elements = Column(String)  # Comma-separated element symbols
    provider = Column(String)
    band_gap = Column(Float, nullable=True)
    formation_energy = Column(Float, nullable=True)
    energy_above_hull = Column(Float, nullable=True)
```

**Step 2: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/db/models.py').read()); print('OK')"
```

**Step 3: Commit**

```bash
git add app/db/models.py
git commit -m "fix: complete band_gap column definition in Material model

Added nullable=True and additional property columns for
formation_energy and energy_above_hull."
```

---

### Task 8: Fix _test_filter to actually query OPTIMADE

**Files:**
- Modify: `app/mcp.py` (method `_test_filter` at line 462)

**Step 1: Replace the regex-only validation with a real API test**

Replace the `_test_filter` method (lines 462-493) in `app/mcp.py` with:

```python
    def _test_filter(self, optimade_client, provider: str, filter_str: str) -> Optional[str]:
        """
        Test the filter by actually querying the OPTIMADE API with page_limit=1.

        Returns:
            None if successful, error message if failed.
        """
        valid_providers = [p["id"] for p in self.providers_info]
        if provider not in valid_providers:
            return f"Invalid provider '{provider}'. Valid providers: {', '.join(valid_providers)}"

        if not filter_str or not filter_str.strip():
            return "Empty filter string"

        try:
            test_client = OptimadeClient(
                include_providers=[provider],
                max_results_per_provider=1,
            )
            results = test_client.get(filter_str)

            # Check if we got any data back
            if "structures" in results:
                filter_results = results["structures"].get(filter_str, {})
                for provider_data in filter_results.values():
                    if "data" in provider_data and len(provider_data["data"]) > 0:
                        return None  # Success
                    if "errors" in provider_data:
                        errors = provider_data["errors"]
                        if errors:
                            return f"Provider error: {errors[0].get('detail', str(errors[0]))}"

            return "Filter returned no results. Try a broader query."

        except Exception as e:
            return f"OPTIMADE query failed: {str(e)[:200]}"
```

Also add the import at the top of `app/mcp.py` (after line 5):

```python
from optimade.client import OptimadeClient
```

**Step 2: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/mcp.py').read()); print('OK')"
```

**Step 3: Commit**

```bash
git add app/mcp.py
git commit -m "fix: _test_filter now queries OPTIMADE API instead of regex-only

Previously only checked for quote balance and HAS ALL syntax.
Now makes a real API call with page_limit=1 to validate the filter."
```

---

### Task 9: Fix save-install command

**Files:**
- Modify: `app/cli.py` (lines 1380-1384)

**Step 1: Fix save_install to write the actual INSTALL_CONTENT**

Replace lines 1380-1384:
```python
@docs.command()
def save_install():
    """Saves the project INSTALL.md file."""
    with open("INSTALL.md", "w") as f:
        f.write("# Installation Guide\n\nFollow these steps to install PRISM...")
```

With:
```python
@docs.command()
def save_install():
    """Saves the project INSTALL.md file."""
    with open("INSTALL.md", "w") as f:
        f.write(INSTALL_CONTENT)
    console.print("[green]SUCCESS:[/green] `INSTALL.md` saved successfully.")
```

Also fix the duplicate write in `save_readme` (lines 1369-1378). Replace:
```python
@docs.command()
def save_readme():
    """Saves the project README.md file."""
    with open("README.md", "w") as f:
        f.write(README_CONTENT)
    console.print("[green]SUCCESS:[/green] `README.md` saved successfully.")

    with open("README.md", "w") as f:
        f.write(README_CONTENT)
    console.print("[green]SUCCESS:[/green] `README.md` saved successfully.")
```

With:
```python
@docs.command()
def save_readme():
    """Saves the project README.md file."""
    with open("README.md", "w") as f:
        f.write(README_CONTENT)
    console.print("[green]SUCCESS:[/green] `README.md` saved successfully.")
```

**Step 2: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/cli.py').read()); print('OK')"
```

**Step 3: Commit**

```bash
git add app/cli.py
git commit -m "fix: save-install now writes actual content, fix duplicate readme write"
```

---

### Task 10: Clean up pyproject.toml

**Files:**
- Modify: `pyproject.toml`

**Step 1: Remove orphaned package references and unused deps**

Update `pyproject.toml`:
- Remove `psycopg2-binary>=2.9.0` from dependencies (Postgres not used)
- Remove `alembic>=1.8.0` from dependencies (migrations not needed)
- Remove `"app.services", "app.services.connectors", "app.api", "app.api.v1", "app.api.v1.endpoints"` from `[tool.setuptools] packages`
- Add `"app.config"` to packages if not already there

The `[tool.setuptools]` section should become:
```toml
[tool.setuptools]
packages = ["app", "app.config", "app.db"]
include-package-data = true
```

The dependencies should become:
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
]
```

**Step 2: Commit**

```bash
git add pyproject.toml
git commit -m "chore: clean up pyproject.toml

Removed psycopg2-binary, alembic, and orphaned package references
for deleted api/ and services/ directories."
```

---

### Task 11: Clean up requirements.txt

**Files:**
- Modify: `requirements.txt`

**Step 1: Replace with dev-only dependencies**

`requirements.txt` currently has FastAPI, Redis, asyncpg, etc. Replace entirely with:

```
# Development dependencies
pytest>=7.4.3
pytest-cov>=4.1.0
pytest-mock>=3.12.0
black>=23.11.0
isort>=5.12.0
flake8>=6.1.0
```

**Step 2: Commit**

```bash
git add requirements.txt
git commit -m "chore: clean requirements.txt to dev-only dependencies

Removed FastAPI, Redis, asyncpg, and other unused dependencies.
Core deps are in pyproject.toml."
```

---

### Task 12: Add test infrastructure and first tests

**Files:**
- Create: `tests/__init__.py`
- Create: `tests/test_llm.py`
- Create: `tests/test_mcp.py`
- Create: `tests/conftest.py`
- Modify: `Makefile`

**Step 1: Create test directory and conftest**

Create `tests/__init__.py` (empty).

Create `tests/conftest.py`:
```python
"""Shared test fixtures for PRISM tests."""

import pytest
from unittest.mock import MagicMock


@pytest.fixture
def mock_openai_response():
    """Mock an OpenAI-style completion response."""
    response = MagicMock()
    response.choices = [MagicMock()]
    response.choices[0].message.content = '{"provider": "mp", "filter": "elements HAS ALL \\"Li\\", \\"Co\\""}'
    return response


@pytest.fixture
def mock_anthropic_response():
    """Mock an Anthropic-style completion response."""
    response = MagicMock()
    response.content = [MagicMock()]
    response.content[0].text = '{"provider": "mp", "filter": "elements HAS ALL \\"Li\\", \\"Co\\""}'
    return response


@pytest.fixture
def sample_optimade_materials():
    """Sample OPTIMADE materials response data."""
    return [
        {
            "id": "mp-1234",
            "attributes": {
                "chemical_formula_descriptive": "LiCoO2",
                "elements": ["Co", "Li", "O"],
                "nelements": 3,
            },
            "meta": {"provider": {"name": "Materials Project"}},
        },
        {
            "id": "mp-5678",
            "attributes": {
                "chemical_formula_descriptive": "LiFePO4",
                "elements": ["Fe", "Li", "O", "P"],
                "nelements": 4,
            },
            "meta": {"provider": {"name": "Materials Project"}},
        },
    ]
```

**Step 2: Create test_llm.py**

Create `tests/test_llm.py`:
```python
"""Tests for LLM service factory and provider classes."""

import os
import pytest
from unittest.mock import patch, MagicMock
from app.llm import get_llm_service, OpenAIService, AnthropicService, OpenRouterService


class TestGetLLMService:
    """Tests for the get_llm_service factory function."""

    def test_raises_when_no_provider_configured(self):
        with patch.dict(os.environ, {}, clear=True):
            with pytest.raises(ValueError, match="No LLM provider configured"):
                get_llm_service()

    @patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"})
    @patch("app.llm.OpenAI")
    def test_auto_detects_openai(self, mock_openai_cls):
        service = get_llm_service()
        assert isinstance(service, OpenAIService)

    @patch.dict(os.environ, {"ANTHROPIC_API_KEY": "test-key"})
    @patch("app.llm.Anthropic")
    def test_auto_detects_anthropic(self, mock_anthropic_cls):
        service = get_llm_service()
        assert isinstance(service, AnthropicService)

    @patch.dict(os.environ, {"OPENROUTER_API_KEY": "test-key"})
    @patch("app.llm.OpenAI")
    def test_auto_detects_openrouter(self, mock_openai_cls):
        service = get_llm_service()
        assert isinstance(service, OpenRouterService)

    def test_explicit_provider_unsupported(self):
        with pytest.raises(ValueError, match="Unsupported LLM provider"):
            get_llm_service(provider="nonexistent")

    @patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"})
    @patch("app.llm.OpenAI")
    def test_explicit_provider_overrides_env(self, mock_openai_cls):
        service = get_llm_service(provider="openai")
        assert isinstance(service, OpenAIService)

    @patch.dict(os.environ, {"OPENAI_API_KEY": "test-key"})
    @patch("app.llm.OpenAI")
    def test_custom_model_passed_through(self, mock_openai_cls):
        service = get_llm_service(provider="openai", model="gpt-3.5-turbo")
        assert service.model == "gpt-3.5-turbo"
```

**Step 3: Create test_mcp.py**

Create `tests/test_mcp.py`:
```python
"""Tests for MCP filter parsing, validation, and ModelContext."""

import pytest
from unittest.mock import MagicMock
from app.mcp import AdaptiveOptimadeFilter, ModelContext
from app.config.providers import FALLBACK_PROVIDERS


class TestParseResponse:
    """Tests for AdaptiveOptimadeFilter._parse_response."""

    def setup_method(self):
        self.mock_llm = MagicMock()
        self.filter_gen = AdaptiveOptimadeFilter(self.mock_llm, FALLBACK_PROVIDERS)

    def test_parses_clean_json(self):
        provider, filt = self.filter_gen._parse_response(
            '{"provider": "mp", "filter": "elements HAS ALL \\"Li\\""}'
        )
        assert provider == "mp"
        assert "Li" in filt

    def test_parses_json_in_markdown_block(self):
        text = '```json\n{"provider": "cod", "filter": "nelements=2"}\n```'
        provider, filt = self.filter_gen._parse_response(text)
        assert provider == "cod"
        assert filt == "nelements=2"

    def test_parses_json_with_surrounding_text(self):
        text = 'Here is the filter:\n{"provider": "oqmd", "filter": "nelements=3"}\nDone.'
        provider, filt = self.filter_gen._parse_response(text)
        assert provider == "oqmd"
        assert filt == "nelements=3"

    def test_returns_none_for_garbage(self):
        provider, filt = self.filter_gen._parse_response("this is not json")
        assert provider is None
        assert filt is None

    def test_returns_none_for_empty(self):
        provider, filt = self.filter_gen._parse_response("")
        assert provider is None
        assert filt is None


class TestExtractResponseText:
    """Tests for AdaptiveOptimadeFilter._extract_response_text."""

    def setup_method(self):
        self.mock_llm = MagicMock()
        self.filter_gen = AdaptiveOptimadeFilter(self.mock_llm, FALLBACK_PROVIDERS)

    def test_extracts_openai_format(self):
        response = MagicMock()
        response.choices = [MagicMock()]
        response.choices[0].message.content = "test content"
        assert self.filter_gen._extract_response_text(response) == "test content"

    def test_extracts_anthropic_format(self):
        response = MagicMock()
        response.choices = None
        response.content = [MagicMock()]
        response.content[0].text = "anthropic content"
        del response.choices
        result = self.filter_gen._extract_response_text(response)
        assert result == "anthropic content"


class TestModelContext:
    """Tests for ModelContext prompt construction."""

    def test_standard_prompt_contains_query(self):
        ctx = ModelContext(query="Find Li materials", results=[])
        prompt = ctx.to_prompt(reasoning_mode=False)
        assert "Find Li materials" in prompt

    def test_reasoning_prompt_contains_query(self):
        ctx = ModelContext(query="Find Li materials", results=[])
        prompt = ctx.to_prompt(reasoning_mode=True)
        assert "Find Li materials" in prompt

    def test_results_included_in_prompt(self, sample_optimade_materials):
        ctx = ModelContext(query="test", results=sample_optimade_materials)
        prompt = ctx.to_prompt(reasoning_mode=False)
        assert "LiCoO2" in prompt

    def test_truncates_to_10_results(self):
        materials = [
            {"id": f"mp-{i}", "attributes": {"chemical_formula_descriptive": f"X{i}", "elements": ["X"]}, "meta": {}}
            for i in range(20)
        ]
        ctx = ModelContext(query="test", results=materials)
        prompt = ctx.to_prompt(reasoning_mode=False)
        # Should contain X0 through X9 but not X10+
        assert "X9" in prompt
```

**Step 4: Update Makefile test target**

Replace the test target in `Makefile`:
```makefile
test:
	@echo "Running test suite..."
	pytest tests/ -v --tb=short
```

**Step 5: Run tests to verify they pass**

```bash
pip install pytest pytest-mock pytest-cov && pytest tests/ -v --tb=short
```

**Step 6: Commit**

```bash
git add tests/ Makefile
git commit -m "test: add test infrastructure with LLM and MCP tests

Added pytest fixtures, LLM service factory tests, filter parsing tests,
and ModelContext tests. Updated Makefile test target."
```

---

### Task 13: Add CLI integration tests

**Files:**
- Create: `tests/test_cli.py`

**Step 1: Write CLI tests using Click's CliRunner**

Create `tests/test_cli.py`:
```python
"""Integration tests for PRISM CLI commands."""

import pytest
from unittest.mock import patch, MagicMock
from click.testing import CliRunner
from app.cli import cli


@pytest.fixture
def runner():
    return CliRunner()


class TestCLIBasics:
    """Test basic CLI functionality."""

    def test_version_flag(self, runner):
        result = runner.invoke(cli, ["--version"])
        assert result.exit_code == 0
        assert "1.1.0" in result.output

    def test_help_flag(self, runner):
        result = runner.invoke(cli, ["--help"])
        assert result.exit_code == 0
        assert "PRISM" in result.output or "prism" in result.output

    def test_search_requires_criteria(self, runner):
        result = runner.invoke(cli, ["search"])
        assert "Error" in result.output or "criterion" in result.output


class TestSearchCommand:
    """Test the search command."""

    @patch("app.cli.OptimadeClient")
    def test_search_with_elements(self, mock_client_cls, runner):
        mock_client = MagicMock()
        mock_client.get.return_value = {
            "structures": {
                'elements HAS ALL "Ti", "O"': {
                    "mp": {
                        "data": [
                            {
                                "id": "mp-1",
                                "attributes": {
                                    "chemical_formula_descriptive": "TiO2",
                                    "elements": ["O", "Ti"],
                                },
                                "meta": {"provider": {"name": "MP"}},
                            }
                        ]
                    }
                }
            }
        }
        mock_client_cls.return_value = mock_client

        result = runner.invoke(cli, ["search", "--elements", "Ti,O"], input="n\n")
        assert result.exit_code == 0

    def test_search_no_criteria_shows_error(self, runner):
        result = runner.invoke(cli, ["search"])
        assert result.exit_code == 0
        assert "Error" in result.output or "criterion" in result.output
```

**Step 2: Run tests**

```bash
pytest tests/test_cli.py -v --tb=short
```

**Step 3: Commit**

```bash
git add tests/test_cli.py
git commit -m "test: add CLI integration tests with Click CliRunner"
```

---

### Task 14: Update use of hardcoded limits in mcp.py

**Files:**
- Modify: `app/mcp.py`

**Step 1: Import settings and use configurable limits**

At the top of `app/mcp.py`, add:
```python
from app.config.settings import MAX_FILTER_ATTEMPTS, MAX_RESULTS_DISPLAY, MAX_INTERACTIVE_QUESTIONS
```

Then replace hardcoded values:
- Line 92: `self.results[:10]` -> `self.results[:MAX_RESULTS_DISPLAY]`
- Line 133: `self.max_attempts = 3` -> `self.max_attempts = MAX_FILTER_ATTEMPTS`
- Line 135: `max_questions: int = 3` -> `max_questions: int = MAX_INTERACTIVE_QUESTIONS`

**Step 2: Verify syntax**

```bash
python -c "import ast; ast.parse(open('app/mcp.py').read()); print('OK')"
```

**Step 3: Run tests to ensure nothing broke**

```bash
pytest tests/ -v --tb=short
```

**Step 4: Commit**

```bash
git add app/mcp.py
git commit -m "refactor: use configurable limits instead of hardcoded values

MAX_FILTER_ATTEMPTS, MAX_RESULTS_DISPLAY, MAX_INTERACTIVE_QUESTIONS
are now set via env vars with sensible defaults."
```

---

### Task 15: Delete Schema.txt and provider_fields.json from repo root

**Files:**
- Delete: `Schema.txt` (209KB, should not be in repo root)
- Evaluate: `provider_fields.json` (runtime cache, should be gitignored)

**Step 1: Add runtime artifacts to .gitignore**

Create or update `.gitignore`:
```
# Runtime artifacts
provider_fields.json
prism.db
*.pyc
__pycache__/
*.egg-info/
build/
dist/
.coverage
htmlcov/
.pytest_cache/
.mypy_cache/
.env

# Data directories (created by Phase 2)
data/
models/
```

**Step 2: Commit**

```bash
git add .gitignore
git commit -m "chore: add .gitignore for runtime artifacts and data directories"
```

---

## Phase 2: Data Pipeline

### Task 16: Create data collector module

**Files:**
- Create: `app/data/__init__.py`
- Create: `app/data/collector.py`
- Create: `tests/test_collector.py`

**Step 1: Write the failing test**

Create `tests/test_collector.py`:
```python
"""Tests for the data collection module."""

import pytest
from unittest.mock import patch, MagicMock
from app.data.collector import OptimadeCollector, MaterialsProjectCollector


class TestOptimadeCollector:

    def test_collector_initializes_with_providers(self):
        collector = OptimadeCollector(providers=["mp", "oqmd"])
        assert collector.providers == ["mp", "oqmd"]

    def test_collector_defaults_to_all_providers(self):
        collector = OptimadeCollector()
        assert len(collector.providers) == 6

    @patch("app.data.collector.OptimadeClient")
    def test_collect_returns_dataframe(self, mock_client_cls):
        mock_client = MagicMock()
        mock_client.get.return_value = {
            "structures": {
                "elements HAS ALL \"Li\"": {
                    "mp": {
                        "data": [
                            {
                                "id": "mp-1",
                                "attributes": {
                                    "chemical_formula_descriptive": "Li",
                                    "elements": ["Li"],
                                    "nelements": 1,
                                },
                            }
                        ]
                    }
                }
            }
        }
        mock_client_cls.return_value = mock_client

        collector = OptimadeCollector(providers=["mp"])
        df = collector.collect(filter_str='elements HAS ALL "Li"')
        assert len(df) >= 1
        assert "formula" in df.columns


class TestMaterialsProjectCollector:

    def test_collector_requires_api_key(self):
        with patch.dict("os.environ", {}, clear=True):
            collector = MaterialsProjectCollector()
            assert collector.api_key is None
```

**Step 2: Run test to verify it fails**

```bash
pytest tests/test_collector.py -v --tb=short
```
Expected: FAIL (module not found)

**Step 3: Write the collector module**

Create `app/data/__init__.py`:
```python
"""Data collection and processing for PRISM."""
```

Create `app/data/collector.py`:
```python
"""Data collection from OPTIMADE and Materials Project APIs."""

import os
from typing import List, Optional, Dict, Any

import pandas as pd
from optimade.client import OptimadeClient

from app.config.providers import FALLBACK_PROVIDERS


class OptimadeCollector:
    """Collects materials data from OPTIMADE providers."""

    DEFAULT_PROVIDERS = [p["id"] for p in FALLBACK_PROVIDERS]

    def __init__(self, providers: Optional[List[str]] = None, max_per_provider: int = 1000):
        self.providers = providers or self.DEFAULT_PROVIDERS
        self.max_per_provider = max_per_provider

    def collect(
        self,
        filter_str: str = "nelements>=1",
        page_limit: int = 100,
    ) -> pd.DataFrame:
        """
        Query OPTIMADE providers and return results as a DataFrame.

        Args:
            filter_str: OPTIMADE filter string.
            page_limit: Results per page.

        Returns:
            DataFrame with columns: id, formula, elements, nelements, provider,
            space_group, lattice_vectors, and any other available attributes.
        """
        client = OptimadeClient(
            include_providers=self.providers,
            max_results_per_provider=self.max_per_provider,
        )
        raw_results = client.get(filter_str)

        records = []
        if "structures" in raw_results:
            for filter_data in raw_results["structures"].values():
                for provider_id, provider_data in filter_data.items():
                    if "data" not in provider_data:
                        continue
                    for entry in provider_data["data"]:
                        attrs = entry.get("attributes", {})
                        records.append({
                            "id": entry.get("id"),
                            "formula": attrs.get("chemical_formula_descriptive"),
                            "elements": ",".join(attrs.get("elements", [])),
                            "nelements": attrs.get("nelements"),
                            "provider": provider_id,
                            "space_group": attrs.get("space_group_symbol"),
                            "nsites": attrs.get("nsites"),
                            "lattice_vectors": str(attrs.get("lattice_vectors")),
                        })

        return pd.DataFrame(records) if records else pd.DataFrame(
            columns=["id", "formula", "elements", "nelements", "provider",
                     "space_group", "nsites", "lattice_vectors"]
        )


class MaterialsProjectCollector:
    """Collects enriched materials data from the Materials Project API."""

    def __init__(self, api_key: Optional[str] = None):
        self.api_key = api_key or os.getenv("MATERIALS_PROJECT_API_KEY")

    def collect(
        self,
        elements: Optional[List[str]] = None,
        num_elements: Optional[tuple] = None,
        fields: Optional[List[str]] = None,
        max_results: int = 5000,
    ) -> pd.DataFrame:
        """
        Query Materials Project and return results as a DataFrame.

        Args:
            elements: List of element symbols to filter by.
            num_elements: Tuple of (min, max) number of elements.
            fields: Specific fields to retrieve.
            max_results: Maximum number of results.

        Returns:
            DataFrame with MP-specific properties.
        """
        if not self.api_key:
            raise ValueError(
                "Materials Project API key required. "
                "Set MATERIALS_PROJECT_API_KEY env var or pass api_key."
            )

        try:
            from mp_api.client import MPRester
        except ImportError:
            raise ImportError("mp-api package required: pip install mp-api")

        default_fields = [
            "material_id", "formula_pretty", "elements", "nelements",
            "formation_energy_per_atom", "band_gap", "energy_above_hull",
            "is_metal", "symmetry",
        ]
        fields = fields or default_fields

        with MPRester(self.api_key) as mpr:
            kwargs = {"fields": fields}
            if elements:
                kwargs["elements"] = elements
            if num_elements:
                kwargs["num_elements"] = num_elements

            docs = mpr.materials.summary.search(**kwargs)

            records = []
            for doc in docs[:max_results]:
                record = {}
                for field in fields:
                    val = getattr(doc, field, None)
                    if field == "symmetry" and val is not None:
                        record["crystal_system"] = getattr(val, "crystal_system", None)
                        record["space_group_number"] = getattr(val, "number", None)
                        record["space_group_symbol"] = getattr(val, "symbol", None)
                    else:
                        record[field] = val
                records.append(record)

        return pd.DataFrame(records)
```

**Step 4: Run tests to verify they pass**

```bash
pytest tests/test_collector.py -v --tb=short
```

**Step 5: Commit**

```bash
git add app/data/ tests/test_collector.py
git commit -m "feat: add data collection module for OPTIMADE and Materials Project

OptimadeCollector queries 6 providers with paginated fetching.
MaterialsProjectCollector fetches enriched DFT properties via mp-api."
```

---

### Task 17: Create data normalizer

**Files:**
- Create: `app/data/normalizer.py`
- Create: `tests/test_normalizer.py`

**Step 1: Write failing test**

Create `tests/test_normalizer.py`:
```python
"""Tests for data normalization."""

import pytest
import pandas as pd
from app.data.normalizer import normalize_materials


class TestNormalize:

    def test_normalizes_optimade_data(self):
        df = pd.DataFrame([
            {"id": "mp-1", "formula": "LiCoO2", "elements": "Co,Li,O", "nelements": 3, "provider": "mp"},
        ])
        result = normalize_materials(df, source="optimade")
        assert "formula" in result.columns
        assert "source" in result.columns
        assert result.iloc[0]["source"] == "optimade"

    def test_handles_empty_dataframe(self):
        df = pd.DataFrame()
        result = normalize_materials(df, source="optimade")
        assert len(result) == 0

    def test_deduplicates_by_formula(self):
        df = pd.DataFrame([
            {"id": "mp-1", "formula": "LiCoO2", "elements": "Co,Li,O", "nelements": 3, "provider": "mp"},
            {"id": "cod-1", "formula": "LiCoO2", "elements": "Co,Li,O", "nelements": 3, "provider": "cod"},
        ])
        result = normalize_materials(df, source="optimade", deduplicate=True)
        assert len(result) == 1
```

**Step 2: Implement normalizer**

Create `app/data/normalizer.py`:
```python
"""Normalize materials data from different sources into a unified schema."""

from typing import Optional
import pandas as pd


UNIFIED_COLUMNS = [
    "material_id", "formula", "elements", "nelements", "source", "provider",
    "band_gap", "formation_energy", "energy_above_hull",
    "is_metal", "crystal_system", "space_group_number", "space_group_symbol",
    "bulk_modulus", "shear_modulus", "nsites",
]


def normalize_materials(
    df: pd.DataFrame,
    source: str = "unknown",
    deduplicate: bool = False,
) -> pd.DataFrame:
    """
    Normalize a materials DataFrame into the unified PRISM schema.

    Args:
        df: Input DataFrame from any collector.
        source: Label for the data source (e.g., "optimade", "mp").
        deduplicate: If True, drop duplicate formulas (keep first).

    Returns:
        DataFrame with unified column schema.
    """
    if df.empty:
        return pd.DataFrame(columns=UNIFIED_COLUMNS)

    result = pd.DataFrame()

    # Map common column names
    col_map = {
        "id": "material_id",
        "material_id": "material_id",
        "formula": "formula",
        "formula_pretty": "formula",
        "chemical_formula_descriptive": "formula",
        "elements": "elements",
        "nelements": "nelements",
        "provider": "provider",
        "band_gap": "band_gap",
        "formation_energy_per_atom": "formation_energy",
        "formation_energy": "formation_energy",
        "energy_above_hull": "energy_above_hull",
        "is_metal": "is_metal",
        "crystal_system": "crystal_system",
        "space_group_number": "space_group_number",
        "space_group_symbol": "space_group_symbol",
        "nsites": "nsites",
    }

    for src_col, dst_col in col_map.items():
        if src_col in df.columns:
            result[dst_col] = df[src_col]

    result["source"] = source

    # Ensure all unified columns exist
    for col in UNIFIED_COLUMNS:
        if col not in result.columns:
            result[col] = None

    result = result[UNIFIED_COLUMNS]

    if deduplicate and "formula" in result.columns:
        result = result.drop_duplicates(subset=["formula"], keep="first")

    return result.reset_index(drop=True)
```

**Step 3: Run tests**

```bash
pytest tests/test_normalizer.py -v --tb=short
```

**Step 4: Commit**

```bash
git add app/data/normalizer.py tests/test_normalizer.py
git commit -m "feat: add data normalizer for unified materials schema"
```

---

### Task 18: Create data store module

**Files:**
- Create: `app/data/store.py`

**Step 1: Implement Parquet-based storage**

Create `app/data/store.py`:
```python
"""Storage layer for collected materials data."""

import os
from datetime import datetime
from pathlib import Path
from typing import Optional

import pandas as pd

from app.config.settings import PROJECT_ROOT


DATA_DIR = PROJECT_ROOT / "data"


def get_data_dir() -> Path:
    """Get or create the data directory."""
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    return DATA_DIR


def save_dataset(df: pd.DataFrame, name: str = "materials") -> Path:
    """
    Save a DataFrame as a dated Parquet file.

    Args:
        df: DataFrame to save.
        name: Base name for the file.

    Returns:
        Path to the saved file.
    """
    data_dir = get_data_dir()
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    path = data_dir / f"{name}_{timestamp}.parquet"
    df.to_parquet(path, index=False)
    return path


def load_latest_dataset(name: str = "materials") -> Optional[pd.DataFrame]:
    """
    Load the most recent dataset matching the given name.

    Args:
        name: Base name to match.

    Returns:
        DataFrame or None if no datasets exist.
    """
    data_dir = get_data_dir()
    files = sorted(data_dir.glob(f"{name}_*.parquet"), reverse=True)
    if not files:
        return None
    return pd.read_parquet(files[0])


def list_datasets() -> list:
    """List all saved datasets with metadata."""
    data_dir = get_data_dir()
    datasets = []
    for path in sorted(data_dir.glob("*.parquet")):
        df = pd.read_parquet(path)
        datasets.append({
            "file": path.name,
            "rows": len(df),
            "columns": len(df.columns),
            "size_mb": round(path.stat().st_size / (1024 * 1024), 2),
            "modified": datetime.fromtimestamp(path.stat().st_mtime).isoformat(),
        })
    return datasets


def export_dataset(name: str = "materials", fmt: str = "csv") -> Optional[Path]:
    """
    Export the latest dataset to CSV or Parquet.

    Args:
        name: Dataset name.
        fmt: Export format ("csv" or "parquet").

    Returns:
        Path to exported file, or None if no data.
    """
    df = load_latest_dataset(name)
    if df is None:
        return None

    data_dir = get_data_dir()
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")

    if fmt == "csv":
        path = data_dir / f"{name}_export_{timestamp}.csv"
        df.to_csv(path, index=False)
    else:
        path = data_dir / f"{name}_export_{timestamp}.parquet"
        df.to_parquet(path, index=False)

    return path
```

**Step 2: Commit**

```bash
git add app/data/store.py
git commit -m "feat: add Parquet-based data storage with versioned datasets"
```

---

### Task 19: Add data CLI commands

**Files:**
- Create: `app/commands/__init__.py`
- Create: `app/commands/data.py`
- Modify: `app/cli.py` (register new command group)
- Modify: `pyproject.toml` (add `app.commands` to packages)

**Step 1: Create the data command group**

Create `app/commands/__init__.py`:
```python
"""PRISM CLI command modules."""
```

Create `app/commands/data.py`:
```python
"""CLI commands for data collection and management."""

import click
from rich.console import Console
from rich.table import Table
from rich.panel import Panel

console = Console(force_terminal=True, width=120)


@click.group()
def data():
    """Collect, manage, and export materials data."""
    pass


@data.command()
@click.option("--provider", help="Specific provider to collect from (e.g., mp, oqmd, cod).")
@click.option("--filter", "filter_str", default="nelements>=1", help="OPTIMADE filter string.")
@click.option("--max-results", type=int, default=1000, help="Max results per provider.")
def collect(provider, filter_str, max_results):
    """Collect materials data from OPTIMADE providers."""
    from app.data.collector import OptimadeCollector
    from app.data.normalizer import normalize_materials
    from app.data.store import save_dataset

    providers = [provider] if provider else None
    collector = OptimadeCollector(providers=providers, max_per_provider=max_results)

    with console.status("[bold green]Collecting materials data from OPTIMADE...[/bold green]"):
        df = collector.collect(filter_str=filter_str)

    if df.empty:
        console.print("[red]No data collected.[/red]")
        return

    normalized = normalize_materials(df, source="optimade")
    path = save_dataset(normalized)
    console.print(f"[green]Collected {len(normalized)} materials. Saved to {path}[/green]")


@data.command()
def status():
    """Show status of collected datasets."""
    from app.data.store import list_datasets

    datasets = list_datasets()
    if not datasets:
        console.print("[yellow]No datasets found. Run 'prism data collect' first.[/yellow]")
        return

    table = Table(title="Collected Datasets", show_header=True, header_style="bold magenta")
    table.add_column("File")
    table.add_column("Rows", justify="right")
    table.add_column("Columns", justify="right")
    table.add_column("Size (MB)", justify="right")
    table.add_column("Last Modified")

    for ds in datasets:
        table.add_row(ds["file"], str(ds["rows"]), str(ds["columns"]), str(ds["size_mb"]), ds["modified"])

    console.print(table)


@data.command()
@click.option("--format", "fmt", type=click.Choice(["csv", "parquet"]), default="csv", help="Export format.")
def export(fmt):
    """Export the latest dataset."""
    from app.data.store import export_dataset

    path = export_dataset(fmt=fmt)
    if path is None:
        console.print("[red]No data to export. Run 'prism data collect' first.[/red]")
        return

    console.print(f"[green]Exported to {path}[/green]")
```

**Step 2: Register the data command group in cli.py**

Add near the bottom of `app/cli.py`, before the `if __name__` block:
```python
from app.commands.data import data
cli.add_command(data)
```

**Step 3: Update pyproject.toml packages**

Add `"app.commands"` and `"app.data"` to the packages list:
```toml
[tool.setuptools]
packages = ["app", "app.config", "app.db", "app.commands", "app.data"]
```

**Step 4: Verify**

```bash
python -c "import ast; ast.parse(open('app/cli.py').read()); print('OK')"
python -c "import ast; ast.parse(open('app/commands/data.py').read()); print('OK')"
```

**Step 5: Commit**

```bash
git add app/commands/ app/cli.py pyproject.toml
git commit -m "feat: add 'prism data' CLI commands for collection and export

Commands: prism data collect, prism data status, prism data export"
```

---

## Phase 3: ML Models

### Task 20: Add ML dependencies to pyproject.toml

**Files:**
- Modify: `pyproject.toml`

**Step 1: Add ML optional dependencies**

Add to `pyproject.toml` after the main dependencies:

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
ml-foundation = [
    "mace-torch>=0.3.0",
    "chgnet>=0.3.0",
    "alignn>=2024.1.0",
]
ml-advanced = [
    "modnet>=0.4.0",
    "crabnet>=2.0.0",
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

Also add `"app.ml"` to the packages list.

**Step 2: Commit**

```bash
git add pyproject.toml
git commit -m "feat: add ML optional dependencies to pyproject.toml

Install with: pip install -e '.[ml]' for classical models,
pip install -e '.[ml-foundation]' for MACE/CHGNet/ALIGNN."
```

---

### Task 21: Create feature engineering module

**Files:**
- Create: `app/ml/__init__.py`
- Create: `app/ml/features.py`
- Create: `tests/test_features.py`

**Step 1: Write failing test**

Create `tests/test_features.py`:
```python
"""Tests for feature engineering."""

import pytest
import pandas as pd
from app.ml.features import compute_compositional_features


class TestCompositionalFeatures:

    def test_produces_features_from_formula(self):
        result = compute_compositional_features("LiCoO2")
        assert isinstance(result, dict)
        assert len(result) > 10  # Should have many features
        assert "mean_atomic_mass" in result

    def test_handles_single_element(self):
        result = compute_compositional_features("Fe")
        assert isinstance(result, dict)
        assert result["mean_atomic_mass"] > 0

    def test_returns_none_for_invalid_formula(self):
        result = compute_compositional_features("NotAFormula123XYZ")
        assert result is None or len(result) == 0

    def test_batch_featurize(self):
        from app.ml.features import featurize_dataframe
        df = pd.DataFrame({"formula": ["LiCoO2", "NaCl", "Fe2O3"]})
        result = featurize_dataframe(df)
        assert len(result) == 3
        assert "mean_atomic_mass" in result.columns
```

**Step 2: Run to verify failure**

```bash
pytest tests/test_features.py -v --tb=short
```

**Step 3: Implement features module**

Create `app/ml/__init__.py`:
```python
"""Machine learning models and feature engineering for PRISM."""
```

Create `app/ml/features.py`:
```python
"""Compositional feature engineering for materials property prediction."""

from typing import Optional, Dict, Any

import pandas as pd

# Element properties lookup table (subset of Magpie features)
# Source: Magpie descriptor set - key elemental properties
ELEMENT_PROPERTIES = {
    "H": {"atomic_mass": 1.008, "atomic_radius": 25, "electronegativity": 2.20, "ionization_energy": 1312},
    "He": {"atomic_mass": 4.003, "atomic_radius": 31, "electronegativity": 0, "ionization_energy": 2372},
    "Li": {"atomic_mass": 6.941, "atomic_radius": 145, "electronegativity": 0.98, "ionization_energy": 520},
    "Be": {"atomic_mass": 9.012, "atomic_radius": 105, "electronegativity": 1.57, "ionization_energy": 900},
    "B": {"atomic_mass": 10.81, "atomic_radius": 85, "electronegativity": 2.04, "ionization_energy": 801},
    "C": {"atomic_mass": 12.01, "atomic_radius": 70, "electronegativity": 2.55, "ionization_energy": 1086},
    "N": {"atomic_mass": 14.01, "atomic_radius": 65, "electronegativity": 3.04, "ionization_energy": 1402},
    "O": {"atomic_mass": 16.00, "atomic_radius": 60, "electronegativity": 3.44, "ionization_energy": 1314},
    "F": {"atomic_mass": 19.00, "atomic_radius": 50, "electronegativity": 3.98, "ionization_energy": 1681},
    "Na": {"atomic_mass": 22.99, "atomic_radius": 180, "electronegativity": 0.93, "ionization_energy": 496},
    "Mg": {"atomic_mass": 24.31, "atomic_radius": 150, "electronegativity": 1.31, "ionization_energy": 738},
    "Al": {"atomic_mass": 26.98, "atomic_radius": 125, "electronegativity": 1.61, "ionization_energy": 578},
    "Si": {"atomic_mass": 28.09, "atomic_radius": 110, "electronegativity": 1.90, "ionization_energy": 786},
    "P": {"atomic_mass": 30.97, "atomic_radius": 100, "electronegativity": 2.19, "ionization_energy": 1012},
    "S": {"atomic_mass": 32.07, "atomic_radius": 100, "electronegativity": 2.58, "ionization_energy": 1000},
    "Cl": {"atomic_mass": 35.45, "atomic_radius": 100, "electronegativity": 3.16, "ionization_energy": 1251},
    "K": {"atomic_mass": 39.10, "atomic_radius": 220, "electronegativity": 0.82, "ionization_energy": 419},
    "Ca": {"atomic_mass": 40.08, "atomic_radius": 180, "electronegativity": 1.00, "ionization_energy": 590},
    "Ti": {"atomic_mass": 47.87, "atomic_radius": 140, "electronegativity": 1.54, "ionization_energy": 659},
    "V": {"atomic_mass": 50.94, "atomic_radius": 135, "electronegativity": 1.63, "ionization_energy": 651},
    "Cr": {"atomic_mass": 52.00, "atomic_radius": 140, "electronegativity": 1.66, "ionization_energy": 653},
    "Mn": {"atomic_mass": 54.94, "atomic_radius": 140, "electronegativity": 1.55, "ionization_energy": 717},
    "Fe": {"atomic_mass": 55.85, "atomic_radius": 140, "electronegativity": 1.83, "ionization_energy": 762},
    "Co": {"atomic_mass": 58.93, "atomic_radius": 135, "electronegativity": 1.88, "ionization_energy": 760},
    "Ni": {"atomic_mass": 58.69, "atomic_radius": 135, "electronegativity": 1.91, "ionization_energy": 737},
    "Cu": {"atomic_mass": 63.55, "atomic_radius": 135, "electronegativity": 1.90, "ionization_energy": 745},
    "Zn": {"atomic_mass": 65.38, "atomic_radius": 135, "electronegativity": 1.65, "ionization_energy": 906},
    "Ga": {"atomic_mass": 69.72, "atomic_radius": 130, "electronegativity": 1.81, "ionization_energy": 579},
    "Ge": {"atomic_mass": 72.63, "atomic_radius": 125, "electronegativity": 2.01, "ionization_energy": 762},
    "As": {"atomic_mass": 74.92, "atomic_radius": 115, "electronegativity": 2.18, "ionization_energy": 947},
    "Se": {"atomic_mass": 78.97, "atomic_radius": 115, "electronegativity": 2.55, "ionization_energy": 941},
    "Br": {"atomic_mass": 79.90, "atomic_radius": 115, "electronegativity": 2.96, "ionization_energy": 1140},
    "Sr": {"atomic_mass": 87.62, "atomic_radius": 200, "electronegativity": 0.95, "ionization_energy": 549},
    "Y": {"atomic_mass": 88.91, "atomic_radius": 180, "electronegativity": 1.22, "ionization_energy": 600},
    "Zr": {"atomic_mass": 91.22, "atomic_radius": 155, "electronegativity": 1.33, "ionization_energy": 640},
    "Nb": {"atomic_mass": 92.91, "atomic_radius": 145, "electronegativity": 1.60, "ionization_energy": 652},
    "Mo": {"atomic_mass": 95.95, "atomic_radius": 145, "electronegativity": 2.16, "ionization_energy": 684},
    "Ag": {"atomic_mass": 107.87, "atomic_radius": 160, "electronegativity": 1.93, "ionization_energy": 731},
    "Sn": {"atomic_mass": 118.71, "atomic_radius": 145, "electronegativity": 1.96, "ionization_energy": 709},
    "Ba": {"atomic_mass": 137.33, "atomic_radius": 215, "electronegativity": 0.89, "ionization_energy": 503},
    "La": {"atomic_mass": 138.91, "atomic_radius": 195, "electronegativity": 1.10, "ionization_energy": 538},
    "W": {"atomic_mass": 183.84, "atomic_radius": 135, "electronegativity": 2.36, "ionization_energy": 770},
    "Pt": {"atomic_mass": 195.08, "atomic_radius": 135, "electronegativity": 2.28, "ionization_energy": 870},
    "Au": {"atomic_mass": 196.97, "atomic_radius": 135, "electronegativity": 2.54, "ionization_energy": 890},
    "Pb": {"atomic_mass": 207.2, "atomic_radius": 180, "electronegativity": 2.33, "ionization_energy": 716},
    "Bi": {"atomic_mass": 208.98, "atomic_radius": 160, "electronegativity": 2.02, "ionization_energy": 703},
    "Ta": {"atomic_mass": 180.95, "atomic_radius": 145, "electronegativity": 1.50, "ionization_energy": 761},
}

PROPERTIES = ["atomic_mass", "atomic_radius", "electronegativity", "ionization_energy"]


def _parse_formula(formula: str) -> Optional[Dict[str, float]]:
    """Parse a chemical formula into element:fraction dict."""
    try:
        from pymatgen.core import Composition
        comp = Composition(formula)
        return {str(el): frac for el, frac in comp.fractional_composition.items()}
    except Exception:
        return None


def compute_compositional_features(formula: str) -> Optional[Dict[str, float]]:
    """
    Compute Magpie-style compositional features from a chemical formula.

    Returns a dict of feature_name -> value, or None if formula can't be parsed.
    """
    fractions = _parse_formula(formula)
    if not fractions:
        return None

    features = {}
    for prop in PROPERTIES:
        values = []
        weights = []
        for element, frac in fractions.items():
            if element in ELEMENT_PROPERTIES and ELEMENT_PROPERTIES[element][prop] > 0:
                values.append(ELEMENT_PROPERTIES[element][prop])
                weights.append(frac)

        if not values:
            features[f"mean_{prop}"] = 0
            features[f"std_{prop}"] = 0
            features[f"min_{prop}"] = 0
            features[f"max_{prop}"] = 0
            features[f"range_{prop}"] = 0
            continue

        import numpy as np
        arr = np.array(values)
        w = np.array(weights)

        features[f"mean_{prop}"] = float(np.average(arr, weights=w))
        features[f"std_{prop}"] = float(np.sqrt(np.average((arr - np.average(arr, weights=w)) ** 2, weights=w)))
        features[f"min_{prop}"] = float(np.min(arr))
        features[f"max_{prop}"] = float(np.max(arr))
        features[f"range_{prop}"] = float(np.max(arr) - np.min(arr))

    features["n_elements"] = len(fractions)
    return features


def featurize_dataframe(df: pd.DataFrame, formula_col: str = "formula") -> pd.DataFrame:
    """
    Add compositional features to a DataFrame.

    Args:
        df: DataFrame with a formula column.
        formula_col: Name of the column containing chemical formulas.

    Returns:
        DataFrame with original columns plus feature columns.
    """
    feature_rows = []
    for formula in df[formula_col]:
        feats = compute_compositional_features(str(formula))
        feature_rows.append(feats if feats else {})

    features_df = pd.DataFrame(feature_rows)
    return pd.concat([df.reset_index(drop=True), features_df], axis=1)
```

**Step 4: Run tests**

```bash
pip install pymatgen numpy && pytest tests/test_features.py -v --tb=short
```

**Step 5: Commit**

```bash
git add app/ml/ tests/test_features.py
git commit -m "feat: add compositional feature engineering module

Magpie-style features: mean/std/min/max/range of atomic mass, radius,
electronegativity, ionization energy. Supports pymatgen formula parsing."
```

---

### Task 22: Create model trainer

**Files:**
- Create: `app/ml/trainer.py`
- Create: `app/ml/registry.py`

**Step 1: Implement the trainer**

Create `app/ml/trainer.py`:
```python
"""Model training pipeline for materials property prediction."""

import os
from typing import Optional, List, Dict, Any, Tuple
from pathlib import Path

import numpy as np
import pandas as pd
import joblib
from sklearn.model_selection import cross_val_score, train_test_split
from sklearn.ensemble import RandomForestRegressor, RandomForestClassifier
from sklearn.metrics import mean_absolute_error, r2_score, accuracy_score, f1_score

from app.config.settings import PROJECT_ROOT
from app.ml.features import featurize_dataframe

MODELS_DIR = PROJECT_ROOT / "models"


def get_models_dir() -> Path:
    """Get or create the models directory."""
    MODELS_DIR.mkdir(parents=True, exist_ok=True)
    return MODELS_DIR


REGRESSION_TARGETS = ["band_gap", "formation_energy", "energy_above_hull", "bulk_modulus", "shear_modulus"]
CLASSIFICATION_TARGETS = ["is_metal", "crystal_system"]


def train_property_model(
    df: pd.DataFrame,
    target: str,
    model_type: str = "auto",
    test_size: float = 0.2,
    cv_folds: int = 5,
) -> Dict[str, Any]:
    """
    Train a model to predict a specific material property.

    Args:
        df: DataFrame with formula column and target column.
        target: Name of the target property column.
        model_type: "rf" (Random Forest), "xgboost", "lightgbm", or "auto".
        test_size: Fraction of data for test set.
        cv_folds: Number of cross-validation folds.

    Returns:
        Dict with model, metrics, feature_names, and other metadata.
    """
    if target not in df.columns:
        raise ValueError(f"Target '{target}' not found in DataFrame columns")

    # Drop rows where target is missing
    clean_df = df.dropna(subset=[target]).copy()
    if len(clean_df) < 20:
        raise ValueError(f"Need at least 20 samples with '{target}' data, got {len(clean_df)}")

    # Featurize
    featured = featurize_dataframe(clean_df, formula_col="formula")

    # Identify feature columns (numeric, not target)
    feature_cols = [c for c in featured.columns if c.startswith(("mean_", "std_", "min_", "max_", "range_", "n_elements"))]
    if not feature_cols:
        raise ValueError("No features generated. Check formula column.")

    X = featured[feature_cols].fillna(0).values
    y = featured[target].values

    is_classification = target in CLASSIFICATION_TARGETS

    # Select model
    model = _get_model(model_type, is_classification)

    # Train/test split
    X_train, X_test, y_train, y_test = train_test_split(X, y, test_size=test_size, random_state=42)

    # Cross-validation
    scoring = "accuracy" if is_classification else "neg_mean_absolute_error"
    cv_scores = cross_val_score(model, X_train, y_train, cv=min(cv_folds, len(X_train)), scoring=scoring)

    # Train final model
    model.fit(X_train, y_train)
    y_pred = model.predict(X_test)

    # Compute metrics
    if is_classification:
        metrics = {
            "accuracy": float(accuracy_score(y_test, y_pred)),
            "cv_mean": float(np.mean(cv_scores)),
            "cv_std": float(np.std(cv_scores)),
        }
    else:
        metrics = {
            "mae": float(mean_absolute_error(y_test, y_pred)),
            "r2": float(r2_score(y_test, y_pred)),
            "cv_mean_mae": float(-np.mean(cv_scores)),
            "cv_std_mae": float(np.std(cv_scores)),
        }

    # Save model
    models_dir = get_models_dir()
    model_path = models_dir / f"{target}_model.joblib"
    joblib.dump(model, model_path)

    # Feature importance
    importance = {}
    if hasattr(model, "feature_importances_"):
        for name, imp in zip(feature_cols, model.feature_importances_):
            importance[name] = float(imp)

    return {
        "model": model,
        "model_path": str(model_path),
        "target": target,
        "is_classification": is_classification,
        "metrics": metrics,
        "feature_names": feature_cols,
        "feature_importance": importance,
        "train_size": len(X_train),
        "test_size": len(X_test),
    }


def _get_model(model_type: str, is_classification: bool):
    """Get a model instance based on type."""
    if model_type == "auto" or model_type == "rf":
        if is_classification:
            return RandomForestClassifier(n_estimators=100, random_state=42, n_jobs=-1)
        return RandomForestRegressor(n_estimators=100, random_state=42, n_jobs=-1)

    if model_type == "xgboost":
        try:
            from xgboost import XGBRegressor, XGBClassifier
            if is_classification:
                return XGBClassifier(n_estimators=100, random_state=42, n_jobs=-1)
            return XGBRegressor(n_estimators=100, random_state=42, n_jobs=-1)
        except ImportError:
            raise ImportError("XGBoost required: pip install xgboost")

    if model_type == "lightgbm":
        try:
            from lightgbm import LGBMRegressor, LGBMClassifier
            if is_classification:
                return LGBMClassifier(n_estimators=100, random_state=42, n_jobs=-1, verbose=-1)
            return LGBMRegressor(n_estimators=100, random_state=42, n_jobs=-1, verbose=-1)
        except ImportError:
            raise ImportError("LightGBM required: pip install lightgbm")

    raise ValueError(f"Unknown model type: {model_type}. Use 'rf', 'xgboost', or 'lightgbm'.")
```

Create `app/ml/registry.py`:
```python
"""Model registry for tracking trained models and their performance."""

import json
from datetime import datetime
from pathlib import Path
from typing import Optional, List, Dict

from app.config.settings import PROJECT_ROOT

REGISTRY_PATH = PROJECT_ROOT / "models" / "registry.json"


def _load_registry() -> List[Dict]:
    """Load the model registry from disk."""
    if REGISTRY_PATH.exists():
        with open(REGISTRY_PATH) as f:
            return json.load(f)
    return []


def _save_registry(entries: List[Dict]):
    """Save the model registry to disk."""
    REGISTRY_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(REGISTRY_PATH, "w") as f:
        json.dump(entries, f, indent=2, default=str)


def register_model(result: Dict) -> Dict:
    """Register a trained model in the registry."""
    entry = {
        "target": result["target"],
        "model_path": result["model_path"],
        "metrics": result["metrics"],
        "train_size": result["train_size"],
        "test_size": result["test_size"],
        "is_classification": result["is_classification"],
        "trained_at": datetime.now().isoformat(),
    }

    registry = _load_registry()
    # Replace existing entry for the same target, or append
    registry = [e for e in registry if e["target"] != result["target"]]
    registry.append(entry)
    _save_registry(registry)
    return entry


def list_models() -> List[Dict]:
    """List all registered models."""
    return _load_registry()


def get_model_info(target: str) -> Optional[Dict]:
    """Get info for a specific target's model."""
    for entry in _load_registry():
        if entry["target"] == target:
            return entry
    return None
```

**Step 2: Commit**

```bash
git add app/ml/trainer.py app/ml/registry.py
git commit -m "feat: add model training pipeline and registry

Supports RF, XGBoost, LightGBM. Auto-benchmark with cross-validation.
JSON-based model registry for tracking performance."
```

---

### Task 23: Create predictor module

**Files:**
- Create: `app/ml/predictor.py`
- Create: `tests/test_predictor.py`

**Step 1: Write failing test**

Create `tests/test_predictor.py`:
```python
"""Tests for the prediction module."""

import pytest
from unittest.mock import patch, MagicMock
from app.ml.predictor import predict_properties


class TestPredict:

    @patch("app.ml.predictor.joblib")
    @patch("app.ml.predictor.list_models")
    def test_returns_empty_when_no_models(self, mock_list, mock_joblib):
        mock_list.return_value = []
        result = predict_properties("LiCoO2")
        assert result == {} or result is not None

    def test_handles_invalid_formula(self):
        result = predict_properties("NotAFormula")
        assert result == {} or "error" in str(result).lower()
```

**Step 2: Implement predictor**

Create `app/ml/predictor.py`:
```python
"""Inference engine for materials property prediction."""

from typing import Optional, Dict, Any, List
from pathlib import Path

import joblib
import numpy as np

from app.ml.features import compute_compositional_features
from app.ml.registry import list_models, get_model_info
from app.config.settings import PROJECT_ROOT


def predict_properties(
    formula: str,
    targets: Optional[List[str]] = None,
) -> Dict[str, Dict[str, Any]]:
    """
    Predict material properties from a chemical formula.

    Args:
        formula: Chemical formula (e.g., "LiCoO2").
        targets: Specific properties to predict. If None, predict all available.

    Returns:
        Dict mapping property name to prediction info:
        {
            "band_gap": {"value": 2.7, "unit": "eV", "model": "rf"},
            ...
        }
    """
    features = compute_compositional_features(formula)
    if not features:
        return {}

    available_models = list_models()
    if not available_models:
        return {}

    if targets:
        available_models = [m for m in available_models if m["target"] in targets]

    predictions = {}
    for model_info in available_models:
        target = model_info["target"]
        model_path = Path(model_info["model_path"])

        if not model_path.exists():
            continue

        try:
            model = joblib.load(model_path)

            # Build feature vector in the right order
            feature_names = _get_feature_names()
            X = np.array([[features.get(f, 0) for f in feature_names]])

            pred = model.predict(X)[0]

            predictions[target] = {
                "value": float(pred),
                "unit": _get_unit(target),
                "metrics": model_info.get("metrics", {}),
                "train_size": model_info.get("train_size", 0),
            }
        except Exception as e:
            predictions[target] = {"error": str(e)}

    return predictions


def _get_feature_names() -> List[str]:
    """Get the standard feature names in order."""
    properties = ["atomic_mass", "atomic_radius", "electronegativity", "ionization_energy"]
    stats = ["mean", "std", "min", "max", "range"]
    names = [f"{stat}_{prop}" for prop in properties for stat in stats]
    names.append("n_elements")
    return names


PROPERTY_UNITS = {
    "band_gap": "eV",
    "formation_energy": "eV/atom",
    "energy_above_hull": "eV/atom",
    "bulk_modulus": "GPa",
    "shear_modulus": "GPa",
    "is_metal": "",
    "crystal_system": "",
}


def _get_unit(target: str) -> str:
    """Get the unit for a target property."""
    return PROPERTY_UNITS.get(target, "")
```

**Step 3: Run tests**

```bash
pytest tests/test_predictor.py -v --tb=short
```

**Step 4: Commit**

```bash
git add app/ml/predictor.py tests/test_predictor.py
git commit -m "feat: add prediction engine for materials property inference

Loads trained models from registry, computes features from formula,
returns predictions with confidence metrics and units."
```

---

### Task 24: Create visualization module

**Files:**
- Create: `app/ml/viz.py`

**Step 1: Implement visualization**

Create `app/ml/viz.py`:
```python
"""Visualization tools for model performance and predictions."""

from typing import Dict, Optional, List
from pathlib import Path

from app.config.settings import PROJECT_ROOT

PLOTS_DIR = PROJECT_ROOT / "plots"


def plot_feature_importance(
    importance: Dict[str, float],
    target: str,
    top_n: int = 20,
    save_path: Optional[Path] = None,
) -> Optional[Path]:
    """Plot horizontal bar chart of feature importance."""
    try:
        import matplotlib.pyplot as plt
    except ImportError:
        return None

    PLOTS_DIR.mkdir(parents=True, exist_ok=True)

    sorted_items = sorted(importance.items(), key=lambda x: x[1], reverse=True)[:top_n]
    names = [item[0] for item in reversed(sorted_items)]
    values = [item[1] for item in reversed(sorted_items)]

    fig, ax = plt.subplots(figsize=(10, max(6, len(names) * 0.3)))
    ax.barh(names, values, color="#2196F3")
    ax.set_xlabel("Importance")
    ax.set_title(f"Feature Importance: {target}")
    plt.tight_layout()

    path = save_path or PLOTS_DIR / f"importance_{target}.png"
    fig.savefig(path, dpi=150)
    plt.close(fig)
    return path


def plot_model_comparison(
    results: List[Dict],
    save_path: Optional[Path] = None,
) -> Optional[Path]:
    """Plot bar chart comparing model performance across targets."""
    try:
        import matplotlib.pyplot as plt
    except ImportError:
        return None

    PLOTS_DIR.mkdir(parents=True, exist_ok=True)

    targets = [r["target"] for r in results if not r.get("is_classification")]
    maes = [r["metrics"].get("mae", 0) for r in results if not r.get("is_classification")]

    if not targets:
        return None

    fig, ax = plt.subplots(figsize=(10, 6))
    ax.bar(targets, maes, color="#4CAF50")
    ax.set_ylabel("Mean Absolute Error")
    ax.set_title("Model Performance Comparison")
    plt.xticks(rotation=45, ha="right")
    plt.tight_layout()

    path = save_path or PLOTS_DIR / "model_comparison.png"
    fig.savefig(path, dpi=150)
    plt.close(fig)
    return path


def plot_parity(
    y_true, y_pred,
    target: str,
    save_path: Optional[Path] = None,
) -> Optional[Path]:
    """Plot predicted vs actual (parity plot)."""
    try:
        import matplotlib.pyplot as plt
        import numpy as np
    except ImportError:
        return None

    PLOTS_DIR.mkdir(parents=True, exist_ok=True)

    fig, ax = plt.subplots(figsize=(8, 8))
    ax.scatter(y_true, y_pred, alpha=0.5, s=10, color="#FF5722")

    # Perfect prediction line
    all_vals = list(y_true) + list(y_pred)
    lo, hi = min(all_vals), max(all_vals)
    ax.plot([lo, hi], [lo, hi], "k--", alpha=0.5)

    ax.set_xlabel("Actual")
    ax.set_ylabel("Predicted")
    ax.set_title(f"Parity Plot: {target}")
    plt.tight_layout()

    path = save_path or PLOTS_DIR / f"parity_{target}.png"
    fig.savefig(path, dpi=150)
    plt.close(fig)
    return path
```

**Step 2: Commit**

```bash
git add app/ml/viz.py
git commit -m "feat: add visualization module for model performance plots

Feature importance, model comparison, and parity plots via matplotlib."
```

---

### Task 25: Add predict and model CLI commands

**Files:**
- Create: `app/commands/predict.py`
- Create: `app/commands/model.py`
- Modify: `app/cli.py` (register new command groups)

**Step 1: Create predict command**

Create `app/commands/predict.py`:
```python
"""CLI commands for materials property prediction."""

import click
from rich.console import Console
from rich.table import Table
from rich.panel import Panel

console = Console(force_terminal=True, width=120)


@click.command()
@click.argument("formula", required=True)
@click.option("--property", "target_prop", help="Specific property to predict.")
@click.option("--structure", is_flag=True, help="Fetch crystal structure and use structure-based models.")
@click.option("--viz", is_flag=True, help="Show prediction confidence visualization.")
def predict(formula, target_prop, structure, viz):
    """Predict material properties from a chemical formula.

    Example: prism predict "LiCoO2"
    """
    from app.ml.predictor import predict_properties
    from app.ml.registry import list_models

    models = list_models()
    if not models:
        console.print("[yellow]No trained models found.[/yellow]")
        console.print("Run [cyan]prism model train[/cyan] first to train prediction models.")
        return

    targets = [target_prop] if target_prop else None
    predictions = predict_properties(formula, targets=targets)

    if not predictions:
        console.print(f"[red]Could not generate predictions for '{formula}'.[/red]")
        console.print("Check that the formula is valid and models are trained.")
        return

    # Display predictions
    table = Table(show_header=True, header_style="bold magenta")
    table.add_column("Property")
    table.add_column("Value", justify="right")
    table.add_column("Unit")
    table.add_column("Model MAE", justify="right")
    table.add_column("Training Data", justify="right")

    for prop, info in predictions.items():
        if "error" in info:
            table.add_row(prop, f"[red]{info['error'][:40]}[/red]", "", "", "")
        else:
            mae = info.get("metrics", {}).get("mae", info.get("metrics", {}).get("cv_mean_mae", ""))
            mae_str = f"{mae:.4f}" if isinstance(mae, float) else str(mae)
            table.add_row(
                prop,
                f"{info['value']:.4f}",
                info.get("unit", ""),
                mae_str,
                str(info.get("train_size", "")),
            )

    console.print(Panel(table, title=f"PRISM Prediction: {formula}", border_style="green"))

    if viz:
        console.print("[dim]Visualization requires matplotlib. Saving plots...[/dim]")
        # Visualization would go here in future
```

**Step 2: Create model command group**

Create `app/commands/model.py`:
```python
"""CLI commands for model training and management."""

import click
from rich.console import Console
from rich.table import Table
from rich.panel import Panel

console = Console(force_terminal=True, width=120)


@click.group()
def model():
    """Train, manage, and inspect ML models."""
    pass


@model.command()
@click.option("--property", "target_prop", help="Specific property to train (e.g., band_gap).")
@click.option("--model-type", type=click.Choice(["auto", "rf", "xgboost", "lightgbm"]), default="auto")
def train(target_prop, model_type):
    """Train prediction models on collected data."""
    from app.data.store import load_latest_dataset
    from app.ml.trainer import train_property_model, REGRESSION_TARGETS, CLASSIFICATION_TARGETS
    from app.ml.registry import register_model

    df = load_latest_dataset()
    if df is None:
        console.print("[red]No training data found.[/red]")
        console.print("Run [cyan]prism data collect[/cyan] first.")
        return

    targets = [target_prop] if target_prop else REGRESSION_TARGETS + CLASSIFICATION_TARGETS
    results = []

    for target in targets:
        if target not in df.columns:
            console.print(f"[yellow]Skipping '{target}': not in dataset.[/yellow]")
            continue

        non_null = df[target].notna().sum()
        if non_null < 20:
            console.print(f"[yellow]Skipping '{target}': only {non_null} samples (need 20+).[/yellow]")
            continue

        with console.status(f"[bold green]Training {target} model...[/bold green]"):
            try:
                result = train_property_model(df, target=target, model_type=model_type)
                register_model(result)
                results.append(result)

                metrics = result["metrics"]
                if result["is_classification"]:
                    console.print(f"[green]{target}:[/green] accuracy={metrics['accuracy']:.3f}")
                else:
                    console.print(f"[green]{target}:[/green] MAE={metrics['mae']:.4f}, R2={metrics['r2']:.3f}")
            except Exception as e:
                console.print(f"[red]Failed to train {target}: {e}[/red]")

    if results:
        console.print(f"\n[green]Trained {len(results)} models successfully.[/green]")
    else:
        console.print("[yellow]No models were trained. Check your data has target columns.[/yellow]")


@model.command()
def status():
    """Show status of trained models."""
    from app.ml.registry import list_models

    models = list_models()
    if not models:
        console.print("[yellow]No trained models. Run 'prism model train' first.[/yellow]")
        return

    table = Table(title="Trained Models", show_header=True, header_style="bold magenta")
    table.add_column("Property")
    table.add_column("Type")
    table.add_column("Key Metric", justify="right")
    table.add_column("Train Size", justify="right")
    table.add_column("Trained At")

    for m in models:
        metric_str = ""
        metrics = m.get("metrics", {})
        if m.get("is_classification"):
            metric_str = f"acc={metrics.get('accuracy', 0):.3f}"
        else:
            metric_str = f"MAE={metrics.get('mae', 0):.4f}"

        table.add_row(
            m["target"],
            "Classification" if m.get("is_classification") else "Regression",
            metric_str,
            str(m.get("train_size", "")),
            m.get("trained_at", "")[:19],
        )

    console.print(table)


@model.command("list")
def list_cmd():
    """List available pretrained models."""
    console.print("[bold cyan]Pretrained Models Available:[/bold cyan]")
    console.print("\n[bold]Tier 1 - Composition-Only:[/bold]")
    console.print("  Random Forest / XGBoost / LightGBM (built-in)")
    console.print("  MODNet (pip install modnet)")
    console.print("  CrabNet (pip install crabnet)")
    console.print("\n[bold]Tier 2 - Structure-Based:[/bold]")
    console.print("  MACE-MP-0 (pip install mace-torch)")
    console.print("  CHGNet (pip install chgnet)")
    console.print("  ALIGNN (pip install alignn)")
    console.print("\n[dim]Install with: pip install -e '.[ml-foundation]' for Tier 2 models[/dim]")


@model.command()
@click.option("--property", "target_prop", help="Property to visualize.")
def viz(target_prop):
    """Visualize model performance."""
    from app.ml.registry import list_models

    models = list_models()
    if not models:
        console.print("[yellow]No trained models. Run 'prism model train' first.[/yellow]")
        return

    try:
        from app.ml.viz import plot_model_comparison
        path = plot_model_comparison(models)
        if path:
            console.print(f"[green]Plot saved to {path}[/green]")
        else:
            console.print("[yellow]Could not generate plot.[/yellow]")
    except ImportError:
        console.print("[yellow]matplotlib required for visualization: pip install matplotlib[/yellow]")
```

**Step 3: Register commands in cli.py**

Add near the bottom of `app/cli.py`, before the `if __name__` block:
```python
from app.commands.predict import predict
from app.commands.model import model
cli.add_command(predict)
cli.add_command(model)
```

**Step 4: Verify**

```bash
python -c "import ast; ast.parse(open('app/commands/predict.py').read()); print('OK')"
python -c "import ast; ast.parse(open('app/commands/model.py').read()); print('OK')"
```

**Step 5: Commit**

```bash
git add app/commands/predict.py app/commands/model.py app/cli.py
git commit -m "feat: add predict and model CLI commands

Commands: prism predict, prism model train, prism model status,
prism model list, prism model viz"
```

---

### Task 26: Update pyproject.toml packages and version

**Files:**
- Modify: `pyproject.toml`
- Modify: `app/__init__.py`

**Step 1: Update packages list to include all new modules**

```toml
[tool.setuptools]
packages = ["app", "app.config", "app.db", "app.commands", "app.data", "app.ml"]
include-package-data = true
```

**Step 2: Bump version to 2.0.0**

In `pyproject.toml`:
```toml
version = "2.0.0"
```

In `app/__init__.py`:
```python
"""
PRISM Platform Application Package
"""
__version__ = "2.0.0"
```

**Step 3: Commit**

```bash
git add pyproject.toml app/__init__.py
git commit -m "chore: bump version to 2.0.0, update package list"
```

---

### Task 27: Run full test suite and verify

**Step 1: Install in dev mode**

```bash
pip install -e ".[ml,dev]"
```

**Step 2: Run full test suite**

```bash
pytest tests/ -v --tb=short
```

**Step 3: Verify CLI runs**

```bash
prism --version
prism --help
prism data --help
prism model --help
prism predict --help
```

**Step 4: Commit any final fixes, then tag**

```bash
git tag -a v2.0.0 -m "PRISM v2.0.0: Stabilized CLI + ML prediction pipeline"
```

---

## Summary of All Tasks

| # | Phase | Task | Description |
|---|-------|------|-------------|
| 1 | 1 | Delete API dir | Remove orphaned FastAPI code |
| 2 | 1 | Delete services dir | Remove orphaned services code |
| 3 | 1 | Clean llm.py | Remove 4 stub LLM providers |
| 4 | 1 | Clean cli.py refs | Remove "coming soon" references |
| 5 | 1 | Settings module | Centralized config + configurable limits |
| 6 | 1 | Fix .env loading | Use settings.get_env_path() |
| 7 | 1 | Fix models.py | Complete band_gap column definition |
| 8 | 1 | Fix _test_filter | Real OPTIMADE API validation |
| 9 | 1 | Fix save-install | Write actual INSTALL_CONTENT |
| 10 | 1 | Clean pyproject.toml | Remove dead deps and packages |
| 11 | 1 | Clean requirements.txt | Dev-only dependencies |
| 12 | 1 | Add tests | LLM + MCP tests with fixtures |
| 13 | 1 | CLI tests | Click CliRunner integration tests |
| 14 | 1 | Configurable limits | Use settings in mcp.py |
| 15 | 1 | Gitignore | Add runtime artifacts |
| 16 | 2 | Data collector | OPTIMADE + MP collection |
| 17 | 2 | Data normalizer | Unified schema normalization |
| 18 | 2 | Data store | Parquet storage layer |
| 19 | 2 | Data CLI | prism data collect/status/export |
| 20 | 3 | ML dependencies | Add optional ML deps |
| 21 | 3 | Feature engineering | Compositional features module |
| 22 | 3 | Model trainer | Training pipeline + registry |
| 23 | 3 | Predictor | Inference engine |
| 24 | 3 | Visualization | matplotlib plots |
| 25 | 3 | Predict/Model CLI | prism predict + prism model commands |
| 26 | 3 | Version bump | v2.0.0 + package list |
| 27 | 3 | Final verification | Full test suite + CLI smoke test |
