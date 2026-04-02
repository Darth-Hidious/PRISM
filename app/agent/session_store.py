"""Session persistence — JSONL files with resume, fork, and rotation.

Sessions are stored as newline-delimited JSON at ~/.prism/sessions/.
Each line is a message or metadata event. Sessions can be:
- Resumed: `prism` auto-loads last session, or `/session resume <id>`
- Forked: `/session fork` branches the current conversation
- Listed: `/session list` shows all saved sessions
- Rotated: files rotate at 256KB (max 3 backups)
"""

from __future__ import annotations

import json
import os
import time
import uuid
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional


SESSIONS_DIR = Path.home() / ".prism" / "sessions"
MAX_FILE_SIZE = 256 * 1024  # 256KB
MAX_ROTATIONS = 3
LATEST_REF = "latest"


@dataclass
class SessionMeta:
    """Session metadata — written as first line of JSONL file."""
    session_id: str
    created_at: float
    updated_at: float = 0.0
    model: str = ""
    turn_count: int = 0
    compaction_count: int = 0
    parent_session_id: Optional[str] = None
    branch_name: Optional[str] = None


@dataclass
class SessionEntry:
    """A single entry in the session JSONL file."""
    type: str           # 'meta', 'message', 'tool_call', 'tool_result', 'system', 'compaction'
    role: str = ""      # 'user', 'assistant', 'tool', 'system'
    content: str = ""
    tool_name: str = ""
    call_id: str = ""
    timestamp: float = field(default_factory=time.time)
    data: Optional[dict] = None  # extra structured data


class SessionStore:
    """Manages session persistence to JSONL files."""

    def __init__(self, sessions_dir: Path | None = None):
        self.sessions_dir = sessions_dir or SESSIONS_DIR
        self.sessions_dir.mkdir(parents=True, exist_ok=True)
        self._current_id: str | None = None
        self._current_path: Path | None = None
        self._meta: SessionMeta | None = None

    def new_session(self, model: str = "") -> str:
        """Create a new session. Returns session_id."""
        sid = f"{time.strftime('%Y%m%d_%H%M%S')}_{uuid.uuid4().hex[:8]}"
        self._current_id = sid
        self._current_path = self.sessions_dir / f"{sid}.jsonl"
        self._meta = SessionMeta(
            session_id=sid,
            created_at=time.time(),
            updated_at=time.time(),
            model=model,
        )
        self._write_entry(SessionEntry(type='meta', data=asdict(self._meta)))
        self._update_latest(sid)
        return sid

    def resume_session(self, ref: str = LATEST_REF) -> tuple[str, list[dict]] | None:
        """Resume a session by ID or 'latest'. Returns (session_id, messages) or None."""
        sid = self._resolve_ref(ref)
        if sid is None:
            return None

        path = self.sessions_dir / f"{sid}.jsonl"
        if not path.exists():
            return None

        messages = []
        meta = None
        for line in path.read_text().splitlines():
            if not line.strip():
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue

            if entry.get("type") == "meta":
                meta = entry.get("data", {})
                continue

            role = entry.get("role", "")
            content = entry.get("content", "")
            if role and content:
                msg = {"role": role, "content": content}
                if entry.get("call_id"):
                    msg["tool_call_id"] = entry["call_id"]
                if entry.get("tool_name"):
                    msg["tool_name"] = entry["tool_name"]
                if entry.get("data"):
                    msg.update(entry["data"])
                messages.append(msg)

        self._current_id = sid
        self._current_path = path
        if meta:
            self._meta = SessionMeta(**{k: v for k, v in meta.items() if k in SessionMeta.__dataclass_fields__})
        self._update_latest(sid)
        return sid, messages

    def fork_session(self, branch_name: str = "") -> str:
        """Fork current session into a new one. Returns new session_id."""
        old_id = self._current_id
        old_path = self._current_path

        # Create new session
        new_id = self.new_session(model=self._meta.model if self._meta else "")
        if self._meta:
            self._meta.parent_session_id = old_id
            self._meta.branch_name = branch_name or f"fork-{new_id[:8]}"

        # Copy messages from old session
        if old_path and old_path.exists():
            for line in old_path.read_text().splitlines():
                if not line.strip():
                    continue
                try:
                    entry = json.loads(line)
                    if entry.get("type") != "meta":
                        self._write_raw(line)
                except json.JSONDecodeError:
                    continue

        return new_id

    def append_message(self, role: str, content: str, **kwargs) -> None:
        """Append a message to the current session."""
        if not self._current_path:
            return
        entry = SessionEntry(
            type='message',
            role=role,
            content=content,
            tool_name=kwargs.get("tool_name", ""),
            call_id=kwargs.get("call_id", ""),
            data=kwargs.get("data"),
        )
        self._write_entry(entry)
        if self._meta:
            self._meta.updated_at = time.time()
            if role in ('user', 'assistant'):
                self._meta.turn_count += 1

    def append_compaction(self, summary: str) -> None:
        """Record a compaction event."""
        entry = SessionEntry(type='compaction', content=summary)
        self._write_entry(entry)
        if self._meta:
            self._meta.compaction_count += 1

    def list_sessions(self, limit: int = 20) -> list[dict]:
        """List available sessions, most recent first."""
        sessions = []
        for path in sorted(self.sessions_dir.glob("*.jsonl"), reverse=True):
            if len(sessions) >= limit:
                break
            first_line = path.open().readline().strip()
            if not first_line:
                continue
            try:
                entry = json.loads(first_line)
                meta = entry.get("data", {})
                sessions.append({
                    "session_id": path.stem,
                    "created_at": meta.get("created_at", 0),
                    "turn_count": meta.get("turn_count", 0),
                    "model": meta.get("model", ""),
                    "size_kb": path.stat().st_size / 1024,
                    "is_latest": path.stem == self._resolve_ref(LATEST_REF),
                })
            except (json.JSONDecodeError, OSError):
                continue
        return sessions

    @property
    def current_id(self) -> str | None:
        return self._current_id

    @property
    def meta(self) -> SessionMeta | None:
        return self._meta

    # ── Internal ────────────────────────────────────────────────────

    def _write_entry(self, entry: SessionEntry) -> None:
        if not self._current_path:
            return
        self._maybe_rotate()
        line = json.dumps(asdict(entry), separators=(",", ":"))
        with self._current_path.open("a") as f:
            f.write(line + "\n")

    def _write_raw(self, line: str) -> None:
        if not self._current_path:
            return
        with self._current_path.open("a") as f:
            f.write(line.rstrip() + "\n")

    def _maybe_rotate(self) -> None:
        if not self._current_path or not self._current_path.exists():
            return
        if self._current_path.stat().st_size < MAX_FILE_SIZE:
            return
        # Rotate: .jsonl → .1.jsonl → .2.jsonl → delete .3.jsonl
        for i in range(MAX_ROTATIONS, 0, -1):
            src = self._current_path.with_suffix(f".{i}.jsonl") if i > 0 else self._current_path
            dst = self._current_path.with_suffix(f".{i + 1}.jsonl")
            if i == MAX_ROTATIONS:
                src.unlink(missing_ok=True)
            elif src.exists():
                src.rename(dst)

    def _resolve_ref(self, ref: str) -> str | None:
        if ref == LATEST_REF:
            latest_path = self.sessions_dir / ".latest"
            if latest_path.exists():
                return latest_path.read_text().strip()
            # Fallback: most recent file
            files = sorted(self.sessions_dir.glob("*.jsonl"), reverse=True)
            return files[0].stem if files else None
        return ref

    def _update_latest(self, sid: str) -> None:
        latest_path = self.sessions_dir / ".latest"
        latest_path.write_text(sid)
