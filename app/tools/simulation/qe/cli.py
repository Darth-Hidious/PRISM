"""`python -m app.tools.simulation.qe.cli` — the command line behind `prism qe`
and the palette's QE entries. Prints JSON.

  status                       what is provisioned, and the run defaults
  settings [--set k=v]...      show, or set and show, the persisted defaults
  run --structure S [--calc scf] [--set k=v]...   write, run, parse
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def _set_pairs(pairs: list[str]) -> dict:
    from app.tools.simulation.qe import runtime

    out = {}
    for pair in pairs or []:
        if "=" not in pair:
            raise SystemExit(json.dumps({"error": f"--set expects key=value, got {pair!r}"}))
        key, value = pair.split("=", 1)
        out[key.strip()] = runtime.coerce_setting(key.strip(), value.strip())
    return out


def main(argv: list[str] | None = None) -> int:
    from app.tools.simulation.qe import runtime

    parser = argparse.ArgumentParser(prog="prism qe")
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status")
    p_set = sub.add_parser("settings")
    p_set.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    p_run = sub.add_parser("run")
    p_run.add_argument("--structure", required=True)
    p_run.add_argument("--calc", default="scf")
    p_run.add_argument("--workdir")
    p_run.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    args = parser.parse_args(argv)

    if args.cmd == "status":
        print(json.dumps(runtime.status(config={}), indent=2))
        return 0
    if args.cmd == "settings":
        try:
            new = _set_pairs(args.set)
        except ValueError as exc:
            print(json.dumps({"error": str(exc)}))
            return 2
        if new:
            saved = {k: v for k, v in runtime.load_settings_file().items() if not k.startswith("_")}
            saved.update(new)
            runtime.save_settings_file(saved)
        eff = runtime.settings(config={})
        eff["settings_file"] = str(runtime.settings_path())
        print(json.dumps(eff, indent=2))
        return 0
    if args.cmd == "run":
        from app.tools.simulation.qe.tools import _qe_run

        try:
            overrides = _set_pairs(args.set)
        except ValueError as exc:
            print(json.dumps({"error": str(exc)}))
            return 2
        kwargs = {"structure": args.structure, "calculation": args.calc, **overrides}
        if args.workdir:
            kwargs["workdir"] = args.workdir
        out = _qe_run(**kwargs)
        print(json.dumps(out, indent=2, default=str))
        return 0 if out.get("status") == "ok" else 1
    return 2


if __name__ == "__main__":
    sys.exit(main())
