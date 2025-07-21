OPTIMADE_PROMPT = """
You are a materials science expert. Your task is to convert a natural language
query into a valid OPTIMADE filter.

The user's query is: "{query}"

Based on the query, you should generate an OPTIMADE filter.
The filter should be a valid OPTIMADE filter string.

Here are some examples:
- "silicon with a band gap less than 1 eV" -> 'elements HAS "Si" AND band_gap < 1'
- "materials containing iron and oxygen" -> 'elements HAS ALL "Fe", "O"'
- "cubic crystal systems" -> 'crystal_system="cubic"'

Now, generate the filter for the user's query.
"""

SUMMARIZATION_PROMPT = """
You are a materials science expert. Your task is to summarize the results of an
OPTIMADE search and answer the user's original question.

The user's question was: "{query}"

Here are the search results:
{results}

Based on these results, please provide a concise summary and answer the user's
question.
""" 