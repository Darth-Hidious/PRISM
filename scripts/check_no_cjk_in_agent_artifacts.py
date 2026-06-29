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

# Allowlist: files that intentionally contain CJK
# These test that the sanitizer PRESERVES CJK/Unicode text.
ALLOWLIST: set[str] = {
    "crates/tui/tests/unit.rs",  # sanitizer tests use CJK/emoji to verify preservation
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
    """Check if a file should be scanned."""
    # Check allowlist using relative path
    try:
        rel = str(path.relative_to(PROJECT_ROOT))
    except ValueError:
        rel = str(path)
    if rel in ALLOWLIST:
        return False
    ext = path.suffix.lower()
    return ext in SCAN_EXTENSIONS


def main() -> int:
    """Scan all target files for CJK characters. Exit 1 if found."""
    violations: list[tuple[str, int, str, str]] = []

    # Scan individual files
    for name in SCAN_FILES:
        path = PROJECT_ROOT / name
        if path.is_file() and should_scan_file(path):
            findings = find_cjk_in_file(path)
            for line_num, line, cjk in findings:
                violations.append((str(path), line_num, line, cjk))

    # Scan directories
    for dir_name in SCAN_DIRS:
        dir_path = PROJECT_ROOT / dir_name
        if not dir_path.is_dir():
            continue
        for file_path in dir_path.rglob("*"):
            if file_path.is_dir():
                if should_skip_dir(file_path):
                    continue
                continue
            # Skip files in skipped directories
            if any(part in SKIP_DIRS for part in file_path.parts):
                continue
            if should_scan_file(file_path):
                findings = find_cjk_in_file(file_path)
                for line_num, line, cjk in findings:
                    violations.append((str(file_path), line_num, line, cjk))

    # Report
    if violations:
        print(f"FAIL: {len(violations)} CJK character(s) found in agent artifacts\n")
        for file_path, line_num, line, cjk in violations:
            rel = Path(file_path).relative_to(PROJECT_ROOT)
            print(f"  {rel}:{line_num}: CJK chars: {cjk!r}")
            print(f"    line: {line}")
            print()
        print("If CJK is intentional, add the file to ALLOWLIST in this script.")
        return 1

    print("OK: no CJK language drift detected in agent artifacts")
    return 0


if __name__ == "__main__":
    sys.exit(main())