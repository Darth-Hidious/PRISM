"""Plugin status — the queryable inventory for the Python plugin plane.

Run as ``python -m app.plugins.status`` (the Rust CLI's ``prism plugins
list`` spawns exactly that) to print JSON:

    {"plane": "python", "loaded": {name: source, ...},
     "failed": {name: "source: error", ...}}

Listing EXECUTES plugin discovery (the same precedent as ``prism tools``,
which spawns the tool server to list tools): a plugin's ``register()``
runs in-process. That is the only way to report loaded/failed honestly —
a side-effect-free scan could name files but never say whether they load.
The failure contract is the loader's: one broken plugin is named, skipped,
and recorded; the rest still load.
"""
import json
import sys


def collect() -> dict:
    """Discover plugins in THIS process and return the status dict."""
    from app.plugins.loader import discover_all_plugins
    from app.plugins.registry import PluginRegistry

    registry = PluginRegistry()
    discover_all_plugins(registry)
    return {
        "plane": "python",
        "loaded": registry.loaded_plugins(),
        "failed": registry.failed_plugins(),
    }


def main() -> int:
    status = collect()
    json.dump(status, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
