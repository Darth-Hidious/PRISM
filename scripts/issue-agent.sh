#!/usr/bin/env bash
# PRISM Issue Agent — local fixer
#
# Picks up issues that GitHub Actions already triaged (labeled "needs-fix"),
# runs Claude Code locally to analyze and fix them.
#
# Usage:
#   ./scripts/issue-agent.sh              # Fix all needs-fix issues
#   ./scripts/issue-agent.sh --watch      # Poll every 5 minutes
#   ./scripts/issue-agent.sh --issue 7    # Fix a specific issue
#
# Flow:
#   1. GitHub Actions (issue-triage.yml) → categorizes, labels, renames, comments
#   2. This script → picks up "needs-fix" issues, runs Claude Code to fix them
#   3. Claude Code → creates branch, fixes code, opens PR, asks reporter to verify

set -euo pipefail

REPO="Darth-Hidious/PRISM"
POLL_INTERVAL=300

log() { printf '\033[1;36m[issue-agent]\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m[issue-agent]\033[0m %s\n' "$1"; }
err() { printf '\033[1;31m[issue-agent]\033[0m %s\n' "$1" >&2; }

check_deps() {
    command -v gh >/dev/null || { err "gh CLI not found"; exit 1; }
    command -v claude >/dev/null || { err "claude CLI not found"; exit 1; }
    [ -n "${ANTHROPIC_API_KEY:-}" ] || { err "ANTHROPIC_API_KEY not set"; exit 1; }
}

process_issue() {
    local issue_num="$1"
    log "Fixing issue #${issue_num}..."

    # Fetch issue safely
    local issue_json
    issue_json=$(gh issue view "$issue_num" --repo "$REPO" --json title,body,author,labels,state 2>&1) || {
        err "Failed to fetch issue #${issue_num}"
        return 1
    }

    local title author state
    title=$(echo "$issue_json" | jq -r '.title')
    author=$(echo "$issue_json" | jq -r '.author.login')
    state=$(echo "$issue_json" | jq -r '.state')

    [ "$state" = "OPEN" ] || { log "#${issue_num} is ${state}, skipping"; return 0; }

    log "#${issue_num}: '${title}' by ${author}"

    # Write to temp files
    local tmpdir
    tmpdir=$(mktemp -d)
    echo "$issue_json" | jq -r '.title' > "${tmpdir}/title.txt"
    echo "$issue_json" | jq -r '.body // ""' > "${tmpdir}/body.txt"
    echo "$issue_json" | jq -r '[.labels[].name] | join(", ")' > "${tmpdir}/labels.txt"

    # Make sure we're on main and clean
    cd "$(git rev-parse --show-toplevel)"
    git checkout main 2>/dev/null
    git pull origin main --rebase 2>/dev/null || true

    # Run Claude Code
    log "Running Claude Code..."
    claude -p "You are fixing PRISM issue #${issue_num} by ${author}.

Read: ${tmpdir}/title.txt (title), ${tmpdir}/body.txt (error details), ${tmpdir}/labels.txt (category).

This issue was already triaged. Your job is to FIX it.

Steps:
1. Read the issue files to understand the problem.
2. Find the root cause in the codebase.
3. Create a fix branch:
   git checkout -b fix/issue-${issue_num}
4. Make the code changes.
5. Verify:
   cargo check --workspace
   cargo clippy --workspace -- -D warnings
   cargo fmt --all --check
6. Commit:
   git add -A
   git commit -m 'fix: description (#${issue_num})'
7. Push:
   git push origin fix/issue-${issue_num}
8. Open PR:
   gh pr create --repo ${REPO} --title 'fix: description (#${issue_num})' --body 'Fixes #${issue_num}

   Root cause: ...
   Fix: ...'
9. Comment on the issue asking the reporter to verify:
   gh issue comment ${issue_num} --repo ${REPO} --body 'Fix submitted in PR #... — could you try the fix branch and confirm it works?'
10. Remove the needs-fix label:
    gh issue edit ${issue_num} --repo ${REPO} --remove-label 'needs-fix' --add-label 'fix-submitted'
11. Switch back: git checkout main

Key files:
- pyproject.toml — Python dependencies
- install.sh — installer script
- Cargo.toml — Rust workspace
- crates/*/src/ — Rust source
- app/tools/ — Python tools
- frontend/src/ — Ink TUI" \
    --allowedTools "Bash,Read,Write,Edit,Glob,Grep"

    local exit_code=$?
    rm -rf "$tmpdir"
    git checkout main 2>/dev/null || true

    [ $exit_code -eq 0 ] && log "#${issue_num} fixed" || warn "#${issue_num} agent exited ${exit_code}"
    return $exit_code
}

find_needs_fix() {
    gh issue list --repo "$REPO" --state open --label "needs-fix" --json number --jq '.[].number'
}

# ── Main ────────────────────────────────────────────────────────

check_deps

case "${1:-}" in
    --issue)
        [ -n "${2:-}" ] || { err "Usage: $0 --issue <number>"; exit 1; }
        process_issue "$2"
        ;;
    --watch)
        log "Watching for needs-fix issues every ${POLL_INTERVAL}s... (Ctrl+C to stop)"
        while true; do
            issues=$(find_needs_fix 2>/dev/null || echo "")
            if [ -n "$issues" ]; then
                for num in $issues; do
                    process_issue "$num" || true
                    sleep 5
                done
            fi
            sleep "$POLL_INTERVAL"
        done
        ;;
    *)
        issues=$(find_needs_fix 2>/dev/null || echo "")
        if [ -z "$issues" ]; then
            log "No needs-fix issues"
            exit 0
        fi
        for num in $issues; do
            process_issue "$num" || true
        done
        ;;
esac
