import os
from abc import ABC, abstractmethod
from openai import OpenAI
import vertexai
from vertexai.generative_models import GenerativeModel
from dotenv import load_dotenv
from anthropic import Anthropic

# Default models per provider (previously in app.agent.models, now deleted)
_DEFAULT_MODELS = {
    "openai": "gpt-4o",
    "anthropic": "claude-sonnet-4-20250514",
    "google": "gemini-2.0-flash",
    "openrouter": "anthropic/claude-sonnet-4-20250514",
}


def _get_default_model(provider: str) -> str:
    return _DEFAULT_MODELS.get(provider, "gpt-4o")

# Load the .env file from the app directory
dotenv_path = os.path.join(os.path.dirname(__file__), '.env')
if os.path.exists(dotenv_path):
    load_dotenv(dotenv_path=dotenv_path)

class LLMService(ABC):
    @abstractmethod
    def get_completion(self, prompt: str, stream: bool = False):
        pass

class OpenAIService(LLMService):
    def __init__(self, model: str = None):
        self.client = OpenAI(api_key=os.getenv("OPENAI_API_KEY"))
        self.model = model or os.getenv("LLM_MODEL") or _get_default_model("openai")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream
        )

class VertexAIService(LLMService):
    def __init__(self, model: str = None):
        project_id = os.getenv("GOOGLE_CLOUD_PROJECT")
        vertexai.init(project=project_id)
        model_name = model or os.getenv("LLM_MODEL") or _get_default_model("google")
        self.model = GenerativeModel(model_name)

    def get_completion(self, prompt: str, stream: bool = False):
        return self.model.generate_content(prompt, stream=stream)

class AnthropicService(LLMService):
    def __init__(self, model: str = None):
        self.client = Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
        self.model = model or os.getenv("LLM_MODEL") or _get_default_model("anthropic")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.messages.create(
            model=self.model,
            max_tokens=512,  # Reduced from 1024 to save tokens
            messages=[
                {"role": "user", "content": prompt}
            ],
            stream=stream
        )

class OpenRouterService(LLMService):
    def __init__(self, model: str = None):
        self.client = OpenAI(
            base_url="https://openrouter.ai/api/v1",
            api_key=os.getenv("OPENROUTER_API_KEY"),
        )
        # Accept any model - OpenRouter supports many models
        self.model = model or os.getenv("LLM_MODEL") or _get_default_model("openrouter")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream
        )

def get_llm_service(provider: str = None, model: str = None) -> LLMService:
    # Determine provider from environment variables if not specified
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
            raise ValueError("No LLM provider configured. Please set an API key in the .env file.")

    # Map provider string to service class
    provider_map = {
        "openrouter": OpenRouterService,
        "openai": OpenAIService,
        "anthropic": AnthropicService,
        "vertexai": VertexAIService,
    }

    service_class = provider_map.get(provider.lower())
    
    if not service_class:
        raise ValueError(f"Unsupported LLM provider: {provider}")

    return service_class(model=model)
