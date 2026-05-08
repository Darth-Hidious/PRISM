# PRISM Fabric v1 — Implementation Spec

**Status:** Design draft. **NO CODE until user-approved.**
**Authors:** Sid + Claude (2026-05-08)
**Companion to:** [prism_fabric_2026.md](./prism_fabric_2026.md) (strategic thesis)
**Audience:** PRISM contributors deciding whether to start building

This doc is the **implementation-level companion** to the strategic Fabric thesis. It maps the five planes from that doc onto concrete crates that already exist, identifies exactly what's missing, and proposes an ordered build that gets us to the aerospace-prime use case in ~7 weeks.

---

## TL;DR

**Fabric is not greenfield.** PRISM already has the foundation:

| Strategic plane | Existing crate | Status |
|---|---|---|
| Compute plane | `crates/node/` | ✅ Hardware probe, E2EE (X25519/ChaCha20/Ed25519), container executor, platform heartbeat, crash-safe state. **Solid.** |
| Network plane (partial) | `crates/mesh/` | ⚠️ mDNS + platform discovery, dataset pub/sub, federated queries, Kafka. Needs: peer-to-peer overlay (Tailscale-shape) for cross-site WAN. |
| Control plane | `marc27-platform` + `crates/policy/` | ✅ Orgs / users / projects / nodes / RBAC via OPA-regorus. Per-action policy gating for workflows + tools verified working today. |
| Agentic layer | All the work merged today | ✅ 31 tools (8 unified + 3 memory + 17 standalone), stateful artifact memory, research(), promote_artifact KG bridge. |
| ML plane | (mostly green-field) | ❌ Federated training, distributed inference, gradient aggregation. **The remaining work.** |

**Fabric v1 = closing the gap on the network plane (peer-to-peer overlay) + adding cross-org primitives + locality-aware compute placement.** The ML plane (federated training, sharded inference) is **deferred to v2** because it's the boss fight and requires the foundation to be rock-solid first.

---

## Five concrete additions for Fabric v1

These map to the strategic vision but are scoped as engineering deliverables.

### F1. **Cross-org federation primitives** — sites compose without merging

**Problem (from the aerospace-prime walkthrough):** A 13-office prime has one MARC27 org per office. Each office has its own mesh, datasets, policies, and HPC. Today, mesh discovery is single-org; cross-org compute requires manual coordination.

**Build:**
- New `crates/federation/` crate
- Three core types:
  - `OrgIdentity { org_id, public_key, trust_level, contact_url }` — backed by Ed25519 from `crates/node/crypto.rs`
  - `FederationManifest { trusted_peers: [OrgIdentity], shared_resources: { datasets, compute_targets, policies } }` — declarative YAML in `~/.prism/federation.yaml`
  - `CrossOrgDecision { allowed_orgs: [OrgIdentity], denied_orgs: [(OrgIdentity, reason)], obligations }`
- New CLI: `prism federation { list | trust <peer> | revoke <peer> | manifest }`
- Cross-org peer trust extends `crates/node/crypto.rs` Ed25519 verification: requests from peer orgs must carry signatures verifiable against a key in the federation manifest.

**Estimate:** ~600 LOC. ~1 week.

### F2. **Cross-org policy intersection** — both orgs must agree

**Problem:** When org A invokes a workflow that touches org B's resources, today only org A's OPA fires. Org B has no say.

**Build:**
- Extend `prism_policy::PolicyEngine::evaluate()` to take a `&[OrgIdentity]` list. Each org's policy is evaluated; the result is the intersection.
- New helper: `intersect_decisions(decisions: &[PolicyDecision]) -> PolicyDecision` — takes the most restrictive decision; aggregates obligations from all orgs.
- Test fixtures: 2-org and 3-org policy scenarios (allow ∩ allow = allow; allow ∩ deny = deny with reason; obligations[A] ∪ obligations[B]).

**Estimate:** ~200 LOC + tests. ~3 days.

### F3. **Locality-aware compute placement** — "compute near data"

**Problem:** Today's `compute_submit` picks `cheapest` or `fastest` from the GLOBAL provider list. It doesn't know which office "owns" a dataset, so it can't avoid expensive cross-site egress.

**Build:**
- Add `home_node` field to `crates/mesh/subscription.rs` dataset metadata
- New `provider_preference: 'co_located'` strategy in the broker. Reads dataset's `home_node`; prefers that node or its same-site peers; falls through to `cheapest` if no co-located capacity.
- Cost estimate extension: `compute(action='estimate')` returns `egress_factor × cross_site_GB` cost separately so the agent can see when it's burning money on data movement.

**Estimate:** ~300 LOC. ~1 week.

### F4. **Capability descriptors + burst routing** — heterogeneous fleet awareness

**Problem:** Office A has a 10K-core SLURM cluster. Office B has a 4-GPU workstation. Office C has 12 MacBooks + a $12K/mo RunPod budget. Today, the agent has to KNOW which office is which.

**Build:**
- Extend `crates/node/detect.rs` hardware probe to report:
  ```rust
  pub struct ComputeCapability {
      pub slurm_available: bool,
      pub max_cores_per_job: u32,
      pub max_walltime_minutes: u32,
      pub egress_cost_per_gb_usd: f64,
      pub latency_to_org_kb_ms: u32,
      // existing fields: cpu_cores, ram_gb, gpus, runtimes
  }
  ```
- Compute target abstraction in the broker: jobs declare `compute_target: 'fast' | 'cheap' | 'large_memory' | 'low_egress' | 'training' | 'interactive'`. The broker maps these symbolic targets to physical providers using the capability descriptors + locality + federated policy.
- Burst routing: when local mesh is saturated AND federation manifest allows, falls through to public cloud providers (RunPod / Lambda) with cost cap from the federation manifest.

**Estimate:** ~400 LOC across `crates/node/detect.rs` + `crates/compute/`. ~1.5 weeks.

### F5. **Signed cross-org audit envelope** — provenance across boundaries

**Problem:** When org A runs a workflow that uses org B's dataset on org C's GPU, who is liable? Today's audit log records the action *within* one org. Cross-org audit needs cryptographic signing replicated to every involved party.

**Build:**
- New `crates/audit/` crate
- `AuditEnvelope { workflow, principal, resources_touched: [(org_id, resource)], timestamp, signatures: [Ed25519Sig] }`
- Each org's node daemon signs the envelope before passing it on. Stored locally at `~/.prism/audit/<envelope_id>.json`.
- Mesh subscription topic for audit metadata (NOT payload — just `{envelope_id, workflow, principals, timestamp, sig_count}`). Subscribers replicate. Tampering is detected by signature check.
- Optional opt-in archival to platform.marc27.com (off by default; all signing orgs must consent).

**Estimate:** ~500 LOC. ~1.5 weeks.

---

## What is **explicitly out of scope** for Fabric v1

These are planned for v2 once v1 is solid:

- **Federated LoRA fine-tuning** (Flower-style) — milestone 6 in the strategic doc
- **DiLoCo low-communication training** — milestone 7
- **Petals/SWARM-style sharded inference** — milestone 8
- **TEE attestation** for confidential compute (Intel TDX / AMD SEV-SNP)
- **MPC primitives** for sum / argmax / k-NN over distributed datasets
- **Differential privacy budgets** at the framework level (can be done in workflows for v1)
- **Tailscale-style WAN overlay**: v1 still relies on platform-mediated discovery + WebSocket relay. A real P2P overlay is v2.

The reason for this scope: F1–F5 give us cross-org RBAC + locality routing + audit, which is enough for the aerospace-prime walkthrough without ML training. Training comes once v1 is demoed and customers ask for it.

---

## Architecture diagram (implementation view)

```
┌─ Fabric v1 ──────────────────────────────────────────────────────┐
│                                                                  │
│   ┌─ NEW (F1) ──────────────────┐                                │
│   │ crates/federation/          │                                │
│   │   OrgIdentity (Ed25519)     │                                │
│   │   FederationManifest        │                                │
│   │   CrossOrgDecision          │                                │
│   └─────────────────────────────┘                                │
│              │                                                   │
│              ▼                                                   │
│   ┌─ EXTENDED (F2, F3, F4) ────────────────────────────────┐     │
│   │ crates/policy/  ← cross-org intersection               │     │
│   │ crates/mesh/    ← home_node locality metadata          │     │
│   │ crates/node/    ← capability descriptor extension      │     │
│   │ crates/compute/ ← compute_target abstraction + burst   │     │
│   └────────────────────────────────────────────────────────┘     │
│              │                                                   │
│              ▼                                                   │
│   ┌─ NEW (F5) ──────────────────┐                                │
│   │ crates/audit/               │                                │
│   │   AuditEnvelope             │                                │
│   │   sign / verify / replicate │                                │
│   └─────────────────────────────┘                                │
│              │                                                   │
│              ▼                                                   │
│   ┌─ REUSED (no rewrite) ───────────────────────────────────┐    │
│   │ crates/node/      E2EE, hardware probe, executor       │    │
│   │ crates/mesh/      mDNS / platform discovery, pub/sub   │    │
│   │ crates/policy/    OPA + regorus                        │    │
│   │ crates/compute/   broker, BYOC                         │    │
│   │ marc27-platform   orgs, users, projects, nodes         │    │
│   └────────────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────────────┘
```

The blue boxes (NEW/EXTENDED) total **~2000 LOC** for v1. The grey boxes (REUSED) total ~15K+ LOC and don't change.

---

## Build sequence — milestones with success criteria

| # | Milestone | Estimate | Success criterion |
|---|---|---|---|
| **F1** | Federation primitives | 1 week | `prism federation trust <peer>` writes a manifest entry; subsequent calls verify peer signatures correctly. Tests cover trust / revoke / verify-fail / verify-pass. |
| **F2** | Cross-org policy intersection | 3 days | Given two `default.rego` policies with different rules, `evaluate(input, [orgA, orgB])` returns the most restrictive decision; obligations from both orgs are surfaced. |
| **F3** | Locality metadata | 1 week | `compute_submit(provider_preference='co_located', inputs={dataset_uri: '...'})` routes to the dataset's home node when capacity is available; falls through cleanly when not. |
| **F4** | Capability descriptors + burst | 1.5 weeks | Hardware probe reports SLURM availability + walltime. `compute_target='training'` maps to SLURM nodes if any peer reports it, else to cloud. Cost estimate visible to agent before dispatch. |
| **F5** | Signed audit envelope | 1.5 weeks | Three-org workflow execution produces a signed envelope. Each org's node has a copy. Tampering with one copy is detected by the next signature verification. |
| **F6** | Three-org docker-compose demo | 1.5 weeks | The aerospace-prime walkthrough (Tokyo data + Munich SLURM + San Diego user) runs end-to-end in a docker-compose harness. Cross-org RBAC, locality routing, audit envelope all visible. |

**Total: ~7 weeks for Fabric v1.**

Each milestone is independently shippable. We can stop after any of them and assess.

---

## Use-case walkthrough — the aerospace prime (concrete)

> **Setup:** Tokyo office has the wind-tunnel CFD dataset (800 GB, sensitive). Munich office has the SLURM cluster (50 nodes, 8x A100 each). San Diego office has the user (a propulsion engineer running PRISM CLI) and a 12-MacBook conference room. Three orgs, one prime contractor.

**User says:** *"Train a turbulence-closure model on the Tokyo CFD data, run it on Munich's HPC, return the model to my workstation."*

### Today (without Fabric)

1. Engineer manually identifies which office owns the data.
2. Engineer requests VPN access from Tokyo IT (2-3 days).
3. Engineer downloads CFD data (800 GB locally → 4 hours).
4. Engineer re-uploads to Munich SLURM via SCP (4 more hours).
5. Engineer manually writes SLURM script.
6. Engineer waits 4 hours for training, downloads model artifacts back to San Diego.
7. **Total: 2 weeks of coordination, ~$3K of egress, no audit trail.**

### With Fabric v1

1. Agent calls `research(question="...")` → MARC27 RLM identifies Tokyo's CFD dataset by metadata (already works today).
2. Agent calls:
   ```python
   compute_submit(
     image='turbulence-trainer:1.0',
     inputs={'dataset_uri': 'tokyo://cfd/2024-q3'},
     compute_target='training',         # F4: symbolic target
     provider_preference='co_located',  # F3: locality-aware
     budget_max_usd=500,
   )
   ```
3. **F3 routes:** locality says Tokyo data → Munich compute (lowest egress + has SLURM per F4 capability descriptor).
4. **F1 + F2 federation:** Tokyo's policy allows Munich access to this dataset under contract X. Munich's policy allows compute consumption ≤ $500. San Diego's policy allows the agent to spend ≤ $500. Cross-org policy intersection (F2) succeeds.
5. **F5 audit envelope:** signed by Tokyo (data export), Munich (compute consumption), San Diego (job initiation). Each org has a copy.
6. Munich SLURM trains the model. Tokyo never sees the model artifacts (data sovereignty preserved). San Diego receives signed model + provenance pointer.
7. **Total: 8 minutes from question to model. Egress: $40 (Munich-to-San-Diego model only, not Tokyo-to-Munich data). Audit trail: cryptographically signed by all three parties.**

This is what Fabric v1 enables. **Zero new physics; just composing what's already in PRISM with five small additions.**

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Federation trust sprawl (transitive trust loops) | Pairwise-only trust in v1. No transitive. Documented. |
| Workflow recursion (workflows-in-workflows infinite loop) | NOT artificially limited. Per user direction: workflows are workflows; depth limits are arbitrary policy that break legit use cases. Loops self-resolve via OPA `obligations` (audit_log makes runaway loops visible) and via per-workflow cost caps in the federation manifest. |
| Policy intersection deny-by-default blocks legit workflows | `PolicyDecision.reason` always includes WHO denied + WHY. Override path: workflow author bumps `role` in the input with admin sign-off. |
| Egress cost surprise | Locality routing (F3) + `budget_max_usd` cap + pre-flight cost estimate via `compute(action='estimate')`. Cost is visible to the agent before dispatch. |
| Multi-org Rego version drift | Federation manifest pins peer's policy version. Drift triggers a warning, not a hard fail. |
| WAN overlay vaporware | v1 explicitly does NOT promise Tailscale-grade P2P. Uses platform-mediated discovery + WebSocket relay. A real overlay is v2. |
| Audit envelope log bloat | Audit metadata replicated via mesh (small); full payload only stored at signing orgs. Compaction policy: keep last 90 days local; older envelopes referenced by hash only. |

---

## Open questions for you

These are real design choices I want your input on before code:

1. **Federation trust model** — pairwise only (my recommendation, simpler) or transitive with hop limit?

2. **Policy intersection semantics** — strict intersection (most restrictive wins) or weighted (each org has a "trust weight," decisions aggregate)? My recommendation: strict for v1.

3. **Crate naming** — `crates/federation/` for the new crate (technical primitive), reserve "Fabric" as the *product* name? Or `crates/fabric/` as the umbrella? My recommendation: `federation`.

4. **v1 success criterion** — the docker-compose three-org aerospace-prime walkthrough. Yes / no / different?

5. **Should F5 (audit envelope) be in v1?** It's ~1.5 weeks of work. Without it, cross-org actions still happen (F1-F4 sufficient) but provenance is weaker. With it, contracts can rely on the audit trail. My recommendation: include in v1 — the customer story is much weaker without it.

6. **Should we attempt a real P2P overlay in v1?** I said deferred-to-v2 because it's hard. But if we use libp2p or iroh-net, it might be ~2 extra weeks for v1. Your call. My recommendation: defer; platform-mediated discovery is enough for the prime use case.

---

## What I'd build first if you OK

**Phase A (3 weeks):** F1 + F2 + F3.
- Output: agent can run a workflow that requires sign-off from two orgs' policies AND routes co-located.
- Demo to user, iterate.

**Phase B (3 weeks):** F4 + F5.
- Output: capability-aware burst routing + signed audit envelope.

**Phase C (1.5 weeks):** F6 docker-compose three-org demo.
- Output: aerospace-prime walkthrough end-to-end.

Stop after Phase A if the design needs revision. F4–F6 only start when F1–F3 are demoed and approved.

---

**No code. Awaiting your review.**

Critical questions you should answer before I touch a Rust file:
1. Does the use-case framing (aerospace prime, 3 offices, mixed HPC) match what you're targeting?
2. Are F1–F5 the right scope, or am I missing/over-including?
3. Trust model: pairwise or transitive?
4. v1 success criterion: docker-compose three-org demo — or something different?
5. Is "no ML plane in v1" too restrictive, or right-sized?
