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
import os
import sys
import traceback

# Caps applied to every response BEFORE json.dumps, so one verbose cell can
# never emit a multi-GB line that OOMs the Rust backend reading it. The Rust
# side has its own hard line cap as a second line of defense.
MAX_TEXT = 1_000_000  # ~1 MB per stdout/stderr/result stream
MAX_IMAGES = 10  # keep at most this many figures per cell
MAX_IMAGE_BYTES_TOTAL = 8_000_000  # ~8 MB of base64 image data per cell

# The Rust reader (notebook.rs `MAX_LINE_BYTES`) reaps + restarts the kernel
# the instant ONE response line's UTF-8 length exceeds this hard cap — which
# would wipe the whole shared session over a normal exception or print. It is
# mirrored here EXACTLY. The per-field CHARACTER caps above cannot bound this:
# json.dumps (ensure_ascii=True) escapes non-ASCII, so one BMP char becomes
# `\uXXXX` (6 bytes) and one emoji a surrogate pair (12 bytes). A 1M-char field
# can thus serialize to ~12 MB, and three such fields blow past 16 MiB. So
# `_emit` enforces a total-BYTE budget on the serialized line (below the Rust
# cap by a safe margin) as the real guard.
MAX_LINE_BYTES = 16 * 1024 * 1024  # MUST equal notebook.rs MAX_LINE_BYTES
LINE_BUDGET_MARGIN = 256 * 1024  # headroom for the trailing newline + markers
LINE_BUDGET_BYTES = MAX_LINE_BYTES - LINE_BUDGET_MARGIN


def _cap_text(value):
    """Truncate an over-long captured string, noting how much was dropped."""
    if value is None or len(value) <= MAX_TEXT:
        return value
    dropped = len(value) - MAX_TEXT
    return value[:MAX_TEXT] + f"\n...[truncated {dropped} characters]"


def _cap_error(error):
    """Bound every field of an error payload before it is serialized.

    A recoverable exception with a huge message (e.g.
    ``raise ValueError('x' * 20_000_000)``) would otherwise produce a
    response line far beyond the Rust reader's per-line cap — which reaps
    and restarts the kernel, destroying the whole shared session over a
    perfectly normal exception. Caps ename/evalue individually and bounds
    the traceback to ~MAX_TEXT total, keeping the HEAD of an oversized
    frame (the exception line lives there) and reporting anything dropped.
    """
    if not error:
        return error
    frames = []
    kept = 0
    dropped = 0
    for frame in error.get("traceback") or []:
        frame = str(frame)
        room = MAX_TEXT - kept
        if room <= 0:
            dropped += 1
            continue
        if len(frame) > room:
            omitted = len(frame) - room
            frame = frame[:room] + f"\n...[truncated {omitted} characters]"
        kept += len(frame)
        frames.append(frame)
    if dropped:
        frames.append(f"...[{dropped} traceback frame(s) dropped]")
    return {
        "ename": _cap_text(str(error.get("ename", "Error"))),
        "evalue": _cap_text(str(error.get("evalue", ""))),
        "traceback": frames,
    }


def _cap_images(images):
    """Bound both the count and the total base64 size of returned images.

    Returns ``(kept, note)`` — figures past the budget are REPORTED via the
    note (surfaced on stderr), never silently discarded.
    """
    capped = []
    total = 0
    for image in images[:MAX_IMAGES]:
        size = len(image.get("b64", ""))
        if total + size > MAX_IMAGE_BYTES_TOTAL:
            break
        total += size
        capped.append(image)
    dropped = len(images) - len(capped)
    note = f"[{dropped} plot(s) dropped: over the per-cell image budget]" if dropped else None
    return capped, note


class _CappedBuffer(io.TextIOBase):
    """Append-only text sink that stops STORING past ~MAX_TEXT.

    Overflow is counted, not kept, so a print-flood inside a cell cannot
    balloon the sidecar's memory while the cell runs (the old ``StringIO``
    grew by GBs before the end-of-cell cap). ``value()`` stays within
    MAX_TEXT including its truncation marker, so the final ``_cap_text``
    pass never mangles the marker. Duck-types as a ``write()``-able stream
    for the builtin backend's stdout/stderr redirection.
    """

    _ROOM = 100  # reserved for the truncation marker

    def __init__(self):
        super().__init__()
        self._parts = []
        self._length = 0
        self._dropped = 0

    def writable(self):
        return True

    def write(self, text):
        text = str(text)
        room = (MAX_TEXT - self._ROOM) - self._length
        if room > 0:
            kept = text[:room]
            self._parts.append(kept)
            self._length += len(kept)
            self._dropped += len(text) - len(kept)
        else:
            self._dropped += len(text)
        return len(text)

    def value(self):
        out = "".join(self._parts)
        if self._dropped:
            out += f"\n...[truncated {self._dropped} characters]"
        return out

    getvalue = value


def _truncate_utf8_bytes(text, max_bytes):
    """Truncate ``text`` so its UTF-8 encoding is at most ``max_bytes``.

    Cuts on a codepoint boundary (``errors="ignore"`` drops any dangling
    partial char) and appends a VISIBLE marker reporting the dropped byte
    count, so a byte-level truncation is never silent.
    """
    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return text
    dropped = len(encoded) - max_bytes
    head = encoded[:max_bytes].decode("utf-8", errors="ignore")
    return head + f"\n...[truncated {dropped} bytes to fit the wire limit]"


def _enforce_line_budget(obj):
    """Return a copy of ``obj`` whose ``json.dumps`` UTF-8 length is under
    LINE_BUDGET_BYTES, shaving the largest string fields BY BYTES until it
    fits. The per-field character caps cannot guarantee this (see
    MAX_LINE_BYTES): under JSON escaping one char can cost up to 12 bytes.

    Truncatable fields are the text-bearing ones (``stdout``/``stderr``/
    ``result`` and the error's ``evalue``/``ename``/traceback frames).
    Images are base64 and already capped far below the budget
    (MAX_IMAGE_BYTES_TOTAL), so they are never the overflow source and are
    left untouched (truncating base64 would corrupt an otherwise-valid PNG);
    an unreachable last-resort drops them wholesale if text alone can't fit.
    """
    obj = dict(obj)
    error = obj.get("error")
    if isinstance(error, dict):
        error = dict(error)
        error["traceback"] = list(error.get("traceback") or [])
        obj["error"] = error
    else:
        error = None

    slots = []  # (read, write) for each shrinkable string field.

    def add_dict_slot(container, key):
        if isinstance(container.get(key), str):
            slots.append((lambda: container[key], lambda v: container.__setitem__(key, v)))

    for key in ("stdout", "stderr", "result"):
        add_dict_slot(obj, key)
    if error is not None:
        for key in ("evalue", "ename"):
            add_dict_slot(error, key)
        frames = error["traceback"]
        for index in range(len(frames)):
            if isinstance(frames[index], str):
                slots.append(
                    (
                        lambda i=index: frames[i],
                        lambda v, i=index: frames.__setitem__(i, v),
                    )
                )

    def line_len():
        return len(json.dumps(obj).encode("utf-8"))

    # Each pass shaves the currently-largest field by at least the overage,
    # so this converges in a handful of iterations; the guard is paranoia.
    guard = 0
    while slots and guard < 10_000 and line_len() > LINE_BUDGET_BYTES:
        guard += 1
        read, write = max(slots, key=lambda slot: len(slot[0]().encode("utf-8")))
        current = read()
        cur_bytes = len(current.encode("utf-8"))
        overage = line_len() - LINE_BUDGET_BYTES
        write(_truncate_utf8_bytes(current, max(0, cur_bytes - overage - 1024)))

    if obj.get("images") and line_len() > LINE_BUDGET_BYTES:
        obj["images"] = []  # unreachable in practice; keeps the invariant hard.
    return obj


def _emit(obj):
    """Write one protocol JSON line to stdout and flush.

    Guarantees the emitted line's UTF-8 byte length stays under the Rust
    reader's per-line cap (see LINE_BUDGET_BYTES / MAX_LINE_BYTES): a line
    over that cap makes the supervisor reap + restart the kernel, wiping the
    shared session over a normal exception or print. When the serialized
    payload would exceed the budget, the largest string fields are truncated
    by BYTES (with a visible marker) until it fits.
    """
    line = json.dumps(obj)
    if len(line.encode("utf-8")) > LINE_BUDGET_BYTES:
        line = json.dumps(_enforce_line_budget(obj))
    sys.stdout.write(line + "\n")
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

        # Capped at append time — a print-flood over iopub must not balloon
        # the sidecar while the cell runs.
        stdout, stderr = _CappedBuffer(), _CappedBuffer()
        images = []
        result_repr = None
        error = None
        execution_count = None
        kernel_dead = False
        deadline_timeout = timeout if timeout and timeout > 0 else None

        # Drain iopub until the kernel returns to idle for OUR request.
        while True:
            try:
                msg = client.get_iopub_msg(timeout=deadline_timeout)
            except Exception:
                # Soft timeout (or a dead channel): interrupt, then confirm the
                # kernel actually came back to idle before we let it be reused.
                # A GIL-bound loop ignores SIGINT — if it never idles, the
                # kernel is wedged, so mark it dead and let Rust restart it (so
                # "variables intact" is never a lie).
                try:
                    self.manager.interrupt_kernel()
                except Exception:
                    pass
                kernel_dead = not self._drain_to_idle(msg_id, grace=5)
                if kernel_dead:
                    # Rust reaps a wedged kernel and starts fresh — say so,
                    # honestly: "interrupted" alone would imply the session
                    # (its variables) survived, and it did not.
                    detail = (
                        f"cell exceeded {timeout}s and the kernel ignored the "
                        "interrupt — the kernel was restarted, so variables "
                        "from this session were lost. Re-run setup cells, or "
                        "raise the timeout."
                    )
                else:
                    detail = f"cell exceeded {timeout}s and was interrupted"
                error = {
                    "ename": "TimeoutError",
                    "evalue": detail,
                    "traceback": [f"TimeoutError: {detail}"],
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
                (stdout if content.get("name") == "stdout" else stderr).write(
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
            "stdout": stdout.value(),
            "stderr": stderr.value(),
            "result": result_repr,
            "images": images,
            "error": error,
            "execution_count": execution_count,
            "kernel_dead": kernel_dead,
        }

    def _drain_to_idle(self, msg_id, grace):
        """After an interrupt, wait up to ``grace`` seconds for OUR request to
        reach idle. Returns True if the kernel recovered, False if wedged."""
        import time

        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            try:
                msg = self.client.get_iopub_msg(timeout=deadline - time.monotonic())
            except Exception:
                return False
            if msg.get("parent_header", {}).get("msg_id") != msg_id:
                continue
            if (
                msg["header"]["msg_type"] == "status"
                and msg["content"].get("execution_state") == "idle"
            ):
                return True
        return False

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
        # Force a non-interactive matplotlib backend BEFORE any user code can
        # import pyplot. Without this, macOS defaults to the `macosx` GUI
        # backend, where `plt.show()` BLOCKS the sidecar until the Rust
        # hard-timeout kills+restarts it — wiping the shared session. (Only the
        # stdlib fallback does this; the Jupyter backend keeps its inline
        # backend so plots are still captured as images.)
        os.environ.setdefault("MPLBACKEND", "Agg")
        self.globals = {"__name__": "__main__", "__builtins__": __builtins__}
        self.count = 0

    def execute(self, code, timeout):  # timeout enforced by Rust (process kill)
        self.count += 1
        # Capped at write time — a print-flood must not balloon this process
        # by GBs before the end-of-cell truncation gets a chance to run.
        stdout = _CappedBuffer()
        stderr = _CappedBuffer()
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

        images, image_note = self._harvest_matplotlib()
        stderr_text = stderr.value()
        if image_note:
            stderr_text = f"{stderr_text}\n{image_note}" if stderr_text else image_note

        return {
            "stdout": stdout.value(),
            "stderr": stderr_text,
            "result": result_repr,
            "images": images,
            "error": error,
            "execution_count": self.count,
            "kernel_dead": False,
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
        """Snapshot any open matplotlib figures to PNG, then close them.

        Returns ``(images, note)`` — a figure that fails to render is COUNTED
        and reported via the note (surfaced on stderr), never silently
        dropped, and one bad figure doesn't stop the rest from capturing.
        """
        if "matplotlib" not in sys.modules:
            return [], None
        images = []
        failed = 0
        try:
            import matplotlib.pyplot as plt

            for num in plt.get_fignums():
                try:
                    fig = plt.figure(num)
                    buf = io.BytesIO()
                    fig.savefig(buf, format="png", bbox_inches="tight")
                    images.append(
                        {
                            "mime": "image/png",
                            "b64": base64.b64encode(buf.getvalue()).decode("ascii"),
                        }
                    )
                except Exception:
                    failed += 1
            plt.close("all")
        except Exception:
            failed += 1
        note = f"[{failed} plot(s) failed to capture]" if failed else None
        return images, note

    def shutdown(self):
        self.globals.clear()


# ── Backend selection + main loop ────────────────────────────────────────


def _make_kernel():
    """Prefer the real Jupyter kernel; fall back to the stdlib kernel."""
    # Test/diagnostic hook: force the stdlib fallback even where Jupyter is
    # importable, so the fallback path can be exercised deterministically.
    if os.environ.get("PRISM_NOTEBOOK_FORCE_BUILTIN") == "1":
        return (
            BuiltinKernel(),
            "builtin",
            "stdlib kernel (forced via PRISM_NOTEBOOK_FORCE_BUILTIN)",
        )
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
                    # Capped like every error: a fault carrying a huge message
                    # must not exceed the Rust line cap and kill the session.
                    "error": _cap_error(
                        {
                            "ename": type(exc).__name__,
                            "evalue": str(exc),
                            "traceback": traceback.format_exc().splitlines(),
                        }
                    ),
                    "execution_count": None,
                }
            )
            continue

        images, image_note = _cap_images(out["images"])
        stderr = _cap_text(out["stderr"]) or ""
        if image_note:
            stderr = f"{stderr}\n{image_note}" if stderr else image_note
        _emit(
            {
                "event": "result",
                "id": req_id,
                "status": "error" if out["error"] else "ok",
                "stdout": _cap_text(out["stdout"]),
                "stderr": stderr,
                "result": _cap_text(out["result"]),
                "images": images,
                "error": _cap_error(out["error"]),
                "execution_count": out["execution_count"],
                "kernel_dead": out.get("kernel_dead", False),
            }
        )


if __name__ == "__main__":
    main()
