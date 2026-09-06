# CONTEXT — resume here

**Current task**: PRISM pre-flight done; supervised beta ready. Research thread (SX500/FFSC preburner) delivered.

## State
- Installed binary `62482aa94fb9` (prism 1.1.0) = all ten TUI design-review changes + deadline fix + repetition guard. Gates green: tui 712, agent 865, bridge 33, ipc 20; fmt + clippy clean.
- Deliverables sent: `SX500_FFSC_preburner_report.docx` (candidates, novelty, methods, results), `SX500_FFSC_preburner_materials_review.md` (657 lines, archival), `PRISM_PREFLIGHT.docx`, `section13_cluster_expansion.md`.
- Science: 10 alloys through cluster expansion (icet, MACE energies, all CV < 15 meV/atom) + Monte Carlo at 773/1000/1273 K. **9 of 10 are not random solid solutions; only H4 Al4Ti2Co24Cr24Fe23Ni23 keeps a disordered matrix.** Prior art: H1–H4 already published (two with oxidation studies); 5 compositions never assessed.
- No DFT on any candidate — only 1–2 atom path checks. Not viable on 12 cores.

## Blocking before unattended running
1. 54 ms mpirun death in the tool server; torch model-load race killed 6 MACE jobs (lock in make_calc misses the elastic path).
2. No CALPHAD database.
3. No tested unattended approval policy.

## Next
- Verify the 10 novelty verdicts, assess the 5 remaining; query a real patent database.
- Install the Obscura binary (needs explicit OK) and wire papers_fulltext.
- Protected, never commit: holmquist2019_whiterose.txt, schellenberger2018_diva.txt, ontology-pfas-alternatives-candidate.ttl, pfas_alternatives_evidence_log*.md.

## Boundary conditions (owner, 2026-09-06)
- 90 K -> 1000 K in milliseconds, no quantifiable phase change => FCC matrix only; BCC refractory HEAs are OUT (ductile-brittle transition above 90 K).
- Built by additive manufacturing => Al+Ti drives strain-age cracking: <=6 at% prints, 8 borderline, >=12 is a process programme.
- ~700 bar oxygen. Beat Monel K-500 (oxygen) and SX500 (strength/temperature).
- Under these: H4 Al4Ti2Co24Cr24Fe23Ni23 is the lead (only alloy that stays mixed AND prints); O1-Fe0 second; the 15 at% Al alloys and all refractory HEAs are out.
