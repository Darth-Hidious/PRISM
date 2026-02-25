# PRISM Vision & Roadmap

**Version:** 2.5.1
**Updated:** 2026-02-25
**Status:** Phase F complete. All CLI commands built and documented.

---

## CLI Commands — All Complete

| File | Command | Status |
|------|---------|--------|
| `search.py` | `prism search --elements --formula --band-gap-min --providers --refresh` | DONE (v2.5) |
| `ask.py` | `prism ask` — DEPRECATED, redirects to `prism run` | DEPRECATED (v2.5.1) |
| `run.py` | `prism run "goal" --agent --provider --model --confirm` — autonomous agent | DONE (v2.5.1) |
| `serve.py` | `prism serve` — MCP server mode | DONE (v2.5) |
| `data.py` | `prism data collect/import/status` | DONE (v2.5.1) |
| `predict.py` | `prism predict --dataset --target --algorithm` | DONE (v2.5.1) |
| `model.py` | `prism model train/status/calphad` | DONE (v2.5.1) |
| `optimade.py` | `prism optimade` — direct OPTIMADE queries | DONE (v2.5) |
| `sim.py` | `prism sim init/status/jobs` — DFT/MD simulation | DONE (v2.5) |
| `calphad.py` | `prism calphad` — deprecated alias for `prism model calphad` | DEPRECATED (v2.5.1) |
| `plugin.py` | `prism plugin list/init` | DONE (v2.5) |
| `labs.py` | `prism labs list/info/status/subscribe` — premium marketplace | DONE (v2.5.1) |
| `configure.py` | `prism configure --show --anthropic-key --model --reset` | DONE (v2.5.1) |
| `setup.py` | `prism setup` — workflow preferences wizard | DONE (v2.5.1) |
| `update.py` | `prism update --check-only` — version check + upgrade | DONE (v2.5.1) |
| `mcp.py` | `prism mcp` — MCP server management | DONE (v2.5) |
| `advanced.py` | `prism advanced` — dev/debug commands | DONE (v2.5) |
| `docs.py` | `prism docs` — documentation browser/generator | DONE (v2.5) |

## Infrastructure — All Complete

| Feature | Status |
|---------|--------|
| Federated search engine (40+ providers, auto-discovery, caching, circuit breakers) | DONE |
| Provider auto-discovery from OPTIMADE consortium (2-hop chain, weekly cache) | DONE |
| Layer 3 marketplace catalog (mp_native, aflow_native, mpds, omat24) | DONE |
| Split cli/main.py into cli/commands/ — 17 command modules extracted | DONE |
| Model Config Registry — 18 models across 4 providers | DONE |
| Prompt caching (Anthropic) | DONE |
| Retry with exponential backoff | DONE |
| Token & cost tracking | DONE |
| Large result truncation (ResultStore + peek_result) | DONE |
| Doom loop detection (3 identical failures) | DONE |
| Unified capability discovery (auto-injected into system prompt) | DONE |
| Unified settings.json (global + project, Claude Code pattern) | DONE |
| Update notification on startup (GitHub releases check) | DONE |

---

## What's Buildable Now (no new external dependencies)

- Interactive ML property selection in REPL (wire `list_predictable_properties` -> user choice -> predict)
- LLM review agent (second AgentCore call in review skill)
- AflowProvider — AFLUX native API adapter (marketplace entry exists, needs provider impl)
- Expose OMAT24 collector as agent tool
- Multi-objective Pareto selection
- Correlation matrix visualization
- `prism plugin install` command
- Domain-specific validation rules
- Better downstream candidate list formatting
- Automated figure captioning (LLM describes charts)
- Marketplace API backend (replace local catalog with platform API call)

## Next Updates (new deps or APIs)

- Custom GNN models (torch, torch_geometric)
- Surrogate models (Gaussian Process, neural network)
- GFlowNet samplers (gflownet package)
- ThermoCalc connector (tc-python — commercial)
- VASP input/output parsing (pymatgen.io.vasp)
- GenAI material generation (diffusion models, VAEs)
- Active learning loops
- Compute resource auto-detection

## Future (requires external systems)

- LLM graph knowledge
- Deep literature mining (full-text extraction)
- DfM (Design for Manufacturing)
- FEM orchestrators
- Process robot integration (A-Lab style)
- Multi-agent orchestration (supervisor + workers)
- Federated compute

---

## MARC27 SDK Integration (marc27-sdk)

PRISM's platform connector. A separate Python package (`pip install marc27-sdk`)
that talks to `platform.marc27.com/api/v1`. Thin REST client — no logic, just
auth + typed responses.

- **Package:** `marc27-sdk` (repo: `marc27-sdk/`, design docs there)
- **Install:** `pip install marc27-sdk` (will be an optional PRISM dependency)
- **Import:** `from marc27 import PlatformClient`

### What it gives PRISM

| Capability | SDK Method | PRISM Integration Point |
|-----------|-----------|------------------------|
| Device login (`prism login`) | `client.login()` | `app/cli/main.py` — rewrite login command |
| Managed LLM keys | `client.get_llm_key()` | `app/agent/factory.py` — platform-first, local-fallback |
| Marketplace search | `client.marketplace.search()` | `app/cli/main.py` — prism marketplace commands |
| Plugin install | `client.marketplace.install()` | `app/plugins/loader.py` — download + register |
| Model download | `client.marketplace.download()` | `app/ml/` — load models from marketplace |
| HPC job submission | `client.compute.submit_job()` | `app/tools/simulation.py` — new tool |
| Lab booking | `client.labs.book()` | `app/tools/` — new tool |
| Usage metering | `client.projects.get_usage()` | `app/cli/main.py` — prism usage command |
| Org/project switching | `client.switch_project()` | `app/cli/main.py` — prism projects command |

### How PRISM uses it

```python
# In app/agent/factory.py — managed key takes priority over local env
try:
    from marc27 import PlatformClient
    client = PlatformClient()         # reads ~/.prism/credentials.json
    key = client.get_llm_key()        # managed key for active project
    if key:
        return backend_from_key(key)  # use platform-managed provider
except Exception:
    pass                              # fall back to ANTHROPIC_API_KEY etc.
```

### SDK modules (for reference)

```
src/marc27/
├── client.py            <- PlatformClient (the one class PRISM imports)
├── auth.py              <- device auth flow (like gh auth login)
├── credentials.py       <- ~/.prism/credentials.json read/write
├── models.py            <- Pydantic: User, Org, Project, ManagedKey, Resource, Job, Booking
├── exceptions.py        <- AuthError, QuotaExceededError, NotFoundError, etc.
└── api/
    ├── base.py          <- httpx client, auth header injection, retry
    ├── marketplace.py   <- search, install, download, publish
    ├── projects.py      <- llm key, usage, resources
    ├── orgs.py          <- org CRUD, members
    ├── compute.py       <- HPC job submit/status/cancel
    └── labs.py          <- booking, availability, results
```

### Key facts

- Thin wrapper — every method = 1 API endpoint, no business logic
- Auth is transparent — auto-reads credentials, injects JWT/API-key headers
- Identity headers — `X-User-ID`, `X-Project-ID` on every request (audit trail)
- Typed errors — `AuthError`, `QuotaExceededError`, `NotFoundError` (PRISM catches these)
- Platform-first, local-fallback — if logged in, use managed keys; if not, existing .env flow works
- Detailed design: `marc27-sdk/docs/plans/2026-02-25-sdk-detailed-plan.md`
