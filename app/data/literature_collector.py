"""Literature search collector — arXiv + Semantic Scholar."""
import xml.etree.ElementTree as ET
from typing import Dict, List

import requests

from app.data.base_collector import DataCollector


class LiteratureCollector(DataCollector):
    name = "literature"

    ARXIV_API = "http://export.arxiv.org/api/query"
    S2_API = "https://api.semanticscholar.org/graph/v1/paper/search"

    def collect(self, query: str = "", max_results: int = 20,
                sources: List[str] = None, **kwargs) -> List[Dict]:
        """Search arXiv and Semantic Scholar for papers."""
        if not query:
            return []
        sources = sources or ["arxiv", "semantic_scholar"]
        results = []
        if "arxiv" in sources:
            results.extend(self._search_arxiv(query, max_results))
        if "semantic_scholar" in sources:
            results.extend(self._search_s2(query, max_results))
        return results[:max_results]

    def _search_arxiv(self, query: str, max_results: int) -> List[Dict]:
        try:
            params = {
                "search_query": f"all:{query}",
                "start": 0,
                "max_results": max_results,
            }
            resp = requests.get(self.ARXIV_API, params=params, timeout=30)
            resp.raise_for_status()
            return self._parse_arxiv_xml(resp.text)
        except Exception:
            return []

    def _parse_arxiv_xml(self, xml_text: str) -> List[Dict]:
        ns = {"atom": "http://www.w3.org/2005/Atom"}
        root = ET.fromstring(xml_text)
        results = []
        for entry in root.findall("atom:entry", ns):
            title = entry.findtext("atom:title", "", ns).strip()
            summary = entry.findtext("atom:summary", "", ns).strip()
            arxiv_id = entry.findtext("atom:id", "", ns).strip()
            published = entry.findtext("atom:published", "", ns).strip()
            authors = [
                a.findtext("atom:name", "", ns)
                for a in entry.findall("atom:author", ns)
            ]
            results.append({
                "source": "arxiv",
                "source_id": arxiv_id,
                "title": title,
                "authors": authors,
                "abstract": summary,
                "published": published,
                "url": arxiv_id,
                "type": "paper",
            })
        return results

    def _search_s2(self, query: str, max_results: int) -> List[Dict]:
        try:
            params = {
                "query": query,
                "limit": min(max_results, 100),
                "fields": "title,authors,abstract,year,url,citationCount,externalIds",
            }
            resp = requests.get(self.S2_API, params=params, timeout=30)
            resp.raise_for_status()
            data = resp.json()
            results = []
            for paper in data.get("data", []):
                authors = [a.get("name", "") for a in (paper.get("authors") or [])]
                results.append({
                    "source": "semantic_scholar",
                    "source_id": paper.get("paperId", ""),
                    "title": paper.get("title", ""),
                    "authors": authors,
                    "abstract": paper.get("abstract", ""),
                    "year": paper.get("year"),
                    "url": paper.get("url", ""),
                    "citations": paper.get("citationCount", 0),
                    "type": "paper",
                })
            return results
        except Exception:
            return []

    def supported_params(self) -> List[str]:
        return ["query", "max_results", "sources"]
