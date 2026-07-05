"""Patent search collector — Lens.org API."""
import os
from typing import Dict, List

import requests

from app.tools.data_collectors.base_collector import CollectorConfigError, DataCollector


class PatentCollector(DataCollector):
    name = "patents"

    LENS_API = "https://api.lens.org/patent/search"

    def collect(self, query: str = "", max_results: int = 20, **kwargs) -> List[Dict]:
        """Search patents via Lens.org API."""
        if not query:
            return []
        token = os.getenv("LENS_API_TOKEN")
        if not token:
            # Missing credential is a misconfiguration, not "no patents found".
            # Raising lets prior_art_search report `patents_error` so the agent
            # knows the source was skipped rather than genuinely empty.
            raise CollectorConfigError(
                "patents source requires LENS_API_TOKEN (Lens.org) — not configured"
            )

        try:
            headers = {
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            }
            body = {
                "query": {"match": {"title": query}},
                "size": min(max_results, 50),
                "include": ["lens_id", "title", "abstract", "date_published",
                            "inventor", "applicant", "jurisdiction"],
            }
            resp = requests.post(self.LENS_API, json=body, headers=headers, timeout=30)
            resp.raise_for_status()
            data = resp.json()
            results = []
            for hit in data.get("data", []):
                inventors = [
                    inv.get("extracted_name", {}).get("value", "")
                    for inv in (hit.get("inventor") or [])
                ]
                applicants = [
                    app.get("extracted_name", {}).get("value", "")
                    for app in (hit.get("applicant") or [])
                ]
                results.append({
                    "source": "lens_patents",
                    "source_id": f"lens:{hit.get('lens_id', '')}",
                    "title": hit.get("title", ""),
                    "abstract": (hit.get("abstract") or ""),
                    "published": hit.get("date_published", ""),
                    "inventors": inventors,
                    "applicants": applicants,
                    "jurisdiction": hit.get("jurisdiction", ""),
                    "type": "patent",
                })
            return results
        except Exception:
            return []

    def supported_params(self) -> List[str]:
        return ["query", "max_results"]
