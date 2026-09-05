"""Loading a MACE model is not thread-safe, so PRISM serialises it.

2026-09-05: three MD jobs started in the same second in the runner's thread
pool; each loaded the model, and torch.fx's symbolic tracer — a process-global
patcher used while deserialising the traced graph module — raised
"CURRENT_PATCHER is None in finally block" in one of them. The job failed at
step 0 and the review lost a leg."""
import sys
import threading
import time
import types

import pytest

from app.tools.simulation.mace.core import calculator as calc


def test_model_loads_never_overlap():
    active, peak, lock = [0], [0], threading.Lock()

    def slow_load(tag):
        with lock:
            active[0] += 1; peak[0] = max(peak[0], active[0])
        time.sleep(0.05)
        with lock:
            active[0] -= 1
        return f"calc-{tag}"

    threads = [threading.Thread(target=lambda i=i: calc.serialized_load(slow_load, i)) for i in range(4)]
    for t in threads: t.start()
    for t in threads: t.join()
    assert peak[0] == 1, f"{peak[0]} loads ran at once"


def test_make_calc_loads_under_the_lock(monkeypatch):
    # Stub the heavy imports if the test environment lacks them.
    hub = sys.modules.get("huggingface_hub") or types.ModuleType("huggingface_hub")
    hub.hf_hub_download = lambda repo_id, filename: "/tmp/fake-model.model"
    monkeypatch.setitem(sys.modules, "huggingface_hub", hub)
    seen = {}

    def fake_construct(path, dtype, device, head):
        seen["locked"] = calc.LOAD_LOCK.locked()
        seen["args"] = (path, dtype, device, head)
        return "the calculator"

    monkeypatch.setattr(calc, "_construct", fake_construct)
    monkeypatch.setattr(calc, "resolve_model", lambda *a, **k: ("org/repo", "file.model", "MIT"), raising=False)
    out = calc.make_calc(head=calc.DEFAULT_HEAD, device="cpu", dtype="float32")
    assert out == "the calculator"
    assert seen["locked"] is True, "construction happened outside the load lock"
    assert seen["args"][1:] == ("float32", "cpu", calc.DEFAULT_HEAD)
