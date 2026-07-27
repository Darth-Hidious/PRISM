"""Soviet/Russian, Chinese and Japanese literature — sources Western-indexed
materials platforms largely do not cover.

Reachable backends (official APIs / documented protocols only):

  cyberleninka       OAI-PMH harvest of Russian open-access journals, weighted
                     to VIAM/VILS aerospace-materials titles. NOTE: the site's
                     robots.txt disallows /api/, so the private search API is
                     off limits; /oai is not disallowed and OAI-PMH exists for
                     exactly this. Harvest is BOUNDED, so an empty result means
                     "not in the pages we scanned", never "not in the source" —
                     the status string says how much was scanned.
  ntrs_translations  NASA NTRS filtered to stiType=TECHNICAL_TRANSLATION — the
                     NASA TT F programme. Predominantly Soviet/Russian source
                     material but NOT exclusively (a sampled page also held
                     Japanese and French originals), and NTRS does not publish
                     the original language, so no per-record origin is claimed.
  jstage             J-STAGE public WebAPI. Japanese metallurgy (ISIJ, JIM),
                     bilingual ja/en metadata.
  internet_archive   archive.org advancedsearch — scanned Soviet handbooks and
                     GOST standards.

Gated backends are declared, not faked: they return a named error saying what
credential or licence is required. An unreachable source must never look like
an empty one.
"""
import hashlib
import time
import xml.etree.ElementTree as ET
from typing import Dict, List, Optional

import requests

try:  # pragma: no cover - depends on install profile
    # Hardens XML parsing against entity-expansion DoS when available.
    # NOT a declared dependency (it only arrives transitively via nbconvert),
    # so the stdlib parser stays the fallback rather than a hard import error.
    # Element *construction* below always uses stdlib ET, which is compatible.
    from defusedxml.ElementTree import fromstring as _xml_fromstring
except ImportError:  # pragma: no cover
    _xml_fromstring = ET.fromstring

from app.tools.data_collectors.alloy_designations import find_designations, infer_language
from app.tools.data_collectors.base_collector import DataCollector

# Sources that exist but cannot be collected without credentials or a licence.
# Reported verbatim so the owner sees the actual blocker and its price.
GATED_SOURCES: Dict[str, str] = {
    "elibrary": (
        "eLIBRARY.RU (Russian Science Citation Index): robots.txt disallows "
        "/querybox.asp and every *_items.asp listing — the search surface is "
        "off limits to crawlers — and full text needs a paid institutional "
        "subscription. Not collected. Licence is quoted per-organisation by "
        "Научная электронная библиотека; no public price list."
    ),
    "cnki": (
        "CNKI (中国知网): oversea.cnki.net/robots.txt read 'User-agent: * / "
        "Disallow: /' when checked 2026-07-27 (the host intermittently returns "
        "522 from outside CN), and www.cnki.net does not answer from here. "
        "Access is an institutional licence sold per database module; no "
        "public price list. Not collected."
    ),
    "wanfang": (
        "Wanfang Data (万方数据): no reachable robots.txt and search sits "
        "behind a login-gated SPA. Institutional licence required; no public "
        "price list. Not collected."
    ),
}


# Short function words are common enough in Russian technical prose that
# OR-matching on one turns the local filter into "match everything" — a query
# for "покрытия ДЛЯ лопаток" would return unrelated turbine papers while the
# status line still implied relevance.
_STOPWORDS = frozenset("""
для при как что его она они все или это над под из по на в и с к о а но не
the for and with from that this are was were has have of in on to at by
""".split())


def _crude_stem(token: str) -> str:
    """First 6 characters of a token.

    Deliberately NOT morphology: Russian inflects heavily ('сплав' / 'сплавов'
    / 'сплавах'), and truncation is enough to match stems during a local filter
    without pretending to be a lemmatiser.
    """
    return token[:6]


def _query_tokens(query: str) -> List[str]:
    """Stemmed, stopword-free tokens for the local relevance filter."""
    return [_crude_stem(t) for t in query.lower().split()
            if len(t) >= 3 and t not in _STOPWORDS]


def _matches(text: str, tokens: List[str]) -> bool:
    low = (text or "").lower()
    return any(t in low for t in tokens)


class EasternLiteratureCollector(DataCollector):
    name = "eastern_literature"

    CYBERLENINKA_OAI = "https://cyberleninka.ru/oai"
    NTRS_API = "https://ntrs.nasa.gov/api/citations/search"
    JSTAGE_API = "https://api.jstage.jst.go.jp/searchapi/do"
    ARCHIVE_API = "https://archive.org/advancedsearch.php"

    # Russian OA journals whose scope is aerospace/structural materials.
    # journal_29527 is VIAM's «Авиационные материалы и технологии» — the
    # ZhS-series superalloy and VT-titanium literature the West does not index.
    CYBERLENINKA_SETS = (
        "journal_29527",  # Авиационные материалы и технологии (VIAM)
        "journal_554",    # Технология легких сплавов (VILS)
        "journal_35668",  # Аддитивные технологии... Металлы, сплавы, композиты
        "journal_32493",  # Литьё и металлургия
        "journal_7938",   # Вестник ЮУрГУ. Металлургия
        "journal_31354",  # Вестник ПНИПУ. Машиностроение, материаловедение
    )

    OAI_NS = {
        "oai": "http://www.openarchives.org/OAI/2.0/",
        "dc": "http://purl.org/dc/elements/1.1/",
        "oai_dc": "http://www.openarchives.org/OAI/2.0/oai_dc/",
    }
    ATOM_NS = {"a": "http://www.w3.org/2005/Atom"}

    DEFAULT_SOURCES = ("cyberleninka", "ntrs_translations", "jstage", "internet_archive")
    PAGE_DELAY_S = 0.2  # politeness between OAI pages

    def collect(self, query: str = "", max_results: int = 20,
                sources: Optional[List[str]] = None, **kwargs) -> List[Dict]:
        return self.collect_with_status(query, max_results, sources)["results"]

    def collect_with_status(self, query: str = "", max_results: int = 20,
                            sources: Optional[List[str]] = None) -> Dict:
        """Per-source outcomes alongside results, so a gated or failed source is
        named rather than showing up as a thinner list."""
        if not query:
            return {"results": [], "source_status": {}}
        sources = list(sources or self.DEFAULT_SOURCES)
        handlers = {
            "cyberleninka": self._search_cyberleninka,
            "ntrs_translations": self._search_ntrs_translations,
            "jstage": self._search_jstage,
            "internet_archive": self._search_archive,
        }
        per_source: List[List[Dict]] = []
        status: Dict[str, str] = {}
        for src in sources:
            if src in GATED_SOURCES:
                status[src] = f"blocked: {GATED_SOURCES[src]}"
                continue
            handler = handlers.get(src)
            if handler is None:
                status[src] = f"error: unknown source {src!r}"
                continue
            try:
                hits, err = handler(query, max_results)
            except Exception as e:
                # One backend's bug must not discard the sources that already
                # succeeded, nor erase source_status — which is the only thing
                # telling the caller a source was skipped rather than empty.
                status[src] = f"error: {type(e).__name__}: {e}"
                continue
            per_source.append((src, hits))
            status[src] = err or f"ok ({len(hits)} results)"

        merged = self._interleave([h for _, h in per_source], max_results)
        # The per-handler count above is what the source RETURNED; the budget
        # may have trimmed it. Report both, or a status of "ok (20 results)"
        # sitting beside 15 kept records is a lie by omission.
        kept = {}
        for rec in merged:
            kept[rec.get("source")] = kept.get(rec.get("source"), 0) + 1
        for src, hits in per_source:
            if hits and kept.get(src, 0) != len(hits):
                status[src] += f"; {kept.get(src, 0)} kept after max_results trim"
        return {"results": merged, "source_status": status}

    @staticmethod
    def _interleave(per_source: List[List[Dict]], limit: int) -> List[Dict]:
        """Round-robin the per-source lists before applying `limit`.

        Concatenating and slicing would let whichever source ran first consume
        the whole budget, so a status line reading "jstage: ok (3 results)"
        could sit next to zero J-STAGE records — the status would be a lie.
        Round-robin keeps every reachable source represented.
        """
        merged: List[Dict] = []
        if limit <= 0:
            return merged
        for tier in range(max((len(s) for s in per_source), default=0)):
            for hits in per_source:
                if tier < len(hits):
                    merged.append(hits[tier])
                    if len(merged) >= limit:
                        return merged
        return merged

    # ------------------------------------------------------------- enrichment

    @staticmethod
    def _enrich(record: Dict, declared_language: Optional[str] = None) -> Dict:
        """Attach language provenance and resolved alloy designations."""
        title = record.get("title", "")
        lang, basis = infer_language(title, declared_language)
        record["source_language"] = lang
        record["language_basis"] = basis
        record["designations"] = find_designations(
            f"{title} {record.get('abstract', '')}"
        )
        return record

    # ------------------------------------------------------------- backends

    def _search_cyberleninka(self, query: str, max_results: int,
                             max_pages_per_set: int = 4):
        """Bounded OAI-PMH harvest, filtered locally.

        OAI-PMH has no keyword search, so we walk resumption tokens for each
        curated journal set and filter titles. The scan budget is reported in
        the status string because a bounded scan returning nothing is NOT
        evidence the source lacks the topic.
        """
        tokens = _query_tokens(query)
        if not tokens:
            return [], ("error: query has no searchable token "
                        "(3+ characters, not a stopword)")
        results: List[Dict] = []
        scanned = 0
        try:
            for set_spec in self.CYBERLENINKA_SETS:
                params = {"verb": "ListRecords", "metadataPrefix": "oai_dc",
                          "set": set_spec}
                for page in range(max_pages_per_set):
                    if len(results) >= max_results:
                        break
                    if page:
                        time.sleep(self.PAGE_DELAY_S)
                    resp = requests.get(self.CYBERLENINKA_OAI, params=params,
                                        timeout=30)
                    resp.raise_for_status()
                    root = _xml_fromstring(resp.content)
                    for rec in root.findall(".//oai:record", self.OAI_NS):
                        scanned += 1
                        parsed = self._parse_oai_record(rec, set_spec)
                        if parsed and _matches(parsed["title"], tokens):
                            results.append(parsed)
                    token_el = root.find(".//oai:resumptionToken", self.OAI_NS)
                    if token_el is None or not (token_el.text or "").strip():
                        break
                    params = {"verb": "ListRecords",
                              "resumptionToken": token_el.text.strip()}
                if len(results) >= max_results:
                    break
        except Exception as e:
            return results, f"error: {type(e).__name__}: {e} (scanned {scanned})"
        return results, (
            f"ok ({len(results)} results; scanned {scanned} records, harvest "
            f"bounded at {max_pages_per_set} pages/set — empty is not absence)"
        )

    def _parse_oai_record(self, rec: ET.Element, set_spec: str) -> Optional[Dict]:
        md = rec.find(".//oai_dc:dc", self.OAI_NS)
        if md is None:
            return None
        def texts(tag):
            return [e.text for e in md.findall(f"dc:{tag}", self.OAI_NS) if e.text]
        titles = texts("title")
        if not titles:
            return None
        ids = texts("identifier")
        langs = texts("language")
        # Never emit a blank source_id: normalize_records() dedups on it, so
        # two identifier-less records — from this source or any other — would
        # collapse into one and a real record would vanish without a word.
        # blake2s, not hash(): str hashing is salted per process, so hash()
        # would give the same article a different id on every run.
        source_id = ids[0] if ids else "cyberleninka:{}:{}".format(
            set_spec, hashlib.blake2s(titles[0].encode("utf-8")).hexdigest()[:12]
        )
        return self._enrich({
            "source": "cyberleninka",
            "source_id": source_id,
            "title": titles[0],
            "authors": texts("creator"),
            "abstract": (texts("description") or [""])[0],
            "year": (texts("date") or [None])[0],
            "url": ids[0] if ids else "",
            "journal_set": set_spec,
            "type": "paper",
        }, declared_language=langs[0] if langs else None)

    def _search_ntrs_translations(self, query: str, max_results: int):
        """NASA NTRS restricted to Technical Translations — the NASA TT F
        programme's Russian-to-English translation seam."""
        try:
            params = {"q": query, "stiType": "TECHNICAL_TRANSLATION",
                      "page.size": min(max_results, 100)}
            resp = requests.get(self.NTRS_API, params=params, timeout=30)
            resp.raise_for_status()
            data = resp.json()
        except Exception as e:
            return [], f"error: {type(e).__name__}: {e}"
        results = []
        # `or []` not a get() default: a "results": null envelope would sail
        # past the default and blow up the loop.
        for r in (data.get("results") or []):
            if not (r.get("title") or "").strip():
                continue
            authors = [
                (a.get("meta", {}).get("author", {}) or {}).get("name", "")
                for a in (r.get("authorAffiliations") or [])
            ]
            results.append(self._enrich({
                "source": "ntrs_translations",
                "source_id": f"ntrs:{r.get('id', '')}",
                "title": r.get("title", ""),
                "authors": [a for a in authors if a],
                "abstract": r.get("abstract") or "",
                "report_numbers": r.get("otherReportNumbers") or [],
                "url": f"https://ntrs.nasa.gov/citations/{r.get('id', '')}",
                # "en" is the language of the text NTRS actually serves, which
                # is what source_language means. The ORIGINAL language is not
                # in the NTRS metadata, so we do not claim it per record: the
                # series is predominantly Russian-origin but demonstrably also
                # carries Japanese and French translations.
                "full_text_available": r.get("disseminated") != "METADATA_ONLY",
                "translated_series": "NASA Technical Translation",
                "original_language": None,  # not exposed by the NTRS API
                "type": "paper",
            }, declared_language="en"))
        return results, None

    def _search_jstage(self, query: str, max_results: int):
        """J-STAGE public WebAPI (service=3, article search)."""
        try:
            params = {"service": 3, "text": query, "count": min(max_results, 100)}
            resp = requests.get(self.JSTAGE_API, params=params, timeout=40)
            resp.raise_for_status()
            root = _xml_fromstring(resp.content)
        except Exception as e:
            return [], f"error: {type(e).__name__}: {e}"
        # J-STAGE answers a rejected query (e.g. Cyrillic text, which it cannot
        # index) with status ERR_* AND a single empty <entry>. Parsing that
        # entry would turn a refusal into a phantom titleless record — an error
        # wearing a result's clothes. WARN_* codes accompany valid results.
        status_code = root.findtext(".//a:result/a:status", "", self.ATOM_NS)
        if status_code.startswith("ERR"):
            return [], f"error: J-STAGE rejected the query ({status_code})"
        results = []
        for entry in root.findall("a:entry", self.ATOM_NS):
            def sub(path):
                el = entry.find(path, self.ATOM_NS)
                return (el.text or "").strip() if el is not None and el.text else ""
            ja_title = sub("a:article_title/a:ja")
            en_title = sub("a:article_title/a:en")
            # Prefer the Japanese title as the record's own title: it is the
            # original, and demoting it to a translation loses the source text.
            title = ja_title or en_title
            if not title:
                continue  # a titleless record carries nothing; do not emit it
            # Per author, not per entry: choosing one language for the whole
            # entry drops a foreign co-author who only has an <en> name (or a
            # Japanese one who only has <ja>) with no sign anything was lost.
            authors = []
            for author in entry.findall("a:author", self.ATOM_NS):
                for path in ("a:ja/a:name", "a:en/a:name"):
                    el = author.find(path, self.ATOM_NS)
                    if el is not None and (el.text or "").strip():
                        authors.append(el.text.strip())
                        break
            results.append(self._enrich({
                "source": "jstage",
                "source_id": sub("a:doi") or sub("a:id"),
                "title": title,
                "title_en": en_title,
                "authors": [a for a in authors if a],
                "abstract": "",
                "year": sub("a:pubyear"),
                "journal": sub("a:material_title/a:ja") or sub("a:material_title/a:en"),
                "url": sub("a:article_link/a:ja") or sub("a:article_link/a:en"),
                "type": "paper",
            }))
        return results, None

    def _search_archive(self, query: str, max_results: int):
        """archive.org advancedsearch — scanned Soviet handbooks / GOST texts."""
        try:
            params = [("q", query), ("rows", min(max_results, 100)),
                      ("output", "json")]
            params += [("fl[]", f) for f in
                       ("identifier", "title", "year", "language", "creator")]
            resp = requests.get(self.ARCHIVE_API, params=params, timeout=40)
            resp.raise_for_status()
            docs = resp.json().get("response", {}).get("docs", [])
        except Exception as e:
            return [], f"error: {type(e).__name__}: {e}"
        results = []
        for d in docs:
            if not (d.get("title") or "").strip():
                continue  # untitled scan: nothing to identify or extract from
            creator = d.get("creator")
            declared = d.get("language")
            if isinstance(declared, list):
                declared = declared[0] if declared else None
            results.append(self._enrich({
                "source": "internet_archive",
                "source_id": f"archive:{d.get('identifier', '')}",
                "title": d.get("title", "") or "",
                "authors": creator if isinstance(creator, list) else ([creator] if creator else []),
                "abstract": "",
                "year": d.get("year"),
                "url": f"https://archive.org/details/{d.get('identifier', '')}",
                "type": "scan",
            }, declared_language=declared))
        return results, None

    def supported_params(self) -> List[str]:
        return ["query", "max_results", "sources"]
