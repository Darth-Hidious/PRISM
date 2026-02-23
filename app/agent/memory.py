"""Session and persistent memory for the agent."""
import json
import os
import uuid
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional


class SessionMemory:
    """Manages session state and persistence to disk."""

    def __init__(self, storage_dir: Optional[str] = None):
        self._data: Dict[str, Any] = {}
        self._history: List[Dict] = []
        self._session_id: Optional[str] = None
        self._storage_dir = Path(storage_dir) if storage_dir else Path.home() / ".prism" / "sessions"

    def set(self, key: str, value: Any) -> None:
        self._data[key] = value

    def get(self, key: str, default: Any = None) -> Any:
        return self._data.get(key, default)

    def set_history(self, history: List[Dict]) -> None:
        self._history = history

    def get_history(self) -> List[Dict]:
        return self._history

    def save(self) -> str:
        self._storage_dir.mkdir(parents=True, exist_ok=True)
        if not self._session_id:
            self._session_id = datetime.now().strftime("%Y%m%d_%H%M%S") + "_" + uuid.uuid4().hex[:8]
        filepath = self._storage_dir / f"{self._session_id}.json"
        payload = {"session_id": self._session_id, "timestamp": datetime.now().isoformat(), "data": self._data, "history": self._history}
        filepath.write_text(json.dumps(payload, indent=2, default=str))
        return self._session_id

    def load(self, session_id: str) -> None:
        filepath = self._storage_dir / f"{session_id}.json"
        payload = json.loads(filepath.read_text())
        self._session_id = payload["session_id"]
        self._data = payload.get("data", {})
        self._history = payload.get("history", [])

    def list_sessions(self) -> List[Dict]:
        if not self._storage_dir.exists():
            return []
        sessions = []
        for f in sorted(self._storage_dir.glob("*.json"), reverse=True):
            try:
                payload = json.loads(f.read_text())
                sessions.append({"session_id": payload["session_id"], "timestamp": payload.get("timestamp")})
            except (json.JSONDecodeError, KeyError):
                continue
        return sessions
