"""Script/language detection and alloy-designation handling for non-Latin sources.

Two jobs, both of which exist to stop the pipeline lying about foreign text:

1. **Language provenance.** Cyrillic and CJK titles must never be recorded as
   English by omission. Every record carries `source_language` *and*
   `language_basis` — how we know. "undetermined" is a legal, honest answer;
   silently defaulting to "en" is not.

2. **Designation equivalence.** Soviet/Russian (ЖС6У, ВТ6) and Chinese (GH4169)
   grades map to Western grades only where a standard defines the composition on
   both sides — and even then the mapping is `nominal_composition`, NOT
   interchangeability. Everything else is an explicit REFUSAL with a reason.
   A wrong equivalence is worse than none, so the refusal table is the feature.
"""
import re
from typing import Dict, List, Optional, Tuple

# ---------------------------------------------------------------- script/language

_CYRILLIC = re.compile(r"[Ѐ-ӿ]")
_HAN = re.compile(r"[一-鿿]")
_KANA = re.compile(r"[぀-ヿ]")
_HANGUL = re.compile(r"[가-힯]")
_LATIN = re.compile(r"[A-Za-z]")


def detect_script(text: str) -> str:
    """Dominant script of `text`: cyrillic | han | japanese | hangul | latin | unknown.

    Kana anywhere implies Japanese even when Han characters outnumber it —
    kanji-heavy Japanese titles are the norm, and Han-only is the only safe
    signal for Chinese.
    """
    if not text:
        return "unknown"
    if _KANA.search(text):
        return "japanese"
    counts = {
        "cyrillic": len(_CYRILLIC.findall(text)),
        "han": len(_HAN.findall(text)),
        "hangul": len(_HANGUL.findall(text)),
        "latin": len(_LATIN.findall(text)),
    }
    top = max(counts, key=lambda k: counts[k])
    return top if counts[top] else "unknown"


# Script -> language is only safe for scripts with effectively one language in
# this corpus. Latin is deliberately absent: Latin script says nothing about
# language, and guessing "en" there is exactly the failure mode we refuse.
_SCRIPT_LANG = {"cyrillic": "ru", "japanese": "ja", "han": "zh", "hangul": "ko"}


# ISO 639-2 -> 639-1 for the languages in scope. A lossless code conversion,
# not an inference: sources disagree on which register they quote ("rus" from
# Internet Archive, "ru" from an OAI dc:language), and a field that is
# sometimes "ru" and sometimes "rus" cannot be joined on downstream.
_ISO_639_2_TO_1 = {"rus": "ru", "jpn": "ja", "chi": "zh", "zho": "zh",
                   "kor": "ko", "eng": "en", "ger": "de", "deu": "de",
                   "fre": "fr", "fra": "fr", "ukr": "uk"}


def normalise_language_code(code: str) -> str:
    """'rus' -> 'ru', 'ru-RU' -> 'ru'. Unknown codes pass through lowercased."""
    code = code.strip().lower().replace("_", "-")
    primary = code.split("-")[0]
    return _ISO_639_2_TO_1.get(primary, primary)


def infer_language(text: str, declared: Optional[str] = None) -> Tuple[Optional[str], str]:
    """Return (language, basis). `declared` is the source's own metadata value.

    basis is one of: "declared" | "script" | "undetermined".
    """
    if declared:
        return normalise_language_code(declared), "declared"
    lang = _SCRIPT_LANG.get(detect_script(text))
    return (lang, "script") if lang else (None, "undetermined")


# ---------------------------------------------------------------- designations

# Uppercase Latin letters that are visually identical to Cyrillic ones. Used
# only to build an ALTERNATE lookup key — never to rewrite stored text — so a
# user typing "BT6" on a Latin keyboard still finds Cyrillic "ВТ6" while
# genuinely-Latin grades like GH4169 keep matching on their raw form.
_LAT_TO_CYR = str.maketrans("ABCEHKMOPTXY", "АВСЕНКМОРТХУ")
_CYR_TO_LAT = str.maketrans("АВСЕНКМОРТХУ", "ABCEHKMOPTXY")

# A mapped designation asserts *nominal composition overlap between two named
# standards*, not drop-in substitution. Both standards are always cited.
DESIGNATION_EQUIVALENCES: Dict[str, Dict] = {
    "ВТ6": {
        "western": "Ti-6Al-4V",
        "equivalence_type": "nominal_composition",
        "standards": ["GOST 19807-91", "ASTM B348 Grade 5 (UNS R56400)"],
        "note": "Vanadium ranges differ (GOST 3.5-5.3 %, ASTM 3.5-4.5 %); "
                "not an interchangeability statement.",
    },
    "GH4169": {
        "western": "Alloy 718",
        "equivalence_type": "nominal_composition",
        "standards": ["GB/T 14992", "ASTM B637 (UNS N07718)"],
        "note": "GB/T 14992 defines the GH designation system; GH4169 is the "
                "wrought Ni-Fe-Cr-Nb-Mo age-hardening grade of the 718 type.",
    },
    "08Х18Н10Т": {
        "western": "AISI 321",
        "equivalence_type": "nominal_composition",
        "standards": ["GOST 5632-2014", "ASTM A240 Type 321 (UNS S32100)"],
        "note": "Nickel upper limit differs (GOST 11 %, ASTM 12 %).",
    },
}

# Grades we deliberately refuse to map. Each reason is the actual technical
# objection, so a reader can overturn it with evidence rather than guess.
REFUSED_EQUIVALENCES: Dict[str, str] = {
    "ЖС6У": "VIAM cast Ni-base superalloy (OST 1 90126 family). No Western "
            "grade shares its composition; commonly mis-equated with IN-100 "
            "or MAR-M200. Refused.",
    "ЖС32": "VIAM Re-bearing single-crystal alloy. No Western equivalent grade. "
            "Refused.",
    "ВЖЛ21": "VIAM cast alloy. No Western equivalent grade. Refused.",
    "ХН77ТЮР": "Frequently equated with Nimonic 80A in secondary literature, "
               "but the aluminium ranges differ materially and no standard "
               "asserts the equivalence. Refused.",
    "12Х18Н10Т": "Often equated with AISI 321, but the carbon ceiling differs "
                 "(0.12 % vs 0.08 %) — a different carbon class. Use "
                 "08Х18Н10Т for the 321 comparison. Refused.",
    "GH3536": "Commonly equated with Hastelloy X (UNS N06002), and GB/T "
              "14992-2005 does tabulate it, but the equivalence has not been "
              "checked against the primary standard text — only secondary "
              "trade aggregators. Promote only after that check. Refused.",
}

# One or more (Cyrillic-letters + digits) segments, with an optional leading
# 2-digit carbon prefix and an optional trailing letter suffix. The repeating
# group is load-bearing: GOST steel grades alternate (08 Х 18 Н 10 Т), so a
# single contiguous letter run cannot describe them. Hyphenated forms (ВТ-6)
# are the same grade written conventionally.
_CYRILLIC_GRADE = re.compile(r"\b\d{0,2}(?:[А-ЯЁ]{1,4}-?\d{1,3})+[А-ЯЁ]{0,4}\b")
# Chinese superalloy/titanium designation families (GB/T 14992, GB/T 3620.1).
# Explicit lookarounds rather than \b: Han and Kana are word characters to
# Python's `re`, so \b never fires inside unspaced CJK ("镍基合金GH4169的组织")
# and the pattern would only ever match Latin-spaced text.
_CHINESE_GRADE = re.compile(
    r"(?<![0-9A-Za-z])(?:GH|DZ|ZTC|TC|TA|TB)\d{1,4}(?![0-9A-Za-z])"
)

# Cyrillic-rendered chemical formulas are structurally identical to two-letter
# grades (СО2 looks exactly like ВТ6), so no regex can separate them. They are
# common in the OCR'd handbook text these collectors target, and while they
# would only ever resolve to "unknown" — never a fabricated equivalence — they
# are noise in the record. Denylisted explicitly rather than pretended away.
_NOT_GRADES = {"СО2", "СО", "Н2О", "Н2", "О2", "СН4", "NO2", "SO2", "CO2"}


def _lookup_keys(designation: str) -> List[str]:
    """Raw key first, then hyphen-stripped, then the homoglyph foldings.

    'ВТ-6' and 'ВТ6' are the same grade written two conventional ways, so the
    table stores one form and the lookup normalises to it.
    """
    raw = designation.strip().upper()
    keys = [raw]
    if "-" in raw:
        keys.append(raw.replace("-", ""))
    return keys + [k.translate(t) for k in list(keys)
                   for t in (_LAT_TO_CYR, _CYR_TO_LAT)]


def map_designation(designation: str) -> Dict:
    """Resolve one designation. Status is mapped | refused | unknown.

    `unknown` means "we found something that looks like a grade and have no
    entry for it" — it is never upgraded to a guess.
    """
    for key in _lookup_keys(designation):
        if key in DESIGNATION_EQUIVALENCES:
            return {"designation": designation, "status": "mapped",
                    **DESIGNATION_EQUIVALENCES[key]}
        if key in REFUSED_EQUIVALENCES:
            return {"designation": designation, "status": "refused",
                    "reason": REFUSED_EQUIVALENCES[key]}
    return {"designation": designation, "status": "unknown",
            "reason": "No standard-cited Western equivalent on file; "
                      "no equivalence asserted."}


def find_designations(text: str) -> List[Dict]:
    """Extract alloy designations from `text` and resolve each one."""
    if not text:
        return []
    found: List[str] = []
    for pattern in (_CYRILLIC_GRADE, _CHINESE_GRADE):
        for match in pattern.findall(text):
            if match not in found and match.upper() not in _NOT_GRADES:
                found.append(match)
    return [map_designation(d) for d in found]
