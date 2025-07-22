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

## CRITICAL: OPTIMADE Filter Syntax (EXACT FORMAT):
- Elements: `elements HAS ALL "Ni", "Ta"` (double quotes, HAS ALL operator)
- Number of elements: `nelements=2`
- Formula: `chemical_formula_descriptive="SiO2"`
- Combine: `elements HAS ALL "Li", "Co" AND nelements=2`

## Provider Selection Guidelines:
- Materials Project (mp): Best for general materials, DFT calculations
- OQMD (oqmd): Good for high-throughput calculations
- COD (cod): Best for crystal structures, experimental data
- AFLOW (aflow): Good for alloys and intermetallics
- JARVIS (jarvis): Good for 2D materials and properties

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

Example 4:
User Query: "Find lithium cobalt materials"
Your Response:
{{
  "provider": "mp",
  "filter": "elements HAS ALL \"Li\", \"Co\""
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

REASONING_FILTER_PROMPT = """
You are a materials science expert AI. Your task is to generate a precise OPTIMADE filter based on a user's query. You must follow a strict reasoning process and then output a JSON object containing the filter.

## User Query: 
{query}

## Available Providers:
{providers}

## OPTIMADE Schema Context:
{schema_context}

---

## INSTRUCTIONS

### Step 1: Deconstruct the Query
Analyze the user's request to identify the core scientific intent, including elements, formulas, and material properties.

### Step 2: Map to OPTIMADE Schema
Translate the user's intent into the correct OPTIMADE filter syntax. Refer to the schema context and follow these critical rules:
- **Elements**: Use `elements HAS ALL "Element1", "Element2"`. The element symbols must be in double quotes.
- **Number of Elements**: Use `nelements=3`.
- **Formula**: Use `chemical_formula_descriptive="H2O"`.
- **Combining**: Use `AND` to combine filters, e.g., `elements HAS ALL "Ga", "N" AND nelements=2`.

### Step 3: Generate Response
First, write out your reasoning process following the steps above. After your reasoning, you MUST conclude with the final JSON object in a markdown block.

---

## EXAMPLE RESPONSE

**Step 1: Deconstruct the Query**
The user is looking for materials containing both Lithium and Cobalt.

**Step 2: Map to OPTIMADE Schema**
The key properties are the presence of two elements. This maps to the `elements` field in the OPTIMADE schema. The correct syntax is `elements HAS ALL "Li", "Co"`.

**Step 3: Generate Response**
Based on the analysis, I will select 'mp' as the provider due to its general-purpose nature and construct the filter.

```json
{{
  "provider": "mp",
  "filter": "elements HAS ALL \"Li\", \"Co\""
}}
```

---

## YOUR TURN
Follow the instructions and provide your reasoning and the final JSON object for the user query.
"""