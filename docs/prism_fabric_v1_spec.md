# PRISM Fabric v1 — Implementation Spec

**Status:** Decisions locked 2026-05-08 — building.
**Authors:** Sid + Claude (2026-05-08)
**Companion to:** [prism_fabric_2026.md](./prism_fabric_2026.md) (strategic thesis)

This is the **implementation-level** companion to the Fabric thesis. It maps the strategic planes onto the existing PRISM crates, identifies the gaps, and lays out an ordered build for v1.

---

## TL;DR

- **Crate strategy: extend `crates/mesh/`.** No new `crates/fabric/` or `crates/federation/`. Less to maintain. The product/marketing name is "PRISM Fabric"; the crate stays `prism-mesh`.
- **Trust model: transitive root CA = MARC27 platform.** Identity comes from MARC27 login. Tokens carry `org_id`, `project_id`, RBAC. No pairwise MoUs to manage.
- **Policy intersection: strictest-wins** when multiple orgs are involved in a single workflow. Most restrictive decision applies. Obligations from all parties union.
- **v1 success criterion: cross-site inference roundtrip.** "I submit an inference request at site A; it runs on a GPU at site B; the result returns to site A. With policy enforcement and a signed audit trail."
- **P2P overlay: direct + relay nodes.** Platform is the relay-of-record for v1. Direct connections allowed when peers reach each other; libp2p/iroh-net is a v1.5 hardening item, not a blocker.

---

## What's already done (no rewrite needed)

| Strategic plane | Existing crate | Status |
|---|---|---|
| Compute plane | `crates/node/` | ✅ Hardware probe, E2EE (X25519 / ChaCha20 / Ed25519), container executor, platform heartbeat, crash-safe state. |
| Network plane (partial) | `crates/mesh/` | ⚠️ mDNS + platform discovery, dataset pub/sub, federated queries, Kafka. **Needs:** cross-org peer routing + relay-aware connect logic. |
| Control plane | `marc27-platform` + `crates/policy/` | ✅ Orgs / users / projects / nodes / RBAC via OPA-regorus. Per-action policy gating verified live. |
| Agentic layer | (recent rounds) | ✅ ~31 tools, stateful artifact memory, `research()`, `promote_artifact` KG bridge. |
| Workflow layer | `crates/workflows/` | ✅ Robust schema (top-level + step-level aliases), nested workflows, OPA-gated. |
| ML plane | (mostly green-field) | ❌ Federated training, sharded inference. **Out of scope for v1** — comes once v1 is solid. |

**Fabric v1 = closing the cross-org gap on the network plane, plus locality-aware compute placement, plus signed cross-org audit. The ML plane is v2.**

---

## Five additions for Fabric v1

All five extend existing crates. No greenfield modules unless justified.

### F1. **Cross-org federation primitives** — sites compose without merging

**Where it lives:** `crates/mesh/src/federation.rs` (new module inside the existing crate).

**Why mesh:** Mesh already owns peer discovery + identity + the WebSocket transport. Federation is "the multi-org generalization of mesh subscriptions" — same shape, more entities.

**Design (revised — root-CA model):**

The MARC27 platform is the trust anchor. Every PRISM node is logged in to MARC27 via `prism login`. The login token carries:
- `org_id`
- `project_id`
- `user_id`
- `roles[]` (RBAC scopes)
- platform-signed `node_pubkey` (Ed25519, set at first node-up)

**Cross-org peer trust = "both peers carry valid platform-signed tokens."** No manual `prism federation trust <peer>`. The platform already knows which orgs exist, who's in them, and what they can do.

```rust
// crates/mesh/src/federation.rs
pub struct PeerIdentity {
    pub org_id: OrgId,
    pub project_id: Option<ProjectId>,
    pub node_id: NodeId,
    pub node_pubkey: Ed25519PublicKey,
    pub platform_signature: Ed25519Signature,  // platform-signed claim
    pub roles: Vec<Role>,
    pub valid_until: chrono::DateTime<Utc>,
}

pub struct CrossOrgRequest {
    pub source: PeerIdentity,
    pub target_org: OrgId,
    pub action: String,           // e.g. "inference.submit"
    pub resource: String,         // e.g. "node://munich-01/gpu-cluster"
    pub payload: serde_json::Value,
    pub request_signature: Ed25519Signature,  // signed by source.node_pubkey
}

pub fn verify_peer(
    request: &CrossOrgRequest,
    platform_root_pubkey: &Ed25519PublicKey,  // baked-in or fetched once at boot
) -> Result<(), TrustError> {
    // 1. Verify platform_signature over the source identity claims
    // 2. Verify request_signature over the request body
    // 3. Check valid_until is in the future
    // 4. Check roles include the requested action's required role
}
```

**CLI:** `prism federation peers` (read-only — lists known peer orgs from platform). No mutating commands; trust is managed in the MARC27 platform UI, not in the CLI.

**Estimate:** ~400 LOC. ~4 days.

### F2. **Cross-org policy intersection** — strictest-wins

**Where it lives:** `crates/policy/src/intersect.rs` (new module).

**Design:** When a workflow touches resources owned by ≥2 orgs, every involved org's policy fires. The decisions intersect with most-restrictive-wins semantics:

```rust
pub fn intersect_decisions(decisions: &[PolicyDecision]) -> PolicyDecision {
    // allow ∩ allow = allow with union of obligations
    // allow ∩ deny  = deny with all denying reasons surfaced
    // deny ∩ deny   = deny with all denying reasons surfaced
}
```

`PolicyEngine::evaluate_cross_org(input, &[org_id])` queries each org's policy bundle (cached via mesh), computes per-org decisions, then folds with `intersect_decisions`.

**Why strictest-wins (not weighted or origin-only):**
- Predictable. No "trust score" tuning.
- Defaults safe: a single denial blocks the action.
- Auditable: the denying party + reason is always visible.
- It's what regulators and contracts expect ("if any party objects, no action").

**Test fixtures:** 2-org `allow ∩ allow`, 2-org `allow ∩ deny`, 3-org with two obligations + one denial, obligations union check.

**Estimate:** ~200 LOC + tests. ~3 days.

### F3. **Locality-aware compute placement** — "compute near data"

**Where it lives:** `crates/compute/src/broker.rs` (extend) + `crates/mesh/src/subscription.rs` (extend metadata).

**Design:**
- Add `home_node: NodeId` to dataset subscription metadata
- New `provider_preference: 'co_located'` strategy in the broker. Reads `home_node` from the dataset URI's metadata; prefers that node or its same-site peers; falls through to `cheapest` when no co-located capacity is available
- `compute(action='estimate')` returns a separate `egress_factor × cross_site_GB` line in the cost breakdown so the agent sees egress before dispatch

**Estimate:** ~300 LOC. ~1 week.

### F4. **Capability descriptors + burst routing** — heterogeneous fleets

**Where it lives:** `crates/node/src/detect.rs` (extend) + `crates/compute/src/broker.rs` (extend).

**Design:**
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

Symbolic compute targets (`'fast' | 'cheap' | 'large_memory' | 'low_egress' | 'training' | 'interactive'`) map to physical providers via capability descriptors + locality + cross-org policy. Burst to public cloud (RunPod / Lambda) when local mesh is saturated AND federation manifest allows AND budget cap permits.

**Estimate:** ~400 LOC across `crates/node/detect.rs` + `crates/compute/`. ~1.5 weeks.

### F5. **Signed cross-org audit envelope** — provenance across boundaries

**Where it lives:** `crates/audit/` (new small crate — single-responsibility, used by mesh + compute + policy).

**Why a new crate (not a mesh module):** Audit is consumed by mesh, compute, AND policy. Putting it in any one of them creates a circular dep. A small dedicated crate is cleaner.

**Design:**
```rust
pub struct AuditEnvelope {
    pub envelope_id: Uuid,
    pub workflow: String,
    pub principal: PeerIdentity,
    pub resources_touched: Vec<(OrgId, String)>,
    pub timestamp: DateTime<Utc>,
    pub signatures: Vec<(OrgId, Ed25519Signature)>,  // each org signs in turn
}
```

- Each org's node daemon signs the envelope before passing it on
- Stored locally at `~/.prism/audit/<envelope_id>.json`
- Mesh subscription topic for envelope **metadata** (`{envelope_id, workflow, principals, timestamp, sig_count}`); replicated. Tampering is detected by signature check.
- Optional opt-in archival to platform.marc27.com (off by default; all signing orgs must opt in)

**Why keep F5 in v1 even with platform root CA:** Belt-and-suspenders. The platform CA gives us identity. F5 gives us a tamper-evident execution trail that survives a platform compromise. Customer contracts will ask for it.

**Estimate:** ~500 LOC. ~1.5 weeks.

---

## What's explicitly out of scope for v1

| Item | Why deferred |
|---|---|
| Federated LoRA fine-tuning (Flower-style) | ML plane = v2 |
| DiLoCo low-comm training | ML plane = v2 |
| Petals/SWARM sharded inference | ML plane = v2; v1's "inference" = single-node remote inference, not sharded |
| TEE attestation (Intel TDX / AMD SEV-SNP) | Confidential compute = v2 |
| MPC sum / argmax / k-NN over distributed datasets | Workflow-layer concern; v1 covers it via OPA |
| Differential privacy budgets (framework-level) | Workflow-layer in v1 |
| libp2p / iroh-net P2P overlay | Platform-relay floor is sufficient for v1; native overlay is v1.5 |

---

## Architecture (revised)

```
┌─ Fabric v1 ──────────────────────────────────────────────────────┐
│                                                                  │
│   ┌─ EXTENDED (F1, F3, F4) ────────────────────────────────┐     │
│   │ crates/mesh/   ← federation.rs module + home_node meta │     │
│   │ crates/node/   ← capability descriptor extension       │     │
│   │ crates/compute/← compute_target abstraction + burst    │     │
│   └────────────────────────────────────────────────────────┘     │
│              │                                                   │
│              ▼                                                   │
│   ┌─ EXTENDED (F2) ─────────────────┐                            │
│   │ crates/policy/  ← intersect.rs   │                           │
│   └─────────────────────────────────┘                            │
│              │                                                   │
│              ▼                                                   │
│   ┌─ NEW (F5) ──────────────────────┐                            │
│   │ crates/audit/                    │                           │
│   │   AuditEnvelope                  │                           │
│   │   sign / verify / replicate      │                           │
│   └─────────────────────────────────┘                            │
│              │                                                   │
│              ▼                                                   │
│   ┌─ REUSED (no rewrite) ───────────────────────────────────┐    │
│   │ crates/node/      E2EE, hardware probe, executor       │    │
│   │ crates/mesh/      mDNS / platform discovery, pub/sub   │    │
│   │ crates/policy/    OPA + regorus                        │    │
│   │ crates/compute/   broker, BYOC                         │    │
│   │ marc27-platform   orgs, users, projects, nodes, RBAC   │    │
│   └────────────────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────────────┘
```

**One new crate (`audit`), three extended crates, ~1800 LOC total** for v1.

---

## Build sequence

| # | Milestone | Estimate | Success criterion |
|---|---|---|---|
| **F1** | Federation primitives (root-CA model) | 4 days | A node from org A can submit a signed `CrossOrgRequest` to a node in org B; B verifies via platform pubkey + token expiry + RBAC roles. `prism federation peers` lists known peer orgs. |
| **F2** | Cross-org policy intersection | 3 days | `evaluate_cross_org(input, &[orgA, orgB])` returns the most-restrictive decision; obligations from both orgs surface; tested over 2-org and 3-org fixtures. |
| **F3** | Locality metadata | 1 week | `compute_submit(provider_preference='co_located', dataset_uri='...')` routes to the dataset's home node when capacity exists; falls through cleanly when not. Egress shown in cost estimate. |
| **F4** | Capability descriptors + burst | 1.5 weeks | Hardware probe reports SLURM availability + walltime. `compute_target='training'` maps to SLURM peers when available, else to cloud. Cost estimate visible before dispatch. |
| **F5** | Signed audit envelope | 1.5 weeks | Three-org workflow execution produces a signed envelope. Each org has a copy. Tampering with one copy is detected by the next signature check. |
| **F6** | **Cross-site inference demo** (the v1 done bar) | 1 week | User at site A runs `prism infer --model llama-3-70b --prompt "..."`. Routes via F3 to site B's GPU. F1 verifies cross-org request. F2 confirms both orgs' policies allow. F5 envelope signed by both orgs. Result returns to site A. End-to-end docker-compose harness. |

**Total: ~5–6 weeks for Fabric v1.**

Each milestone is independently shippable.

---

## v1 success criterion (concrete)

User runs at their workstation (site A):

```bash
prism infer --model llama-3-70b --prompt "Why is FCC iron stable below 910°C?" \
  --provider-preference co_located \
  --budget 0.50
```

What happens:

1. **F4** capability descriptors say site B has an A100 reachable with the right runtime.
2. **F3** locality routing says: site B is the cheapest for this prompt size.
3. **F1** the request is signed with site A's node key + platform token.
4. **F1** site B verifies the platform signature, checks the token's `valid_until` and `roles[]`.
5. **F2** policy intersection: A's policy (budget cap), B's policy (which models it serves), and the project policy all evaluate. Strictest-wins.
6. Site B runs inference on its GPU.
7. **F5** envelope signed by A (request) + B (execution); both sites store a copy.
8. Result streams back over the mesh subscription channel to site A.

**No VPN. No SCP. No manual SLURM script. Audit trail is cryptographic.**

This is what "inference on distributed computers that exist somewhere else in the world, and my place" means in code.

---

## v1.5 — the aerospace-prime walkthrough

The 3-office aerospace example (Tokyo CFD data + Munich SLURM + San Diego user) is the **v1.5 demo**, not the v1 done bar. v1 proves the inference roundtrip; v1.5 proves the same primitives compose into a real cross-org *training* job. v1.5 needs the v2 ML plane (federated training), so it's framed as a milestone for after v2 lands.

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Workflow recursion (workflows-in-workflows) | NOT artificially limited. Per user direction: workflows are workflows. Loops self-resolve via OPA `obligations` (audit_log makes runaway loops visible) and per-workflow cost caps. |
| Policy intersection deny-by-default blocks legit workflows | `PolicyDecision.reason` always names the denying org + rule. Override path: workflow author bumps `role` in input with admin sign-off (logged in F5 envelope). |
| Egress cost surprise | F3 locality routing + `budget_max_usd` cap + pre-flight cost estimate via `compute(action='estimate')`. |
| Platform root-CA single point of failure | Acknowledged. Mitigation: F5 audit envelope is platform-independent (Ed25519 signatures over execution facts). If platform is compromised, audit trail still proves who-did-what-when. |
| Multi-org Rego version drift | Federation manifest pins peer policy version. Drift triggers a warning, not a hard fail. |
| WAN P2P overlay vaporware | v1 explicitly relies on platform-mediated relay. Direct connections work when reachable. libp2p / iroh-net upgrade is v1.5. |
| Audit envelope log bloat | Metadata replicated via mesh (small); full payload only at signing orgs. Compaction: keep last 90 days local; older referenced by hash. |

---

## Decisions locked (was: open questions)

| # | Question | Decision |
|---|---|---|
| 1 | Trust model | **Transitive root-CA via MARC27 platform.** Identity from `prism login`. Tokens carry `org_id`, `project_id`, `roles[]`. Platform signs the node pubkey at first boot. No pairwise MoUs. |
| 2 | Policy intersection semantics | **Strictest-wins.** Most restrictive decision applies. Obligations union. Predictable, safe-default, auditable. |
| 3 | Crate naming | **Extend `crates/mesh/`.** Federation lives in a new module inside mesh, not in a separate crate. Audit gets its own small crate to avoid circular deps. Product name "PRISM Fabric" stays as marketing. |
| 4 | v1 success criterion | **Cross-site inference roundtrip.** User at site A → GPU at site B → result returns. Aerospace 3-org training walkthrough is v1.5. |
| 5 | P2P overlay scope | **Direct + relay nodes.** Platform is relay-of-record for v1. Direct connections work when reachable. libp2p/iroh-net hardening is v1.5. |
| 6 | F5 (audit envelope) in v1 | **Yes — keep.** Belt-and-suspenders against platform CA compromise. Customer contracts will ask for it. |

---

## Build phases (concrete order)

**Phase A (1.5 weeks): F1 + F2 — federation + cross-org policy.**
- Output: a signed cross-org request between two nodes is verified end-to-end. Two-org policy intersection works.
- Stop and demo before continuing.

**Phase B (2.5 weeks): F3 + F4 — locality + capability.**
- Output: `compute_submit` routes co-located + bursts to cloud when needed; capability descriptors visible.
- Stop and demo.

**Phase C (1.5 weeks): F5 — audit envelope.**
- Output: 3-org execution produces a signed envelope; tampering detected.

**Phase D (1 week): F6 — the cross-site inference demo.**
- Output: docker-compose harness with two sites; end-to-end inference roundtrip with all of F1–F5 visible in the trace.

**Total: ~6.5 weeks.** Each phase is independently shippable.

---

**Going in build order: F1 → F2 → F3 → F4 → F5 → F6.**
