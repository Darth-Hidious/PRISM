# Tests failing due to missing `app.cli.tui` module

Multiple python tests are currently failing because the `app.cli.tui` module was recently removed from the codebase, but the corresponding tests that explicitly reference or test this UI module have not been removed or updated.

The following test files contain obsolete tests that cause `make test` to fail:
- `tests/test_repl_cards.py`
- `tests/test_stream_refactor.py`
- `tests/test_repl_skills.py`
- `tests/test_skill_loading.py`
- `tests/test_autonomous.py`
- `tests/test_repl.py`
- `tests/test_cli.py`

**Resolution:**
These obsolete tests should be removed to ensure a clean passing test suite, since the TUI has been entirely removed from the application backend.