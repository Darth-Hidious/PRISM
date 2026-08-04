"""Pseudopotential resolution for Quantum ESPRESSO (pw.x).

Maps chemical elements to UPF files found in a local directory. This module
never downloads anything: it only inspects files already on disk. If an
element has no usable file, it raises with a message that lists exactly what
is missing and what was found — a silent default would point pw.x at the
wrong pseudopotential, which converges to a plausible-looking but wrong
energy.

Matching rules (deterministic, in priority order) for element ``E`` against
the ``*.upf`` files in the directory:

1. Exact stem match:      ``Si.upf``  (filename stem equals ``E``)
2. Case-sensitive prefix: ``Si.pbe-n-rrkjus_psl.1.0.0.UPF`` (name starts
   with ``E`` followed by ``.``, ``-`` or ``_``)
3. Case-insensitive prefix: ``si.oncv.upf``

Within a tier, the first filename in sorted order wins, so repeated calls on
the same directory are reproducible.
"""
from __future__ import annotations

from pathlib import Path
from typing import Dict, Iterable, List, Union

_SEPARATORS = (".", "-", "_")


class PseudopotentialNotFoundError(FileNotFoundError):
    """Raised when one or more elements have no UPF file in the directory."""


def _upf_files(directory: Path) -> List[Path]:
    return sorted(
        p for p in directory.iterdir()
        if p.is_file() and p.suffix.lower() == ".upf"
    )


def _match_element(element: str, files: List[Path]) -> Path | None:
    # Tier 1: exact stem match.
    for p in files:
        if p.stem == element:
            return p
    # Tier 2: case-sensitive prefix followed by a separator.
    for p in files:
        if p.name.startswith(element) and p.name[len(element):][:1] in _SEPARATORS:
            return p
    # Tier 3: case-insensitive prefix fallback.
    for p in files:
        if p.name.lower().startswith(element.lower()) and \
                p.name[len(element):][:1] in _SEPARATORS:
            return p
    return None


def resolve_pseudopotentials(
    elements: Iterable[str],
    directory: Union[str, Path],
) -> Dict[str, str]:
    """Map each element to a UPF filename (basename) inside ``directory``.

    Parameters
    ----------
    elements:
        Element symbols required by the structure, e.g. ``["Si", "O"]``.
    directory:
        Existing directory containing ``*.upf`` files.

    Returns
    -------
    dict
        ``{element: filename}`` — filenames only, not full paths. QE's
        ``&control / pseudo_dir`` carries the directory.

    Raises
    ------
    PseudopotentialNotFoundError
        If any element has no candidate file, or the directory does not
        exist / contains no UPF files. The message lists every missing
        element and every file that was found.
    """
    directory = Path(directory)
    if not directory.is_dir():
        raise PseudopotentialNotFoundError(
            f"Pseudopotential directory does not exist: {directory}"
        )

    files = _upf_files(directory)
    if not files:
        raise PseudopotentialNotFoundError(
            f"No .upf files found in {directory}. PRISM does not download "
            f"pseudopotentials; stage UPF files (e.g. from the QE website or "
            f"PSLibrary) in this directory first."
        )

    mapping: Dict[str, str] = {}
    missing: List[str] = []
    for element in elements:
        element = str(element).strip().capitalize()
        hit = _match_element(element, files)
        if hit is None:
            missing.append(element)
        else:
            mapping[element] = hit.name

    if missing:
        raise PseudopotentialNotFoundError(
            f"No pseudopotential found for element(s): {', '.join(missing)}. "
            f"UPF files present in {directory}: "
            f"{', '.join(p.name for p in files)}. "
            f"PRISM does not download pseudopotentials — add the missing "
            f"file(s) to this directory and retry."
        )
    return mapping
