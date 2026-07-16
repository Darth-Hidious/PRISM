#!/usr/bin/env python3
# Copyright (c) 2025-2026 MARC27. Licensed under MARC27 Source-Available License.
"""PRISM notebook kernel sidecar.

A tiny, dependency-optional Python kernel that PRISM (Rust) owns as a
supervised child process, mirroring the `python-bridge` tool-server pattern:
one JSON object per line on stdin (requests), one JSON object per line on
stdout (responses). All human/library chatter is kept OFF stdout so the
protocol stream stays clean.

Two backends, picked at startup with zero configuration:

  * ``jupyter`` — a real IPython kernel via ``jupyter_client``/``ipykernel``
    when both import. Gives rich outputs (``display_data``, inline PNG plots,
    ``execute_result`` reprs) exactly like a Jupyter notebook.
  * ``builtin`` — a pure-stdlib persistent ``exec`` kernel used when the
    Jupyter stack is absent, so code STILL runs out of the box. Captures
    stdout/stderr, echoes the last expression's ``repr`` (notebook-style),
    and harvests any open matplotlib figures to PNG if matplotlib happens to
    be installed. No pip install required.

Protocol
--------
startup  -> {"event":"hello","ok":true,"backend":"jupyter"|"builtin",
             "python":"3.x.y","detail":"..."}
request  <- {"op":"execute","id":N,"code":"...","timeout":120}
response -> {"event":"result","id":N,"status":"ok"|"error",
             "stdout":"...","stderr":"...","result":<repr|null>,
             "images":[{"mime":"image/png","b64":"..."}],
             "error":{"ename","evalue","traceback"}|null,
             "execution_count":N}
request  <- {"op":"shutdown"}   -> clean kernel shutdown, then process exit.

The Rust supervisor enforces a hard wall-clock timeout by killing+restarting
this process; the ``timeout`` field additionally lets the jupyter backend
return a clean soft-timeout (interrupting the kernel) without a process kill.
"""

import ast
import base64
import io
import json
import sys
import traceback


def _emit(obj):
    """Write one protocol JSON line to stdout and flush."""
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


# ── Jupyter backend ──────────────────────────────────────────────────────


class JupyterKernel:
    """Real IPython kernel driven over ZMQ by jupyter_client."""

    def __init__(self):
        from jupyter_client.manager import start_new_kernel

        # start_new_kernel blocks until the kernel is ready to accept input.
        self.manager, self.client = start_new_kernel(kernel_name="python3")

    def execute(self, code, timeout):
        client = self.client
        msg_id = client.execute(code)

        stdout, stderr = [], []
        images = []
        result_repr = None
        error = None
        execution_count = None
        deadline_timeout = timeout if timeout and timeout > 0 else None

        # Drain iopub until the kernel returns to idle for OUR request.
        while True:
            try:
                msg = client.get_iopub_msg(timeout=deadline_timeout)
            except Exception:
                # Soft timeout (or a dead channel): interrupt and report
                # honestly rather than hanging the whole sidecar.
                try:
                    self.manager.interrupt_kernel()
                except Exception:
                    pass
                error = {
                    "ename": "TimeoutError",
                    "evalue": f"cell exceeded {timeout}s and was interrupted",
                    "traceback": [f"TimeoutError: cell exceeded {timeout}s"],
                }
                break

            parent = msg.get("parent_header", {}).get("msg_id")
            if parent != msg_id:
                continue  # output from a different request — ignore.

            mtype = msg["header"]["msg_type"]
            content = msg["content"]

            if mtype == "status":
                if content.get("execution_state") == "idle":
                    break
            elif mtype == "stream":
                (stdout if content.get("name") == "stdout" else stderr).append(
                    content.get("text", "")
                )
            elif mtype in ("execute_result", "display_data"):
                data = content.get("data", {})
                if mtype == "execute_result":
                    execution_count = content.get("execution_count")
                    if "text/plain" in data:
                        result_repr = data["text/plain"]
                if "image/png" in data:
                    images.append({"mime": "image/png", "b64": data["image/png"]})
            elif mtype == "error":
                error = {
                    "ename": content.get("ename", "Error"),
                    "evalue": content.get("evalue", ""),
                    "traceback": content.get("traceback", []),
                }
            elif mtype == "execute_input":
                execution_count = content.get("execution_count", execution_count)

        # Reap the shell reply so it doesn't leak into the next cell.
        try:
            client.get_shell_msg(timeout=1)
        except Exception:
            pass

        return {
            "stdout": "".join(stdout),
            "stderr": "".join(stderr),
            "result": result_repr,
            "images": images,
            "error": error,
            "execution_count": execution_count,
        }

    def shutdown(self):
        try:
            self.client.stop_channels()
        except Exception:
            pass
        try:
            self.manager.shutdown_kernel(now=True)
        except Exception:
            pass


# ── Builtin (stdlib-only) backend ────────────────────────────────────────


class BuiltinKernel:
    """Persistent ``exec`` namespace — works with any Python 3, no deps.

    SECURITY: ``exec``/``eval`` here run code the user or the in-app agent
    explicitly submitted as a notebook cell — running arbitrary Python IS the
    contract of a notebook kernel, exactly like a Jupyter kernel. There is no
    sandbox and none is implied; the code is trusted the same way ``/python``
    and the ``execute_python`` tool are, and the ``notebook_exec`` agent tool
    is approval-gated for that reason. Do not treat this as untrusted input.
    """

    def __init__(self):
        self.globals = {"__name__": "__main__", "__builtins__": __builtins__}
        self.count = 0

    def execute(self, code, timeout):  # timeout enforced by Rust (process kill)
        self.count += 1
        stdout = io.StringIO()
        stderr = io.StringIO()
        result_repr = None
        error = None

        old_out, old_err = sys.stdout, sys.stderr
        sys.stdout, sys.stderr = stdout, stderr
        try:
            block, last_expr = self._split_last_expr(code)
            if block is not None:
                exec(block, self.globals)  # noqa: S102 — a notebook runs user code
            if last_expr is not None:
                value = eval(last_expr, self.globals)  # noqa: S307 — notebook eval
                if value is not None:
                    result_repr = repr(value)
        except Exception:
            etype, evalue, tb = sys.exc_info()
            error = {
                "ename": etype.__name__ if etype else "Error",
                "evalue": str(evalue),
                "traceback": traceback.format_exception(etype, evalue, tb),
            }
        finally:
            sys.stdout, sys.stderr = old_out, old_err

        return {
            "stdout": stdout.getvalue(),
            "stderr": stderr.getvalue(),
            "result": result_repr,
            "images": self._harvest_matplotlib(),
            "error": error,
            "execution_count": self.count,
        }

    @staticmethod
    def _split_last_expr(code):
        """Return (compiled-exec-block, compiled-eval-last-expr).

        Mirrors the notebook rule: if the final statement is a bare
        expression, evaluate it so its ``repr`` becomes the cell's output.
        On a syntax error, hand the whole thing to ``exec`` so the error
        surfaces normally.
        """
        try:
            parsed = ast.parse(code, mode="exec")
        except SyntaxError:
            return compile(code, "<cell>", "exec"), None
        if not parsed.body:
            return None, None
        last = parsed.body[-1]
        if isinstance(last, ast.Expr):
            head = ast.Module(body=parsed.body[:-1], type_ignores=[])
            expr = ast.Expression(body=last.value)
            block = compile(head, "<cell>", "exec") if head.body else None
            return block, compile(expr, "<cell>", "eval")
        return compile(parsed, "<cell>", "exec"), None

    @staticmethod
    def _harvest_matplotlib():
        """Snapshot any open matplotlib figures to PNG, then close them."""
        if "matplotlib" not in sys.modules:
            return []
        images = []
        try:
            import matplotlib.pyplot as plt

            for num in plt.get_fignums():
                fig = plt.figure(num)
                buf = io.BytesIO()
                fig.savefig(buf, format="png", bbox_inches="tight")
                images.append(
                    {
                        "mime": "image/png",
                        "b64": base64.b64encode(buf.getvalue()).decode("ascii"),
                    }
                )
            plt.close("all")
        except Exception:
            return images
        return images

    def shutdown(self):
        self.globals.clear()


# ── Backend selection + main loop ────────────────────────────────────────


def _make_kernel():
    """Prefer the real Jupyter kernel; fall back to the stdlib kernel."""
    try:
        import ipykernel  # noqa: F401
        import jupyter_client  # noqa: F401

        try:
            return JupyterKernel(), "jupyter", "IPython kernel via jupyter_client"
        except Exception as exc:  # kernel failed to launch — degrade, don't die.
            return (
                BuiltinKernel(),
                "builtin",
                f"jupyter_client present but kernel launch failed ({exc}); "
                f"using stdlib kernel",
            )
    except Exception:
        return (
            BuiltinKernel(),
            "builtin",
            "stdlib kernel (install jupyter_client + ipykernel for rich outputs)",
        )


def main():
    kernel, backend, detail = _make_kernel()
    _emit(
        {
            "event": "hello",
            "ok": True,
            "backend": backend,
            "python": "%d.%d.%d" % sys.version_info[:3],
            "detail": detail,
        }
    )

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception:
            continue

        op = req.get("op")
        if op == "shutdown":
            kernel.shutdown()
            _emit({"event": "goodbye", "ok": True})
            return
        if op != "execute":
            _emit({"event": "error", "id": req.get("id"), "message": f"unknown op: {op}"})
            continue

        req_id = req.get("id")
        code = req.get("code", "")
        timeout = req.get("timeout")
        try:
            out = kernel.execute(code, timeout)
        except Exception as exc:  # a kernel-level fault must not kill the loop.
            _emit(
                {
                    "event": "result",
                    "id": req_id,
                    "status": "error",
                    "stdout": "",
                    "stderr": "",
                    "result": None,
                    "images": [],
                    "error": {
                        "ename": type(exc).__name__,
                        "evalue": str(exc),
                        "traceback": traceback.format_exc().splitlines(),
                    },
                    "execution_count": None,
                }
            )
            continue

        _emit(
            {
                "event": "result",
                "id": req_id,
                "status": "error" if out["error"] else "ok",
                "stdout": out["stdout"],
                "stderr": out["stderr"],
                "result": out["result"],
                "images": out["images"],
                "error": out["error"],
                "execution_count": out["execution_count"],
            }
        )


if __name__ == "__main__":
    main()
