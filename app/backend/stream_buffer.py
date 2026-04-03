"""Markdown-aware stream buffer — holds text until safe to flush.

Prevents broken rendering when the LLM is mid-code-block or mid-table.
Flushes at safe boundaries: blank lines, closed code fences, list breaks.

Usage:
    buf = MarkdownStreamBuffer()
    for token in llm_stream:
        for chunk in buf.push(token):
            emit_to_frontend(chunk)  # safe to render
    for chunk in buf.flush():
        emit_to_frontend(chunk)  # final flush
"""


class MarkdownStreamBuffer:
    """Buffers streaming markdown text until safe rendering boundaries.

    State machine:
    - NORMAL: flush on blank lines or when buffer exceeds threshold
    - IN_CODE_FENCE: buffer everything until closing fence
    - IN_TABLE: buffer until blank line after table row
    """

    FLUSH_THRESHOLD = 200  # chars — flush normal text after this many

    def __init__(self):
        self._buffer = ""
        self._in_code_fence = False
        self._fence_marker = ""  # "```" or "~~~"
        self._in_table = False

    def push(self, text: str) -> list[str]:
        """Push new text. Returns list of safe-to-render chunks (may be empty)."""
        self._buffer += text
        return self._try_flush()

    def flush(self) -> list[str]:
        """Force-flush everything remaining (call at end of stream)."""
        if self._buffer:
            out = self._buffer
            self._buffer = ""
            self._in_code_fence = False
            self._in_table = False
            return [out]
        return []

    def _try_flush(self) -> list[str]:
        chunks = []

        while True:
            flushed = self._try_flush_once()
            if flushed is None:
                break
            chunks.append(flushed)

        return chunks

    def _try_flush_once(self) -> str | None:
        if not self._buffer:
            return None

        # State: inside a code fence — wait for closing fence
        if self._in_code_fence:
            close_pos = self._buffer.find(f"\n{self._fence_marker}")
            if close_pos == -1:
                # Also check if fence is at the very start (no leading newline)
                if self._buffer.startswith(self._fence_marker) and len(self._buffer) > len(self._fence_marker):
                    pass  # opening fence, not closing
                return None  # still waiting for close
            # Found closing fence — flush the entire code block
            end = self._buffer.find("\n", close_pos + 1)
            if end == -1:
                end = len(self._buffer)
            else:
                end += 1  # include the newline
            chunk = self._buffer[:end]
            self._buffer = self._buffer[end:]
            self._in_code_fence = False
            self._fence_marker = ""
            return chunk

        # Check for opening code fence
        for marker in ("```", "~~~"):
            fence_pos = self._buffer.find(marker)
            if fence_pos != -1:
                # Flush everything before the fence
                if fence_pos > 0:
                    chunk = self._buffer[:fence_pos]
                    self._buffer = self._buffer[fence_pos:]
                    return chunk
                # Start buffering the code block
                self._in_code_fence = True
                self._fence_marker = marker
                return None

        # State: inside a table — wait for blank line
        if self._in_table:
            blank_pos = self._buffer.find("\n\n")
            if blank_pos != -1:
                chunk = self._buffer[:blank_pos + 2]
                self._buffer = self._buffer[blank_pos + 2:]
                self._in_table = False
                return chunk
            return None

        # Check for table start (line with | characters)
        lines = self._buffer.split("\n")
        for i, line in enumerate(lines):
            stripped = line.strip()
            if stripped.startswith("|") and stripped.endswith("|") and stripped.count("|") >= 3:
                self._in_table = True
                # Flush everything before the table
                if i > 0:
                    before = "\n".join(lines[:i]) + "\n"
                    self._buffer = "\n".join(lines[i:])
                    return before
                return None

        # Normal text — flush at safe boundaries
        # Safe: blank line, or buffer exceeds threshold
        blank_pos = self._buffer.find("\n\n")
        if blank_pos != -1:
            chunk = self._buffer[:blank_pos + 2]
            self._buffer = self._buffer[blank_pos + 2:]
            return chunk

        # Threshold flush — find last newline and flush up to it
        if len(self._buffer) > self.FLUSH_THRESHOLD:
            last_nl = self._buffer.rfind("\n")
            if last_nl > 0:
                chunk = self._buffer[:last_nl + 1]
                self._buffer = self._buffer[last_nl + 1:]
                return chunk

        return None
