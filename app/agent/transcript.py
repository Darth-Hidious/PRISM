"""Transcript management — rolling window with lazy compaction.

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
        """Compact older entries into a structured summary, keeping last N.

        Produces a summary with: scope, tools used, recent requests,
        pending work (inferred), key files, timeline. Designed so the
        agent can resume without losing context.
        """
        if len(self.entries) <= keep_last:
            return None

        old = self.entries[:-keep_last]
        recent = self.entries[-keep_last:]

        # Gather data from old entries
        user_msgs = [e for e in old if e.role == 'user']
        assistant_msgs = [e for e in old if e.role == 'assistant']
        tool_calls = [e for e in old if e.tool_name]
        all_text = " ".join(e.content for e in old if e.content)

        # Build structured summary
        summary_parts = []

        # Scope
        summary_parts.append(
            f"Conversation summary ({len(old)} messages compacted: "
            f"{len(user_msgs)} user, {len(assistant_msgs)} assistant, "
            f"{len(tool_calls)} tool calls)"
        )

        # Tools used (deduplicated, preserving order)
        if tool_calls:
            tool_names = list(dict.fromkeys(e.tool_name for e in tool_calls if e.tool_name))
            summary_parts.append(f"Tools used: {', '.join(tool_names)}")

        # Recent user requests (last 3)
        if user_msgs:
            recent_topics = [e.content[:80].replace('\n', ' ') for e in user_msgs[-3:]]
            summary_parts.append(f"Recent requests: {' | '.join(recent_topics)}")

        # Pending work (infer from keywords)
        pending = _extract_pending_work(all_text)
        if pending:
            summary_parts.append(f"Pending work: {'; '.join(pending)}")

        # Key files (extract paths mentioned)
        files = _extract_key_files(all_text)
        if files:
            summary_parts.append(f"Key files: {', '.join(files)}")

        # Current state (last assistant message)
        if assistant_msgs:
            last = assistant_msgs[-1].content[:150].replace('\n', ' ')
            summary_parts.append(f"Last response: {last}")

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


# ── Compaction helpers ──────────────────────────────────────────────

_PENDING_KEYWORDS = {"todo", "next", "pending", "follow up", "remaining", "need to", "should", "will"}
_FILE_EXTENSIONS = {".rs", ".py", ".ts", ".tsx", ".js", ".json", ".yaml", ".yml", ".toml", ".md", ".csv"}


def _extract_pending_work(text: str, limit: int = 3) -> list[str]:
    """Infer pending work items from conversation text."""
    import re
    results = []
    # Match lines that START with a pending keyword or contain TODO/FIXME
    pattern = re.compile(
        r'(?:^|\.\s+)((?:todo|next|pending|remaining|need to|should|will)\b.{10,80})',
        re.IGNORECASE | re.MULTILINE,
    )
    for match in pattern.finditer(text):
        clean = match.group(1).strip().rstrip(".")
        if clean and clean not in results:
            results.append(clean)
            if len(results) >= limit:
                break
    return results


def _extract_key_files(text: str, limit: int = 8) -> list[str]:
    """Extract file paths mentioned in conversation text."""
    seen = set()
    results = []
    for word in text.split():
        # Must contain a path separator and a known extension
        if "/" not in word:
            continue
        # Clean up surrounding punctuation
        clean = word.strip("\"'`,;:()[]{}").rstrip(".")
        ext = "." + clean.rsplit(".", 1)[-1] if "." in clean else ""
        if ext in _FILE_EXTENSIONS and clean not in seen:
            # Normalize home dir
            if clean.startswith("/Users/"):
                parts = clean.split("/")
                if len(parts) > 3:
                    clean = "~/" + "/".join(parts[3:])
            seen.add(clean)
            results.append(clean)
            if len(results) >= limit:
                break
    return results
