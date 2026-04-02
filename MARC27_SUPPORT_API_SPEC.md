# MARC27 Support Tickets API — Spec for marc27-core

**From:** PRISM agent
**To:** marc27-core agent
**Date:** 2026-04-02
**Context:** PRISM v2.5.0 has a `prism report` command that files bug reports. It needs platform API endpoints.

---

## Endpoints Needed

### 1. POST `/api/v1/support/tickets`

Create a support ticket from a PRISM node's bug report.

**Auth:** Bearer JWT (user must be logged in via `prism login`)

**Request:**
```json
{
  "type": "bug_report",
  "description": "Install fails on Ubuntu 22.04",
  "prism_version": "2.5.0",
  "python_version": "Python 3.13.12",
  "os": "docker, ollama, python (x86_64)",
  "cpu_cores": 12,
  "ram_gb": 24,
  "docker": true,
  "log": "... truncated error output ...",
  "user_id": "044d5402-...",
  "project_id": "00000000-..."
}
```

**Response (201):**
```json
{
  "ticket_id": "TKT-00042",
  "status": "open",
  "created_at": "2026-04-02T...",
  "github_issue_url": "https://github.com/Darth-Hidious/PRISM/issues/8"
}
```

**Behavior:**
1. Store ticket in Postgres (user_id, project_id, description, system_info, log, status)
2. If the ticket has a matching GitHub issue (same description), link them
3. Return ticket_id for dashboard display

---

### 2. GET `/api/v1/support/tickets`

List user's support tickets.

**Auth:** Bearer JWT
**Query params:** `?status=open&limit=20`

**Response:**
```json
{
  "tickets": [
    {
      "ticket_id": "TKT-00042",
      "type": "bug_report",
      "description": "Install fails on Ubuntu 22.04",
      "status": "open",
      "github_issue_url": "https://github.com/...",
      "created_at": "...",
      "updated_at": "...",
      "resolution": null
    }
  ],
  "count": 1
}
```

---

### 3. PATCH `/api/v1/support/tickets/{ticket_id}`

Update ticket status (used by the issue agent when a fix is submitted).

**Auth:** Bearer JWT (admin or ticket owner)

**Request:**
```json
{
  "status": "fix_submitted",
  "resolution": "Fixed in PR #12 — pyiron-atomistics removed from default [all] deps",
  "github_pr_url": "https://github.com/Darth-Hidious/PRISM/pull/12"
}
```

**Status values:** `open`, `triaged`, `fix_submitted`, `resolved`, `closed`, `wontfix`

---

### 4. Dashboard Page

**URL:** `https://platform.marc27.com/dashboard/support`

Shows:
- User's open tickets with status badges
- Resolution details when a fix is submitted
- Link to GitHub issue/PR
- "Was this resolved?" button that the user clicks to confirm

---

## Integration Flow

```
User hits error → prism report "description" --log-file error.log
  ↓
PRISM CLI:
  1. Captures system context (version, OS, Python, CPU, RAM, Docker)
  2. Files GitHub issue (gh issue create)
  3. Sends to MARC27 platform (POST /support/tickets)
  ↓
GitHub Actions (issue-triage.yml):
  4. Categorizes, labels, renames, comments with suggestions
  ↓
Local Claude Code agent (scripts/issue-agent.sh):
  5. Picks up needs-fix issue
  6. Analyzes, fixes, opens PR
  7. Comments asking reporter to verify
  ↓
Issue Agent also:
  8. Updates MARC27 ticket: PATCH /support/tickets/{id} with fix details
  ↓
User's MARC27 Dashboard:
  9. Shows ticket with status update + resolution
  10. User clicks "Resolved" → closes ticket + GitHub issue
```

---

## Database Schema (Postgres)

```sql
CREATE TABLE support_tickets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ticket_id TEXT UNIQUE NOT NULL,  -- TKT-00001 format
    user_id UUID NOT NULL REFERENCES users(id),
    project_id UUID REFERENCES projects(id),
    type TEXT NOT NULL DEFAULT 'bug_report',
    description TEXT NOT NULL,
    system_info JSONB,  -- {prism_version, python_version, os, cpu, ram, docker}
    log TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    resolution TEXT,
    github_issue_url TEXT,
    github_pr_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tickets_user ON support_tickets(user_id);
CREATE INDEX idx_tickets_status ON support_tickets(status);
```
