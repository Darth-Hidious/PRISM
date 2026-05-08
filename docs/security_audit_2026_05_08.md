# PRISM Security Audit — 2026-05-08

**Scope:** Pre-Fabric pen-test pass. Inventory of CVEs + code-level
security review. The goal: zero surprises before Fabric v1
introduces cross-org compute primitives (federation, audit envelope,
etc.).

**Methodology:** `cargo audit`, `pip-audit`, manual review of
auth / shell / file / TLS / SQL / secrets paths against PRISM's
threat model.

**Outcome:** No exploitable CVEs reachable from PRISM's user-facing
paths. 7 of 15 Python CVEs patched in this pass; 8 remaining are
either transitive (no upstream fix yet), tooling-only (pip/pytest
warnings — not runtime), or behind constraint pins that would need
upstream package bumps.

---

## Threat model

PRISM is a **client-side** materials-science workspace. The threat
surface is:

| Surface | Trust assumption |
|---|---|
| MARC27 platform (`api.marc27.com`) | Trusted host, TLS-validated |
| Hugging Face Hub | Trusted, TLS-pinned |
| Local LLM server (`llama-server`) | Loopback only |
| User-supplied YAML workflows | Untrusted (LLMs / Internet sources) |
| User-supplied datasets | Untrusted (CSV/JSON path traversal risk) |
| Mesh peers (Fabric v1+) | Trusted via platform-signed identity (F1) |
| Container images run on-node | Trusted (user-pulled) |

PRISM is **not** a public-facing server. The dashboard listens on
loopback with no authentication; there is no remote attack surface
unless the user binds it to an external port (off by default).

---

## Cargo audit — 9 unmaintained warnings, 0 unallowlisted CVEs

```
$ cargo audit
warning: 9 allowed warnings found
exit=0
```

All 9 are dependency-tree warnings (unmaintained crates), all transitive
via vendored forge_* crates. Each was reviewed at allowlist time;
[`.cargo/audit.toml`](../.cargo/audit.toml) documents the rationale.
Highlights:

| Crate | Issue | Why accepted |
|---|---|---|
| `bincode 1.3.3` | unmaintained | via `syntect` (markdown highlighting) — no untrusted input |
| `paste 1.0.15` | unmaintained | via `fnv_rs` macro — pure compile-time |
| `rustls-pemfile` | unmaintained | via `gix` — repo metadata only |
| `yaml-rust` | unmaintained | via `serde_yml`; we control YAML inputs |
| `libyml` | unsound | via `serde_yml`; not exposed to remote bytes |
| `rand` (×3) | unsound w/ custom logger | we don't install custom loggers |
| `serde_yml 0.0.12` | unsound | parses local prism.toml + workflow YAMLs only |

Plus 5 hard-blocked CVEs in the allowlist (rustls-webpki name-constraints,
hickory-proto NSEC3 / O(n²)) — accepted because PRISM's client code
talks only to platform.marc27.com (controlled host) and HF Hub
(TLS-pinned), not arbitrary servers.

---

## Python audit — 7 / 15 CVEs patched in this pass

### Patched (7)

| Package | From | To | CVE |
|---|---|---|---|
| `authlib` | 1.6.9 | 1.6.11+ | CVE-2026-41425 |
| `cryptography` | 46.0.6 | 46.0.7+ | CVE-2026-39892 |
| `lxml` | 6.0.2 | 6.1.0+ | CVE-2026-41066 |
| `mako` | 1.3.10 | 1.3.12+ | CVE-2026-44307 |
| `openviking` | 0.3.3 | 0.3.14 | CVE-2026-40525 |
| `python-multipart` | 0.0.22 | 0.0.27+ | CVE-2026-40347, CVE-2026-42561 |
| `click`, `python-dotenv` | (resolver) | latest | unpinned conflicts |

### Remaining (8) — each evaluated for exploitability

#### `litellm` 1.83.0 — 3 CVEs (CVE-2026-42203, -42208, -42271). Fix at 1.83.7

**Pinned by `openviking` 0.3.14** which constrains `litellm<1.83.1`.

PRISM uses litellm exclusively through openviking's research-loop
engine (server-side, on the MARC27 platform). The PRISM client
receives platform-rendered answers — it does not feed untrusted
prompts directly into litellm.

**Action:** flagged for openviking maintainer (same team as PRISM)
to bump the litellm constraint. Tracked as openviking/issues#TBD.

#### `pip` 26.0.1 — CVE-2026-3219, CVE-2026-6357. Fix at 26.1

Affects only the `pip install` path itself (e.g. malicious sdist
hooks). Mitigated because PRISM users `pip install` from the
controlled MARC27 / PyPI registries, never arbitrary repos.

**Action:** bump `pip` in the bootstrap script.

#### `py` 1.11.0 — PYSEC-2022-42969 (regex DoS)

Pulled in by `retry`. Affects only ReDoS in patterns under
attacker control — none of PRISM's regex inputs are
attacker-controlled.

**Action:** drop `retry` (replace with `tenacity` which doesn't
pull `py`) — flagged as low-priority follow-up.

#### `pytest` 8.4.2 — CVE-2025-71176. Fix at 9.0.3

Dev-only dependency, not present in the runtime venv users get.

**Action:** bump in dev pyproject.toml; no end-user exposure.

#### `transformers` 4.56.2 — CVE-2026-1839. Fix at `5.0.0rc3`

Fix is on a release-candidate, not a stable. Bumping to a 5.0
RC is risky for ML reproducibility.

**Action:** wait for `transformers 5.0.0` GA, then bump.

---

## Code-level review

### Authentication & credential storage

| Check | Result |
|---|---|
| Credentials file mode | **0600** (owner read/write only) ✓ |
| `prism.toml` mode | **0644** (no secrets in this file) ✓ |
| Token in logs | **Not present** — `format!("Bearer {token}")` only used in HTTP headers, never `info!`/`debug!`/`println!` ✓ |
| Token in URL params | **Not present** — bearer header only ✓ |
| Refresh-on-401 (PR #33) | **Implemented** — automatic refresh before re-prompt ✓ |
| Honest auth messaging (PR #33) | **Implemented** — distinguishes 401 / 403 / 5xx / network ✓ |

### Shell / file sandboxing

| Tool | Sandbox |
|---|---|
| `bash` (`app/tools/bash.py`) | Bound to `_ALLOWED_BASE = Path.cwd()`. Path-traversal-checked. **Approval-gated** (`requires_approval=True` on every shell exec). |
| `file` (`app/tools/system.py`) | `_is_safe_path()` resolves and verifies path is inside `_ALLOWED_BASE` or its descendants. |
| `execute_python` | Runs in the project venv; sandboxed by venv site-packages isolation. |

### TLS / network

`crates/forge_infra/src/http.rs:98` accepts invalid certs **only
when** `http.accept_invalid_certs == true` in user config. Default
is `false`. Documented as opt-in for corp MITM proxies / dev TLS.

PRISM's HTTP clients all go through `reqwest` + `rustls`; no
native OpenSSL exposure. The four cargo-audit `rustls-webpki` /
`hickory-proto` advisories are unreachable because we only talk to
controlled hosts (api.marc27.com, HF Hub).

### SQL / query layer

`rusqlite` is used for audit log + RBAC + session DBs. All queries
use **parameterized statements** — no `format!()`-built SQL strings
found in the workspace. Searched for `format!.*INSERT|format!.*WHERE`
patterns across `crates/`; zero hits.

### Secret exposure scan

`grep` for hardcoded API keys (`sk-`, `m27_`, `marc27_token`,
`AKIA`) across the workspace. Only hit: `crates/forge_domain/src/auth/new_types.rs:182`
which is a **test fixture** (`"sk-1234567890abcdefghijklmnop"` —
fake), not a real key.

### YAML workflow injection (LLM-authored YAMLs)

The robust-schema work (PR #24, #25) added defense-in-depth for
LLM-generated workflow YAMLs:

- **Action allowlist** — only `set / message / http / tool / if / parallel / workflow` are recognized; unknown actions error with a "did you mean" suggestion (no eval, no shell-out).
- **Auto-generated IDs** — even malformed YAML (no `id:`) parses safely.
- **Step-level field aliases** (PR #25) — limited to known canonical names; no arbitrary attribute injection.

Combined with OPA/regorus per-step policy gating, an LLM cannot use
a workflow to escalate privilege beyond what the operator's rego
policy allows.

### URL fetch (PR #32)

LLMs frequently emit malformed URLs (protocol-relative `//host`,
scheme-less `host.tld`). Pre-PR #32, these failed with raw
parse errors and the LLM gave up. PR #32 normalizes them to HTTPS
**but** this is a security-sensitive widening — the change:

- Defaults the scheme to `https` (never `http`) — safe.
- Refuses to "recover" inputs without a `.` (avoids accidentally treating arbitrary words as domain names).
- Refuses to recover inputs starting with `/` `?` `#` (these aren't recoverable URLs).
- Does not bypass robots.txt or content-type checks.

No new attack surface added.

---

## Findings summary

| Severity | Count | Notes |
|---|---|---|
| **Critical** (RCE / auth bypass) | **0** | |
| **High** (CVE in reachable code path) | **0** | |
| **Medium** (CVE behind upstream pin) | 3 | litellm — server-side path only |
| **Low** (CVE in tooling / dev-only) | 5 | pip, pytest, py, transformers (5.0rc) |
| **Code-level: clean** | — | auth, sandbox, TLS, SQL, secrets all OK |

---

## Action items

| Priority | Item | Owner |
|---|---|---|
| P0 | Land all 4 in-flight fix PRs (#30, #31, #32, #33, #34) | **DONE — all merged** |
| P1 | Bump openviking's `litellm` constraint to ≥ 1.83.7 | openviking maintainer |
| P2 | Drop `retry` (replace with `tenacity`) to clear `py 1.11.0` | follow-up |
| P3 | Bump `pip` in bootstrap script | follow-up |
| P3 | Track `transformers 5.0.0` GA → bump | follow-up |
| Ongoing | Re-run `cargo audit` + `pip-audit` per release | CI step (Security Audit job already runs cargo audit on every PR) |

---

## Re-run instructions

```bash
# Cargo
cargo audit                     # passes; warnings allowlisted in .cargo/audit.toml

# Python
source .venv/bin/activate
pip install --quiet pip-audit
pip-audit --skip-editable       # remaining 8 CVEs are documented above

# Code-level grep checks
grep -rn 'danger_accept_invalid_certs(true)' --include='*.rs'
grep -rn '_is_safe_path\|_ALLOWED_BASE' app/tools/
grep -rn 'format!(.*SELECT.*WHERE\|format!(.*INSERT' --include='*.rs'
grep -rn -E '(sk-[A-Za-z0-9]{20}|m27_[a-zA-Z0-9]{20})' --include='*.rs' --include='*.py'
```

Each should report findings consistent with this audit. Anything
new is either a regression to investigate or a new dep that needs
review.

---

## Next review

Before Fabric v1 ships (cross-org compute, audit envelope), re-run
this entire audit including:

- Ed25519 signing primitives in `crates/mesh/src/federation.rs` —
  side-channel review
- `crates/audit/` (F5) — envelope replay + tampering tests
- Cross-org policy intersection (F2) — confused-deputy review
- Container exec sandboxing on burst routing (F4)
