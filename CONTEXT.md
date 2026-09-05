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
NEXT: model window lists fallbacks; SSO scope (Mirdyne provider exists in `prism login`; SAML/IdP is platform-side); bash.py `.git`/`.prism` deletion refusal — owner's call; CONTEXT.md ≤20 lines at session end.
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
