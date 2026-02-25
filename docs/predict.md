# `prism predict` & `prism model` -- ML Property Prediction

Predict material properties from chemical formulas (composition-based) or crystal
structures (pre-trained GNNs). Train custom models on collected datasets.

## Two Prediction Pathways

| Pathway | Input | Models | Training? |
|---------|-------|--------|-----------|
| **Composition-based** | Chemical formula (e.g. `Fe2O3`) | RF, XGBoost, LightGBM, GBR, Linear | Yes (user trains on datasets) |
| **Structure-based** | Crystal structure (lattice + positions) | M3GNet, MEGNet (via matgl) | No (pre-trained, zero-shot) |

## CLI Commands

### `prism predict <formula>`

```bash
prism predict LiCoO2                                   # Default: band_gap, random_forest
prism predict Si --property formation_energy --algorithm xgboost
prism predict Fe2O3 --all-properties                    # All trained models
```

### `prism model train`

```bash
prism model train                                       # First dataset, band_gap, random_forest
prism model train --property band_gap --algorithm xgboost --dataset mp_data
```

### `prism model status`

Shows trained models (with metrics) and pre-trained GNNs (with install status).

## Agent Tools

| Tool | Description | Input |
|------|-------------|-------|
| `predict_property` | Composition-based prediction | formula, property_name, algorithm |
| `predict_structure` | Structure-based GNN prediction | structure (lattice/species/coords), model |
| `list_models` | List trained + pre-trained models | (none) |
| `list_predictable_properties` | Inspect dataset for numeric columns | dataset_name |

## Feature Engineering

Two backends, auto-selected:

| Backend | Features | Elements | Install |
|---------|----------|----------|---------|
| **matminer** (preferred) | 132 Magpie features | All elements | `pip install matminer` |
| **Built-in** (fallback) | 22 features | 44 common elements | Always available |

matminer is used automatically when installed. The `list_models` tool reports which
backend is active.

## Pre-trained GNN Models

These predict from crystal structures with zero training:

| Model | Property | Unit | Package |
|-------|----------|------|---------|
| `m3gnet-eform` | Formation energy | eV/atom | matgl |
| `megnet-eform` | Formation energy | eV/atom | matgl |
| `megnet-bandgap` | Band gap | eV | matgl |

```bash
pip install matgl   # Installs all three
```

The agent uses `predict_structure` to call these. Example:

```
User: Predict the formation energy of this BCC iron structure

Agent:
  [predict_structure] model=m3gnet-eform, structure={lattice: ..., species: ["Fe"], coords: ...}
  -> formation_energy = -0.082 eV/atom (M3GNet pre-trained)
```

## Composition-based Algorithm Registry

| Algorithm | Library | Optional? |
|-----------|---------|-----------|
| `random_forest` | scikit-learn | Always available |
| `gradient_boosting` | scikit-learn | Always available |
| `linear` | scikit-learn | Always available |
| `xgboost` | XGBoost | `pip install xgboost` |
| `lightgbm` | LightGBM | `pip install lightgbm` |

New algorithms can be added via plugins (see below).

## Plugin Models

Anyone can register custom algorithms via the plugin system:

```python
# ~/.prism/plugins/my_model.py
from app.tools.base import Tool

def register(registry):
    # Option 1: Register as algorithm (composition-based, trainable)
    registry.algorithm_registry.register(
        "my_gpr",
        "Gaussian Process Regressor",
        lambda: GaussianProcessRegressor(kernel=RBF()),
    )

    # Option 2: Register as tool (any prediction logic)
    registry.tool_registry.register(Tool(
        name="predict_with_crabnet",
        description="Predict property using CrabNet transformer",
        input_schema={...},
        func=_my_prediction_function,
    ))
```

For pip-installable plugins, use entry points:

```toml
# pyproject.toml
[project.entry-points."prism.plugins"]
my_model = "my_package.plugin"
```

### Publishing to the PRISM Marketplace

Models built with `execute_python` or registered as plugins can be published
to the PRISM platform marketplace. The platform handles:

- **Distribution**: install via `prism plugin install <name>`
- **Monetization**: pricing, licensing, usage tracking
- **Security**: sandboxing, access control, API key management
- **Discovery**: searchable catalog with benchmarks and descriptions

## Architecture

```
Formula → composition_features() → sklearn model → prediction
            (matminer or basic)     (AlgorithmRegistry)

Structure → matgl.load_model() → GNN inference → prediction
            (pre-trained M3GNet/MEGNet)

Plugin model → AlgorithmRegistry.register() or Tool → callable by agent
```

## Related

- [`prism data`](data.md) -- Collect training data
- [`prism sim`](sim.md) -- Generate structures for GNN prediction
- [Plugins](plugins.md) -- Register custom models
- [ACKNOWLEDGMENTS](../ACKNOWLEDGMENTS.md) -- matminer, matgl, scikit-learn credits
