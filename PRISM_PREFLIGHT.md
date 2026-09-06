# PRISM pre-flight check

**Build under test:** `prism 1.1.0`, binary `62482aa94fb9`, branch `feat/composability-seam`, installed 2026-09-06 08:50.
**Checked by:** an end-to-end run on the installed binary, the full test suites of every crate touched, and the research runs of 5–6 September used as the live workload.
**Verdict in one line:** ready for a supervised beta with a named operator watching; **not** ready to run unattended, for three specific reasons listed under Blocking.

---

## 1. Green — verified this session

| # | Item | Evidence |
|---|---|---|
| 1 | Binary builds, installs and runs | `cargo build --release` clean; delete-then-copy install; `prism --version` answers |
| 2 | End-to-end task on the installed build | Prompt → `hea_descriptors` → correct Ω 2.164 / VEC 8.52 for Ni60Co12Cr12Al8W4Mo4, matching the stored record; turn closed in 16 s |
| 3 | Test suites, all crates touched | prism-tui 712, prism-agent 865, prism-python-bridge 33, prism-ipc 20 — 0 failures |
| 4 | Lint and format | `cargo fmt --check` clean; `clippy -D warnings` clean on tui, agent, bridge, ipc |
| 5 | Consent is informed | Approval popup shows the action's arguments; Enter never approves; a destructive call requires the tool name typed in full |
| 6 | The human can take the turn back | Esc cancels a running turn; a mid-turn message is held, not lost, and dispatched once |
| 7 | The deadline no longer silences the writer | `file` and `apply_patch` survive the time budget; search and compute are refused |
| 8 | A looping tool is stopped | Six consecutive same-tool calls differing only by a number are refused, with the reason in the model's own slot |
| 9 | Interface survives a narrow terminal | Workspace becomes an overlay below 100 columns; nothing is dropped |
| 10 | Keyboard reaches everything the pointer reaches | Line cursor opens references, marks and asks, with no mouse |
| 11 | The frame holds still while the model streams | Fixed-width footer slots; reserved strip row; verified live (`tok/s: —  cost: —` when idle) |
| 12 | A session is not destroyed by one keystroke | Backspace no longer starts a new session; the palette route asks first, naming what is lost |
| 13 | Long work reports its age | Elapsed time on the footer pill, the running tool row and the spinner |
| 14 | Evidence classes reach the operator | RED/YELLOW/GREEN badges on every card; source tables carry counts and totals |
| 15 | Provenance is durable | 61 MACE jobs, all QE runs and every tool call recorded and queryable after the fact |
| 16 | Licence and account walls reach a human | Patents backend, CNKI and similar surface as a "needs a human" task with what to obtain and where |
| 17 | JavaScript pages no longer read as empty | `web read` escalates to Obscura automatically and names it in the result |
| 18 | Quantum ESPRESSO reports honestly | Real exit code and stderr on failure; cutoff from the pseudopotential set's hints; one OpenMP thread per rank; pinned-cutoff source reported |
| 19 | Dead jobs do not read as running | A job whose owner process is gone is marked interrupted, with the step it reached |
| 20 | Science sidecar answers | CALPHAD tool no longer dies on a dispatch type error |

## 2. Amber — works, with a stated limit

| # | Item | Limit |
|---|---|---|
| 1 | DFT on real alloys | Not achievable on this hardware. Only 1–2 atom cells converge; a 16-atom, 6-element cell did not converge in 2 h on 12 cores. Belongs on a cluster or out of the screening loop. |
| 2 | Cluster expansion + Monte Carlo | Works well (10 alloys, all fits under 15 meV/atom). Rigid-lattice only: no vibrational or magnetic entropy, no relaxation beyond the fitted lattice. |
| 3 | MACE potential | Foundation model, PBE-trained. No combustion chemistry — it cannot address ignition, which is the property that decides the oxidizer side. |
| 4 | Prior-art search | 10 of 15 compositions assessed; verdicts never independently checked; no structured patent database was queried. |
| 5 | Obscura | Integration is committed and tested; the binary is not installed, so the fallback currently degrades honestly rather than rendering. |
| 6 | Time budgets | Now safe (the writer survives), but a run still loses unfinished compute when the clock stops. |
| 7 | Credits | The account is overdrawn; hosted delegation is unavailable and several runs hit usage limits mid-task. |

## 3. Blocking — fix before unattended operation

| # | Item | Why it blocks | Fix |
|---|---|---|---|
| 1 | Two unexplained faults | An `mpirun` launch died in 54 ms inside the tool server with no diagnosis, and a torch model-load race killed 6 of 27 MACE jobs across the session. Both are silent-ish failures in the compute path. | Reproduce each under load; the model-load race has a lock in `make_calc` that does not cover the elastic path. |
| 2 | No CALPHAD database | The single most valuable missing computation: phase fractions, σ/μ risk and solidification cracking cannot be checked before a casting decision. | Install a database and wire `calphad` to it. |
| 3 | Unattended approval policy is unproven | Every research run tonight needed a human to answer approvals. There is no tested policy for which tools may run unwatched. | Define and test an allowlist; the destructive tripwire already forces a human, so this is about the routine cases. |

## 4. Recommended beta shape

Supervised beta: one named operator watching a session, answering approvals, with the time budget set and the workspace visible. Under that shape everything in section 1 holds and the amber items are stated limits rather than surprises. Unattended running waits on the three blocking items.

## 5. What tonight's workload proved

Eleven research runs on the real product produced a 657-line reviewed document and a candidates report, including a full cluster-expansion and Monte Carlo study of ten alloys whose numbers were checked against the tool records before publication. Three runs lost their written section to a deadline defect and one lost its budget to a tool loop — both are now fixed, tested and committed. That is the honest measure of readiness: the product did the work, and its failures were findable, reproducible and repairable within the session.
