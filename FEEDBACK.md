# Feedback and System Review

Based on my investigation of the `PRISM` codebase (v2.5.0), here are the immediate issues and necessary improvements needed:

## Broken Tests
The Python test suite currently fails significantly (`make test`).
There are multiple underlying causes for these failures:

1. **Deleted `app.cli.tui` Module**:
   The application's `app.cli.tui` module was completely removed, as mentioned in the `CHANGELOG.md` for version 2.5.0 (`Rich REPL dropped`). However, many obsolete tests that assert its old behavior are still present in the `tests/` directory (e.g., `test_repl_cards.py`, `test_repl_skills.py`, `test_stream_refactor.py`).
2. **Improper `pycalphad` Mocking**:
   The `test_calphad_bridge.py`, `test_calphad_integration.py`, and `test_calphad_tools.py` fail because they try to mock an `importlib` reference that doesn't exist on `app.simulation.calphad_bridge`. Because the environment no longer has `pycalphad` installed, these mocked availability checks must be rewritten.
3. **Broken Prompt Assertions**:
   `test_north_star.py` asserts the existence of variables (like `DEFAULT_SYSTEM_PROMPT` in `app.agent.core`) that were recently moved or renamed in `app/agent/prompts.py` (`INTERACTIVE_SYSTEM_PROMPT`).

## System Build and Dependencies
1. The **`issue-agent.sh`** tool used to perform automated code fixes has a flawed verification step. By default, it skips `make test` and only runs `cargo check`, which means any automated PRs will incorrectly pass the automated checks even if the Python tests are horribly broken. **I have provided a patch to ensure `make test` runs before `cargo check`.**
2. The `cargo build` process (and by extension `prism report` command) requires the system to have `libcurl4-openssl-dev` installed to successfully compile the `rdkafka-sys` library dependency. Without it, the build fails abruptly.

## Recommendations
I have documented the issues in the `issues/` directory within this PR. These should be converted into GitHub Issues by the maintainer or resolved in subsequent commits. It's imperative that the test suite is refactored to remove the obsolete TUI tests so that CI integration can pass reliably.