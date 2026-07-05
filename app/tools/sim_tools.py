"""Simulation tools — atomistic structure creation, simulation, and job management.

All tools follow the same pattern as data.py / prediction.py:
  - Each tool function accepts **kwargs and returns a dict.
  - Lazy imports inside tool functions.
  - Registration via create_simulation_tools(registry).
"""
from app.tools.base import Tool, ToolRegistry


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _guard():
    """Return an error dict if pyiron is unavailable, else None.

    auto_provision=True: an actual tool call is the ONE place a blocking
    first-use install is acceptable (user asked for a simulation).
    """
    from app.tools.simulation.bridge import check_pyiron_available, _pyiron_missing_error
    if not check_pyiron_available(auto_provision=True):
        return _pyiron_missing_error()
    return None


# ===========================================================================
# C-1  Structure Tools
# ===========================================================================

def _create_structure(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        element = kwargs["element"]
        crystal_structure = kwargs.get("crystal_structure", "fcc")
        lattice_constant = kwargs.get("lattice_constant")
        repeat_x = kwargs.get("repeat_x", 1)
        repeat_y = kwargs.get("repeat_y", 1)
        repeat_z = kwargs.get("repeat_z", 1)

        # NOTE: pyiron's StructureFactory.bulk() takes the element as the
        # FIRST positional arg (named `name` in the signature), not as
        # `element=`. Earlier versions accepted `element=`; newer ones
        # don't. Pass positionally for forward-compat.
        bulk_kwargs = {"crystalstructure": crystal_structure}
        if lattice_constant is not None:
            bulk_kwargs["a"] = lattice_constant

        atoms = pr.create.structure.bulk(element, **bulk_kwargs)

        if any(r > 1 for r in (repeat_x, repeat_y, repeat_z)):
            atoms = atoms.repeat([int(repeat_x), int(repeat_y), int(repeat_z)])

        sid = bridge.structures.store(atoms)
        cell = atoms.cell.tolist() if hasattr(atoms.cell, "tolist") else str(atoms.cell)
        positions = atoms.positions.tolist()[:5] if hasattr(atoms.positions, "tolist") else []

        return {
            "structure_id": sid,
            "formula": atoms.get_chemical_formula(),
            "n_atoms": len(atoms),
            "cell": cell,
            "positions_preview": positions,
        }
    except Exception as e:
        return {"error": str(e)}


def _modify_structure(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        structure_id = kwargs["structure_id"]
        operation = kwargs["operation"]
        params = kwargs.get("params", {})

        atoms = bridge.structures.get(structure_id)
        if atoms is None:
            return {"error": f"Structure {structure_id} not found"}

        # Work on a copy
        atoms = atoms.copy()

        if operation == "supercell":
            nx = params.get("nx", 2)
            ny = params.get("ny", 2)
            nz = params.get("nz", 2)
            atoms = atoms.repeat([int(nx), int(ny), int(nz)])
        elif operation == "strain":
            strain = params.get("strain", 0.01)
            atoms.set_cell(atoms.cell * (1 + strain), scale_atoms=True)
        elif operation == "add_vacancy":
            index = params.get("index", 0)
            if 0 <= index < len(atoms):
                del atoms[int(index)]
            else:
                return {"error": f"Atom index {index} out of range (0-{len(atoms)-1})"}
        elif operation == "substitute_atom":
            index = params.get("index", 0)
            new_element = params.get("element")
            if new_element is None:
                return {"error": "Missing 'element' in params for substitute_atom"}
            if 0 <= index < len(atoms):
                atoms[int(index)].symbol = new_element
            else:
                return {"error": f"Atom index {index} out of range (0-{len(atoms)-1})"}
        else:
            return {"error": f"Unknown operation: {operation}. Supported: supercell, strain, add_vacancy, substitute_atom"}

        new_id = bridge.structures.store(atoms)
        return {
            "structure_id": new_id,
            "formula": atoms.get_chemical_formula(),
            "n_atoms": len(atoms),
            "operation": operation,
        }
    except Exception as e:
        return {"error": str(e)}


def _get_structure_info(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        structure_id = kwargs["structure_id"]
        atoms = bridge.structures.get(structure_id)
        if atoms is None:
            return {"error": f"Structure {structure_id} not found"}

        info = {
            "structure_id": structure_id,
            "formula": atoms.get_chemical_formula(),
            "n_atoms": len(atoms),
            "cell": atoms.cell.tolist() if hasattr(atoms.cell, "tolist") else str(atoms.cell),
            "volume": float(atoms.get_volume()) if hasattr(atoms, "get_volume") else None,
            "pbc": atoms.pbc.tolist() if hasattr(atoms.pbc, "tolist") else list(atoms.pbc),
        }

        # Try to get symmetry info (requires spglib)
        try:
            from pyiron_atomistics.atomistics.structure.atoms import ase_to_pyiron
            pyiron_struct = ase_to_pyiron(atoms)
            info["symmetry"] = {
                "space_group": pyiron_struct.get_spacegroup().get("InternationalTableSymbol", "unknown"),
            }
        except Exception:
            info["symmetry"] = {"space_group": "unavailable (spglib not found)"}

        return info
    except Exception as e:
        return {"error": str(e)}


def _list_potentials(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        element = kwargs.get("element")
        potential_type = kwargs.get("potential_type")

        # Use a LAMMPS job to query the potential database
        job = pr.create.job.Lammps("_tmp_potential_query")
        potentials_df = job.list_potentials()

        results = []
        for _, row in potentials_df.iterrows():
            name = str(row.get("Name", ""))
            species = row.get("Species", [])
            model = str(row.get("Model", ""))

            # Filter by element if specified
            if element and element not in str(species):
                continue
            # Filter by potential type if specified
            if potential_type and potential_type.lower() not in model.lower() and potential_type.lower() not in name.lower():
                continue

            results.append({
                "name": name,
                "model": model,
                "species": str(species),
            })

        job.remove()
        return {"potentials": results[:50], "count": len(results)}
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# C-2  Simulation / Job Tools
# ===========================================================================

_JOB_CODE_MAP = {
    "lammps": "Lammps",
    "vasp": "Vasp",
    "abinit": "Abinit",
    "gpaw": "Gpaw",
    "qe": "QuantumEspresso",
}


def _run_simulation(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        structure_id = kwargs["structure_id"]
        code = kwargs.get("code", "lammps").lower()
        potential = kwargs.get("potential")
        parameters = kwargs.get("parameters", {})

        atoms = bridge.structures.get(structure_id)
        if atoms is None:
            return {"error": f"Structure {structure_id} not found"}

        class_name = _JOB_CODE_MAP.get(code)
        if class_name is None:
            return {"error": f"Unsupported code: {code}. Supported: {list(_JOB_CODE_MAP.keys())}"}

        import uuid as _uuid
        job_name = f"prism_{code}_{_uuid.uuid4().hex[:6]}"
        job = getattr(pr.create.job, class_name)(job_name)
        job.structure = atoms

        if potential:
            job.potential = potential

        # Apply code-specific parameters
        calc_type = parameters.get("calc_type", "static")
        if calc_type == "minimize" and hasattr(job, "calc_minimize"):
            pressure = parameters.get("pressure")
            job.calc_minimize(pressure=pressure)
        elif calc_type == "md" and hasattr(job, "calc_md"):
            temperature = parameters.get("temperature", 300)
            n_ionic_steps = parameters.get("n_ionic_steps", 1000)
            job.calc_md(temperature=temperature, n_ionic_steps=n_ionic_steps)
        elif hasattr(job, "calc_static"):
            job.calc_static()

        job.run()

        jid = bridge.jobs.store(job, job_name)
        return {
            "job_id": jid,
            "code": code,
            "status": str(job.status),
            "job_name": job_name,
        }
    except Exception as e:
        return {"error": str(e)}


def _get_job_status(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        job_id = kwargs["job_id"]
        job = bridge.jobs.get(job_id)
        if job is None:
            return {"error": f"Job {job_id} not found"}

        return {
            "job_id": job_id,
            "status": str(job.status),
            "job_name": getattr(job, "job_name", job_id),
        }
    except Exception as e:
        return {"error": str(e)}


def _get_job_results(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        job_id = kwargs["job_id"]
        properties = kwargs.get("properties", ["energy_tot", "forces", "stress", "volume"])
        job = bridge.jobs.get(job_id)
        if job is None:
            return {"error": f"Job {job_id} not found"}

        if str(job.status) != "finished":
            return {"error": f"Job {job_id} has not finished (status: {job.status})"}

        results = {"job_id": job_id}
        for prop in properties:
            try:
                val = job[prop]
                # Convert numpy arrays to lists for JSON serialisation
                if hasattr(val, "tolist"):
                    val = val.tolist()
                results[prop] = val
            except Exception:
                results[prop] = None

        return results
    except Exception as e:
        return {"error": str(e)}


def _list_jobs(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        status_filter = kwargs.get("status_filter")
        code_filter = kwargs.get("code_filter")

        summaries = bridge.jobs.to_summary_list()
        if status_filter:
            summaries = [s for s in summaries if s["status"] == status_filter]
        if code_filter:
            summaries = [s for s in summaries if code_filter.lower() in s["code"].lower()]

        return {"jobs": summaries, "count": len(summaries)}
    except Exception as e:
        return {"error": str(e)}


def _delete_job(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        job_id = kwargs["job_id"]
        confirm = kwargs.get("confirm", False)

        if not confirm:
            return {"error": "Set confirm=true to delete the job."}

        job = bridge.jobs.get(job_id)
        if job is None:
            return {"error": f"Job {job_id} not found"}

        # Try to remove from pyiron project too
        try:
            job.remove()
        except Exception:
            pass

        bridge.jobs.delete(job_id)
        return {"deleted": job_id}
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# C-3  HPC + Workflow Tools
# ===========================================================================

def _submit_hpc_job(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        structure_id = kwargs["structure_id"]
        code = kwargs.get("code", "lammps").lower()
        potential = kwargs.get("potential")
        parameters = kwargs.get("parameters", {})
        queue = kwargs.get("queue", "default")
        cores = kwargs.get("cores", 1)
        walltime = kwargs.get("walltime", "01:00:00")

        atoms = bridge.structures.get(structure_id)
        if atoms is None:
            return {"error": f"Structure {structure_id} not found"}

        class_name = _JOB_CODE_MAP.get(code)
        if class_name is None:
            return {"error": f"Unsupported code: {code}"}

        import uuid as _uuid
        job_name = f"prism_hpc_{code}_{_uuid.uuid4().hex[:6]}"
        job = getattr(pr.create.job, class_name)(job_name)
        job.structure = atoms

        if potential:
            job.potential = potential

        # HPC settings
        job.server.queue = queue
        job.server.cores = int(cores)
        job.server.run_time = walltime

        calc_type = parameters.get("calc_type", "static")
        if calc_type == "minimize" and hasattr(job, "calc_minimize"):
            job.calc_minimize(pressure=parameters.get("pressure"))
        elif calc_type == "md" and hasattr(job, "calc_md"):
            job.calc_md(
                temperature=parameters.get("temperature", 300),
                n_ionic_steps=parameters.get("n_ionic_steps", 1000),
            )
        elif hasattr(job, "calc_static"):
            job.calc_static()

        job.run()

        jid = bridge.jobs.store(job, job_name)
        return {
            "job_id": jid,
            "code": code,
            "queue": queue,
            "cores": cores,
            "walltime": walltime,
            "status": str(job.status),
        }
    except Exception as e:
        return {"error": str(e)}


def _check_hpc_queue(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        # pyiron exposes queue status through the project's queue_status
        try:
            queue_status = pr.queue_status()
            jobs = []
            if hasattr(queue_status, "iterrows"):
                for _, row in queue_status.iterrows():
                    jobs.append({k: str(v) for k, v in row.items()})
            return {"queue_jobs": jobs, "count": len(jobs)}
        except Exception:
            # Fallback: return running/submitted jobs from our store
            summaries = bridge.jobs.to_summary_list()
            running = [s for s in summaries if s["status"] in ("submitted", "running", "collect")]
            return {"queue_jobs": running, "count": len(running)}
    except Exception as e:
        return {"error": str(e)}


def _run_convergence_test(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        structure_id = kwargs["structure_id"]
        code = kwargs.get("code", "lammps").lower()
        potential = kwargs.get("potential")
        parameter_name = kwargs["parameter_name"]
        parameter_values = kwargs["parameter_values"]

        atoms = bridge.structures.get(structure_id)
        if atoms is None:
            return {"error": f"Structure {structure_id} not found"}

        class_name = _JOB_CODE_MAP.get(code)
        if class_name is None:
            return {"error": f"Unsupported code: {code}"}

        import uuid as _uuid
        energies = []
        for val in parameter_values:
            job_name = f"prism_conv_{_uuid.uuid4().hex[:6]}"
            job = getattr(pr.create.job, class_name)(job_name)
            job.structure = atoms.copy()
            if potential:
                job.potential = potential

            # Apply the convergence parameter
            if parameter_name == "encut" and hasattr(job, "set_encut"):
                job.set_encut(val)
            elif parameter_name == "kpoints" and hasattr(job, "set_kpoints"):
                job.set_kpoints([int(val)] * 3)
            else:
                # Generic: try setting as input parameter
                try:
                    job.input[parameter_name] = val
                except Exception:
                    pass

            job.calc_static()
            job.run()

            energy = None
            try:
                energy = float(job["energy_tot"])
            except Exception:
                pass
            energies.append(energy)

        return {
            "parameter_name": parameter_name,
            "parameter_values": parameter_values,
            "energies": energies,
        }
    except Exception as e:
        return {"error": str(e)}


_WORKFLOW_MAP = {
    "elastic_constants": "ElasticMatrix",
    "phonons": "PhonopyJob",
    "equation_of_state": "Murnaghan",
    "thermal_expansion": "QuasiHarmonicApproximation",
}


def _run_workflow(**kwargs) -> dict:
    err = _guard()
    if err:
        return err
    try:
        from app.tools.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        workflow_type = kwargs["workflow_type"]
        structure_id = kwargs["structure_id"]
        parameters = kwargs.get("parameters", {})

        atoms = bridge.structures.get(structure_id)
        if atoms is None:
            return {"error": f"Structure {structure_id} not found"}

        wf_class_name = _WORKFLOW_MAP.get(workflow_type)
        if wf_class_name is None:
            return {"error": f"Unknown workflow: {workflow_type}. Supported: {list(_WORKFLOW_MAP.keys())}"}

        import uuid as _uuid
        job_name = f"prism_wf_{workflow_type}_{_uuid.uuid4().hex[:6]}"

        job = getattr(pr.create.job, wf_class_name)(job_name)

        # Workflows need a reference job for the underlying DFT/interatomic code
        ref_code = parameters.get("code", "lammps").lower()
        ref_class_name = _JOB_CODE_MAP.get(ref_code, "Lammps")
        ref_job = getattr(pr.create.job, ref_class_name)(f"{job_name}_ref")
        ref_job.structure = atoms

        if parameters.get("potential"):
            ref_job.potential = parameters["potential"]

        job.ref_job = ref_job
        job.run()

        jid = bridge.jobs.store(job, job_name)

        result_data = {}
        try:
            if workflow_type == "elastic_constants" and hasattr(job, "elastic_matrix"):
                em = job.elastic_matrix
                result_data["elastic_matrix"] = em.tolist() if hasattr(em, "tolist") else str(em)
            elif workflow_type == "equation_of_state":
                result_data["equilibrium_volume"] = float(job["equilibrium_volume"]) if "equilibrium_volume" in job else None
                result_data["equilibrium_energy"] = float(job["equilibrium_energy"]) if "equilibrium_energy" in job else None
                result_data["bulk_modulus"] = float(job["equilibrium_bulk_modulus"]) if "equilibrium_bulk_modulus" in job else None
        except Exception:
            pass

        return {
            "job_id": jid,
            "workflow_type": workflow_type,
            "status": str(job.status),
            "results": result_data,
        }
    except Exception as e:
        return {"error": str(e)}


# ===========================================================================
# Registration
# ===========================================================================

# ---------------------------------------------------------------------------
# Round 5 unified dispatchers
# ---------------------------------------------------------------------------

def _structure(**kwargs) -> dict:
    """Unified structure dispatcher. Replaces create/modify/get_structure_info."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: create, modify, info",
            "hint": (
                "structure(action='create', element='Al') / "
                "structure(action='modify', structure_id=..., operation=...) / "
                "structure(action='info', structure_id=...)"
            ),
        }
    if action == "create":
        if not kwargs.get("element"):
            return {"error": "Action 'create' requires `element`"}
        return _create_structure(**kwargs)
    if action == "modify":
        if not kwargs.get("structure_id") or not kwargs.get("operation"):
            return {"error": "Action 'modify' requires `structure_id` and `operation`"}
        return _modify_structure(**kwargs)
    if action == "info":
        if not kwargs.get("structure_id"):
            return {"error": "Action 'info' requires `structure_id`"}
        return _get_structure_info(**kwargs)
    return {"error": f"Unknown action '{action}'. Valid: create, modify, info"}


def _sim_run(**kwargs) -> dict:
    """Unified simulation dispatcher. Replaces run_simulation + submit_hpc_job."""
    target = kwargs.pop("target", "local")
    if target not in ("local", "hpc"):
        return {"error": f"Unknown target '{target}'. Valid: local, hpc"}
    if not kwargs.get("structure_id"):
        return {"error": f"Action target='{target}' requires `structure_id`"}
    if target == "local":
        return _run_simulation(**kwargs)
    # target == "hpc"
    return _submit_hpc_job(**kwargs)


def _sim_job(**kwargs) -> dict:
    """Unified job-mgmt dispatcher. Replaces get_job_status / get_job_results /
    list_jobs / delete_job."""
    action = kwargs.pop("action", None)
    if not action:
        return {
            "error": "Missing 'action'. Valid: status, results, list, delete",
            "hint": (
                "sim_job(action='status', job_id=...) / "
                "sim_job(action='results', job_id=..., properties=[...]) / "
                "sim_job(action='list', status_filter=...) / "
                "sim_job(action='delete', job_id=..., confirm=true)"
            ),
        }
    if action == "status":
        if not kwargs.get("job_id"):
            return {"error": "Action 'status' requires `job_id`"}
        return _get_job_status(**kwargs)
    if action == "results":
        if not kwargs.get("job_id"):
            return {"error": "Action 'results' requires `job_id`"}
        return _get_job_results(**kwargs)
    if action == "list":
        return _list_jobs(**kwargs)
    if action == "delete":
        if not kwargs.get("job_id"):
            return {"error": "Action 'delete' requires `job_id`"}
        return _delete_job(**kwargs)
    return {"error": f"Unknown action '{action}'. Valid: status, results, list, delete"}


# ---------------------------------------------------------------------------
# Tool descriptions
# ---------------------------------------------------------------------------

_STRUCTURE_DESCRIPTION = (
    "Build, transform, and inspect atomistic crystal structures (via "
    "pyiron / ASE). ONE tool, three actions:\n"
    "  • action='create' — bulk crystal structure from element + crystal type. "
    "Required: `element`. Optional: `crystal_structure` (fcc default; bcc, "
    "hcp, diamond, ...), `lattice_constant`, `repeat_x/y/z` for supercells. "
    "Returns a structure_id used by other sim tools.\n"
    "  • action='modify' — transform an existing structure. Required: "
    "`structure_id`, `operation` (supercell / strain / add_vacancy / "
    "substitute_atom). Operation-specific args go in `params`.\n"
    "  • action='info' — composition, cell vectors, volume, symmetry of a "
    "stored structure. Required: `structure_id`.\n"
    "Structure storage is session-local; IDs are not persisted across runs. "
    "NOT for searching materials databases (use materials_search) and NOT "
    "for visualizing structures (no 3D viewer here)."
)


_SIM_RUN_DESCRIPTION = (
    "Run an atomistic simulation. ONE tool, two targets:\n"
    "  • target='local' — execute on this machine (fast feedback, small jobs). "
    "Default if `target` omitted.\n"
    "  • target='hpc' — submit to an HPC queue (SLURM/PBS/SGE) for large jobs. "
    "Optional: `queue` (default 'default'), `cores` (default 1), `walltime` "
    "(default '01:00:00').\n"
    "Both targets share: `structure_id` (REQUIRED), `code` (lammps default; "
    "vasp, abinit, gpaw, qe), `potential` (interatomic potential name for "
    "LAMMPS — use `list_potentials` to discover), `parameters` (calc_type: "
    "'static'|'minimize'|'md', temperature, pressure, n_ionic_steps, ...).\n"
    "COMPUTE-HEAVY action (runs locally via pyiron — no credits charged) — "
    "requires_approval=True. The harness "
    "prompts before each call. Use compute_estimate via `compute(action="
    "'estimate')` first if you need a cost preview for the cloud broker, "
    "though this tool dispatches via pyiron not the broker."
)


_SIM_JOB_DESCRIPTION = (
    "Manage atomistic simulation jobs (started by `sim_run`). ONE tool, four "
    "actions:\n"
    "  • action='status' — current status of one job (queued / running / "
    "finished / aborted). Required: `job_id`. Cheap to call repeatedly in a "
    "polling loop.\n"
    "  • action='results' — pull energy / forces / stress / volume from a "
    "FINISHED job. Required: `job_id`. Optional: `properties` (list of "
    "specific fields to retrieve).\n"
    "  • action='list' — enumerate jobs. Optional: `status_filter`, "
    "`code_filter`.\n"
    "  • action='delete' — destructive. Removes job + output files. "
    "Required: `job_id`, `confirm=true`. Cannot be undone.\n"
    "NOT for the MARC27 compute broker (use `compute(action='status')` for "
    "cloud broker jobs); these are local pyiron jobs."
)


# ---------------------------------------------------------------------------
# Registration
# ---------------------------------------------------------------------------

def create_simulation_tools(registry: ToolRegistry) -> None:
    """Register simulation tools (guarded by pyiron availability).

    Round 5 collapses (13 → 7):
      structure   ← create_structure + modify_structure + get_structure_info
      sim_run     ← run_simulation + submit_hpc_job
      sim_job     ← get_job_status + get_job_results + list_jobs + delete_job
      list_potentials, run_convergence_test, run_workflow,
      check_hpc_queue stay standalone (different shapes / specialized concepts)
    """

    # --- Unified: structure (3 → 1) ----------------------------------------
    registry.register(Tool(
        name="structure",
        description=_STRUCTURE_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "modify", "info"],
                    "description": "Which structure operation to perform.",
                },
                "element": {
                    "type": "string",
                    "description": "Chemical element for action='create' (e.g. 'Fe', 'Al', 'Si').",
                },
                "crystal_structure": {
                    "type": "string",
                    "description": "Crystal structure type for action='create': fcc, bcc, hcp, diamond. Default: fcc.",
                },
                "lattice_constant": {
                    "type": "number",
                    "description": "Lattice constant in Angstroms for action='create'. Optional.",
                },
                "repeat_x": {"type": "integer", "description": "Supercell repeat in x for action='create'. Default 1."},
                "repeat_y": {"type": "integer", "description": "Supercell repeat in y for action='create'. Default 1."},
                "repeat_z": {"type": "integer", "description": "Supercell repeat in z for action='create'. Default 1."},
                "structure_id": {
                    "type": "string",
                    "description": "Structure ID for action='modify' or action='info'.",
                },
                "operation": {
                    "type": "string",
                    "description": "Modification operation for action='modify': supercell, strain, add_vacancy, substitute_atom.",
                },
                "params": {
                    "type": "object",
                    "description": "Operation-specific parameters for action='modify' (e.g. {nx:2,ny:2,nz:2}).",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_structure,
    ))

    # --- Unified: sim_run (2 → 1) ------------------------------------------
    registry.register(Tool(
        name="sim_run",
        description=_SIM_RUN_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "enum": ["local", "hpc"],
                    "default": "local",
                    "description": "'local' runs on this machine; 'hpc' submits to a SLURM/PBS/SGE queue.",
                },
                "structure_id": {
                    "type": "string",
                    "description": "Structure to simulate. Required.",
                },
                "code": {
                    "type": "string",
                    "description": "Simulation code: lammps (default), vasp, abinit, gpaw, qe.",
                },
                "potential": {
                    "type": "string",
                    "description": "Interatomic potential name (LAMMPS). Use list_potentials to discover.",
                },
                "parameters": {
                    "type": "object",
                    "description": "Code-specific params: {calc_type, temperature, pressure, n_ionic_steps, ...}.",
                },
                "queue": {
                    "type": "string",
                    "description": "Queue/partition for target='hpc'. Default 'default'.",
                },
                "cores": {
                    "type": "integer",
                    "description": "CPU cores for target='hpc'. Default 1.",
                },
                "walltime": {
                    "type": "string",
                    "description": "Wall-time limit for target='hpc' (e.g. '01:00:00').",
                },
            },
            "required": ["structure_id"],
            "additionalProperties": False,
        },
        func=_sim_run,
        requires_approval=True,
    ))

    # --- Unified: sim_job (4 → 1) ------------------------------------------
    registry.register(Tool(
        name="sim_job",
        description=_SIM_JOB_DESCRIPTION,
        input_schema={
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "results", "list", "delete"],
                    "description": "Which job-management operation to perform.",
                },
                "job_id": {
                    "type": "string",
                    "description": "Job ID for action='status'/'results'/'delete'.",
                },
                "properties": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Properties to retrieve for action='results' (energy_tot, forces, stress, volume).",
                },
                "status_filter": {
                    "type": "string",
                    "description": "Filter for action='list': finished, running, aborted.",
                },
                "code_filter": {
                    "type": "string",
                    "description": "Code filter for action='list': lammps, vasp, etc.",
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Required (true) for action='delete'. Confirms destructive op.",
                },
            },
            "required": ["action"],
            "additionalProperties": False,
        },
        func=_sim_job,
    ))

    # --- Standalone (kept) -------------------------------------------------
    # Catalog query — different concept (database lookup, not action on structure)
    registry.register(Tool(
        name="list_potentials",
        description=(
            "List interatomic potentials (EAM, MEAM, Tersoff, LJ, ...) "
            "available in the pyiron LAMMPS potential database for a given "
            "element / type. Use BEFORE sim_run when you need a `potential` "
            "argument and don't know what's available. NOT for materials "
            "search (use materials_search) and NOT for ML model listing "
            "(use list_models)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "element": {"type": "string", "description": "Filter by element symbol, e.g. 'Fe'"},
                "potential_type": {"type": "string", "description": "Filter by type: eam, meam, tersoff, lj"},
            },
            "required": [],
        },
        func=_list_potentials,
    ))

    # HPC queue inspection — different concept (queue-level, not job-level)
    registry.register(Tool(
        name="check_hpc_queue",
        description=(
            "Inspect the HPC queue (SLURM/PBS/SGE) for running and queued "
            "atomistic simulation jobs. Returns queue-level state across "
            "all of YOUR jobs. Different from sim_job(action='list') which "
            "lists pyiron-tracked jobs in your local session — this one "
            "talks to the actual scheduler. Use to see what's pending in "
            "the queue, not what you've spawned locally."
        ),
        input_schema={"type": "object", "properties": {}},
        func=_check_hpc_queue,
    ))

    # Convergence test — different shape (parameter sweep, not single sim)
    registry.register(Tool(
        name="run_convergence_test",
        description=(
            "Run an atomistic convergence test: vary one parameter (encut, "
            "kpoints, ecutwfc, ...) across N values and return energies for "
            "each. Use to check that simulation parameters are converged "
            "before running a real production calc. Returns "
            "{parameter_values, energies} suitable for plotting. Different "
            "shape from sim_run (which runs ONE simulation); this dispatches "
            "N simulations in sequence. Compute-heavy (runs locally, no "
            "charge) — keep N reasonable (3-7 values typically)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "structure_id": {"type": "string", "description": "Structure ID"},
                "code": {"type": "string", "description": "Simulation code. Default: lammps"},
                "potential": {"type": "string", "description": "Potential name"},
                "parameter_name": {"type": "string", "description": "Parameter to vary: encut, kpoints, ecutwfc, ..."},
                "parameter_values": {
                    "type": "array",
                    "items": {"type": "number"},
                    "description": "List of values to test. Keep length 3-7 to bound compute cost.",
                },
            },
            "required": ["structure_id", "parameter_name", "parameter_values"],
        },
        func=_run_convergence_test,
    ))

    # Predefined workflow — different shape (multi-step, named)
    registry.register(Tool(
        name="run_workflow",
        description=(
            "Run a predefined named workflow on a structure. Available: "
            "elastic_constants (full elastic tensor), phonons (phonon "
            "dispersion + DOS), equation_of_state (Murnaghan EoS fit → bulk "
            "modulus), thermal_expansion (quasi-harmonic approx). Each "
            "workflow internally dispatches multiple sim_run calls — "
            "this is the highest-level atomistic-sim entry point. NOT a "
            "substitute for sim_run (which runs ONE calc); use sim_run when "
            "you want fine-grained control. Compute-heavy (runs locally, no charge)."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "workflow_type": {
                    "type": "string",
                    "enum": ["elastic_constants", "phonons", "equation_of_state", "thermal_expansion"],
                    "description": "Predefined workflow name.",
                },
                "structure_id": {"type": "string", "description": "Structure ID"},
                "parameters": {
                    "type": "object",
                    "description": "Workflow params: {code: 'lammps'|'vasp'|..., potential: '...'}",
                },
            },
            "required": ["workflow_type", "structure_id"],
        },
        func=_run_workflow,
    ))
