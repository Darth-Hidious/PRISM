import os
from abc import ABC, abstractmethod
from openai import OpenAI
import vertexai
from vertexai.generative_models import GenerativeModel
from dotenv import load_dotenv
from anthropic import Anthropic

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
        self.model = model or os.getenv("LLM_MODEL", "gpt-4o")

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
        model_name = model or os.getenv("LLM_MODEL", "gemini-1.5-pro")
        self.model = GenerativeModel(model_name)

    def get_completion(self, prompt: str, stream: bool = False):
        return self.model.generate_content(prompt, stream=stream)

class AnthropicService(LLMService):
    def __init__(self, model: str = None):
        self.client = Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
        self.model = model or os.getenv("LLM_MODEL", "claude-3-5-sonnet-20240620")

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
        self.model = model or os.getenv("LLM_MODEL", "anthropic/claude-3.5-sonnet")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream
        )

# Upcoming LLM providers - coming soon
class PerplexityService(LLMService):
    def __init__(self, model: str = None):
        # Coming soon - Perplexity AI integration
        raise NotImplementedError("Perplexity integration coming soon!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("Perplexity integration coming soon!")

class GrokService(LLMService):
    def __init__(self, model: str = None):
        # Coming soon - Grok (xAI) integration
        raise NotImplementedError("Grok integration coming soon!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("Grok integration coming soon!")

class OllamaService(LLMService):
    def __init__(self, model: str = None):
        # Coming soon - Local Ollama model support
        raise NotImplementedError("Ollama local model support coming soon!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("Ollama local model support coming soon!")

class PRISMCustomService(LLMService):
    def __init__(self, model: str = None):
        # Coming soon - Custom PRISM model trained on materials science literature
        raise NotImplementedError("PRISM Custom Model coming soon - trained on massive materials science corpus!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("PRISM Custom Model coming soon!")

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
        elif os.getenv("PERPLEXITY_API_KEY"):
            provider = "perplexity"
        elif os.getenv("GROK_API_KEY"):
            provider = "grok"
        elif os.getenv("OLLAMA_HOST"):
            provider = "ollama"
        elif os.getenv("PRISM_CUSTOM_API_KEY"):
            provider = "prism_custom"
        else:
            raise ValueError("No LLM provider configured. Please set an API key in the .env file.")

    # Map provider string to service class
    provider_map = {
        "openrouter": OpenRouterService,
        "openai": OpenAIService,
        "anthropic": AnthropicService,
        "vertexai": VertexAIService,
        "perplexity": PerplexityService,
        "grok": GrokService,
        "ollama": OllamaService,
        "prism_custom": PRISMCustomService,
    }

    service_class = provider_map.get(provider.lower())
    
    if not service_class:
        raise ValueError(f"Unsupported LLM provider: {provider}")

    return service_class(model=model)