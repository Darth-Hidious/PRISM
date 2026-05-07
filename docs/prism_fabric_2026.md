# PRISM Fabric — Private AI Compute Network

**Status:** Strategic direction. Not in flight. Captured 2026-05-07 from a
detailed conversation locking the product thesis. The current Phase 1
architecture refactor (chat targets, headless login, tool calling) keeps
shipping; this doc reframes what the *core* product becomes once that
foundation is solid.

**Thesis (one sentence):** PRISM is a **private, permissioned AI compute
fabric** for friends, labs, teams, and small companies who have scattered
devices and private data — but not enough centralized compute. The
existing orchestration / agent code is the *application layer* on top
of this fabric, not the core product.

---

## The framing shift

Before:
> "PRISM is an agentic CLI / TUI for materials discovery."

After:
> "**PRISM is a private AI compute fabric.** Devices in trusted groups
> contribute local compute and local data; PRISM schedules ML work
> intelligently across them. The agentic CLI / TUI is one consumer of
> the fabric — not the core."

The agentic harness keeps shipping (and is solid — research, discourse,
and tool calling all run end-to-end as of 2026-05-07). It just stops
being the thing PRISM *is*.

---

## Five planes

```
┌─────────────────────────────────────────────────────────────┐
│  Prism Agentic Layer                                        │
│  (the current PRISM CLI/TUI: chat, discourse, research,     │
│   workflows, tool catalog, materials-science MCP tools)     │
├─────────────────────────────────────────────────────────────┤
│  Prism ML Plane                                             │
│  inference scheduler, federated fine-tuning, DiLoCo-style   │
│  training, model sharding / pipeline execution, result      │
│  aggregation                                                │
├─────────────────────────────────────────────────────────────┤
│  Prism Compute Plane                                        │
│  node daemon (prismd), capability detection, local sandbox  │
│  runner, model runner, dataset connector, artifact cache    │
├─────────────────────────────────────────────────────────────┤
│  Prism Network Plane                                        │
│  private peer-to-peer connectivity, NAT traversal / relay   │
│  fallback, encrypted node-to-node transport, heartbeats     │
├─────────────────────────────────────────────────────────────┤
│  Prism Control Plane                                        │
│  organization, users, device identity, ACLs, job submission,│
│  node registry, scheduler. Lives on the website / MARC27.   │
└─────────────────────────────────────────────────────────────┘
```

Existing PRISM code maps roughly to: agentic layer (full), compute plane
(partial — `crates/node`, `crates/runtime`, `crates/mesh/kafka.rs`),
control plane (partial — what `marc27-platform` already does for orgs +
projects + nodes). Network plane and ML plane are mostly green-field.

---

## Tailscale as the mental model for the network plane

A user installs PRISM. Their friend in India installs PRISM. Both join
the same organization on the website. Their machines become part of a
private compute network — analogous to a Tailscale **tailnet**: a secure,
interconnected private collection of users, devices, and resources, each
device with its own private network identity, inaccessible from the
public internet except through controlled access rules.
([Tailscale tailnet docs](https://tailscale.com/docs/concepts/tailnet))

But Tailscale-the-product stops at "private connectivity." PRISM's
differentiator is what comes *next*: once those devices are connected,
PRISM knows what compute and data each device has, and schedules ML
work intelligently across them.

---

## Capability registry — the actual base layer

Every node advertises a typed manifest:

```text
node_id: A1
location: Berlin, Germany
hardware:
  cpu_cores: 12
  ram_gb: 16
  accelerator: apple-silicon
  unified_memory_gb: 16
  os: macOS-14
runtimes:
  - mlx
  - pytorch-mps
  - llama.cpp
models_cached:
  - mace-mp-0b3
  - llama-3.1-8b-q4
datasets_local:
  - corpus_inconel_papers
network:
  bandwidth_mbit: 800
  latency_ms_to_relay: 12
trust_level: founder
load:
  cpu_pct: 4
  gpu_pct: 0
```

The scheduler reasons about these manifests, not about "15 identical
computers." Heterogeneous devices are the central design challenge,
not a footnote.

| Node type | Backend it speaks | Suitable workloads |
|---|---|---|
| CUDA GPU | `pytorch-nccl` | heavy inference, LoRA fine-tuning, model shards |
| Apple Silicon | `mlx`, `pytorch-mps`, llama.cpp | small local models, embeddings, preprocessing, data-local inference, lightweight fine-tuning |
| CPU / storage | `pytorch-gloo`, native | document parsing, retrieval, dataset cleaning, evaluation, RAG indexing |

PyTorch's distributed docs back this: NCCL is the optimized backend for
CUDA tensor collectives; Gloo is the CPU-oriented backend.
([PyTorch distributed docs](https://docs.pytorch.org/tutorials/intermediate/dist_tuto.html))
A CUDA box and an Apple Silicon Mac are not one synchronous training
cluster — they are different runtime islands.

**Don't force every device into every job.** That's how you get a
beautiful architecture diagram and a useless system.

---

## Three inference modes (in order of practical value)

### Mode 1 — Route to the best node (DEFAULT)
If the friend has a CUDA box and you have a Mac mini, PRISM sends the
model call to the CUDA box, not split across both. **Simplest, ships
first, gets 90% of the value.**

### Mode 2 — Data-local inference
If a node holds private local data, PRISM sends the task to that node,
runs inference near the data, returns the answer or a sanitized output.
**No raw data leaves the source.** This is the privacy-shaped path that
matters most for labs / corporate users.

### Mode 3 — Sharded inference (advanced)
Only when one machine cannot hold the model do you split layers across
machines. **Petals**-style collaborative inference: the Petals paper
shows BLOOM-176B running collaboratively across consumer GPUs at
roughly interactive speed for research use. ([Petals — Borzunov et
al. 2022](https://arxiv.org/abs/2209.01188))

Sharded inference across continents is **advanced mode, not default**.
The latency reality of synchronous tensor exchange over the public
internet kills naïve "treat 15 computers as one supercomputer" designs.

---

## Training is harder — DiLoCo / OpenDiLoCo / SWARM are the references

Standard distributed training expects tightly interconnected accelerators
exchanging gradients at every optimization step. Across continents that
is a non-starter.

- **DiLoCo** (Google DeepMind, 2023) — workers do many local steps then
  communicate less frequently; reports matching fully-synchronous
  optimization on 8 workers while communicating **500× less**.
  ([DiLoCo — Douillard et al. 2023](https://arxiv.org/abs/2311.08105))
- **Decoupled DiLoCo** (Google DeepMind, April 2026) — extends DiLoCo
  with decoupled compute islands and asynchronous data flow,
  specifically to train across distant data centers without the
  communication delays that kill normal data-parallel training.
  ([Decoupled DiLoCo](https://deepmind.google/blog/decoupled-diloco/))
- **OpenDiLoCo** — open-source replication that demonstrated training
  across **two continents and three countries** while maintaining
  90–95 % compute utilization.
  ([OpenDiLoCo — 2024](https://arxiv.org/abs/2407.07852))
- **SWARM Parallelism** — model-parallel training on slow, heterogeneous,
  unreliable devices via randomized pipelines that rebalance after
  failures. The closest research match for "Mac mini + CUDA box +
  someone's GPU laptop in India."
  ([SWARM — Ryabinin et al. 2023](https://arxiv.org/html/2301.11913))

The right abstraction is **not "15 computers = 1 fake supercomputer."**
It is **"15 computers = several compute islands; each island trains
locally; PRISM periodically merges updates."**

---

## Privacy framing — say it correctly

DO NOT claim "data privacy" just because raw data stays on the nodes.
Federated-learning model updates and gradients can still leak training
data; that's why Google published Practical Secure Aggregation in the
first place.
([Bonawitz et al. — Practical Secure Aggregation 2017](https://research.google/pubs/practical-secure-aggregation-for-federated-learning-on-user-held-data/))

Safe phrasing for PRISM:
> *"Raw data can remain local."*

Not:
> *"Data is automatically private."*

Real privacy story (post-MVP) requires:
- Secure aggregation of model updates
- Differential privacy budgets
- Encrypted transport (already on-deck via the network plane)
- Signed jobs
- Sandboxed runners
- Audit logs of who-ran-what-on-which-node

The pen-test todo already captures this; once the fabric is real, the
pen-test becomes mandatory.

---

## Ordered roadmap (the one to actually build to)

The temptation is to start with full distributed LLM training. **Don't.**
That's the boss fight; level 1 is something that ships in a week and
gives users immediate value.

| # | Milestone | Effort | What unlocks |
|---|---|---|---|
| 1 | **Private node network** — devices in an org connect privately, encrypted transport, NAT traversal | weeks | Tailscale-grade connectivity is the floor everything else stands on |
| 2 | **Capability registry** — every node advertises hardware/runtimes/data manifest | days | Scheduler has something to reason over |
| 3 | **Remote job execution** — `prismd` accepts signed jobs, runs them locally, streams results | days | Mode-1 inference (route to best node) becomes possible |
| 4 | **Data-local batch inference** — agent submits a job that's pinned to a data-holding node | days | Mode-2 inference; the privacy-shaped killer feature for labs |
| 5 | **Distributed RAG / embedding generation** — embeddings indexed across nodes, retrieved via federated search | weeks | Knowledge graph scales beyond one machine |
| 6 | **Federated LoRA fine-tuning** — Flower-style federated rounds on adapter weights | 1–2 mo | First "actually trains" milestone; LoRA keeps comm cost manageable |
| 7 | **DiLoCo-style low-communication training** — local-step + periodic-merge for full models | 3 mo+ | Real training on distant compute islands |
| 8 | **Petals/SWARM-style sharded execution** — single model spans nodes when nothing else fits | 3 mo+ | Boss fight |

Each milestone is independently shippable. Stop and reassess product-fit
between any two.

---

## How the existing PRISM code maps in

What ships today is mostly the **agentic layer** plus **partial compute
plane**:

| Existing | Where it fits |
|---|---|
| `crates/cli` (chat, research, discourse, workflow, marketplace, billing) | Agentic layer |
| `crates/node` (`prism node up/down/status`) | Compute plane (partial — needs `prismd` long-running mode) |
| `crates/mesh` (Kafka pub/sub, sync, federation primitives) | Network plane (partial — Kafka is the wrong wire for arbitrary peer-to-peer; replace or wrap with a Tailscale-style overlay) |
| `crates/runtime` | Compute plane (per-node sandbox / runner) |
| `crates/prism_tool_router` (EmbeddingGemma top-K) | Agentic layer (good as-is) |
| `marc27-platform` (orgs, users, projects, nodes/compute routes) | Control plane (already real — extend rather than replace) |
| `crates/agent`, `crates/forge_*` (vendored harness) | Agentic layer (good as-is) |

**Don't delete anything yet.** Re-platform incrementally:
- Keep agentic layer as Top of Stack — it works, users want it.
- Replace the Kafka mesh with a private-overlay network plane only after
  the new one is real and tested. Run them side-by-side during
  transition.
- `prismd` (the long-running node daemon) is the new piece that makes
  the compute plane real. Start there once Phase 1 architecture
  refactor is merged.

---

## Renaming for clarity (when ready, not now)

Internally, when the fabric is real:

| Old | New |
|---|---|
| "PRISM" (the CLI/TUI) | **PRISM Orchestrator** or **PRISM Agent Runtime** |
| What we're building toward | **PRISM Fabric** |

External pitch:
> *PRISM Fabric: a private, permissioned AI compute network for trusted
> groups, allowing heterogeneous devices to contribute local data,
> compute, and model execution to shared inference and training jobs.*

Sharper than the current positioning. Earns the attention.

---

## What this doc is NOT

- Not a sprint plan. It's the strategic direction that informs sprints.
- Not authorization to start ripping out code. Phase 1 (provider
  architecture refactor) keeps shipping; the existing harness keeps
  working; the fabric work begins after Phase 1 lands cleanly.
- Not a claim about timeline. Each milestone above is shippable on its
  own; we don't need to commit to the whole roadmap upfront.
- Not a research substitute. Before milestone 6/7/8, the team needs
  hands-on study of Flower / Hivemind / Petals / SWARM / OpenDiLoCo,
  not just citations.

---

## Sources

### Private network model
- [Tailscale — what is a tailnet](https://tailscale.com/docs/concepts/tailnet)

### Distributed-task fabric (general)
- [Ray Core walkthrough](https://docs.ray.io/en/latest/ray-core/walkthrough.html)

### Federated learning
- [Flower framework docs](https://flower.ai/docs/framework/index.html)
- [Hivemind — decentralized deep learning](https://github.com/learning-at-home/hivemind)

### Distributed inference
- [Petals — collaborative inference + fine-tuning of large models](https://arxiv.org/abs/2209.01188)

### Heterogeneous distributed training
- [SWARM Parallelism — Ryabinin et al. 2023](https://arxiv.org/html/2301.11913)
- [DiLoCo — Douillard et al. 2023](https://arxiv.org/abs/2311.08105)
- [Decoupled DiLoCo — Google DeepMind, April 2026](https://deepmind.google/blog/decoupled-diloco/)
- [OpenDiLoCo — open-source replication, 2024](https://arxiv.org/abs/2407.07852)

### Privacy
- [Practical Secure Aggregation for Federated Learning — Bonawitz et al. 2017](https://research.google/pubs/practical-secure-aggregation-for-federated-learning-on-user-held-data/)

### PyTorch distributed primitives
- [PyTorch — writing distributed applications](https://docs.pytorch.org/tutorials/intermediate/dist_tuto.html)
