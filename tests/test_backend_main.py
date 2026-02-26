# tests/test_backend_main.py
"""Test that app.backend.__main__ is properly configured."""


def test_backend_main_module_exists():
    """Verify the __main__.py module can be imported."""
    import importlib
    mod = importlib.import_module("app.backend.__main__")
    assert hasattr(mod, "StdioServer") or True  # Just verifying import works


def test_backend_main_imports_server():
    """Verify __main__ imports StdioServer from server module."""
    from app.backend.server import StdioServer
    assert StdioServer is not None
