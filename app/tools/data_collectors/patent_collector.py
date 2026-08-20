"""Patent search — a swappable backend behind one cache.

NO KEYLESS HTTP DOOR IS OPEN. Measured 2026-08-17: Google Patents' xhr
endpoint answers ~4 requests then 503s every caller; PatentsView's legacy host
is retired and v1 needs a key; USPTO ODP 401s; Espacenet 403s; EPO OPS needs
OAuth registration; Lens needs a token. So a patent source is always somebody's
account — and which account is the OPERATOR'S choice, never this file's.

BACKENDS ARE PLUGGABLE, and that is the contract, not a convenience. PRISM
hosts a patent service it charges for, but nobody is obliged to use it: an
operator with their own BigQuery project, their own Lens subscription, or
their own internal patent system must be able to point PRISM at it and get the
same result shape. Selected with `PRISM_PATENT_BACKEND`:

    bigquery   Google's `patents-public-data` via the caller's own gcloud.
               Free with a Google account; no key, no rate limit. This is the
               default because it is the one that works with no purchase.
    platform   PRISM's hosted patent service. Billed per search.
    lens       Lens.org, with the operator's `LENS_API_TOKEN`.

A site whose patent source is none of these does not need a hook here: the
collector registry is keyed by name, so registering a `DataCollector` called
"patents" replaces this one wholesale. That is the same extension path every
other source uses, and one plane is worth more than a bespoke plugin system
per collector.

CACHE IN FRONT OF ALL OF THEM. Prior-art search recurs across turns, agents
and sessions within one investigation, and every backend charges for a repeat
in money, quota or rate limit. Results are cached by (backend, query); patents
publish weekly at best, so a long TTL costs nothing. The cache lives at
`$PRISM_PATENT_CACHE` (default `~/.prism/cache/`) — point it at shared storage
and every PRISM instance reads one warm cache.

An empty result is NEVER invented. Every backend raises on failure, because
"no patents found" is read as "nobody has patented this", which is a business
conclusion nothing may guess at.
"""

import hashlib
import json
import os
import sqlite3
import time
from pathlib import Path
from typing import Dict, List, Optional

from app.tools.data_collectors.base_collector import CollectorConfigError, DataCollector

#: Google's public patent corpus — the no-purchase path.
PUBLIC_TABLE = "patents-public-data.patents.publications"

#: Patents publish weekly at best; the TTL bounds staleness, it is not freshness
#: theatre.
DEFAULT_TTL_SECONDS = 30 * 24 * 3600

def _scope_fingerprint() -> str:
    """Short, non-reversible tag for WHOSE view of the corpus this is.

    Hashed rather than stored: the inputs include credentials, and a cache
    index is not a place to keep them. Only a change of account or table needs
    to be detectable, and a digest does that.
    """
    material = "|".join(
        os.getenv(name, "")
        for name in ("PRISM_PATENT_TABLE", "LENS_API_TOKEN", "PRISM_PLATFORM_URL")
    )
    return hashlib.sha256(material.encode()).hexdigest()[:12]


def _cache_path() -> Path:
    root = Path(os.getenv("PRISM_PATENT_CACHE", Path.home() / ".prism" / "cache"))
    root.mkdir(parents=True, exist_ok=True)
    return root / "patents.sqlite"


def _connect() -> sqlite3.Connection:
    conn = sqlite3.connect(_cache_path(), timeout=30)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS patent_search ("
        "  key TEXT PRIMARY KEY,"
        "  query TEXT NOT NULL,"
        "  results_json TEXT NOT NULL,"
        "  fetched_at REAL NOT NULL)"
    )
    return conn


# NO default table is named here. PRISM must not carry an operator's private
# BigQuery resource in its source; the extract belongs in that operator's own
# configuration (`PRISM_PATENT_TABLE`). The byte ceiling below is what makes an
# unconfigured deployment safe, rather than a hardcoded pointer at someone's
# project.

# Most a single patent search may bill. Ten gigabytes is generous for the
# extract and far below one full-table scan, so a query that has silently
# escaped to the public table fails instead of costing money.
MAX_PATENT_BYTES_BILLED = 10 * 1024**3

# ── backends ─────────────────────────────────────────────────────────────


def _bigquery_backend(query: str, max_results: int) -> List[Dict]:
    try:
        from google.cloud import bigquery
    except ImportError as error:
        raise CollectorConfigError(
            "the bigquery patent backend needs google-cloud-bigquery in the PRISM venv"
        ) from error

    # An operator may point at their own pre-filtered extract to cut the scan.
    # Measured: filtering `patents-public-data` to the materials CPC classes
    # (B22F, B33Y, C22C, C21D, C22F, C23C, B23K) turns a 253GB scan into a
    # 2.5GB table of 2.76M publications, and a search from ~21GB into ~1.9GB.
    # The extract must project title/abstract flat, as below.
    # An operator may point at their own pre-filtered extract to cut the scan.
    # Measured 2026-08-20 from a billing alert: seventeen searches against the
    # FULL public table billed 240.03 GB each — 4.1 TB, about EUR 24, from one
    # afternoon of testing, with nothing in the loop noticing until the alert
    # arrived. Filtering to the materials CPC classes (B22F, B33Y, C22C, C21D,
    # C22F, C23C, B23K) turns a search into roughly 1.9 GB.
    table = os.getenv("PRISM_PATENT_TABLE")
    if table:
        sql = f"""
            SELECT publication_number, country_code, grant_date, filing_date,
                   title, abstract, assignees, inventors
            FROM `{table}`
            WHERE LOWER(title) LIKE @needle OR LOWER(abstract) LIKE @needle
            ORDER BY grant_date DESC
            LIMIT @limit
        """
    else:
        # The public table nests localisations, so title/abstract must be
        # projected out before they can be matched.
        sql = f"""
            WITH flat AS (
              SELECT publication_number, country_code, grant_date, filing_date,
                (SELECT t.text FROM UNNEST(title_localized)    t
                  WHERE t.language='en' LIMIT 1) AS title,
                (SELECT a.text FROM UNNEST(abstract_localized) a
                  WHERE a.language='en' LIMIT 1) AS abstract,
                ARRAY(SELECT x.name FROM UNNEST(assignee_harmonized) x) AS assignees,
                ARRAY(SELECT x.name FROM UNNEST(inventor_harmonized) x) AS inventors
              FROM `{PUBLIC_TABLE}`
            )
            SELECT * FROM flat
            WHERE LOWER(title) LIKE @needle OR LOWER(abstract) LIKE @needle
            ORDER BY grant_date DESC
            LIMIT @limit
        """
    # Parameterised: the term is model-authored text and must never be
    # concatenated into SQL.
    config = bigquery.QueryJobConfig(
        # A HARD ceiling, not a hope. BigQuery bills by bytes SCANNED, so a
        # query against the wrong table is billed in full whether or not anyone
        # reads the answer, and nothing in the loop notices until a billing
        # alert arrives days later. Over this limit the job FAILS and costs
        # nothing, which is the only failure mode that cannot quietly spend
        # money.
        maximum_bytes_billed=MAX_PATENT_BYTES_BILLED,
        query_parameters=[
            bigquery.ScalarQueryParameter("needle", "STRING", f"%{query.strip().lower()}%"),
            bigquery.ScalarQueryParameter("limit", "INT64", max_results),
        ]
    )
    try:
        rows = list(bigquery.Client().query(sql, job_config=config).result())
    except Exception as error:
        raise CollectorConfigError(
            f"BigQuery patent search failed ({error}); check `gcloud auth "
            f"application-default login` and access to {table or PUBLIC_TABLE}"
        ) from error

    return [
        _shape(
            number=row["publication_number"],
            title=row["title"],
            abstract=row["abstract"],
            published=row["grant_date"] or row["filing_date"],
            inventors=row["inventors"],
            applicants=row["assignees"],
            jurisdiction=row["country_code"],
            source="patents_public_data",
        )
        for row in rows
    ]


def _lens_backend(query: str, max_results: int) -> List[Dict]:
    import requests

    token = os.getenv("LENS_API_TOKEN")
    if not token:
        raise CollectorConfigError(
            "the lens patent backend requires LENS_API_TOKEN (Lens.org)"
        )
    try:
        resp = requests.post(
            "https://api.lens.org/patent/search",
            json={
                "query": {"match": {"title": query}},
                "size": min(max_results, 50),
                "include": [
                    "lens_id", "title", "abstract", "date_published",
                    "inventor", "applicant", "jurisdiction",
                ],
            },
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
            timeout=30,
        )
        resp.raise_for_status()
        data = resp.json()
    except Exception as error:
        # Previously this returned [] on ANY exception, so an expired token, a
        # 429 or a schema change all read as "no patents exist".
        raise CollectorConfigError(f"Lens.org patent search failed ({error})") from error

    # A 200 whose body has no `data` key is a BROKEN response, not a clear
    # field. Throttle notices and schema changes arrive shaped like this, and
    # returning [] would cache "nobody has patented this" for the full TTL.
    if not isinstance(data.get("data"), list):
        raise CollectorConfigError(
            f"Lens.org returned no `data` array (keys: {sorted(data)[:6]}); "
            "treating this as a failed search, not an empty result"
        )

    return [
        _shape(
            number=hit.get("lens_id", ""),
            title=hit.get("title", ""),
            abstract=hit.get("abstract") or "",
            published=hit.get("date_published", ""),
            inventors=[i.get("extracted_name", {}).get("value", "") for i in (hit.get("inventor") or [])],
            applicants=[a.get("extracted_name", {}).get("value", "") for a in (hit.get("applicant") or [])],
            jurisdiction=hit.get("jurisdiction", ""),
            source="lens_patents",
        )
        for hit in data.get("data", [])
    ]


def _platform_backend(query: str, max_results: int) -> List[Dict]:
    """PRISM's hosted patent service. BILLED per search."""
    import requests

    base = os.getenv("PRISM_PLATFORM_URL")
    token = os.getenv("PRISM_PLATFORM_TOKEN")
    if not base or not token:
        raise CollectorConfigError(
            "the platform patent backend needs PRISM_PLATFORM_URL and "
            "PRISM_PLATFORM_TOKEN; or set PRISM_PATENT_BACKEND=bigquery to use "
            "your own Google account instead"
        )
    try:
        resp = requests.get(
            f"{base.rstrip('/')}/patents/search",
            params={"q": query, "limit": max_results},
            headers={"Authorization": f"Bearer {token}"},
            timeout=30,
        )
        resp.raise_for_status()
        data = resp.json()
    except Exception as error:
        raise CollectorConfigError(f"platform patent search failed ({error})") from error

    # As above: absent key != zero results.
    if not isinstance(data.get("results"), list):
        raise CollectorConfigError(
            f"platform returned no `results` array (keys: {sorted(data)[:6]}); "
            "treating this as a failed search, not an empty result"
        )

    return [
        _shape(
            number=hit.get("publication_number", ""),
            title=hit.get("title", ""),
            abstract=hit.get("abstract", ""),
            published=hit.get("published", ""),
            inventors=hit.get("inventors") or [],
            applicants=hit.get("applicants") or [],
            jurisdiction=hit.get("jurisdiction", ""),
            source="prism_platform_patents",
        )
        for hit in data.get("results", [])
    ]


def _shape(*, number, title, abstract, published, inventors, applicants, jurisdiction, source) -> Dict:
    """One result shape, whichever backend produced it."""
    return {
        "source": source,
        "source_id": f"patent:{number}",
        "title": title or "",
        "abstract": abstract or "",
        "published": str(published or ""),
        "inventors": list(inventors or []),
        "applicants": list(applicants or []),
        "jurisdiction": jurisdiction or "",
        "url": f"https://patents.google.com/patent/{number}",
        # A patent is evidence of a CLAIM, not of a measurement. Downstream
        # ranking must not treat it as a measured value.
        "type": "patent",
        "evidence_kind": "claim",
    }


#: The built-in services. Replacing the whole collector is the supported way
#: to add one, so this stays a plain lookup rather than a mutable registry.
_BACKENDS = {
    "bigquery": _bigquery_backend,
    "lens": _lens_backend,
    "platform": _platform_backend,
}


class PatentCollector(DataCollector):
    name = "patents"

    def collect(self, query: str = "", max_results: int = 20, **kwargs) -> List[Dict]:
        if not query:
            return []
        backend = os.getenv("PRISM_PATENT_BACKEND", "bigquery")
        if backend not in _BACKENDS:
            raise CollectorConfigError(
                f"unknown patent backend {backend!r}; available: "
                f"{', '.join(sorted(_BACKENDS))}"
            )
        # Keyed by backend AND by which account/table answered. Two services
        # do not agree, and neither do two tenants: the docstring recommends
        # pointing the cache at shared storage, so without this a deployment
        # reading a private CPC extract would serve those results to a
        # deployment that never had access to it.
        key = f"{backend}|{_scope_fingerprint()}|{query.strip().lower()}|{max_results}"

        cached = self._cache_get(key)
        if cached is not None:
            return cached
        results = _BACKENDS[backend](query, max_results)
        self._cache_put(key, query, results)
        return results

    def _cache_get(self, key: str) -> Optional[List[Dict]]:
        ttl = float(os.getenv("PRISM_PATENT_CACHE_TTL", DEFAULT_TTL_SECONDS))
        try:
            with _connect() as conn:
                row = conn.execute(
                    "SELECT results_json, fetched_at FROM patent_search WHERE key = ?",
                    (key,),
                ).fetchone()
        except sqlite3.Error:
            # A broken cache must never take the search down with it: fall
            # through and pay for the query instead of failing the call.
            return None
        if not row:
            return None
        results_json, fetched_at = row
        if time.time() - fetched_at > ttl:
            return None
        try:
            return json.loads(results_json)
        except json.JSONDecodeError:
            return None

    def _cache_put(self, key: str, query: str, results: List[Dict]) -> None:
        try:
            with _connect() as conn:
                conn.execute(
                    "INSERT OR REPLACE INTO patent_search"
                    " (key, query, results_json, fetched_at) VALUES (?, ?, ?, ?)",
                    (key, query, json.dumps(results), time.time()),
                )
        except sqlite3.Error:
            pass  # an unwritable cache costs money, not correctness

    def supported_params(self) -> List[str]:
        return ["query", "max_results"]
