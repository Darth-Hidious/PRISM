"""Tests for agent session and persistent memory."""
import pytest
import tempfile
from app.agent.memory import SessionMemory


class TestSessionMemory:
    def test_save_and_load_session(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            mem.set("last_query", "Find silicon materials")
            mem.set("provider", "mp")
            session_id = mem.save()
            mem2 = SessionMemory(storage_dir=tmpdir)
            mem2.load(session_id)
            assert mem2.get("last_query") == "Find silicon materials"
            assert mem2.get("provider") == "mp"

    def test_get_missing_key_returns_default(self):
        mem = SessionMemory()
        assert mem.get("nonexistent") is None
        assert mem.get("nonexistent", "fallback") == "fallback"

    def test_save_and_load_history(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            history = [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hello"}]
            mem.set_history(history)
            session_id = mem.save()
            mem2 = SessionMemory(storage_dir=tmpdir)
            mem2.load(session_id)
            assert mem2.get_history() == history

    def test_list_sessions(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            mem.set("x", 1)
            mem.save()
            mem2 = SessionMemory(storage_dir=tmpdir)
            mem2.set("y", 2)
            mem2.save()
            mem3 = SessionMemory(storage_dir=tmpdir)
            sessions = mem3.list_sessions()
            assert len(sessions) >= 2
