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
    def __init__(self):
        self.client = OpenAI(api_key=os.getenv("OPENAI_API_KEY"))
        self.model = os.getenv("LLM_MODEL", "gpt-4o")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream
        )

class VertexAIService(LLMService):
    def __init__(self):
        project_id = os.getenv("GOOGLE_CLOUD_PROJECT")
        vertexai.init(project=project_id)
        self.model = GenerativeModel(os.getenv("LLM_MODEL", "gemini-2.5-pro"))

    def get_completion(self, prompt: str, stream: bool = False):
        return self.model.generate_content(prompt, stream=stream)

class AnthropicService(LLMService):
    def __init__(self):
        self.client = Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
        self.model = os.getenv("LLM_MODEL", "claude-4-opus")

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
    def __init__(self):
        self.client = OpenAI(
            base_url="https://openrouter.ai/api/v1",
            api_key=os.getenv("OPENROUTER_API_KEY"),
        )
        # Use a known free model as the default
        self.model = os.getenv("LLM_MODEL", "google/gemma-2-9b-it")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream
        )

# Upcoming LLM providers - coming soon
class PerplexityService(LLMService):
    def __init__(self):
        # Coming soon - Perplexity AI integration
        raise NotImplementedError("Perplexity integration coming soon!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("Perplexity integration coming soon!")

class GrokService(LLMService):
    def __init__(self):
        # Coming soon - Grok (xAI) integration
        raise NotImplementedError("Grok integration coming soon!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("Grok integration coming soon!")

class OllamaService(LLMService):
    def __init__(self):
        # Coming soon - Local Ollama model support
        raise NotImplementedError("Ollama local model support coming soon!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("Ollama local model support coming soon!")

class PRISMCustomService(LLMService):
    def __init__(self):
        # Coming soon - Custom PRISM model trained on materials science literature
        raise NotImplementedError("PRISM Custom Model coming soon - trained on massive materials science corpus!")
    
    def get_completion(self, prompt: str, stream: bool = False):
        raise NotImplementedError("PRISM Custom Model coming soon!")

def get_llm_service() -> LLMService:
    if os.getenv("OPENAI_API_KEY"):
        return OpenAIService()
    elif os.getenv("GOOGLE_CLOUD_PROJECT"):
        return VertexAIService()
    elif os.getenv("ANTHROPIC_API_KEY"):
        return AnthropicService()
    elif os.getenv("OPENROUTER_API_KEY"):
        return OpenRouterService()
    # Upcoming providers - will be enabled soon
    elif os.getenv("PERPLEXITY_API_KEY"):
        return PerplexityService()
    elif os.getenv("GROK_API_KEY"):
        return GrokService()
    elif os.getenv("OLLAMA_HOST"):
        return OllamaService()
    elif os.getenv("PRISM_CUSTOM_API_KEY"):
        return PRISMCustomService()
    else:
        raise ValueError("No LLM provider configured.") 