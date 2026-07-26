# Copyright (c) 2025-2026 MARC27. Licensed under MIT License.
"""ONE provenance format for every PRISM tool result that reports a number.

This is not a new format. It is the bundle the MACE job runner already
writes (``app/tools/simulation/mace/jobs/provenance.py``) — same key names
(``tool_name`` / ``input`` / ``versions`` / ``host`` / ``created_at_iso8601``)
— lifted here so the CALPHAD, ML and pyiron tools emit the same thing, plus
the three PROV-O relations stated explicitly:

  wasGeneratedBy    the Activity: which engine, which version, which call
  wasDerivedFrom    the Entities consumed: TDB file, model file, dataset,
                    structure — each with a content hash where one exists
  wasAttributedTo   the Agent: PRISM itself, its version, the host

The bar this has to clear: given a number a PRISM tool produced, a scientist
must be able to say exactly how it was produced and re-run it. So every
bundle also carries ``units`` (per output key), ``units_policy`` (PRISM
never converts — it reports what the engine reported), and ``reproduce``
(the tool call that regenerates it).

Everything here is best-effort and MUST NOT raise: these run inside tool
calls. A value that cannot be determined is recorded as an explicit
``"unknown"`` / ``{"error": ...}`` — never omitted, never guessed.
"""

from __future__ import annotations

import hashlib
import platform
import socket
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable

from app import __version__ as PRISM_VERSION

#: Bumped when a computed number's meaning changes (new engine call, changed
#: normalisation). Stored in every bundle so a stale cached number is
#: identifiable rather than silently mixed with a fresh one.
PROVENANCE_SCHEMA_VERSION = "1"

UNITS_POLICY = (
    "verbatim — PRISM applies no unit conversion; values are exactly as the "
    "named engine reported them, and `units` records that engine's convention"
)


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def versions_of(*modules: str) -> dict[str, str]:
    """Installed version of each named module. Absent modules say so."""
    out: dict[str, str] = {
        "prism": PRISM_VERSION,
        "python": platform.python_version(),
    }
    for mod in modules:
        try:
            m = __import__(mod)
            out[mod] = str(getattr(m, "__version__", "unknown"))
        except Exception:
            out[mod] = "absent"
    return out


def collect_host() -> dict[str, str]:
    """Where the number was produced. Never raises."""
    info = {
        "platform": sys.platform,
        "machine": platform.machine(),
        "python_impl": platform.python_implementation(),
    }
    try:
        info["hostname"] = socket.gethostname()
    except Exception:
        info["hostname"] = "unknown"
    return info


def file_ref(path: str | Path | None, role: str) -> dict[str, Any]:
    """A content-addressed reference to an input file.

    The hash is what makes "re-run it" meaningful: a TDB or a .joblib can be
    edited in place, and then the same call gives a different answer. An
    unreadable file records the failure instead of dropping the input.
    """
    ref: dict[str, Any] = {"role": role, "path": str(path) if path else None}
    if not path:
        ref["error"] = "no path recorded"
        return ref
    p = Path(path)
    try:
        h = hashlib.sha256()
        with p.open("rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        stat = p.stat()
        ref["sha256"] = h.hexdigest()
        ref["size_bytes"] = stat.st_size
        ref["modified_at"] = datetime.fromtimestamp(
            stat.st_mtime, timezone.utc
        ).isoformat()
    except Exception as exc:
        ref["error"] = f"{type(exc).__name__}: {exc}"
    return ref


def build(
    *,
    tool_name: str,
    engine: str,
    engine_version: str,
    activity: str,
    inputs: dict[str, Any],
    units: dict[str, str],
    derived_from: Iterable[dict[str, Any]] = (),
    reproduce: str | None = None,
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the provenance bundle for one computed result.

    ``engine``/``engine_version``/``activity`` answer "what ran"; ``units``
    maps each numeric key of the result to its unit as the engine reports it
    (use ``"unknown"`` when it genuinely is — never guess a unit).
    """
    return {
        "schema_version": PROVENANCE_SCHEMA_VERSION,
        "tool_name": tool_name,
        "created_at_iso8601": utc_now_iso(),
        "input": inputs,
        "units": units,
        "units_policy": UNITS_POLICY,
        "versions": versions_of(engine.split(".")[0]) if engine else versions_of(),
        "host": collect_host(),
        "wasGeneratedBy": {
            "activity": activity,
            "engine": engine,
            "engine_version": engine_version,
        },
        "wasDerivedFrom": list(derived_from),
        "wasAttributedTo": {
            "agent": "PRISM",
            "agent_type": "SoftwareAgent",
            "prism_version": PRISM_VERSION,
            "host": collect_host().get("hostname", "unknown"),
        },
        "reproduce": reproduce,
        **(extra or {}),
    }


def attach(result: dict[str, Any], prov: dict[str, Any]) -> dict[str, Any]:
    """Attach ``prov`` to a successful result, in place.

    A result carrying ``error`` is left untouched: an error is a defect to
    surface, and dressing it in provenance would make a failure look like a
    measurement.
    """
    if isinstance(result, dict) and "error" not in result:
        result["provenance"] = prov
    return result
