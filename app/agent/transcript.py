"""Transcript management — rolling window with lazy compaction.

Inspired by claw-code's TranscriptStore pattern:
- Bounded conversation history (not unlimited)
- Lazy compaction — only triggered when exceeding threshold
- Turn budget enforcement (max turns + max tokens)
- Immutable session snapshots for persistence
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional
import time
import uuid


@dataclass
class TurnBudget:
    """Limits for a conversation session."""
    max_turns: int = 30
    max_input_tokens: int = 200_000
    compact_after_turns: int = 20
    warn_at_token_pct: float = 0.8  # warn when 80% of token budget used

    def exhausted(self, turns: int, input_tokens: int) -> bool:
        return turns >= self.max_turns or input_tokens >= self.max_input_tokens

    def should_compact(self, turns: int) -> bool:
        return turns >= self.compact_after_turns

    def should_warn(self, input_tokens: int) -> bool:
        return input_tokens >= int(self.max_input_tokens * self.warn_at_token_pct)


@dataclass
class CostEvent:
    """A single cost event in the audit trail."""
    label: str
    input_tokens: int
    output_tokens: int
    timestamp: float = field(default_factory=time.time)

    def __str__(self) -> str:
        return f"{self.label}:in={self.input_tokens},out={self.output_tokens}"


@dataclass
class CostTracker:
    """Append-only cost log — auditable, non-blocking.

    Tracks both cumulative totals and individual events.
    """
    total_input: int = 0
    total_output: int = 0
    events: list[CostEvent] = field(default_factory=list)

    def record(self, label: str, input_tokens: int, output_tokens: int) -> None:
        self.total_input += input_tokens
        self.total_output += output_tokens
        self.events.append(CostEvent(label, input_tokens, output_tokens))

    @property
    def total_tokens(self) -> int:
        return self.total_input + self.total_output

    def summary(self) -> str:
        return f"{self.total_input} in, {self.total_output} out ({len(self.events)} events)"


@dataclass
class TranscriptEntry:
    """A single entry in the conversation transcript."""
    role: str           # 'user', 'assistant', 'tool', 'system'
    content: str        # text content or tool result summary
    tool_name: Optional[str] = None
    tokens: int = 0     # approximate token count
    timestamp: float = field(default_factory=time.time)


class TranscriptStore:
    """Rolling-window transcript with lazy compaction.

    Maintains a bounded conversation history. When compact_after_turns
    is exceeded, older entries are summarized into a single system message.
    """

    def __init__(self, budget: TurnBudget | None = None):
        self.budget = budget or TurnBudget()
        self.entries: list[TranscriptEntry] = []
        self.turn_count: int = 0
        self.cost: CostTracker = CostTracker()
        self.session_id: str = uuid.uuid4().hex[:12]
        self._compacted: bool = False

    def append(self, entry: TranscriptEntry) -> None:
        """Add an entry to the transcript."""
        self.entries.append(entry)
        if entry.role in ('user', 'assistant'):
            self.turn_count += 1
        self._compacted = False

    def record_cost(self, label: str, input_tokens: int, output_tokens: int) -> None:
        """Record a cost event."""
        self.cost.record(label, input_tokens, output_tokens)

    def should_compact(self) -> bool:
        """Check if compaction should be triggered."""
        return self.budget.should_compact(self.turn_count) and not self._compacted

    def compact(self, keep_last: int = 6) -> str | None:
        """Compact older entries into a summary, keeping last N entries.

        Returns the summary text if compaction happened, None otherwise.
        """
        if len(self.entries) <= keep_last:
            return None

        # Split into old and recent
        old = self.entries[:-keep_last]
        recent = self.entries[-keep_last:]

        # Build summary of old entries
        tool_calls = [e for e in old if e.tool_name]
        user_msgs = [e for e in old if e.role == 'user']

        summary_parts = []
        if user_msgs:
            summary_parts.append(
                f"Previous topics: {', '.join(e.content[:50] for e in user_msgs[-3:])}"
            )
        if tool_calls:
            tool_names = list(dict.fromkeys(e.tool_name for e in tool_calls if e.tool_name))
            summary_parts.append(f"Tools used: {', '.join(tool_names)}")
        summary_parts.append(f"({len(old)} entries compacted)")

        summary = "\n".join(summary_parts)

        # Replace entries with summary + recent
        self.entries = [
            TranscriptEntry(
                role='system',
                content=f"[Conversation context compacted]\n{summary}",
                tokens=len(summary.split()),
            )
        ] + recent

        self._compacted = True
        return summary

    def budget_exhausted(self) -> bool:
        """Check if the turn/token budget is exceeded."""
        return self.budget.exhausted(self.turn_count, self.cost.total_input)

    def budget_warning(self) -> str | None:
        """Return a warning message if approaching budget limits."""
        if self.budget.should_warn(self.cost.total_input):
            pct = int(self.cost.total_input / self.budget.max_input_tokens * 100)
            return f"Token budget: {pct}% used ({self.cost.total_input:,} / {self.budget.max_input_tokens:,})"
        if self.turn_count >= self.budget.max_turns - 3:
            return f"Turn budget: {self.turn_count} / {self.budget.max_turns} turns used"
        return None

    def to_messages(self) -> list[dict]:
        """Convert transcript to message list for LLM API."""
        messages = []
        for e in self.entries:
            msg = {"role": e.role, "content": e.content}
            if e.tool_name:
                msg["tool_name"] = e.tool_name
            messages.append(msg)
        return messages

    def snapshot(self) -> SessionSnapshot:
        """Create an immutable snapshot for persistence."""
        return SessionSnapshot(
            session_id=self.session_id,
            turn_count=self.turn_count,
            entries=tuple(self.entries),
            cost_events=tuple(self.cost.events),
            total_input_tokens=self.cost.total_input,
            total_output_tokens=self.cost.total_output,
        )


@dataclass(frozen=True)
class SessionSnapshot:
    """Immutable session state for persistence — frozen, hashable, JSON-safe."""
    session_id: str
    turn_count: int
    entries: tuple[TranscriptEntry, ...]
    cost_events: tuple[CostEvent, ...]
    total_input_tokens: int
    total_output_tokens: int
