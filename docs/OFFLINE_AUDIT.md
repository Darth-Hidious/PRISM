# Offline / Standalone Cold-Start Audit

Product version: `1.0.0` (from the workspace/package manifests, not git tags).

This is the pre-fix audit for a compute node with no account, no network
egress, no Docker, and no root. Line references are to the source tree audited
before the offline changes in this work.

## Startup and per-turn outbound targets

| Target | Code path | Timing | Lazy / degradation / disablement |
|---|---|---|---|
| `api.marc27.com` or `MARC27_PLATFORM_URL` | `crates/runtime/src/lib.rs:404-445`; Rust platform calls in `crates/client/src/api.rs:194-307`; boot checks in `crates/cli/src/boot_checks.rs:46-258` | Setup, node registration, explicit platform commands, and platform-backed turns | Client helpers return errors; some commands require the platform. `--offline` is intended to disable it, but the flag was set after venv provisioning at `crates/cli/src/main.rs:1444,1463-1465`. |
| Marketplace API and its artifact URL | `crates/cli/src/tool_sync.rs:120-210`; startup invocation `crates/cli/src/main.rs:1507-1517` | Detached startup sync for TUI/backend/resume/campaign when credentials exist; explicit marketplace commands | Detached and non-fatal, but still attempts egress. No hard global gate before the offline fix. |
| Configured LLM endpoint | HTTP client `crates/llm/src/lib.rs:198-230,276-305`; provider registry `crates/core/providers.toml:56-166` | Every chat, extraction, workflow LLM step, and agent turn | Per-request errors are returned; a blackholed endpoint can wait for the configured timeout. No global offline gate before the fix. |
| Local LLM endpoints (`localhost:11434`, `:8080`, `:1234`, `:8000`) | `crates/core/providers.toml:173-192`; probes `crates/node/src/detect.rs:410-482` | Local model discovery and local inference | Intended to be local-only and usually fails quickly when refused; a listening but stalled service can delay. Not globally gated. |
| Python platform API | `app/tools/_platform_client.py:59-106`; default resolution `app/tools/_platform_creds.py:29-78` | Platform-backed Python tool calls | Already returns a structured offline error when `PRISM_OFFLINE=1`; the environment was propagated too late for venv provisioning. |
| OPTIMADE federation and provider hosts | Discovery/refresh `app/tools/search_engine/providers/discovery.py:16-121`, `refresh.py:12-80`; overrides `app/tools/search_engine/providers/provider_overrides.json:8-263` | Materials search, provider refresh, and explicit discovery | Provider failures are isolated by the search layer. Hosts are dynamic, so the configured/provider URL set is not finite. No global offline gate before the fix. |
| Materials catalog APIs | `app/plugins/catalog.json:10-82` (Materials Project, AFLOW, MPDS, OMAT24/Hugging Face entries); materials search tools | Explicit materials/search tools | Lazy and tool-level errors; no global offline gate before the fix. |
| Web search/read | DuckDuckGo `app/tools/system.py:108`, `app/tools/web.py:237-243`; Firecrawl local/API `app/tools/web.py:19-20,43-120` | Explicit web tools | Lazy and tool-level errors. No global offline gate before the fix. |
| Literature and patent APIs | `app/tools/data_collectors/literature_collector.py:13-86` (arXiv/Semantic Scholar); `patent_collector.py:13-39` (Lens) | Explicit collectors | Lazy and collector-level errors. No global offline gate before the fix. |
| Remote embeddings | `crates/embed/src/openai.rs:57-116`; optional native model bootstrap described at `crates/embed/src/lib.rs:10-34` | Lazy retrieval/provenance embedding and query paths | Some callers fall back to keyword/no embedding; remote embedding had no global offline gate. Native model download is not safe on a fresh offline node. |
| PyPI / GitHub / `bootstrap.pypa.io` | `crates/python-bridge/src/venv.rs:63-99,173-186` | First Python-dependent command | **Not clean before the fix**: pip/get-pip/source fallback can contact the network and can wait. No offline provisioning path existed. |
| Node control plane HTTPS/WebSocket | `crates/node/src/daemon.rs:1-2,528-673`; platform client and node registry | Online `node up`, heartbeat, reconnect, job dispatch | Online daemon reconnects; `node up --offline` avoids registration. Top-level hard offline did not cover all startup work. |
| Local mesh/Kafka/mDNS and peer URLs | `crates/mesh/src/mdns.rs`, `kafka.rs:29`, `federated_query.rs:62`, `sync.rs:170` | Node/mesh startup or explicit mesh queries | Offline mesh handle is clean, but local probes and configured peer URLs remain explicit network paths. |
| User-supplied workflow/LLM URLs | `crates/workflows/src/lib.rs:379-959,1208-1588`; workflow SSRF validation at `:2302-2337` | Explicit workflow execution | Lazy; SSRF protection covers local/private targets, but no hard offline gate before the fix. |
| User-supplied MCP servers | `crates/agent/src/mcp.rs:121-167` and `connect_one` in the same module | Agent startup when `~/.prism/mcp.json` exists | Per-server failure is logged and skipped; configured local/remote MCP servers can still be contacted. |

The finite provider list in `crates/core/providers.toml` currently names
OpenAI, Anthropic, Google/Gemini, OpenRouter, Groq, Cerebras, Z.ai, Mistral,
DeepSeek, xAI, Together, Fireworks, Cohere, plus local Ollama/llama.cpp/
LM Studio/vLLM endpoints. User configuration can add arbitrary HTTP(S) LLM,
embedding, workflow, MCP, peer, and provider URLs; those are therefore
reported as dynamic targets rather than invented as a closed host list.

## Cold-start conclusion

The required invariant is stronger than “requests eventually error”: a fresh
offline invocation must not invoke package bootstrap, marketplace sync,
platform registration, remote LLM/embedding resolution, provider refresh, or
remote MCP discovery. Local-only work may proceed, and an explicitly selected
local service may be contacted with bounded timeouts. Missing Python/science
extras must fail immediately with a structured, actionable result or use a
local wheelhouse; they must not tell the user to run `pip` manually on the
compute node.

The subsequent commits implement that invariant, offline wheelhouse
provisioning, and filesystem-safe local persistence.
