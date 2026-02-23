# PRISM Revival Design Document

**Date:** 2026-02-23
**Status:** Approved
**Approach:** Fix-First, Build-Second (3 phases)

---

## Current State Assessment

PRISM v1.1.0 is a materials science CLI tool that uses LLMs to translate natural language queries into OPTIMADE database filters. It queries 6+ materials databases (Materials Project, OQMD, COD, AFLOW, JARVIS, Materials Cloud).

**What works:** CLI command structure, OPTIMADE filter generation, LLM provider abstraction (4 providers), Rich UI, interactive mode, multi-step reasoning.

**What's broken/incomplete:**
- Zero test coverage (tests were removed)
- Orphaned REST API code (FastAPI endpoints, job processor, scheduler, Redis connector) -- never connected to CLI
- 4 "coming soon" LLM providers crash if env vars are set
- `_test_filter()` only does regex validation, never queries the actual OPTIMADE API
- `.env` loading searched in 3 inconsistent locations
- `band_gap` column in models.py has incomplete definition
- Hardcoded limits (max 3 attempts, 10 results, 3 questions) with no configuration
- `save-install` writes a stub instead of actual content
- No ML/prediction models exist

---

## Phase 1: Stabilize & Clean Up

### 1a. Remove Dead Code
- Delete `app/api/` directory (orphaned FastAPI endpoints)
- Delete `app/services/job_processor.py`, `job_scheduler.py`, `materials_service.py`
- Delete `app/services/connectors/redis_connector.py`
- Delete `app/services/enhanced_nomad_connector.py`
- Remove 4 "coming soon" LLM providers from `llm.py` (Perplexity, Grok, Ollama, PRISMCustom) and their env-var detection in `get_llm_service()`
- Clean up `pyproject.toml`: remove orphaned package references, remove `psycopg2-binary` and `alembic`

### 1b. Fix Known Issues
- Fix `_test_filter()` in `mcp.py` -- add real OPTIMADE API test query with `page_limit=1`
- Consolidate `.env` loading to one consistent approach
- Fix `band_gap` column in `models.py`
- Make hardcoded limits configurable via env vars or config
- Fix `save-install` doc generation

### 1c. Code Organization
Break up 1565-line `cli.py` into modules:
- `cli.py` -- command definitions only
- `commands/ask.py` -- the `ask` command logic
- `commands/search.py` -- the `search` command logic
- `commands/configure.py` -- configuration commands
- `commands/optimade.py` -- OPTIMADE database commands
- `utils/enrichment.py` -- Materials Project enrichment
- `utils/display.py` -- Rich table/panel display helpers

### 1d. Add Tests
- `tests/test_llm.py` -- LLM service factory
- `tests/test_mcp.py` -- filter parsing, validation, ModelContext
- `tests/test_cli.py` -- CLI commands with Click CliRunner
- Mock all external API calls

---

## Phase 2: Data Pipeline

### 2a. Data Collection (`app/data/collector.py`)
- **OPTIMADE Collector**: Paginated bulk fetching from all 6 providers
  - Fields: formula, elements, space group, lattice parameters, atomic positions
- **Materials Project Collector**: Enriched properties via `mp-api`
  - Formation energy, band gap, energy above hull, bulk/shear modulus, density, magnetic ordering, electronic structure, thermo, elasticity, dielectric
- Rate limiting to respect API quotas

### 2b. Data Normalization (`app/data/normalizer.py`)
- Unified schema across all providers
- Feature categories: compositional, structural, physical
- Output: Pandas DataFrame saved as Parquet files

### 2c. Storage (`app/data/store.py`)
- Parquet files in `data/` directory (versioned by collection date)
- SQLite for metadata (providers queried, timestamps, record counts)
- CLI commands: `prism data collect`, `prism data status`, `prism data export`

### 2d. Feature Engineering (`app/ml/features.py`)
- Magpie-style compositional features (~130 features per material)
- Structural features: space group encoding, lattice parameter ratios, volume per atom
- Element property lookup table (static JSON)
- Integration with matminer's 270+ featurizers

---

## Phase 3: ML Models (SoTA-Informed)

### 3a. Tiered Model Architecture

**Tier 1: Composition-Only (from formula alone)**
- Classical: XGBoost, LightGBM, Random Forest on matminer features
- MODNet: Best composition-only model on Matbench; great on small datasets
- CrabNet: Transformer-based composition model

**Tier 2: Structure-Based (crystal structure from OPTIMADE)**
- MACE-MP-0: Pretrained foundation model (30M params)
- CHGNet: Best formation energy (81 meV/atom MAE), predicts magnetic moments
- ALIGNN: 50+ pretrained property models from NIST/JARVIS

**Tier 3: Future -- Foundation Model Fine-Tuning**
- MatterTune for fine-tuning MACE/CHGNet on custom datasets

### 3b. Target Properties
- Band gap (eV) -- regression
- Formation energy (eV/atom) -- regression
- Energy above hull (eV/atom) -- regression
- Bulk modulus (GPa) -- regression
- Shear modulus (GPa) -- regression
- Is metallic -- classification
- Crystal system -- classification

### 3c. CLI Interface

```
# Predictions
prism predict "LiCoO2"                     # Composition-only (Tier 1)
prism predict "LiCoO2" --structure          # Fetch structure, use Tier 2
prism predict "LiCoO2" --property band_gap  # Specific property
prism predict --interactive                 # Guided prediction

# Model management
prism model train                           # Train on collected data
prism model train --property band_gap       # Train specific model
prism model benchmark                       # Matbench-style comparison
prism model status                          # Available models + metrics
prism model list                            # List pretrained models

# Visualization
prism model viz --property band_gap         # Performance plots
prism predict "LiCoO2" --viz               # Prediction confidence
```

### 3d. Prediction Flow

```
Composition-only:
  Formula -> pymatgen.Composition -> matminer featurizers -> 270 features
  -> MODNet / CrabNet / XGBoost ensemble -> predictions + confidence

Structure-based:
  Formula -> OPTIMADE query -> crystal structure
  -> MACE-MP-0 / CHGNet / ALIGNN -> structure-informed predictions
  -> Compare with composition-only -> Rich table output
```

### 3e. Visualization
- Feature importance bar charts (top-20 features per model)
- Model comparison (MAE/RMSE across models per property)
- Parity plots (predicted vs actual on test set)
- Prediction confidence distributions
- matplotlib saved as PNG or displayed in terminal via Rich

### 3f. Training Pipeline
- Auto-benchmark: train all models, rank by 5-fold CV score
- Hyperparameter tuning via Optuna
- Model registry in SQLite (property, algorithm, date, metrics)
- Versioned model files in `models/` directory (joblib serialization)

---

## Dependencies (Final)

### Remove
- `psycopg2-binary` (Postgres not used)
- `alembic` (migrations not needed for SQLite)

### Add
```
# Core ML
scikit-learn>=1.3.0
xgboost>=2.0.0
lightgbm>=4.0.0
optuna>=3.4.0

# Materials-specific ML
pymatgen>=2024.1.1
matminer>=0.9.0
modnet>=0.4.0
crabnet>=2.0.0

# Foundation models (Tier 2)
mace-torch>=0.3.0
chgnet>=0.3.0
alignn>=2024.1.0

# Visualization
matplotlib>=3.7.0

# Data
pyarrow>=14.0.0
joblib>=1.3.0
```

---

## File Structure (Target)

```
PRISM/
  app/
    __init__.py
    cli.py              # Command group definitions
    commands/
      __init__.py
      ask.py            # ask command
      search.py         # search command
      configure.py      # configure/advanced commands
      optimade.py       # optimade list-dbs etc
      predict.py        # predict command (NEW)
      model.py          # model train/status/benchmark (NEW)
      data.py           # data collect/status/export (NEW)
    config/
      branding.py
      providers.py
      settings.py       # Centralized config (NEW)
    data/
      __init__.py
      collector.py      # OPTIMADE + MP data collection (NEW)
      normalizer.py     # Schema normalization (NEW)
      store.py          # Parquet + metadata storage (NEW)
    db/
      database.py
      models.py
    llm.py              # Cleaned (4 providers only)
    mcp.py              # Fixed filter testing
    prompts.py
    ml/
      __init__.py
      features.py       # Feature engineering (NEW)
      trainer.py        # Model training pipeline (NEW)
      registry.py       # Model registry (NEW)
      predictor.py      # Inference engine (NEW)
      viz.py            # Visualization (NEW)
    utils/
      __init__.py
      enrichment.py     # MP enrichment (extracted)
      display.py        # Rich display helpers (extracted)
  tests/
    __init__.py
    test_llm.py
    test_mcp.py
    test_cli.py
    test_collector.py
    test_features.py
    test_predictor.py
  models/               # Trained model files (NEW)
  data/                 # Collected datasets (NEW)
  docs/
    plans/
    SECURITY.md
  pyproject.toml
  requirements.txt
  README.md
```
