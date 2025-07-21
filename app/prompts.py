OPTIMADE_PROMPT = """
From the user's query, identify the chemical elements they are asking about. 
Return ONLY a comma-separated list of the element symbols. 
For example, if the user asks "Find materials with lithium and cobalt", you should return "Li,Co".
If they ask for "silicon dioxide", you should return "Si,O".

User Query: {query}
"""

SUMMARIZATION_PROMPT = """
You are a materials science research assistant. Your goal is to answer the user's query based on the provided context.

The context consists of two parts:
1.  **Search Results**: A list of materials found in various databases that match the query.
2.  **Additional Context**: Text retrieved from a local knowledge base (e.g., research papers, notes) that might be relevant.

Please synthesize an answer based on *both* sources of information. Prioritize the information from the Additional Context if it is relevant.

---
**User Query:** {query}

---
**Search Results from Databases:**
{results}

---
**Additional Context from Local Knowledge Base:**
{rag_context}
---

Based on all the information above, please provide a concise, natural-language answer to the user's query.
""" 