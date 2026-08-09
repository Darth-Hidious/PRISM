#!/usr/bin/env python3
# Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
"""Verify OWNERSHIP.yml against the actual repository.

The IP architecture (section 5.2) requires a manifest mapping every path to an
owner, licence, layer and permitted use, and section 9 step 9 calls it the
"technical evidence of legal boundaries". A manifest nobody checks stops being
evidence the first time someone adds a directory, so this asserts four things:

  1. Every entry names a declared owner and a declared licence.
  2. Every entry matches at least one tracked file -- no entries for paths that
     no longer exist, which is how a manifest quietly becomes fiction.
  3. Every tracked file is covered by exactly one entry -- no unowned code.
  4. Every declared licence points at a licence file that exists.

Exit 0 when all hold, 1 otherwise. Wire into CI.

Usage:  python3 scripts/check_ownership.py [--manifest OWNERSHIP.yml]
"""

from __future__ import annotations

import argparse
import fnmatch
import subprocess
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.exit("check_ownership: pyyaml is required (pip install pyyaml)")


def matches(path: str, pattern: str) -> bool:
    """Match a tracked path against a manifest pattern.

    `dir/**` covers everything beneath `dir/`. Everything else is an exact
    path or a literal glob.

    The manifest deliberately avoids bare `*.ext` globs at the repository
    root and lists root files one by one instead. A root glob would absorb
    every future root document into whatever owner it names, which is the
    drift this check exists to catch: the new file would pass silently
    instead of failing until someone assigns it.
    """
    if pattern.endswith("/**"):
        return path.startswith(pattern[:-2])
    return fnmatch.fnmatch(path, pattern)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default="OWNERSHIP.yml")
    args = ap.parse_args()

    manifest = Path(args.manifest)
    if not manifest.exists():
        print(f"FAIL: {manifest} not found", file=sys.stderr)
        return 1

    doc = yaml.safe_load(manifest.read_text())
    owners = doc.get("owners", {})
    licences = doc.get("licences", {})
    entries = doc.get("paths", [])

    # Tracked files PLUS untracked-but-not-ignored ones.
    #
    # Using bare `git ls-files` was a real bug: a new file is invisible to it
    # until it is committed, so this check passed while OWNERSHIP.yml itself
    # was still untracked and went red the instant it was added. `--others
    # --exclude-standard` brings in anything git would consider new and is not
    # covered by .gitignore, so an unowned file fails BEFORE it lands rather
    # than after.
    files = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    tracked = sorted(set(files))

    failures: list[str] = []

    # 1. declared owner and licence
    for e in entries:
        if e.get("owner") not in owners:
            failures.append(f"{e.get('path')}: undeclared owner {e.get('owner')!r}")
        if e.get("licence") not in licences:
            failures.append(f"{e.get('path')}: undeclared licence {e.get('licence')!r}")

    # 2. no dead entries
    for e in entries:
        pat = e["path"]
        if not any(matches(f, pat) for f in tracked):
            failures.append(f"{pat}: matches no tracked file (stale entry)")

    # 3. no unowned files
    patterns = [e["path"] for e in entries]
    uncovered = [f for f in tracked if not any(matches(f, p) for p in patterns)]
    for f in uncovered:
        failures.append(f"{f}: covered by no OWNERSHIP.yml entry")

    # 4. licence files exist
    for name, meta in licences.items():
        ref = meta.get("file")
        if ref and not Path(ref).exists():
            failures.append(f"licence {name}: file {ref} does not exist")

    if failures:
        print(f"OWNERSHIP.yml: {len(failures)} problem(s)\n", file=sys.stderr)
        for f in failures[:50]:
            print(f"  {f}", file=sys.stderr)
        if len(failures) > 50:
            print(f"  ... and {len(failures) - 50} more", file=sys.stderr)
        return 1

    print(
        f"OWNERSHIP.yml OK: {len(entries)} entries cover {len(tracked)} tracked files; "
        f"{len(owners)} owners, {len(licences)} licences"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
