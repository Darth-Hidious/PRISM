"""Scratchpad: append-only execution log for the agent."""

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Dict, List, Optional


@dataclass
class ScratchpadEntry:
    """A single entry in the scratchpad."""
    timestamp: str
    step_type: str
    tool_name: Optional[str]
    summary: str
    data: Optional[Dict[str, Any]] = None


class Scratchpad:
    """Append-only log of agent actions, decisions, and findings.

    The agent writes an entry after every tool call automatically.
    Can be serialized to Markdown for reports or displayed in the REPL.
    """

    def __init__(self):
        self._entries: List[ScratchpadEntry] = []

    @property
    def entries(self) -> List[ScratchpadEntry]:
        return list(self._entries)

    def log(self, step_type: str, tool_name: Optional[str] = None,
            summary: str = "", data: Optional[Dict[str, Any]] = None) -> None:
        """Append an entry to the scratchpad."""
        entry = ScratchpadEntry(
            timestamp=datetime.now().isoformat(timespec="seconds"),
            step_type=step_type,
            tool_name=tool_name,
            summary=summary,
            data=data,
        )
        self._entries.append(entry)

    def to_markdown(self) -> str:
        """Render the scratchpad as a Markdown section."""
        if not self._entries:
            return "## Methodology\n\n*No actions recorded.*\n"
        lines = ["## Methodology", ""]
        for i, e in enumerate(self._entries, 1):
            tool_str = f" (`{e.tool_name}`)" if e.tool_name else ""
            lines.append(f"{i}. **{e.step_type}**{tool_str} — {e.summary}  ")
            lines.append(f"   *{e.timestamp}*")
        return "\n".join(lines)

    def to_dict(self) -> List[Dict[str, Any]]:
        """Serialize entries to a list of dicts (for JSON persistence)."""
        return [
            {
                "timestamp": e.timestamp,
                "step_type": e.step_type,
                "tool_name": e.tool_name,
                "summary": e.summary,
                "data": e.data,
            }
            for e in self._entries
        ]

    @classmethod
    def from_dict(cls, entries: List[Dict[str, Any]]) -> "Scratchpad":
        """Restore a Scratchpad from serialized entries."""
        pad = cls()
        for d in entries:
            pad._entries.append(ScratchpadEntry(
                timestamp=d.get("timestamp", ""),
                step_type=d.get("step_type", ""),
                tool_name=d.get("tool_name"),
                summary=d.get("summary", ""),
                data=d.get("data"),
            ))
        return pad

    def to_text(self) -> str:
        """Plain-text summary for the agent to read its own log."""
        if not self._entries:
            return "Scratchpad is empty."
        lines = []
        for i, e in enumerate(self._entries, 1):
            tool_str = f" ({e.tool_name})" if e.tool_name else ""
            lines.append(f"{i}. [{e.step_type}]{tool_str} {e.summary} @ {e.timestamp}")
        return "\n".join(lines)
