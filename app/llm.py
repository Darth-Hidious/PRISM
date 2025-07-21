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
            max_tokens=1024,
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
        self.model = os.getenv("LLM_MODEL", "OpenRouter-Free")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model=self.model,
            messages=[{"role": "user", "content": prompt}],
            stream=stream
        )

def get_llm_service() -> LLMService:
    if os.getenv("OPENAI_API_KEY"):
        return OpenAIService()
    elif os.getenv("GOOGLE_CLOUD_PROJECT"):
        return VertexAIService()
    elif os.getenv("ANTHROPIC_API_KEY"):
        return AnthropicService()
    elif os.getenv("OPENROUTER_API_KEY"):
        return OpenRouterService()
    else:
        raise ValueError("No LLM provider configured.") 