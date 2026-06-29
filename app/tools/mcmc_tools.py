"""MCMC alloy discovery tools — lightweight, torch-free, local-default.

Replaces gfn_sample/gfn_discover for local PRISM installations.
Uses Metropolis-Hastings with physics descriptors instead of a
trained GFlowNet. No torch dependency.
"""

from app.tools.base import Tool, ToolRegistry


def _mcmc_sample(**kw) -> dict:
    """Cold-start alloy generation via MCMC (no torch needed)."""
    import numpy as np
    from app.tools.gflownet.spaces import AlloyDesignSpace, ELEMENT_DATA
    from app.tools.gflownet.surrogates.physics import PhysicsSurrogate
    from app.tools.mcmc_sampler import mcmc_sample

    elements_str = kw.get("elements")
    if elements_str:
        if isinstance(elements_str, str):
            elements = [e.strip() for e in elements_str.split(",") if e.strip()]
        else:
            elements = list(elements_str)
    else:
        elements = list(ELEMENT_DATA.keys())

    space = AlloyDesignSpace(elements=elements, n_units=100)
    surro = PhysicsSurrogate(space)

    pref_str = kw.get("preferences")
    if pref_str:
        w = np.array([float(x) for x in pref_str.split(",")])
    else:
        w = np.ones(len(surro.objective_names)) / len(surro.objective_names)
    w = w / w.sum()

    n = kw.get("n_samples", 32)
    n_steps = kw.get("n_steps", 1000)
    n_burn = kw.get("n_burn", 200)

    X, R = mcmc_sample(
        space,
        surro,
        w,
        n_samples=n,
        n_burn=n_burn,
        n_steps=n_steps,
        seed=kw.get("seed"),
    )

    show = kw.get("show", 10)
    results = []
    for i in range(min(show, len(X))):
        cnt = (X[i] * space.n_units).round().astype(int)
        results.append(
            {
                "formula": space.formula(cnt),
                "reward": round(float(R[i]), 4),
                "composition": {
                    space.elements[j]: round(float(X[i][j]), 4)
                    for j in range(space.n_elements)
                    if X[i][j] > 0.01
                },
            }
        )

    return {
        "mode": "mcmc_cold_start",
        "elements": space.elements,
        "objectives": surro.objective_names,
        "preference": np.round(w, 3).tolist(),
        "n_sampled": len(X),
        "top_alloys": results,
    }


def _mcmc_discover(**kw) -> dict:
    """Multi-round MCMC discovery with adaptive step size."""
    import numpy as np
    from app.tools.gflownet.spaces import AlloyDesignSpace, ELEMENT_DATA
    from app.tools.gflownet.surrogates.physics import PhysicsSurrogate
    from app.tools.mcmc_sampler import mcmc_discover

    elements_str = kw.get("elements")
    if elements_str:
        if isinstance(elements_str, str):
            elements = [e.strip() for e in elements_str.split(",") if e.strip()]
        else:
            elements = list(elements_str)
    else:
        elements = list(ELEMENT_DATA.keys())

    space = AlloyDesignSpace(elements=elements, n_units=100)
    surro = PhysicsSurrogate(space)

    pref_str = kw.get("preferences")
    if pref_str:
        w = np.array([float(x) for x in pref_str.split(",")])
    else:
        w = np.ones(len(surro.objective_names)) / len(surro.objective_names)
    w = w / w.sum()

    n_rounds = kw.get("n_rounds", 5)
    batch_size = kw.get("batch_size", 16)

    result = mcmc_discover(
        space,
        surro,
        w,
        n_rounds=n_rounds,
        batch_size=batch_size,
        seed=kw.get("seed"),
    )

    X = result["X"]
    R = result["R"]
    show = kw.get("show", 10)
    pareto = []
    for i in range(min(show, len(X))):
        cnt = (X[i] * space.n_units).round().astype(int)
        pareto.append(
            {
                "formula": space.formula(cnt),
                "reward": round(float(R[i]), 4),
                "composition": {
                    space.elements[j]: round(float(X[i][j]), 4)
                    for j in range(space.n_elements)
                    if X[i][j] > 0.01
                },
            }
        )

    return {
        "mode": "mcmc_discovery",
        "elements": space.elements,
        "objectives": surro.objective_names,
        "n_rounds": result["n_rounds"],
        "n_total": result["n_total"],
        "n_unique": result["n_unique"],
        "top_alloys": pareto,
    }


def create_mcmc_tools(registry: ToolRegistry) -> None:
    """Register MCMC alloy discovery tools (local default, no torch)."""
    registry.register(
        Tool(
            name="alloy_sample",
            description=(
                "Generate diverse alloy compositions using MCMC (Metropolis-Hastings). "
                "Cold-start mode uses physics descriptors (delta, VEC, entropy, density, "
                "melting point). No ML model needed — lightweight and fast. "
                "Returns top alloys ranked by scalarized reward."
            ),
            input_schema={
                "type": "object",
                "properties": {
                    "elements": {
                        "type": "string",
                        "description": "Comma-separated elements (e.g. 'Ni,Cr,Co,Fe'). Defaults to all available.",
                    },
                    "preferences": {
                        "type": "string",
                        "description": "Comma-separated preference weights for objectives",
                    },
                    "n_samples": {"type": "integer", "default": 32},
                    "n_steps": {"type": "integer", "default": 1000},
                    "show": {"type": "integer", "default": 10},
                },
            },
            func=_mcmc_sample,
            requires_approval=False,
            source="builtin",
            source_detail="MCMC physics sampler",
        )
    )

    registry.register(
        Tool(
            name="alloy_discover",
            description=(
                "Multi-round MCMC discovery with adaptive step size. "
                "Runs multiple sampling rounds, narrowing the search around "
                "the best compositions found. Returns unique top candidates."
            ),
            input_schema={
                "type": "object",
                "properties": {
                    "elements": {
                        "type": "string",
                        "description": "Comma-separated elements (e.g. 'Ni,Cr,Co,Fe')",
                    },
                    "preferences": {
                        "type": "string",
                        "description": "Comma-separated preference weights",
                    },
                    "n_rounds": {"type": "integer", "default": 5},
                    "batch_size": {"type": "integer", "default": 16},
                    "show": {"type": "integer", "default": 10},
                },
            },
            func=_mcmc_discover,
            requires_approval=False,
            source="builtin",
            source_detail="MCMC adaptive discovery",
        )
    )
