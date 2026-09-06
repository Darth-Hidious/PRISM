# SX500-adjacent preburner materials — candidates for fabrication

*Prepared 2026-09-06 from PRISM research runs of 5–6 September 2026 for review by materials scientists. Every number below is read from the run records (tool provenance store, MACE job store, Quantum ESPRESSO output files); job and record ids are given so each can be checked. Evidence classes: **execution** = computed here; **literature** = cited source; **screening** = empirical descriptor, ranks but does not qualify.*

## 1. What to build

| ID | Composition (at%) | Family | Target phase | Environment | Justification | First test |
|---|---|---|---|---|---|---|
| **O2-TF-Fe0** | Ni60Co12Cr12Al8W4Mo4 | Ni-base, γ/γ′ single crystal | FCC γ + γ′ (Ni3Al) | Oxidizer-rich (near-pure O2, 50–61 MPa, 460–873 K) | Primary blade alloy. Ti- and Fe-free: Ti alloys are NASA's ignition-sensitive class and steels rank more flammable than Ni alloys (NBS 1972; WSTF 1991). | Cast button; XRD/SEM; cyclic oxidation 700–900 K in O2; promoted-ignition screen (ASTM G124) |
| **O2-TF** | Ni54Co12Cr12Al8W4Mo4Fe6 | Ni-base, γ/γ′ | FCC γ + γ′ | Oxidizer-rich | Same blade alloy keeping 6 at% Fe if strength or cost requires; second to O2-TF-Fe0 on oxygen grounds. | As O2-TF-Fe0 |
| **O1-Fe0** | Ni45Co20Cr15W10Mo5Al5 | Ni-base solid solution | FCC γ (+γ′) | Oxidizer-rich | Conservative control; W and Mo for creep. | As above |
| **O3-Fe0** | Ni60Al15Co10Cr10W5 | Ni-base, alumina-former | FCC (+B2 NiAl) | Oxidizer-rich duct/liner | Alumina-forming gas-path alloy; confirm α-Al2O3 scale before promoting. | Cyclic oxidation, scale identification (XRD/EDS) |
| **O4-Fe0** | Ni50Al15Co20Cr15 | Ni-base dual phase | FCC + B2 | Oxidizer-rich liner/manifold | Cast- or wrought-capable; the cheapest of the set. | As above |
| **F1** | Ni40Fe20Co10Cr10Al8W5Mo4Ti3 | Ni–Fe base, γ/γ′ | FCC + γ′ | Fuel-rich (H2-rich, reducing) | After the ONERA THYMONEL 8 precedent; aimed at low hydrogen sensitivity. Ω undefined (Ti–W pair missing from the mixing-enthalpy table). | Tensile + LCF in 34.5 MPa H2 (NASA CR-199837 protocol) |
| **F2** | Fe40Ni25Cr15Al12Mo4W4 | FeCrAl-type | BCC + FCC | Fuel-rich (CH4-rich) | Chosen against coking on the methane side. | Coking exposure 850 K vs Inconel 718 coupon |
| **H1** | Al20Cr20Mo20Nb20Ti20 | Refractory HEA, Al+Cr | BCC (+ protective Al2O3/Cr2O3 scale intended) | Oxidizer-rich — candidate against the Monel K-500 oxygen bar and SX500 mechanical bar (R7) | Al and Cr added to a refractory base to form a protective scale; refractory oxidation and MoO3 volatility are the risk the design answers. Standing against the bars: see 1.1. | Cyclic oxidation 700–1300 K; pesting check; promoted ignition |
| **H2** | Al13Cr7Mo19Nb18Ta26Ti17 | Refractory HEA, Ta-rich, Al+Cr | BCC | Oxidizer-rich (R7) | Ta-rich variant of H1 with the highest melting estimate of the set (≈2505 K). Standing against the bars: see 1.1. | As H1 |
| **H3** | Al20Mo10Nb20Ta10Ti20Zr20 | Refractory HEA (Senkov AlMo0.5NbTa0.5TiZr family) | BCC | Oxidizer-rich (R7), control | Literature RHEA control; stable in MD at 772 and 1000 K (below). | As H1 |
| **H4** | Al4Ti2Co24Cr24Fe23Ni23 | High-entropy superalloy | FCC + γ′ (Al/Ti) | Oxidizer-rich (R7) | Near-equiatomic Co-Cr-Fe-Ni with Al+Ti for γ′. Carries 23 at% Fe and 2 at% Ti — both flagged for the oxygen side (R7 caveat). | Oxidation + promoted-ignition screen before any mechanical work |
| **H5** | Al8Ti4Co20Cr15Fe18Ni35 | High-entropy superalloy, Ni-rich | FCC + γ′ | Oxidizer-rich (R7) | Higher Ni and γ′ fraction than H4; 18 at% Fe, 4 at% Ti — same caveat. | As H4 |
| **H6** | Ni55Al15Co10Cr10W5Mo5 | Ni-rich HEA | FCC + γ′/B2 | Oxidizer-rich (R7) | Ti- and Fe-free Ni-rich HEA; highest moduli of the high-entropy set (below). | As O2-TF-Fe0 |
| **N1** | Nb20Mo20Ta20Ti20Ni20 | Ni-bearing refractory HEA | BCC | Screened; review §11: OUT for oxygen side (refractory oxidation, volatile MoO3); fuel side open | Dynamically stable 80–1000 K; zero imaginary phonon modes. | Oxidation test decides |
| **N2** | Nb20Mo20Ta20W20Ni20 | Ni-bearing refractory HEA | BCC | As N1 | Dynamically stable 80–1000 K. | As N1 |

Order of making: cast button samples of O2-TF-Fe0, O2-TF, O1-Fe0, O3-Fe0, O4-Fe0, F1, F2 first (all pass the Tier 0 screen and carry no titanium on the oxidizer side); H1–H6 follow once their oxygen-side verdicts are written; N1/N2 only if an oxidation test overturns the refractory-oxidation verdict of the review. **No candidate has a measured promoted-ignition threshold; the literature read reports none above 13.8 MPa.**

### 1.1 Where the high-entropy candidates stand against the two bars

*Written by the high-entropy research run (session 20260906_023503) as its closing synthesis; quoted verbatim. Its labels: A = H3, B = H1, B′ = H2, D1 = H4, D2 = H5, E = H6; C = Al20Nb20Ti20V20Si20 was excluded for an incomplete screen.*

**Against the Monel K-500 oxygen bar:** nothing computed here measures oxygen compatibility. Per the review's own §10.5/§11 record, Monel-class is the only class *measured* above ORPB pressure (>68.9 MPa, WSTF) and no ignition data exist above ~13.8 MPa for anything else. E (Ni-rich FCC, Al+Cr formers, small W/Mo fractions) is the closest *by analogy* to the Ni–Cu class; A/B/B′ inherit §11's ruling against bare Mo/Nb/Ta (MoO₃ volatilisation >923 K, interstitial-O embrittlement), and A additionally carries 20 at% Ti — per the review's own NASA ranking, titanium is the ignition-sensitive element, so A is *disqualified-looking on oxygen grounds* despite the best strength record. B′ has the only *measured* protective-scale response of the set, but in air at ambient pressure.

**Against the SX500 mechanical bar:** elastic constants are not yield strength. E's G/B 0.365 / G 66 GPa and D1's G 57 GPa are respectable FCC screening numbers; A has a real literature strength (964 MPa at 1273 K, arc-melted predecessor noted as "strength above 800 °C needs to be enhanced") that plausibly beats polycrystal Ni superalloys at that temperature, but single-crystal γ/γ′ creep capability is a different game and SX500's composition is unpublished. None of this substitutes for creep/LCF testing, and **nothing here substitutes for promoted-ignition testing at ~60 MPa** (ASTM G124-class screen on actual buttons).

**Missing before any of this is believable, per candidate:** A — cyclic oxidation 460–873 K in O2 + ignition screen (and a Ti-reduction variant); B/B′ — high-pO2 promotion test, B′ relax/elastic/MD completion; C — a Tier 0 table entry that can even be computed (Si radius gap) + MACE refuses Si ground states; D1/D2 — CALPHAD γ′ fractions (tier 2 unavailable: pycalphad not importable), √′ lattice-parameter mismatch, intermediate-temperature embrittlement check; E — everything: it is a design with screens, no literature existence.

## 2. Constraints

| Requirement | Name | Statement |
|---|---|---|
| R1 | Hot-oxygen ignition & oxidation resistance (ORPB). | Near-pure O2 at 460–873 K, 50–61 MPa, with particle/friction ignition initiators a documented reality . Metals ranked by NASA/WSTF promoted-ignition data; Ag, Cu, bronze, and Monel are the ignition-resistant benchmarks; most structural superalloys will burn on |
| R2 | Creep/fatigue strength at 600–900 K under 100–800 bar | (turbine blades, disks, hot gas ducts). Single-crystal Ni-base alloys are the proven solution . |
| R3 | HEE resistance (FRPB, hydrogen engines). | Ductility/fatigue loss in high-pressure H2, worst near room temperature, quantified at **34.5 MPa H2** for SSME-class alloys . |
| R4 | Coking/carbon-deposition tolerance (FRPB, methanolox/methane). | Ni-bearing surfaces are active methane-pyrolysis/carbon-formation catalysts in the catalysis literature ; engineering-alloy coking data at FFSC FRPB conditions is sparse (open question). |
| R5 | Thermal-cycling (LCF/TMF) life. | Reusability targets: RD-170 designed for ≥20 flights (achieved 21 hot tests on one unit) ; SSME blade HCF/LCF failures under hydrogen + thermal fatigue are extensively documented . |
| R6 | Manufacturability & inspectability. | High-gradient single-crystal casting + HIP + powder metallurgy are the enablers . |
| R7 | Customer exclusion and target | Monel K-500 excluded by customer (ESA) statement — the oxygen-compatibility baseline to match, not a candidate; SX500-class single-crystal superalloy is the mechanical bar to beat. Relayed by the owner 2026-09-06. |
| Ti/Fe | Oxidizer-side exclusion | Titanium is in NASA's ignition-sensitive class and iron-base alloys rank more flammable than Ni-base in high-pressure oxygen (NBS survey NTRS 19740017534; WSTF NTRS 19920013202) — titanium out and iron minimised on the oxidizer side. |

## 3. Calculations performed

### 3.1 Tier 0 empirical screen (hea_descriptors; class: screening)

Ω = Yang solid-solution parameter (≥ 1.1 favours solid solution), δ = atomic-size mismatch (≤ 6.6 %), VEC = valence-electron concentration (≥ 8 → FCC, < 6.9 → BCC, Guo & Liu), Tm = rule-of-mixtures melting estimate, ΔHmix = Miedema mixing enthalpy.

| Candidate | Ω | δ (%) | VEC | Tm est. (K) | ΔHmix (kJ/mol) |
|---|---|---|---|---|---|
| O2-TF-Fe0 Ni60Co12Cr12Al8W4Mo4 | 2.16 | 4.83 | 8.52 | 1848.9 | -9.06 |
| O2-TF Ni54Co12Cr12Al8W4Mo4Fe6 | 2.55 | 4.79 | 8.40 | 1853.9 | -8.88 |
| O1-Fe0 Ni45Co20Cr15W10Mo5Al5 | 3.37 | 4.84 | 8.25 | 2019.2 | -7.45 |
| O3-Fe0 Ni60Al15Co10Cr10W5 | 1.43 | 5.54 | 8.25 | 1756.4 | -12.31 |
| O4-Fe0 Ni50Al15Co20Cr15 | 1.40 | 5.16 | 8.15 | 1684.6 | -12.36 |
| F1 Ni40Fe20Co10Cr10Al8W5Mo4Ti3 | — | 5.37 | 8.00 | 1881.7 | — |
| F2 Fe40Ni25Cr15Al12Mo4W4 | 2.71 | 5.00 | 7.44 | 1859.1 | -8.60 |
| H1 Al20Cr20Mo20Nb20Ti20 | 1.81 | 4.90 | 4.80 | 2140.1 | -15.84 |
| H2 Al13Cr7Mo19Nb18Ta26Ti17 | 2.83 | 3.52 | 4.83 | 2504.6 | -12.69 |
| H3 Al20Mo10Nb20Ta10Ti20Zr20 | 1.53 | 4.45 | 4.30 | 2169.1 | -20.60 |
| H4 Al4Ti2Co24Cr24Fe23Ni23 | 3.24 | 3.67 | 7.94 | 1837.6 | -7.38 |
| H5 Al8Ti4Co20Cr15Fe18Ni35 | 2.08 | 4.98 | 8.04 | 1763.7 | -11.40 |
| H6 Ni55Al15Co10Cr10W5Mo5 | 1.62 | 5.75 | 8.05 | 1814.8 | -12.80 |
| N1 Nb20Mo20Ta20Ti20Ni20 | 2.05 | 6.19 | 6.00 | 2521.0 | -16.48 |
| N2 Nb20Mo20Ta20W20Ni20 | 3.04 | 5.79 | 6.40 | 2871.8 | -12.64 |

### 3.2 MACE relaxation and elastic constants (class: execution; MACE foundation potential, 100-atom special-quasirandom cells)

| Composition | Relaxed E/atom (eV) | K (GPa) | G (GPa) | E (GPa) | ν | Pugh | Jobs |
|---|---|---|---|---|---|---|---|
| F2 | -7.6907 | — | — | — | — | — | 7H1YJCPY  |
| O2 (original, Ti-bearing) | -7.0685 | 184.3 | 63.2 | 170.2 | 0.346 | ductile | NA3SC794 FAZFFZV1 |
| F1 | -7.3589 | 179.0 | 59.3 | 160.1 | 0.351 | ductile | WPNKWCE3 513F9KH0 |
| N1 Nb20Mo20Ta20Ti20Ni20 | -9.3723 | 189.3 | 39.5 | 110.7 | 0.403 | ductile | 9VY4E8Q7 SSP1BXJK |
| N2 Nb20Mo20Ta20W20Ni20 | -10.3428 | 233.2 | 50.2 | 140.5 | 0.400 | ductile | Q7FSP4EF 5W4VEP0Z |
| C1 NbMoTaW (control) | -11.5272 | 239.8 | 48.7 | 136.9 | 0.405 | ductile | B6DN5T37 0WGZ5G9S |
| M Ni68Cu30Fe2 (Monel-class baseline, excluded) | -5.2888 | 175.6 | 61.0 | 164.0 | 0.344 | ductile | REPJXWK5 5TV48WM5 |
| H3 Al20Mo10Nb20Ta10Ti20Zr20 | -8.4242 | 132.1 | 33.2 | 91.9 | 0.384 | ductile | WK3TKH53 533EY65Y |
| H1 Al20Cr20Mo20Nb20Ti20 | -8.4900 | 166.8 | 40.7 | 112.9 | 0.387 | ductile | SE6FGKQ9 D8BA22N8 |
| H5 Al8Ti4Co20Cr15Fe18Ni35 | -7.0752 | 171.8 | 57.3 | 154.8 | 0.350 | ductile | 3TYK6V7T 6Q06YDKK |
| H4 Al4Ti2Co24Cr24Fe23Ni23 | -7.5771 | 168.3 | 51.0 | 139.0 | 0.362 | ductile | Q6G9H9TA RV6SW0WE |
| H6 Ni55Al15Co10Cr10W5Mo5 | -6.7276 | 181.1 | 66.0 | 176.7 | 0.337 | ductile | 3ZSHRJDV B2VDPYES |

### 3.3 Molecular dynamics — dynamic stability against temperature (mace_md_equilibrate, NVT Langevin, 1 fs, friction 0.01/fs; class: execution)

A leg is *stable* when the cell keeps its structure over the run; the standard deviation is of the energy per atom. This is a dynamic-stability result, not strength or oxidation.

| Composition | Set T (K) | Mean T (K) | E/atom (eV) | σ(E) | Steps | Stable | Status | Job |
|---|---|---|---|---|---|---|---|---|
| H1 Al20Cr20Mo20Nb20Ti20 | 772 | — | — | — | 1000 | — | failed — module is not installed as a submodule | A187CAXX |
| H1 Al20Cr20Mo20Nb20Ti20 | 1000 | — | — | — | 1000 | — | failed — module is not installed as a submodule | 9CFNX17C |
| H3 Al20Mo10Nb20Ta10Ti20Zr20 | 772 | 783.8 | -8.3201 | 0.0073 | 1000 | True | succeeded | GJFF105Z |
| H3 Al20Mo10Nb20Ta10Ti20Zr20 | 1000 | 985.2 | -8.2885 | 0.0092 | 1000 | True | succeeded | XN96X2G4 |
| H4 Al4Ti2Co24Cr24Fe23Ni23 | 772 | — | — | — | 1000 | — | running | 3VQR3V70 |
| H4 Al4Ti2Co24Cr24Fe23Ni23 | 1000 | — | — | — | 1000 | — | running | AF904H57 |
| H5 Al8Ti4Co20Cr15Fe18Ni35 | 772 | — | — | — | 1000 | — | failed — module is not installed as a submodule | 6MECMA0X |
| H5 Al8Ti4Co20Cr15Fe18Ni35 | 1000 | — | — | — | 1000 | — | failed — module is not installed as a submodule | WJC5YHW1 |
| H6 Ni55Al15Co10Cr10W5Mo5 | 772 | — | — | — | 1000 | — | running | 9VRF2K56 |
| H6 Ni55Al15Co10Cr10W5Mo5 | 1000 | — | — | — | 1000 | — | running | KH72GFM0 |
| N1 Nb20Mo20Ta20Ti20Ni20 | 80 | 77.5 | -9.3624 | 0.0004 | 1000 | True | succeeded | W334YCY1 |
| N1 Nb20Mo20Ta20Ti20Ni20 | 300 | 309.6 | -9.3347 | 0.0020 | 1000 | True | succeeded | CDVW15SN |
| N1 Nb20Mo20Ta20Ti20Ni20 | 772 | 780.5 | -9.2645 | 0.0049 | 1000 | True | succeeded | KGMDJS1G |
| N1 Nb20Mo20Ta20Ti20Ni20 | 1000 | 1009.7 | -9.2469 | 0.0094 | 1000 | True | succeeded | X2Q0B5BN |
| N2 Nb20Mo20Ta20W20Ni20 | 80 | 77.9 | -10.3331 | 0.0008 | 1000 | True | succeeded | 15MZ8V4D |
| N2 Nb20Mo20Ta20W20Ni20 | 300 | 280.7 | -10.3086 | 0.0013 | 1000 | True | succeeded | R08Z523K |
| N2 Nb20Mo20Ta20W20Ni20 | 772 | 730.9 | -10.2523 | 0.0064 | 1000 | True | succeeded | 4RJG0YMP |
| N2 Nb20Mo20Ta20W20Ni20 | 1000 | 1001.0 | -10.2148 | 0.0048 | 1000 | True | succeeded | BNRKZ55T |
| O1 (original, Fe-bearing) | 772 | 752.9 | -7.5438 | 0.0059 | 2000 | True | succeeded | NCP0RXFH |
| O1 + 2 O interstitials | 772 | 769.5 | -7.4033 | 0.0091 | 2000 | True | succeeded | 09REBB22 |
| O2 (original, Ti-bearing) | 772 | — | — | — | 2000 | — | failed — CURRENT_PATCHER is None in finally block | 16J4XG4D |
| O2 (original, Ti-bearing) | 772 | 801.8 | -6.9671 | 0.0094 | 2000 | True | succeeded | D8QCGQVJ |

![MD energy vs temperature](chart_md_energy.png)


### 3.4 Phonons (mace_phonon_harmonic; class: execution)

| Composition | Imaginary modes | Harmonically stable | F_vib(0 K) → F_vib(1000 K) (eV/atom) | Quality | Job |
|---|---|---|---|---|---|
| N1 Nb20Mo20Ta20Ti20Ni20 | 0 | True | 0.026 → -0.436 | harmonic_supercell_identity_4q | 26HAKRVW |

Thermal expansion was not derived: the quasi-harmonic method needs three volumes and one was computed.


### 3.5 Quantum ESPRESSO (pw.x 7.4.1, PseudoDojo NC SR v0.4 PBE standard; class: execution)

These confirm the DFT path runs on the elements involved. They are pseudopotential total energies on a different reference from MACE, and the two-atom cells are ordered pairs, not the candidate alloys — **not** a per-atom cross-check of the MACE energies. A same-reference anchor on a candidate's own cell is still owed.

| Cell | Calculation | Cutoffs | k-mesh | Total energy (Ry) | SCF iterations | Run by |
|---|---|---|---|---|---|---|
| fcc Ni, 1 atom, a=3.524 Å | scf | 100 / 400 Ry (caller) | 21×21×21 (286 irr.) | -335.5755 | 7 | maintainer rerun after tool fix (not an agent computation) |
| MoNb , 2 atoms | scf | 68.0 / 272.0 Ry (caller) | 14×14×14 | -265.1379 | — | agent, run 10 |
| AlZr , 2 atoms | scf | 60.0 / 240.0 Ry (caller) | 13×13×13 | -105.9195 | — | agent, run 11 |

![Elastic constants](chart_moduli.png)


### 3.6 Not computed

- CALPHAD phase fractions, σ/μ phase risk and Scheil solidification (no thermodynamic database on this machine).
- Cluster expansion of the refractory sublattice (icet installed on 6 September, not yet run).
- Classical MD with LAMMPS (no validated interatomic potential for these compositions).
- Oxidation kinetics, promoted-ignition thresholds, hydrogen embrittlement, coking, creep and fatigue — all require experiment; no computation here substitutes for them.
- Four run-11 MD legs failed on a PRISM tool concurrency bug (torch.fx model load); four more were still running when this report was built and are listed as such.


## 4. Sources

### 4.1 Literature actually cited (numbered as in the full review)

1. lpre.de (NPO Energomash compilation), "РД-170 (11Д521) и РД-171 (11Д520)" — GG 53.5 MPa / O/F 54.3 / outlet gas 190–600 °C; turbine inlet 772 K at 50.9 MPa; anti-ignition materials & coating practice; failure history 1990/2007/2009. https://lpre.de/energomash/RD-170/
2. Astronautix, "RD-170" — chamber 300 atm; ~400 °C oxygen-rich gas. http://www.astronautix.com/r/rd-170.html
3. Astronautix, "RD-0120" — single fuel-rich preburner at 527 °C driving the HP turbopump. http://www.astronautix.com/r/rd-0120.html
4. *System Analysis of a 35-tonf-Class FFSC Methane Engine and Investigation of Oxidizer-Rich Preburner Operating Conditions*, J. Inst. Sci. Tech. Info. (2026), doi:10.54726/jisti.40.1.3 — ORPB 700 K / 610 bar / O/F 63; FRPB 850 K / 570 bar / O/F 0.13.
5. Wikipedia, "SpaceX Raptor" — FFSC, chamber pressure ~300 bar; SX300→SX500; SX500 rated 12,000 psi hot O2. https://en.wikipedia.org/wiki/SpaceX_Raptor
6. NextBigFuture — SX500 developed by SpaceX to handle hot oxygen at ~800 atm (press). https://www.nextbigfuture.com/
7. Fabbaloo — SX300/SX500/SX600 nickel-rich Inconel-family single-crystal superalloys (press). https://www.fabbaloo.com/
8. Peters, Biondo, DeLuca, *Investigation of Advanced Processed Single-Crystal Turbine Blade Alloys*, NASA CR-199837 (1995) — PWA 1482/1484 tailored for H2 turbopumps; screening in 34.5 MPa H2; ~10× fatigue life vs PWA 1480. https://ntrs.nasa.gov/citations/19960017548
9. Wikipedia, "BE-4" — oxygen-rich staged combustion, LOX/CH4, Pc 140 bar, first flight 2024. https://en.wikipedia.org/wiki/BE-4
10. Wikipedia, "YF-100" — Chinese LOX/kerosene oxidizer-rich staged combustion. https://en.wikipedia.org/wiki/YF-100
11. Caron, Cornu, Khan, De Monicault, *Development of a Hydrogen Resistant Superalloy for Single Crystal Blade Application in Rocket Engine Turbopumps*, Superalloys 1996, 53–60, doi:10.7449/1996/superalloys_1996_53_60 — THYMONEL 8 (single-crystal Ni+Fe, low HEE + high fatigue, turbopump airfoils).
12. Engine History, "SSME Part 6: HPFTP Turbine Blade Failures" — directionally solidified MAR-M-246 blades. https://www.enginehistory.org/Rockets/SSME/SSME6.pdf
13. NASA Facts, *Integrated Powerhead Demonstrator* (Wikisource) — dual ox-rich + fuel-rich preburners (FFSC). https://en.wikisource.org/wiki/NASA_Facts/Integrated_Powerhead_Demonstrator
14. NASA NTRS 20040084662 — IPD: 250-klbf H2/O2 full-flow staged-combustion demonstrator. https://ntrs.nasa.gov/api/citations/20040084662/downloads/20040084662.pdf
15. Astronautix, "IPD" — Aerojet-designed and tested oxidizer preburner. http://www.astronautix.com/i/ipd.html
16. GlobalSpec, "Preburner Tested for AR1 Rocket Engine" — LOX/kerosene ORSC, 500 klbf. https://insights.globalspec.com/article/4992/
17. US Air Force (wpafb.af.mil), "Air Force Demonstrates Rocket Engine Preburner for Advanced Liquid Rocket Engines" — HCB preburner, "extreme oxygen environments that conventional metals cannot survive." https://www.wpafb.af.mil/News/Article-Display/Article/1996823/
18. Wikipedia, "LE-7" — LH2/LOX staged combustion (Japan). https://en.wikipedia.org/wiki/LE-7
19. Vanoverbeke & Claus, *SSME Fuelside Preburner Two-Dimensional Analysis*, NASA TM-87299 (1986). https://ntrs.nasa.gov/citations/19860014145
20. *Fatigue Failure of Space Shuttle Main Engine Turbine Blades* (NASA techdoc via Internet Archive). https://archive.org/stream/nasa_techdoc_20000004407/
21. US Patent 5,023,050 — Superalloy for high-temperature hydrogen service (SSME context). https://patents.google.com/patent/US5023050A/en
22. Bowen & Nagy, *The evaluation of single crystal superalloys for turbopump blades in gaseous hydrogen* (PWA 1480E). https://www.semanticscholar.org/paper/9259952fa81f7819e1bb24f2076a620e66ac2008
23. Fritzemeier & Schnittgrund, *The influence of advanced processing on PWA 1480*, NASA (1989). http://hdl.handle.net/2060/19910015004
24. Petrasek & Stephens, *Fiber reinforced superalloys for rocket engines*, NASA (1988) — SSME blade environment: temperature, hydrogen, HCF, thermal fatigue/shock. http://hdl.handle.net/2060/19890006619
25. Morea & Wu, *Advanced High Pressure O2/H2 Technology*, NASA (1985). https://ntrs.nasa.gov/citations/19850018551
26. *Role of Microstructure on Hydrogen Embrittlement of Nickel Base Superalloy Single Crystals* (1994), doi:10.1002/9781118803363.ch81
27. *Assessment of Hydrogen Embrittlement in High-Alloy Chromium-Nickel Steels and Alloys in Hydrogen at High Pressures and Temperatures*, Phys. Met. Metallogr. (2018), doi:10.1007/s11223-019-00035-2
28. *Promoted Ignition Behavior of Engineering Alloys in High-Pressure Oxygen*, ASTM STP, doi:10.1520/stp26741s
29. *Friction-Induced Ignition of Metals in High-Pressure Oxygen*, ASTM STP, doi:10.1520/stp26742s
30. *Promoted Ignition-Combustion Behavior of Engineering Alloys at Elevated Temperatures and Pressures in Oxygen Gas Mixtures* (2000), doi:10.1520/stp12491s; *…Cast and Wrought Engineering Alloys in Oxygen-Enriched Atmospheres* (2003), doi:10.1520/stp11587s; *Ignition of Metals and Alloys in Gaseous Oxygen by Frictional Heating* (1986), doi:10.1520/stp19308s
31. *Frictional Ignition of Metals in High Pressure Oxygen: A Critical Reassessment of NASA Test Data*, AIAA 2023-1489, doi:10.2514/6.2023-1489
32. *Oxide tribolayer breakdown on sliding metal contacts drives thermal ignition*, Tribology Int. (2024), doi:10.1016/j.triboint.2024.110484
33. *Frictional ignition of Ti40 fireproof titanium alloys for aero-engine in oxygen-containing media*, Trans. Nonferrous Met. Soc. China (2013), doi:10.1016/s1003-6326(13)62728-4
34. US Patent 6,007,645 — Advanced high-strength, highly oxidation-resistant single-crystal superalloy (low Cr). https://patents.google.com/patent/US6007645A/en
35. *Experiments on an Oxidizer-Rich Preburner for Staged Combustion Cycle Rocket Engines*, J. Propulsion & Power (2014), doi:10.2514/1.b35202 (abstract via ResearchGate: https://www.researchgate.net/publication/277674980)
36. *Oxidizer-Rich Staged Combustion Cycle Preburner and Main Chamber Injector Testing at Purdue University*, AIAA 2004-3524, doi:10.2514/6.2004-3524
37. *Oxidizer-Rich Preburner Technology for Oxygen/Hydrogen Full Flow Cycle Applications* (2004), doi:10.2514/5.9781600866760.0683.0701
38. *Test verification of LOX/RP-1 high-pressure fuel/oxidizer-rich preburner designs* (1982/1983), doi:10.2514/6.1982-1153; doi:10.2514/3.8588
39. *Oxidizer-rich staged combustion rocket engines use and development in Russia* (1995), doi:10.2514/6.1995-3607
40. *LOX/Methane Studies for Fuel Rich Preburner* (2003), doi:10.2514/6.2003-5063
41. KSPE: *Development of Design Code for Oxidizer-Rich Preburner… Using Cantera* (2022), doi:10.6108/kspe.2022.26.6.010; *Results of Cold Flow Test and Design of Injectors for Oxidizer-rich Preburner* (2018), doi:10.6108/kspe.2018.22.1.052; *Numerical Analysis on Cooling Characteristics of Oxidizer-Rich Preburner* (2013), doi:10.6108/kspe.2013.17.3.067
42. *Linear Acoustic Analysis of the Preburner of an Oxidizer-Rich Staged Combustion Engine*, JPP (2019), doi:10.2514/1.b37132 (+ open PDF at https://www.yang.gatech.edu/publications/Journal/JPP+(2019,+Lioi)+ORSC+preburner.pdf)
43. Filin & Mkrtchyan, *Little-known facts of turbopump unit creation history in liquid rocket engine*, Vestnik MAI (2021), doi:10.34759/vst-2021-2-63-72
44. Chinese-language industry sources on GH-series superalloys in rocket engines (non-peer-reviewed): https://zhuanlan.zhihu.com/p/536997770 ; http://www.bq-sam.com/page95?article_id=99 ; https://www.hangbogroup.com/article/fb3eb163.html
45. YF-100K (130-t pump-back-swing staged-combustion engine, Chinese wiki). https://sat.huijiwiki.com/wiki/YF-100K
46. *Enhanced Oxidation Resistance of Ultrafine-Grain Microstructure AlCoCrFeNi High Entropy Alloy*, ACS Omega (2022), doi:10.1021/acsomega.1c06014 — PMC9026100, ingested into the local knowledge graph this run (claims: 98 μm as-cast grain → ~1 μm refined layer, 300–650 μm depth).
47. *High oxidation resistance of AlCoCrFeNi HEA through severe shear deformation processing*, J. Alloys Compd. (2022), doi:10.1016/j.jallcom.2022.165385
48. *Effect of Ti Addition on the Microstructure and High-Temperature Oxidation Property of AlCoCrFeNi HEA*, Korean J. Met. Mater. (2020), doi:10.1007/s12540-020-00708-7
49. *High-Temperature Oxidation of CoCrFeMnNi- and AlCoCrFeNi-Based HEAs: A Critical Review* (2026), doi:10.1007/s11085-026-10454-7
50. *Y/Sc Co-Doping Enhances Cyclic Oxidation Resistance of AlCoCrFeNi HEA* (2026), doi:10.1007/s11085-026-10426-x
51. *Oxidation behavior of arc melted AlCoCrFeNi multi-component HEAs*, J. Alloys Compd. (2016), doi:10.1016/j.jallcom.2016.02.257
52. NOMAD/OPTIMADE federated record: HEA Ti6WCr20Co16Ni60Al5 (ρ ≈ 8750 kg/m³). https://nomad-lab.eu/prod/v1/gui/entry/id/h_HJopxvQ7uejJukYthhaQ/BXal9lJHZjtpABB78ZRB1XyL9hnA
53. *Improvement of Hydrogen-Resistant Gas Turbine Engine Blades (single-crystal manufacturing, ceramic molds)*, open access (2024), PMC11396716. https://pmc.ncbi.nlm.nih.gov/articles/PMC11396716/
54. Methane carbon-formation/coking on Ni catalysts — catalysis literature anchor, doi:10.1016/s0167-2991(01)80186-5 (Studies in Surface Science and Catalysis). Alloy-specific coking data remains an open question (OQ4).
55. Tier-0 descriptors and Tier-1 MACE results, Section 5; MACE-MH-1 `omat_pbe` head, seed 20260506, cache keys `205808ae031c11985893fa4e1be2a01bf3f4a6e233f28336bd81408d76861a13` (relax) and `6baadf640fad4c6b57e91033ca41b980490be278624c59f54c85d6355ad70d5d` (elastic). Machine tiers: 0–1 available; 2–3 not installed.
56. Reed, *The Superalloys: Fundamentals and Applications* (2006). (open PDF copy seen in this run's searches)
57. Segersäll, *On Thermomechanical Fatigue of Single-Crystal Superalloys*, Linköping PhD thesis (2014), doi:10.3384/diss.diva-111643
58. Perrut, Caron, Thomas, Couret, *High temperature materials for aerospace applications: Ni-based superalloys and γ-TiAl alloys*, C. R. Physique (2018), doi:10.1016/j.crhy.2018.10.002
59. Yang et al., *Influence of platform position on stray grain nucleation in Ni-based single-crystal blade clusters*, China Foundry (2021), doi:10.1007/s41230-021-1053-3
60. Wikipedia, "YF-215" — Chinese 200-ton-class FFSC methalox engine in development. https://en.wikipedia.org/wiki/YF-215
NTRS 19740017534 — A survey of compatibility of materials with high pressure oxygen service (NBS for MSFC; NTRS date 1972).
NTRS 19920013202 — Test methods for determining the suitability of metal alloys for use in oxygen-enriched environments (NASA WSTF, 1991).

### 4.2 Software and data

- Quantum ESPRESSO 7.4.1 (pw.x), PseudoDojo NC SR v0.4 PBE standard set (cutoff hints per element from the set's own files).
- MACE foundation potential via PRISM's mace tools (relaxation, elastic constants, NVT dynamics, harmonic phonons).
- Tier 0 descriptors: Yang Ω/δ, Guo & Liu VEC, Miedema ΔHmix (PRISM hea_descriptors).
- Materials Project / OPTIMADE lookups for reference phases (γ′-Ni3Al on the hull, E_f −0.426 eV/atom; L1₂ a ≈ 3.564 Å).
- Records: PRISM provenance store sessions 20260905_220957_a078f501, 20260905_223133_6392089c, 20260906_002859_c1e2e95d, 20260906_013528_ba7e29ee, 20260906_023503_1cb6c2ce; MACE job store ~/.local/state/mace-mcp/cache/mace_jobs.db.

### 4.3 Full record

The complete research narrative, verdict reasoning and citation verification are in `SX500_FFSC_preburner_materials_review.md` (444 lines), kept as the archival document behind this report.
