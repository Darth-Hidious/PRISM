#!/usr/bin/env bash
# PRISM Issue Agent — runs locally via Claude Code
#
# Polls GitHub for new issues, triages them, attempts fixes, and asks
# reporters to verify. Runs on YOUR machine so you see everything live.
#
# Usage:
#   ./scripts/issue-agent.sh              # Process all open untriaged issues
#   ./scripts/issue-agent.sh --watch      # Poll every 5 minutes
#   ./scripts/issue-agent.sh --issue 7    # Process a specific issue
#
# Requirements:
#   - gh CLI authenticated
#   - claude CLI installed
#   - ANTHROPIC_API_KEY set

set -euo pipefail

REPO="Darth-Hidious/PRISM"
POLL_INTERVAL=300  # 5 minutes
NOTIFY_EMAIL="${NOTIFY_EMAIL:-}"

# ── Helpers ─────────────────────────────────────────────────────

log() { printf '\033[1;36m[issue-agent]\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m[issue-agent]\033[0m %s\n' "$1"; }
err() { printf '\033[1;31m[issue-agent]\033[0m %s\n' "$1" >&2; }

check_deps() {
    command -v gh >/dev/null || { err "gh CLI not found"; exit 1; }
    command -v claude >/dev/null || { err "claude CLI not found"; exit 1; }
    [ -n "${ANTHROPIC_API_KEY:-}" ] || { err "ANTHROPIC_API_KEY not set"; exit 1; }
}

# ── Process a single issue ──────────────────────────────────────

process_issue() {
    local issue_num="$1"

    log "Processing issue #${issue_num}..."

    # Fetch issue details safely
    local issue_json
    issue_json=$(gh issue view "$issue_num" --repo "$REPO" --json title,body,author,labels,state 2>&1) || {
        err "Failed to fetch issue #${issue_num}"
        return 1
    }

    local title body author state
    title=$(echo "$issue_json" | jq -r '.title')
    author=$(echo "$issue_json" | jq -r '.author.login')
    state=$(echo "$issue_json" | jq -r '.state')

    if [ "$state" != "OPEN" ]; then
        log "Issue #${issue_num} is ${state}, skipping"
        return 0
    fi

    # Check if already triaged
    local labels
    labels=$(echo "$issue_json" | jq -r '.labels[].name' 2>/dev/null || echo "")
    if echo "$labels" | grep -q "agent-triaged"; then
        log "Issue #${issue_num} already triaged, skipping"
        return 0
    fi

    log "Issue #${issue_num}: '${title}' by ${author}"

    # Write issue context to temp files (safe — no shell interpolation)
    local tmpdir
    tmpdir=$(mktemp -d)
    echo "$issue_json" | jq -r '.title' > "${tmpdir}/title.txt"
    echo "$issue_json" | jq -r '.body // ""' > "${tmpdir}/body.txt"

    # Run Claude Code on this issue
    log "Running Claude Code agent..."
    cd "$(git rev-parse --show-toplevel)"

    claude -p "You are the PRISM Issue Agent. Process GitHub issue #${issue_num} by ${author}.

Read the issue title from ${tmpdir}/title.txt and body from ${tmpdir}/body.txt.

Tasks:
1. READ the issue title and body files first.

2. CATEGORIZE: bug, install, feature, docs, or question.

3. RENAME if the title is vague. Run:
   gh issue edit ${issue_num} --repo ${REPO} --title 'better title here'

4. LABEL the issue. Run:
   gh issue edit ${issue_num} --repo ${REPO} --add-label 'bug'
   gh issue edit ${issue_num} --repo ${REPO} --add-label 'agent-triaged'

5. ANALYZE the error — read the full body, find root cause.
   Check relevant files: pyproject.toml, install.sh, Cargo.toml, etc.

6. If it's a bug or install issue, ATTEMPT A FIX:
   - git checkout -b fix/issue-${issue_num}
   - Make changes
   - cargo check --workspace (must pass)
   - cargo clippy --workspace -- -D warnings (must pass)
   - git add and commit
   - git push origin fix/issue-${issue_num}
   - gh pr create --repo ${REPO} --title 'fix: description (#${issue_num})' --body 'Fixes #${issue_num}'

7. COMMENT on the issue with your analysis:
   gh issue comment ${issue_num} --repo ${REPO} --body 'your analysis here'
   Ask: 'Could you try again and let us know if this resolves your issue?'

8. After everything, switch back to main:
   git checkout main

Important:
- Read the actual error output carefully — don't guess
- The repo is a Rust workspace with Python tools
- install.sh is the installer script users run
- pyproject.toml has dependency groups: [all], [simulation], [full]
- Run cargo fmt before committing" \
    --allowedTools "Bash,Read,Write,Edit,Glob,Grep"

    local exit_code=$?

    # Cleanup
    rm -rf "$tmpdir"

    # Make sure we're back on main
    git checkout main 2>/dev/null || true

    if [ $exit_code -eq 0 ]; then
        log "Issue #${issue_num} processed successfully"
    else
        warn "Issue #${issue_num} agent exited with code ${exit_code}"
    fi

    # Email notification (if configured)
    if [ -n "$NOTIFY_EMAIL" ] && command -v mail >/dev/null; then
        echo "PRISM Issue Agent processed #${issue_num}: ${title}" | \
            mail -s "[PRISM] Issue #${issue_num} triaged" "$NOTIFY_EMAIL" 2>/dev/null || true
    fi

    return $exit_code
}

# ── Find untriaged issues ──────────────────────────────────────

find_untriaged() {
    gh issue list --repo "$REPO" --state open --json number,labels \
        --jq '.[] | select(.labels | map(.name) | index("agent-triaged") | not) | .number'
}

# ── Main ────────────────────────────────────────────────────────

check_deps

case "${1:-}" in
    --issue)
        [ -n "${2:-}" ] || { err "Usage: $0 --issue <number>"; exit 1; }
        process_issue "$2"
        ;;
    --watch)
        log "Watching for new issues every ${POLL_INTERVAL}s..."
        while true; do
            issues=$(find_untriaged 2>/dev/null || echo "")
            if [ -n "$issues" ]; then
                for num in $issues; do
                    process_issue "$num" || true
                    sleep 5  # brief pause between issues
                done
            else
                log "No untriaged issues"
            fi
            sleep "$POLL_INTERVAL"
        done
        ;;
    *)
        # Process all untriaged issues once
        issues=$(find_untriaged 2>/dev/null || echo "")
        if [ -z "$issues" ]; then
            log "No untriaged issues found"
            exit 0
        fi
        for num in $issues; do
            process_issue "$num" || true
        done
        ;;
esac
