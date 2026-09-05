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
  internet_archive   archive.org advancedsearch, texts only — scanned Soviet
                     handbooks, GOST standards, Chinese technical scans.
  openalex           OpenAlex works filtered by language (zh / ru / ja): the
                     reachable index of Chinese, Russian and Japanese journal
                     literature — native titles, DOIs, declared language.
                     Keyless. The only Chinese-language path that answers.

Queries are ROUTED BY LANGUAGE. The caller (the model) supplies translations
in `queries` ({"ru": …, "zh": …, "ja": …}); each source receives only a
language it indexes — CyberLeninka's titles are Russian, J-STAGE rejects
Cyrillic, NTRS translations are English. A source with no usable query is
SKIPPED and says which language it needs, instead of scanning for seven
seconds and matching nothing. Sources run concurrently under one deadline; a
late source is named as late, never waited on forever.

Gated backends are declared, not faked: they return a named error saying what
credential or licence is required. An unreachable source must never look like
an empty one.
"""
import concurrent.futures as cf
import hashlib
import json
import os
import time
import xml.etree.ElementTree as ET
from functools import partial
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import requests

try:  # pragma: no cover - depends on install profile
    # Hardens XML parsing against entity-expansion DoS when available.
    # NOT a declared dependency (it only arrives transitively via nbconvert),
    # so the stdlib parser stays the fallback rather than a hard import error.
    # Element *construction* below always uses stdlib ET, which is compatible.
    from defusedxml.ElementTree import fromstring as _xml_fromstring
except ImportError:  # pragma: no cover
    _xml_fromstring = ET.fromstring

from app.tools.data_collectors.alloy_designations import (
    detect_script,
    find_designations,
    infer_language,
)
from app.tools.data_collectors.base_collector import DataCollector

# Sources that exist but cannot be collected without credentials or a licence.
# Reported verbatim so the owner sees the actual blocker and its price.
GATED_SOURCES: Dict[str, Dict[str, str]] = {
    "elibrary": {
        "reason": (
            "eLIBRARY.RU (Russian Science Citation Index): robots.txt disallows "
            "/querybox.asp and every *_items.asp listing — the search surface is "
            "off limits to crawlers — and full text needs a paid institutional "
            "subscription. Not collected. Licence is quoted per-organisation by "
            "Научная электронная библиотека; no public price list."
        ),
        "what": "an organisation subscription to eLIBRARY.RU (RSCI); quoted per organisation",
        "url": "https://elibrary.ru/",
    },
    "cnki": {
        "reason": (
            "CNKI (中国知网): oversea.cnki.net/robots.txt read 'User-agent: * / "
            "Disallow: /' when checked 2026-07-27 (the host intermittently returns "
            "522 from outside CN), and www.cnki.net does not answer from here. "
            "Access is an institutional licence sold per database module; no "
            "public price list. Not collected."
        ),
        "what": "an institutional CNKI licence (sold per database module)",
        "url": "https://oversea.cnki.net/",
    },
    "wanfang": {
        "reason": (
            "Wanfang Data (万方数据): no reachable robots.txt and search sits "
            "behind a login-gated SPA. Institutional licence required; no public "
            "price list. Not collected."
        ),
        "what": "an institutional Wanfang Data licence",
        "url": "https://www.wanfangdata.com.cn/",
    },
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


_SCRIPT_TO_LANG = {"cyrillic": "ru", "han": "zh", "japanese": "ja",
                   "hangul": "ko", "latin": "en"}
_LANGUAGE_NAMES = {"ru": "Russian", "zh": "Chinese", "ja": "Japanese",
                   "ko": "Korean", "en": "English"}


def _query_tokens(query: str) -> List[str]:
    """Stemmed, stopword-free tokens for the local relevance filter.

    CJK terms are whitespace chunks kept whole: "涂层" (coating) is two
    characters, and the Latin/Cyrillic 3-character floor dropped it — so a
    Chinese query had no tokens and matched nothing.
    """
    if detect_script(query) in ("han", "japanese", "hangul"):
        return [t for t in query.split() if t and t not in _STOPWORDS]
    return [_crude_stem(t) for t in query.lower().split()
            if len(t) >= 3 and t not in _STOPWORDS]


def _matches(text: str, tokens: List[str]) -> bool:
    low = (text or "").lower()
    return any(t in low for t in tokens)


class EasternLiteratureCollector(DataCollector):
    name = "eastern_literature"

    CYBERLENINKA_OAI = "https://cyberleninka.ru/oai"
    OPENALEX_API = "https://api.openalex.org/works"
    OPENALEX_LANGUAGES = ("zh", "ru", "ja")
    # Which languages each backend can actually be asked in. `None` = any.
    SOURCE_LANGUAGES: Dict[str, Optional[Tuple[str, ...]]] = {
        "cyberleninka": ("ru",),
        "ntrs_translations": ("en",),
        "jstage": ("ja", "en"),
        "internet_archive": None,
    }
    DEFAULT_DEADLINE_S = 45.0
    CACHE_TTL_S = 24 * 3600
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

    DEFAULT_SOURCES = ("openalex", "cyberleninka", "ntrs_translations", "jstage",
                       "internet_archive")
    PAGE_DELAY_S = 0.2  # politeness between OAI pages

    def supported_params(self) -> List[str]:
        return ["query", "max_results", "sources", "queries"]

    def collect(self, query: str = "", max_results: int = 20,
                sources: Optional[List[str]] = None, **kwargs) -> List[Dict]:
        return self.collect_with_status(
            query, max_results, sources, queries=kwargs.get("queries"))["results"]

    @staticmethod
    def _queries_by_language(query: str, queries: Optional[Dict[str, str]]) -> Dict[str, str]:
        """The query in every language we have it: the base query under the
        language its script says it is, plus the caller's translations (blank
        ones dropped)."""
        langs: Dict[str, str] = {}
        base = (query or "").strip()
        if base:
            langs[_SCRIPT_TO_LANG.get(detect_script(base), "en")] = base
        for lang, text in (queries or {}).items():
            text = (text or "").strip()
            if text:
                langs[str(lang).lower()] = text
        return langs

    def collect_with_status(self, query: str = "", max_results: int = 20,
                            sources: Optional[List[str]] = None,
                            queries: Optional[Dict[str, str]] = None,
                            deadline_s: Optional[float] = None) -> Dict:
        """Per-source outcomes alongside results, so a gated, skipped, late or
        failed source is named rather than showing up as a thinner list."""
        if not query:
            return {"results": [], "source_status": {}, "needs_human": []}
        sources = list(sources or self.DEFAULT_SOURCES)
        langs = self._queries_by_language(query, queries)
        # What the agent cannot obtain itself — a licence, an account — with
        # the page where a human gets it. Announced to the human, not buried
        # in a status string.
        needs_human: List[Dict[str, str]] = []
        deadline = float(deadline_s or os.environ.get("PRISM_EASTERN_DEADLINE_S")
                         or self.DEFAULT_DEADLINE_S)
        handlers = {
            "cyberleninka": self._search_cyberleninka,
            "ntrs_translations": self._search_ntrs_translations,
            "jstage": self._search_jstage,
            "internet_archive": self._search_archive,
        }
        status: Dict[str, str] = {}
        jobs: List[Tuple[str, object]] = []  # (status key, zero-arg callable), in order
        for src in sources:
            if src in GATED_SOURCES:
                gate = GATED_SOURCES[src]
                status[src] = f"blocked: {gate['reason']}"
                needs_human.append({"source": src, "what": gate["what"],
                                    "url": gate["url"], "reason": gate["reason"]})
                continue
            if src == "openalex":
                for lang in self.OPENALEX_LANGUAGES:
                    native = lang in langs
                    q = langs[lang] if native else (langs.get("en") or query)
                    jobs.append((f"openalex:{lang}",
                                 partial(self._search_openalex, q, max_results, lang,
                                         native_query=native)))
                continue
            handler = handlers.get(src)
            if handler is None:
                status[src] = f"error: unknown source {src!r}"
                continue
            allowed = self.SOURCE_LANGUAGES.get(src)
            if allowed is None:
                jobs.append((src, partial(handler, query, max_results)))
                continue
            q = next((langs[lang] for lang in allowed if lang in langs), None)
            if q is None:
                names = " or ".join(_LANGUAGE_NAMES.get(l, l) for l in allowed)
                status[src] = (
                    f"skipped: needs a {names} query — pass queries={{'{allowed[0]}': …}} "
                    f"(got {sorted(langs) or 'nothing'}); not searched"
                )
                continue
            jobs.append((src, partial(handler, q, max_results)))

        # Concurrent, under one deadline. A source still running at the
        # deadline is named as late and abandoned (its own HTTP timeouts end
        # the thread); it is never allowed to hold the others hostage.
        per_source: List[Tuple[str, List[Dict]]] = []
        if jobs:
            pool = cf.ThreadPoolExecutor(max_workers=len(jobs))
            futures = {pool.submit(fn): key for key, fn in jobs}
            done, pending = cf.wait(futures, timeout=deadline)
            for fut in pending:
                status[futures[fut]] = f"timeout: no answer within {deadline:g}s"
            pool.shutdown(wait=False, cancel_futures=True)
            outcomes: Dict[str, Tuple[List[Dict], Optional[str]]] = {}
            for fut in done:
                key = futures[fut]
                try:
                    outcomes[key] = fut.result()
                except Exception as e:
                    # One backend's bug must not discard the sources that
                    # already succeeded, nor erase source_status.
                    status[key] = f"error: {type(e).__name__}: {e}"
            for key, _ in jobs:
                if key in outcomes:
                    hits, err = outcomes[key]
                    per_source.append((key, hits))
                    status[key] = err or f"ok ({len(hits)} results)"

        merged = self._interleave([h for _, h in per_source], max_results)
        # The per-handler count above is what the source RETURNED; the budget
        # may have trimmed it. Report both, or a status of "ok (20 results)"
        # sitting beside 15 kept records is a lie by omission.
        kept: Dict[str, int] = {}
        for rec in merged:
            kept[rec.get("source")] = kept.get(rec.get("source"), 0) + 1
        for key, hits in per_source:
            src = key.split(":", 1)[0]
            if hits and kept.get(src, 0) != len(hits) and not key.startswith("openalex:"):
                status[key] += f"; {kept.get(src, 0)} kept after max_results trim"
        return {"results": merged, "source_status": status, "needs_human": needs_human}

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

    def _search_openalex(self, query: str, max_results: int, language: str,
                         native_query: bool = True):
        """OpenAlex works filtered to one language — the reachable index of
        Chinese, Russian and Japanese journal articles, with native titles and
        the language DECLARED by the record. Keyless; a contact address is
        sent only if the operator set PRISM_CONTACT_EMAIL (never invented)."""
        params = {
            "search": query,
            "filter": f"language:{language}",
            "per-page": max(1, min(int(max_results), 50)),
            "select": "id,display_name,publication_year,language,doi,authorships,"
                      "primary_location",
        }
        mailto = (os.environ.get("PRISM_CONTACT_EMAIL") or "").strip()
        if mailto:
            params["mailto"] = mailto
        try:
            resp = requests.get(self.OPENALEX_API, params=params, timeout=30)
            resp.raise_for_status()
            data = resp.json()
        except Exception as e:
            return [], f"error: {type(e).__name__}: {e} (language:{language})"
        total = (data.get("meta") or {}).get("count")
        results: List[Dict] = []
        for w in (data.get("results") or []):
            title = (w.get("display_name") or "").strip()
            if not title:
                continue
            doi = (w.get("doi") or "").replace("https://doi.org/", "")
            authors = [((a.get("author") or {}).get("display_name") or "")
                       for a in (w.get("authorships") or [])]
            journal = (((w.get("primary_location") or {}).get("source") or {})
                       .get("display_name")) or ""
            results.append(self._enrich({
                "source": "openalex",
                "source_id": doi or w.get("id", ""),
                "title": title,
                "authors": [a for a in authors if a],
                "abstract": "",
                "year": w.get("publication_year"),
                "doi": doi,
                "journal": journal,
                "url": w.get("doi") or w.get("id") or "",
                "language_filter": language,
                "type": "paper",
            }, declared_language=w.get("language") or language))
        hint = "" if native_query else (
            f" — pass queries={{'{language}': …}} for a native-title match")
        return results, (
            f"ok ({len(results)} of {total if total is not None else '?'}; "
            f"language:{language}; query in {language if native_query else 'en'}{hint})"
        )

    # ------------------------------------------------------ harvest cache

    @staticmethod
    def _cache_dir() -> Path:
        return Path(os.environ.get("PRISM_EASTERN_CACHE_DIR")
                    or Path.home() / ".prism" / "cache" / "eastern")

    def _read_cache(self, set_spec: str) -> Optional[Dict]:
        path = self._cache_dir() / "cyberleninka" / f"{set_spec}.json"
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None
        ttl = float(os.environ.get("PRISM_EASTERN_CACHE_TTL_S") or self.CACHE_TTL_S)
        if time.time() - float(data.get("fetched_epoch", 0)) > ttl:
            return None
        return data

    def _write_cache(self, set_spec: str, records: List[Dict], scanned: int) -> None:
        path = self._cache_dir() / "cyberleninka" / f"{set_spec}.json"
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps({
                "fetched_epoch": time.time(), "scanned": scanned, "records": records,
            }, ensure_ascii=False), encoding="utf-8")
        except (OSError, TypeError, ValueError):
            pass  # a cache that cannot be written only costs the next harvest

    @staticmethod
    def _get_with_retry(url: str, params: Dict, timeout: int):
        """One retry after a short pause: CyberLeninka answers 503 under load
        (measured 2026-09-05), and a single 503 ended the whole harvest."""
        try:
            resp = requests.get(url, params=params, timeout=timeout)
            resp.raise_for_status()
            return resp
        except Exception:
            time.sleep(0.5)
            resp = requests.get(url, params=params, timeout=timeout)
            resp.raise_for_status()
            return resp

    def _harvest_set(self, set_spec: str, max_pages: int) -> Tuple[List[Dict], int, bool]:
        """All parsed records of one journal set, from the cache when fresh.

        OAI-PMH has no keyword search, so the harvest is the same for every
        query; only the local filter differs. Caching it turns a repeat
        search from seconds of network into milliseconds."""
        cached = self._read_cache(set_spec)
        if cached is not None:
            return cached.get("records") or [], int(cached.get("scanned") or 0), True
        params = {"verb": "ListRecords", "metadataPrefix": "oai_dc", "set": set_spec}
        records: List[Dict] = []
        scanned = 0
        for page in range(max_pages):
            if page:
                time.sleep(self.PAGE_DELAY_S)
            resp = self._get_with_retry(self.CYBERLENINKA_OAI, params, 30)
            root = _xml_fromstring(resp.content)
            for rec in root.findall(".//oai:record", self.OAI_NS):
                scanned += 1
                parsed = self._parse_oai_record(rec, set_spec)
                if parsed:
                    records.append(parsed)
            token_el = root.find(".//oai:resumptionToken", self.OAI_NS)
            if token_el is None or not (token_el.text or "").strip():
                break
            params = {"verb": "ListRecords", "resumptionToken": token_el.text.strip()}
        self._write_cache(set_spec, records, scanned)
        return records, scanned, False

    def _search_cyberleninka(self, query: str, max_results: int,
                             max_pages_per_set: int = 4):
        """Bounded OAI-PMH harvest of the curated sets — in parallel, from the
        cache when fresh — filtered locally.

        The scan budget is reported in the status string because a bounded
        scan returning nothing is NOT evidence the source lacks the topic.
        """
        tokens = _query_tokens(query)
        if not tokens:
            return [], ("error: query has no searchable token "
                        "(3+ characters, not a stopword)")
        results: List[Dict] = []
        seen_ids = set()
        scanned = 0
        cached_sets = 0
        errors: List[str] = []
        harvested: Dict[str, List[Dict]] = {}
        with cf.ThreadPoolExecutor(max_workers=len(self.CYBERLENINKA_SETS)) as pool:
            futures = {pool.submit(self._harvest_set, set_spec, max_pages_per_set): set_spec
                       for set_spec in self.CYBERLENINKA_SETS}
            for fut in cf.as_completed(futures):
                try:
                    records, n, from_cache = fut.result()
                except Exception as e:
                    errors.append(f"{futures[fut]}: {type(e).__name__}: {e}")
                    continue
                scanned += n
                cached_sets += int(from_cache)
                harvested[futures[fut]] = records
        # Merge in the curated set order, not thread-completion order, so the
        # same query yields the same list every run.
        for set_spec in self.CYBERLENINKA_SETS:
            for rec in harvested.get(set_spec, []):
                if _matches(rec.get("title", ""), tokens) and rec.get("source_id") not in seen_ids:
                    seen_ids.add(rec.get("source_id"))
                    results.append(rec)
        results = results[:max_results]
        if errors and scanned == 0:
            return [], f"error: {'; '.join(errors)} (scanned 0)"
        cache_note = f"; {cached_sets}/{len(self.CYBERLENINKA_SETS)} sets from cache" if cached_sets else ""
        err_note = f"; {len(errors)} set(s) failed: {'; '.join(errors)}" if errors else ""
        return results, (
            f"ok ({len(results)} results; scanned {scanned} records{cache_note}, harvest "
            f"bounded at {max_pages_per_set} pages/set — empty is not absence{err_note})"
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
        """archive.org advancedsearch, texts only — scanned Soviet handbooks,
        GOST texts, Chinese technical scans. A Chinese query returned GitHub
        mirrors (measured 2026-09-05): the query is restricted to texts and
        every hit must carry a query term in its title; the dropped count is
        reported so a thin list is not mistaken for a thin corpus."""
        tokens = _query_tokens(query)
        try:
            params = [("q", f"({query}) AND mediatype:texts"),
                      ("rows", min(max_results, 100)), ("output", "json")]
            params += [("fl[]", f) for f in
                       ("identifier", "title", "year", "language", "creator")]
            resp = requests.get(self.ARCHIVE_API, params=params, timeout=40)
            resp.raise_for_status()
            docs = resp.json().get("response", {}).get("docs", [])
        except Exception as e:
            return [], f"error: {type(e).__name__}: {e}"
        results = []
        dropped = 0
        for d in docs:
            title = (d.get("title") or "").strip()
            if not title:
                continue  # untitled scan: nothing to identify or extract from
            if tokens and not _matches(title, tokens):
                dropped += 1
                continue
            creator = d.get("creator")
            declared = d.get("language")
            if isinstance(declared, list):
                declared = declared[0] if declared else None
            results.append(self._enrich({
                "source": "internet_archive",
                "source_id": f"archive:{d.get('identifier', '')}",
                "title": title,
                "authors": creator if isinstance(creator, list) else ([creator] if creator else []),
                "abstract": "",
                "year": d.get("year"),
                "url": f"https://archive.org/details/{d.get('identifier', '')}",
                "type": "scan",
            }, declared_language=declared))
        status = None if not dropped else f"ok ({len(results)} results; {dropped} off-topic dropped)"
        return results, status
