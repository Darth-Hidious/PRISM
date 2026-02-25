# prism run — Autonomous Agent Mode

Run PRISM's AI agent autonomously on a research goal. The agent uses a
Think-Act-Observe-Repeat (TAOR) loop to break down the goal, call tools,
and synthesize a final answer.

## Usage

```bash
prism run "your research goal here"
prism run "find stable perovskites for solar cells" --confirm
prism run "compare band gaps across OMAT24 and MP" --model gpt-4.1
prism run "plan DFT relaxations for Fe-Ni alloys" --agent calphad_expert
```

## Options

| Flag | Description |
|------|-------------|
| `--agent NAME` | Use a named agent config from the plugin registry |
| `--provider NAME` | LLM provider: `anthropic`, `openai`, `openrouter`, `marc27` |
| `--model NAME` | Model override (e.g. `claude-sonnet-4-6`, `gpt-4.1`, `glm-4.7`) |
| `--confirm` | Require user confirmation before expensive tool calls |
| `--dangerously-accept-all` | Auto-approve all tool calls without prompting |
| `--no-mcp` | Disable loading tools from external MCP servers |

## How It Works

1. **Goal input** — your natural language goal is sent to the LLM.
2. **Planning** — for complex goals, the agent outputs a `<plan>...</plan>` block
   listing numbered steps before executing anything.
3. **TAOR loop** — the agent repeatedly:
   - **Think**: decide what tool or skill to use next
   - **Act**: call the tool with arguments
   - **Observe**: read the tool result
   - **Repeat**: continue until the goal is answered
4. **Synthesis** — the agent produces a final Markdown answer with citations.

The loop runs up to 30 iterations by default.

## Provider Auto-Detection

If `--provider` is not specified, PRISM checks environment variables in order:

1. `MARC27_TOKEN` or `~/.prism/marc27_token` — MARC27 managed backend
2. `ANTHROPIC_API_KEY` — Anthropic (Claude models)
3. `OPENAI_API_KEY` — OpenAI (GPT/o3 models)
4. `OPENROUTER_API_KEY` — OpenRouter (multi-provider gateway)

Run `prism setup` to configure keys interactively.

---

## LLM Connection Layer

The agent backend includes six production-grade features for resilience,
cost control, and observability.

### Model Config Registry

Every supported model has a frozen `ModelConfig` with context window size,
max output tokens, pricing, and capability flags (caching, thinking, tools).
The agent uses model-aware `max_tokens` instead of a hardcoded value.

**18 models across 4 providers:**

| Provider | Models | Context | Default max_tokens |
|----------|--------|---------|-------------------|
| Anthropic | claude-opus-4-6, claude-sonnet-4-6, claude-haiku-4-5 | 200K | 8K–32K |
| OpenAI | gpt-4o, gpt-4.1, gpt-5, o3, o3-mini | 128K–1M | 4K–16K |
| Google | gemini-2.5-pro, gemini-2.5-flash, gemini-3.1-pro | 1M | 8K–16K |
| Zhipu | glm-5, glm-4.7, glm-4.5-air | 128K–200K | 4K–16K |

Unknown models get conservative defaults (128K context, 8K max_tokens).
OpenRouter-prefixed IDs (e.g. `anthropic/claude-opus-4-6`) are stripped
automatically.

### Prompt Caching

Anthropic system prompts are wrapped with `cache_control: {"type": "ephemeral"}`,
enabling the API to cache and reuse the system prompt across turns. Cache reads
are 90% cheaper than re-processing. OpenAI does server-side caching automatically.

### Retry with Exponential Backoff

Transient API errors (HTTP 429, 500, 502, 503) are retried up to 3 times
with exponential backoff: 1s, 2s, 4s (capped at 8s). The `Retry-After`
header is respected when present. Auth errors (401) and bad requests (400)
are never retried.

### Token and Cost Tracking

Every API response extracts token counts into a `UsageInfo` record:
- `input_tokens`, `output_tokens`
- `cache_creation_tokens`, `cache_read_tokens` (Anthropic)

Costs are calculated from the model's pricing config and accumulated across
turns. Each `TurnComplete` event carries:
- `usage` — tokens for this turn
- `total_usage` — cumulative tokens across all turns
- `estimated_cost` — cumulative USD estimate

### Large Result Handling (RLM-Inspired ResultStore)

When a tool result exceeds 30,000 characters, the full result is stored
in an in-memory `ResultStore` and the agent receives:
- A 2,000-character preview
- A `result_id` for use with the `peek_result` tool
- Instructions to page through the full result

The agent can then call `peek_result(result_id="<id>", offset=0, limit=5000)`
to read specific sections, or use `export_results_csv` to save the full
result to a file.

This follows the RLM paradigm (Zhang et al., "Recursive Language Models",
MIT CSAIL, 2025) — treating large inputs as external environment variables
the agent can programmatically access rather than cramming them into the
context window.

### Doom Loop Detection

The agent tracks the last 10 tool calls. If the same tool with the same
arguments fails 3 times consecutively, a system warning is injected:

> DOOM LOOP DETECTED: tool_name has failed 3 times with the same arguments.
> Try a different approach, different arguments, or ask the user for help.

The agent sees this warning and changes strategy.

---

## Available Tools and Skills

The `run` command has access to all registered tools and skills:

**Data access:**
- `search_materials` — federated OPTIMADE search across 30+ providers
- `query_materials_project` — Materials Project native API
- `query_omat24` — Meta OMAT24 dataset
- `export_results_csv` — save tabular results to CSV

**Literature and patents:**
- `search_arxiv`, `search_semantic_scholar` — scientific papers
- `search_patents` — Lens.org patent search

**Analysis:**
- `predict_properties` — ML property prediction
- `calculate_phase_diagram`, `calculate_equilibrium` — CALPHAD thermodynamics
- `validate_dataset`, `review_dataset` — data quality

**Visualization:**
- `plot_comparison`, `plot_correlation_heatmap`, `plot_property_distribution`

**Multi-step skills:**
- `materials_discovery` — end-to-end pipeline (acquire, predict, visualize, report)
- `plan_simulations` — auto-routes CALPHAD vs DFT vs MD
- `analyze_phases` — phase stability analysis
- `generate_report` — Markdown/HTML/PDF reports

---

## Search Fusion and Provider Separation

The federated search system fuses materials from multiple OPTIMADE providers
by identity key (`formula::space_group`). Crucially, **provider provenance is
fully preserved**:

- `Material.sources: list[str]` — which providers contributed this record
- `PropertyValue.source: str` — which provider supplied each property value
- Conflicting values are stored in `extra_properties` with source-tagged keys
  (e.g. `band_gap:cod`, `band_gap:mp`)

This means you can:

1. **Collect enriched data** — fuse properties across MP, COD, AFLOW, JARVIS, etc.
2. **Separate by provider** — filter results by `Material.sources` to get
   provider-specific subsets for training DFT surrogates or ML models.
3. **Benchmark models** — train on one provider's data, test against another's,
   using the source tags to split cleanly.
4. **Compare values** — examine how the same property differs across providers
   using the conflict entries in `extra_properties`.

```python
# Example: separate MP data for training, COD for benchmarking
from app.search.fusion import fuse_materials

fused = fuse_materials(all_results)
mp_only = [m for m in fused if "mp" in m.sources]
cod_only = [m for m in fused if "cod" in m.sources]
```

Or pre-filter at query time:

```python
from app.search.query import MaterialSearchQuery

query = MaterialSearchQuery(elements=["Fe", "Ni"], providers=["mp"])
```
