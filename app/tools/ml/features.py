"""Feature engineering for materials property prediction.

Two backends:
  1. matminer (preferred) — 132 Magpie features via ElementProperty
  2. Built-in fallback — 22 features from hardcoded element data (44 elements)

matminer is used automatically when installed; otherwise falls back silently.
"""
from typing import Dict


def _check_matminer_available() -> bool:
    try:
        from matminer.featurizers.composition import ElementProperty  # noqa: F401
        return True
    except ImportError:
        return False


# ---------------------------------------------------------------------------
# Backend 1: matminer (132 Magpie features)
# ---------------------------------------------------------------------------

def _composition_features_matminer(formula: str) -> Dict[str, float]:
    """Generate 132 Magpie features via matminer + pymatgen."""
    from pymatgen.core import Composition
    from matminer.featurizers.composition import ElementProperty

    try:
        comp = Composition(formula)
    except Exception:
        return {}

    featurizer = ElementProperty.from_preset("magpie")
    try:
        values = featurizer.featurize(comp)
        labels = featurizer.feature_labels()
        features = dict(zip(labels, values))
        features["n_elements"] = len(comp.elements)
        features["total_atoms_in_formula"] = comp.num_atoms
        return features
    except Exception:
        return {}


# ---------------------------------------------------------------------------
# Backend 2: Built-in fallback (22 features, 44 elements)
# ---------------------------------------------------------------------------

ELEMENT_DATA = {
    "H": {"atomic_mass": 1.008, "atomic_number": 1, "electronegativity": 2.20, "atomic_radius": 25},
    "Li": {"atomic_mass": 6.941, "atomic_number": 3, "electronegativity": 0.98, "atomic_radius": 145},
    "Be": {"atomic_mass": 9.012, "atomic_number": 4, "electronegativity": 1.57, "atomic_radius": 105},
    "B": {"atomic_mass": 10.81, "atomic_number": 5, "electronegativity": 2.04, "atomic_radius": 85},
    "C": {"atomic_mass": 12.01, "atomic_number": 6, "electronegativity": 2.55, "atomic_radius": 70},
    "N": {"atomic_mass": 14.01, "atomic_number": 7, "electronegativity": 3.04, "atomic_radius": 65},
    "O": {"atomic_mass": 16.00, "atomic_number": 8, "electronegativity": 3.44, "atomic_radius": 60},
    "F": {"atomic_mass": 19.00, "atomic_number": 9, "electronegativity": 3.98, "atomic_radius": 50},
    "Na": {"atomic_mass": 22.99, "atomic_number": 11, "electronegativity": 0.93, "atomic_radius": 180},
    "Mg": {"atomic_mass": 24.31, "atomic_number": 12, "electronegativity": 1.31, "atomic_radius": 150},
    "Al": {"atomic_mass": 26.98, "atomic_number": 13, "electronegativity": 1.61, "atomic_radius": 125},
    "Si": {"atomic_mass": 28.09, "atomic_number": 14, "electronegativity": 1.90, "atomic_radius": 110},
    "P": {"atomic_mass": 30.97, "atomic_number": 15, "electronegativity": 2.19, "atomic_radius": 100},
    "S": {"atomic_mass": 32.07, "atomic_number": 16, "electronegativity": 2.58, "atomic_radius": 100},
    "Cl": {"atomic_mass": 35.45, "atomic_number": 17, "electronegativity": 3.16, "atomic_radius": 100},
    "K": {"atomic_mass": 39.10, "atomic_number": 19, "electronegativity": 0.82, "atomic_radius": 220},
    "Ca": {"atomic_mass": 40.08, "atomic_number": 20, "electronegativity": 1.00, "atomic_radius": 180},
    "Ti": {"atomic_mass": 47.87, "atomic_number": 22, "electronegativity": 1.54, "atomic_radius": 140},
    "V": {"atomic_mass": 50.94, "atomic_number": 23, "electronegativity": 1.63, "atomic_radius": 135},
    "Cr": {"atomic_mass": 52.00, "atomic_number": 24, "electronegativity": 1.66, "atomic_radius": 140},
    "Mn": {"atomic_mass": 54.94, "atomic_number": 25, "electronegativity": 1.55, "atomic_radius": 140},
    "Fe": {"atomic_mass": 55.85, "atomic_number": 26, "electronegativity": 1.83, "atomic_radius": 140},
    "Co": {"atomic_mass": 58.93, "atomic_number": 27, "electronegativity": 1.88, "atomic_radius": 135},
    "Ni": {"atomic_mass": 58.69, "atomic_number": 28, "electronegativity": 1.91, "atomic_radius": 135},
    "Cu": {"atomic_mass": 63.55, "atomic_number": 29, "electronegativity": 1.90, "atomic_radius": 135},
    "Zn": {"atomic_mass": 65.38, "atomic_number": 30, "electronegativity": 1.65, "atomic_radius": 135},
    "Ga": {"atomic_mass": 69.72, "atomic_number": 31, "electronegativity": 1.81, "atomic_radius": 130},
    "Ge": {"atomic_mass": 72.63, "atomic_number": 32, "electronegativity": 2.01, "atomic_radius": 125},
    "As": {"atomic_mass": 74.92, "atomic_number": 33, "electronegativity": 2.18, "atomic_radius": 115},
    "Se": {"atomic_mass": 78.96, "atomic_number": 34, "electronegativity": 2.55, "atomic_radius": 115},
    "Sr": {"atomic_mass": 87.62, "atomic_number": 38, "electronegativity": 0.95, "atomic_radius": 200},
    "Zr": {"atomic_mass": 91.22, "atomic_number": 40, "electronegativity": 1.33, "atomic_radius": 155},
    "Nb": {"atomic_mass": 92.91, "atomic_number": 41, "electronegativity": 1.60, "atomic_radius": 145},
    "Mo": {"atomic_mass": 95.96, "atomic_number": 42, "electronegativity": 2.16, "atomic_radius": 145},
    "Hf": {"atomic_mass": 178.49, "atomic_number": 72, "electronegativity": 1.30, "atomic_radius": 155},
    "Ta": {"atomic_mass": 180.95, "atomic_number": 73, "electronegativity": 1.50, "atomic_radius": 145},
    "W": {"atomic_mass": 183.84, "atomic_number": 74, "electronegativity": 2.36, "atomic_radius": 135},
    "Re": {"atomic_mass": 186.21, "atomic_number": 75, "electronegativity": 1.90, "atomic_radius": 135},
    "Sn": {"atomic_mass": 118.71, "atomic_number": 50, "electronegativity": 1.96, "atomic_radius": 145},
    "Ba": {"atomic_mass": 137.33, "atomic_number": 56, "electronegativity": 0.89, "atomic_radius": 215},
    "Pt": {"atomic_mass": 195.08, "atomic_number": 78, "electronegativity": 2.28, "atomic_radius": 135},
    "Au": {"atomic_mass": 196.97, "atomic_number": 79, "electronegativity": 2.54, "atomic_radius": 135},
    "Pb": {"atomic_mass": 207.2, "atomic_number": 82, "electronegativity": 2.33, "atomic_radius": 180},
}


def _parse_formula(formula: str) -> Dict[str, float]:
    """Parse a simple chemical formula into element:count dict."""
    import re
    pattern = r'([A-Z][a-z]?)(\d*\.?\d*)'
    matches = re.findall(pattern, formula)
    composition = {}
    for elem, count in matches:
        if elem:
            composition[elem] = float(count) if count else 1.0
    return composition


def _composition_features_basic(formula: str) -> Dict[str, float]:
    """Generate 22 composition features from hardcoded element data."""
    comp = _parse_formula(formula)
    if not comp:
        return {}

    total_atoms = sum(comp.values())
    fractions = {elem: count / total_atoms for elem, count in comp.items()}

    features = {}
    features["n_elements"] = len(comp)
    features["total_atoms_in_formula"] = total_atoms

    for prop_name in ["atomic_mass", "atomic_number", "electronegativity", "atomic_radius"]:
        values = []
        weights = []
        for elem, frac in fractions.items():
            if elem in ELEMENT_DATA and prop_name in ELEMENT_DATA[elem]:
                values.append(ELEMENT_DATA[elem][prop_name])
                weights.append(frac)

        if not values:
            continue

        import statistics
        weighted_avg = sum(v * w for v, w in zip(values, weights))
        features[f"avg_{prop_name}"] = weighted_avg
        features[f"min_{prop_name}"] = min(values)
        features[f"max_{prop_name}"] = max(values)
        features[f"range_{prop_name}"] = max(values) - min(values)
        if len(values) > 1:
            features[f"std_{prop_name}"] = statistics.stdev(values)
        else:
            features[f"std_{prop_name}"] = 0.0

    return features


# ---------------------------------------------------------------------------
# Public API — auto-selects best available backend
# ---------------------------------------------------------------------------

_USE_MATMINER = _check_matminer_available()


def composition_features(formula: str) -> Dict[str, float]:
    """Generate composition-based features from a chemical formula.

    Uses matminer (132 Magpie features) if installed, otherwise falls back
    to the built-in 22-feature set.
    """
    if _USE_MATMINER:
        result = _composition_features_matminer(formula)
        if result:
            return result
    return _composition_features_basic(formula)


def get_feature_backend() -> str:
    """Return which feature backend is active."""
    return "matminer" if _USE_MATMINER else "basic"
