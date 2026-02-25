# `prism sim` -- Atomistic Simulation

Manage atomistic simulations via the [pyiron](https://pyiron.org/) framework.
PRISM wraps pyiron's Project/Job system, exposing structure creation, simulation
execution, HPC submission, and result retrieval as agent-callable tools.

## Requirements

Simulation tools require pyiron (installed automatically with `prism-platform[all]`
or `prism-platform[simulation]`):

```bash
pip install prism-platform[simulation]
```

> **Note:** pyiron requires Python < 3.14. On Python 3.14+, simulation tools
> are gracefully skipped.

## CLI Subcommands

```bash
prism sim status    # Show pyiron config, available codes, job/structure counts
prism sim jobs      # List simulation jobs (optional --status filter)
prism sim init      # Initialize a pyiron project directory
```

## Agent Tools (13 tools)

The real power is in the agent tools, available via `prism run` and the REPL.

### Structure Tools

| Tool | Description |
|------|-------------|
| `create_structure` | Create a crystal structure (element, crystal type, lattice constant, supercell) |
| `modify_structure` | Modify a structure: supercell, strain, vacancy, substitution |
| `get_structure_info` | Get composition, cell, volume, symmetry for a stored structure |
| `list_potentials` | List available interatomic potentials (EAM, MEAM, Tersoff, etc.) |

### Simulation Tools

| Tool | Description | Approval |
|------|-------------|----------|
| `run_simulation` | Run LAMMPS/VASP/ABINIT/GPAW/QE on a structure | Yes |
| `get_job_status` | Check job status (initialized/running/finished/aborted) | No |
| `get_job_results` | Retrieve energy, forces, stress, volume from finished job | No |
| `list_jobs` | List jobs with optional status/code filter | No |
| `delete_job` | Delete a job and its output files (requires confirm) | No |

### HPC & Workflow Tools

| Tool | Description | Approval |
|------|-------------|----------|
| `submit_hpc_job` | Submit simulation to HPC queue (SLURM/PBS/SGE) | Yes |
| `check_hpc_queue` | Check running/queued HPC jobs | No |
| `run_convergence_test` | Vary one parameter (encut, kpoints) and collect energies | No |
| `run_workflow` | Run predefined workflow: elastic constants, phonons, EOS, thermal expansion | No |

## Architecture

```
prism sim / prism run (agent)
  └─ simulation tools (app/tools/simulation.py)
       └─ PyironBridge (app/simulation/bridge.py)
            ├─ StructureStore (in-memory, UUID-keyed)
            ├─ JobStore (in-memory, UUID-keyed)
            ├─ HPC config (~/.prism/hpc_config.json)
            └─ pyiron Project (lazy init)
                 ├─ pyiron_base: Job FSM, HDF5, queue adapters
                 └─ pyiron_atomistics: LAMMPS, VASP, Sphinx, GPAW wrappers
```

PRISM does NOT duplicate pyiron. The bridge layer translates between PRISM's
tool interface (JSON in, JSON out) and pyiron's Python object API. Advanced
users can use `execute_python` for direct pyiron scripting.

## Example Agent Interaction

```
User: Calculate the elastic constants of BCC iron

Agent:
  [create_structure] element=Fe, crystal_structure=bcc → struct_abc123
  [list_potentials] element=Fe → 12 potentials found
  [run_workflow] workflow_type=elastic_constants, structure_id=struct_abc123,
                 parameters={code: lammps, potential: "2009--Mendelev..."}
  → C11=243 GPa, C12=138 GPa, C44=122 GPa
```

## HPC Configuration

```bash
# Via the agent (in REPL or run mode):
"Configure HPC for SLURM with 8 cores and 2 hour walltime"

# Or manually:
~/.prism/hpc_config.json:
{
  "queue_system": "SLURM",
  "queue_name": "compute",
  "cores": 8,
  "walltime": "02:00:00"
}
```

## Supported Simulation Codes

| Code | Job Class | Use Case |
|------|-----------|----------|
| LAMMPS | `Lammps` | Classical MD, interatomic potentials |
| VASP | `Vasp` | DFT, electronic structure |
| ABINIT | `Abinit` | DFT, pseudopotentials |
| GPAW | `Gpaw` | DFT, real-space grid |
| Quantum ESPRESSO | `QuantumEspresso` | DFT, plane waves |

## Supported Workflows

| Workflow | pyiron Master | Output |
|----------|---------------|--------|
| `elastic_constants` | ElasticMatrix | 6x6 elastic tensor (GPa) |
| `phonons` | PhonopyJob | Phonon band structure, DOS |
| `equation_of_state` | Murnaghan | Equilibrium volume, bulk modulus |
| `thermal_expansion` | QuasiHarmonicApproximation | Temperature-dependent properties |

## Related

- [`prism calphad`](calphad.md) -- CALPHAD thermodynamic calculations
- [`prism run`](run.md) -- Autonomous agent mode
- [`prism data`](data.md) -- Data pipeline
- [Plugins](plugins.md) -- Extend with custom tools
- [pyiron documentation](https://pyiron.readthedocs.io/)
