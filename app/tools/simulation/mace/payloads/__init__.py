"""HF Job payload scripts.

Each script is invoked by ``hf jobs uv run`` with a single positional
argument: a path to a JSON file containing the canonical job spec
``{tool_name, input_payload, cache_key, seed, results_repo}``.

They use PEP 723 inline dependency metadata so ``uv`` resolves the env
inside the L4×1 container without an external pyproject.toml.

The payloads bundle ``mace_core`` source into the script via a sibling
clone, since the HF Jobs container only sees the script file. To avoid
that fragility, every payload begins by ``pip install``-ing
``mace-mcp`` itself from the public PyPI release once one exists; for
development, set ``MACE_MCP_DEV_INSTALL_URL`` to a wheel URL.
"""
