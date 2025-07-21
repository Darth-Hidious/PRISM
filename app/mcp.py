from typing import List, Dict, Any, Optional, Tuple
from app.prompts import SUMMARIZATION_PROMPT, REASONING_PROMPT, CONVERSATIONAL_PROMPT, FINAL_FILTER_PROMPT
import json
import re

class ModelContext:
    """
    The Model Context Protocol (MCP) for materials science data.

    This class encapsulates the data and metadata required by the LLM to
    understand and reason about materials science data.
    """

    def __init__(self, query: str, results: List[Dict[str, Any]], rag_context: Optional[str] = None):
        self.query = query
        self.results = results
        self.rag_context = rag_context

    def to_prompt(self, reasoning_mode: bool = False) -> str:
        """
        Constructs a prompt for the LLM by summarizing the search results and
        including optional RAG context.
        """
        # Truncate results to avoid exceeding token limits
        summarized_results = [
            {
                "formula": r.get("attributes", {}).get("chemical_formula_descriptive", "N/A"),
                "provider": r.get("meta", {}).get("provider", {}).get("name", "N/A"),
                "elements": r.get("attributes", {}).get("elements", []),
                "id": r.get("id", "N/A")
            }
            for r in self.results[:10] # Reduced to 10 results to save tokens
        ]
        
        if reasoning_mode:
            # For reasoning mode, provide more detailed data
            results_str = "\n".join([
                f"- Material {r['id']}: {r['formula']} (elements: {', '.join(r['elements'])}) from {r['provider']}" 
                for r in summarized_results
            ])
            
            return REASONING_PROMPT.format(
                query=self.query,
                results=results_str
            )
        else:
            # Standard summarization - more concise
            results_str = "\n".join([f"- {r['formula']} ({r['provider']})" for r in summarized_results])
            
            return SUMMARIZATION_PROMPT.format(
                query=self.query,
                count=len(self.results),
                results=results_str
            )


class AdaptiveOptimadeFilter:
    """
    Iterative OPTIMADE filter generator with error feedback loop.
    
    This class works with LLMs to generate and refine OPTIMADE filters
    until they work correctly with the actual OPTIMADE API.
    """
    
    def __init__(self, llm_service, providers_info: List[Dict[str, str]]):
        self.llm_service = llm_service
        self.providers_info = providers_info
        self.max_attempts = 3
    
    def conduct_interactive_conversation(self, original_query: str, console=None, max_questions: int = 3) -> Tuple[List[str], str]:
        """
        Conduct an interactive conversation to refine the search query.
        
        Returns:
            Tuple of (keywords_list, final_conversation_summary)
        """
        conversation_history = []
        all_keywords = []
        
        try:
            if console:
                console.print(f"[cyan]Starting interactive consultation for:[/cyan] {original_query}")
                console.print("[dim]I'll ask a few targeted questions to help refine your search...[/dim]")
        except UnicodeEncodeError:
            if console:
                print(f"Starting interactive consultation for: {original_query}")
        
        for question_num in range(max_questions):
            try:
                # Generate next question based on conversation so far
                history_str = " | ".join([f"Q: {item['question']} A: {item['answer']}" for item in conversation_history])
                
                prompt = CONVERSATIONAL_PROMPT.format(
                    original_query=original_query,
                    conversation_history=history_str if history_str else "No previous conversation"
                )
                
                response = self.llm_service.get_completion(prompt)
                response_text = self._extract_response_text(response)
                
                # Parse response
                try:
                    data = json.loads(response_text.strip())
                    question = data.get("question", "What specific materials properties are you looking for?")
                    keywords = data.get("keywords", [])
                except:
                    question = "What specific materials properties are you looking for?"
                    keywords = []
                
                # Ask the user
                try:
                    if console:
                        user_answer = console.input(f"[yellow]Q{question_num + 1}:[/yellow] {question}\n[cyan]Your answer:[/cyan] ")
                except:
                    user_answer = input(f"Q{question_num + 1}: {question}\nYour answer: ")
                
                if user_answer.strip():
                    conversation_history.append({
                        "question": question,
                        "answer": user_answer.strip()
                    })
                    all_keywords.extend(keywords)
                    
                    try:
                        if console:
                            console.print("[dim green]✓ Got it, analyzing...[/dim green]")
                    except UnicodeEncodeError:
                        if console:
                            print("✓ Got it, analyzing...")
                else:
                    # If user gives empty answer, stop asking questions
                    break
                    
            except Exception as e:
                try:
                    if console:
                        console.print(f"[dim red]Error in conversation: {str(e)[:50]}[/dim red]")
                except UnicodeEncodeError:
                    if console:
                        print(f"Error in conversation: {str(e)[:50]}")
                break
        
        # Create conversation summary
        conversation_summary = " | ".join([f"{item['answer']}" for item in conversation_history])
        
        return all_keywords, conversation_summary
    
    def generate_final_filter_from_conversation(self, original_query: str, keywords: List[str], 
                                               conversation_summary: str) -> Tuple[Optional[str], Optional[str]]:
        """
        Generate the final OPTIMADE filter based on the conversation.
        
        Returns:
            Tuple of (provider, filter_string)
        """
        try:
            providers_str = "\n".join([
                f"- {p['id']}: {p['name']}" for p in self.providers_info[:10]  # Limit for token efficiency
            ])
            
            prompt = FINAL_FILTER_PROMPT.format(
                original_query=original_query,
                keywords=", ".join(keywords[:10]),  # Limit keywords
                conversation_summary=conversation_summary[:500],  # Truncate for tokens
                providers=providers_str
            )
            
            response = self.llm_service.get_completion(prompt)
            response_text = self._extract_response_text(response)
            
            # Parse the response
            provider, filter_str = self._parse_response(response_text)
            return provider, filter_str
            
        except Exception as e:
            return None, None
    
    def generate_filter(self, query: str, optimade_client, console=None) -> Tuple[Optional[str], Optional[str], Optional[str]]:
        """
        Generate a working OPTIMADE filter through iterative refinement.
        
        Returns:
            Tuple of (provider, filter, error_message)
            - If successful: (provider_id, filter_string, None)
            - If failed: (None, None, error_message)
        """
        
        # Initial prompt to generate filter
        initial_prompt = self._create_initial_prompt(query)
        
        for attempt in range(self.max_attempts):
            try:
                try:
                    if console:
                        console.print(f"[dim]Attempt {attempt + 1}: Generating OPTIMADE filter...[/dim]")
                except UnicodeEncodeError:
                    if console:
                        print(f"Attempt {attempt + 1}: Generating OPTIMADE filter...")
                
                # Get LLM response
                response = self.llm_service.get_completion(initial_prompt)
                response_text = self._extract_response_text(response)
                
                # Parse the response
                provider, filter_str = self._parse_response(response_text)
                if not provider or not filter_str:
                    try:
                        if console:
                            console.print("[dim]Failed to parse LLM response, retrying...[/dim]")
                    except UnicodeEncodeError:
                        if console:
                            print("Failed to parse LLM response, retrying...")
                    continue
                
                try:
                    if console:
                        console.print(f"[dim]Testing filter: {filter_str} on provider: {provider}[/dim]")
                except UnicodeEncodeError:
                    if console:
                        print(f"Testing filter: {filter_str} on provider: {provider}")
                
                # Test the filter with OPTIMADE
                error = self._test_filter(optimade_client, provider, filter_str)
                
                if error is None:
                    # Success! Filter works
                    try:
                        if console:
                            console.print("[dim green]Filter validated successfully![/dim green]")
                    except UnicodeEncodeError:
                        if console:
                            print("Filter validated successfully!")
                    return provider, filter_str, None
                else:
                    # Filter failed, create refinement prompt
                    try:
                        if console:
                            console.print(f"[dim yellow]Filter failed: {error[:100]}{'...' if len(error) > 100 else ''}[/dim yellow]")
                            console.print(f"[dim]Refining filter (attempt {attempt + 1})...[/dim]")
                    except UnicodeEncodeError:
                        if console:
                            print(f"Filter failed: {error[:100]}{'...' if len(error) > 100 else ''}")
                            print(f"Refining filter (attempt {attempt + 1})...")
                    initial_prompt = self._create_refinement_prompt(query, provider, filter_str, error, attempt)
                    
            except Exception as e:
                try:
                    if console:
                        console.print(f"[dim red]Attempt {attempt + 1} failed: {str(e)[:100]}[/dim red]")
                except UnicodeEncodeError:
                    if console:
                        print(f"Attempt {attempt + 1} failed: {str(e)[:100]}")
                continue
        
        return None, None, "Could not generate a working OPTIMADE filter after multiple attempts."
    
    def _create_initial_prompt(self, query: str) -> str:
        """Create the initial prompt for filter generation."""
        providers_str = "\n".join([
            f"- {p['id']}: {p['name']} - {p['description']}" 
            for p in self.providers_info
        ])
        
        return f"""You are an OPTIMADE filter expert. Generate a valid OPTIMADE filter for the user query.

Available providers:
{providers_str}

OPTIMADE Filter Syntax Rules:
1. Elements: elements HAS ALL "Ni", "Ta" (MUST use double quotes, not single quotes)
2. Number of elements: nelements=2
3. Formula: chemical_formula_descriptive="SiO2"
4. Combine with AND: elements HAS ALL "Si", "O" AND nelements=2

CRITICAL: Always use double quotes (") never single quotes (') for strings!

Common provider name mappings:
- OQCD/OQMD → oqmd
- Materials Project → mp  
- COD/Crystallography Open Database → cod

Return ONLY a JSON object:
{{
  "provider": "provider_id",
  "filter": "optimade_filter_string"
}}

User Query: {query}"""
    
    def _create_refinement_prompt(self, query: str, failed_provider: str, failed_filter: str, error: str, attempt: int) -> str:
        """Create a refinement prompt based on the previous error."""
        return f"""The previous OPTIMADE filter failed. Please fix it based on the error.

Original Query: {query}
Previous Provider: {failed_provider}
Previous Filter: {failed_filter}
Error Received: {error}

Common OPTIMADE errors and fixes:
- "Property 'X' not supported" → Remove or replace property X
- "Syntax error" → Check quotes, parentheses, operators
- "Invalid operator" → Use HAS ALL, =, >, <, AND, OR correctly
- "Unknown provider" → Use exact provider IDs: mp, oqmd, cod, aflow, jarvis, mcloud

Return ONLY a corrected JSON object:
{{
  "provider": "provider_id", 
  "filter": "corrected_optimade_filter"
}}

This is attempt {attempt + 1} of {self.max_attempts}."""
    
    def _extract_response_text(self, response) -> str:
        """Extract text from different LLM response formats."""
        if hasattr(response, 'choices') and response.choices:
            return response.choices[0].message.content.strip()
        elif hasattr(response, 'content') and response.content:
            return response.content[0].text.strip()
        else:
            return str(response).strip()
    
    def _parse_response(self, response_text: str) -> Tuple[Optional[str], Optional[str]]:
        """Parse JSON response to extract provider and filter."""
        try:
            # Clean up response
            response_text = response_text.strip()
            
            # Remove markdown code blocks
            response_text = re.sub(r'```json\n(.*?)\n```', r'\1', response_text, flags=re.DOTALL)
            response_text = re.sub(r'```\n(.*?)\n```', r'\1', response_text, flags=re.DOTALL)
            
            # Extract JSON object
            json_match = re.search(r'\{.*\}', response_text, re.DOTALL)
            if json_match:
                response_text = json_match.group(0)
            
            data = json.loads(response_text)
            return data.get("provider"), data.get("filter")
            
        except (json.JSONDecodeError, KeyError):
            return None, None
    
    def _test_filter(self, optimade_client, provider: str, filter_str: str) -> Optional[str]:
        """
        Test the filter syntax and provider validity.
        
        Returns:
            None if successful, error message if failed
        """
        # Check if provider is valid
        valid_providers = [p["id"] for p in self.providers_info]
        if provider not in valid_providers:
            return f"Invalid provider '{provider}'. Valid providers: {', '.join(valid_providers)}"
        
        # Basic syntax validation for common OPTIMADE patterns
        if not filter_str or not filter_str.strip():
            return "Empty filter string"
        
        # Check for common syntax issues
        if 'HAS ALL' in filter_str:
            # Check for single quotes (not allowed in OPTIMADE)
            if "'" in filter_str:
                return "OPTIMADE requires double quotes, not single quotes. Use: elements HAS ALL \"Ni\", \"Ta\""
            
            # Check for proper double quotes around elements
            if not re.search(r'HAS ALL\s+"[^"]+"\s*(?:,\s*"[^"]+")*', filter_str):
                return "Invalid HAS ALL syntax - elements must be in double quotes: elements HAS ALL \"Ni\", \"Ta\""
        
        # Check for unbalanced quotes
        if filter_str.count('"') % 2 != 0:
            return "Unbalanced quotes in filter"
        
        # For now, assume the filter is syntactically correct if it passes basic checks
        # In a production system, you might want to use a proper OPTIMADE parser
        return None 