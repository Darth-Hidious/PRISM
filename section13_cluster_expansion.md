## 13. Cluster expansion and Monte Carlo — does the solid solution survive?

**Question.** Tier-0 descriptors (Ω, δ, VEC) only guess whether these candidate alloys stay random solid solutions. Here each alloy gets an actual configurational-thermodynamics treatment: a cluster expansion (CE) fitted to MACE energies of 76 random supercells spanning the alloy's composition neighbourhood, then canonical Monte Carlo (mchammer) on a 108-atom (FCC) / 128-atom (BCC) cell at the alloy composition at 773, 1000 and 1273 K. First-shell Warren–Cowley parameters α_ij are frame-averaged over the equilibrated second half of each run: α_ij < 0 = unlike pairs attract (ordering tendency, e.g. Ni–Al/γ′, B2 Al–Ti motifs); α_ij > 0 = like pairs cluster / unlike pairs avoid (segregation tendency). Mixing energies are CE rigid-lattice configurational energies in meV/atom (no entropy term in the number itself — entropy acts through the MC sampling).

**Bottom line: none of the ten alloys is a random solid solution in equilibrium at preburner-relevant temperatures.** Nine show strong, cooling-strengthened short-range order at all three temperatures (already strong at 1273 K, no order–disorder transition in-window). The single partial exception is H4 (Cantor-family): its Co–Cr–Fe–Ni *matrix* stays essentially random (|α| ≤ 0.2) at all temperatures; only its dilute Al/Ti solutes order with Ni. Ordering motifs are chemically specific and follow known phase chemistry: γ′ (Ni–Al) in the Ni alloys, B2 (Al–Ti, and Ni–Ti/Ni–Ta in N1/N2) in the refractory alloys, and strong refractory/refractory sorting (Cr–Mo, Mo–Ta, Nb–Ta, W–Al avoidances) — the σ/μ precursors the review worries about, visible directly in the SRO.

**Method as actually run (final protocol, applied identically to every alloy).**
- Libraries confirmed in this environment: icet 4.0 (ClusterSpace / StructureContainer / ClusterExpansion), mchammer (imports cleanly; the project has no `__version__` attribute — not queried, per task), trainstation 1.2, ase 3.29. Energies: MACE-MP (Materials Project 2023-12-10 small 128L0 checkpoint, the `mace_mp` model family used elsewhere in this review), float32, single-point, CPU.
- Parent lattice: FCC for the Ni-base alloys and H4 (O2-TF-Fe0, H6, H4, O1-Fe0, O3-Fe0); BCC for the refractory alloys (H1, N1, N2, H3, H2). Fixed lattice constant a0 = composition-weighted average of standard elemental lattice constants (metastable reference phases included); single-lattice CE — MC runs on a rigid cell.
- **Self-interaction control (this cost the FCC alloys their first two fits):** icet warned that the first Monte Carlo cell self-interacted. The cause was geometric: rhombohedral primitive-repeat supercells ((2,4,4) 32-atom fitting cells, (5,5,4) 100-atom MC cells) have minimum periodic image widths of only 4.2–8.4 Å (FCC) / 4.4 Å (BCC) — smaller than 2× the pair cutoffs, so clusters interacted with their own images in the *fitting data* and, for one alloy's chosen cutoffs, in the MC cell. Final protocol uses **cubic-conventional supercells with the width verified by assertion**: fitting cells 3×3×3 FCC (108 atoms, 10.9 Å) / 4×4×4 BCC (128 atoms, 12.8–13.2 Å), MC cells 108/128 atoms likewise; every fit and MC run asserts min-width ≥ 2×pair-cutoff. An intermediate energy-cache poisoning (energies computed on mis-sized cells) was also found and purged; all reported numbers come from the verified-geometry cache.
- Cluster space: the suggested pair ~7 Å / triplet ~5 Å was built first and **rejected on evidence**: for a 6-component alloy that space defines 606 parameters, and with the mandated 40–80-structure budget the 5-fold CV error was 31–130 meV/atom (least-squares divergent; ridge 130; ARDR 32; lasso 31). Three protocol axes were then tested (sampling domain: whole simplex vs local composition neighbourhood; cell size; cutoff/method scan). CV-optimal setting kept for all alloys: cutoffs **[4.5 Å pair, 3.5 Å triplet]** (FCC alloys) or **[5.0, 4.0]** where it tied/won (H1, N1, N2) — pairs through the 2nd neighbour shell, first-shell triplets — and **76 structures**: 70 cells with per-element fractions jittered ×exp(U(−0.55, +0.55)) around the target composition + 6 cells at the rounded target composition (largest-remainder integer counts; MC composition deviates ≤0.6 at% from nominal). Fit target: mixing energy per atom vs pure-element MACE references at the same a0. Regressor per alloy by 5-fold CV over {lasso α=0.001/0.003, ARDR, ridge}. CV gate: worse than ~15 meV/atom → flag unreliable, do not report MC numbers as findings. **All ten alloys passed the gate** (CV 4.3–14.2 meV/atom), and each fit was additionally verified against a held-out MACE single-point at the exact target composition (agreement 0–8 meV/atom, column in §13.11).
- MC: mchammer `CanonicalEnsemble`, 400,000 trial steps per temperature (≈3,100–4,600 sweeps/site; 37–71k steps/s), first 25 % discarded as equilibration, first-shell Warren–Cowley averaged over ~160 equilibrated snapshots via a custom snapshot observer (this mchammer has no `trajectory` argument and no `ensemble.atoms`). Equilibrium configurational thermodynamics only.

Status: **all 10 alloys completed — O2-TF-Fe0, H6, H4, H1, N1, N2, O1-Fe0, H3, O3-Fe0, H2. None skipped.**

### Results table

| alloy | lattice | CE cutoffs Å | structures fitted | CV error meV/atom | SRO at 773 K | SRO at 1000 K | SRO at 1273 K | verdict |
|---|---|---|---|---|---|---|---|---|
| O2-TF-Fe0 (Ni60Co12Cr12Al8W4Mo4) | FCC | 4.5 / 3.5 | 76 | 8.0 | very strong (max α=1.00) | very strong (0.99) | strong (0.92) | not a random SS at any T; Ni–Al order + W/Mo–Al/W–Mo avoidance; strengthens on cooling, no transition in-window |
| H6 (Ni55Al15Co10Cr10W5Mo5) | FCC | 4.5 / 3.5 | 76 | 8.8 | very strong (max α=1.88) | very strong (1.65) | very strong (1.35) | not a random SS at any T; strongest FCC SRO; Cr–Mo pairing = σ precursor |
| H4 (Al4Ti2Co24Cr24Fe23Ni23) | FCC | 4.5 / 3.5 | 76 | 6.3 | matrix ~random; Al–Ti +0.96, Ti–Ni −0.70, Ni–Al −0.56 | matrix ~random; Ti–Ni −0.45 | solute SRO only (Al–Ti +0.87) | matrix stays disordered at all 3 T; only dilute Al/Ti order with Ni (γ′/η precursors), growing below ~1000 K |
| H1 (Al20Cr20Mo20Nb20Ti20) | BCC | 5.0 / 4.0 | 76 | 6.4 | very strong (max α=1.20) | very strong (1.15) | very strong (1.07) | not a random SS at any T; Ti–Cr/Cr–Mo attraction, Al–Cr avoidance, B2-like Al–Ti order |
| N1 (Nb20Mo20Ta20Ti20Ni20) | BCC | 5.0 / 4.0 | 76 | 7.0 | very strong (max α=1.53) | very strong (1.33) | strong (1.19) | not a random SS at any T; Ni–Ti ordering, Ni–Mo avoidance |
| N2 (Nb20Mo20Ta20W20Ni20) | BCC | 5.0 / 4.0 | 76 | 7.1 | very strong (max α=1.86) | very strong (1.82) | very strong (1.61) | not a random SS at any T; Ni–Ta ordering, Nb–Ta / Ni–Mo near-total avoidance |
| O1-Fe0 (Ni45Co20Cr15W10Mo5Al5) | FCC | 4.5 / 3.5 | 76 | 9.5 | very strong (max α=1.04) | very strong (0.94) | strong (0.77) | not a random SS at any T; Ni–Al order + Al–Cr/Mo–Al/W–Mo avoidance |
| H3 (Al20Mo10Nb20Ta10Ti20Zr20) | BCC | 4.5 / 3.5 | 76 | 7.7 | very strong (max α=1.27) | very strong (1.18) | strong (0.89) | not a random SS at any T; Ti–Mo and Al–Zr ordering, Al–Mo avoidance |
| O3-Fe0 (Ni60Al15Co10Cr10W5) | FCC | 4.5 / 3.5 | 76 | 4.3 | very strong (max α=1.69) | very strong (1.24) | strong (0.99) | not a random SS at any T; W–Al near-total avoidance, Co–W ordering, Ni–Al order |
| H2 (Al13Cr7Mo19Nb18Ta26Ti17) | BCC | 4.5 / 3.5 | 76 | 8.5 | extreme (max α=2.30) | extreme (2.02) | extreme (1.64) | not a random SS at any T; strongest SRO in the set: B2 Al–Ti order + Mo–Ta/Ta–Cr sorting |

### 13.1 O2-TF-Fe0 — Ni60Co12Cr12Al8W4Mo4 (FCC, primary blade candidate)

CE: a0 = 3.629 Å, 76 structures (108-atom cells), 86 parameters, lasso (α=0.001), CV = **8.0 meV/atom** (train 2.1). Held-out target-composition cell: MACE −290 vs CE −295 meV/atom.

MC (108 atoms, 400k steps/T): ⟨E_mix⟩ = −356 / −379 / −388 meV/atom at 1273/1000/773 K; max |α| = 0.92 / 0.99 / 1.00.

Significant α (1273/1000/773 K): W–Mo +0.92 / +0.99 / +1.00; Al–W +0.84 / +0.97 / +1.00; Al–Mo +0.69 / +0.95 / +1.00; Al–Cr +0.49 / +0.84 / +0.95; Ni–Al −0.26 / −0.37 / −0.40; Cr–Mo −0.32 / −0.75 / −0.69; Ni–W −0.19 / −0.32 / −0.39; Co–Al −0.20 / −0.26 / −0.23.

**Verdict.** Not a random solid solution anywhere in 773–1273 K. Two SRO motifs strengthen monotonically on cooling with no regime change in the window: (1) **Ni–Al ordering** (α −0.26 → −0.40) — the γ′/L1₂ building block present as short-range order, consistent with the Ni₃Al hull anchor in §9.1; (2) **near-complete first-shell avoidance among the refractory/Al species** (W–Mo, W–Al, Mo–Al → +1.00) with W/Mo bonding instead to Cr/Ni/Co (Cr–Mo −0.69, Ni–W −0.39 at 773 K). Rare-species caveat: W and Mo are 4 at% (≈4–5 atoms per MC cell), so their individual α carry large scatter (frame σ ≈ 0.2–0.4); signs and trends are robust. If a random γ is the design target this fails; if γ′-type order is intended (as §9.1 says), the SRO is supportive and the message is that W/Mo do not randomise into the matrix — σ/μ tendency is already visible at the SRO level.

### 13.2 H6 — Ni55Al15Co10Cr10W5Mo5 (FCC)

CE: a0 = 3.634 Å, 76 structures, 86 parameters, lasso (α=0.001), CV = **8.8 meV/atom** (train 2.0). Held-out target cell: MACE −392 vs CE −400 meV/atom.

MC: ⟨E_mix⟩ = −491 / −506 / −516 meV/atom (1273/1000/773 K); max |α| = 1.35 / 1.65 / 1.88.

Significant α (1273/1000/773 K): Cr–Mo −1.35 / −1.65 / −1.88; Co–W −1.05 / −1.29 / −1.46; Cr–W −0.74 / −0.99 / −1.18; Al–W +0.89 / +0.99 / +1.00; Al–Mo +0.87 / +0.98 / +1.00; Mo–W +0.84 / +0.98 / +0.99; Co–Mo −0.42 / −0.64 / −0.90; Ni–Al −0.46 / −0.54 / −0.60; Al–Cr +0.33 / +0.46 / +0.62.

**Verdict.** The most strongly ordered FCC alloy of the Ni-base group: not a random solid solution at any temperature, already far from random at 1273 K (max |α| = 1.35). Motifs: (1) Cr–Mo attraction (α → −1.9) — the classic σ-phase building block, a clear short-range precursor to σ/μ formation; (2) Co–W / Cr–W attraction (α ≤ −1) — W in a Co/Cr-rich environment; (3) near-total W/Mo–Al avoidance (→ +1.0); (4) Ni–Al ordering (−0.60 at 773 K). Behaviour strengthens monotonically on cooling; no transition within 773–1273 K. The W/Mo σ-risk flagged in §9 is visible at the SRO level for H6 more strongly than for O2-TF-Fe0 (α_CrMo −1.9 vs −0.7).

### 13.3 H4 — Al4Ti2Co24Cr24Fe23Ni23 (FCC, Cantor-family control)

CE: a0 = 3.614 Å, 76 structures, 86 parameters, ARDR, CV = **6.3 meV/atom** (train 1.9) — the best FCC fit in the set. Held-out target cell: MACE −137 vs CE −137 meV/atom.

MC: ⟨E_mix⟩ = −151 / −155 / −160 meV/atom (1273/1000/773 K).

Significant α (1273/1000/773 K): Al–Ti +0.87 / +0.96 / +0.96; Ti–Ni −0.38 / −0.45 / −0.70; Ni–Al −0.33 / −0.40 / −0.56; Ti–Cr +0.27 / +0.37 / +0.48; Al–Cr +0.20 / +0.30 / +0.42; Fe–Co −0.13 / −0.16 / −0.20; all Co–Cr–Fe–Ni matrix pairs |α| ≤ 0.2 at every T.

**Verdict.** The equiatomic Cantor matrix **stays disordered** at all three temperatures — the closest to a genuine random solid solution in this set (max matrix α ≈ 0.2, weak Fe–Co only). SRO grows only around the dilute solutes: Ti and Al both order with Ni (Ti–Ni −0.70, Ni–Al −0.56 at 773 K — Ni₃Ti/η and Ni₃Al/γ′ motifs) while avoiding each other (Al–Ti ≈ +0.96) and Cr. **Behaviour changes below ≈1000 K** (solute α roughly doubles from 1273 to 773 K; matrix α never exceeds ~0.2). Interpretation: the matrix is random-equilibrium at preburner temperatures; the solute SRO is the precursor state of γ′/η precipitation on aging, not matrix decomposition. Best solid-solution former of the FCC group.

### 13.4 H1 — Al20Cr20Mo20Nb20Ti20 (BCC refractory)

CE: a0 = 3.168 Å, 76 structures (128-atom cells), 75 parameters, lasso (α=0.003), CV = **6.4 meV/atom** (train 1.5). Held-out target cell: MACE −341 vs CE −344 meV/atom.

MC (128 atoms, 400k steps/T): ⟨E_mix⟩ = −473 / −500 / −514 meV/atom (1273/1000/773 K); max |α| = 1.07 / 1.15 / 1.20.

Significant α (1273/1000/773 K): Ti–Cr −1.07 / −1.15 / −1.20; Al–Cr +0.97 / +1.00 / +1.00; Ti–Al −0.82 / −0.84 / −0.82; Cr–Mo −0.73 / −0.93 / −1.06; Nb–Al −0.77 / −0.60 / −0.56; Ti–Mo +0.51 / +0.73 / +0.86; Nb–Ti +0.48 / +0.32 / +0.21; Al–Mo −0.42 / −0.61 / −0.67.

**Verdict.** Not a random solid solution at any temperature: strong, cooling-enhanced SRO. Motifs: (1) Ti–Cr and Cr–Mo attraction (α ≈ −1.1 to −1.2 at 773 K) with Al–Cr near-total avoidance (+1.00); (2) a clear **Al–Ti ordering tendency** (α_TiAl ≈ −0.82 at all T) — the B2 AlTi motif this alloy family is known to form, visible directly in the SRO; (3) Nb roughly indifferent (small α), Mo siding with Ti/Cr. Ordering is significant already at 1273 K (max |α| ≈ 1.1). On the rigid BCC lattice this alloy is a strongly ordered (B2-precursing) solid solution, not a random one, at every preburner-relevant temperature.

### 13.5 N1 — Nb20Mo20Ta20Ti20Ni20 (BCC refractory + Ni)

CE: a0 = 3.190 Å, 76 structures, 75 parameters, lasso (α=0.003), CV = **7.0 meV/atom** (train 1.5). Held-out target cell: MACE −267 vs CE −271 meV/atom.

MC: ⟨E_mix⟩ = −369 / −379 / −392 meV/atom (1273/1000/773 K); max |α| = 1.19 / 1.33 / 1.53.

Significant α (1273/1000/773 K): Mo–Ta −1.19 / −1.33 / −1.53; Ti–Ni −0.84 / −1.00 / −1.36; Ni–Mo +0.96 / +0.98 / +1.00; Ni–Ta −0.81 / −0.81 / −0.74; Mo–Nb −0.72 / −0.76 / −0.77; Nb–Ta +0.58 / +0.62 / +0.60; Ti–Ta +0.49 / +0.55 / +0.68.

**Verdict.** Not a random solid solution at any temperature. Motifs: (1) **Ni–Ti ordering** (α → −1.36 at 773 K) with simultaneous near-total **Ni–Mo avoidance** (+1.00) — Ni partitions away from the Mo/Nb sub-lattice into a Ti/Ta-rich environment (Ni–Ta −0.74); (2) Mo–Ta attraction (−1.53) with Nb–Ta avoidance (+0.60). Behaviour strengthens on cooling; ordering already strong at 1273 K. The 20 at% Ni does not randomise into the BCC refractory lattice — it develops its own local chemistry (Ti/Ta-coordinated), a B2-like ordering precursor.

### 13.6 N2 — Nb20Mo20Ta20W20Ni20 (BCC refractory + Ni)

CE: a0 = 3.189 Å, 76 structures, 75 parameters, lasso (α=0.001), CV = **7.1 meV/atom** (train 0.6). Held-out target cell: MACE −235 vs CE −236 meV/atom.

MC: ⟨E_mix⟩ = −401 / −423 / −432 meV/atom (1273/1000/773 K); max |α| = 1.61 / 1.82 / 1.86.

Significant α (1273/1000/773 K): Ni–Ta −1.61 / −1.82 / −1.86; Nb–Ta +0.95 / +0.99 / +1.00; Ni–Mo +0.97 / +0.99 / +0.99; Ni–Nb −0.95 / −0.94 / −0.98; W–Ni +0.65 / +0.84 / +0.92; Mo–Ta −0.81 / −0.79 / −0.85; W–Mo −0.51 / −0.68 / −0.75; W–Ta +0.46 / +0.59 / +0.67; W–Nb −0.39 / −0.58 / −0.70.

**Verdict.** One of the two most strongly ordered alloys of the entire set. Not a random solid solution at any temperature, with SRO already extreme at 1273 K (max |α| = 1.6). Motifs: (1) **Ni–Ta ordering** (α → −1.9) with Ni avoiding Mo/W (α ≥ +0.9) — Ni sits in an Nb/Ta-rich coordination; (2) **Nb–Ta near-total first-shell avoidance** (+1.00 at 773 K) and W–Ta avoidance (+0.67) vs W–Mo attraction (−0.75): the refractory metals themselves are chemically sorted (W with Mo; Nb with Ni; Ta with Ni). Behaviour strengthens on cooling, monotonically, no transition in-window.

### 13.7 O1-Fe0 — Ni45Co20Cr15W10Mo5Al5 (FCC, conservative control)

CE: a0 = 3.637 Å, 76 structures, 86 parameters, lasso (α=0.001), CV = **9.5 meV/atom** (train 2.2). Held-out target cell: MACE −329 vs CE −330 meV/atom.

MC: ⟨E_mix⟩ = −430 / −458 / −476 meV/atom (1273/1000/773 K); max |α| = 0.77 / 0.94 / 1.04.

Significant α (1273/1000/773 K): Ni–Al −0.57 / −0.83 / −1.04; Al–Mo +0.77 / +0.89 / +0.97; W–Mo +0.71 / +0.83 / +0.97; Al–Cr +0.64 / +0.94 / +1.00; Al–W +0.51 / +0.64 / +0.90; Co–Cr −0.29 / −0.47 / −0.64; Ni–Mo −0.28 / −0.35 / −0.42; Co–Ni +0.22 / +0.34 / +0.46; Al–Co +0.19 / +0.42 / +0.69.

**Verdict.** Not a random solid solution at any temperature. Motifs: (1) strong **Ni–Al ordering** (−1.04 at 773 K — γ′ order, as intended by design) with Al avoiding Cr/Mo/W/Co (all → +0.7…+1.0), i.e. Al lives in a Ni-rich coordination; (2) W–Mo +0.97 (refractory mutual avoidance) with W and Mo individually preferring Ni/Co (Ni–Mo −0.42, Co–Cr −0.64); (3) weak Co–Ni like-pair tendency (+0.46 at 773 K). Strengthens monotonically on cooling, no transition in-window. Same qualitative picture as O2-TF-Fe0 with a somewhat stronger Ni–Al SRO, consistent with its role as the γ+γ′ control alloy.

### 13.8 H3 — Al20Mo10Nb20Ta10Ti20Zr20 (BCC refractory, Zr-bearing)

CE: a0 = 3.251 Å, 76 structures, 111 parameters, lasso (α=0.001), CV = **7.7 meV/atom** (train 0.5). Held-out target cell: MACE −246 vs CE −245 meV/atom.

MC: ⟨E_mix⟩ = −312 / −346 / −368 meV/atom (1273/1000/773 K); max |α| = 0.89 / 1.18 / 1.27.

Significant α (1273/1000/773 K): Ti–Mo −0.89 / −1.18 / −1.27; Al–Zr −0.85 / −1.13 / −1.19; Al–Mo +0.84 / +0.96 / +0.98; Zr–Ti +0.36 / +0.68 / +0.88; Zr–Ta +0.37 / +0.59 / +0.76; Ta–Mo −0.39 / −0.55 / −0.71; Ti–Nb −0.10 / −0.44 / −0.72; Mo–Nb +0.01 / +0.40 / +0.70; Zr–Mo −0.24 / −0.46 / −0.64; Al–Ti −0.28 / −0.45 / −0.61; Nb–Zr −0.30 (1000 K) / −0.60 (773 K).

**Verdict.** Not a random solid solution at any temperature. Motifs: (1) **Ti–Mo ordering** (−1.27 at 773 K) and **Al–Zr ordering** (−1.19) — two independent ordering channels — while Al strongly avoids Mo (+0.98); (2) like-species avoidance among the big refractories: Zr–Ti +0.88, Zr–Ta +0.76, Mo–Nb +0.70 at 773 K; (3) secondary Ti–Al (−0.61) and Ti–Nb (−0.72) ordering at low T. Strengthens on cooling, no transition in-window. The Zr chemistry is distinct: Zr orders with Al and avoids the Ti/Ta/Nb/Mo sub-lattice — consistent with the strong AlZr/TiZr intermetallic chemistry of that pair.

### 13.9 O3-Fe0 — Ni60Al15Co10Cr10W5 (FCC, alumina-forming duct/liner alloy)

CE: a0 = 3.629 Å, 76 structures, 55 parameters, ARDR, CV = **4.3 meV/atom** (train 2.1) — the best fit of the whole campaign. Held-out target cell: MACE −369 vs CE −369 meV/atom.

MC: ⟨E_mix⟩ = −458 / −467 / −477 meV/atom (1273/1000/773 K); max |α| = 0.99 / 1.24 / 1.69.

Significant α (1273/1000/773 K): Co–W −0.91 / −1.24 / −1.69; Al–W +0.99 / +1.00 / +1.00; Cr–Al +0.45 / +0.60 / +0.78; Ni–Al −0.39 / −0.44 / −0.50; W–Cr +0.26 / +0.37 / +0.35; Co–Cr −0.31 / −0.35 / −0.31; Ni–Cr +0.24 (773 K).

**Verdict.** Not a random solid solution at any temperature, despite the cleanest fit. Motifs: (1) **near-total W–Al first-shell avoidance** (+1.00 at all T) with W strongly bonding Co (−1.69 at 773 K); (2) **Ni–Al ordering** (−0.50 at 773 K — the B2-NiAl/γ′ motif; §9.1 already flags B2-NiAl as likely for this alloy and that is exactly what the SRO shows, tempered by the small Al coordination set); (3) Al–Cr avoidance (+0.78). §9.1's concern that "ordered B2 at grain boundaries can embrittle" is here at the SRO level: the Ni–Al order parameter grows on cooling and is already −0.4 at 1000 K. Strengthens monotonically on cooling, no transition in-window.

### 13.10 H2 — Al13Cr7Mo19Nb18Ta26Ti17 (BCC refractory)

CE: a0 = 3.272 Å, 76 structures, 111 parameters, ARDR, CV = **8.5 meV/atom** (train 1.4). Held-out target cell: MACE −201 vs CE −200 meV/atom.

MC: ⟨E_mix⟩ = −325 / −344 / −361 meV/atom (1273/1000/773 K); max |α| = 1.64 / 2.02 / 2.30.

Significant α (1273/1000/773 K): Al–Ti −1.64 / −2.02 / −2.30; Mo–Ta −1.34 / −1.60 / −1.88; Ta–Cr −1.03 / −1.23 / −1.60; Cr–Al +0.96 / +0.98 / +0.99; Cr–Mo +0.95 / +0.96 / +0.99; Al–Mo +0.96 / +0.97 / +0.98; Ti–Ta +0.67 / +0.83 / +0.97; Cr–Nb −0.48 / −0.39 / −0.16; Nb–Ta +0.44 / +0.44 / +0.38; Nb–Al −0.43 / −0.27 / −0.01; Ti–Nb −0.18 / −0.50 / −0.90.

**Verdict.** The most strongly ordered alloy of the entire campaign (max |α| = 2.30 at 773 K — α values beyond ±1 arise because WC bounds depend on composition, here c_Ti = 0.17). Not a random solid solution at any temperature, and far from random even at 1273 K. Motifs: (1) an **extreme Al–Ti ordering tendency** (−2.3) — on the BCC lattice this is the B2 AlTi order signal, essentially saturated at 773 K; (2) Mo–Ta and Ta–Cr attraction (−1.9, −1.6) with like/near-species avoidance (Cr–Al, Cr–Mo, Al–Mo all ≈ +1.0) — the refractory sub-lattice is chemically demixed into Mo-Ta-Cr and Ti-Al-Nb factions (Ti–Nb −0.90 at 773 K). Strengthens monotonically on cooling, no transition in-window. If cast, this alloy would be expected to form ordered B2-type domains, not a random BCC solution.

### 13.11 Fit-quality summary (all 10 pass the 15 meV/atom CV gate)

| alloy | CV meV/atom | train meV/atom | held-out target cell: MACE vs CE (meV/atom) | method (parameters) |
|---|---|---|---|---|
| O3-Fe0 | 4.3 | 2.1 | −369 vs −369 | ARDR (55) |
| H4 | 6.3 | 1.9 | −137 vs −137 | ARDR (86) |
| H1 | 6.4 | 1.5 | −341 vs −344 | lasso α=0.003 (75) |
| N1 | 7.0 | 1.5 | −267 vs −271 | lasso α=0.003 (75) |
| N2 | 7.1 | 0.6 | −235 vs −236 | lasso α=0.001 (75) |
| O2-TF-Fe0 | 8.0 | 2.1 | −290 vs −295 | lasso α=0.001 (86) |
| H3 | 7.7 | 0.5 | −246 vs −245 | lasso α=0.001 (111) |
| H2 | 8.5 | 1.4 | −201 vs −200 | ARDR (111) |
| H6 | 8.8 | 2.0 | −392 vs −400 | lasso α=0.001 (86) |
| O1-Fe0 | 9.5 | 2.2 | −329 vs −330 | lasso α=0.001 (86) |

### What this method cannot tell us

The rigid-lattice CE-MC answers one narrow question — is the *occupancy* of the parent lattice random at temperature T — and nothing else. It cannot tell us: (1) **static relaxations and size effects beyond the single fixed a0** — every W/Ta/Zr atom is forced onto an average-size cell; real large solutes would locally expand the lattice, shifting ECI magnitudes and the SRO temperature scale (order–disorder temperatures inferred from these runs carry systematic errors of easily ±20–30 %); (2) **vibrational entropy**, absent from the fit; (3) **magnetism** — Cr, Co, Ni, Fe are non-magnetic here, and Cr–Co/–Fe SRO in these systems is known to be magnetism-entangled; (4) **long-range order and true phase separation** — a ~100-atom cell shows SRO but cannot nucleate γ′ precipitates, B2 domains, σ/μ/Laves particles or grain-boundary segregation, and an SRO signal says nothing about how much ordered phase a real casting would make; for the BCC alloys a B2-like SRO signal here does not prove an equilibrium B2 *phase* — that requires long-distance order the small cell cannot sustain; (5) **kinetics** — MC is equilibrium; SRO at 773 K is only realised in a real alloy if diffusion is fast enough on service timescales (at 600–900 K, substitutional diffusion in these systems is slow, so as-cast material can remain metastably random while equilibrium predicts order); (6) the **MACE-MP error bar** (~5–10 meV/atom vs DFT on ordering energies) sits underneath the CV error and both propagate into T-scales; (7) **interstitials and sublattice structure** — everything is forced onto one substitutional lattice (no carbides/oxides, no Ni₃Al off-stoichiometry, no anti-site thermodynamics beyond what the CE captures). The α magnitudes for rare species (W, Mo at 4–5 at%) carry large statistical scatter (frame-to-frame σ ≈ 0.2–0.5). The verdicts are therefore comparative and mechanistic (which pairs order, which segregate, trends with T), not quantitative phase-fraction predictions.

---
