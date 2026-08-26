"""A running cell can be stopped without losing the kernel.

The interrupt machinery already existed for the per-cell timeout path — the
sidecar interrupts the kernel when a deadline elapses — but no human or agent
could reach it. A scientist watching a cell they already know is wrong (a
runaway sweep, a mistyped bound) had exactly one escape: reset the kernel, which
throws away every variable in the session. And because the kernel is
single-occupancy, the wrong cell blocked every other cell and the agent with it.

`execute` blocks the sidecar's loop, so an interrupt that waits its turn is not
an interrupt. stdin is read on its own thread and `interrupt` is answered there,
out of band, while every other op keeps its arrival order.
"""

import json
import subprocess
import sys
import time
from pathlib import Path

import pytest

KERNEL = (
    Path(__file__).resolve().parents[1]
    / "crates"
    / "agent"
    / "src"
    / "notebook_kernel.py"
)


def _send(proc, obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


@pytest.fixture()
def kernel():
    proc = subprocess.Popen(
        [sys.executable, str(KERNEL)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )
    hello = json.loads(proc.stdout.readline())
    assert hello["event"] == "hello"
    yield proc, hello
    try:
        _send(proc, {"op": "shutdown"})
        proc.wait(timeout=10)
    except Exception:
        proc.kill()


def test_a_running_cell_can_be_interrupted(kernel):
    proc, hello = kernel
    if hello.get("backend") != "jupyter":
        pytest.skip("exec fallback cannot be interrupted; it reports so instead")

    _send(
        proc,
        {
            "op": "execute",
            "id": "cell-1",
            "code": "import time\nfor _ in range(600): time.sleep(1)\n",
            "timeout": 600,
        },
    )
    time.sleep(2)  # let the cell actually start

    started = time.monotonic()
    _send(proc, {"op": "interrupt", "id": "int-1"})

    ack = json.loads(proc.stdout.readline())
    assert ack["event"] == "interrupted", ack
    assert ack["ok"] is True, ack

    result = json.loads(proc.stdout.readline())
    assert result["event"] == "result", result
    assert result["status"] == "error", result
    assert result["error"]["ename"] == "KeyboardInterrupt", result["error"]

    elapsed = time.monotonic() - started
    assert elapsed < 30, (
        f"the interrupt must not wait for the cell's own 600s deadline; took {elapsed:.1f}s"
    )


def test_the_kernel_survives_an_interrupt_and_keeps_its_variables(kernel):
    proc, hello = kernel
    if hello.get("backend") != "jupyter":
        pytest.skip("exec fallback cannot be interrupted")

    # Something worth not losing.
    _send(proc, {"op": "execute", "id": "setup", "code": "keep_me = 1234\n", "timeout": 60})
    assert json.loads(proc.stdout.readline())["status"] == "ok"

    _send(
        proc,
        {
            "op": "execute",
            "id": "slow",
            "code": "import time\nfor _ in range(600): time.sleep(1)\n",
            "timeout": 600,
        },
    )
    time.sleep(2)
    _send(proc, {"op": "interrupt", "id": "int-2"})
    assert json.loads(proc.stdout.readline())["event"] == "interrupted"
    assert json.loads(proc.stdout.readline())["status"] == "error"

    # The whole point of interrupting instead of resetting.
    _send(proc, {"op": "execute", "id": "after", "code": "keep_me\n", "timeout": 60})
    after = json.loads(proc.stdout.readline())
    assert after["status"] == "ok", after
    assert "1234" in str(after.get("result")), after


def test_an_unknown_op_is_still_rejected(kernel):
    proc, _ = kernel
    _send(proc, {"op": "teleport", "id": "x"})
    reply = json.loads(proc.stdout.readline())
    assert reply["event"] == "error"
    assert "unknown op" in reply["message"]
