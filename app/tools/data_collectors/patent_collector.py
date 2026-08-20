"""Patent search — a swappable backend over a corpus that is cut ONCE.

NO KEYLESS HTTP DOOR IS OPEN. Measured 2026-08-17: Google Patents' xhr
endpoint answers ~4 requests then 503s every caller; PatentsView's legacy host
is retired and v1 needs a key; USPTO ODP 401s; Espacenet 403s; EPO OPS needs
OAuth registration; Lens needs a token. So a patent source is always somebody's
account — and which account is the OPERATOR'S choice, never this file's.

BACKENDS ARE PLUGGABLE, and that is the contract, not a convenience. An
operator with their own BigQuery project, their own Lens subscription, or their
own hosted patent service must be able to point PRISM at it and get the same
result shape:

    platform   A hosted patent service. Needs PRISM_PLATFORM_URL and
               PRISM_PLATFORM_TOKEN. Billed per search.
    bigquery   The caller's own Google Cloud project. Needs PRISM_PATENT_TABLE
               (a flat extract), or builds one once — see below.
    lens       Lens.org, with the operator's LENS_API_TOKEN.

NOTHING IS DEFAULTED. Which backend answers is decided by which credentials
are present, in the order above, and PRISM_PATENT_BACKEND overrides that
order. With no credentials at all the collector REFUSES and names every route.
It used to fall through to an unconfigured full-corpus scan, and that guess
billed 240.03 GB per search — 4.3 TB and about EUR 24 from one afternoon of
testing, unnoticed until the billing alert arrived.

THE COST IS THE CORPUS, NOT THE QUERY. That is why this file caches two
different things. The expensive one is the corpus: the public patent table is
253 GB to scan, so the bigquery backend cuts it ONCE — filtered to the
materials CPC classes, projected flat — into a ~2.5 GB extract in the caller's
own project, after which a search reads ~1.9 GB. A result cache alone could
never have helped, because seventeen phrasings of a handful of questions are
seventeen misses and therefore seventeen full scans.

THE RESULT CACHE IS KEYED ON CONTENT, NOT ON PHRASING, for the same reason:
"PFAS free seals" and "seals PFAS-free" share one entry. When a stored answer
is served to a differently-worded question that substitution is logged — a
broad cache silently answering a question nobody asked is worse than a miss.
The cache lives at `$PRISM_PATENT_CACHE` (default `~/.prism/cache/`); point it
at shared storage and every PRISM instance reads one warm cache.

A site whose patent source is none of these does not need a hook here: the
collector registry is keyed by name, so registering a `DataCollector` called
"patents" replaces this one wholesale.

An empty result is NEVER invented. Every backend raises on failure, because
"no patents found" is read as "nobody has patented this", which is a business
conclusion nothing may guess at.
"""

import hashlib
import json
import logging
import os
import re
import shlex
import sqlite3
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Dict, List, Optional

from app.tools.data_collectors.base_collector import CollectorConfigError, DataCollector

logger = logging.getLogger(__name__)

#: Google's free public patent corpus. It is the source the extract is CUT
#: FROM, never the table a search runs against — see MAX_PATENT_BYTES_BILLED.
#: It is public and billed to the caller's own account; no operator's private
#: resource is named anywhere in this file.
PUBLIC_TABLE = "patents-public-data.patents.publications"

#: Where the cut lands. The PROJECT is read off the caller's own client at
#: runtime (`client.project`) and never written down here.
EXTRACT_DATASET = "prism_patents"
EXTRACT_TABLE = "materials_publications"

#: The CPC classes PRISM actually asks about: powder metallurgy (B22F),
#: additive manufacturing (B33Y), alloys (C22C), heat treatment of ferrous
#: (C21D) and non-ferrous (C22F) metals, coating (C23C), welding (B23K).
#: Measured: this filter turns 253 GB / ~140M publications into a 2.5 GB
#: extract of 2.76M publications.
MATERIALS_CPC = ("B22F", "B33Y", "C22C", "C21D", "C22F", "C23C", "B23K")

#: Patents publish weekly at best; the TTL bounds staleness, it is not freshness
#: theatre.
DEFAULT_TTL_SECONDS = 30 * 24 * 3600

# Most a single patent search may bill. Ten gigabytes is generous for the
# extract (~1.9 GB per search) and far below one full-corpus pass, so a query
# that has silently escaped to the public table fails instead of costing money.
MAX_PATENT_BYTES_BILLED = 10 * 1024**3

# Most the ONE-TIME extract build may bill. Building it reads the whole public
# corpus once, and that pass measured 253 GB, so this is the smallest ceiling
# that still leaves the corpus room to grow. It is deliberately a SEPARATE,
# larger number from the per-search ceiling above: the build is allowed to be
# expensive exactly once, a search never is.
MAX_EXTRACT_BUILD_BYTES_BILLED = 300 * 1024**3

# On-demand analysis price, used ONLY to print a human-readable estimate before
# the build spends anything. A signpost, not billing truth.
USD_PER_TIB_SCANNED = 6.25


def _scope_fingerprint() -> str:
    """Short, non-reversible tag for WHOSE view of the corpus this is.

    Hashed rather than stored: the inputs include credentials, and a cache
    index is not a place to keep them. Only a change of account or table needs
    to be detectable, and a digest does that.

    The Google project is in here because the bigquery backend now resolves an
    extract inside the CALLER'S project: two tenants sharing one cache
    directory must not read each other's rows. Residual: a project taken from
    application-default credentials rather than the environment is invisible
    here, so tenants who share a cache dir should set the project explicitly.
    """
    material = "|".join(
        os.getenv(name, "")
        for name in (
            "PRISM_PATENT_TABLE",
            "LENS_API_TOKEN",
            "PRISM_PLATFORM_URL",
            "GOOGLE_CLOUD_PROJECT",
            "GCLOUD_PROJECT",
        )
    )
    return hashlib.sha256(material.encode()).hexdigest()[:12]


#: Bumped when the cache KEY changes meaning. v1 keyed on the literal query
#: string; reading those rows under a normalised key would serve an answer
#: stored for one question to a different one without anyone noticing.
_CACHE_SCHEMA = 2
_CACHE_TABLE = f"patent_search_v{_CACHE_SCHEMA}"

#: Words that carry no corpus meaning, so two phrasings that differ only in
#: these are the same question. Deliberately tiny — a big stopword list starts
#: NOT normalised. An earlier version keyed the cache on sorted content words so
#: rephrasings would share an entry. Two reviewers independently proved that
#: ships wrong answers:
#:
#:     key("seals with PFAS") == key("seals without PFAS")  ->  "pfas seals"
#:
#: because "without" is a stopword — a negation and its affirmation collapse to
#: one entry, while the BACKEND still matches the literal contiguous phrase. Key
#: collision is not predicate identity. With empty results cached for 30 days,
#: one unlucky phrasing serves a false "no prior art" to every rephrasing for a
#: month, which is exactly the conclusion this module's docstring forbids anyone
#: from guessing at.
#:
#: The economics that justified normalising are also gone: against the CPC
#: extract a search reads ~1.9 GB (~EUR 0.012), not 240 GB. Sharing an entry now
#: saves about one cent and can cost the answer.


def _cache_path() -> Path:
    root = Path(os.getenv("PRISM_PATENT_CACHE", Path.home() / ".prism" / "cache"))
    root.mkdir(parents=True, exist_ok=True)
    return root / "patents.sqlite"


def _connect() -> sqlite3.Connection:
    conn = sqlite3.connect(_cache_path(), timeout=30)
    conn.execute(
        f"CREATE TABLE IF NOT EXISTS {_CACHE_TABLE} ("
        "  key TEXT PRIMARY KEY,"
        "  query TEXT NOT NULL,"
        "  results_json TEXT NOT NULL,"
        "  fetched_at REAL NOT NULL)"
    )
    return conn


# ── the corpus extract ───────────────────────────────────────────────────


@contextmanager
def _build_lock():
    """Serialise the one expensive thing in this file, across processes.

    Two searches starting together would otherwise both find the extract
    missing and both launch a full-corpus pass — the 253 GB scan, twice. The
    lock file lives under the cache dir because that is the one directory this
    module already owns, and the one operators are told to share.
    """
    path = _cache_path().with_suffix(".build.lock")
    try:
        import fcntl
    except ImportError:
        # No flock (Windows). The existence re-check under this contextmanager
        # still narrows the window; it cannot close it. Say so rather than
        # pretend the guard held.
        logger.warning(
            "no file locking on this platform; two concurrent patent searches "
            "could each start an extract build"
        )
        yield
        return
    with open(path, "w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def _extract_sql(target: str) -> str:
    """The one-time cut: the public corpus filtered to the materials CPC
    classes and projected FLAT, so a later search is a plain LIKE over two
    columns instead of an UNNEST over the whole corpus.

    `IF NOT EXISTS` is belt to the caller's braces, NOT a cost guard: we cannot
    rely on the SELECT being skipped when the table already exists, which is
    why `_ensure_extract` checks first and holds a lock.
    """
    return f"""
        CREATE TABLE IF NOT EXISTS `{target}` AS
        SELECT publication_number, country_code, grant_date, filing_date,
          (SELECT t.text FROM UNNEST(title_localized)    t
            WHERE t.language='en' LIMIT 1) AS title,
          (SELECT a.text FROM UNNEST(abstract_localized) a
            WHERE a.language='en' LIMIT 1) AS abstract,
          ARRAY(SELECT x.name FROM UNNEST(assignee_harmonized) x) AS assignees,
          ARRAY(SELECT x.name FROM UNNEST(inventor_harmonized) x) AS inventors
        FROM `{PUBLIC_TABLE}`
        WHERE EXISTS (
          SELECT 1 FROM UNNEST(cpc) c
          WHERE REGEXP_CONTAINS(c.code, r'^({"|".join(MATERIALS_CPC)})')
        )
    """


def _table_exists(client, target: str) -> bool:
    """Only a genuine 404 means "absent".

    A permission error must NOT read as "not there, go build it" — that would
    answer a misconfigured credential with a 253 GB scan.
    """
    from google.api_core.exceptions import NotFound

    try:
        client.get_table(target)
        return True
    except NotFound:
        return False


def _build_extract(client, target: str) -> None:
    from google.cloud import bigquery

    # The destination must live where the source does, and the public corpus is
    # in the US multi-region. An operator who needs another location builds
    # their own extract and points PRISM_PATENT_TABLE at it.
    dataset = bigquery.Dataset(f"{client.project}.{EXTRACT_DATASET}")
    dataset.location = "US"
    client.create_dataset(dataset, exists_ok=True)

    sql = _extract_sql(target)

    # Say what it will cost BEFORE spending it. A dry run bills nothing, and it
    # is the only honest estimate — the 253 GB figure in this file came from one.
    dry = client.query(
        sql,
        job_config=bigquery.QueryJobConfig(dry_run=True, use_query_cache=False),
    )
    scanned_gb = (dry.total_bytes_processed or 0) / 1024**3
    logger.warning(
        "building the patent extract %s: one pass of the public corpus, "
        "~%.1f GB, ~USD %.2f. This happens ONCE — every later search reads the "
        "extract (~1.9 GB) instead. Set PRISM_PATENT_AUTOBUILD=0 to refuse.",
        target,
        scanned_gb,
        scanned_gb / 1024 * USD_PER_TIB_SCANNED,
    )

    job = client.query(
        sql,
        job_config=bigquery.QueryJobConfig(
            # Same reasoning as the search ceiling, one size up: over this the
            # job FAILS and costs nothing, which is the only failure mode that
            # cannot quietly spend money.
            maximum_bytes_billed=MAX_EXTRACT_BUILD_BYTES_BILLED,
        ),
    )
    job.result()
    # The job id is the receipt: without it nobody can tie a line on the bill
    # back to this build.
    logger.warning(
        "patent extract %s built by job %s (%.1f GB billed)",
        target,
        job.job_id,
        (job.total_bytes_billed or 0) / 1024**3,
    )


def _ensure_extract(client) -> str:
    """Resolve the flat extract a search runs against, building it ONCE if needed."""
    configured = os.getenv("PRISM_PATENT_TABLE")
    if configured:
        return configured

    # The caller's OWN default project, read off the client at runtime.
    target = f"{client.project}.{EXTRACT_DATASET}.{EXTRACT_TABLE}"
    if _table_exists(client, target):
        return target

    # Fail CLOSED on anything that is not an explicit yes. `== "0"` treated
    # `false`, `no`, `off` and `False` as PERMISSION TO SPEND — the one switch
    # standing between a misconfiguration and a 253 GB scan must not depend on
    # guessing the operator's spelling.
    if os.getenv("PRISM_PATENT_AUTOBUILD", "1").strip().lower() not in {
        "1",
        "true",
        "yes",
        "on",
    }:
        # Shell-quoted, because the SQL contains both single quotes and
        # backticks: an error message with a command that does not actually
        # run is the same dead end as no message.
        command = shlex.quote(" ".join(_extract_sql(target).split()))
        raise CollectorConfigError(
            f"no patent extract at {target}, and PRISM_PATENT_AUTOBUILD=0 "
            f"forbids building one. Either point PRISM_PATENT_TABLE at an "
            f"extract you already have, or build it yourself:\n\n"
            f"  bq --location=US query --use_legacy_sql=false "
            f"--maximum_bytes_billed={MAX_EXTRACT_BUILD_BYTES_BILLED} "
            f"{command}"
        )

    with _build_lock():
        # Re-check under the lock: whoever held it before us may have built it
        # while we waited, and a second 253 GB pass would be pure waste.
        if not _table_exists(client, target):
            _build_extract(client, target)
    return target


# ── backends ─────────────────────────────────────────────────────────────


def _bigquery_backend(query: str, max_results: int) -> List[Dict]:
    try:
        from google.cloud import bigquery
    except ImportError as error:
        raise CollectorConfigError(
            "the bigquery patent backend needs google-cloud-bigquery in the PRISM venv"
        ) from error

    try:
        client = bigquery.Client()
    except Exception as error:
        raise CollectorConfigError(
            f"BigQuery is not authenticated ({error}); run "
            f"`gcloud auth application-default login`"
        ) from error

    try:
        table = _ensure_extract(client)
    except CollectorConfigError:
        raise
    except Exception as error:
        # A build or lookup fault is a FAILED search, not an empty one.
        raise CollectorConfigError(
            f"could not resolve the patent extract ({error})"
        ) from error

    # A search NEVER touches the public corpus. Measured 2026-08-20 from a
    # billing alert: seventeen searches against the full table billed 240.03 GB
    # EACH — 4.3 TB, about EUR 24, from one afternoon of testing, with nothing
    # in the loop noticing until the alert arrived. Against the CPC extract the
    # same search is ~1.9 GB.
    sql = f"""
        SELECT publication_number, country_code, grant_date, filing_date,
               title, abstract, assignees, inventors
        FROM `{table}`
        WHERE LOWER(title) LIKE @needle OR LOWER(abstract) LIKE @needle
        ORDER BY grant_date DESC
        LIMIT @limit
    """
    # Parameterised: the term is model-authored text and must never be
    # concatenated into SQL.
    config = bigquery.QueryJobConfig(
        # A HARD ceiling, not a hope. BigQuery bills by bytes SCANNED, so a
        # query against the wrong table is billed in full whether or not anyone
        # reads the answer. Over this limit the job FAILS and costs nothing.
        maximum_bytes_billed=MAX_PATENT_BYTES_BILLED,
        query_parameters=[
            bigquery.ScalarQueryParameter("needle", "STRING", f"%{query.strip().lower()}%"),
            bigquery.ScalarQueryParameter("limit", "INT64", max_results),
        ],
    )
    try:
        rows = list(client.query(sql, job_config=config).result())
    except Exception as error:
        raise CollectorConfigError(
            f"BigQuery patent search failed ({error}); check `gcloud auth "
            f"application-default login` and access to {table}"
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
    """A hosted patent service, named entirely by the operator. BILLED per search."""
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


def resolve_backend() -> str:
    """Which patent service answers, decided by what is actually configured.

    There is no default. The old unconditional `bigquery` default was the bug:
    an unconfigured deployment fell through to a full scan of the public
    corpus and billed 240 GB a search. Credentials are the signal — an
    operator who has set up a route has said which one they mean.
    """
    explicit = os.getenv("PRISM_PATENT_BACKEND", "").strip()
    if explicit:
        # An explicit choice is obeyed even if its credentials are missing, so
        # the error names the credential rather than silently routing elsewhere.
        if explicit not in _BACKENDS:
            raise CollectorConfigError(
                f"unknown patent backend {explicit!r}; available: "
                f"{', '.join(sorted(_BACKENDS))}"
            )
        return explicit
    if os.getenv("PRISM_PLATFORM_URL") and os.getenv("PRISM_PLATFORM_TOKEN"):
        return "platform"
    if os.getenv("PRISM_PATENT_TABLE"):
        return "bigquery"
    if os.getenv("LENS_API_TOKEN"):
        return "lens"
    raise CollectorConfigError(
        "no patent backend is configured, and PRISM will not guess — the guess "
        "used to be a full scan of the public corpus, which billed 240 GB per "
        "search. Configure one of:\n"
        "  platform  PRISM_PLATFORM_URL + PRISM_PLATFORM_TOKEN "
        "(a hosted patent service, billed per search)\n"
        "  bigquery  PRISM_PATENT_TABLE, a flat extract in your own Google "
        "project; or PRISM_PATENT_BACKEND=bigquery to have PRISM cut one once "
        "from the public corpus into your own default project\n"
        "  lens      LENS_API_TOKEN (a Lens.org subscription)\n"
        "PRISM_PATENT_BACKEND overrides this order."
    )


class PatentCollector(DataCollector):
    name = "patents"

    #: Which backend answered the last call. A zero has to be attributable to a
    #: named corpus; "0 results" on its own is the shape of a silent failure.
    last_backend: Optional[str] = None

    def collect(self, query: str = "", max_results: int = 20, **kwargs) -> List[Dict]:
        if not query:
            return []
        backend = resolve_backend()
        self.last_backend = backend
        # Keyed by backend AND by which account/table answered: two services do
        # not agree, and neither do two tenants. The query part is the LITERAL
        # phrase, because that is what the backend matches — see the note above
        # `_CACHE_SCHEMA`. The schema version stays in the key so entries written
        # under the withdrawn normalised scheme can never be read back.
        key = (
            f"v{_CACHE_SCHEMA}|{backend}|{_scope_fingerprint()}"
            f"|{query.strip().lower()}|{max_results}"
        )

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
                    f"SELECT query, results_json, fetched_at FROM {_CACHE_TABLE}"
                    " WHERE key = ?",
                    (key,),
                ).fetchone()
        except sqlite3.Error:
            # A broken cache must never take the search down with it: fall
            # through and pay for the query instead of failing the call.
            return None
        if not row:
            return None
        stored_query, results_json, fetched_at = row
        if time.time() - fetched_at > ttl:
            return None
        try:
            results = json.loads(results_json)
        except json.JSONDecodeError:
            return None
        # No substitution check is needed: the key IS the literal query, so a
        # hit cannot belong to a different question. The stored literal is kept
        # for inspection, not for reconciliation.
        del stored_query
        return results

    def _cache_put(self, key: str, query: str, results: List[Dict]) -> None:
        try:
            with _connect() as conn:
                conn.execute(
                    f"INSERT OR REPLACE INTO {_CACHE_TABLE}"
                    " (key, query, results_json, fetched_at) VALUES (?, ?, ?, ?)",
                    (key, query, json.dumps(results), time.time()),
                )
        except sqlite3.Error:
            pass  # an unwritable cache costs money, not correctness

    def supported_params(self) -> List[str]:
        return ["query", "max_results"]
