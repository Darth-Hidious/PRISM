# PRISMA Code Integrity Audit

**Repository:** `/Users/siddharthakovid/Downloads/prism-unmuzzle`<br>
**Branch:** `feat/composability-seam`<br>
**Audit window:** 2026-08-26<br>
**Starting revision:** `0bc6fcc761ae5dabd5218df712116d8fbfebd4b0`<br>
**Final revision reviewed:** `a572070e0091b2be790ca38cd937e23369e08c19`<br>
**Comparison baseline:** `private/main` at `70fd760ca54c9df90b51ce88b3c900dc6cbfdd06`<br>
**Public remote:** `origin` → `Darth-Hidious/PRISM`<br>
**Private remote:** `private` → `MirdyneLabs/prism-engine`

> Scope note: `HEAD` advanced during the audit. The working tree began at
> `0bc6fcc7` with five pre-existing untracked research artifacts. Two tracked
> files were then changed by another process, and those changes were committed as
> `c7fcfe60` while this audit was still running. `HEAD` advanced a second time to
> `a572070e` during the adversarial pass. I made no production-code changes.
> Findings and test results below identify the applicable revision.

## 1. Executive Summary

### Overall assessment: **C — Significant AI-agent-induced architectural or functional degradation**

PRISMA is not a fraudulent shell and it is not dominated by fake scientific
implementations. Substantial parts are real, carefully reasoned, and well tested.
The Python suite is large and green under its normal offline exclusions; Clippy
passes at warning-as-error; the current ingest code contains unusually explicit
guards against fabricated facts, partial featurization, lost provenance, and
false success.

The repository is nevertheless not production-trustworthy as a whole. The audit
confirmed multiple high-impact integrity failures:

- malformed configuration can be silently replaced with defaults and then
  overwritten, destroying unrelated settings;
- several user-facing commands print failure but return exit status 0, and
  `report` unconditionally claims submission even when nothing was submitted;
- free-form backend and input parsing can silently redirect a requested remote
  run to local execution or discard malformed inputs;
- marketplace synchronization intentionally overwrites local tool source without
  a backup, local-change check, content digest, or atomic write;
- remote MACE jobs and their cache keys do not bind results to the resolved
  weights and dependency environment that produced them;
- the same MACE cache key is shared by fake and real backends, so an
  agent-visible fake run can satisfy a later real/automatic request;
- the non-approval federated-query tool accepts a model-controlled URL and sends
  the user’s literal query to that host and response-supplied peers;
- automatic MACE routing selects the platform for operations the platform
  backend explicitly does not implement, without the documented fallback;
- test machinery contains real false-pass paths, and the complete Rust workspace
  gate is red at the final reviewed revision.

The current branch is 15 commits and 187 changed files ahead of `private/main`
(+18,122/−1,834). The wider repository has 821 commits, 25 workspace packages,
990 tracked files, and 357,234 Rust/Python/TypeScript lines. The
largest executable surface, `crates/cli/src/main.rs`, is about 22,000 lines; its
`main`/dispatcher has hundreds of branches and callees. That concentration is a
major cause of inconsistent validation and exit semantics.

### Sabotage conclusion

I found **technical degradation**, including one historical episode that looks
superficially sabotage-like: a large refactor replaced chunk-safe SSE parsing
with line-based JSON parsing, removed relevant tests, and silently dropped
events. It was fixed eight days later in `3e1b792d` with a candid explanation.
I did **not** find credible evidence of intentional sabotage.

The more economical explanation is a mixture of:

1. a lossy/squashed public import that obscures pre-release history;
2. very large AI-assisted commits spanning unrelated subsystems;
3. incomplete integration seams and happy-path implementations;
4. inconsistent command contracts in a giant dispatcher;
5. selective rather than whole-workspace completion gates;
6. test designs that validate source strings or helper objects instead of the
   shipped process.

Recent history repeatedly documents defects, adds honesty guards, labels fake
backends, preserves uncertainty, and discloses known red tests. Those are strong
counter-indicators to deliberate degradation.

### Validation result

| Gate | Result | Exact observation |
| --- | --- | --- |
| Python offline/default tests | Pass | 1,515 passed, 15 skipped, 4 deselected; 16 warnings; 118.22 s |
| Excluded `app/tools/tests` | Pass when called directly | 9 passed, 5 skipped |
| Rust format | **Fail** | `cargo fmt --all -- --check` reports diffs in five `crates/compute` files |
| Rust workspace, all targets | **Fail** | Final `a572070e` run: 3 failing targets / 5 failing test cases |
| Rust Clippy | Pass | `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`, 55.77 s |
| Rust dependency advisory scan | **Fail** | `RUSTSEC-2023-0071` through `rsa 0.9.10` / `jsonwebtoken 10.4.0`; seven acknowledged warnings |
| Current TUI package | Pass | 498 tests including doctest; new hidden-sidebar regression test passed |

### Confidence

- **High** confidence that material functional and architectural degradation
  exists.
- **High, but reduced after adversarial review** confidence that the evidence
  does not support deliberate sabotage (approximately 80%).
- **Moderate-to-high** confidence in the severity ranking. Some security findings
  are conditional on library embedding or attacker-controlled local files.
- **Incomplete** confidence for live platform, live Hugging Face, real MACE,
  Docker, scheduler, relay, and hardware behavior because the audit deliberately
  did not spend credentials, submit jobs, or contact real services.

## 2. Sabotage Hypothesis Assessment

### Direct answer

**Did I find evidence that the codebase was intentionally or systematically
degraded? No credible evidence of intent. I did find a recurring, systematic
engineering pattern that degrades integrity: oversized AI-assisted changes,
incomplete seams, inconsistent error contracts, and selective validation.**

“Systematic” describes the observable development failure mode; it does not
establish motive.

### Evidence that supports the hypothesis

1. **Misleading success is repeated.** `publish`, `report`, `gpus`,
   `ingest --status`, query ownership lookup, and provenance recording have
   independent paths where failure becomes success, an empty result, or a
   plausible default.
2. **A historical regression paired implementation degradation with test
   loss.** `51f5260d` replaced multiline/chunk-safe discourse SSE handling with
   line-oriented parsing and removed three high-value tests. `3e1b792d` later
   states that the intermediate behavior silently dropped every discourse
   event.
3. **Commit messages occasionally overstate completion.** `c8774b02` says a
   Poland/Germany peer is reachable by key, but the iroh transport is only
   exported and tested; no production discovery or federated-query path invokes
   it. `bbd88398` says provider tests were converted, while two CLI tests still
   expect Anthropic and fail.
4. **Tests sometimes pin text rather than behavior.** The marketplace catalog
   test counts names in source, and `c7fcfe60` added a source-string assertion
   while simultaneously violating the repository’s `no_exit_to_cli` gate.
5. **The codebase contains concealment-capable mechanisms.** Broad catches,
   `unwrap_or_default`, silent remote-wins synchronization, optional approval
   channels, and fallback routing can all hide a failure behind normal output.
6. **The MACE fake/real boundary is porous.** An agent can explicitly select
   `fake`, while backend-agnostic cache identity lets that result satisfy a later
   real request. Provenance still labels the original result fake, which makes
   this a serious cache-design defect rather than a concealed substitution.
7. **A model-controlled federated query can egress literal query text.** The
   current tool is non-approval and accepts an unconstrained dashboard URL; it
   trusts response-supplied peer addresses. This is a real exfiltration/SSRF
   mechanism, though not a demonstrated platform-credential leak.

These facts justify a serious integrity audit. They do not, alone or together,
identify a malicious actor or intent.

### Evidence against the hypothesis

1. The highest-risk configuration and false-success patterns are present in the
   July public-root import (`a56d8229`) rather than appearing as a later,
   targeted replacement of known-good code.
2. Test volume increased on the audited branch: +162 anchored Rust test
   attributes and +69 Python test definitions versus `private/main`, with no
   broad deletion of skip markers to manufacture a green dashboard.
3. Numerous recent changes explicitly refuse plausible-but-wrong science:
   incomplete element coverage no longer produces a feature vector; MACE head
   mismatches fail; fully deferred PDF ingestion fails; fake MACE provenance
   says no weights ran; uncertain HEA values remain absent rather than being
   zero-filled.
4. The repository contains candid forensic documents (`UNMUZZLE_FINDINGS.md`,
   `AUDIT_FIXES.md`), commit messages that admit the author’s own mistakes, and
   WIP branches named as incomplete. `0bc6fcc7` explicitly disclosed a red
   JSpace benchmark.
5. The severe SSE regression was detected, explained, and repaired rather than
   preserved or further obscured.
6. No selectively activated quality-reduction switch, hidden host/time trigger,
   secret backdoor, or coherent destructive objective was found. The
   adversarial pass did find query-text egress and fake-cache reuse; neither is
   covert, and both have straightforward design-error explanations.
7. The observed defects cluster around mundane pressure points: error mapping,
   CLI dispatch, local file lifecycle, dependency features, subprocesses, and
   incomplete integration. They do not cluster around a coherent destructive
   objective.

### Simpler explanations

For every sabotage-consistent issue, a simpler explanation fits better:

| Pattern | Simpler explanation |
| --- | --- |
| Success after printed failure | Human/agent optimized interactive messaging but did not preserve process exit contracts |
| Remote request silently becomes local | Free-form string plus catch-all match added before enum validation |
| Config loss | “Defaults are safe” defensive pattern applied to a write path |
| Source-string tests | Fast regression pinning without constructing the real surface |
| Orphaned iroh module | Feature branch landed transport before integration |
| SSE regression plus test deletion | 746-file consolidation/refactor with insufficient behavioral preservation |
| Huge dependency additions | Cargo feature unification and convenience defaults misunderstood |
| Red full workspace despite selective green suites | Completion checked package-local gates rather than `--workspace --all-targets` |

### Unresolved questions

- The public history begins with a very large squashed release and hidden/original
  refs exist locally. Which pre-public instructions produced the initial
  false-success and configuration designs cannot be reconstructed from the
  visible branch alone.
- `tool_sync` says “remote wins silently (per design decision),” but the actual
  decision record was not found. It should be reviewed by the person who owns
  user-data policy.
- The intended trust boundary for library consumers of `prism-agent` is not
  explicit. If third parties can call an agent turn without an approval
  receiver, F-010 becomes more severe.
- The remote MACE deployment contract may pin container images or models outside
  this repository. If so, that evidence must be brought into the provenance and
  cache key; it is not presently visible here.

### Confidence

**High confidence (approximately 80%) that deliberate sabotage is unsupported.**
There is high confidence in systematic *sloppiness and architectural drift*, but
low confidence that motive can be inferred from repository artifacts.

## 3. Critical Findings

No finding met the **Critical** threshold: I did not demonstrate platform-token
exfiltration, unauthenticated remote code execution in the shipped default
surface, or irreversible platform data loss. The adversarial pass did demonstrate
query-text egress and a fake-to-real cache collision; both are High below. The
High findings require remediation before treating the CLI as
production-reliable. Detailed first-pass findings later downgraded to Medium
remain in this section for audit transparency and are explicitly labelled.

### F-001 — `configure` can destroy a malformed but valuable configuration

**Severity:** High<br>
**Evidence strength:** Confirmed<br>
**File(s):** `crates/cli/src/main.rs`<br>
**Exact line(s) or symbol(s):** `handle_configure`, especially current lines
6331–6333 and 6393–6396<br>
**Commit(s) introducing the behavior:** present in public-root release
`a56d8229`; no earlier public predecessor is available<br>
**Previous behavior:** no correct behavior is visible in public history; before
the write, the malformed file still contains the user’s settings<br>
**Current behavior:** `NodeConfig::from_file(...).unwrap_or_default()` turns any
read/TOML/schema failure into a complete default configuration. A subsequent
single-field edit serializes and writes that default object over the original
file with `std::fs::write`.

**Why it is wrong or suspicious:** A read error is not evidence that the user
wants every unrelated setting reset. This is the characteristic
“defensive-default on a destructive write path” failure: it appears robust but
converts recoverable syntax damage into silent data loss. The write is also
non-atomic and has no backup.

**Reproduction method:**

1. Create a temporary `HOME/.prism/prism.toml` containing useful `platform` and
   `llm` values plus one TOML syntax error.
2. Run `prism configure --show`: it exits 0 and prints default LlamaCPP values.
3. Run `prism configure --model replacement`.
4. Inspect the file: unrelated platform/LLM values are gone and a default config
   has replaced them.

**Impact:** Silent loss of endpoint, model, provider, and other configuration;
commands may subsequently run against defaults or the wrong backend.

**Likely explanation:** defensive programming gone wrong; normal bug; LLM
happy-path completion. A deliberate explanation is unnecessary.

**Recommended remediation:** Refuse all writes when the existing file cannot be
read and parsed; report the parse location; write to a same-directory temporary
file, `fsync` as appropriate, then rename atomically; preserve a dated backup;
add a regression test proving malformed input is byte-for-byte unchanged.

### F-002 — Invalid global or project configuration is silently ignored

**Severity:** Medium (downgraded from High in the adversarial pass)<br>
**Evidence strength:** Confirmed<br>
**File(s):** `crates/core/src/config.rs`<br>
**Exact line(s) or symbol(s):** `NodeConfig::load`, current lines 588–614;
particularly the two `if let Ok(...)` guards<br>
**Commit(s) introducing the behavior:** present in `a56d8229`<br>
**Previous behavior:** no earlier public implementation is available<br>
**Current behavior:** an unreadable or invalid global/project TOML file is
ignored without a user-visible error, and resolution continues with defaults or
the other file.

**Adversarial correction:** The first pass incorrectly treated whole-file
project replacement as a defect. `config.rs` explicitly says “simple: just
replace,” and `main.rs` test setup around 20156–20163 states that project config
**REPLACES** global config. No user-facing document promising field-wise overlay
was found. Replacement is therefore an intentional precedence contract; only
silent invalid-file handling is retained as a finding.

**Why it is wrong or suspicious:** A malformed file is materially different
from an absent file. Quietly ignoring it makes typos look like missing
credentials, disabled services, or default model selection.

**Reproduction method:** Put invalid TOML in either search location and call
`NodeConfig::load` through a configuration consumer. Resolution returns normally
with defaults/the other file and produces no user-visible diagnostic.

**Impact:** Wrong service/model selection and difficult-to-diagnose configuration
drift; unlike F-001, this path does not itself overwrite the bad file.

**Likely explanation:** defensive-default behavior and an API that cannot return
diagnostics; normal bug.

**Recommended remediation:** Return a `Result` or diagnostic bundle, distinguish
absent from invalid, surface the exact path/parse location, and test invalid
global/project cases. Preserve whole-file project precedence unless the product
owner deliberately changes that documented/tested contract.

### F-003 — Human-mode Hugging Face publishing reports failure but exits successfully

**Severity:** High<br>
**Evidence strength:** Confirmed<br>
**File(s):** `crates/cli/src/main.rs`<br>
**Exact line(s) or symbol(s):** `Commands::Publish` Hugging Face arm, current
lines approximately 4988–5120<br>
**Commit(s) introducing the behavior:** present in `a56d8229`; later offline and
JSON guards improved adjacent paths but retained the human-mode contract<br>
**Previous behavior:** no correct public predecessor; JSON mode currently
propagates the same failures<br>
**Current behavior:** when `hf repo create` or `hf upload` exits non-zero or
cannot spawn, human mode prints `Upload failed`/installation instructions and
falls out of the match with `Ok(())`. JSON mode returns an error.

**Why it is wrong or suspicious:** Process exit status is part of the CLI API.
Humans and automation invoking the same operation receive incompatible truth
values. Printed stderr does not make a failed publish successful.

**Reproduction method:** Place a fake `hf` earlier on `PATH` that exits 1, then
run a human-mode publish of a temporary existing file. Observe failure text and
shell exit status 0. Repeat with `--json` and observe non-zero.

**Impact:** CI/release scripts can mark unpublished models or artifacts as
released; downstream links and provenance may refer to work that never left the
machine.

**Likely explanation:** normal bug; interactive UX implemented separately from
structured error propagation.

**Recommended remediation:** Return an error on every create/upload failure in
all output modes. Keep friendly stderr as context, but make the exit contract
identical. Add fake-executable tests for spawn error, create failure, upload
failure, and success.

### F-004 — `report` unconditionally claims submission even when every submission failed

**Severity:** Medium (downgraded from High in the adversarial pass)<br>
**Evidence strength:** Confirmed<br>
**File(s):** `crates/cli/src/main.rs`<br>
**Exact line(s) or symbol(s):** `handle_report`, current lines 15816–15977;
unconditional success text at line 15976<br>
**Commit(s) introducing the behavior:** reporting path present in `a56d8229` /
`d5ee42b6` lineage; wording evolved in `ea5f55af` and `d07fac0a` without
fixing the contract<br>
**Previous behavior:** no correct public predecessor identified<br>
**Current behavior:** a failed `gh issue create` prints an error and continues.
With no usable platform credential, the platform step is skipped. The function
then prints “Report submitted. We’ll follow up on GitHub and your platform
dashboard.” and returns `Ok(())`.

**Why it is wrong or suspicious:** This is a direct false factual statement. It
is especially harmful for a bug-report command because the user may discard the
only copy of diagnostic context believing it was filed.

**Reproduction method:** Use a fake `gh` executable that exits 1 and a temporary
home with no credentials. Run `prism report <description>`. The observed output
contains `failed` followed by `Report submitted`; exit status is 0; neither
destination received a report.

**Impact:** Lost support reports, false assurance, and automation that cannot
detect filing failure.

**Likely explanation:** incomplete aggregation of two optional destinations;
normal bug. The unconditional prose is stronger than an ordinary missing check
but still has an obvious implementation explanation.

**Recommended remediation:** Track per-destination outcomes. Return non-zero if
the user requested a destination and none succeeded; print “report saved
locally/not submitted” with the body path; never mention a platform dashboard
unless a platform ticket was actually created.

### F-005 — Unknown `run` backends become local runs and malformed inputs disappear

**Severity:** High<br>
**Evidence strength:** Strong evidence (static path fully traced; no Docker job
was submitted during the audit)<br>
**File(s):** `crates/cli/src/main.rs`<br>
**Exact line(s) or symbol(s):** `RunArgs.backend` (free-form `String`);
`validate_run_backend_target`; `handle_run` input loop around 15496–15506; backend
match catch-all around 15603–15670<br>
**Commit(s) introducing the behavior:** present in `a56d8229`; HyperQueue/BYOC
validation added later but does not validate the complete backend enum<br>
**Previous behavior:** no earlier public correct implementation; documented
values are `local`, `marc27/platform`, `byoc`, and `hyperqueue/hq`<br>
**Current behavior:** any spelling not matched by the remote/HQ arms reaches `_`
and constructs a local compute router. Inputs without `=` are silently omitted.
Target flags are evaluated before the backend string: `--backend local --ssh
host`, `--backend platform --slurm host`, and a typo plus
`--k8s-context` silently become BYOC. `validate_run_backend_target` only rejects
the inverse case (`byoc` with no target); HyperQueue alone has a conflict check.

**Why it is wrong or suspicious:** `--backend platfrom` is not a harmless
default request; it can execute a container locally when the user intended a
controlled remote system. A malformed scientific parameter can disappear while
the job still runs with defaults.

**Reproduction method:** Argument parsing accepts `--backend platfrom`. Static
dispatch resolves it to target `{"kind":"local"}`. The target-first cases above
resolve to BYOC regardless of the explicit backend. Likewise `--input
temperature` contributes no key to `inputs_json` and raises no error. A live
submit was not attempted because it would invoke Docker/remote compute.

**Impact:** Wrong execution environment, unexpected local resource/secret
exposure, and scientifically invalid runs with missing parameters.

**Likely explanation:** incomplete validation and an overly broad catch-all;
ordinary AI-agent sloppiness.

**Recommended remediation:** Replace the string/flag precedence with one typed,
mutually exclusive backend-target matrix; a `ValueEnum` alone is insufficient.
Reject every input lacking a non-empty key and `=`; test misspellings and all
backend/target contradictions without invoking a backend.

### F-006 — Contradictory ingest modes are accepted and can upload data despite `--schema-only`

**Severity:** High<br>
**Evidence strength:** Confirmed for dispatch and validation; real upload
deliberately not executed<br>
**File(s):** `crates/cli/src/main.rs`<br>
**Exact line(s) or symbol(s):** `Commands::Ingest` dispatch, current lines
4180–4290; `handle_ingest_platform`, current lines 9608–9665<br>
**Commit(s) introducing the behavior:** platform upload added in `ca0d6d43`;
repair conflict checks added in `5b665387` but did not generalize to all modes<br>
**Previous behavior:** local `--schema-only` builds/prints schema without model
extraction; `--platform` uploads and performs holistic extraction<br>
**Current behavior:** precedence is `repair → status → platform → watch → local`.
Only `repair` rejects conflicting flags. `--platform --watch --schema-only` is
accepted, selects platform, discards `watch` and `schema_only`, reads the entire
PDF, and constructs an authenticated upload.

**Why it is wrong or suspicious:** A flag explicitly requesting no extraction
and no document processing must not be silently discarded in favor of a costly,
privacy-relevant upload.

**Reproduction method:** `prism ingest Cargo.toml --platform --watch
--schema-only` parses successfully and reaches the platform arm; the local magic
check then fails only because Cargo.toml is not a PDF. With a real PDF, static
tracing shows full-body upload and no schema-only branch.

**Impact:** Unexpected document disclosure, platform cost, and operation
different from the user’s explicit request.

**Likely explanation:** requirement misunderstanding/incomplete mode matrix;
normal bug.

**Recommended remediation:** Model ingest mode as a mutually exclusive enum or
Clap group. Reject `--platform` with `--watch`/`--schema-only` unless a real
platform schema-only endpoint exists. Add parse/dispatch matrix tests.

### F-007 — Automatic marketplace synchronization can silently destroy local tool code

**Severity:** High<br>
**Evidence strength:** Strong evidence<br>
**File(s):** `crates/cli/src/tool_sync.rs` and the explicit marketplace install
arm in `crates/cli/src/main.rs`<br>
**Exact line(s) or symbol(s):** module contract lines 1–8; `load_manifest`
74–81; `sync_tools` 120–230; writes around 208–230; `save_manifest` 83–91;
explicit install refusal near `main.rs` 4537–4549<br>
**Commit(s) introducing the behavior:** present in `a56d8229`; later startup
integration retained “remote wins silently” as intentional behavior<br>
**Previous behavior:** explicit marketplace installation refuses to overwrite
an existing destination; a locally edited tool remains under user control<br>
**Current behavior:** startup/full sync compares only remote version to a
manifest. A version change overwrites `~/.prism/tools/<slug>.py` without hashing
the existing file, asking, backing it up, or writing atomically. Manifest parse
errors become an empty manifest; response-body decoding errors become an empty
string that can be written as the tool.

**Why it is wrong or suspicious:** This is an explicitly destructive policy
hidden in an automatic maintenance path, and it contradicts the safety posture
of explicit install. A local tool is user-authored executable source, not a
disposable cache.

**Reproduction method:** With a fake marketplace client, install version 1,
modify the resulting Python file locally, return version 2 and new/empty body,
then call `sync_tools`. Static and unit-level flow writes the response over the
local edit and updates the manifest.

**Impact:** User code loss, deployment drift, and potentially a zero-length or
unreviewed executable tool appearing during TUI/backend startup.

**Likely explanation:** an explicit but unsafe product decision; architectural
drift. No hidden malicious mechanism is required.

**Recommended remediation:** Separate immutable marketplace cache from a
user-editable tool directory; record content hashes and signatures; refuse or
stage conflicts; keep recoverable backups; validate non-empty Python source;
use atomic writes; make synchronization opt-in or visibly report changes.

### F-008 — Notebook registry failures can erase state and process control is unsafe

**Severity:** Medium (downgraded from High in the adversarial pass)<br>
**Evidence strength:** Strong evidence<br>
**File(s):** `crates/cli/src/notebook.rs`<br>
**Exact line(s) or symbol(s):** registry path/load lines 24–42; `save_registry`
61–70; liveness lines 74–87; `handle_stop` lines 185–210<br>
**Commit(s) introducing the behavior:** present in `a56d8229`<br>
**Previous behavior:** no correct public predecessor; a valid registry is the
only durable link between names, ports, URLs, and process IDs<br>
**Current behavior:** home/read/JSON errors become an empty vector. `list` then
persists that vector, potentially replacing corrupt but recoverable state with
`[]`. Liveness uses Unix `kill(pid, 0)` only. Stop ignores failure to spawn or
the exit status of `kill` and removes the registry entry regardless.

**Why it is wrong or suspicious:** The registry controls live processes. PID
reuse can target an unrelated process; a failed kill can leave a notebook
running after PRISMA has forgotten it; corruption is converted into destructive
normalization.

**Reproduction method:** Place invalid JSON in the registry and invoke list;
trace shows default-empty followed by save. For stop, use a stale/unowned PID:
the kill result is discarded and the entry is removed.

**Impact:** Orphaned servers, accidental signaling of an unrelated process,
lost recovery information, and misleading “stopped” state.

**Likely explanation:** normal Unix lifecycle bug plus defensive-default
handling.

**Recommended remediation:** Return parse errors without rewriting; store
process start-time/nonce and verify identity before signaling; propagate kill
status; remove only after confirmed exit; write registry atomically and retain a
backup.

### F-009 — Remote MACE results and cache identity are not bound to the computation environment

**Severity:** High<br>
**Evidence strength:** Strong evidence<br>
**File(s):** `app/tools/simulation/mace/backends/hf_jobs.py`,
`app/tools/simulation/mace/payloads/_common.py`, all five payload scripts,
`app/tools/simulation/mace/cache/hashing.py`,
`app/tools/simulation/mace/core/calculator.py`, and MACE provenance helpers<br>
**Exact line(s) or symbol(s):** HF launch materialization around
`hf_jobs.py` 120–160 and result assembly 194–203; dynamic install at
`_common.py` 69–88; `cache_key` 71–91; `make_calculator` 154–166;
`calc_signature` 169–184<br>
**Commit(s) introducing the behavior:** remote MACE architecture present in
`a56d8229`; `0bc6fcc7` improved fake-mode honesty but did not close this remote
identity gap<br>
**Previous behavior:** local provenance collection observes the local host and
installed versions; remote execution should identify the remote image, package
set, recipe, and exact model weights<br>
**Current behavior:** payloads use floating PEP 723 dependencies and can run
`pip install mace-mcp` (or an environment-provided URL). Model download omits an
explicit Hugging Face revision. `calc_signature` records repository and
filename, not a resolved revision or file digest. Remote results omit the
runtime/package/weight digests. The cache key binds tool version, structure,
head, parameters, and two Git SHAs, but not dependency versions, container
image, model revision, or weight hash.

**Why it is wrong or suspicious:** Two physically different computations can
produce the same cache key and superficially similar provenance. A cached
result can therefore be reused after weights or dependencies move, while local
host metadata is mistaken for the remote environment.

**Reproduction method:** Static equivalence: hold all `cache_key` arguments
constant while changing the resolved Hugging Face model revision, wheel
versions, or `MACE_MCP_DEV_INSTALL_URL`. The key is unchanged by construction,
yet forces/energies may change.

**Impact:** Non-reproducible scientific results, stale-cache reuse across model
changes, and provenance that cannot identify the actual calculation.

**Likely explanation:** incomplete provenance model and convenience-driven
dependency management; not evidence of fake physics.

**Recommended remediation:** Pin container digest and every remote dependency;
pin model revision; compute and record weight SHA-256; return remote environment
manifest and recipe digest; include those identities in the cache key; refuse
cache reads made under a different calculator signature.

### F-010 — Missing approval receiver is treated as approval

**Severity:** Medium (conditional on embedding/custom call sites; downgraded
after confirming standard shipped callers supply a receiver)<br>
**Evidence strength:** Strong evidence<br>
**File(s):** `crates/agent/src/agent_loop.rs` and approval-gated Python/bash tool
definitions<br>
**Exact line(s) or symbol(s):** approval helper around lines 62–91; tool gate
around lines 3760–3805; bash approval metadata in `app/tools/bash.py` and code
approval metadata in `app/tools/code.py`<br>
**Commit(s) introducing the behavior:** legacy behavior predates/was carried
through `2ab08b73`; standard service/protocol callers now supply a receiver<br>
**Previous behavior:** where a receiver exists, gated tools wait for an explicit
decision; there is no demonstrated earlier fail-closed library default<br>
**Current behavior:** if `approval_rx` is `None`, the helper returns
`ApprovalDecision::Proceed`. Standard shipped service and protocol paths pass a
receiver, but the public/internal agent API permits absent receivers.

**Why it is wrong or suspicious:** Security policy should not weaken when a UI
channel is missing. A custom embedding, test harness promoted to production, or
future call site can execute shell/code tools without approval merely by
omitting the receiver.

**Reproduction method:** Construct an agent turn with an approval-gated tool and
`approval_rx = None`; the pure branch returns `Proceed` before waiting for a
decision. No dangerous command was run during this audit.

**Impact:** Conditional arbitrary local command/code execution without the
operator confirmation promised by tool metadata.

**Likely explanation:** legacy compatibility and avoidance of deadlock in
headless contexts; defensive programming gone wrong.

**Recommended remediation:** Default to deny when a gated tool has no approval
channel. Require an explicit, narrowly named policy capability for trusted
headless operation; make it impossible to confuse “channel unavailable” with
“approved”; test every construction path.

### F-011 — Unvalidated model identifiers reach path construction and pickle-based loading

**Severity:** Medium (conditional local-code-execution primitive; downgraded
because exploitation requires a pre-existing malicious artifact)<br>
**Evidence strength:** Strong evidence<br>
**File(s):** `app/tools/ml/registry.py`, `app/tools/ml/predictor.py`,
`app/tools/prediction.py`<br>
**Exact line(s) or symbol(s):** `ModelRegistry.save_model/load_model` lines
29–72; `Predictor.predict` lines 60–90; prediction tool schemas/registration
around `app/tools/prediction.py` 335–366 and 483–501<br>
**Commit(s) introducing the behavior:** present in `a56d8229`<br>
**Previous behavior:** no earlier safe public implementation; the code comment
assumes only locally trained models enter the directory<br>
**Current behavior:** `property_name` and `algorithm` are concatenated directly
into `models_dir / f"{property_name}_{algorithm}.joblib"`. The tool schema does
not constrain `property_name` and registration does not require approval.
`joblib.load` is pickle-based and executes the selected artifact before formula
validation.

**Why it is wrong or suspicious:** The safety comment is an unenforced
assumption. Path components can escape the intended filename/directory on
platforms and layouts where separators are accepted. If an attacker can place
or point to a malicious joblib artifact, invoking prediction can deserialize it.

**Reproduction method:** Static dataflow from tool input → `Predictor.predict` →
`ModelRegistry.load_model` → constructed path → `joblib.load`. A live exploit
was not attempted; the standalone system Python available to one audit worker
did not have `joblib` installed.

**Impact:** Local arbitrary-code execution when combined with a reachable
malicious pickle, shared writable model directory, or crafted identifier.

**Likely explanation:** normal unsafe-deserialization/path-validation bug. The
comment demonstrates an assumed trust model, not malicious intent.

**Recommended remediation:** Permit only canonical property/algorithm
identifiers; resolve and verify containment under `models_dir`; use a
non-executable model format where possible; sign/hash artifacts; load only
registry-enumerated files; consider approval for training/model-loading tools.

### F-012 — The TUI “E2E” tests can report failed checks while Pytest marks them passed

**Severity:** High<br>
**Evidence strength:** Confirmed<br>
**File(s):** `tests/test_tui_e2e.py` and `pyproject.toml`<br>
**Exact line(s) or symbol(s):** `TUITestReport.success` and `summary` around
168–180; seven non-`None` test returns at lines 215, 254, 291, 343, 366, 379,
and 417; a separate always-true `or True` assertion at 207; module-level binary
skip around 46–49<br>
**Commit(s) introducing the behavior:** introduced in hidden/pre-public lineage
and carried into `a56d8229`; `d8c4e4ba` documented the issue but
did not convert returns to assertions<br>
**Previous behavior:** the module’s direct-script runner consumes the returned
boolean correctly<br>
**Current behavior:** seven functions named `test_*` return booleans or literal
`True`. Pytest ignores non-`None` return values and emits
`PytestReturnNotNoneWarning`; a report whose internal checks failed can still be
counted as a passing test. The whole module skips when the expected release
binary is absent.

**Why it is wrong or suspicious:** This is a concrete test-gaming *effect*
without evidence of gaming intent. The suite can be green while its own
semantic report says failure.

**Reproduction method:** Run the normal Pytest command. It reports 1,515 passes
and emits warnings naming these functions because they returned non-`None`.
Replace/force a report result to `False`: Pytest still treats the return itself
as success unless an assertion raises.

**Impact:** Broken TUI behavior can ship behind a green suite; this is
particularly material because `c7fcfe60` documents a real terminal regression
that 496 object-level tests missed.

**Likely explanation:** dual-use direct-script/Pytest design mistake; ordinary
AI-assisted test sloppiness.

**Recommended remediation:** Make every `test_*` assert the report result and
return `None`; keep direct-script orchestration in separately named helpers;
build the binary in the test fixture or fail explicitly when the intended
artifact is missing; add a real PTY/tmux smoke gate.

### F-013 — The complete Rust gate is red, while recent commits cite selective green suites

**Severity:** Medium release/process blocker (downgraded after the adversarial
pass; the underlying gate failure remains confirmed)<br>
**Evidence strength:** Confirmed<br>
**File(s):** `crates/agent/tests/jspace_benchmark.rs`,
`crates/agent/tests/fixtures/jspace_candidates.json`,
`crates/cli/src/use_command.rs`, `crates/cli/src/boot_checks.rs`,
`crates/server/tests/no_exit_to_cli.rs`, and commit/gate documentation<br>
**Exact line(s) or symbol(s):** JSpace candidate assertions around lines 210 and
253; Anthropic expectations around `use_command.rs` 560–584 and 709–735;
new exit-to-CLI strings at `boot_checks.rs` 415 and 730; guard assertion at
`no_exit_to_cli.rs` 88<br>
**Commit(s) introducing the behavior:** `b6699271` collapsed tool names;
`0bc6fcc7` partially updated fixtures and explicitly left JSpace red;
`bbd88398` removed the bundled Anthropic provider but missed two tests;
`c7fcfe60` added the `prism login` wording and source assertion<br>
**Previous behavior:** the tests matched the prior tool/provider surface; the
exit-to-CLI guard passed before `c7fcfe60`<br>
**Current behavior:** at the `0bc6fcc7` file contents, full
`cargo test --workspace --all-targets --locked --offline --no-fail-fast`
failed four tests: two JSpace candidate expectations and two CLI provider
expectations. At final `a572070e`, a complete rerun failed exactly five test
cases across three targets: the same four plus the server gate, because both
production and test strings instruct the user to run a CLI command. The TUI
package itself is green at 498 tests including doctest.

**Why it is wrong or suspicious:** The branch is not merge-gate clean. Package
or TUI-local counts in commit messages cannot substitute for a full workspace
run. The newest commit’s test asserts the very source string that a cross-cutting
policy test prohibits.

**Reproduction method:**

- `cargo test --workspace --all-targets --locked --offline --no-fail-fast`
- `cargo test -p prism-server --test no_exit_to_cli --locked --offline -- --nocapture`

The latter fails at final HEAD with both `boot_checks.rs:415` and
`boot_checks.rs:730`.

**Impact:** Known contract drift, unreliable completion claims, and inability to
use the repository’s own full test gate as a release signal.

**Likely explanation:** incomplete fixture/test updates and selective completion
criteria. `0bc6fcc7` candidly disclosed part of the red state, which argues
against concealment.

**Recommended remediation:** Make the full locked/offline workspace/all-targets
command mandatory before merge; repair product behavior first, then align
semantic fixtures; remove source-self-assertions; report exact commands and
failures in commit messages.

### F-014 — `prism tui --fake-backend` is not hermetic despite promising no subprocess or network work

**Severity:** Medium (downgraded from High in the adversarial pass)<br>
**Evidence strength:** Strong evidence<br>
**File(s):** `crates/cli/src/main.rs` and Python-tool synchronization/setup paths<br>
**Exact line(s) or symbol(s):** fake-backend help around lines 203–216;
`command_needs_python`/Python resolution around 1795–1970; signed-in tool sync
around 1858–1874 and 2001–2019; fake short-circuit around 5184–5197<br>
**Commit(s) introducing the behavior:** fake-TUI seam predates the audited branch;
startup provisioning/synchronization evolved around it without an early guard<br>
**Previous behavior:** documented fake mode promises deterministic fixtures with
no subprocess, network, or LLM<br>
**Current behavior:** global Python resolution/provisioning and signed-in tool
synchronization occur before dispatch reaches the fake-TUI short-circuit.
Those paths can spawn Python/package setup and contact the marketplace.

**Why it is wrong or suspicious:** A test seam that performs real setup or
network work can mutate state, hang offline tests, and make “deterministic” runs
environment-dependent.

**Reproduction method:** Static startup order demonstrates that pre-dispatch
setup precedes the fake branch. A destructive provisioning/network reproduction
was not performed. Use a future fake client/process recorder to assert zero
external effects before the short-circuit.

**Impact:** Non-hermetic tests, unexpected startup side effects, and potential
credential/network use from a mode explicitly advertised as fake.

**Likely explanation:** architectural drift from adding global startup work
after the fake seam; not a hidden fake-production path.

**Recommended remediation:** Parse and short-circuit fake mode before all
provisioning, sync, auth, and project-state initialization; enforce the contract
with process/network-deny tests and a temporary empty home.

### F-015 — Fake MACE results can satisfy later real or automatic requests

**Severity:** High<br>
**Evidence strength:** Confirmed by isolated cache construction<br>
**File(s):** `app/tools/mace.py`,
`app/tools/simulation/mace/primitives.py`,
`app/tools/simulation/mace/cache/hashing.py`,
`app/tools/simulation/mace/cache/store.py`,
`app/tools/simulation/mace/jobs/runner.py`,
`app/tools/simulation/mace/control.py`, and MACE schemas/provenance<br>
**Exact line(s) or symbol(s):** agent-facing backend enum at `mace.py` 414–430;
primitive key-before-selection at `primitives.py` 47–80 (and sibling
primitives); `cache_key` 71–91; cache store 3–14/28–54; cache-hit shortcut
`runner.py` 59–105; dropped resolver provenance at `control.py` 185–202 and
`schemas.py` 402–409<br>
**Commit(s) introducing the behavior:** hidden/pre-public
`f62e31edb8beed7ae8f78d1f834212730e9b3484`, carried unchanged into
`a56d8229`<br>
**Previous behavior:** no backend-namespaced cache design was found; a sibling
patent cache demonstrates the expected namespace-aware pattern<br>
**Current behavior:** `fake` is available in the production agent schema and via
`MACE_MCP_BACKEND`. All backends compute the same key because backend/model
identity is absent. `JobRunner.submit` accepts any `result.json` by key before
executing the selected backend and records the new job backend as `cache`.

**Why it is wrong or suspicious:** A deterministic fake energy/structure is not
interchangeable with a MACE calculation. Although the stored provenance honestly
says `backend=fake` and no weights ran, ordinary job success/cache fields do not
name the original backend, and the downstream cached-structure resolver drops
the provenance despite promising a bundle path.

**Reproduction method:** In a temporary cache, seed a fake result/provenance
under a normal primitive key, then call `JobRunner.submit` with the same inputs
and `backend_name="local"`. The audit returned
`requested_backend=local`, `recorded_job_backend=cache`,
`cache_hit=true`, and the seeded energy −3.14159; dereferenced provenance still
said fake/no weights. Default `auto` is equally reachable after any fake seed.

**Impact:** Plausible fake numbers and fake-derived structures can enter later
real scientific workflows without executing weights; downstream provenance can
be lost.

**Likely explanation:** uniform cache-key design error and a test seam leaking
into the agent-facing surface. Explicit fake labels and retained primary
provenance are strong evidence against deliberate concealment.

**Recommended remediation:** Remove `fake` from production schemas/builds;
namespace cache by backend class and resolved calculator/environment identity;
refuse fake entries for non-fake requests; carry provenance through every
resolver; add a cross-backend cache-isolation regression test.

### F-016 — Non-approval federated query can exfiltrate query text and perform SSRF

**Severity:** High (privacy/SSRF; no current platform-token exfiltration
demonstrated)<br>
**Evidence strength:** Confirmed with a safe loopback recorder<br>
**File(s):** `crates/agent/src/command_tools.rs`,
`crates/cli/src/main.rs`, `crates/runtime/src/offline.rs`<br>
**Exact line(s) or symbol(s):** offered query tool at `command_tools.rs`
417–435; unconstrained `dashboard_url` 1783–1787; verbatim argument building
3967–4001/4717–4734; `handle_federated_query` at `main.rs` 15240–15356;
`offline::check_url` 129–137<br>
**Commit(s) introducing the behavior:** squashed current-lineage root
`a56d8229`; non-ancestor pre-squash objects `21a051a6` (CLI) and `45586a08`
(agent surface). `d07fac0a` removed platform-token exfiltration;
`cf2988c4` added hard-offline gates but no online host restriction.<br>
**Previous behavior:** earlier paths were worse because a platform credential
could reach a non-loopback host; that part is fixed<br>
**Current behavior:** the model-visible `query` tool is `ReadOnly`,
`requires_approval:false`, and accepts a free-form dashboard URL. Online,
`check_url` is a no-op. The handler GETs the chosen host’s peer list, then POSTs
the literal query to that host and every response-supplied address/port over
HTTP. No peer identity/allowlist binds those destinations.

**Why it is wrong or suspicious:** A prompt-injected URL can make a read-labelled
tool disclose private query text and probe internal, link-local, or
cloud-metadata destinations. A hostile first host chooses second-order targets.

**Reproduction method:** A temporary recorder at `127.0.0.1:18765` returned one
peer. Running `prism query 'PRIVATE QUERY CONTENT 7e31' --federated
--dashboard-url http://127.0.0.1:18765` exited 0. The recorder observed GET
`/api/mesh/nodes`, POST `/api/query` with the sentinel literal, POST
`/api/sessions` with synthetic peer identity, and a second query POST.

**Impact:** User-query/limited identity-metadata disclosure and online SSRF.
No `Authorization` header was observed; non-loopback session creation strips
the stored platform token. A destination-issued session token is not a local
secret.

**Likely explanation:** “read-only” was interpreted as non-mutating, while
network egress and data disclosure were not included in the permission model.
The source comments openly describe the prompt-injection risk, making accidental
incomplete hardening much more plausible than sabotage.

**Recommended remediation:** Restrict unattended dashboard URLs to loopback;
remove arbitrary URLs from the model schema or require explicit approval and an
allowlist; authenticate and identity-bind discovered peers; block private,
link-local, and metadata targets where appropriate; require TLS for remote
peers; reject non-success responses before parsing.

### F-017 — Automatic MACE routing selects platform operations the backend cannot execute

**Severity:** High<br>
**Evidence strength:** Strong evidence (static control flow; live platform job
not submitted)<br>
**File(s):** `app/tools/simulation/mace/backends/base.py`,
`app/tools/simulation/mace/backends/platform.py`,
`app/tools/simulation/mace/jobs/runner.py`<br>
**Exact line(s) or symbol(s):** `select_backend` 103–118; platform capability
table 69–78 and unsupported branches 118–127; single-backend execution
`_run_one` 163–204<br>
**Commit(s) introducing the behavior:** hidden/pre-public
`963abfd3d068542a125a7b2190f67d9f09235d06`, carried into the public lineage<br>
**Previous behavior:** no working automatic fallback implementation was found<br>
**Current behavior:** when `PRISM_PROJECT_ID` is present, `auto` prefers
`platform` for every GPU-bound tool, including elastic, phonon, and dilute
operations. `PlatformBackend` supports only relax and MD and raises
`NotImplementedError` for the others. Its module prose says the runner falls
through; `JobRunner` executes exactly one selected backend and marks failure.

**Why it is wrong or suspicious:** Signed-in/default users are routed away from
available local/HF backends into a known unsupported implementation. The clean
backend abstraction and “falls through” comment make the missing retry easy to
miss.

**Reproduction method:** With platform configuration present, call
`select_backend` for elastic/phonon/dilute under `auto` and observe platform
selection; trace `JobRunner._run_one` to the explicit
`NotImplementedError`. No paid/live job was sent.

**Impact:** Default scientific operations fail for configured users despite an
available capable backend; time is lost and automation can misdiagnose platform
availability.

**Likely explanation:** selector and backend capability tables were designed
separately; the explicit error/TODO is transparent and argues against intent.

**Recommended remediation:** Make backend capability a typed predicate used by
selection; choose only capable backends; either implement an explicit,
auditable fallback policy or remove the false fallback claim; test every
primitive × backend × `auto` configuration.

## 4. Suspicious Git History

The visible history is unusual but explainable. The public root is a squashed
release; subsequent consolidation commits mix removals, architecture, tests, and
behavior. A dagger (†) marks changes where implementation and tests changed
together and therefore deserved additional scrutiny.

| Date | Commit | Files / size | Change | Why notable | Risk |
| --- | --- | ---: | --- | --- | --- |
| 2026-07-03 | `a56d8229` | 1,381; +312,169 | First stable public release † | Lossy root import includes 724 vendored Forge files (~149k lines) and most initial designs; no visible predecessor for many defects | Medium forensic risk; not evidence of a malicious import |
| 2026-07-05 | `51f5260d` | 808; +5,192/−163,290 | “research modules + deploy path” † | Actual diff removes 24 Forge crates/746 files and consolidates the TUI; commit title understates scope; introduces the SSE regression below | High |
| 2026-07-13 | `3e1b792d` | focused | Repairs double-encoded/chunked discourse SSE † | Commit explanation confirms intermediate code dropped events; restoration is evidence of error correction | Positive, historical |
| 2026-07-27 | `68b4bb16` | focused tool/science changes † | “adversarial-review round — 8 defects, one of them mine” | Candidly closes partial-feature/fake-number defects; strong evidence against sabotage | Positive |
| 2026-07-27 | `d8c4e4ba` | test/docs | Documents TUI-E2E test issue | Identifies cause but leaves Pytest-return false-pass design intact | Medium |
| 2026-08-17 | `9e1f2943` | 168; +32,474/−11,651 | Ingest/agent/TUI “reliability, reachability and honesty” checkpoint † | Large unrelated checkpoint; many new audit documents; difficult to review atomically, but net test coverage rises | High auditability risk |
| 2026-08-18 | `167ebb8d` | focused | Makes unreadable graph visible | Fixes some sibling empty-on-error paths but leaves ownership search swallowing store/query errors | Positive but incomplete |
| 2026-08-24 | `ebdded66` | ~+134/−3 | OPTIMADE registry refresh | Small, coherent, no suspicious weakening found | Low |
| 2026-08-24 | `4aa66178` | ~+74 | Endpoint-moved diagnostic | Improves an opaque JSON error | Low/positive |
| 2026-08-24 | `c8774b02` | ~+1,802/−76 | Adds iroh key-addressed transport † | Commit title claims cross-country reachability; module is not wired into production discovery/query; lockfile adds 115 package entries and removes one | Medium integration/bloat risk |
| 2026-08-24 | `93c14e70` | +31/−19 | Changes ontology IRIs | Coherent branding/schema change; migration compatibility needs domain review | Medium |
| 2026-08-24 | `0069ec34` | ~+309 | Terminal notebook rendering † | Useful capability; notebook lifecycle/registry weaknesses predate it and remain | Medium |
| 2026-08-24 | `5cdba1ff` | +9/−3 | Dashboard notebook acceptance | Narrow behavior fix | Low |
| 2026-08-24 | `4f6a8700` | 55; +185/−124 | Removes advertised tool counts † | Broad string/tool-surface cleanup; tests that count source names remain brittle | Medium |
| 2026-08-24 | `7c3827ba` | ~+246/−1 | Provider palette | Adds another provider/config path atop existing configuration systems | Medium |
| 2026-08-25 | `ac9d8c86` | ~+73 | Repairs `ALWAYS_INCLUDE` behavior † | Name and behavior brought back into alignment | Positive |
| 2026-08-26 | `bbd88398` | +57/−43 | Removes bundled Anthropic provider † | Commit says tests were converted, but two `use_command` tests were missed and full gate is red | High process risk |
| 2026-08-26 | `ba13ebf2` | +3,404/−630 | TUI interaction/honesty work † | Very large UI change with object-level tests; no PTY-level protection | High |
| 2026-08-26 | `b6699271` | +3,711/−562 | Collapses three tool families † | Changes model-visible tool surface and leaves JSpace benchmark fixture inconsistent | High |
| 2026-08-26 | `0bc6fcc7` | 100; +7,879/−366 | Lands eight months of outstanding ingest/tools/compute/provenance work † | Explicitly selective gates, formatting red, JSpace red, and known huge local build artifacts; also contains valuable honesty fixes | High |
| 2026-08-26 | `c7fcfe60` | 2; +137/−4 | Fixes tmux raw-mode regression and relabels public catalogs † | Candid live reproduction, but new source string immediately violates `no_exit_to_cli`; branch moved during this audit | Medium |
| 2026-08-26 | `a572070e` | 4; +69/−1 | Stops keys entering an invisible sidebar; corrects scope copy † | Focused live-UX repair with a render-plus-key regression test; candidly discloses a one-frame stale-footer residue; branch moved again during adversarial review | Positive counterevidence |

### High-churn interpretation

- `a56d8229` is an expected squashed/bootstrap release, not evidence that 312k
  lines were maliciously injected at once. It does, however, limit “previous
  behavior” reconstruction.
- `51f5260d` is mostly a legitimate removal of vendored/native duplicate
  architecture, but its title is not an adequate description of a 163k-line
  removal and its SSE regression demonstrates the danger of such consolidation.
- `9e1f2943` and `0bc6fcc7` are review-hostile checkpoint commits. Their
  comments and documents frequently identify uncertainty honestly; the primary
  concern is defect density and auditability, not motive.
- 442 of 821 visible commits contain `Co-Authored-By: Claude` metadata. This
  establishes heavy AI assistance, not authorship of any particular defect and
  not intent.

## 5. Functional Regressions

### Confirmed current or gate-level regressions

| Finding | Input / state | Expected | Actual | Introduced / exposed |
| --- | --- | --- | --- | --- |
| Provider test contract | full workspace test after removing bundled Anthropic | tests match registry; suite green | two tests still require Anthropic / `ANTHROPIC_API_KEY` | `bbd88398` |
| JSpace candidate contract | `jspace_benchmark` fixtures after tool-family collapse | candidates include the tool names/shape expected by benchmark or benchmark is migrated coherently | two assertions fail because `deploy_list` is absent | `b6699271` / partial `0bc6fcc7` update |
| Exit-to-CLI policy | `cargo test -p prism-server --test no_exit_to_cli` | no user-facing string tells a user to leave the current surface and run a CLI command | `boot_checks.rs:415` and test source at 730 contain `run prism login` and gate fails | `c7fcfe60` |
| Formatting gate | `cargo fmt --all -- --check` | no diff | diffs in `crates/compute/src/backend.rs`, `byoc.rs`, `hyperqueue.rs`, `lib.rs`, `marc27.rs` | principally `0bc6fcc7` |

### Confirmed historical regressions now fixed

1. **Discourse SSE loss (`51f5260d` → `3e1b792d`).**
   The earlier parser preserved multiline/chunked SSE framing. The consolidation
   replaced it with per-line JSON parsing and removed three corresponding tests.
   Double-encoded or chunk-split discourse events were silently dropped. The
   fix restored buffered decoding and regression tests. This is the strongest
   repository example of implementation degradation plus test weakening, but
   the repair and explanation materially weaken a sabotage interpretation.

2. **TUI unusable inside tmux (`0bc6fcc7` contents → `c7fcfe60`).**
   Moving graphics detection caused `Picker::from_query_stdio` to leave a thread
   that disabled raw mode after TUI startup when tmux swallowed the query.
   Keystrokes and escape sequences landed in the status line. The author drove
   three real binaries, documented the output, and fixed multiplexer fallback.
   The incident demonstrates that 496 App-level tests did not exercise terminal
   setup.

3. **Partial element coverage produced plausible predictions (pre-`68b4bb16`).**
   The basic featurizer once computed a full-looking vector from only elements
   present in its 43-element table. Compounds such as BSb/BOs/TcB could collapse
   to boron-like features. Current code refuses a property block when any element
   is uncovered and versions the backend. This was a serious scientific defect
   that is now explicitly repaired.

4. **Keystrokes routed into a hidden sidebar (fixed by `a572070e`).**
   Below 100 columns the Workspace sidebar was not rendered but retained focus;
   printable input was dropped. The fix records actual layout visibility,
   redirects focus on the next key, and adds a render-plus-input regression
   test. The disclosed one-frame footer lag remains. This focused live repair is
   positive counterevidence to deliberate degradation.

### High-impact defects without reconstructable prior good behavior

The configuration, publish/report, notebook registry, tool synchronization, and
`run` routing findings are confirmed defects, but the visible public history
starts with them. They should not be described as a later “regression” without
pre-public evidence.

## 6. Fake / Stub / Placeholder Implementations

### Production-path assessment

I did not find a hidden trigger that automatically selects fake physics as the
initial backend. The adversarial pass did find a production-reachable fake
backend whose cached result can later satisfy a real request (F-015), so the
first-pass “test seam only” conclusion was wrong.

| Implementation | What it really does | Reachability / honesty | Assessment |
| --- | --- | --- | --- |
| `app/plugins/thermocalc.py` | Thin connector-stub/example around Thermo-Calc concepts | Every description says STUB/not implemented; registration requires commercial `tc_python`; absent from normal bootstrap | Honest example; isolate or retain by product choice |
| `app/tools/labs.py::submit` | Returns `not_implemented` rather than submitting a lab job | Response is explicit; surrounding metadata discovery broadly catches exceptions | Honest placeholder; do not advertise as operational |
| MACE `FakeBackend` | Generates deterministic fake/test result records without loading weights | Explicitly selectable from the production agent schema; provenance says fake/no weights, but cache identity is shared with real backends | **Unsafe production boundary: High F-015** |
| Alternate ontology engine fallbacks | Return an explicit unavailable/error result | No fabricated ontology inference found | Honest degradation |
| `gpus` offline path | Emits a documented JSON error object | Explicit API contract exits 0 even for the error object | Unconventional exit contract, not silent/fake |
| iroh transport | Real iroh endpoint/request code | Only exported/tested, not production-wired | Orphaned implementation, not fake transport |

### Plausible-but-misleading surfaces

- `generate_cli_guide` claims a workflow run will execute but omits `--execute`;
  the generated command only produces a dry-run plan. This is misleading
  documentation generation, not a fake workflow engine.
- `ingest --status` extracts a narrow response shape and turns missing/wrong
  fields into zeros, making an incompatible server response look like an empty
  graph.
- Query ownership helpers turn store/query errors into an empty owner list,
  which looks like a valid “no owner” semantic result.
- Remote MACE payloads invoke real calculators, but a previously seeded fake
  result can satisfy a real request through the shared cache (F-015). Primary
  provenance remains honest; the downstream structure resolver can drop it.

## 7. Silent Failure Paths

The following register includes High findings above and additional Medium paths.
“Silent” includes success exit with an error object/message, plausible defaults,
or loss of an error before the caller can act.

| ID | Location | Failure converted to | Severity | Evidence |
| --- | --- | --- | --- | --- |
| F-001 | `main.rs::handle_configure` | complete default config, then overwrite | High | Runtime-reproduced |
| F-002 | `core/config.rs::NodeConfig::load` | invalid file treated like absent file | Medium | Runtime/source-confirmed |
| F-003 | `main.rs::Commands::Publish` | printed error + exit 0 | High | Runtime-reproduced with fake executable |
| F-004 | `main.rs::handle_report` | unconditional “submitted” + exit 0 | Medium | Runtime-reproduced |
| F-007 | `tool_sync.rs` | empty manifest/body and destructive overwrite | High | Static/dataflow strong |
| F-008 | `notebook.rs` | empty registry / forgotten process | Medium | Static/dataflow strong |
| F-015 | MACE cache/runner | fake result → normal real-request cache hit | High | Isolated cache reproduction |
| F-016 | federated query | model-controlled URL → literal query/peer sends | High | Safe loopback reproduction |
| F-017 | MACE auto selection | known unsupported platform operation → failure, no claimed fallback | High | Static control flow |
| F-101 | `main.rs::handle_gpus` / dispatcher | documented structured error + exit 0 | Low contract drift | Runtime-reproduced |
| F-102 | `main.rs::handle_ingest_status`, ~9779–9825 | missing/wrong response fields → 0 counts; nested jobs omitted | Medium | Loopback fixture returned 123/456/789 but human output printed 0/0/0 |
| F-103 | `main.rs::semantic_hit_owners`, ~13909–13931 | store/query error → empty owners | Medium | Static; sibling error path fixed in `167ebb8d` |
| F-104 | `main.rs::record_remote_ingest`, ~9698–9776 | provenance open/record failure → warning or ignored result | Medium | Static |
| F-105 | `tool_sync.rs::load_manifest` | read/JSON error → empty manifest | Medium within High F-007 | Static |
| F-106 | `notebook.rs::load_registry` | home/read/JSON error → empty registry | Medium within F-008 | Static |
| F-107 | `app/tools/labs.py` catalog/subscription helpers | broad exception → empty lists | Medium | Static |
| F-108 | `app/tools/web.py` module initialization | 2-second localhost registration failure → cached absence | Medium | Static |
| F-109 | Python tool wrappers across `app/tools` | broad `except Exception` → `{"error": ...}` | Context-dependent | Often intentional agent protocol; damaging where callers ignore `error` |

Additional command-contract drift:

- `plugins` ignores the already resolved global Python interpreter and passes
  the raw CLI value. With `PRISM_PYTHON=/usr/bin/false` and no `python3` on
  `PATH`, it still attempts `python3` and its nested failure returns through a
  human-friendly path rather than honoring the chosen interpreter.
- `notebook start` reconstructs `~/.prism/venv/bin/python` and ignores
  `--python`. `--python /usr/bin/false notebook start` therefore complains about
  the PRISMA venv instead of using the requested executable.
- The generated CLI guide’s `workflow run cold_start` lacks `--execute`, so it
  exits 0 with `dry_run/planned` while prose says it executes.

### Error-handling ranking

1. **Data-destructive:** configuration and tool sync.
2. **Externally misleading:** publish/report, run routing, ingest mode.
3. **Scientific identity/fake reuse:** MACE backend-agnostic cache, remote
   environment identity, and remote ingest record.
4. **Privacy/SSRF:** unattended federated query accepts arbitrary primary and
   response-supplied destinations.
5. **Operational state loss:** notebook registry and stop.
6. **Plausible empty/default output:** ingest status, query owners, catalogs.
7. **UX/automation mismatch:** gpus, guide, interpreter selection.

## 8. Test Integrity Assessment

### What was counted and run

- Exactly 3,552 anchored Rust `#[test]`/`#[tokio::test]`-style attributes and
  1,448 Python `test_*` definitions were counted across 189 test files. Versus
  `private/main`, those counts are +162 Rust and +69 Python. Attribute
  counts are not executed-test counts because parameterization, cfgs, and
  generated cases differ.
- Fourteen Rust tests are ignored; 28 Python skip/skipif markers were found.
- Normal Python configuration sets `testpaths = ["tests"]`.
- Default offline Python run: **1,515 passed, 15 skipped, 4 deselected**.
- `app/tools/tests` is outside `testpaths`; direct execution produced **9 passed,
  5 skipped**.
- Full Rust workspace/all-targets run failed five cases across three targets as
  detailed in F-013; the current TUI package itself passed 498 tests including
  doctest.

### Tests that do not validate what their names imply

1. **`tests/test_tui_e2e.py`** returns booleans from test functions. This is a
   confirmed false-pass mechanism (F-012).
2. **Marketplace catalog coverage** scans source text for names and compares
   them with a live/static catalog count. Dead strings and comments can satisfy
   the test; executable registration is not required.
3. **`c7fcfe60` public-catalog test** constructs the desired rendered strings
   locally and then asserts that source includes matching literals. It does not
   execute boot checks or verify authentication semantics.
4. **Most TUI unit tests** instantiate `App` directly. They do not run terminal raw
   mode, PTY input, graphics detection, or the event loop; the tmux failure
   survived hundreds of green tests. `a572070e` adds a stronger real-render plus
   key-routing regression, but still not a PTY/tmux process test.
5. **Real iroh round-trip** is correctly marked ignored because it uses the n0
   relay, but no hermetic production-wiring test replaces it.
6. **App tool tests** can be modified and pass when called directly while never
   running under the configured default Pytest gate.

### Evidence of test weakening

The strongest instance is historical: `51f5260d` removed three SSE tests while
replacing a robust parser with a weaker one. The current implementation has
restored coverage. On the audited branch, test *quantity* rose; I did not find a
branch-wide pattern of assertions being deleted to hide degradation.

### Test verdict

The suites contain substantial semantic value—especially ingestion, provenance,
auth, offline, and feature-backend guards—but they cannot currently certify the
shipped product:

- the complete Rust gate is red;
- default Python discovery omits a tool-test subtree;
- the only named TUI E2E file is partly a direct-script harness masquerading as
  Pytest semantics;
- live terminal, remote scheduler, platform, relay, and MACE boundaries remain
  mostly unexecuted.

## 9. Architectural Entropy

### Concentration and parallel systems

1. **Monolithic CLI dispatcher.** `crates/cli/src/main.rs` is approximately
   22,000 lines. The graph-indexed `main` path has roughly 100 direct callees and
   very high branch complexity. Validation, authentication, Python selection,
   output, and exit semantics are repeated across arms. F-001 through F-006,
   F-013, F-016, and several Medium findings all arise in this file.
2. **Multiple configuration authorities.** `NodeConfig` global/project TOML,
   chat target state, provider TOML registry, CLI state/credentials, dotenv, and
   environment variables each implement different precedence and error
   behavior. Adding the provider palette increased this seam rather than
   consolidating it.
3. **Parallel Python interpreter resolution.** The root CLI resolves/provisions
   a Python path, while plugin and notebook modules independently choose another
   executable.
4. **Parallel tool installation paths.** Explicit marketplace installation
   refuses overwrite; automatic sync silently overwrites. The same user artifact
   has incompatible ownership rules.
5. **Parallel discovery/transport concepts.** mDNS/platform discovery and HTTP
   federated queries remain production paths; iroh is a separate exported
   transport without an adapter into either.
6. **Mixed model/provenance layers.** Local runner provenance observes local
   packages, remote HF payload provenance reports a thinner shape, cache
   identity is maintained separately, and calculator identity omits resolved
   weight digests.
7. **Python dictionary protocols.** Many tools use broad untyped dictionaries
   plus local JSON schemas. This is appropriate at the LLM boundary but remains
   dynamic deep into domain code, enabling missing fields to become defaults.

### Largest high-review modules

| File | Approx. lines | Entropy concern |
| --- | ---: | --- |
| `crates/cli/src/main.rs` | 22,311 | Multiple products and policies in one dispatcher |
| `crates/provenance/src/emmo.rs` | 12,413 | Large ontology/provenance mapping surface |
| `crates/agent/src/command_tools.rs` | 12,287 | Broad tool dispatch/security boundary |
| `crates/agent/src/protocol.rs` | 11,533 | Protocol types and service behavior concentrated |
| `crates/agent/src/agent_loop.rs` | 7,001 | Tool execution, approval, subagents, LLM loop |
| `crates/tui/src/app.rs` | 6,864 | UI state, tool behavior, and lifecycle |
| `crates/llm/src/lib.rs` | 5,169 | Provider/stream/tool-call compatibility |
| `crates/provenance/src/lib.rs` | 5,030 | Persistence and scientific identity |
| `crates/ingest/src/text_extract.rs` | 4,817 | Scientific extraction and multi-pass agreement |
| `crates/tui/src/render.rs` | 4,794 | UI layout/render state |

### Collapse/removal estimate

Without redesigning capabilities, a focused refactor could plausibly:

- split `main.rs` into command modules with a shared outcome/exit contract;
- collapse Python resolution into one immutable runtime context;
- unify explicit install and background sync behind one artifact ownership
  policy;
- make all configuration sources feed one typed, diagnostic-producing resolver;
- integrate iroh through an explicit `PeerTransport` seam or remove it until
  ready;
- unify local/remote calculator identity and provenance.

This is likely to remove hundreds to low-thousands of duplicated validation and
glue lines, but a precise deletion count requires implementation work and should
not be invented from static review.

## 10. Dead and Orphaned Code

| Component | Evidence of reachability | Classification | Recommendation |
| --- | --- | --- | --- |
| `crates/mesh/src/iroh_transport.rs` | production references are module export plus tests; `DiscoveryMethod` only has mDNS/platform; federated query still uses HTTP | Orphaned experimental subsystem (Medium architecture/bloat risk) | Wire through a transport interface with authorization, or remove dependency/module until ready |
| `crates/mesh/tests/iroh_peer_roundtrip.rs` | real test is `#[ignore]` and manually contacts n0 | Experimental validation only | Keep only if integration work is scheduled and CI has an explicit live lane |
| `app/plugins/thermocalc.py` | not in production bootstrap; registration requires `tc_python` and every description says STUB | Honest guarded example | Keep isolated as an example or remove by product choice; not an integrity-mandated deletion |
| Labs `submit` | returns explicit `not_implemented` | Advertised placeholder | Hide from capability discovery or implement |
| Old provider/tool strings | source-count tests and docs retain names not present in the active registry | Dead textual surface | Replace text scanning with registry execution, then remove stale strings |
| Pre-removal Forge artifacts | largely removed by `51f5260d`; root history still contains vendor bulk | Historical, not current dead code | No current deletion needed; document release provenance |

The MACE fake backend is not selected initially by `auto`, but it is explicitly
agent-selectable and its cache can satisfy later real requests (F-015). Test
support helpers and ignored live tests otherwise remain explicit surfaces and
should not be called dead merely because normal production does not invoke them.

## 11. Dependency / Supply-Chain Findings

### Rust advisory scan

`cargo audit --no-fetch` inspected 1,058 locked crates and exited non-zero:

- **`RUSTSEC-2023-0071` — Marvin Attack in `rsa 0.9.10`.** Dependency path is
  `rsa → jsonwebtoken 10.4.0 → prism-client`. Repository use observed during
  review verifies public-key JWTs; it does not perform attacker-observable
  private RSA operations. Therefore runtime exploitability in the reviewed path
  is Low, but dependency/CI integrity severity is Medium and the advisory should
  not be waived silently.
- Seven allowed warnings cover unmaintained/yanked transitive crates including
  `paste`, `ttf-parser`, `memmap2`, `rand`, and yanked `spin`. They do not prove
  compromise, but the exceptions should have owners and expiry criteria.

Cargo.lock contains registry checksums and no suspicious Git dependency was
found. JavaScript lockfiles are present. These are positive supply-chain
controls.

### Unnecessary image-format expansion

`crates/tui/Cargo.toml` tries to disable default features on direct `image` and
enable only PNG/JPEG, but `ratatui-image`’s `image-defaults` feature re-enables
`image/default-formats` and Rayon through feature unification. `0bc6fcc7` added
roughly 54 packages/571 lockfile lines including AVIF/rav1e/ravif, EXR, GIF,
TIFF, QOI, WebP, Rayon, and libfuzzer-related transitive code.

The full Cargo run visibly compiled `rav1e`, `ravif`, `exr`, `gif`, `tiff`,
`qoi`, `webp`, and `rayon`. PRISMA’s intended terminal figures need PNG/JPEG.
This is a Medium build-time, disk, binary-size, and attack-surface regression,
most consistent with misunderstood Cargo feature unification.

### Python dependency integrity

`pyproject.toml` uses broad lower-bound ranges for the core environment and
large scientific extras; there is no repository-wide resolved Python lock.
Reproducibility therefore depends on installation date and resolver state.
Remote MACE payloads add a second, floating dependency environment (F-009).

Static import analysis found declared packages with no ordinary in-repository
import, including some of `anthropic`, `openai`, `sqlalchemy`, `tenacity`,
`firecrawl`, and `google-cloud-aiplatform`. This is a review list, not proof of
dead dependency: plugins and dynamic imports may use them. `bbd88398` removed
the bundled Anthropic provider while leaving the SDK declared, so Anthropic is
the clearest candidate for removal or optionalization.

### Dynamic download/execute boundaries

- MACE remote payloads can invoke `pip install` against
  `MACE_MCP_DEV_INSTALL_URL` or unpinned `mace-mcp`, then execute the installed
  code in a remote job.
- Hugging Face model files are downloaded without an explicit revision and
  loaded by MACE.
- Marketplace tools are downloaded and written as executable Python source
  without a local signature/content-ownership gate (F-007).
- ML joblib artifacts are executable-deserialization formats (F-011).

Each mechanism has a plausible product purpose. They are part of the broader,
inconsistent artifact-trust surface below.

### F-401 — Mutable runtime sidecar is auto-pulled and published on all interfaces

**Severity:** Medium, conditional supply-chain/network exposure<br>
**Evidence:** `crates/node/src/runtime_service.rs` 33–35, 60–62, 99–148, and
230–267; introduced by `25edcb6f`<br>
**Behavior:** local ingest/node self-healing pulls and runs mutable
`ghcr.io/darth-hidious/marc27-runtime:latest` with no digest/version
attestation. `-p {port}:8090` publishes on all host interfaces, even though the
client accepts a loopback base URL and describes the service as local. The
manual command repeats the mapping.

This can silently change executable runtime code between identical PRISMA
revisions and expose the sidecar to the LAN. Authentication inside the external
image was not audited, so unauthenticated compromise is not claimed. Pin an
image digest, attest/report it, and bind `127.0.0.1:{port}:8090` (plus an
explicit IPv6 policy).

### F-402 — Python environment recovery executes mutable upstream code

**Severity:** Medium, conditional bootstrap integrity risk<br>
**Evidence:** `crates/python-bridge/src/venv.rs` 327–350 and 633–694<br>
**History:** `fd48c0f1` introduced the pip bootstrap; `7459d793` added
offline/Windows guards; `85ffb7d4` introduced release-wheel fallback<br>
**Behavior:** if `ensurepip` leaves a pipless venv, PRISMA downloads current
`https://bootstrap.pypa.io/get-pip.py` and pipes it into the venv Python without
a pinned digest. If the versioned PRISMA wheel is absent/fails, the compiled
binary silently installs unpinned `git+https://github.com/Darth-Hidious/PRISM.git`
main; `describe()` reports only “from git main.”

These are candid self-healing shortcuts, not covert behavior, but they break
revision identity and execute mutable remote source. Ship/pin a wheel and pip
bootstrap artifact for each release; on development builds require an explicit
ref/commit and record it.

The MACE, marketplace, joblib, sidecar, pip-bootstrap, Git-main, and model-weight
mechanisms have plausible product purposes. Together they require one artifact
trust policy rather than seven independent conventions.

## 12. Performance Regressions

### Measured build/disk behavior

- Before the external cleanup that occurred during the audit, `target/` occupied
  approximately **83 GiB**, the filesystem was full, and the first full Cargo
  test attempt failed with “No space left on device.”
- Another process removed/rebuilt artifacts, temporarily reducing `target/` to
  ~14 GiB. Audit compilation first grew it to ~51 GiB; the final full run and
  focused TUI validation grew it to **65 GiB**, leaving ~16 GiB free.
- A pre-clean debug `prism` binary was approximately **423 MiB**.
- Full workspace compilation/test startup took about two minutes before tests;
  Clippy took 55.77 s after artifacts were available.

The 83→14→51→65 GiB movement is partly a local workflow/concurrency problem, not a
source-only regression. `AUDIT_FIXES.md` independently documents agents
colliding around `cargo clean`/`target` and a prior ~110 GiB build tree. The
image feature expansion above is a concrete source contributor, but it does not
explain all disk use.

### Algorithmic/network hot paths

| Path | Behavior | Risk | Status |
| --- | --- | --- | --- |
| `app/tools/skills/prediction.py::_predict_properties` | Up to ~2PN composition featurizations plus one tiny model prediction per row, with Python row iteration | High latency on large datasets | Suspected; no production-size benchmark |
| `app/tools/materials/informatics.py`, ~301–429 | Up to eight sequential Materials Project requests and model retraining per call | Network/API cost and repeated training | Strong static evidence |
| `app/tools/web.py`, ~33–71 | POSTs to localhost during module import with a two-second timeout; result cached for process lifetime | Startup stall and stale availability state | Strong static evidence |
| `handle_ingest_platform` | Reads full PDF into memory, clones bytes into request body, then buffers the full SSE body for a run that may take minutes | At least two full input buffers plus full event stream; no progressive output | Confirmed design |
| `prism run` | Always sleeps two seconds after submission before initial status | Fixed latency even when backend already responds | Confirmed; small per invocation |
| Remote/fake MACE cache mismatch | Can improve speed through stale or fake hits while silently sacrificing validity | Integrity outweighs performance gain | High F-009/F-015 |

No claim is made that these paths dominate real workloads without profiling.
The first remediation should be instrumentation (input size, request count,
feature time, model time, memory), followed by batching/vectorization and
streaming.

## 13. Scientific / Numerical Integrity

### Positive controls found

The current branch contains meaningful scientific-integrity defenses:

- seeded ML train/test split (`random_state=42`);
- seeded random-supercell construction on production MACE call paths;
- feature-backend identity saved with trained models and checked at prediction;
- refusal when the built-in element table lacks any element needed for a
  property block;
- explicit MACE head/license validation to prevent r²SCAN labels on single-head
  PBE weights;
- fake backend provenance that says fake/no model weights;
- ingestion agreement thresholds, deferred-document accounting, and failure
  when every PDF is starved behind vision;
- HEA screening code that labels unverified pair-cell values as indicative and
  now preserves missing values instead of substituting zero.

These are not cosmetic comments: several are enforced by tests and branch
conditions. They are strong evidence that the codebase is trying to prevent
plausible fabricated science.

### F-201 — Basic formula parser accepts trailing garbage and unbalanced input

**Severity:** Medium<br>
**Evidence:** Confirmed by direct pure-Python execution<br>
**Location:** `app/tools/ml/features.py::_parse_segment/_parse_formula`,
lines 119–196<br>
**Behavior:** unmatched/unknown characters are skipped and open groups are
folded into the result. `Fe2O3garbage`, `Fe2O3)`, and `Fe2(O3` can all reduce to
the same Fe/O composition rather than fail validation.

This parser is a fallback featurizer, not a canonical chemistry parser. Even so,
accepting malformed material identity and producing plausible features is
dangerous. Return a structured parse error unless the entire normalized input is
consumed and delimiters balance.

### F-202 — “std” composition descriptors ignore stoichiometric weights

**Severity:** Medium<br>
**Evidence:** Confirmed by direct execution and source<br>
**Location:** `app/tools/ml/features.py` 217–263<br>
**Behavior:** averages use atomic fractions, but `statistics.stdev(values)`
treats each distinct element equally. The fallback returned the same
approximately 1.414 standard-deviation descriptor for Fe0.99Ni0.01 and
Fe0.5Ni0.5 despite radically different composition fractions.

If the intended descriptor is unweighted “elemental diversity,” it must be
named/documented as such. If it is meant to be a compositional distribution
statistic, compute the weighted population standard deviation and bump the
backend version to invalidate old models.

### F-203 — Five-row training can persist an undefined R² metric

**Severity:** Medium<br>
**Evidence:** Strong source-level evidence; standard sklearn semantics<br>
**Location:** `app/tools/skills/prediction.py` 73–114 and
`app/tools/ml/trainer.py` 40–65<br>
**Behavior:** skill training allows five valid rows. A 20% test split yields one
test observation; `r2_score` is undefined and returns NaN with a warning. The
metric is converted to float, persisted, and displayed as holdout quality.

Require enough holdout observations for each metric, use cross-validation for
small samples, represent unavailable metrics as null with a reason, and never
serialize NaN as a quality score.

### F-204 — Remote MACE provenance/cache identity is incomplete

This is High F-009. It is the most consequential scientific-integrity finding:
the equations/calculator may be real while the result cannot be tied to the
exact weights and environment.

### F-205 — Fake MACE cache entries cross the real-backend boundary

This is High F-015. Primary fake provenance is accurate, but cache selection is
backend-agnostic and the downstream structure resolver drops the provenance.
The result is a current pathway by which a fake-derived number or structure can
be consumed as a normal real-request cache hit.

### F-206 — Automatic routing chooses unsupported MACE operations

This is High F-017. A configured platform causes `auto` to select platform for
elastic, phonon, and dilute tasks even though that backend explicitly rejects
them and the claimed fallback is not implemented.

### F-207 — Dataset prediction is computationally repetitive and in-sample

Current code now attaches an explicit `in_sample_warning` and exposes holdout
metrics, which is good. It still fits on rows from the same dataset and writes
predictions across those rows; users can mistake `predicted_*` for independent
evidence if callers drop that warning. Provenance consumers should retain the
warning and training-row identities, not only the numeric column.

### Numerical/non-determinism assessment

I searched production scientific call paths for unseeded randomness. The
principal ML split and MACE/supercell callers provide deterministic seeds.
`build_supercell` accepts an optional seed, but reviewed production callers pass
one. Timestamps and UUIDs affect provenance identity, not the computed physics.
No high-confidence path was found where unexplained randomness changes a
scientific value for identical explicit inputs.

Potential nondeterminism remains in:

- floating remote dependencies/model revisions (F-009);
- backend-agnostic fake/real cache reuse (F-015);
- environment-dependent `auto` backend selection, including unsupported paths
  (F-017);
- GPU/backend numerical variation not recorded in sufficient detail;
- unordered/dynamic external catalogs;
- automatic network retries and marketplace synchronization affecting available
  tools between identical CLI invocations.

## 14. Security Findings

### High

1. **Federated query text exfiltration/SSRF (F-016).** An offered,
   non-approval tool accepts a model-controlled host, sends the literal query,
   and trusts response-supplied peer addresses. No platform token was observed,
   but query content and limited identity metadata leave the machine.
2. **Downloaded marketplace code overwrites local executable source (F-007).**
   This is primarily integrity/data loss; compromise of marketplace or delivery
   also makes it a code-supply path.

### Medium, conditional

1. **Approval absence fails open (F-010).** Standard server/protocol paths
   currently supply a receiver, but the API default is unsafe for embeddings and
   future call sites.
2. **Path construction plus joblib deserialization (F-011).** Exploitation needs
   a malicious reachable artifact or writable model directory, but the input
   boundary does not enforce the safety assumption.

### Medium/Low

| ID | Finding | Evidence and reachability | Severity |
| --- | --- | --- | --- |
| F-301 | Session token accepted in URL query | `crates/server/src/auth.rs` around 179–214 accepts token query parameters; URLs leak through logs/history/referrers more readily than headers | Low; retain only for constrained transports with redaction |
| F-302 | Project-scoped model catalog appears unauthenticated | **Unverified external observation:** `c7fcfe60`’s author reports that `/projects/{id}/llm/models` and marketplace resources returned 200 without Authorization; this audit did not reproduce it | Potential platform tenancy issue outside this repository |
| F-303 | `rsa` timing advisory | `RUSTSEC-2023-0071` transitive via JWT library; reviewed path verifies public keys | Medium dependency hygiene, Low observed runtime exploit |
| F-304 | Dynamic `MACE_MCP_DEV_INSTALL_URL` | Remote job installs and executes environment-selected package source | Medium deployment supply-chain risk; potentially High impact |
| F-305 | Unpinned model download | Hugging Face repository/filename without revision/digest | Medium supply-chain and High reproducibility risk |
| F-401 | Mutable `latest` sidecar / all-interface port publish | `runtime_service.rs` auto-pulls mutable image and maps `port:8090` | Medium, conditional |
| F-402 | Mutable pip/Git-main bootstrap | `venv.rs` executes unpinned `get-pip.py` and can install repository main | Medium, conditional |

### Positive security observations

- Cargo registry dependencies are locked and checksummed.
- Server authentication middleware generally fails closed and scopes access.
- Peer identity debugging redacts tokens.
- Offline guards now cover several direct HTTP and subprocess egress paths.
- Retry logic reviewed in billing/submission paths generally avoids ambiguous
  duplicate spending.
- No current platform-credential exfiltration, covert telemetry channel, hidden
  attacker endpoint, or selective security bypass was demonstrated. F-016 does
  expose query text and limited identity metadata to model-chosen destinations.

## 15. AI-Generated-Code Fingerprints

These patterns are characteristic of coding-agent output but do not prove which
model authored them:

1. **Huge prose-driven functions.** `main.rs`, `agent_loop.rs`,
   `command_tools.rs`, and protocol files accrete one requirement at a time
   instead of enforcing shared contracts.
2. **Verbose forensic comments beside incomplete behavior.** Some comments are
   excellent; others explain an intended safety property that the type system
   does not enforce (for example, the joblib directory trust assumption).
3. **Local fixes that miss sibling paths.** `167ebb8d` fixes one unreadable-graph
   path while owner lookup still defaults empty; repair-mode flag validation
   does not validate other ingest modes; JSON publish errors differ from human
   mode.
4. **Duplicate resolution utilities.** Python executable, configuration,
   provider, tool installation, and provenance identity are recomputed in
   adjacent modules with subtle divergence.
5. **Source-string tests.** They are fast for an agent to generate from a prose
   requirement, but they prove that words exist rather than behavior occurs.
6. **Interfaces landed before integration.** The iroh transport is polished,
   documented, and tested in isolation while no production caller uses it.
7. **Broad dictionary/`unwrap_or_default` boundaries.** These reduce compiler or
   schema friction and create plausible empty objects when the implementation
   does not understand a response.
8. **Large checkpoint commits with narrative titles.** The change description
   is often thoughtful but cannot make a 100–800-file diff reviewable.
9. **“Honesty” patches after live failures.** Later commits add detailed refusal
   wording and safeguards but sometimes omit the cross-cutting gate or adjacent
   output mode.
10. **Generic abstraction without capability coupling.** MACE selects a backend
    without consulting its supported operations, and its cache identity omits
    backend/model provenance. The interfaces are clean while their global
    invariants are absent.
11. **Comments promising behavior that no caller implements.** The platform
    backend says the runner falls through; the runner executes one backend and
    fails. This is more consistent with separately generated modules than with a
    hidden selective trigger.

Overall fingerprint assessment: **mostly earnest, frequently sophisticated,
sometimes damaging through incomplete integration and local reasoning.** The
comments and test additions do not look like random low-capability output; the
dominant problem is that high-volume agent work was merged faster than the
repository’s global contracts could be revalidated.

## 16. Top 20 Highest-Risk Files

| Rank | File | Risk | Why / key symbols | Relevant commits |
| ---: | --- | --- | --- | --- |
| 1 | `crates/cli/src/main.rs` | Critical review priority | 22k-line dispatcher; `handle_configure`, ingest modes, publish/report, run, query, status, pre-dispatch side effects | `a56d8229`, `ca0d6d43`, `9e1f2943`, `0bc6fcc7` |
| 2 | `crates/agent/src/command_tools.rs` | Very High | Model-visible query URL, permission metadata, CLI argument construction | root lineage, `b6699271` |
| 3 | `crates/cli/src/tool_sync.rs` | Very High | Automatic executable-code download/overwrite; manifest/write lifecycle | root lineage, startup integration |
| 4 | `app/tools/simulation/mace/primitives.py` | Very High | Builds backend-agnostic keys before backend selection | `f62e31ed` |
| 5 | `app/tools/simulation/mace/jobs/runner.py` | Very High | Accepts any cache result before requested backend; no fallback | `f62e31ed`, `963abfd3` |
| 6 | `app/tools/simulation/mace/cache/hashing.py` | Very High | Omits backend, model/environment, and weight digest | root lineage |
| 7 | `app/tools/simulation/mace/backends/base.py` | Very High | `auto` routing ignores backend capability | `963abfd3` |
| 8 | `app/tools/simulation/mace/backends/platform.py` | Very High | Supports only relax/MD while prose claims fallback | `963abfd3` |
| 9 | `app/tools/simulation/mace/backends/hf_jobs.py` | Very High | Remote packaging, launch, and result identity | root lineage, `0bc6fcc7` |
| 10 | `app/tools/simulation/mace/payloads/_common.py` | Very High | Dynamic package install and remote calculator setup | root lineage |
| 11 | `app/tools/simulation/mace/core/calculator.py` | Very High | Floating model download; incomplete calculator signature | `68b4bb16` and later |
| 12 | `crates/node/src/runtime_service.rs` | High review | Mutable `latest` sidecar and all-interface port mapping | `25edcb6f` |
| 13 | `crates/python-bridge/src/venv.rs` | High review | Mutable `get-pip.py` and Git-main fallback | `fd48c0f1`, `85ffb7d4` |
| 14 | `tests/test_tui_e2e.py` | High test risk | Pytest false passes and release-binary conditional skip | pre-public, `d8c4e4ba` |
| 15 | `app/tools/ml/registry.py` | High review | Unvalidated paths and pickle/joblib loading | root lineage |
| 16 | `app/tools/ml/features.py` | High science review | Formula parser, hard-coded table, weighted/unweighted descriptors | `68b4bb16`, `0bc6fcc7` |
| 17 | `crates/agent/src/agent_loop.rs` | High review | Approval default, tool execution, subagents, LLM loop | `2ab08b73`, `9e1f2943` |
| 18 | `crates/provenance/src/lib.rs` | High review | Large persistence spine; remote/local identity must converge | `9e1f2943`, `0bc6fcc7` |
| 19 | `crates/ingest/src/text_extract.rs` | High science review | Extraction, agreement, deferred facts, scientific data path | `9e1f2943`, `0bc6fcc7` |
| 20 | `crates/cli/src/notebook.rs` | Medium-High review | Registry corruption, PID identity, ignored kill result | root lineage, `0069ec34` |

Files just outside the top 20 that deserve domain review include
`crates/agent/src/protocol.rs`, `crates/provenance/src/emmo.rs`,
`crates/tui/src/image_view.rs`, `crates/mesh/src/iroh_transport.rs`,
`app/tools/materials/informatics.py`, and `app/tools/web.py`.

## 17. Remediation Plan

### Immediate — before more feature development or release

1. **Freeze the branch to stabilization work.** Require one immutable candidate
   revision per validation run; do not allow background agents to move `HEAD`
   during a release/audit gate.
2. **Restore a truthful full gate.** Fix the five current Rust failures and
   formatting; add `app/tools/tests` to normal Pytest discovery; convert every
   TUI test return to assertions.
3. **Stop destructive/state-losing defaults.** Fix F-001/F-007 with backups,
   conflicts, and atomic writes; make F-002/F-008 fail visibly without erasing
   recoverable state.
4. **Unify exit semantics.** A failed publish/report/GPU/status operation must
   return a failed `CommandOutcome` regardless of human/JSON rendering.
5. **Validate command modes before side effects.** Enums for run backend and
   ingest mode; strict `key=value` parsing; fake TUI early short-circuit.
6. **Close scientific identity and routing gaps.** Pin and hash MACE image,
   packages, recipe, weights, and backend; isolate fake entries; make `auto`
   consult capabilities before accepting cached results or selecting a backend.
7. **Close unattended query egress.** Restrict federated dashboards to loopback
   unless approved/allowlisted and authenticate every discovered peer.
8. **Fail approval closed.** No receiver means no execution unless an explicit
   trusted-headless capability is provided.

### Short-term — next stabilization cycle

1. Split each high-risk CLI command into a module exposing:
   `validate(input) -> Plan`, `execute(Plan) -> CommandOutcome`, and a separate
   renderer. Make exit code derive only from `CommandOutcome`.
2. Replace non-diagnostic configuration loading with a typed resolver that
   preserves intentional whole-file project precedence and reports the source
   and parse status of every effective file.
3. Define one executable-artifact trust model for marketplace Python, joblib,
   Hugging Face weights, remote payloads, runtime containers, pip bootstrap, and
   Git fallbacks: source, digest, signature, owner, revision, and update policy.
4. Add PTY-level smoke tests for plain terminal, tmux, redirected stdin/stdout,
   resize, mouse, and raw-mode restoration.
5. Add hermetic adapters for platform, HF CLI/jobs, Docker, scheduler, and iroh;
   tests should assert outbound request/process transcripts and exit codes.
6. Make formula parsing total/strict, rename or correct standard-deviation
   descriptors, and make small-sample metrics explicitly unavailable.
7. Benchmark dataset prediction and platform ingest; batch featurization and
   stream uploads/events where measurements justify it.
8. Reduce dependency features and introduce a resolved Python lock for release
   profiles and remote scientific jobs.

### Long-term — recurrence prevention

1. Enforce commit-size/change-domain budgets. Large checkpoint commits require
   staged merges and an independently authored behavioral test plan.
2. Make the release gate a single versioned script that runs format, Clippy,
   full locked/offline Rust all-targets, default plus tool Python tests, PTY
   smoke, advisory scan, and artifact-size budgets.
3. Require change claims to link to executable evidence. Source-string tests do
   not satisfy behavioral claims.
4. Add mutation testing around error propagation, configuration resolution,
   scientific validation, and approval. These paths are especially vulnerable
   to “return default/success” mutations.
5. Preserve machine-readable provenance for commit/test environment, container
   image, model weights, dependency lock, seeds, hardware, and remote results.
6. Run periodic adversarial reviews by a different agent/person than the
   implementer. The repository’s own successful fixes show this practice is
   valuable.
7. Keep WIP/incomplete work in explicitly non-release branches; never land it
   behind a polished capability claim until the production call path is traced
   end to end.

## 18. Code That Should Probably Be Reverted

### Selective reverts/restorations worth considering

1. **`c7fcfe60`, `boot_checks.rs` “run prism login” hunk only.** The tmux
   `image_view.rs` fix is evidence-backed and should stay. The boot-check wording
   should be reverted or rewritten inside the existing surface because it
   violates a deliberate cross-surface policy and makes final HEAD red. Do not
   revert the public-catalog honesty labels.
2. **Automatic remote-wins startup synchronization.** If a safe ownership model
   cannot be implemented immediately, revert the call sites that apply
   marketplace updates automatically and fall back to the explicit install flow,
   which already refuses overwrite. Preserve catalog viewing.
3. **`c8774b02` dependency/module landing, conditionally.** If there is no near-term
   owner and design for production discovery, request routing, authorization,
   relay policy, and integration testing, selectively revert the iroh dependency
   and orphan module. Keep the design/experiment on its feature branch. If that
   integration is imminent, do not revert merely because the seam is unfinished.
4. **Historical SSE parser:** no action; `3e1b792d` already restored the superior
   behavior. Any future consolidation should preserve those exact regression
   tests.

### Changes that should not be reverted

- Do **not** wholesale revert `0bc6fcc7`. It combines known red gates with
  substantive safeguards: deferred-ingest truth, provenance improvements,
  featurizer identity, fake-backend honesty, and compute additions. Revert or fix
  narrow hunks after tests identify them.
- Do **not** revert `bbd88398` by reintroducing a bundled Anthropic provider just
  to make stale tests green. The intended product change is coherent; update the
  two missed tests and remove/optionalize the unused dependency.
- Do **not** revert `51f5260d` wholesale. Its large deletion removed duplicated
  vendored Forge architecture. The confirmed SSE damage has already been
  selectively repaired.
- Do **not** revert `c7fcfe60`’s tmux detection. It is based on a live
  three-binary reproduction and fixes a real shipped-process failure.

For F-001/F-002/F-003/F-004/F-005, no visible earlier public hunk is clearly
superior. These need targeted correction rather than historical restoration.

## 19. Code That Should Be Deleted

Deletion should follow evidence, not aesthetic preference.

### Strong candidates

1. **Production exposure of MACE `backend="fake"`.** Keep the fake backend in
   test-only support, but delete it from agent-facing production schemas and
   environment selection.
2. **Unused Anthropic-specific dependency/config remnants** after confirming no
   plugin/dynamic consumer. `bbd88398` removed the bundled provider; residual SDK
   weight should not remain by inertia.
3. **Source-name/count test machinery** that scans code strings rather than the
   executed tool registry. Replace it with registry enumeration first, then
   delete the regex/count lists and stale names.
4. **Duplicated direct-script test entry points** in `test_tui_e2e.py` after
   extracting a proper shared harness. Do not delete the behavioral checks;
   delete the dual semantics that let Pytest ignore results.
5. **Stale documentation/count claims** in `PRISM.md` and related generated
   surfaces: 43/47 crates, 99 tools, 109 suites, 873 Python tests, active Forge
   crates, and missing campaign statements do not match the repository.

### Conditional candidates

- `app/plugins/thermocalc.py` is an explicitly guarded STUB/example, not a
  deceptive production feature. Keep it under an examples/experimental boundary
  or remove it by product-maintenance choice; deletion is not audit-mandated.
- The iroh module/dependency should be deleted from this branch if integration is
  not scheduled, as described in Section 18.
- Labs submission should be removed from advertised capability discovery while
  it returns `not_implemented`; the code can remain experimental if clearly
  isolated.
- Declared Python packages with no verified consumer should be removed or moved
  to optional extras after dynamic/plugin usage is checked.
- Compatibility/provider branches should be deleted only after telemetry or a
  release policy proves they are unused; this audit did not treat a single
  implementation interface as automatically wasteful.

Build artifacts under `target/` are not repository code. They should be managed
by a disk-budget/GC policy, not committed or deleted as part of this forensic
report.

## 20. Verification Matrix

| Component | Inspected | Executed | Tests inspected | Git history checked | Status | Confidence |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| Repository architecture/docs | Yes | N/A | N/A | Yes | Architecture understood; docs materially stale | High |
| Git history / branch delta | Yes | Git only | History of key tests | Yes, including root/high-churn commits | Review-hostile checkpoints; no malicious pattern | High |
| CLI parse/dispatch | Yes, high-value commands | Representative safe cases | Yes | Yes | Multiple validation/exit defects | High |
| Configuration | Yes | Yes, isolated temporary home | Yes | Yes | High-risk write loss plus Medium invalid-file silence; whole-file project precedence is intentional | High |
| Publish/report | Yes | Yes with fake executables/no credentials | Yes | Yes | False-success confirmed | High |
| Run/compute routing | Yes | Parse/static only; no Docker/scheduler submit | Yes | Yes | Typo/input loss strong | Moderate-to-high |
| Ingest local | Extensive high-value paths | Unit/integration through Cargo; no real model/API | Extensive | Yes | Strong honesty work; mode/provenance gaps | High for inspected paths |
| Ingest platform/status | Yes | Loopback/status and safe pre-upload checks | Yes | Yes | Shape defaults and flag conflict | High |
| Query/provenance | Targeted | Local tests plus safe federated loopback recorder | Yes | Yes | Empty-on-error path and High query-text/SSRF egress | High |
| Tool marketplace/sync | Yes | No real marketplace; isolated/static | Yes | Yes | Destructive policy | High |
| Notebook lifecycle | Yes | No live Jupyter process killed | Yes | Yes | Registry/PID issues | Moderate-to-high |
| TUI application | Targeted | Fake/unit plus 498-test current package; no audit-run live tmux | Yes | Yes | Test boundary weak; `a572070e` render/key test passed | Moderate |
| Python tools overall | Broad anomaly and targeted paths | 1,515 default + 14 separate definitions | Yes | Targeted | Mostly real; discovery gap | High for tests, Moderate for all tools |
| ML features/training | Yes | Pure parser/descriptor probes; suite tests | Yes | Yes | Three Medium scientific issues | High |
| MACE local/fake | Yes | Isolated fake→local cache reproduction; no weights | Yes | Yes | Provenance honest but cache boundary unsafe | High |
| MACE HF remote | Yes | Not submitted | Yes | Yes | Provenance/cache gap | High static, Low live |
| Mesh/iroh | Yes | Unit build/tests; live ignored test not run | Yes | Yes | Transport orphaned | High reachability assessment |
| Agent approval/tools | Targeted security trace | Tests only; no dangerous command | Yes | Yes | Conditional fail-open | Moderate-to-high |
| Server auth | Targeted | Offline tests; no live deployment | Yes | Targeted | Generally fail-closed; token-query risk | Moderate |
| Rust tests/build | Yes | Full all-targets, targeted gate | Yes | Yes | Red at final HEAD | High |
| Rust lint/format | Yes | Clippy and fmt | N/A | Relevant commits | Clippy green, fmt red | High |
| Rust supply chain | Yes | Offline audit/tree | N/A | Lock changes | One advisory; feature bloat | High |
| Python supply chain | Yes | No fresh resolve/install | N/A | Manifest history targeted | Unlocked/floating | Moderate |
| Runtime sidecar / venv bootstrap | Targeted | Not pulled/installed | Tests/source inspected | Yes | Mutable image, broad port map, mutable Python bootstrap | Moderate static |
| Performance | Targeted | Build time/disk measured; no workload profile | Tests reviewed | Targeted | Build/disk and several suspected hot paths | Moderate |
| Non-determinism | Targeted code audit | Seeded pure paths only | Yes | Targeted | Remote environment is dominant gap | Moderate-to-high |
| External platform | Client contracts only | No credentialed/live mutation | Mock tests | Client history | Platform semantics unverified | Low live confidence |

## 21. Unverified Areas

The following areas were deliberately or practically not executed:

1. Real platform login, ticket creation, document upload, graph mutation,
   marketplace installation, billing, and project-tenancy behavior.
2. Real Hugging Face repository creation, artifact upload, HF Jobs submission,
   result retrieval, and model-weight download.
3. Real MACE inference/relaxation/phonon/MD/elastic calculations and numerical
   comparison against reference datasets.
4. Docker container submission, Kubernetes, SSH, Slurm, HyperQueue, GPU
   enumeration, and job cancellation on real infrastructure.
5. Live n0/iroh relay round trip; the repository test is explicitly ignored and
   network-dependent.
6. PTY-level TUI execution under tmux/screen/kitty/sixel during this audit. The
   new commit’s author supplied measured evidence, but it was not independently
   reproduced here.
7. Full browser dashboard, VS Code extension, notebooks in an actual Jupyter
   process, and JavaScript UI build/test gates.
8. Every one of 357,234 counted Rust/Python/TypeScript source lines manually.
   Coverage was broad and
   risk-directed, aided by graph/search/history and full suites; it was not a
   formal proof or complete line-by-line verification.
9. Pre-`a56d8229` behavior hidden by the squashed public import, including the
   original user prompts/instructions that motivated initial architecture.
10. Platform-side unauthenticated catalog endpoints documented in `c7fcfe60`.
    They are outside this repository and require a separate API/tenancy audit.
11. License/legal correctness of model weights, third-party services, and source
    available dependencies beyond obvious code comments.
12. Exploit demonstration for joblib path/pickle or absent approval channel. No
    malicious artifact or dangerous command was executed.
13. Fresh Python dependency resolution across supported OS/Python versions,
    because the project has no single lock to reproduce and network installation
    would mutate the environment.
14. The mutable runtime sidecar was not pulled/run and the venv recovery paths
    were not allowed to download/execute bootstrap code.

The five pre-existing untracked files present throughout the audit—
`4H-SiC_kappa_100_1000K_fit_evaluated.png`,
`NbMoTaW_oqmd-6223708.cif`, `comparison.png`, `proposals_dump.json`, and
`proposals_err.txt`—were treated as pre-existing user artifacts and not
modified or evaluated as repository code. This audit report is the sixth
untracked file at final handoff.

## 22. Final Verdict

### 1. Is PRISMA trustworthy enough to continue developing?

**Yes, as a stabilization branch; no, as a production/release candidate.**

There is a real and valuable system here. The ingest, provenance, orchestration,
scientific-tool, mesh, and TUI layers are not mere scaffolding. Recent code
contains unusually strong self-audit and scientific-honesty work. Rebuilding the
entire product would discard more verified value than it would remove risk.

However, a CLI that can overwrite config after a parse error, claim that failed
reports/publishes succeeded, silently route an unknown backend to local
execution, and pass test functions whose internal result is false cannot be
called operationally trustworthy. The final branch also fails its own complete
Rust gate.

### 2. Are there major hidden regressions?

**Yes.** The most important are hidden not by obfuscation but by the difference
between printed prose and machine outcome, default-empty fallbacks, selective
test discovery, and isolated component tests. The historical SSE failure proves
that a high-volume refactor can both lose behavior and lose the tests that would
have caught it. The tmux incident proves that hundreds of green object-level UI
tests can miss the shipped process.

The audit did not find a hidden trigger that initially selects a fake-physics
engine. It did find a production-reachable fake backend whose cache entry can
satisfy a later real request (F-015), plus automatic routing into unsupported
platform operations (F-017). Remote environment/model identity and fallback
feature/statistical behavior add further plausible-but-invalid pathways.

### 3. Is there evidence of systematic degradation?

**There is evidence of a systematic development failure mode:** oversized
heavily AI-assisted changes, parallel configuration/runtime systems, local fixes that
miss sibling paths, and gates that validate packages rather than the whole
product. That pattern has repeatedly degraded functionality and auditability.

### 4. Is there credible evidence of deliberate sabotage?

**No.** The repository evidence supports ordinary bugs, incomplete integration,
requirement misunderstanding, architectural drift, and AI-agent overproduction
far better. The strongest sabotage-like historical event was repaired and
documented. Recent commits actively add refusal, provenance, uncertainty, and
honesty checks; known failures are sometimes explicitly disclosed. No motive can
be inferred responsibly.

### 5. Which five issues should be fixed first?

1. **Configuration and executable-tool integrity (F-001/F-002/F-007):** refuse
   malformed writes, preserve intentional precedence, use backups/atomic writes,
   and never overwrite local tool code silently.
2. **Truthful, typed execution routing (F-003/F-004/F-005/F-006):** one outcome
   contract plus a mutually exclusive backend/mode/input matrix.
3. **MACE validity (F-009/F-015/F-017):** isolate fake cache entries, pin/hash
   the remote environment/weights, and select only capable backends.
4. **Federated-query privacy (F-016):** loopback/approval/allowlist rules and
   authenticated, identity-bound peers.
5. **Release-test integrity (F-012/F-013/F-014):** full workspace gate, default
   tool tests, real assertions, and PTY/fake-hermetic smoke tests.

Approval fail-closed and joblib containment should be handled in the same
immediate security pass rather than deferred behind feature work.

### 6. Which parts should the owner personally review?

1. `crates/cli/src/main.rs` command dispatch and the intended meaning of exit 0.
2. `crates/cli/src/tool_sync.rs`, because “remote wins silently” is a product/data
   ownership decision, not merely an implementation detail.
3. `crates/agent/src/command_tools.rs` and `handle_federated_query`: decide which
   “read-only” tools may disclose data or reach arbitrary hosts.
4. The entire MACE chain: primitives, backend capability selection, job runner,
   cache, payload pins, model weights, results, and provenance.
5. `crates/agent/src/agent_loop.rs` approval policy for headless/library use.
6. `c8774b02` and mesh architecture: decide whether iroh is a committed product
   direction and, if so, how authorization/discovery integrate.
7. The release gate and commit policy. This is the leverage point that allowed
   most findings to coexist.

### 7. Continue, selectively revert, or rebuild?

**Continue from the current lineage, freeze feature work, and selectively
revert/fix.**

- Keep the current branch as the evidence-bearing stabilization base.
- Preserve the tmux fix and scientific honesty work.
- Rewrite/revert the new boot-check exit instruction.
- Disable/revert automatic destructive tool synchronization until it is safe.
- Remove iroh from this branch if no production integration is imminent.
- Do not wholesale roll back `0bc6fcc7`, `51f5260d`, or the provider removal.
- Rebuild only narrow subsystems whose contracts are fundamentally inconsistent:
  configuration resolution/write lifecycle, command outcome/exit rendering, and
  scientific artifact/cache/backend identity.

The decisive conclusion is: **PRISMA suffered material integrity degradation
during heavily AI-assisted development, chiefly through incomplete global
reasoning and insufficient whole-product validation. Git evidence cannot assign
those defects cleanly to agent output versus human decisions/review. It does not
support deliberate sabotage.**

## 23. Adversarial Reassessment

### Method

After the first-pass report was complete through Section 22, three independent
reviewers were asked to attack it from different directions:

1. try to falsify the “no sabotage” conclusion and search for missed fake,
   selective, or systematically degrading behavior;
2. challenge severity, causality, and remediation, looking specifically for
   overstatement and simpler explanations;
3. fact-check every material count, path, revision, reproduction, and
   current-versus-historical claim against final `a572070e`.

The review was not a prose-only exercise. It traced additional production paths,
ran a safe federated-query loopback recorder, constructed an isolated fake→local
MACE cache hit, reran the final full Rust workspace gate, and executed the
current TUI tests. No production file was modified.

### Findings that survived

- **F-001, F-003, F-005, F-006, F-007, F-009, and F-012 remain High.**
  Their mechanisms and reproductions survived direct challenge.
- F-004, F-008, F-010, F-011, F-013, and F-014 remain real mechanisms, but their
  final severities were reduced as described below.
- The historical `51f5260d` SSE regression/test-loss episode remains the
  strongest sabotage-like sequence, but its focused restoration in
  `3e1b792d` and candid explanation still favor an incomplete mega-refactor.
- The high-churn/monolithic architecture and selective-gate diagnosis survived.
- The scientific code contains both genuine integrity safeguards and genuine
  plausible-wrong pathways; neither side of that assessment was rejected.

### Findings downgraded or corrected

1. **F-002: High → Medium; first-pass overlay claim rejected.** Whole-file
   project replacement is explicit and tested. Only invalid-file silence remains
   a defect.
2. **F-004: High → Medium.** The support-report statement is materially false,
   but it is not execution compromise or application-data corruption.
3. **F-008: High → Medium.** Registry loss and ignored kill status are real;
   unrelated-process harm requires PID reuse/shared-host conditions.
4. **F-010: conditional High → Medium.** Standard shipped callers supply an
   approval receiver; risk is in custom/future embedding.
5. **F-011: conditional High → Medium.** Path escape plus joblib loading is a
   dangerous primitive, but code execution additionally needs a malicious
   pre-existing artifact/untrusted writable directory.
6. **F-013: High → Medium release blocker.** Five failures make the branch
   unmergeable, but four are stale/known expectations rather than direct
   production regressions.
7. **F-014: High → Medium.** The real command is
   `prism tui --fake-backend`; it promises no subprocess/network/LLM, and
   pre-dispatch setup violates that hermeticity. Production damage was not
   demonstrated.
8. **Iroh risk: High → Medium architecture/bloat.** The transport is orphaned
   and its commit claim is overstated, but unused experimental code does not
   itself degrade the active transport.
9. **Thermo-Calc deletion recommendation withdrawn.** It is an explicit,
   import-guarded STUB/example absent from normal bootstrap; deletion is a
   product-maintenance choice.
10. **`gpus` reclassified.** JSON-error-plus-exit-0 is documented and explicit,
    so it is unconventional exit-contract drift, not a silent failure.

Fact-check corrections also changed final metadata to 15 commits / 187 files /
+18,122/−1,834, 821 visible commits, 357,234 counted source lines, 442
Claude-coauthored commits, 3,552 Rust test attributes, and the correct
`crates/agent/src/protocol.rs` path.

### Findings upgraded or expanded

1. **F-005 strengthened.** Target-first dispatch means explicit
   `--backend local/platform` plus SSH/Kubernetes/Slurm flags silently becomes
   BYOC; typo→local is only one direction of the routing defect.
2. **F-009 retained High.** Remote weight/environment identity remains absent
   from provenance/cache identity. The related fake/real collision was separated
   as F-015 because it has its own direct reproduction and remediation.
3. **Test-gate evidence strengthened.** The final `a572070e` workspace run
   confirmed exactly five failures across three targets; the current TUI package
   is independently green at 498 tests, so the report no longer conflates the
   two.

### New High findings

1. **F-015 — fake-to-real MACE cache reuse.** `fake` is agent-visible; all
   backends share a key/storage namespace; cache acceptance occurs before the
   requested backend runs. A seeded fake energy was returned for a later local
   request. Primary provenance remains labelled fake, but a downstream structure
   resolver drops it.
2. **F-016 — federated-query text egress and SSRF.** A non-approval, read-labelled
   tool accepts an unconstrained model-provided dashboard URL and trusts its peer
   list. Loopback reproduction observed literal query POSTs. No current
   platform-token leak was demonstrated.
3. **F-017 — automatic MACE routing selects unsupported platform operations.**
   Platform is preferred for GPU-bound elastic/phonon/dilute tasks, while that
   backend rejects them and `JobRunner` does not perform the documented fallback.

### New Medium supply-chain findings

- **F-401:** mutable `marc27-runtime:latest` is auto-pulled and its port is
  published on all interfaces rather than loopback.
- **F-402:** venv recovery can execute mutable `get-pip.py` and fall back from a
  versioned wheel to unpinned repository main.

Both are openly documented convenience/self-healing choices, which is strong
mundane counterevidence even though the integrity risk is real.

### `a572070e` reassessment

The final commit is positive counterevidence, not a new degradation:

- it records whether the sidebar was actually rendered;
- reroutes a key from an invisible pane into visible input;
- adds a render-plus-key behavioral regression test;
- corrects false “session Structures” wording;
- candidly documents the remaining one-frame stale-footer lag.

Its targeted test and the full 498-test TUI package pass. It does not repair the
five failures elsewhere in the full workspace gate.

### Final adversarial verdict

The second pass found more serious degradation than the first pass in the MACE
and federated-query boundaries. It also removed unsupported or inflated claims.
Those changes offset each other at the repository level:

- **Overall assessment remains C.**
- **Confidence that material integrity degradation exists remains High.**
- **Confidence that deliberate sabotage is unsupported is reduced from about
  85% to about 80%, but the conclusion does not change.**

Why the verdict did not rise to D or E:

- fake provenance explicitly says fake/no weights rather than disguising itself;
- the cache collision applies uniformly, not under a selective hidden trigger;
- unsupported MACE operations fail loudly with an explicit
  `NotImplementedError`;
- federated-query comments acknowledge prompt-injection risk and earlier commits
  removed actual platform-token leakage;
- mutable bootstrap/runtime choices are candid availability shortcuts;
- `a572070e` and other recent commits measure live failures, add behavioral
  tests, and disclose residues.

The adversarially tested conclusion is therefore narrower and more defensible:
**the repository shows systematic integrity degradation caused by process and
architecture during heavily AI-assisted development, but not a coherent pattern
that supports deliberate sabotage.**
