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

    def test_summary_from_first_user_message(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            history = [
                {"role": "user", "content": "Find silicon materials"},
                {"role": "assistant", "content": "Here are some results..."},
            ]
            mem.set_history(history)
            sid = mem.save()
            mem2 = SessionMemory(storage_dir=tmpdir)
            sessions = mem2.list_sessions()
            session = next(s for s in sessions if s["session_id"] == sid)
            assert session["summary"] == "Find silicon materials"
            assert session["message_count"] == 2

    def test_summary_truncated_to_80_chars(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            long_msg = "A" * 120
            mem.set_history([{"role": "user", "content": long_msg}])
            sid = mem.save()
            mem2 = SessionMemory(storage_dir=tmpdir)
            sessions = mem2.list_sessions()
            session = next(s for s in sessions if s["session_id"] == sid)
            assert len(session["summary"]) == 80

    def test_summary_empty_when_no_user_message(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            mem = SessionMemory(storage_dir=tmpdir)
            mem.set_history([{"role": "assistant", "content": "Hello"}])
            sid = mem.save()
            mem2 = SessionMemory(storage_dir=tmpdir)
            sessions = mem2.list_sessions()
            session = next(s for s in sessions if s["session_id"] == sid)
            assert session["summary"] == ""
