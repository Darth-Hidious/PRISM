"""MACE foundation interatomic-potential primitives — native PRISM tools.

Merged from the standalone `mace-mcp` project (Apache-2.0) into PRISM's
`app/tools/simulation/mace/` so the agent invokes them as native function
calls instead of an MCP-over-stdio bridge. The protocol layer was decorative
overhead; primitives + cache + provenance live here directly now.

Framework note: MACE-MH-1 is shipped PyTorch-only by upstream
(mace-foundations, mace-torch >= 0.3.12); mace-jax does not support the
multi-head MH-1 architecture as of 2026. PRISM's broader stack is JAX-native
by default (jax-md, Flax, Equinox, NumPyro, BlackJAX) — MACE is one of the
explicit PyTorch holdouts. Marketplace metadata in marc27-core records this
honestly (framework: pytorch, tier: hybrid-pending-jax) so the research
agent routes framework-aware.
"""

from importlib.metadata import PackageNotFoundError, version

try:
    # Track the host PRISM-platform version so cache keys + provenance bundles
    # invalidate on PRISM upgrades. (mace-mcp used to publish its own version;
    # since the code now lives inside PRISM, version follows PRISM.)
    __version__ = version("prism-platform")
except PackageNotFoundError:
    __version__ = "0.0.0+dev"

__all__ = ["__version__"]
