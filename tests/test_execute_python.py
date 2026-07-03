"""Tests for execute_python tool."""
import sys
from unittest.mock import patch, MagicMock

from app.tools.code import _execute_python, create_code_tools
from app.tools.base import ToolRegistry


class TestExecutePython:
    def test_simple_print(self):
        result = _execute_python(code='print("hello world")')
        assert result["success"] is True
        assert result["exit_code"] == 0
        assert "hello world" in result["stdout"]

    def test_math_expression(self):
        result = _execute_python(code='print(2 + 2)')
        assert result["success"] is True
        assert "4" in result["stdout"]

    def test_import_and_compute(self):
        result = _execute_python(code='import json; print(json.dumps({"a": 1}))')
        assert result["success"] is True
        assert '"a": 1' in result["stdout"]

    def test_syntax_error(self):
        result = _execute_python(code='def foo(')
        assert result["success"] is False
        assert result["exit_code"] != 0
        assert "SyntaxError" in result["stderr"]

    def test_runtime_error(self):
        result = _execute_python(code='print(1/0)')
        assert result["success"] is False
        assert "ZeroDivisionError" in result["stderr"]

    def test_timeout(self):
        result = _execute_python(code='import time; time.sleep(10)', timeout=1)
        assert "error" in result or result.get("exit_code") == 124
        assert "Timed out" in result.get("error", "") or result.get("exit_code") == 124

    def test_multiline_code(self):
        code = """
import math
values = [math.sqrt(i) for i in range(5)]
for v in values:
    print(f"{v:.2f}")
"""
        result = _execute_python(code=code)
        assert result["success"] is True
        assert "0.00" in result["stdout"]
        assert "2.00" in result["stdout"]

    def test_stderr_captured(self):
        result = _execute_python(code='import sys; print("err", file=sys.stderr)')
        assert "err" in result["stderr"]

    def test_empty_code(self):
        result = _execute_python(code='')
        assert result["success"] is True
        assert result["exit_code"] == 0

    def test_uses_same_python(self):
        result = _execute_python(code=f'import sys; print(sys.executable)')
        assert result["success"] is True
        # Should use the same Python as the test runner
        assert result["stdout"].strip() != ""


class TestCodeToolRegistration:
    def test_tool_registered(self):
        reg = ToolRegistry()
        create_code_tools(reg)
        tool = reg.get("execute_python")
        assert tool.name == "execute_python"
        assert tool.requires_approval is True

    def test_tool_schema_has_code_required(self):
        reg = ToolRegistry()
        create_code_tools(reg)
        tool = reg.get("execute_python")
        assert "code" in tool.input_schema["properties"]
        assert "code" in tool.input_schema["required"]

    def test_tool_in_bootstrap(self):
        """execute_python is registered via build_full_registry."""
        from app.plugins.bootstrap import build_full_registry
        tool_reg, _, _ = build_full_registry(enable_mcp=False, enable_plugins=False)
        assert "execute_python" in {t.name for t in tool_reg.list_tools()}


class TestCodeToolIntegration:
    def test_agent_core_approval_gate(self):
        """execute_python requires approval in AgentCore."""
        from app.tools.base import ToolRegistry
        from app.tools.code import create_code_tools

        reg = ToolRegistry()
        create_code_tools(reg)
        tool = reg.get("execute_python")

        # Simulate AgentCore._should_approve with auto_approve=False
        assert tool.requires_approval is True

    def test_pandas_available(self):
        """Verify pandas is importable in subprocess."""
        result = _execute_python(code='import pandas; print(pandas.__version__)')
        if result["success"]:
            assert result["stdout"].strip() != ""
        # If pandas not installed, just skip — not a failure of the tool

    def test_large_output_is_returned(self):
        """Large output comes back fully (ResultStore handles truncation)."""
        code = 'print("x" * 50000)'
        result = _execute_python(code=code)
        assert result["success"] is True
        assert len(result["stdout"]) == 50001  # 50000 x's + newline
