"""Patent search collector.

**NO KEYLESS PATENT API SURVIVES PRODUCTION USE.** Measured 2026-08-17:

    Google Patents xhr   200 with 3494 hits for ~4 requests, then 503 for
                         EVERY caller including plain curl — Google blocks
                         the undocumented endpoint fast. Fine for one probe,
                         useless for an agent making dozens of searches.
    PatentsView legacy   301 (api.patentsview.org retired)
    PatentsView v1       needs a free API key
    USPTO ODP            401 — needs a free API key
    EPO OPS              needs OAuth registration (free)
    Espacenet            403
    Lens.org             needs LENS_API_TOKEN

So a working patent source REQUIRES an operator credential. That is an
owner action, not something this file can solve, and pretending otherwise
would make prior-art absence unreliable — the one claim that must never be
guessed, because "nobody has patented this" is a business decision.

Backend selection, in order:
  1. `LENS_API_TOKEN`  → Lens.org (documented, worldwide)
  2. otherwise         → Google Patents, best-effort, WILL rate-limit

Google Patents stays as the no-credential fallback because a few results
beat none while a key is being obtained — but it raises loudly the moment
it is throttled, rather than reporting an empty result set. Swapping in EPO
OPS or PatentsView is a change to this one file; callers see the same shape.
"""

import json
import os
import urllib.parse
from typing import Dict, List

import requests

from app.tools.data_collectors.base_collector import CollectorConfigError, DataCollector


class PatentCollector(DataCollector):
    name = "patents"

    LENS_API = "https://api.lens.org/patent/search"
    GOOGLE_PATENTS_API = "https://patents.google.com/xhr/query"
    # Identify ourselves. Not a disguise — a courtesy, and it makes our
    # traffic attributable if the operator ever needs to explain it.
    USER_AGENT = "PRISM/1.0 (materials research; +https://marc27.com)"

    def collect(self, query: str = "", max_results: int = 20, **kwargs) -> List[Dict]:
        if not query:
            return []
        if os.getenv("LENS_API_TOKEN"):
            return self._collect_lens(query, max_results)
        return self._collect_google(query, max_results)

    # ── Google Patents (default, keyless) ────────────────────────────────

    def _collect_google(self, query: str, max_results: int) -> List[Dict]:
        # The endpoint takes a URL-encoded query string in `url=`, i.e. the
        # same `q=` a browser would send.
        inner = urllib.parse.urlencode({"q": query})
        params = {"url": inner, "exp": ""}
        try:
            resp = requests.get(
                self.GOOGLE_PATENTS_API,
                params=params,
                headers={"User-Agent": self.USER_AGENT, "Accept": "application/json"},
                timeout=30,
            )
            resp.raise_for_status()
            payload = resp.json()
        except (requests.RequestException, json.JSONDecodeError) as error:
            # A transport or shape failure on an UNDOCUMENTED endpoint is
            # exactly the case that must be loud: "no patents found" would
            # be a lie, and prior-art absence is a claim with consequences.
            raise CollectorConfigError(
                f"Google Patents search failed ({error}); set LENS_API_TOKEN to "
                "use the documented Lens.org API instead"
            ) from error

        results_block = (payload.get("results") or {})
        clusters = results_block.get("cluster") or []
        out: List[Dict] = []
        for cluster in clusters:
            for hit in cluster.get("result") or []:
                patent = hit.get("patent") or {}
                number = patent.get("publication_number", "")
                if not number:
                    continue
                out.append(
                    {
                        "source": "google_patents",
                        "source_id": f"patent:{number}",
                        "title": _unescape(patent.get("title", "")),
                        "abstract": _unescape(patent.get("snippet", "")),
                        "published": patent.get("grant_date")
                        or patent.get("publication_date", ""),
                        "inventors": _split_names(patent.get("inventor", "")),
                        "applicants": _split_names(patent.get("assignee", "")),
                        "jurisdiction": _jurisdiction(number),
                        "url": f"https://patents.google.com/patent/{number}",
                        "has_pdf": bool(patent.get("pdf")),
                        # A patent is evidence of a CLAIM, not of a
                        # measurement. Downstream ranking must not treat it
                        # as a measured value.
                        "type": "patent",
                        "evidence_kind": "claim",
                    }
                )
                if len(out) >= max_results:
                    return out
        return out

    # ── Lens.org (used when a token is configured) ───────────────────────

    def _collect_lens(self, query: str, max_results: int) -> List[Dict]:
        token = os.getenv("LENS_API_TOKEN")
        if not token:
            raise CollectorConfigError(
                "patents source requires LENS_API_TOKEN (Lens.org) — not configured"
            )
        headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
        body = {
            "query": {"match": {"title": query}},
            "size": min(max_results, 50),
            "include": [
                "lens_id",
                "title",
                "abstract",
                "date_published",
                "inventor",
                "applicant",
                "jurisdiction",
            ],
        }
        try:
            resp = requests.post(self.LENS_API, json=body, headers=headers, timeout=30)
            resp.raise_for_status()
            data = resp.json()
        except (requests.RequestException, json.JSONDecodeError) as error:
            # Previously this returned [] on ANY exception, so an expired
            # token, a 429 or a schema change all read as "no patents exist"
            # — the worst possible answer to a prior-art question.
            raise CollectorConfigError(
                f"Lens.org patent search failed ({error})"
            ) from error

        results: List[Dict] = []
        for hit in data.get("data", []):
            results.append(
                {
                    "source": "lens_patents",
                    "source_id": f"lens:{hit.get('lens_id', '')}",
                    "title": hit.get("title", ""),
                    "abstract": hit.get("abstract") or "",
                    "published": hit.get("date_published", ""),
                    "inventors": [
                        inv.get("extracted_name", {}).get("value", "")
                        for inv in (hit.get("inventor") or [])
                    ],
                    "applicants": [
                        app.get("extracted_name", {}).get("value", "")
                        for app in (hit.get("applicant") or [])
                    ],
                    "jurisdiction": hit.get("jurisdiction", ""),
                    "type": "patent",
                    "evidence_kind": "claim",
                }
            )
        return results

    def supported_params(self) -> List[str]:
        return ["query", "max_results"]


def _unescape(text: str) -> str:
    """Google's JSON carries HTML entities (`&hellip;`, `&amp;`) verbatim."""
    import html

    return html.unescape(text or "").strip()


def _split_names(value: str) -> List[str]:
    """Google returns a single comma-joined string for inventor/assignee."""
    if not value:
        return []
    return [part.strip() for part in value.split(",") if part.strip()]


def _jurisdiction(publication_number: str) -> str:
    """Leading letters of a publication number are its authority (US, CN, EP…)."""
    prefix = "".join(ch for ch in publication_number[:2] if ch.isalpha())
    return prefix.upper()
