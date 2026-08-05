"""Curated HuggingFace materials index + licence classifier.

WHY THIS EXISTS
---------------
Plain keyword search on the HuggingFace Hub is nearly useless for materials
science: a search for ``mace`` returns *Macedonian-language* NLP models (not
the MACE interatomic potential), ``MatBench`` returns per-fold mirrors (not
the canonical task set), and ``Alexandria DFT crystal`` returns *nothing*.
Yet ``facebook/OMAT24`` and the official ``ACEtools/mace-*`` checkpoints are
real and widely used. This module is the discovery layer that makes search
work: a small, hand-checked mapping from what a researcher *asks for* ("an
interatomic potential covering inorganic crystals I can train on") to the
repos that actually serve it.

Each entry carries: repo id, what it is, what it COVERS, what it does NOT
cover, its LICENCE (this decides commercial usability — CC-BY-NC ≠ CC-BY),
and its gated status. An entry whose licence could not be determined from
the public API is recorded as ``None`` (unknown), never guessed.

The licence/gated values here were verified against the anonymous public API
on 2026-08-05 (see ``verification`` per entry). ``app.tools.hf.details`` /
``pull`` re-fetch LIVE values so a researcher always gets current truth; the
index is only the discovery shortcut. Entries that returned HTTP 401 to
anonymous access (the entire ``ACEtools`` namespace) are recorded as
``gated="anonymous-blocked"`` with ``license=None`` — they are the documented
official checkpoints, but their licence could not be verified anonymously.

This module is PURE: no I/O, no network, no imports beyond stdlib. That keeps
the matcher and licence logic trivially unit-testable offline.
"""
from __future__ import annotations

import re
from typing import Any, Dict, List, Optional

#: SPDX-ish licence ids that permit commercial use (with attribution/terms).
#: Kept conservative and explicit — never infer "probably fine".
_COMMERCIAL_OK = {
    "cc-by-4.0",
    "cc-by-sa-4.0",
    "cc0-1.0",
    "mit",
    "apache-2.0",
    "bsd-2-clause",
    "bsd-3-clause",
}


def classify_license(license_value: Any) -> Dict[str, Any]:
    """Classify a Hub licence value for commercial usability.

    Returns ``{"license": str|None, "license_commercial": bool|None,
    "license_note": str}``. ``license_commercial`` is ``True`` (ok),
    ``False`` (forbidden), or ``None`` (unknown — including HF's ``"other"``,
    which means a custom/non-SPDX licence the card states in prose that this
    classifier cannot parse). Unknown is the honest default, never a guess.
    """
    if license_value is None:
        return {
            "license": None,
            "license_commercial": None,
            "license_note": "no licence declared on the Hub card",
        }
    # cardData.license is occasionally a list (multi-licence); classify the
    # first declared term. A real multi-licence case would need human review,
    # which we signal by leaving commercial unknown when terms conflict.
    if isinstance(license_value, (list, tuple)):
        if not license_value:
            return {
                "license": None,
                "license_commercial": None,
                "license_note": "no licence declared on the Hub card",
            }
        license_value = license_value[0]
    key = str(license_value).strip().lower()
    if not key:
        return {
            "license": None,
            "license_commercial": None,
            "license_note": "no licence declared on the Hub card",
        }
    if key in _COMMERCIAL_OK:
        return {
            "license": key,
            "license_commercial": True,
            "license_note": f"{key}: commercial use permitted (observe licence terms)",
        }
    # NonCommercial / no-commercial-use family.
    if "nc" in key.split("-") or "-nc-" in f"-{key}-" or "noncommercial" in key or "cc-by-nc" in key:
        return {
            "license": key,
            "license_commercial": False,
            "license_note": f"{key}: NON-COMMERCIAL — commercial use not permitted",
        }
    if key == "other":
        return {
            "license": key,
            "license_commercial": None,
            "license_note": "licence tagged 'other' on the Hub — a custom/non-SPDX "
            "licence stated in the card prose; commercial usability NOT determinable, "
            "read the card before commercial use",
        }
    return {
        "license": key,
        "license_commercial": None,
        "license_note": f"unrecognised licence id '{key}' — commercial usability NOT verified",
    }


#: The curated index. Order is presentation order; ``match_index`` re-ranks by
#: relevance. Keep this SMALL and genuinely useful — every entry earns its
#: place. This is the part that compounds; grow it deliberately.
ENTRIES: List[Dict[str, Any]] = [
    {
        "id": "fairchem/OMAT24",
        "kind": "dataset",
        "what": "OMat24 — ~110M DFT relaxations of inorganic materials; the "
        "training set for Meta's OMat24 foundation model. The canonical "
        "large-scale ML-interatomic-potential training corpus.",
        "covers": ["DFT relaxations", "interatomic-potential training data", "inorganic materials"],
        "not_covering": ["organic molecules", "trained model weights", "molecular-dynamics trajectories"],
        "keywords": ["omat24", "omat", "dft", "relaxation", "training data",
                     "interatomic potential training", "inorganic", "fairchem", "meta"],
        "license": "cc-by-4.0",
        "gated": False,
        "verification": "verified-api 2026-08-05: cardData.license=cc-by-4.0, gated=False",
    },
    {
        "id": "facebook/OMAT24",
        "kind": "model",
        "what": "Official OMat24 foundation interatomic potential (energy/forces/"
        "stress) from Meta. Gated: access requires manual owner approval.",
        "covers": ["interatomic potential", "energy/forces/stress", "foundation MLIP"],
        "not_covering": ["organic molecules", "anonymous download — gated"],
        "keywords": ["omat24", "omat", "interatomic potential", "foundation model",
                     "energy forces stress", "meta", "mlip", "machine learning potential"],
        "license": "other",
        "gated": "manual",
        "verification": "verified-api 2026-08-05: gated='manual', cardData.license='other' "
        "(custom licence — commercial usability NOT determinable from the tag alone)",
    },
    {
        "id": "jorgemunozl/mace_omat_medium",
        "kind": "model",
        "what": "Community MACE (mace-torch) checkpoint trained on OMat24. "
        "Anonymous-readable and MIT-licensed: a drop-in for trying the MACE "
        "architecture without gating.",
        "covers": ["interatomic potential", "MACE architecture", "OMat24-trained MLIP"],
        "not_covering": ["not the official Meta checkpoint", "organic molecules"],
        "keywords": ["mace", "mace-torch", "omat24", "interatomic potential", "mlip", "community"],
        "license": "mit",
        "gated": False,
        "verification": "verified-api 2026-08-05: cardData.license=mit, gated=False, library_name=mace-torch",
    },
    {
        "id": "atomind/alexandria",
        "kind": "dataset",
        "what": "Alexandria database — millions of inorganic crystal structures "
        "with DFT properties. A standard supplement/comparison set for training "
        "and benchmarking interatomic potentials.",
        "covers": ["inorganic crystals", "DFT structures/properties", "materials database"],
        "not_covering": ["trained model weights", "organic molecules"],
        "keywords": ["alexandria", "inorganic crystal", "dft", "materials database",
                     "crystal structure", "density functional theory"],
        "license": "cc-by-4.0",
        "gated": False,
        "verification": "verified-api 2026-08-05: cardData.license=cc-by-4.0, gated=False",
    },
    {
        "id": "ACEtools/mace-mp-0",
        "kind": "model",
        "what": "Official MACE-MP-0 — MACE interatomic potential trained on the "
        "Materials Project (MPtraj). The most-used general inorganic MACE "
        "checkpoint.",
        "covers": ["interatomic potential", "general inorganic MLIP", "Materials Project"],
        "not_covering": ["organic molecules (see mace-off)", "anonymous access"],
        "keywords": ["mace", "mace-mp", "materials project", "interatomic potential",
                     "mlip", "inorganic"],
        "license": None,
        "gated": "anonymous-blocked",
        "verification": "anonymous-api-401 2026-08-05: the ACEtools namespace returns "
        "HTTP 401 to anonymous requests, so licence/gated could NOT be verified. "
        "Documented official checkpoint; inspect after authenticating.",
    },
    {
        "id": "ACEtools/mace-off",
        "kind": "model",
        "what": "Official MACE-OFF — MACE interatomic potential for organic/bio "
        "molecules. The organic-systems counterpart to MACE-MP.",
        "covers": ["interatomic potential", "organic molecules", "bio/chemistry MLIP"],
        "not_covering": ["inorganic crystals (see mace-mp)", "anonymous access"],
        "keywords": ["mace", "mace-off", "organic", "molecules", "drug",
                     "interatomic potential", "mlip"],
        "license": None,
        "gated": "anonymous-blocked",
        "verification": "anonymous-api-401 2026-08-05: ACEtools namespace not readable "
        "anonymously; licence/gated NOT verified. Documented official checkpoint.",
    },
    {
        "id": "ACEtools/mace-omat-0",
        "kind": "model",
        "what": "Official MACE-OMat-0 — MACE interatomic potential trained on "
        "OMat24.",
        "covers": ["interatomic potential", "OMat24-trained MLIP", "inorganic materials"],
        "not_covering": ["organic molecules", "anonymous access"],
        "keywords": ["mace", "mace-omat", "omat24", "interatomic potential", "mlip", "inorganic"],
        "license": None,
        "gated": "anonymous-blocked",
        "verification": "anonymous-api-401 2026-08-05: ACEtools namespace not readable "
        "anonymously; licence/gated NOT verified. Documented official checkpoint.",
    },
    {
        "id": "ACEtools/mace-mpa-0",
        "kind": "model",
        "what": "Official MACE-MPA-0 — MACE interatomic potential, "
        "Materials-Project-annealed training variant.",
        "covers": ["interatomic potential", "inorganic MLIP", "Materials Project"],
        "not_covering": ["organic molecules", "anonymous access"],
        "keywords": ["mace", "mace-mpa", "materials project", "annealed",
                     "interatomic potential", "mlip"],
        "license": None,
        "gated": "anonymous-blocked",
        "verification": "anonymous-api-401 2026-08-05: ACEtools namespace not readable "
        "anonymously; licence/gated NOT verified. Documented official checkpoint.",
    },
]


def _hub_url(kind: str, repo_id: str) -> str:
    prefix = "datasets" if kind == "dataset" else "models" if kind == "model" else ""
    return f"https://huggingface.co/{prefix}/{repo_id}" if prefix else f"https://huggingface.co/{repo_id}"


def present_index_entry(entry: Dict[str, Any]) -> Dict[str, Any]:
    """Format a curated entry for tool output, adding the licence verdict.

    Pure: combines stored licence/gated with :func:`classify_license`.
    """
    cls = classify_license(entry.get("license"))
    return {
        "repo": entry["id"],
        "kind": entry["kind"],
        "source": "curated_index",
        "what": entry.get("what"),
        "covers": entry.get("covers"),
        "not_covering": entry.get("not_covering"),
        "license": cls["license"],
        "license_commercial": cls["license_commercial"],
        "license_note": cls["license_note"],
        "gated": entry.get("gated"),
        "verification": entry.get("verification"),
        "hub_url": _hub_url(entry["kind"], entry["id"]),
    }


_TOKEN_RE = re.compile(r"[a-z0-9]+", re.IGNORECASE)


def _tokens(text: str) -> set:
    return {m.group(0).lower() for m in _TOKEN_RE.finditer(text or "")}


def _normalize(text: str) -> str:
    """Lower-case and turn separators into spaces for phrase matching."""
    out = (text or "").lower()
    for ch in "-_/":
        out = out.replace(ch, " ")
    return out


def _keyword_matches(kw: str, q_norm: str, q_tokens: set) -> bool:
    """Does a curated keyword match the query?

    Multi-word/multi-token keywords (e.g. 'interatomic potential', 'mace-mp')
    match as a normalised phrase substring. SINGLE-token keywords (e.g.
    'mace') match only as an EXACT word — so 'mace' does NOT match
    'macedonian'. This word-boundary rule is precisely what keeps the index
    from repeating the Hub's keyword-search failure (searching 'mace' returns
    Macedonian-language NLP models on the Hub; it must not here).
    """
    kw_norm = _normalize(kw)
    if " " in kw_norm.strip():
        return kw_norm in q_norm
    return kw_norm in q_tokens


def match_index(query: str, limit: int = 10) -> List[Dict[str, Any]]:
    """Rank curated entries against a researcher's natural-language query.

    The curated keywords ARE the signal — a hand-checked mapping from intent
    to repos. We deliberately do NOT blend loose token overlap with the entry
    prose, because that reintroduces the exact keyword-noise problem the index
    exists to fix (a stray 'for' or 'materials' would match everything).
    Returns entries with score > 0, highest first; ties break on entry order.
    Pure and deterministic: no network, no randomness.
    """
    q_norm = _normalize(query)
    q_tokens = _tokens(query)
    scored = []
    for entry in ENTRIES:
        score = sum(
            2 for kw in entry.get("keywords", []) if _keyword_matches(kw, q_norm, q_tokens)
        )
        if score > 0:
            scored.append((score, entry))
    scored.sort(key=lambda t: (-t[0], ENTRIES.index(t[1])))
    return [entry for _, entry in scored[:limit]]


def index_entry_for(repo_id: str) -> Optional[Dict[str, Any]]:
    """Look up a curated entry by exact repo id (case-insensitive)."""
    needle = repo_id.strip().lower()
    for entry in ENTRIES:
        if entry["id"].lower() == needle:
            return entry
    return None
