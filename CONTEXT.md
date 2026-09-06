# CONTEXT — resume here

**Current Task**: Systematic find-and-fix across the crates for six recurring
defect classes (muzzles, lying signals, silent data loss, gates at the wrong
seam, tests that prove nothing, measurement hygiene). Four Opus hunters
produced ranked findings with file:line; fixes land one at a time with a
mutation-tested test each. Prior-session WIP is preserved untouched on `754eccca`.

## Already fixed — do NOT re-fix
- arXiv-only fetch in `papers.rs`: GONE. `full-text`/`claims` now build the
  engine with `parse_sources(&None)` = all sources (`papers.rs:367`, `:479`).
- `--base` refusing DRAFT shards (`5b10e6b6`); relations dropped at the fold
  (`238c8fd9`); exactMatch merge (`44a373e0`, pinned `14b12a35`); fold dropping
  `fact_kind`/`sign_domain` (`6523eae3`); arXiv text bleed (`74f3a2ad`);
  namespace read-compat (`2c95ad2f`); search engine self-strikes (`ec74c992`);
  one bad file ending the corpus (`8f0c8e57`); config silent-ignore + whole
  replace (`ef8667a4`); search/recall withhold (`65d84c17`); recall divisor
  (`5539eaed`); streaming usage None (`0a04aaf1`); patents dropped from digest
  (`02715a3a`); warn! discarded by default (`ea21414c`); deny-error → Allow
  (`8641226b`); synthetic backend labelled validated (`c73643e9`); XML entities deleted (`2e243e58`); reward sign from substring (`e9b373a8`); "Not done" scored done (`372194d0`); audit append lied (`96a7a8bc`); short vision recovery rejected (`57ce345e`); vision page loop discarded pages (`7b1cccd8`); error objects read as success (`e34df92c`); failed fetch = absent (`5c81e3e2`); identity loss silent (`dd6fb995`); claims probe + exit 0 (`e38f3f57`); governance wiped provenance (`67f606cc`); owners file honest+atomic (`5cfdfdec`); reasoning leaked into content (`60f39bee`); narrowed recall counts withheld (`98e6144c`); Kafka probed at its host (`a39860f4`); live-store guard armed for cli/frontend (`ee58a885`); papers/reverify built an unused venv, 65 s→7.5 s (`03bef50f`); policy-load lie (`43bef019`); sidecar crash reported as timeout (`1f5bea58`); DAG budget per wave (`6459675f`); untyped base relation note (`189b485c`). Playbook §13 has the full ranked list with ✅ marks. The Aug 24 promoted
  `~/Downloads/prism-ontology-shards/ontology-polymers.ttl` PREDATES all of
  these — 8 copies of every EMMO upper class, zero typed relations. Re-fold with
  `~/Downloads/prism-ontology-shards/fold.sh`; review; only then promote.

## Key Decisions
- Park only what a missing capability actually blocks; never discard sound work.
- No synthetic health probes: the real read is the health signal (K=3 remote failures).
- `polymers.ttl` (6542 classes / 6172 relations) PROMOTED 1 Sep on owner say-so; installed at `.prism/ontologies/polymers.ttl`.
- Ontology binds AFTER extraction; `value`/`unit` are never required fields.

## State 2026-09-02 01:40
- `cargo clippy --workspace --all-targets -D warnings`: CLEAN (0 warnings) at `cc94a5dd`.
- End-to-end LitXBench run DONE 2026-09-02: unmuzzled 0.3625 vs baseline 0.3789 (19 papers, like-for-like, inside run-to-run noise); 4-paper A/B shows reasoning replay cuts malformed tool calls 23→8 at equal F1. Details playbook §13.10. Was: baseline `out-current` overall F1 0.3789 (19 papers). Outputs → `~/Downloads/prism-gold-eval/harness/out-unmuzzle/` (local, never the repo); scratch store `$SCRATCHPAD/litx/prov.db`; JATS served by `python3 -m http.server 8765` in `harness/jats/`; runner `$SCRATCHPAD/litx/run_rest.sh`; score with `PRISM_OUT_DIR=out-unmuzzle ../.venv/bin/python score.py`.

- reasoning-content replay landed (`4e83c197` mechanism, `d5d7fae4` config knob, default OFF). A/B DONE (4 papers): replay cuts malformed tool calls 23→8 (baseline 11), turns/calls down, F1 unchanged, fewer claims (80 vs 105/115). Default stays OFF; owner decision: provider-aware default for z.ai (playbook §13.10).

## Demo Friday 2026-09-04
- Replay default ON (`864756b4`); TUI log to file (`40a457e4`). Forks: mesh-over-iroh, AG-UI (structure view, marks, openable entities). Owner: top up credits (-73.4 cr), rotate pasted Gemini key, launch `run_gemini.sh` from own shell. Write the demo script Thursday.

- Pushed to origin+private 2026-09-02. Fork branches awaiting audits: `feat/mesh-iroh-transport` (f4863fba), `feat/agui-tui` (859de78f); worktrees under `PRISM/.claude/worktrees/`. Owner: tag v1.1.0-alpha.1 + push to private; set default branch.

- 2026-09-02 pm: WAL warn once/store (`7c5a6d3c`), no default tool-call ceiling (`e688a0e9`), DGM log line (`8d584378`). Mesh + AG-UI forks: both audited DO-NOT-LAND, findings sent to writers. `hea_descriptors` works; red badge = ungrounded input, by design.

- Fork state 2026-09-02 14:10: mesh `feat/mesh-iroh-transport` @0ee47fb7 — two re-audits DO NOT LAND/FIX FIRST (identity-plumbing regression, credential relaxation, cancellation, knobs, fan-out honesty); writer on round 3; DESIGN-ONLY Friday. AG-UI `feat/agui-tui` @9a1b2b52 — my gate green (1,322 lib + 284 cli bins, clippy clean, merge preview clean) but my tui-driver walk found a home-screen typing regression (keys eaten as navigation; raw echo after view switch); re-audits: CIF FIX FIRST (multi-block merge, unterminated `;` → 0 sites, occupancy 0 → atom), marks FIX FIRST (slot wires untested, id injection, no bound, retraction gaps). Writer has all three rounds queued. DEMO BINARY = installed 10:22 build of THIS branch.

- 2026-09-02 pm (later): execute_bash guard hardened (`1eb3b03f`: program name judged, wrappers peeled, root/.git/.prism protected, find/xargs/awk inner validation, wipers refused; 19 of 20 rm -rf shapes passed before). Probe fix cherry-picked `0fa687be` (silent terminal never asked) — AUDIT says it adds a 2 s stall on silent terminals/tmux and can still eat a key over slow ssh: writer doing a follow-up commit (P1–P4) for re-cherry-pick. `prism resume` logs to file `ebbc4599`. gitignore docs re-includes real `d4171585`. Resume defects REPRODUCED: history not shown after resume, picker dates +27d/−2h, header stuck "fetching sessions" → with pi (Qwen 3.8 max, `pi -p`, worktree `/Volumes/Samsung SSD 1TB/pi-resume-wt`, report /tmp/pi-resume-report.md) together with the 11 pre-existing `requires_approval is False` failures (default None since `8763ad26`). Audits: interaction graph ×2 FIX FIRST (frame refusals absent, hull differentiable via fit/hessian, MP_FRAME constant, energy_kind default, spinodal grid bias) → writer fixing before round 2 (DFT collaborator interface); AG-UI FIX FIRST (P1–P4 probe, S1–S7 structure) → writer; mesh round 3 (3 commits, 33/33 mutants) → re-audit running. Second audits (AG-UI B, mesh F) after fixes. Disk: every writer/auditor on its own target; internal 26 GiB, SSD 60 GiB; demo binary still the 10:22 build → reinstall after pi + probe follow-up land.

- 2026-09-03: CREDITS RAN OUT mid-session; five Fable agents died at once. Work
  preserved as WIP commits before anything else (AG-UI `1da413d5`, mesh
  `f20f9bac`). Landed since, each gated: pi's four resume/approval fixes
  (`52f148fb`, merged `c6a96ff5`), `prism --resume` actually resuming
  (`e02bc527`), the resume notice as a system line not model output
  (`e4c7698f`), AG-UI round 4 merged (`89f5a265` → `30cb6589`) and its three
  remaining defects fixed (`c90d407f`: unit column 12→14 so `electrons/atom`
  is not clipped, origin falls back to full-width lines below ~24 columns
  instead of cutting words in half, width-aware header). Demo binary
  `prism 1.1.0` installed 11:31 and driver-verified: resume restores history,
  picker dates correct, source table and descriptor card read at 140 columns.
  HAZARD REPEATED: my first AG-UI gate was green from a SHARED cargo target
  (demo branch's binary); two tests had been failing all along. Isolated
  target since. Open audits recorded in `AUDIT_FINDINGS_2026-09-03.md`:
  interaction-graph fix round FIX FIRST, DFT collaborator interface DO NOT
  LAND (arbitrary write via the `system` argument, tar-slip, no ingest
  containment, frame check defaults to self-certification). Neither merged.
  Mesh round 4 (F1 refusal-path leak, F2 dead 429) queued, design-only for
  the demo.

- 2026-09-03 pm: hover-panel complaints reproduced in the driver and fixed
  (`dc5d8bbd`, `b209ae30`): the panel was a fixed 56/64/92 columns regardless
  of terminal size, `clip`ped every line to an ellipsis, had NO scroll state
  (so arrows moved the list BEHIND it and the panel went stale while looking
  live), and capped sources at 3 behind a "+1 more" no key could reach. Now:
  flexes to the room available (max 104), wraps (hard-wraps unbreakable
  tokens), owns Up/Down/PageUp/PageDown/Home/End while open, position in the
  title (`↑↓ 13-22/40`), every source listed. Verified on the installed
  binary at 140x30/20/12. RESIDUAL, said not hidden: only the lower section
  scrolls, so on a very short terminal the structure panel's header still
  collapses to "+N more lines · cell not drawn" and those lines are not
  reachable by key; the drawing sits between top and bottom as a Canvas, so
  one continuous scroll needs the composition reworked. INSTALL RULE: always
  delete-then-copy (see memory install-must-replace-not-overwrite) — an
  in-place overwrite makes macOS SIGKILL the binary (exit 137) while the same
  bytes run elsewhere.

- 2026-09-04 (demo day): "answer never arrives" ROOT-CAUSED — it did arrive; the
  transcript pinned the prompt at the top (deliberate earlier choice) and the
  answer sat below a 37-item tool card, unseen; End does nothing in prompt
  focus. Fixed `fd23f8fa` and DRIVEN LIVE on the installed build: the answer
  ("Search done — two queries, 67 unique papers", 8 ranked hits with DOIs)
  is on screen with no scrolling. Fixed: the view follows the tail; the question stays in
  the title bar (live) and the sidebar. Two older tests refined to that
  contract, one tiny snapshot moved. Headless `prism research --depth 0` prints
  the full answer (34 papers) — the loop was never the problem. SOURCE TABLE for
  searches now real (`e0301691` tool declares sources; `330c35fc` card extractor
  reads a `sources` array; `94017dcc` the tool-result event carries the raw JSON
  beside the model's digest): nine databases named per search with per-source
  counts, `—` for a source that did not answer vs `0` for searched-and-empty.
  Hover panel: flex width ≤104, wraps (no ellipsis), scrolls (Up/Down/PgUp/
  PgDn/Home/End, position in title), every source listed (`dc5d8bbd`,
  `b209ae30`); transcript stops writing its last column under the scrollbar
  (`a69fa3cc`). HARNESS LESSON: `cargo test <filter>` is a SUBSTRING, not a
  regex — a regex filter ran zero tests and read as "mutation survived";
  gates must check the mutation exit and that ≥1 test ran. STILL OPEN for
  research-readiness: prior_art_search declares no evidence class
  ([unclassified]); kind column clips ("peer-reviewed literature meta…");
  the footer tag is FIXED (`82128837`: "[Ctrl-T: show reasoning]", an action
  not a state; verified by test + accepted snapshot, not driven live — the
  fake thinking_stream detour hit two NEW traps instead: on the home screen a
  sentence starting with a lowercase hotkey letter (s/t) opens a panel instead
  of typing, and the Status window does not close on Escape); the header
  collapses to "+N more lines" on
  very short structure panels with no key to reach them; two Semantic Scholar/
  ChemRxiv sources returned no answer in every run today (`—`) — provider-side,
  now visible. Interaction-graph audits (FIX FIRST / DO NOT LAND) unchanged in
  AUDIT_FINDINGS_2026-09-03.md; mesh round 4 unstarted (writer dead, WIP kept).

- 2026-09-05 00:03: `1beb141d` — background work is on screen (`ui.activity`
  channel: id/text/done; announced today: neural tool-index warm, embedding
  model load, all three compaction sites; `announce_activity` works from a
  spawned task) and `recall` NEVER refuses (below 10% budget it serves a
  ≥2,000-char slice with a `budget_note`; the refusal path is deleted, not
  knobbed). ERROR CENSUS (last 7 days of sessions): 26/80 tool results were
  errors — 14 recall refusals (ALL the retired cumulative-budget bug, last on
  08-31) and 6 worker-timeout/desync (ALL before the no-ceiling fix on 09-02);
  since 09-01 the only tool errors are the model's own apply_patch/notebook
  mistakes and one WARN leaking into a papers_ingest result. LIVE (00:10):
  follow-the-answer and the Ctrl-T wording confirmed on the installed build.
  DONE `30105116`: activity strip on its own row above the prompt (footer clips at 140 cols).
DONE `f87cf6f6`: footer word derives from the turn (busy/working/Ready; text-flush wrote Ready mid-turn); footer trims optional groups so "Ctrl-C quit" survives narrow columns.
DONE `0c5e3ab3`: search digest carries "databases asked" + "branches" lines (model burned 4 recalls to name databases); recall declares durable memory as its source (was SOURCE NOT REPORTED).
DONE `49793a4e`: summary never carries the tool name (was 'recall: recall: 3 results'); sidebar tool rows carry the result's first line; search results + papers CLI declare evidence class/sources; recall inherits class; home shortcuts are shifted letters; table cells inline-parsed.
DONE `c275c49e`: OSTI.GOV is the tenth literature source (page/rows paging; total only in a header → available None).
DONE `f92aea48`: palette rows show what Enter does (was 'palette'); key window offers Semantic Scholar/Lens/patent-table keys + palette entry search.keys; find_tools declares tool catalog; recall labels tool-less records by action type; footer drops credits before the quit hint.
DONE `bb3e6fdd`: Magnitude (local inference server, 127.0.0.1:10100/inference/v1) is a keyless loopback provider, swept like Ollama; key-window labels shortened. NOTE: /tmp/panel-target hit 55 GB and filled the disk (linker failed silently) — cleared incremental + stale deps; watch `df`.
DONE: S2 rate-limit message names the palette entry (Ctrl-P → Search sources & keys).
DONE `409d0849`: papers engine in the palette (search/sweep/full-text/corpus forms → /papers …, CLI-backed root, JSON presenter); palette titles clipped to their column.
DONE `18bb282f`: parity batch 3 — 13 palette entries (ontology list/bind/relations/validate/promote/proposals, provenance stats/failures, reverify list/run/history, matkg load, predict); five new CLI-backed roots; single-body ui.view renders as one tab (was empty for doctor/providers/billing/papers); palette title/hint cells clipped.
DONE `2f5c97fa`: parity batch 4 — schedule list/create/cancel, discourse list/run, publish, report, plugins list (8 entries, 5 forms). All 47 CLI subcommands now have a palette or slash route except daemon-only ones (ipc-serve, backend, setup, tui).
DONE: view panel wraps long lines (was cut at the edge).
DONE `4078e6b0`: panels wrap at spaces. DONE (tools): MACE tier accepts every fcc/bcc/hcp element (tables derived from ASE); LPBF map's evidence class follows property_sources (unsourced → RED, gaps named).
DONE: disk — /tmp/panel-target debug wiped (46 GB) + brew/npm/uv caches; 52 GB free.
DONE `45b8086b`: QE is a standard run — `prism provision qe` builds pw.x (7.4.1) into ~/.prism/qe + PseudoDojo set with manifest; runtime + qe_run/qe_status tools; `prism qe status|settings|run`; palette QE status/settings/run; `/tools reload` hot reload; PROMOTE + KEEP WORKING directives. Live: Si scf 4.3 s.
DONE `fe27d7d4`: settings hub — palette "Settings" → ten large tiles (Model, Search keys, Compute & QE, Approvals, Display, Billing, Account, Nodes, Config, Tools) opening the real windows; meters reset with /new; footer says "overdrawn" on a negative balance.
DONE `6730d420`: destructive words (rm/rmdir/delete/drop/…) no longer abort the model's call — they FORCE a per-call human decision (no auto-approve, no session "always", reason on the popup, 'a' approves this call only). bash.py still refuses `.git`/`.prism` — loosening that is the owner's call (classifier blocked it).
DONE `1d86412c`: own inference does not fail — empty stream → one plain request + streaming retired; `[[fallbacks]]` in ~/.prism/config.toml tried on transport/5xx/429 (never on 4xx, never after partial text); switch announced in the activity strip. README documents it.
DONE `68f85341`: balance refreshes itself after 2 idle min and says "stale" when a fetch failed; "Search sources & keys" opens on Semantic Scholar.
DONE `b493e160`: `prism use fallback add|list|clear` (+ `/use fallback …`, palette "Add fallback model" form, "Fallback models", "Clear fallback models"); `use show` names the fallbacks.
DONE `6d1fd1e6`: palette "Sign in with enterprise SSO" states `prism login --sso-domain` / `--sso-provider-id` (PRISM-side SAML exists; IdP/SAML connections are platform-side, outside this repo).
DONE `98b815fa` `4ce38e3a`: form notes wrap under their column (or on indented lines when cramped); the form modal is as tall as its lines (the key-hint footer was being cut).
DONE `e4137a21`: eastern literature routed by language (model passes `queries` {ru,zh,ja}); OpenAlex per-language = the reachable Chinese/Russian/Japanese source; concurrent under a 45 s deadline; CyberLeninka cached 24 h + retried; archive texts-only + relevance; eastern sources declared to the table. Live: 20 results in 1.9 s.
DONE `d580935b`: licence/account walls → `needs_human` in the tool result → announced once per source → TUI toast + transcript link + palette "Needs a human (N)" panel (what to obtain, where, why).
DONE `0e40783f`: research turns have a clock — `prism research --budget-min` (default 20) → PRISM_TURN_DEADLINE_EPOCH_MS → AgentConfig.turn_deadline_epoch_ms; past it: one system "synthesise now" message, no tools, branches inherit. Measured before: depth-1 run made 59 tool calls in 15 min and was killed with no answer.
DONE `02026b2d`: OpenAlex one-letter titles dropped. LIVE VERIFIED: needs-human wall (cnki) → toast + transcript link + "Needs a human (1)" panel; model supplied ru/zh/ja translations unprompted in the DAG run.
DONE `501f57db`: past the deadline a requested tool is REFUSED (well-formed tool result), two refused rounds end the turn; `prism research` progress lines carry elapsed seconds + activity lines. DATA (provenance, run 2, budget 8): all 87 lane tool calls in minutes 0-6, orchestrate_agents returned at 604 s, then the PARENT's synthesis ran >3 min with no output until the 780 s kill — JSON mode prints nothing until TurnComplete.
DONE: PROOF — `prism research --depth 1 --budget-min 6` ended by itself in 10.7 min with a cited synthesis (lane clocks fired at 376 s/425 s, parent at 564 s; 4/5 lanes ok, 1 failed and said so). DONE `109f9fa1`: digest reads the `eastern` array + string years, so lane citations reach the parent.
DONE `a061c44c`: `papers_fulltext` (free, read-only full text) + READ directive in both prompts; provenance records SPOOLED to provenance.db.spool.jsonl when the store is locked and replayed on the next write (21 records of the live run were dropped before this — cause: my own sqlite watcher held the Turso lock; never hold the store open).
DONE `d462d8b2`: STORY TAB — model-written box per step (narrator on the session model, async, one at a time, `PRISM_STORY=0` off), Enter jumps to the entry; structures headline by composition when the formula is missing. `cbc18cab`: MACE cache meta records the cell's formula.
DONE (2026-09-05 late): `270821da` FOUND "20 of 2123" + DOIs open in browser + reasoning pulse; `0b6055e7` eastern empty-query; `fced61ae` Tier-3 finds provisioned QE; `120518cb` patents wall → needs_human; `cbc18cab` MACE meta formula; story tab + narrator; provenance spool; papers_fulltext + READ directive. Installed `a5a0eacb`. RUN 1 (19:42–20:37, 55 min, $0.14, 77 tools) → SX500_FFSC_preburner_materials_review.md/.docx; 38/38 DOIs verified. RUN 2 started 21:02:23 (clock 21:47:05): proposals with Tier 0/1/3 + MP/OPTIMADE → §9.
DONE (2026-09-05 night): 56ff77e4 offload note names notebook_exec; 085c0d05 qe_run keeps exit code+stderr; ac2d6a12 cutoff from PseudoDojo hints (Ni 98 Ry; manifest now carries hints_ha for 72 elements); 5f5c4956 sidecar _handle TypeError (CALPHAD answered "unavailable" — it was a crash); 20217130 reload_tools loads ~/.prism/api_keys.json; a733c366 TUI: SGR mouse-report reassembly (wheel typed "[<64;…M" into the prompt), wheel over workspace + list follows selection, needs-a-human modal (o/p/l, palette outranks it); bridge: tool call waits ≤ the tool's own promised timeout+60 s. RUN 2 (21:02–) hung from 21:28: execute_python response lost (tool server idle on stdin, TUI awaiting, no ceiling) — 66 tool calls, QE via wrapper failed 3× (54 ms death, cause still unknown; wrapper launch works from shell), model refused unphysical −337 GPa γ′ numbers (60 Ry cutoff). No §9 written.
DONE: RUN 3 (22:10–22:20 on binary 8ec4e7b625d3, 39 tool calls) appended §9 Proposals — O1–O4 (ORPB) + F1–F2 (FRPB) with at%, target phase, Tier-0 Ω/δ/VEC/Tm (all six reproduced exactly from hea_descriptors records), O1 elastic K/G via recall of run 2, one qe_run failed (structure passed as dict — tool wants a path; logged as task), §9.2 gaps, §9.3 seven experiments. .docx rebuilt (60.9 KB, §9 check in appendix) and sent.
DONE (23:00): 2d846970 qe_run takes path | cache:// | dict | JSON structure (formula refused, schema honest); b7839cd5 one OpenMP thread per MPI rank (12 ranks were 144 threads: 1 SCF iteration in 30 min). RUN 4 started 22:31 (clock 22:56): reviewer flag — Ti/Fe ignite in high-pressure O2 — verify vs WSTF/ASTM G124, MACE-MD at 772 K with O interstitials, Ti-free/Fe-lean ORPB variants → §10; 64 tool calls by 22:41 (9 mace_md_equilibrate, 4 cancelled, papers_fulltext ×3), then one LLM response crawling at 0.4 tok/s. QE rerun (fcc Ni SCF, 100/400 Ry, 286 k) relaunched 23:03 through the fixed wrapper.
DONE (23:18): RUN 5 (23:09, 18-min clock; run 4 stalled at 0.3 tok/s after an inline supercell dump) appended §10 Oxygen re-screen: reviewer flag CONFIRMED from NTRS 19740017534 + 19920013202 (passages, record ids); verdicts O1–O4/F1/F2; five Ti-free/Fe-lean variants (all 5 rows reproduce run 4 records); O2 superseded by O2-TF-Fe0 Ni60Co12Cr12Al8W4Mo4; 772 K MD jobs declared unfinished (no numbers quoted). Reading workflow w6ytk483g on the Terminal-Is-All-You-Need paper + owner TUI principles vs crates/tui running.
DONE (23:31): QE rerun through the fixed wrapper converged — fcc Ni 1-atom, scf, 100/400 Ry, 21³ k (286 irr.), 7 iterations, E = −335.5755 Ry (−4565.737 eV), 1608 s on 12 ranks × 1 thread (qe_runs/ni_fcc_rerun; scratch qe_rerun.json). 01105f14 MACE job store: owner_pid + reap on runner start → the three 772 K MD jobs orphaned by the run-4→5 respawn now read 'interrupted' (verified live). RUN 6 (23:22) honestly wrote them as running; RUN 7 (23:31, clock 23:50) resubmits them and patches §10.4.
DONE (00:00): three MD legs finished (2000/2000): O1 T≈753 K E/atom −7.544±0.006 eV stable; O1+2O (cache_ref) T≈769 K −7.403±0.009 stable; O2 T≈802 K −6.967±0.009 stable. 04da5019 serialised MACE model loading (torch.fx patcher race killed one of three simultaneous loads). Short list of 7 alloys inserted at the top of the review (owner: 'too complicated'); .docx front page = the list. Reading workflow wf_89bb2ea8 hit the session limit (12 findings kept, 31 refuted, synthesis failed) → resumed 00:17 as wypl0cuwm.
DONE (00:29): RUN 8 wrote the three MD results into §10.4 (verified vs job store: O1 752.9 K −7.544±0.006 eV; O2 801.8 K −6.967±0.009; O1+2O 769.5 K −7.403±0.009; all stable). QE rerun addendum in §9.2 (attributed). .docx (71 KB) sent. Installed into ~/.prism/venv: lammps 20250722 (python), icet 4.0. RUN 9 started 00:29 (clock 01:08:48): RHEAs with Ni + Ni–Cu, 80/300/772/1000 K MACE-MD, QHA via mace_phonon_harmonic, one QE scf anchor, icet cluster expansion, oxidation/pesting verdicts → §11.
DONE (00:36): TUI design review (workflow wf_89bb2ea8, 95 agents; report scratchpad/tui_design_review.md, 80 KB, sent). Paper = De Masi arXiv 2603.10664 (3 properties: representational compatibility, medium transparency, low barriers); the owner's arXiv link 2603.05344 is a different paper (OpenDev coding-agent report), used as secondary. 16 findings kept / 27 refuted. Top 10: (1) approval popup shows args, Enter≠approve (render.rs:5673, app.rs:3107); (2) turn cancel on Esc + honest mid-turn queue; (3) narrow layout keeps Workspace as overlay (<100 cols); (4) keyboard line cursor for refs/e/m; (5) Backspace no longer new-session (app.rs:2341); (6) hover panel never steals keys; (7) elapsed time on every wait; (8) focus tag outlives credits in the trim ladder; (9) fixed footer slots + reserved strip row; (10) state-first rows. Signature: the evidence ladder (footer evidence gauge, grounding line, one handle grammar).
IN PROGRESS (00:45): workflow wf_00c8db2b (w177ew0km) implements the TUI top-10 in order, one agent per change, test-first + mutant + gate + commit, adversarial skeptic per change, one repair pass; commits land on this branch. Paper read first-hand (De Masi 2026: P1 representational compatibility, P2 transparency of the medium, P3 low barriers; mixed-initiative = approval gate + mid-task redirect + prompt as turn boundary).
DONE (01:35): RUN 9 (00:29–01:11) hit its clock one call short of writing §11 — its final message held the section; appended verbatim by the maintainer with a note (review now 429 lines; heading fixed). Its jobs finished anyway: 8 MD legs N1/N2 × 80/300/772/1000 K, 3/4 elastic (N2 elastic failed: torch.fx 'module is not installed as a submodule' — task), N1 phonon 01M1SX7XX820HBBCA826HAKRVW still running in run 9's tool server (pid 93122; do NOT kill tmux session prism until it finishes). RUN 10 started 01:35 in tmux session prism10 (clock 02:20:15): fetch by id, poll phonon, resubmit N2 elastic, QE anchor on N1, icet CE, patch §11. Another session's terminal-browser tool splits panes into tmux window prism — always target prism:0.0.
DONE (02:05): de138389 Obscura fallback in `web action=read` (Python, no rebuild) — auto-escalates on an unrendered JS shell, result says source='obscura'/escalated + the command, render param forces/skips it, PRISM_OBSCURA_CMD or obscura/agent-browser binary drives it, honest degradation when absent. NOT yet: actual Obscura binary install (download+execute — needs explicit OK), and the same fallback in the Rust papers_fulltext path (needs a binary build; do after research runs). RUN 11 broadened: HEAs + high-entropy superalloys + Ni-HEAs (not refractory-only), Monel K-500 = oxygen baseline / SX500 = mechanical bar (R7), queued behind run 10.
DONE (02:35): §11 completed from run 10's job records (maintainer, run out of budget before writing): N1/N2 dynamically stable at ALL of 80/300/772/1000 K (energies verified vs store); N1 phonon 0 imaginary modes (F_vib one volume, no thermal expansion); QE MoNb 2-atom scf converged (NOT a MACE cross-check — diff reference, not N1); N2 elastic refilled K233 G50; icet CE still not run. .docx 75 KB sent. RUN 11 (broadened HEA: RHEA + HE-superalloy + Ni-HEA vs Monel-K500 oxygen bar + SX500 mechanical bar, R7) launched 02:35 in prism10 (clock 03:19). Approve its file tool when the watcher reports it.
DONE (03:09): disk was 100% full (2.9 GB) — /tmp/panel-target debug cache = 47 GB. Paused TUI workflow, mv debug aside + background delete, freed to 51 GB / 87%. Resumed workflow wf_00c8db2b (now wave6zjd6) from cache: changes 1–4 committed (c7374961 approval-args+Enter≠yes, 647c6de1 repair, 0c6f8e99 turn-cancel, f691bc7f narrow Workspace, 2507f096 line-cursor, 7611cc91 repair), continuing at change 5. RUN 11 (broadened HEA) unaffected, computing on Al/Cr-bearing oxidation-resistant RHEAs + γ′ HE-superalloys + Ni-HEAs; clock 03:19.
DONE (03:36): clean report SX500_FFSC_preburner_candidates.md/.docx (builder scratchpad/build_candidates_report.py; 4 parts: what to build (15 rows: O2-TF-Fe0, O2-TF, O1-Fe0, O3-Fe0, O4-Fe0, F1, F2, H1–H6 high-entropy track, N1, N2), constraints R1–R7 + Ti/Fe, calculations as tables from the stores (Tier 0, MACE relax/elastic, MD vs T, phonons, QE) + 2 charts, sources) — sent. Owner: the long review is 'too much noise'; the clean report is the deliverable, the review is archival. Owner: 'why a time budget?' — it cut runs 9/10/11 one call before writing; fix = keep file/apply_patch at the deadline.
DONE (03:40): RUN 11 (02:35–03:27, broadened HEA) hit its budget before apply_patch — same failure as runs 9/10; its final message (verdicts vs Monel/SX500 bars, patch text) appended verbatim as §12 of the archival review (502 lines) and its two verdict paragraphs quoted as §1.1 of the clean report; clean report rebuilt + sent (51.7 KB). Deadline fix in progress (agent_loop.rs: survives_deadline file/apply_patch; partition_deadline_calls; messages say the file tools remain) — test compiling behind the TUI workflow's cargo lock.
DONE (03:45): owner: DFT NOT done for any candidate (only fcc Ni, MoNb, ZrAl path checks), novelty NEVER checked, clean report 'quite shit' → workflow wvogp6x6z: novelty scout+skeptic per candidate (15), then a proper scientific report (scratchpad/preburner_report.md) with numeric/claims/structure skeptics + repair; deadline fix compiling (bhu58vyy9). DFT campaign launched detached 03:45: 16-atom random supercells (NOT SQS) of O2-TF-Fe0, H6, H4, H1, N1, scf, hint cutoffs, 6 ranks → qe_campaign_results.json (scratchpad dft_campaign.py).
DONE (04:25): PROPER REPORT SX500_FFSC_preburner_report.md/.docx (builder scratchpad/build_report_v2.py) — Summary, 1 candidates+order+standing, 2 requirements, 3 methods (reproducible params), 4 results (Tier0/elastic/MD/phonons/DFT-status), 5 NOVELTY, 6 limitations, 7 recommendations, references, Appendix A record. NOVELTY (workflow wf_014b7d27, 10/15 assessed, UNVERIFIED — skeptics died on credits): H1 (=20Nb20Mo20Cr20Ti20Al, Gorr 2016 oxidation), H2 (2026 RHEA oxidation screen), H3 (Senkov AlMo0.5NbTa0.5TiZr 2014), H4 (He 2015 precipitation-hardened HEA) ALREADY PUBLISHED; O3-Fe0/O4-Fe0 close published; O1-Fe0/O2-TF/F1/F2 not found; O2-TF-Fe0/H5/H6/N1/N2 not assessed. No patent DB queried. 228bd26a qe_status reports pinned-vs-hint cutoff (settings.toml had ecutwfc_ry=60 overriding Ni's 98 Ry hint — unpinned). DFT campaign (16-atom, 5 candidates) still running, none converged.
AUDIT (05:40, owner asked): DFT NEVER ran on any candidate. Only three 2-atom cells ever converged (Si test, NbMo, ZrAl) + the fcc-Ni maintainer rerun. My 16-atom campaign reached 21 SCF iters / 0.135 Ry accuracy on candidate 1 of 5 after ~2 h and was KILLED — not viable on 12 cores; DFT belongs on a cluster or out of the loop. MACE data IS real: 61 succeeded (30 relax, 13 elastic, 17 MD, 1 phonon), 6 failed. GFlowNet: prism-alpha has one (prism_alpha/gfn, jax+optax, optax installed by me); its own notes say it did not learn and the gate test never completed here — owner deprioritised it. OWNER'S PLAN ADOPTED: cluster expansion (icet 4.0) fitted to MACE energies + mchammer Monte Carlo for ~10 RHEA/HESA, run BY PRISM through the TUI with me as the prompting scientist. Run 12b started 05:42 in tmux prism12 (clock 07:12), writes results to section13_cluster_expansion.md (apply_patch rejects a new section — it loops on degenerate hunks; mchammer has no __version__).
DEFECT (05:55): run 12b called write_skill 65 times in a row — names placeholder-skill-1..65, bodies '# placeholder', EVERY call status ok — and burned its 90-min budget; the model diagnosed it itself. No loop guard exists: identical-tool repetition is only stopped when calls FAIL. 65 junk skills removed from ~/.prism/skills. Run 12c restarted 05:55 (clock 07:19) with binding tool rules: notebook_exec + file only, write_skill/run_skill/list_skills/find_tools/orchestrate_agents forbidden, stop a tool after two consecutive failures. Loop detector added to the pane watcher.
DONE (06:50): RUN 12c delivered the owner's plan in full — cluster expansion (icet, 76 structures each, MACE energies) + mchammer canonical MC at 773/1000/1273 K for ALL 10 alloys, every CV error passing the 15 meV/atom gate (4.3–9.5), all 10 CVs verified against the kernel's own recorded output. RESULT: 9 of 10 are NOT random solid solutions at any T; only H4 Al4Ti2Co24Cr24Fe23Ni23 keeps a disordered matrix (solute-only SRO). All refractory HEAs order strongly (H2 extreme, α=2.30). Ni-base γ/γ′: Ni–Al ordering is by design, but W/Mo/Cr clustering is a TCP precursor. Merged as §13 of the review (657 lines) and §5b of the report; .docx 180 KB sent.
NEXT: TUI changes 8/9/10 + verify:7 (resume wf_00c8db2b); deadline-fix commit (2 renamed assertions still red); agent-loop repetition guard (write_skill looped 65×); novelty for 5 unassessed + verify the 10; CALPHAD TDB; PRISM pre-flight; CONTEXT.md ≤20 lines at end.
  must move to its own strip above the prompt (test currently asserts the
  footer); whether the warm activity was visible this turn is unconfirmed.
  HARNESS: two false verdicts today — a regex test filter (substring!) and a
  `^error` grep that called red tests "did not compile"; both fixed in the
  chain and in memory `commit-gate-on-every-exit-code`.

## Next Steps
1. §13 ranked lists CLOSED (T1–T19, B1–B15) except BOOKED B11 attempt-evidence + audit hash chain. EMMO domain/range DONE from upstream source (`21d84b31`); isPartOf untyped upstream.
   Tailcat (`tailscale/tailcat`) = mesh DATA PLANE only, never a PRISM tool; NOT installed; needs Mirdyne control plane first.
2. Then the LitXBench eval (`~/Downloads/prism-gold-eval/harness/`), scratch DB.
3. Merge to main; tag only after.
