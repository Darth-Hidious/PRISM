"""Tests for Scratchpad execution log (Phase F-3)."""
import pytest
from app.agent.scratchpad import Scratchpad, ScratchpadEntry


class TestScratchpad:
    def test_empty_scratchpad(self):
        pad = Scratchpad()
        assert pad.entries == []
        assert "empty" in pad.to_text().lower()

    def test_log_entry(self):
        pad = Scratchpad()
        pad.log("tool_call", tool_name="search_optimade", summary="5 results")
        assert len(pad.entries) == 1
        assert pad.entries[0].tool_name == "search_optimade"
        assert pad.entries[0].summary == "5 results"
        assert pad.entries[0].step_type == "tool_call"

    def test_multiple_entries(self):
        pad = Scratchpad()
        pad.log("tool_call", tool_name="search", summary="found 10")
        pad.log("tool_call", tool_name="predict", summary="predicted 10")
        pad.log("plan", summary="Plan approved")
        assert len(pad.entries) == 3

    def test_to_markdown(self):
        pad = Scratchpad()
        pad.log("tool_call", tool_name="search", summary="5 results")
        md = pad.to_markdown()
        assert "Methodology" in md
        assert "search" in md
        assert "5 results" in md

    def test_to_markdown_empty(self):
        pad = Scratchpad()
        md = pad.to_markdown()
        assert "No actions recorded" in md

    def test_to_dict_roundtrip(self):
        pad = Scratchpad()
        pad.log("tool_call", tool_name="search", summary="5 results", data={"count": 5})
        pad.log("plan", summary="Plan approved")

        d = pad.to_dict()
        assert len(d) == 2
        assert d[0]["tool_name"] == "search"
        assert d[0]["data"] == {"count": 5}

        restored = Scratchpad.from_dict(d)
        assert len(restored.entries) == 2
        assert restored.entries[0].tool_name == "search"
        assert restored.entries[1].step_type == "plan"

    def test_to_text(self):
        pad = Scratchpad()
        pad.log("tool_call", tool_name="search", summary="5 results")
        text = pad.to_text()
        assert "search" in text
        assert "5 results" in text

    def test_data_field_optional(self):
        pad = Scratchpad()
        pad.log("tool_call", tool_name="search", summary="done")
        assert pad.entries[0].data is None

    def test_entries_immutable_copy(self):
        pad = Scratchpad()
        pad.log("tool_call", summary="test")
        entries = pad.entries
        entries.clear()
        assert len(pad.entries) == 1  # Original not affected


class TestScratchpadEntry:
    def test_fields(self):
        entry = ScratchpadEntry(
            timestamp="2026-02-24T12:00:00",
            step_type="tool_call",
            tool_name="search",
            summary="Found 5",
            data={"count": 5},
        )
        assert entry.timestamp == "2026-02-24T12:00:00"
        assert entry.step_type == "tool_call"
        assert entry.tool_name == "search"
        assert entry.summary == "Found 5"
        assert entry.data == {"count": 5}
