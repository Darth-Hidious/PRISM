# Resume here — end of session 2026-05-07

Quick start for tomorrow. Both repos are clean, all work pushed.

## Where to start

```bash
cd ~/Downloads/PRISM
git checkout phase1/provider-architecture
git pull
./target/release/prism research "anything materials-shaped" --depth 2
# ↑ should return a real cited answer; if it doesn't, regression — check Railway
```

## State of both repos

| Repo | Branch | HEAD | Status |
|---|---|---|---|
| `PRISM` | `phase1/provider-architecture` | `f68bedc` | 42 commits ahead of `main`, all pushed, build green, 14/14 unit tests pass |
| `marc27-core` | `main` | `664fd48` | All 3 PRs (retry / tool_calls / null-tolerant) merged + Railway live |

Railway: 9/9 services SUCCESS on `664fd48` as of session end. Public
health endpoint `marc27-api-production.up.railway.app/health` → 200.

## What today fixed

End-to-end tool calling proven working live:
- gpt-5.5 streams real tokens + tool_call arguments through
- `prism research` returns cited materials answers with real DOIs
- `prism discourse run alloy-debate` runs two agents to completion
- `prism query --semantic` retrieves prior research_session memory across runs

Default model swapped: `gemini-3.1-flash-lite-preview` → `gpt-5.5`
(documented Gemini OpenAI-compat shim bug killed tool_calls; gpt-5.5
is OpenAI-native).

Headless login shipped: `prism login --token <PAT>` and
`prism login --no-browser` for HPC / SSH-only environments.

Provider architecture refactor 2/6 steps shipped on the branch
(chat_config + use_command modules, RwLock<ChatTarget> in bridge,
Local + Provider URL routing). Steps 3-6 remaining before merge to main.

## Strategic direction docs locked (in `docs/`)

| File | What |
|---|---|
| `provider_architecture_2026.md` | A/B/C chat targets, the 6-step refactor plan |
| `prism_fabric_2026.md` | Tailscale-style private compute fabric, 5 planes, 8-step roadmap (DiLoCo / Petals / SWARM references) |
| `prism_ide_design_2026.md` | Three-tier hybrid IDE (CLI / Canvas / VSCode extension) — flagged v0.1, needs user-needs research before lock |
| `materials_science_tools_2026.md` | Six-family tool layer — vision |
| `materials_science_tools_research_2026.md` | Deep landscape research, MACE family tier strategy (cloud-only per direction) |
| `search_consolidation_2026.md` | 9 point search tools → 3 unified, ExoMatter slots into existing Provider abstraction |

## Pick-up menu (tomorrow)

| Priority | Action | Time |
|---|---|---|
| **A** | Finish Steps 3-6 of arch refactor (slash command + drop forge config write + boot UI + 6-case e2e + squash-merge to main) | ~6 h |
| **B** | Wrap `app/tools/search_engine/SearchEngine` as `materials_search` MCP tool | ~1 d |
| **C** | Tool-by-tool grind on the 100+ MCP tools — descriptions, schemas, tests | days |
| **D** | Local persistent KG mirror of MARC27's `research_session` indexing | days |
| **E** | TUI polish (the "In" clip, boot sub-3s redesign) | ~1 d |

## Known risks NOT addressed today (the "$1M checklist")

1. Security pen-test — never run, on `FUTURE` backlog
2. Load test under concurrent users — never run
3. Cost cap / runaway-loop guard — not verified
4. Multi-region / RTO-RPO documentation — none
5. Upstream LLM provider failover — only retry, no multi-provider
6. Branch protection bypassed by `--admin` merges today (expedient but risky)

If onboarding paying customers, run that checklist before scaling.
