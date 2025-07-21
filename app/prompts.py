OPTIMADE_PROMPT = """
From the user's query, identify the chemical elements they are asking about. 
Return ONLY a comma-separated list of the element symbols. 
For example, if the user asks "Find materials with lithium and cobalt", you should return "Li,Co".
If they ask for "silicon dioxide", you should return "Si,O".

User Query: {query}
"""

ROUTER_PROMPT = """
You are an expert materials science data router. Your task is to analyze a user's query and select the single most appropriate database provider from the list provided. Then, generate a valid OPTIMADE filter for that provider.

Return a JSON object with the following keys: "provider", "filter".
- "provider": The ID of the selected provider (must match exactly from the list below).
- "filter": The OPTIMADE filter string using proper syntax.

Important notes:
- Use exact provider IDs: "mp", "oqmd", "cod", "aflow", "jarvis", "mcloud"  
- Common name variations: OQCD = oqmd, Materials Project = mp, Crystallography Open Database = cod
- For elements, use format: elements HAS ALL "Ni", "Ta"
- For number of elements, use: nelements=2
- Multiple providers can be mentioned but pick the MOST appropriate one

Available providers:
{providers}

---
Example 1:
User Query: "Find materials with Ni and Ta in OQMD"
Your Response:
{{
  "provider": "oqmd",
  "filter": "elements HAS ALL \"Ni\", \"Ta\""
}}

Example 2:
User Query: "Crystal structures with silicon and oxygen in COD"
Your Response:
{{
  "provider": "cod", 
  "filter": "elements HAS ALL \"Si\", \"O\""
}}

Example 3:
User Query: "Binary compounds in Materials Project"
Your Response:
{{
  "provider": "mp",
  "filter": "nelements=2"
}}
---

Return ONLY a valid JSON object. No other text.

User Query: {query}
"""

SUMMARIZATION_PROMPT = """
Analyze these materials search results:

Query: {query}
Found: {count} materials
Top results: {results}

Provide a brief scientific summary focusing on:
- Key material types found
- Notable patterns or compositions
- Relevance to the query

Keep response under 150 words.
"""

REASONING_PROMPT = """
You are a materials science expert performing multi-step reasoning. Break down the user's query into logical steps and reason through each one.

**User Query:** {query}

**Available Data:** {results}

**Instructions:**
1. First, identify what the user is asking for
2. Break down the query into searchable components
3. Analyze the search results step by step
4. Draw scientific conclusions based on the evidence
5. Provide reasoning for each step

Format your response as:
**Step 1: Understanding the Query**
[Your analysis]

**Step 2: Data Analysis**
[Your analysis]

**Step 3: Scientific Conclusions**
[Your conclusions]

**Final Answer:**
[Synthesized response]
"""

CONVERSATIONAL_PROMPT = """
You are conducting an interactive materials science consultation. Based on the conversation so far, ask ONE specific, targeted question to refine the search.

Original Query: {original_query}
Conversation History: {conversation_history}

Your task:
1. Analyze what information is still needed
2. Ask ONE precise question that will help narrow the search
3. Focus on the most important missing aspect

Return ONLY a JSON object:
{{
  "question": "your specific question here",
  "keywords": ["key", "terms", "from", "conversation", "so", "far"]
}}

The keywords should capture the essential search terms from the conversation to optimize token usage.
"""

FINAL_FILTER_PROMPT = """
Based on this materials science conversation, generate the final OPTIMADE filter and select the best database.

Original Query: {original_query}
Key Information Gathered: {keywords}
Full Conversation: {conversation_summary}

Available Databases: {providers}

Generate a valid OPTIMADE filter using proper syntax:
- Elements: elements HAS ALL "Ni", "Ta" (double quotes required)
- Number of elements: nelements=2  
- Formula: chemical_formula_descriptive="SiO2"
- Combine with AND

Return ONLY a JSON object:
{{
  "provider": "best_database_id",
  "filter": "optimade_filter_string"
}}
""" 