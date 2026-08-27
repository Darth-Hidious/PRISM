# FFSC Methalox Thrust-Chamber / Nozzle Materials — Requirements, Ontology, Screening
**Deliverable of record · 2026-07-03 (platform clock 2026-08-26) · PRISM session**
Every number below is quoted from a source fetched during this run (URL given). Unverified/unquoted items are explicitly flagged as gaps.

## 1. Hot-gas-side service requirements

| # | Requirement | Value | Source (fetched this run) |
|---|---|---|---|
| R1 | Cycle & gas environment | Full-flow staged combustion; **both oxidizer-rich and fuel-rich preburners**; both propellants fully gaseous entering chamber ⇒ main-chamber bulk is O₂-rich at MR 3.6–3.8 | Wikipedia "SpaceX Raptor" (en.wikipedia.org/wiki/SpaceX_Raptor), design section |
| R2 | Chamber pressure | First-version design point **250 bar (25 MPa)**; **300 bar reached in test July 2022**; infobox current **330 bar**. Research chambers run 200 bar (methane/O₂ flamelet study) | Wikipedia Raptor page + arXiv:2108.12046 abstract |
| R3 | Bulk combustion temperature scale | **3953 K** for CH₄ + O₂, stoichiometric, adiabatic constant-pressure, reactants at 25 °C/1 atm | Engineering ToolBox table d_996 (adiabatic flame temperatures) |
| R4 | Wall heat flux at throat | **18 MW/m² max at throat** ("aligns well with values reported in the literature"); **9.3 MW/m² max** in small H₂O₂-cooled chamber. FFSC-at-330-bar flux values NOT publicly quoted — flagged gap | ScienceDirect S2214157X25008834 (snippet); MDPI Aerospace 10(1):65 |
| R5 | Liner hot-gas-wall service cap | GRCop-class liner service strength/creep/LCF quoted **up to 700 °C**; hot-side max wall T "determines the fatigue life of the chamber" | NASA/TM-2005-213566 (NTRS 20050123582); arXiv:1907.11281 §1,§4 |
| R6 | Radiative component cap | C-103 material system **maximum operating temperature 1370 °C**; R-512 coating subject to cracking/spalling under repeated thermal cycling → film cooling needed to hold margin | NTRS 19940018579 (Reed/Biaglow/Schneider, Nov 1993) |
| R7 | Cooling lever | Transpiration/film cooling: wall T = 500–700 K achievable at ~5 % methane transpiration flow fraction | academia.edu record 144262937 (film-cooling analysis) |

## 2. Candidate material classes (ontology persisted to platform KG)
Server confirmed creation: **18 entities, 22 relationships total** (`entities_created` counts from two graph_ingest calls). Read-back verification unavailable this run (see §5).

| Class node | Members / anchors | Quoted basis |
|---|---|---|
| `ffsc_cls_ds_copper_liner` — dispersion-strengthened Cu liners | GRCop-84 (`ffsc_grcop84`, Cu‑8Cr‑4Nb at%), GRCop-42 (`ffsc_grcop42`, Cu‑4Cr‑2Nb) | TM‑2005‑213566: strength/creep/LCF to 700 °C; AIAA‑2019‑4228: PBF builds with integral channels + closeouts, hot-fire demonstrated; oxidation resistance "developed for harsh environments…regeneratively-cooled combustion chambers" |
| `ffsc_cls_ni_superalloy_jacket` — Ni-base superalloy jackets/manifolds | SpaceX SX300/SX500 Inconel-family cast superalloys (Wikipedia quote) | Structural duty only; standalone T-allowable NOT sourced this run → gap flag |
| `ffsc_cls_refractory_radiative` — refractory radiative extensions | C‑103 (Nb‑10Hf‑1Ti) + R‑512A/E fused-silicide coating | NTRS 19940018579 quotes (1370 °C cap; spalling caveat) |
| `ffsc_cls_ir_re_chamber` — Ir-coated Re chambers | "most developed of these high-temperature materials"; engines targeted 22 N / 62 N / 440 N | NTRS 19940018579 |
| `ffsc_cls_uhtc_cmc` — UHTC/CMC hot structures | HfC/TaC matrix composites; oxide-coated Ir/Re chambers | NTRS 19940018579 (research-stage). DB anchors fetched: HfC = CHf, matcloud.mc3d-pbe-v1 mc3d-46672 (MPDS S531899), mc3d-47783/mc3d-38229/mc3d-56647 (ICSD 185985/185992/169399), one cell a≈2.899 Å consistent with NaCl-type; TaC = CTa, matcloud.mc3d-pbesol-v1 mc3d-65933 (MPDS S534147), mc3d-28326/mc3d-49776/mc3d-66082 (ICSD 185986/185993/169400) |
| `ffsc_r512_coating` — silicide/fused-silica coating class | R‑512A/E | spalling limitation quote |
| Phase record: `ffsc_cr2nb_phase` | Cr₂Nb Laves dispersoid | Federation DB records matcloud.mc3d-pbe-v1 mc3d-67604 (MPDS S260524), mc3d-74169 (ICSD 188264) |

Requirement nodes persisted: `ffsc_req_cycle_env`, `ffsc_req_pc`, `ffsc_req_Tgas`, `ffsc_req_q_throat`, `ffsc_req_liner_wall_cap`, `ffsc_req_radiative_cap`, `ffsc_req_film_cooling` — edges encode applies_to / drives_thermal_design_of / governs_duty_of / caps_service_of / mitigates_for.

## 3. Screening verdicts (evidence-cited only)

| Class vs primary FFSC duties | Verdict | Basis |
|---|---|---|
| DS-Cu liners for actively cooled chamber+throat | **PRIMARY — PASS** | Cap 700 °C ≥ cooled-wall design band; conductivity/strength claims; AM route with integral channels proven by hot-fire test (AIAA 2019‑4228); strengthening phase DB-traceable |
| Ni-superalloy jackets/manifolds | **RETAIN — structural class** | Manifold use quoted (SX300/SX500); independent temperature allowable missing → keep out of hot-wall claims until sourced |
| C‑103 + silicide for radiative nozzle extension | **PASS WITH CAVEAT** | 1370 °C ceiling quoted; coating spalling under cycling quoted → lifecycle-limited; film-cooling penalty trade |
| Ir/Re chambers | **OUT OF CLASS for main-chamber scale** | Literature targets are 22–440 N thrusters, not MN-class FFSC |
| UHTC/CMC composites | **WATCHLIST — not qualified** | Named as research systems in 1993-era NASA doc; no flight qualification evidence retrieved |

Duty-to-class mapping conclusion: FFSC methalox throat/chamber hardware resolves to **GRCop-class Cu-Cr-Nb liners + Ni-superalloy structural sets**, with **C‑103/silicide extensions** where radiative duty applies and film-cooling trades documented.

## 4. Assumptions replaced by quotations
Prior assumed values eliminated this run: chamber pressure guesswork → 250/300/330 bar (Wikipedia + arXiv corroboration); "hot enough to melt most alloys" vagueness → 3953 K CH₄/O₂ quoted; heat-flux folklore → 18 MW/m² & 9.3 MW/m² quoted with IDs; liner temp hand-waving → 700 °C NASA/TM quote; radiative cap lore → 1370 °C NASA/NTRS quote; coating worry → spalling sentence quoted; transpiration leverage → 500–700 K @ ~5 % flow quoted.

## 5. Tooling failures encountered (all reproduced ≥1× this run)
1. `prior_art_search` papers engine exited -9 on all 3 queries.
2. Patent backend unconfigured (informational).
3. CyberLeninka OAI HTTP 503 mid-harvest; J-STAGE ERR_001.
4. Silent-failure family in prism CLI wrappers: `papers search`, `ingest_and_wait`, `query --platform`, `knowledge_entity`, `report_bug` — success=false, empty stdout/stderr, exit_code null. Ontology writes succeeded via direct platform service (knowledge_write) but independent read-back was impossible; bug report attempt itself failed through same wrapper (full text preserved here §5).
5. `web read` cannot extract PDF text (raw bytes returned).
6. Semantic Scholar 429 twice.
7. materials_search could not dedup Cr₂Nb polymorphs (missing space group metadata).
8. knowledge_write.embed of this evidence pack FAILED HTTP 402 insufficient_credits ("needs ~13 millicredits") AFTER both graph_ingest writes had succeeded — platform wallet empty. Not retried; human must run `prism billing topup` if the semantic-retrieval copy is wanted.
9. KG read-back exhaustion proof completed: third distinct platform-read tool (`knowledge_paths`, invocation `prism knowledge paths ffsc_grcop84 ffsc_cls_ds_copper_liner --max-hops 2`) failed identically (success=false, empty stdout/stderr, exit_code null). Read-back is therefore impossible across query--platform / knowledge_entity / knowledge_paths; persistence rests solely on the server-side counts returned by knowledge_write.graph_ingest.
10. Federation health note: alexandria.alexandria-pbe OPTIMADE provider returned HTTP 400 and was circuit-broken on the next query; result merging stopped early ("enough results from fast providers"), so provider coverage in results was partial by design.

Consequence: treat KG counts as server-confirmed-but-not-read-back-verified; all chat-fetched quotes remain the authoritative evidence set of record.
