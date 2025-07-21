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

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.chat.completions.create(
            model="gpt-4",
            messages=[{"role": "user", "content": prompt}],
            stream=stream,
        )

class VertexAIService(LLMService):
    def __init__(self):
        project_id = os.getenv("GOOGLE_CLOUD_PROJECT")
        vertexai.init(project=project_id)
        self.model = GenerativeModel("gemini-1.0-pro")

    def get_completion(self, prompt: str, stream: bool = False):
        return self.model.generate_content(prompt, stream=stream)

class AnthropicService(LLMService):
    def __init__(self):
        self.client = Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))

    def get_completion(self, prompt: str, stream: bool = False):
        return self.client.messages.create(
            model="claude-3-opus-20240229",
            max_tokens=1024,
            messages=[
                {"role": "user", "content": prompt}
            ],
            stream=stream
        )

def get_llm_service() -> LLMService:
    if os.getenv("OPENAI_API_KEY"):
        return OpenAIService()
    elif os.getenv("GOOGLE_CLOUD_PROJECT"):
        return VertexAIService()
    elif os.getenv("ANTHROPIC_API_KEY"):
        return AnthropicService()
    else:
        raise ValueError("No LLM provider configured.") 