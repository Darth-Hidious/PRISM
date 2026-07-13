#!/usr/bin/env python3
"""
smoke-mcp-direct.py — call PRISM's MCP tools directly over JSON-RPC stdio.

Bypasses the LLM agent loop entirely — proves whether each tool implementation
itself works against the live MARC27 platform / local services. This catches
classes of bugs the chat-driven harness (scripts/smoke-engine-tools.sh)
cannot:
  - tool wired but its API endpoint returns 5xx
  - tool wired but its arg schema disagrees with its handler
  - tool wired but the platform JWT scope is wrong
  - tool removed from registry but still listed by `tools/list`

Usage:
  scripts/smoke-mcp-direct.py [--server rust|python|both]
                              [--filter PATTERN]
                              [--timeout 30]
                              [--bin ./target/release/prism]
                              [--python /path/to/venv/python]

Output: per-tool PASS/FAIL/SKIP with reason, plus matrix at end.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

# ─── Tool call plans ──────────────────────────────────────────────────────────
# For each tool, what arguments do we pass for a smoke test?
# safe=True  → invoke directly, expect success (read-only or harmless)
# safe=False → SKIP unless --include-destructive (don't ship side effects in CI)
# args=None  → tool is unknown to us, will be auto-classified at runtime by
#              schema (zero required args → call empty; otherwise SKIP-needs-args)

@dataclass
class Plan:
    args: dict | None
    safe: bool
    note: str = ""

# Curated allowlist of read-only / harmless calls. Keys match tool names.
PLANS: dict[str, Plan] = {
    # Rust MCP tools
    "status":               Plan({"args": []},                                 True,  "zero-arg CLI passthrough"),
    "tools":                Plan({"args": []},                                 True),
    "node_probe":           Plan({},                                           True,  "no side effects"),
    "node_status":          Plan({},                                           True),
    "workflow_list":        Plan({},                                           True),
    "discourse_list":       Plan({},                                           True),
    "deploy_list":          Plan({},                                           True),
    "models_list":          Plan({},                                           True),
    "marketplace_search":   Plan({"query": "alloy"},                           True,  "search by keyword"),
    "marketplace_info":     Plan({"name": "calphad"},                          True,  "may legitimately 404"),
    "mesh_discover":        Plan({"timeout": 2},                               True,  "mDNS, 2s window"),
    "mesh_peers":           Plan({},                                           True,  "may fail if no node up"),
    "mesh_subscriptions":   Plan({},                                           True,  "may fail if no node up"),
    "query_local":          Plan({"text": "nickel"},                           True,  "may fail if no local stack"),
    "query_platform":       Plan({"text": "nickel", "limit": 3},               True),
    "research_query":       Plan({"query": "nickel superalloy", "depth": 0},   True,  "depth=0 cheapest path"),
    "models_search":        Plan({"query": "gemini"},                          True),
    "models_info":          Plan({"model_id": "gemini-3.1-flash-lite-preview"}, True),

    # Read-only Python tools (direct-callable with zero args)
    "knowledge_stats":              Plan({},                                  True),
    "compute_gpus":                 Plan({},                                  True),
    "compute_providers":            Plan({},                                  True),
    "list_corpora":                 Plan({},                                  True),
    "list_lab_services":            Plan({},                                  True),
    "check_lab_subscriptions":      Plan({},                                  True),
    "list_models":                  Plan({},                                  True),
    "list_predictable_properties":  Plan({"dataset_name": "demo"},            True,  "may 404 — that's a finding"),
    "list_bash_tasks":              Plan({},                                  True),

    # Read-only Python tools with required args.
    # Arg names below are the ACTUAL ones the Pydantic schema expects (verified
    # against direct-MCP run). The fact that they don't match the prose in
    # `prism tools` descriptions is a real-but-minor finding — see notes.
    "knowledge_search":     Plan({"term": "nickel superalloy", "limit": 3},   True),
    "knowledge_entity":     Plan({"name": "Inconel_718"},                     True, "may 404 → finding"),
    "knowledge_paths":      Plan({"from_entity": "Inconel 718", "to_entity": "creep resistance"}, True),
    "literature_search":    Plan({"query": "high entropy alloys", "max_results": 2}, True),
    "compute_estimate":     Plan({"image": "alpine:latest", "gpu_type": "A100-80GB"}, True),
    "compute_status":       Plan({"job_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),
    "deploy_status":        Plan({"deployment_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),
    "deploy_health":        Plan({"deployment_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),
    "get_lab_service_info": Plan({"service_id": "a-lab"},                     True),
    "marketplace_search":   Plan({"query": "alloy"},                          True),
    "discourse_show":       Plan({"spec_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),
    "discourse_status":     Plan({"instance_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),
    "discourse_turns":      Plan({"instance_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),
    "job_status_lookup":    Plan({"job_id": "00000000-0000-0000-0000-000000000000"}, True, "404 expected"),

    # DESTRUCTIVE — skip by default
    "deploy_create":        Plan(None, False, "would deploy a real container"),
    "deploy_stop":          Plan(None, False, "would stop running deployments"),
    "ingest":               Plan(None, False, "writes to graph"),
    "ingest_file":          Plan(None, False, "writes to graph"),
    "ingest_watch":         Plan(None, False, "writes to graph + spawns watcher"),
    "knowledge_ingest":     Plan(None, False, "writes to knowledge graph"),
    "import_dataset":       Plan(None, False, "writes to DataStore"),
    "execute_bash":         Plan(None, False, "arbitrary code execution"),
    "execute_python":       Plan(None, False, "arbitrary code execution"),
    "edit_file":            Plan(None, False, "modifies workspace files"),
    "marketplace_install":  Plan(None, False, "writes to ~/.prism/tools or ~/.prism/workflows"),
    "mesh_publish":         Plan(None, False, "broadcasts on mesh"),
    "mesh_subscribe":       Plan(None, False, "modifies subscription state"),
    "mesh_unsubscribe":     Plan(None, False, "modifies subscription state"),
    "discourse_create":     Plan(None, False, "uploads spec to platform"),
    "discourse_run":        Plan(None, False, "starts a paid LLM run"),
    "compute_submit":       Plan(None, False, "spends GPU credits"),
    "compute_cancel":       Plan(None, False, "modifies running jobs"),
    "deploy":               Plan(None, False, "CLI passthrough — destructive"),
    "publish":              Plan(None, False, "CLI passthrough — destructive"),
    "publish_artifact":     Plan(None, False, "uploads artifacts"),
    "ingest_pipeline_run":  Plan(None, False, "writes to graph"),
    "run":                  Plan(None, False, "CLI passthrough — destructive"),
    "run_submit":           Plan(None, False, "spends compute credits"),
    "research":             Plan(None, False, "expensive — research_query covers smoke"),
    "agent":                Plan(None, False, "CLI passthrough"),
    "node":                 Plan(None, False, "CLI passthrough — node lifecycle"),
    "mesh":                 Plan(None, False, "CLI passthrough — mesh ops"),
    "workflow":             Plan(None, False, "CLI passthrough"),
    "workflow_run":         Plan(None, False, "executes workflows — may spend compute"),
    "marketplace":          Plan(None, False, "CLI passthrough"),
    "discourse":            Plan(None, False, "CLI passthrough"),
    "models":               Plan(None, False, "CLI passthrough"),
    "query":                Plan(None, False, "CLI passthrough — query_local/platform are typed"),
    "ingest_file_pipeline": Plan(None, False, "writes to graph"),
    "ingest_watch_pipeline":Plan(None, False, "writes to graph"),
    "node_logs":            Plan({"service": "kafka", "tail": 5}, True, "may fail if no node up"),
    "job-status":           Plan(None, False, "CLI passthrough"),
    "workflow_show":        Plan({"name": "forge"}, True, "may 404"),
    "query_federated":      Plan(None, False, "needs running node"),
    "materials_discovery":  Plan(None, False, "spawns long pipeline"),
    "acquire_materials":    Plan(None, False, "writes a dataset"),
    "analyze_phases":       Plan(None, False, "writes results + needs db file"),
    "export_results_csv":   Plan(None, False, "writes file"),
    "generate_report":      Plan(None, False, "writes report"),
    "read_bash_task":       Plan(None, False, "needs task_id"),
    "stop_bash_task":       Plan(None, False, "destructive"),
}

# ─── JSON-RPC stdio client ────────────────────────────────────────────────────

class MCPClient:
    def __init__(self, name: str, cmd: list[str], cwd: str | None = None, env: dict | None = None):
        self.name = name
        self.cmd = cmd
        self.cwd = cwd
        self.env = env
        self.proc: subprocess.Popen | None = None
        self._id = 0

    def __enter__(self):
        self.proc = subprocess.Popen(
            self.cmd,
            cwd=self.cwd,
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )
        # initialize
        self._call("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "smoke-mcp-direct", "version": "0"},
        })
        return self

    def __exit__(self, *a):
        if self.proc and self.proc.poll() is None:
            try:
                self.proc.stdin.close()
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        return False

    def _call(self, method: str, params: dict | None = None, timeout: float = 30.0) -> dict:
        self._id += 1
        req = {"jsonrpc": "2.0", "id": self._id, "method": method}
        if params is not None:
            req["params"] = params
        line = json.dumps(req) + "\n"
        self.proc.stdin.write(line.encode())
        self.proc.stdin.flush()
        # Read until we get a response with this id (skip stderr / non-json)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            raw = self.proc.stdout.readline()
            if not raw:
                if self.proc.poll() is not None:
                    raise RuntimeError(f"server exited with code {self.proc.returncode}")
                continue
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if msg.get("id") == self._id:
                return msg
        raise TimeoutError(f"no response to {method} within {timeout}s")

    def list_tools(self) -> list[dict]:
        r = self._call("tools/list")
        return r.get("result", {}).get("tools", [])

    def call_tool(self, name: str, args: dict, timeout: float = 30.0) -> dict:
        return self._call("tools/call", {"name": name, "arguments": args}, timeout=timeout)


# ─── Verdict logic ────────────────────────────────────────────────────────────

def classify(name: str, schema: dict, plan: Plan | None) -> tuple[str, dict | None, str]:
    """Return (verdict, args, note). verdict in {RUN, SKIP, AUTO}."""
    if plan is None:
        # No explicit plan — best-effort by schema
        required = schema.get("required") or []
        if not required:
            return ("RUN", {}, "auto: zero required args")
        return ("SKIP", None, f"auto: required={required} no plan")
    if not plan.safe:
        return ("SKIP", None, f"destructive: {plan.note}")
    return ("RUN", plan.args or {}, plan.note)


def run_one(client: MCPClient, name: str, schema: dict, args: dict, timeout: float) -> tuple[str, str, float]:
    t0 = time.monotonic()
    try:
        resp = client.call_tool(name, args, timeout=timeout)
        dt = time.monotonic() - t0
        if "error" in resp:
            err = resp["error"].get("message", str(resp["error"]))
            return ("FAIL", err[:200], dt)
        result = resp.get("result", {})
        # MCP can return content array or isError
        if result.get("isError"):
            content = result.get("content", [])
            text = " ".join(c.get("text", "") for c in content if c.get("type") == "text")
            return ("FAIL", text[:200] or "isError=true", dt)
        # success — describe payload size
        content = result.get("content", [])
        size = sum(len(c.get("text", "")) for c in content if c.get("type") == "text")
        return ("PASS", f"{size}b payload", dt)
    except (TimeoutError, RuntimeError) as e:
        return ("FAIL", str(e)[:200], time.monotonic() - t0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", choices=["rust", "python", "both"], default="both")
    ap.add_argument("--filter", default="")
    ap.add_argument("--timeout", type=float, default=45.0)
    ap.add_argument("--bin", default="./target/release/prism")
    ap.add_argument("--python", default=str(Path.home() / ".prism/venv/bin/python3"))
    ap.add_argument("--include-destructive", action="store_true")
    ap.add_argument("--out", default=f"/tmp/prism-smoke/mcp-{time.strftime('%Y%m%d_%H%M%S')}")
    ns = ap.parse_args()

    out_dir = Path(ns.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    print(f"out: {out_dir}", file=sys.stderr)

    servers: list[tuple[str, list[str]]] = []
    if ns.server in ("rust", "both"):
        servers.append(("rust", [ns.bin, "mcp-server-native"]))
    if ns.server in ("python", "both"):
        if Path(ns.python).exists():
            servers.append(("python", [ns.python, "-m", "app.mcp_server"]))
        else:
            print(f"!! skipping python server: {ns.python} not found", file=sys.stderr)

    all_results: list[dict] = []

    for server_name, cmd in servers:
        print(f"\n══ {server_name} MCP server ══", file=sys.stderr)
        print(f"   cmd: {shlex.join(cmd)}", file=sys.stderr)
        env = os.environ.copy()
        try:
            with MCPClient(server_name, cmd, cwd=os.getcwd(), env=env) as client:
                tools = client.list_tools()
                print(f"   tools: {len(tools)}", file=sys.stderr)
                for t in tools:
                    name = t["name"]
                    if ns.filter and ns.filter not in name:
                        continue
                    schema = t.get("inputSchema", {})
                    plan = PLANS.get(name)
                    verdict, args, note = classify(name, schema, plan)
                    if not ns.include_destructive and verdict == "SKIP" and plan and not plan.safe:
                        all_results.append({"server": server_name, "tool": name, "verdict": "SKIP", "reason": note, "dt": 0})
                        continue
                    if verdict == "SKIP":
                        all_results.append({"server": server_name, "tool": name, "verdict": "SKIP", "reason": note, "dt": 0})
                        print(f"   [SKIP] {name:30s} {note}", file=sys.stderr)
                        continue
                    # RUN
                    v, info, dt = run_one(client, name, schema, args, ns.timeout)
                    color = "\033[32m" if v == "PASS" else "\033[31m"
                    reset = "\033[0m"
                    print(f"   [{color}{v}{reset}] {name:30s} {dt:6.2f}s  {info[:60]}", file=sys.stderr)
                    all_results.append({"server": server_name, "tool": name, "verdict": v, "reason": info, "dt": dt, "args": args})
        except Exception as e:
            print(f"!! {server_name} server error: {e}", file=sys.stderr)
            continue

    # Matrix
    print("\n══ MATRIX ══", file=sys.stderr)
    by_v = {}
    for r in all_results:
        by_v.setdefault(r["verdict"], []).append(r)
    for v in ("PASS", "FAIL", "SKIP"):
        n = len(by_v.get(v, []))
        print(f"  {v}: {n}", file=sys.stderr)
    if "FAIL" in by_v:
        print("\n  failures:", file=sys.stderr)
        for r in by_v["FAIL"]:
            print(f"    [{r['server']}] {r['tool']:30s}  {r['reason'][:80]}", file=sys.stderr)

    (out_dir / "results.json").write_text(json.dumps(all_results, indent=2, default=str))
    print(f"\n  → {out_dir}/results.json", file=sys.stderr)

    fail_n = len(by_v.get("FAIL", []))
    return 0 if fail_n == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
