# Tests failing due to `pycalphad` mock errors

The integration tests for the CALPHAD simulation bridge are failing due to improper mocking of the `pycalphad` dependency when simulating an environment where it is not installed.

Specifically, the failures occur in:
- `tests/test_calphad_bridge.py`
- `tests/test_calphad_integration.py`
- `tests/test_calphad_tools.py`
- `tests/test_skill_phase_analysis.py`

These tests rely on patching `importlib.util.find_spec` or the `check_calphad_available` function itself. In the current implementation, mocking `find_spec` using `app.simulation.calphad_bridge.importlib` throws an `AttributeError` because `importlib` is not explicitly imported or exposed as an attribute in the module being tested.

**Resolution:**
To fix these failures, the test suite should directly mock `app.simulation.calphad_bridge.check_calphad_available` returning `False` (for integration tests) or correctly mock `builtins.__import__` to raise an `ImportError`.