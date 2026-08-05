# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""HuggingFace PULL tool — anonymous discovery, details and download.

PRISM can already publish *out* to the Hub (``prism publish --target
huggingface`` shells to the ``hf`` CLI). This is the missing other direction:
finding and pulling models/datasets *in*. Anonymous public API only — this
tool NEVER authenticates, stores no token, and will not fetch gated repos.

Why search is more than a passthrough: keyword search on the Hub is near-
useless for materials science (searching ``mace`` returns Macedonian-language
NLP models, not the MACE interatomic potential). So ``search`` blends a
hand-curated materials index (:mod:`app.tools.hf_index`, the part that
compounds) with raw anonymous API results, tagging each result's source so
the contrast is visible. ``details`` and ``pull`` always hit the LIVE public
API for current truth.

Network-free testing: all I/O is behind two seams — :func:`_request_json`
(metadata GETs) and :func:`_download_file` (file streaming) — which tests
monkeypatch. The matcher and licence logic live in the pure
:mod:`app.tools.hf_index` and need no seams at all.
"""
from __future__ import annotations

import logging
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

from app.tools.base import Tool, ToolRegistry
from app.tools.hf_index import (
    classify_license,
    index_entry_for,
    match_index,
    present_index_entry,
)

logger = logging.getLogger(__name__)

HF_WEB_BASE = "https://huggingface.co"
HF_API_BASE = "https://huggingface.co"
_HEADERS = {"User-Agent": "prism-research/1.0 (anonymous; materials discovery)"}

#: Gated values that mean "not anonymously fetchable". HF ``gated`` is
#: ``False`` | ``True`` | ``"auto"`` | ``"manual"``. Anything truthy blocks
#: an anonymous pull, so we surface it at search time, never after.
_GATED_BLOCKING = {True, "auto", "manual", "anonymous-blocked"}


class _HFError(Exception):
    """Raised by :func:`_request_json` for HTTP errors tests can simulate."""

    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


# --- I/O seams (monkeypatch these in tests; never network from tests) -------


def _request_json(path: str, params: Optional[dict] = None) -> Any:
    """GET one JSON document from the anonymous public Hub API.

    ``path`` begins with ``/api/...``. Raises :class:`_HFError` on 401
    (anonymous access blocked — gated or org-restricted), 404 (not found) or
    transport failure, so callers can convert to an honest error dict.
    """
    import httpx

    url = HF_API_BASE + path
    try:
        r = httpx.get(url, params=params, headers=_HEADERS, timeout=20, follow_redirects=True)
    except Exception as exc:  # DNS/timeout/offline — surface honestly
        raise _HFError(0, f"network error contacting the Hub: {type(exc).__name__}: {exc}") from exc
    if r.status_code == 401:
        raise _HFError(401, "anonymous access blocked (repo is gated or org-restricted)")
    if r.status_code == 404:
        raise _HFError(404, "repo not found on the Hub")
    if r.status_code >= 400:
        raise _HFError(r.status_code, f"Hub API returned HTTP {r.status_code}")
    return r.json()


def _safe_dest(root: Path, rfilename: str) -> Path:
    """Resolve ``rfilename`` under ``root``, refusing anything that escapes.

    The Hub supplies ``rfilename``; we were joining it straight onto the
    destination and calling ``mkdir(parents=True)`` before any I/O, so a
    sibling named ``../../x`` or ``/tmp/x`` wrote outside the target — a
    reviewer reproduced both, creating a directory outside the root without
    downloading a byte. Trusting a remote server for a local write path is the
    bug, regardless of how unlikely such a name is in a git-backed repo.
    """
    root = root.resolve()
    candidate = (root / rfilename).resolve()
    if not candidate.is_relative_to(root):
        raise ValueError(f"refusing path outside the download root: {rfilename!r}")
    return candidate


def _download_file(url: str, dest: Path, budget: int) -> Dict[str, Any]:
    """Stream one file to ``dest``, aborting once ``budget`` bytes are written.

    The size cap used to be checked only AFTER a file finished, so a single
    100 GB sibling was written in full before truncation. The budget is now
    enforced mid-stream, and a partial file is removed rather than left behind
    looking complete.
    """
    import httpx

    dest.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    over = False
    with httpx.stream("GET", url, headers=_HEADERS, timeout=120, follow_redirects=True) as r:
        r.raise_for_status()
        with open(dest, "wb") as fh:
            for chunk in r.iter_bytes():
                if n + len(chunk) > budget:
                    over = True
                    break
                fh.write(chunk)
                n += len(chunk)
    if over:
        dest.unlink(missing_ok=True)
        raise ValueError(
            f"file exceeds the remaining {budget} byte budget; nothing kept for this file"
        )
    return {"path": str(dest), "bytes": n}


# --- helpers ----------------------------------------------------------------


def _plurals_for(kind: str) -> List[str]:
    """Map a user-facing kind to the API path segment(s) to try, in order."""
    kind = (kind or "auto").strip().lower()
    if kind in ("model", "models"):
        return ["models"]
    if kind in ("dataset", "datasets"):
        return ["datasets"]
    return ["models", "datasets"]  # "auto"


def _singular(plural: str) -> str:
    return "model" if plural == "models" else "dataset"


def _gate(value: Any) -> Any:
    """Normalise a raw HF ``gated`` value for output (keeps 'manual'/'auto')."""
    return value


def _license_from_tags(tags: Iterable[str]) -> Optional[str]:
    """Fallback licence extraction from ``license:<id>`` model tags."""
    for tag in tags or []:
        if isinstance(tag, str) and tag.startswith("license:"):
            return tag.split(":", 1)[1]
    return None


def _present_api(obj: Dict[str, Any], plural: str, index_entry: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
    """Format one live API object (search hit or detail) for tool output."""
    singular = _singular(plural)
    card = obj.get("cardData") or {}
    license_value = card.get("license") or _license_from_tags(obj.get("tags", []))
    cls = classify_license(license_value)
    out: Dict[str, Any] = {
        "repo": obj.get("id"),
        "kind": singular,
        "source": "huggingface_api",
        "license": cls["license"],
        "license_commercial": cls["license_commercial"],
        "license_note": cls["license_note"],
        "gated": _gate(obj.get("gated")),
        "downloads": obj.get("downloads"),
        "likes": obj.get("likes"),
        "hub_url": f"{HF_WEB_BASE}/{plural}/{obj.get('id')}",
    }
    # When a curated entry exists for the same repo, fold in its human context
    # (what it is, coverage, gaps) so the researcher gets intent, not just stats.
    entry = index_entry or (index_entry_for(obj.get("id", "")) if obj.get("id") else None)
    if entry:
        out["what"] = entry.get("what")
        out["covers"] = entry.get("covers")
        out["not_covering"] = entry.get("not_covering")
        out["verification"] = entry.get("verification")
        out["source"] = "curated_index+api"
    return out


def _present_blocked(repo_id: str, plural: str) -> Dict[str, Any]:
    """Honest record for a repo the anonymous API refused to read (HTTP 401)."""
    return {
        "repo": repo_id,
        "kind": _singular(plural),
        "source": "huggingface_api",
        "gated": "anonymous-blocked",
        "license": None,
        "license_commercial": None,
        "license_note": "anonymous API access blocked; licence could not be verified",
        "hub_url": f"{HF_WEB_BASE}/{plural}/{repo_id}",
        "note": "PRISM is anonymous-only. This repo requires authentication to "
        "inspect or download; the licence and gated status shown here are "
        "best-effort — confirm by viewing the repo.",
    }


# --- actions ----------------------------------------------------------------


def _search(query: str, kind: str = "auto", use_index: bool = True, limit: int = 10) -> Dict[str, Any]:
    """Blend curated-index matches with raw anonymous Hub search results."""
    query = (query or "").strip()
    if not query:
        return {"error": "query is required"}
    limit = max(1, min(int(limit or 10), 50))
    results: List[Dict[str, Any]] = []
    seen = set()

    if use_index:
        for entry in match_index(query, limit=limit * 2):
            presented = present_index_entry(entry)
            seen.add(presented["repo"])
            results.append(presented)

    api_errors: List[Dict[str, Any]] = []
    for plural in _plurals_for(kind):
        try:
            data = _request_json(f"/api/{plural}", {"search": query, "limit": limit, "full": "true"})
        except _HFError as exc:
            api_errors.append({"kind": _singular(plural), "status": exc.status, "message": exc.message})
            continue
        for obj in data[:limit]:
            repo = obj.get("id")
            if not repo or repo in seen:
                continue
            seen.add(repo)
            results.append(_present_api(obj, plural))

    return {
        "query": query,
        "kind": kind,
        "results": results,
        "count": len(results),
        "api_errors": api_errors,
        "note": (
            "Results tagged source='curated_index' are hand-checked PRISM mappings "
            "(high signal); source='huggingface_api' are raw keyword matches. Hub "
            "keyword search is weak for materials — prefer curated hits, then use "
            "hf(action='details', repo=...) for live licence/gated truth."
        ),
    }


def _details(repo: str, kind: str = "auto") -> Dict[str, Any]:
    """Fetch LIVE licence/gated/file metadata for one repo (anonymous)."""
    repo = (repo or "").strip()
    if not repo:
        return {"error": "repo is required"}
    last: Optional[Dict[str, Any]] = None
    for plural in _plurals_for(kind):
        try:
            obj = _request_json(f"/api/{plural}/{repo}")
        except _HFError as exc:
            if exc.status == 401:
                # Anonymous-blocked is itself the answer — return it honestly.
                return _present_blocked(repo, plural)
            if exc.status == 404:
                last = {"error": exc.message, "repo": repo, "kind": _singular(plural)}
                continue  # try the other kind under 'auto'
            return {"error": exc.message, "repo": repo, "status": exc.status}
        presented = _present_api(obj, plural)
        sib = obj.get("siblings") or []
        presented["file_count"] = len(sib)
        if sib:
            presented["sample_files"] = [s.get("rfilename") for s in sib[:8] if s.get("rfilename")]
        return presented
    return last or {"error": f"repo not found: {repo}"}


def _default_cache(repo_id: str) -> Path:
    safe = repo_id.replace("/", "__")
    return Path.home() / ".prism" / "hf_cache" / safe


def _pull(
    repo: str,
    kind: str = "auto",
    target: Optional[str] = None,
    max_files: int = 50,
    max_bytes_mb: int = 500,
) -> Dict[str, Any]:
    """Download a repo's files into a local cache. Anonymous; gated repos are
    refused *before* any download, never after. Streams with a file-count and
    byte cap so an accidental huge repo cannot fill the disk."""
    repo = (repo or "").strip()
    if not repo:
        return {"error": "repo is required"}
    max_files = max(1, min(int(max_files or 50), 5000))
    cap = max(1, int(max_bytes_mb or 500)) * 1024 * 1024

    # `kind="auto"` must try BOTH kinds, as search and details do. Taking only
    # `_plurals_for(kind)[0]` meant auto never looked at datasets, so pulling a
    # dataset that exists returned "repo not found" -- contradicting this
    # tool's own schema text ("'auto' tries both").
    obj = None
    plural = ""
    last_err: Optional[Dict[str, Any]] = None
    for candidate in _plurals_for(kind):
        try:
            obj = _request_json(f"/api/{candidate}/{repo}")
            plural = candidate
            break
        except _HFError as exc:
            if exc.status == 401:
                return {
                    "error": "repo is not anonymously accessible (gated or org-restricted); "
                    "PRISM will not fetch it",
                    "repo": repo, "gated": "anonymous-blocked",
                }
            if exc.status == 404:
                last_err = {"error": exc.message, "repo": repo, "status": exc.status}
                continue
            return {"error": exc.message, "repo": repo, "status": exc.status}
    if obj is None:
        return last_err or {"error": "repo not found on the Hub", "repo": repo}

    gated = obj.get("gated")
    # Truthiness, not set membership: an unrecognised value such as
    # "restricted" previously fell through the set and proceeded to download.
    if gated:
        return {
            "error": f"repo is gated ({gated!r}); PRISM operates anonymously and will not fetch gated repos",
            "repo": repo, "gated": _gate(gated),
        }

    siblings = obj.get("siblings") or []
    files = [s.get("rfilename") for s in siblings if s.get("rfilename")]
    if not files:
        return {"error": "repo has no readable files (empty or access-restricted)", "repo": repo}

    dest_root = Path(target) if target else _default_cache(repo)
    prefix = "datasets" if plural == "datasets" else ""
    base = f"{HF_WEB_BASE}/{prefix}/{repo}".strip("/")
    manifest: List[Dict[str, Any]] = []
    total = 0
    truncated = False
    for fn in files[:max_files]:
        url = f"{base}/resolve/main/{fn}"
        try:
            info = _download_file(url, _safe_dest(dest_root, fn), cap - total)
        except Exception as exc:
            manifest.append({"file": fn, "error": f"{type(exc).__name__}: {exc}"})
            continue
        total += info["bytes"]
        manifest.append({"file": fn, "bytes": info["bytes"], "path": info["path"]})
        if total >= cap:
            truncated = True
            break

    return {
        "repo": repo,
        "kind": _singular(plural),
        "target": str(dest_root),
        "files": manifest,
        "file_count": len(manifest),
        "bytes": total,
        "truncated": truncated,
        "note": ("Download capped — increase max_files/max_bytes_mb to fetch the rest. "
                 if truncated else None),
    }


# --- dispatcher -------------------------------------------------------------


def _hf(**kwargs: Any) -> Dict[str, Any]:
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: search, details, pull",
            "hint": (
                "hf(action='search', query='interatomic potential for inorganic crystals') "
                "— discover models/datasets (curated index + Hub search).\n"
                "hf(action='details', repo='fairchem/OMAT24') — live licence/gated/files.\n"
                "hf(action='pull', repo='atomind/alexandria') — anonymous download to ~/.prism/hf_cache."
            ),
        }
    if action == "search":
        return _search(
            query=kwargs.get("query", ""),
            kind=kwargs.get("kind", "auto"),
            use_index=kwargs.get("use_index", True),
            limit=kwargs.get("limit", 10),
        )
    if action == "details":
        return _details(repo=kwargs.get("repo", ""), kind=kwargs.get("kind", "auto"))
    if action == "pull":
        return _pull(
            repo=kwargs.get("repo", ""),
            kind=kwargs.get("kind", "auto"),
            target=kwargs.get("target"),
            max_files=kwargs.get("max_files", 50),
            max_bytes_mb=kwargs.get("max_bytes_mb", 500),
        )
    return {"error": f"Unknown action '{action}'. Valid: search, details, pull"}


_HF_DESCRIPTION = """\
Discover and pull models/datasets from the HuggingFace Hub (anonymous public \
API only — PRISM never authenticates and will not fetch gated repos).

ONE tool, three actions:
  • action='search' — find models/datasets for a materials query. Blends a \
HAND-CURATED materials index (high signal: MACE, OMat24, Alexandria, …) with \
raw Hub keyword search. Hub keyword search is weak for materials (searching \
'mace' returns Macedonian-language NLP models, not the MACE potential), so \
prefer results tagged source='curated_index'. Each result carries licence \
(license_commercial: true/false/null where null=unknown) and gated status at \
SEARCH TIME, so a non-commercial or gated source is never a surprise later. \
Params: query (required); kind='model'|'dataset'|'auto' (default auto); \
use_index=true; limit (default 10).
  • action='details' — LIVE licence/gated/file-list for one repo (re-fetches \
current truth; the index may be stale). Params: repo (required, e.g. \
'fairchem/OMAT24'); kind (default auto).
  • action='pull' — anonymous download of a repo's files to a local cache \
(default ~/.prism/hf_cache/<repo>). Refuses gated repos BEFORE downloading. \
Params: repo (required); kind; target; max_files (default 50); \
max_bytes_mb (default 500).

Honest reporting: a repo whose licence cannot be determined (HF 'other', or \
anonymous-access-blocked) is reported license_commercial=null — never guessed. \
A gated/anonymous-blocked repo says so; PRISM offers no command to authenticate.
"""


def create_hf_tools(registry: ToolRegistry) -> None:
    """Register the anonymous HuggingFace pull tool. Never raises."""
    registry.register(Tool(
        name="hf",
        description=_HF_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "details", "pull"],
                    "description": "search=discover, details=live metadata, pull=download.",
                },
                "query": {
                    "type": "string",
                    "description": "Search query (action='search'). Use natural language: "
                    "'interatomic potential for inorganic crystals', 'DFT training data'.",
                },
                "repo": {
                    "type": "string",
                    "description": "Hub repo id 'org/name' (action='details'/'pull'), e.g. "
                    "'fairchem/OMAT24'.",
                },
                "kind": {
                    "type": "string",
                    "enum": ["model", "dataset", "auto"],
                    "default": "auto",
                    "description": "Whether the repo is a model or dataset. 'auto' tries both.",
                },
                "use_index": {
                    "type": "boolean",
                    "default": True,
                    "description": "Blend the curated materials index into search results.",
                },
                "limit": {"type": "integer", "default": 10, "description": "Max search results."},
                "target": {
                    "type": "string",
                    "description": "Local directory for action='pull' (default ~/.prism/hf_cache/<repo>).",
                },
                "max_files": {"type": "integer", "default": 50, "description": "Pull file-count cap."},
                "max_bytes_mb": {"type": "integer", "default": 500, "description": "Pull byte cap (MB)."},
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_hf,
        # Pure network reads against a public registry; no destructive op.
        # 'pull' writes only under its resolved download root -- ENFORCED by
        # `_safe_dest`, not merely asserted (a reviewer escaped the old
        # unvalidated join) -- so it stays off the approval gate, same posture
        # as web/browser tools. Note `target` is still caller-chosen; the
        # guarantee is containment within whatever root is given.
        requires_approval=False,
        examples=[
            {
                "input": {"action": "search", "query": "MACE interatomic potential for inorganic crystals"},
                "output": {
                    "query": "MACE interatomic potential for inorganic crystals",
                    "results": [
                        {
                            "repo": "ACEtools/mace-mp-0", "kind": "model",
                            "source": "curated_index",
                            "license": None, "license_commercial": None,
                            "gated": "anonymous-blocked",
                        }
                    ],
                },
            },
            {
                "input": {"action": "details", "repo": "fairchem/OMAT24", "kind": "dataset"},
                "output": {
                    "repo": "fairchem/OMAT24", "kind": "dataset",
                    "license": "cc-by-4.0", "license_commercial": True, "gated": False,
                },
            },
        ],
    ))
