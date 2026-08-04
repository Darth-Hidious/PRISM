"""Python 3.14 compatibility shim for pycalphad 0.11.2's Workspace.

Problem (observed, not assumed): pycalphad 0.11.2's
``Workspace.__init__`` (pycalphad/core/workspace.py:331) reads
``self.__annotations__``. Until Python 3.13 that fell back to the class
annotation dict; PEP 649 (deferred evaluation of annotations, default in
3.14) removed instance-level ``__annotations__``, so every equilibrium
constructed through Workspace raises::

    AttributeError: 'Workspace' object has no attribute '__annotations__'

This breaks kawin's driving-force / interfacial-composition lookups and
pycalphad's own ``equilibrium()``. As of 2026-07, pycalphad master still
contains the same line (verified against the raw GitHub source), and
0.11.2 is the newest release — there is no upstream fix to wait for in
this pin window.

The shim restores the pre-3.14 semantics exactly: before the original
``__init__`` runs, seed the instance dict with a copy of the class-level
annotations (accessing ``type(self).__annotations__`` on the class is
still supported under PEP 649). Applied only on Python >= 3.14, idempotent,
and scoped to this package's lifetime. It does not change any physics.
"""
from __future__ import annotations

import sys


def apply_py314_workspace_shim() -> None:
    """Patch pycalphad Workspace for PEP 649 on Python 3.14+. No-op on
    older interpreters or once applied."""
    if sys.version_info < (3, 14):
        return
    from pycalphad.core import workspace as _ws

    original = _ws.Workspace.__init__
    if getattr(original, "_prism_py314_annotations_shim", False):
        return

    def _patched_init(self, *args, **kwargs):
        if "__annotations__" not in self.__dict__:
            self.__dict__["__annotations__"] = dict(type(self).__annotations__)
        original(self, *args, **kwargs)

    _patched_init._prism_py314_annotations_shim = True  # type: ignore[attr-defined]
    _patched_init.__doc__ = original.__doc__
    _ws.Workspace.__init__ = _patched_init
