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

    class Constructed:
        def calculate(self, *a, **k):
            return "computed"

    def fake_construct(path, dtype, device, head):
        seen["locked"] = calc.MODEL_GATE.is_loading()
        seen["args"] = (path, dtype, device, head)
        return Constructed()

    monkeypatch.setattr(calc, "_construct", fake_construct)
    monkeypatch.setattr(calc, "resolve_model", lambda *a, **k: ("org/repo", "file.model", "MIT"), raising=False)
    out = calc.make_calc(head=calc.DEFAULT_HEAD, device="cpu", dtype="float32")
    assert out.calculate() == "computed"
    assert seen["locked"] is True, "construction happened outside the model gate"
    assert seen["args"][1:] == ("float32", "cpu", calc.DEFAULT_HEAD)


def test_no_forward_pass_runs_while_a_model_is_loading():
    """The real 2026-09-06 failure: NameError "module is not installed as a
    submodule" raised inside torch.fx's module_call_wrapper during an ordinary
    MD force evaluation. torch.fx patches Module.__call__ *globally* while it
    traces, and loading a MACE model deserialises a traced GraphModule — so a
    load in one thread corrupts a forward pass already running in another.
    Serialising loads against each other (the earlier fix) is not enough:
    loads must also exclude inference."""
    import threading, time

    gate = calc.MODEL_GATE
    overlaps = []
    loading_now = [False]

    def loader():
        for _ in range(4):
            with gate.loading():
                loading_now[0] = True
                time.sleep(0.02)          # the trace window
                loading_now[0] = False

    def runner():
        for _ in range(60):
            with gate.running():
                if loading_now[0]:
                    overlaps.append(1)     # a forward pass during a trace
                time.sleep(0.001)

    threads = [threading.Thread(target=loader)] + [threading.Thread(target=runner) for _ in range(3)]
    for t in threads: t.start()
    for t in threads: t.join()
    assert not overlaps, f"{len(overlaps)} forward passes ran while a model was loading"


def test_inference_still_runs_in_parallel():
    """The gate must not serialise inference — MD does thousands of forward
    passes and they are the whole point of the thread pool."""
    import threading, time

    gate = calc.MODEL_GATE
    active, peak, lock = [0], [0], threading.Lock()

    def runner():
        with gate.running():
            with lock:
                active[0] += 1; peak[0] = max(peak[0], active[0])
            time.sleep(0.05)
            with lock:
                active[0] -= 1

    threads = [threading.Thread(target=runner) for _ in range(4)]
    for t in threads: t.start()
    for t in threads: t.join()
    assert peak[0] >= 2, f"inference was serialised (peak {peak[0]}); only loads may be exclusive"


def test_make_calc_guards_the_calculators_forward_pass(monkeypatch):
    """A calculator handed back by make_calc must take the shared side of the
    gate every time it computes, or the guard protects nothing in production."""
    import sys, types, threading

    hub = sys.modules.get("huggingface_hub") or types.ModuleType("huggingface_hub")
    hub.hf_hub_download = lambda repo_id, filename: "/tmp/fake-model.model"
    monkeypatch.setitem(sys.modules, "huggingface_hub", hub)

    seen = []

    class FakeCalc:
        def calculate(self, *a, **k):
            seen.append(calc.MODEL_GATE.readers())
            return "computed"

    monkeypatch.setattr(calc, "_construct", lambda path, dtype, device, head: FakeCalc())
    monkeypatch.setattr(calc, "resolve_model", lambda *a, **k: ("org/repo", "file.model", "MIT"), raising=False)
    c = calc.make_calc(head=calc.DEFAULT_HEAD, device="cpu", dtype="float32")
    assert c.calculate() == "computed"
    assert seen == [1], f"calculate() must run inside the gate (readers seen: {seen})"
    assert calc.MODEL_GATE.readers() == 0, "the gate must be released afterwards"
