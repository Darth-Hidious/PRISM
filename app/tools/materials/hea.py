# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""High-Entropy Alloy (HEA) / Multi-Principal-Element Alloy (MPEA) design tools.

SCOPE (honest): these are empirical Hume-Rothery-style SCREENING heuristics —
a fast first-pass "is a single-phase solid solution plausible?" flag built
from open literature parameters. They are NOT a CALPHAD phase-equilibria
calculation: no Gibbs-energy minimization, no phase fractions, no temperature
dependence, no ternary+ interaction terms. A commercial database like
Thermo-Calc TCHEA delivers those; this tool deliberately does not claim to.
Use it to triage compositions before committing to CALPHAD/DFT/experiment.

`hea_descriptors`: computes the formability descriptors that decide whether a
multi-principal-element alloy forms a solid solution vs intermetallic vs
segregated: mixing enthalpy (ΔH_mix), entropy of mixing (ΔS_mix), the Yang Ω
parameter, valence electron concentration (VEC), atomic-radius mismatch (δ),
and the Yang solid-solution criterion.

References:
  - Yang & Zhang (2012), "Prediction of solid solution formation...", Mater. Chem. Phys.
    (the Ω + δ criterion).
  - Guo, Liu (2011), "A valence electron concentration criterion for HEAs",
    Intermetallics (the VEC → FCC/BCC criterion).
  - Takeuchi, Inoue (2005), "Classification of bulk metallic glasses by atomic
    size difference, ΔH_mix and ΔS_mix" (the ΔH_mix pair table source).
  - Senkov et al. for refractory HEA validation compositions.
"""

from __future__ import annotations

import logging
import math
from typing import Any

from app.tools.base import Tool, ToolRegistry

logger = logging.getLogger(__name__)

# Atomic fractions are accepted as written only. This tolerance permits normal
# decimal round-off without treating ratios or percentages as fractions.
COMPOSITION_SUM_TOLERANCE = 1e-6
_ELEMENT_SYMBOLS = frozenset(
    "H He Li Be B C N O F Ne Na Mg Al Si P S Cl Ar K Ca Sc Ti V Cr Mn Fe Co "
    "Ni Cu Zn Ga Ge As Se Br Kr Rb Sr Y Zr Nb Mo Tc Ru Rh Pd Ag Cd In Sn Sb "
    "Te I Xe Cs Ba La Ce Pr Nd Pm Sm Eu Gd Tb Dy Ho Er Tm Yb Lu Hf Ta W Re "
    "Os Ir Pt Au Hg Tl Pb Bi Po At Rn Fr Ra Ac Th Pa U Np Pu Am Cm Bk Cf Es "
    "Fm Md No Lr Rf Db Sg Bh Hs Mt Ds Rg Cn Nh Fl Mc Lv Ts Og".split()
)

# Valence electron concentration (VEC) per element — the Guo/Liu convention
# used universally in HEA literature. For transition metals this is the group
# number; for the common HEA p-block elements it's the standard value. A pure
# transition-metal VEC comes from pymatgen's group, but we tabulate to match
# the published HEA values exactly (pymatgen's .valence raises on ambiguous TMs).
_VEC: dict[str, float] = {
    "Sc": 3, "Ti": 4, "V": 5, "Cr": 6, "Mn": 7, "Fe": 8, "Co": 9, "Ni": 10,
    "Cu": 11, "Zn": 12, "Y": 3, "Zr": 4, "Nb": 5, "Mo": 6, "Tc": 7, "Ru": 8,
    "Rh": 9, "Pd": 10, "Ag": 11, "Cd": 12, "Hf": 4, "Ta": 5, "W": 6, "Re": 7,
    "Os": 8, "Ir": 9, "Pt": 10, "Au": 11, "Al": 3, "Si": 4, "Ga": 3, "Ge": 4,
    "Sn": 4, "Sb": 5, "Pb": 4, "Bi": 5, "C": 4, "N": 5, "B": 3, "P": 5,
    "La": 3, "Ce": 3, "Pr": 3, "Nd": 3, "Gd": 3, "Dy": 3, "Mg": 2, "Li": 1,
    "Be": 2, "Na": 1, "K": 1, "Ca": 2, "Sr": 2, "Ba": 2,
}

# Binary mixing enthalpy (ΔH_mix, kJ/mol) for common HEA element pairs, from
# the Miedema model as tabulated by Takeuchi & Inoue (2005) — the standard
# reference used in HEA screening. Symmetric: ΔH(A,B) == ΔH(B,A). Missing pairs
# default to 0 (ideal mixing).
#
# VERIFICATION STATUS (adversarial review a7bfe76 vs the printed Takeuchi-Inoue
# 2005 table + the Senkov refractory-HEA literature):
#   VERIFIED CORRECT: the Al row; the Cr/Mn/Fe/Co/Ni 3d block (reproduces the
#     Cantor-alloy ΔH_mix = -4.16 kJ/mol exactly); the Nb/Mo/Ta/W refractory
#     set as corrected below (Senkov et al. pair values).
#   CORRECTED against the printed table: Ni-Ti (-18→-35), Ni-Nb (-9→-30),
#     Ni-Zr (-34→-49), Mo-Si (-18→-35), Cr-Ta (-9→-7), Mo-Ta (-1→-5),
#     Co-Ti (-18→-28); added missing Nb-W (-8) and Ta-W (-7).
#   TODO(verify vs printed Takeuchi-Inoue 2005): cells OUTSIDE the sets above
#     are NOT re-verified — in particular the Si/metalloid rows (literature
#     silicide values are far more negative than tabulated here, e.g. Ti-Si
#     canon ≈ -66 vs -26 here), Co-Nb, Co-Hf, Ti-V, and the Mg/Sc rows. Treat
#     ΔH_mix for compositions leaning on those cells as indicative only.
_DH_MIX_PAIRS: dict[tuple[str, str], float] = {}
_PAIR_DATA = """
Al-Co -19 Al-Cr -10 Al-Cu -1 Al-Fe -11 Al-Hf -31 Al-Mg -2 Al-Mn -19
Al-Mo -15 Al-Nb -18 Al-Ni -22 Al-Sc -38 Al-Si -19 Al-Ta -19 Al-Ti -30
Al-V -16 Al-W -13 Al-Zr -44
Co-Cr -4 Co-Cu 6 Co-Fe -1 Co-Hf -21 Co-Mn -5 Co-Mo -5 Co-Nb -10 Co-Ni 0
Co-Sc -27 Co-Si -21 Co-Ta -11 Co-Ti -28 Co-V -14 Co-W -4 Co-Zr -41
Cr-Cu 12 Cr-Fe -1 Cr-Hf -7 Cr-Mn 2 Cr-Mo 0 Cr-Nb -7 Cr-Ni -7 Cr-Sc -18
Cr-Si -20 Cr-Ta -7 Cr-Ti -7 Cr-V -2 Cr-W 1 Cr-Zr -12
Cu-Fe 13 Cu-Hf -15 Cu-Mg -3 Cu-Mn -4 Cu-Mo -3 Cu-Nb -3 Cu-Ni 4 Cu-Sc -15
Cu-Si -6 Cu-Ta 1 Cu-Ti -9 Cu-V -2 Cu-W 1 Cu-Zr -23
Fe-Hf -19 Fe-Mn 0 Fe-Mo -2 Fe-Nb -6 Fe-Ni -2 Fe-Sc -16 Fe-Si -18 Fe-Ta -10
Fe-Ti -17 Fe-V -7 Fe-W 0 Fe-Zr -25
Hf-Mo -4 Hf-Nb -4 Hf-Si -24 Hf-Ta -3 Hf-Ti 0 Hf-V -2 Hf-W -2 Hf-Zr 0
Mg-Mn 4 Mg-Mo 9 Mg-Nb -4 Mg-Ni -4 Mg-Si -9 Mg-Sn -6 Mg-Ti -16 Mg-Zr -6
Mn-Mo 5 Mn-Nb -5 Mn-Ni -8 Mn-Si -19 Mn-Ta -8 Mn-Ti -8 Mn-V -1 Mn-Zr -15
Mo-Nb -6 Mo-Ni -7 Mo-Si -35 Mo-Ta -5 Mo-Ti -4 Mo-V -1 Mo-W 0 Mo-Zr -6
Nb-Ni -30 Nb-Si -24 Nb-Ta 0 Nb-Ti -2 Nb-V -1 Nb-W -8 Nb-Zr -4
Ni-Si -23 Ni-Ta -13 Ni-Ti -35 Ni-V -18 Ni-W -3 Ni-Zr -49
Si-Ta -15 Si-Ti -26 Si-V -17 Si-W -12 Si-Zr -36
Ta-Ti -1 Ta-V -1 Ta-W -7 Ta-Zr -2
Ti-V 0 Ti-Zr -3 V-Zr -4
"""
for _line in _PAIR_DATA.strip().split("\n"):
    _parts = _line.split()
    # Format: "El1-El2 value El3-El4 value ..." — each pair is hyphen-joined.
    for _i in range(0, len(_parts), 2):
        _pair, _val = _parts[_i], _parts[_i + 1]
        _a, _b = _pair.split("-")
        _DH_MIX_PAIRS[(_a, _b)] = float(_val)
        _DH_MIX_PAIRS[(_b, _a)] = float(_val)


def _dh_mix_for_pair(a: str, b: str) -> float:
    """Binary mixing enthalpy ΔH_mix(A,B) in kJ/mol (0 = ideal)."""
    return _DH_MIX_PAIRS.get((a, b), 0.0)


def _validate_composition_components(
    elems: list[str],
    fracs: list[float],
    allowed_elements: set[str] | None = None,
) -> tuple[list[str], list[float]]:
    """Validate atomic fractions without normalizing or combining entries.

    Fractions must be finite, strictly positive, and sum to 1.0 within an
    absolute tolerance of ``1e-6``. Rejecting outside that tolerance preserves
    the distinction between what a caller proposed and what was evaluated.
    """
    if len(elems) < 2:
        raise ValueError("composition must contain at least two elements")
    if len(elems) != len(fracs):
        raise ValueError("element and fraction counts differ")
    if len(set(elems)) != len(elems):
        raise ValueError("each element may appear only once")

    invalid = [element for element in elems if element not in _ELEMENT_SYMBOLS]
    if invalid:
        raise ValueError(f"invalid element symbols: {', '.join(invalid)}")
    if allowed_elements is not None:
        outside = [element for element in elems if element not in allowed_elements]
        if outside:
            raise ValueError(
                "elements outside allowed set: " + ", ".join(outside)
            )

    checked: list[float] = []
    for element, value in zip(elems, fracs):
        if isinstance(value, bool):
            raise ValueError(f"fraction for {element} must be numeric, not boolean")
        try:
            fraction = float(value)
        except (TypeError, ValueError) as exc:
            raise ValueError(f"fraction for {element} must be numeric") from exc
        if not math.isfinite(fraction):
            raise ValueError(f"fraction for {element} must be finite")
        if fraction <= 0:
            raise ValueError(f"fraction for {element} must be strictly positive")
        checked.append(fraction)

    total = math.fsum(checked)
    if abs(total - 1.0) > COMPOSITION_SUM_TOLERANCE:
        raise ValueError(
            f"composition fractions must sum to 1.0 ± "
            f"{COMPOSITION_SUM_TOLERANCE:.6f}; got {total:.6f}"
        )
    return list(elems), checked


def _parse_composition_or_raise(
    spec: str | dict[str, float],
) -> tuple[list[str], list[float]]:
    if isinstance(spec, dict):
        elems = list(spec.keys())
        fracs = list(spec.values())
    elif isinstance(spec, str):
        try:
            from pymatgen.core import Composition

            # get_el_amt_dict preserves the caller's raw coefficients. Using
            # get_atomic_fraction here would silently turn a sum-2 proposal
            # into a different, apparently valid material.
            amounts = Composition(spec).get_el_amt_dict()
        except Exception as exc:
            raise ValueError(f"could not parse composition: {spec}") from exc
        elems = list(amounts)
        fracs = list(amounts.values())
    else:
        raise ValueError("composition must be a formula string or fractions dict")
    return _validate_composition_components(elems, fracs)


def _parse_composition(spec: str | dict[str, float]) -> tuple[list[str], list[float]] | None:
    """Parse a strict unit-sum atomic-fraction composition.

    Returns ``None`` for invalid input for backward-compatible internal callers.
    Use ``_parse_composition_or_raise`` at user-facing boundaries that need the
    exact rejection reason.
    """
    try:
        return _parse_composition_or_raise(spec)
    except (TypeError, ValueError):
        return None


def _metallic_radius(sym: str) -> float | None:
    """Goldschmidt (CN12) metallic radius in Å from pymatgen (None if unavailable).

    The HEA δ literature (Yang & Zhang 2012; Guo & Liu 2011) uses metallic
    CN12 radii. pymatgen's `.atomic_radius` is the Slater set — quantized to
    0.05 Å — which inflates δ by 30-60% and even inverts the sign of Al's size
    mismatch vs the 3d metals (Slater Al 1.25 Å < 3d radii, metallic Al 1.43 Å
    > 3d radii). With `.metallic_radius`, δ(CoCrFeMnNi) = 1.12% (literature ≈1%).
    """
    try:
        from pymatgen.core.periodic_table import Element

        r = Element(sym).metallic_radius
        if r is None:
            return None
        r = float(r)
        return r if math.isfinite(r) and r > 0 else None
    except Exception:
        return None


def compute_hea_descriptors(elems: list[str], fracs: list[float]) -> dict[str, Any]:
    """Compute the full HEA formability descriptor set.

    Pure math — no network, no ML, no database. Returns a dict with:
      - ΔS_mix (configurational entropy of mixing), J/(mol·K)
      - ΔH_mix (mixing enthalpy via Miedema pair table), kJ/mol
      - Ω (Yang parameter: Tm·ΔS_mix / |ΔH_mix|)
      - VEC (valence electron concentration)
      - δ (atomic-size mismatch, %, Goldschmidt CN12 metallic radii)
      - Δχ (electronegativity difference, Pauling)
      - phase_prediction (solid_solution | solid_solution_segregation_risk |
        intermetallic_or_segregated) via Yang Ω+δ, with a demixing flag for
        positive-ΔH_mix compositions (e.g. Cu-bearing 3d HEAs)
    """
    elems, fracs = _validate_composition_components(elems, fracs)
    n = len(elems)
    R = 8.314  # J/(mol·K) gas constant

    # ΔS_mix = -R Σ c_i ln(c_i)  (configurational / ideal mixing entropy)
    dS_mix = -R * sum(f * math.log(f) for f in fracs if f > 0)

    # ΔH_mix = 4 Σ_{i≠j} c_i c_j ΔH_mix(i,j)  (Miedema, regular-solution form)
    dH_mix = 0.0
    for i in range(n):
        for j in range(i + 1, n):
            dH_mix += fracs[i] * fracs[j] * _dh_mix_for_pair(elems[i], elems[j])
    dH_mix *= 4.0  # kJ/mol

    # VEC = Σ c_i VEC_i  (Guo/Liu)
    vec = sum(fracs[i] * _VEC.get(elems[i], 0.0) for i in range(n))

    # δ = sqrt(Σ c_i (1 - r_i/r_bar)^2)  ×100 (%)  (atomic-size mismatch,
    # Goldschmidt CN12 metallic radii — the convention of the HEA δ literature)
    radii = [_metallic_radius(e) for e in elems]
    if all(r is not None for r in radii):
        r_bar = sum(fracs[i] * radii[i] for i in range(n))
        delta = 100.0 * math.sqrt(
            sum(fracs[i] * (1.0 - radii[i] / r_bar) ** 2 for i in range(n))
        )
    else:
        delta = None  # radius missing for some element

    # Δχ (electronegativity mismatch, Pauling) — optional, if available
    dchi = None
    try:
        from pymatgen.core.periodic_table import Element

        chis = [Element(e).X for e in elems]
        if all(c is not None for c in chis):
            chi_bar = sum(fracs[i] * chis[i] for i in range(n))
            dchi = math.sqrt(sum(fracs[i] * (chis[i] - chi_bar) ** 2 for i in range(n)))
    except Exception:
        pass

    # Melting point estimate (weighted average of pure-element Tm, °C→K)
    try:
        from pymatgen.core.periodic_table import Element

        tms = [Element(e).melting_point for e in elems]
        if all(t is not None for t in tms):
            tm_bar = sum(fracs[i] * tms[i] for i in range(n))  # K
        else:
            tm_bar = None
    except Exception:
        tm_bar = None

    # Ω = Tm·ΔS_mix / |ΔH_mix|  (Yang solid-solution parameter; ΔS in kJ for unit match)
    omega = None
    if tm_bar and abs(dH_mix) > 1e-9:
        omega = (tm_bar * (dS_mix / 1000.0)) / abs(dH_mix)  # ΔS→kJ/(mol·K)

    # Phase prediction (Yang 2012 + Guo/Liu 2011):
    #  Solid solution likely when Ω ≥ 1.1 AND δ ≤ 6.6%
    #  VEC ≥ 8.0 → FCC; VEC < 6.87 → BCC; 6.87 ≤ VEC < 8.0 → FCC+BCC mixed
    phase = "intermetallic_or_segregated"
    criterion_notes = []
    if omega is not None and delta is not None:
        if omega >= 1.1 and delta <= 6.6:
            phase = "solid_solution"
            criterion_notes.append(
                f"Yang criterion MET: Ω={omega:.2f} ≥ 1.1 and δ={delta:.2f}% ≤ 6.6%"
            )
        else:
            criterion_notes.append(
                f"Yang criterion NOT met: Ω={omega:.2f} (need ≥1.1), δ={delta:.2f}% (need ≤6.6%)"
            )

    # Positive ΔH_mix = net demixing tendency. Ω uses |ΔH_mix|, so the Yang
    # criterion alone can pass compositions that phase-separate — the textbook
    # case is Cu in 3d-TM alloys (CoCrFeNiCu: ΔH_mix = +3.2 kJ/mol, Cu-rich
    # second FCC phase via spinodal-like segregation). Flag it, never silently
    # return solid_solution.
    segregation_risk = False
    if phase == "solid_solution" and dH_mix > 0:
        segregation_risk = True
        phase = "solid_solution_segregation_risk"
        pos_pairs = sorted(
            (
                (elems[i], elems[j], _dh_mix_for_pair(elems[i], elems[j]))
                for i in range(n)
                for j in range(i + 1, n)
                if _dh_mix_for_pair(elems[i], elems[j]) > 0
            ),
            key=lambda p: -p[2],
        )
        pair_str = ", ".join(f"{a}-{b} +{v:g}" for a, b, v in pos_pairs[:3])
        criterion_notes.append(
            f"WARNING: ΔH_mix = +{dH_mix:.2f} kJ/mol > 0 (net repulsive; "
            f"most positive pairs: {pair_str}) — spinodal-like segregation "
            "risk (e.g. Cu-rich demixing in CoCrFeNiCu). Yang Ω uses |ΔH_mix| "
            "and cannot see this; treat single-phase prediction as NOT assured."
        )

    # Crystal structure hint from VEC (Guo & Liu 2011, Intermetallics 19:698 /
    # J. Appl. Phys. 109:103505: FCC stable at VEC ≥ 8.0, BCC stable at
    # VEC < 6.87, FCC+BCC duplex in between).
    if vec is not None:
        if vec < 6.87:
            criterion_notes.append(f"VEC={vec:.2f} < 6.87 → BCC favored (Guo & Liu 2011)")
        elif vec < 8.0:
            criterion_notes.append(
                f"VEC={vec:.2f} ∈ [6.87, 8.0) → BCC+FCC mixed (Guo & Liu 2011)"
            )
        else:
            criterion_notes.append(f"VEC={vec:.2f} ≥ 8.0 → FCC favored (Guo & Liu 2011)")

    return {
        "delta_H_mix_kJ_per_mol": round(dH_mix, 2),
        "delta_S_mix_J_per_molK": round(dS_mix, 2),
        "omega": round(omega, 3) if omega is not None else None,
        "VEC": round(vec, 3),
        "delta_radius_pct": round(delta, 3) if delta is not None else None,
        "delta_chi": round(dchi, 4) if dchi is not None else None,
        "Tm_estimate_K": round(tm_bar, 1) if tm_bar is not None else None,
        "phase_prediction": phase,
        "segregation_risk": segregation_risk,
        "criterion": "Yang (Ω, δ) + Guo/Liu (VEC) — empirical screening, not phase equilibria",
        "rationale": criterion_notes,
        "n_elements": n,
        "elements": elems,
        "fractions": [round(f, 4) for f in fracs],
    }


def create_hea_tools(registry: ToolRegistry) -> None:
    """Register the HEA / alloy-design tools."""
    registry.register(_hea_descriptors_tool())
    logger.info("Registered hea_descriptors tool")


_HEA_SCHEMA: dict = {
    "type": "object",
    "description": (
        "Compute the high-entropy-alloy (HEA) formability descriptors for a "
        "multi-principal-element composition and flag whether a single-phase "
        "solid solution is PLAUSIBLE, using the established empirical "
        "screening criteria (Yang Ω+δ, Guo & Liu 2011 VEC). This is a fast "
        "Hume-Rothery-style first-pass screen — NOT phase equilibria: no "
        "phase fractions, no temperature dependence, no Gibbs energies "
        "(that is what CALPHAD databases like Thermo-Calc TCHEA compute). "
        "Pure math from open literature parameters."
    ),
    "properties": {
        "composition": {
            "type": "string",
            "description": (
                "Atomic-fraction composition with every fraction explicit and "
                "summing to 1.0 ± 1e-6, e.g. "
                "'Co0.2Cr0.2Fe0.2Mn0.2Ni0.2' (the Cantor alloy). "
                "Ratios and percentages are rejected, never normalized."
            ),
        },
        "fractions": {
            "type": "object",
            "description": (
                "Alternative: pass element→fraction directly, e.g. "
                "{\"Co\":0.2,\"Cr\":0.2,\"Fe\":0.2,\"Mn\":0.2,\"Ni\":0.2}. "
                "Values must be finite, positive, and sum to 1.0 ± 1e-6."
            ),
        },
    },
    "additionalProperties": False,
}


def _hea_descriptors_tool() -> Tool:
    def _hea(**kwargs) -> dict:
        comp = kwargs.get("composition")
        fracs_dict = kwargs.get("fractions")
        if not comp and not fracs_dict:
            return {"error": "provide a composition (formula string or fractions dict)"}
        try:
            elems, fracs = _parse_composition_or_raise(fracs_dict if fracs_dict else comp)
            return compute_hea_descriptors(elems, fracs)
        except (TypeError, ValueError) as exc:
            return {"error": f"invalid composition: {exc}"}

    return Tool(
        name="hea_descriptors",
        description=(
            "Compute HEA formability descriptors (ΔH_mix, ΔS_mix, Ω, VEC, δ, Δχ) "
            "and flag solid-solution plausibility (Yang Ω+δ + Guo/Liu VEC "
            "empirical screening criteria). A first-pass screen — not a "
            "phase-equilibria (CALPHAD) calculation."
        ),
        input_schema=_HEA_SCHEMA,
        func=_hea,
        requires_approval=False,
        source="builtin",
        source_detail="materials.hea",
        examples=[
            {
                "input": {"composition": "Co0.2Cr0.2Fe0.2Mn0.2Ni0.2"},
                "output_note": "the Cantor alloy (CoCrFeMnNi) — solid_solution, VEC=8.0 → FCC, δ≈1.1%",
            },
            {
                "input": {"composition": "Nb0.25Mo0.25Ta0.25W0.25"},
                "output_note": "Senkov refractory HEA — solid_solution, VEC=5.5 → BCC",
            },
            {
                "input": {"composition": "Cr0.2Fe0.2Ni0.2Co0.2Cu0.2"},
                "output_note": "Cu-bearing (ΔH_mix=+3.2 kJ/mol) — flagged solid_solution_segregation_risk (Cu-rich demixing), NOT the Cantor alloy",
            },
        ],
    )
