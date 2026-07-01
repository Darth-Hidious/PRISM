#!/usr/bin/env python3
"""Check for accidental CJK/Chinese text in agent-created artifacts.

Scans docs, prompts, scripts, test files, and configuration for CJK
Unified Ideographs and common CJK character ranges.  This catches
language drift where an AI agent accidentally writes Chinese instead of
English.

Exit 0 if clean, exit 1 if CJK characters are found.

Usage:
    python scripts/check_no_cjk_in_agent_artifacts.py
"""

import sys
import re
from pathlib import Path

# ── Configuration ───────────────────────────────────────────────────

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# Directories to scan
SCAN_DIRS = [
    "docs",
    "prompts",
    "scripts",
    "crates/tui/tests",
    "tests",
]

# Individual files to scan
SCAN_FILES = [
    "AGENTS.md",
    "PRISM_TUI_VERIFY.md",
]

# Directories to skip (never scan)
SKIP_DIRS = {
    "target", ".git", "node_modules", ".venv", "__pycache__",
    ".pytest_cache", ".codex", ".claude", ".codebase-memory",
    ".superpowers", ".mcp-tools", ".forge", ".prism",
    "prism_platform.egg-info",
}

# File extensions to scan (text files only)
SCAN_EXTENSIONS = {
    ".md", ".py", ".sh", ".rs", ".toml", ".yaml", ".yml",
    ".json", ".jsonc", ".txt", ".cfg",
}

# CJK Unicode ranges to detect
# Reference: https://en.wikipedia.org/wiki/CJK_Unified_Ideographs
CJK_RANGES = [
    (0x3400, 0x4DBF),    # CJK Unified Ideographs Extension A
    (0x4E00, 0x9FFF),    # CJK Unified Ideographs
    (0xA000, 0xA48F),    # Yi Syllables
    (0xA490, 0xA4CF),    # Yi Radicals
    (0xF900, 0xFAFF),    # CJK Compatibility Ideographs
    (0x20000, 0x2A6DF),  # CJK Unified Ideographs Extension B
    (0x2A700, 0x2B73F),  # CJK Unified Ideographs Extension C
    (0x2B740, 0x2B81F),  # CJK Unified Ideographs Extension D
    (0x2B820, 0x2CEAF),  # CJK Unified Ideographs Extension E
    (0x2CEB0, 0x2EBEF),  # CJK Unified Ideographs Extension F
]

# Allowlist: files that intentionally contain CJK.
# Instead of allowlisting whole files (which hides accidental drift),
# we allowlist specific (file, line_content_substring) pairs.
# A CJK finding is suppressed only if the offending line contains
# the allowlisted substring — so a new Chinese comment in the same
# file is still caught.
#
# Format: { "relative/path": ["substring_on_line_1", "substring_on_line_2"] }
# The substring must be a safe ASCII fragment of the line that
# uniquely identifies the intentional CJK test line.
ALLOWLIST_LINES: dict[str, list[str]] = {
    "crates/tui/tests/unit.rs": [
        # Sanitizer/Unicode tests that verify CJK text is preserved.
        # These substrings uniquely identify the intentional test
        # lines. Accidental Chinese in comments or prose on other
        # lines is still caught.
        "café",     # line with "café" + CJK + emoji
        "x1b[32m",  # ANSI escape + CJK mixed test line
        "append_assistant_text",  # streaming Unicode preservation test
        "sanitize_for_render(input), \"Ti",  # sanitizer assertion
        "last.text, \"Ti",       # App text assertion with CJK
    ],
    "crates/tui/tests/render_snapshots.rs": [
        # Unicode preservation snapshot test:
        "café",
    ],
}

# ── Implementation ──────────────────────────────────────────────────


def is_cjk_char(char: str) -> bool:
    """Check if a character is in any CJK Unicode range."""
    cp = ord(char)
    for low, high in CJK_RANGES:
        if low <= cp <= high:
            return True
    return False


def find_cjk_in_file(path: Path) -> list[tuple[int, str, str]]:
    """Find lines with CJK characters in a file.

    Returns list of (line_number, line_content, cjk_chars_found).
    """
    try:
        text = path.read_text(encoding="utf-8", errors="ignore")
    except Exception:
        return []

    findings = []
    for line_num, line in enumerate(text.splitlines(), 1):
        cjk_chars = [c for c in line if is_cjk_char(c)]
        if cjk_chars:
            findings.append((line_num, line.strip()[:80], "".join(cjk_chars)))
    return findings


def should_skip_dir(d: Path) -> bool:
    """Check if a directory should be skipped."""
    return d.name in SKIP_DIRS


def should_scan_file(path: Path) -> bool:
    """Check if a file should be scanned (by extension)."""
    ext = path.suffix.lower()
    return ext in SCAN_EXTENSIONS


def is_allowlisted_line(rel_path: str, line: str) -> bool:
    """Check if a specific line in a file is allowlisted for CJK.

    Uses ALLOWLIST_LINES: { "relative/path": ["substring1", "substring2"] }.
    A line is allowlisted if it contains any of the allowlisted substrings
    for that file.  This narrows the exception to specific test lines
    instead of entire files.
    """
    substrings = ALLOWLIST_LINES.get(rel_path)
    if not substrings:
        return False
    return any(sub in line for sub in substrings)


def main() -> int:
    """Scan all target files for CJK characters. Exit 1 if found."""
    violations: list[tuple[str, int, str, str]] = []

    def check_file(path: Path) -> None:
        """Scan a single file and append violations."""
        if not should_scan_file(path):
            return
        try:
            rel = str(path.relative_to(PROJECT_ROOT))
        except ValueError:
            rel = str(path)
        findings = find_cjk_in_file(path)
        for line_num, line, cjk in findings:
            if is_allowlisted_line(rel, line):
                continue
            violations.append((str(path), line_num, line, cjk))

    # Scan individual files
    for name in SCAN_FILES:
        path = PROJECT_ROOT / name
        if path.is_file():
            check_file(path)

    # Scan directories
    for dir_name in SCAN_DIRS:
        dir_path = PROJECT_ROOT / dir_name
        if not dir_path.is_dir():
            continue
        for file_path in dir_path.rglob("*"):
            if file_path.is_dir():
                continue
            # Skip files in skipped directories
            if any(part in SKIP_DIRS for part in file_path.parts):
                continue
            check_file(file_path)

    # Report
    if violations:
        print(f"FAIL: {len(violations)} CJK character(s) found in agent artifacts\n")
        for file_path, line_num, line, cjk in violations:
            rel = Path(file_path).relative_to(PROJECT_ROOT)
            print(f"  {rel}:{line_num}: CJK chars: {cjk!r}")
            print(f"    line: {line}")
            print()
        print("If CJK is intentional, add the line substring to ALLOWLIST_LINES.")
        return 1

    print("OK: no CJK language drift detected in agent artifacts")
    return 0


if __name__ == "__main__":
    sys.exit(main())