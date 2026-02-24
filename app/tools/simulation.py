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
    """Return an error dict if pyiron is unavailable, else None."""
    from app.simulation.bridge import check_pyiron_available, _pyiron_missing_error
    if not check_pyiron_available():
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
        from app.simulation.bridge import get_bridge
        bridge = get_bridge()
        pr = bridge.get_project()

        element = kwargs["element"]
        crystal_structure = kwargs.get("crystal_structure", "fcc")
        lattice_constant = kwargs.get("lattice_constant")
        repeat_x = kwargs.get("repeat_x", 1)
        repeat_y = kwargs.get("repeat_y", 1)
        repeat_z = kwargs.get("repeat_z", 1)

        bulk_kwargs = {"element": element, "crystalstructure": crystal_structure}
        if lattice_constant is not None:
            bulk_kwargs["a"] = lattice_constant

        atoms = pr.create.structure.bulk(**bulk_kwargs)

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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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
        from app.simulation.bridge import get_bridge
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

def create_simulation_tools(registry: ToolRegistry) -> None:
    """Register all simulation tools (guarded by pyiron availability)."""

    # --- C-1: Structure tools ------------------------------------------------
    registry.register(Tool(
        name="create_structure",
        description="Create an atomistic crystal structure. Returns structure ID for use in simulations.",
        input_schema={
            "type": "object",
            "properties": {
                "element": {"type": "string", "description": "Chemical element symbol, e.g. 'Fe', 'Al', 'Si'"},
                "crystal_structure": {"type": "string", "description": "Crystal structure type: fcc, bcc, hcp, diamond, etc. Default: fcc"},
                "lattice_constant": {"type": "number", "description": "Lattice constant in Angstroms (optional, uses default for element)"},
                "repeat_x": {"type": "integer", "description": "Supercell repeat in x. Default: 1"},
                "repeat_y": {"type": "integer", "description": "Supercell repeat in y. Default: 1"},
                "repeat_z": {"type": "integer", "description": "Supercell repeat in z. Default: 1"},
            },
            "required": ["element"],
        },
        func=_create_structure,
    ))

    registry.register(Tool(
        name="modify_structure",
        description="Modify an existing structure: supercell, strain, add_vacancy, or substitute_atom.",
        input_schema={
            "type": "object",
            "properties": {
                "structure_id": {"type": "string", "description": "ID of the structure to modify"},
                "operation": {"type": "string", "description": "Operation: supercell, strain, add_vacancy, substitute_atom"},
                "params": {"type": "object", "description": "Operation-specific parameters (e.g. {nx:2,ny:2,nz:2} for supercell, {strain:0.01} for strain, {index:0} for vacancy, {index:0,element:'Ni'} for substitution)"},
            },
            "required": ["structure_id", "operation"],
        },
        func=_modify_structure,
    ))

    registry.register(Tool(
        name="get_structure_info",
        description="Get detailed info about a stored structure: composition, cell, volume, symmetry.",
        input_schema={
            "type": "object",
            "properties": {
                "structure_id": {"type": "string", "description": "ID of the structure"},
            },
            "required": ["structure_id"],
        },
        func=_get_structure_info,
    ))

    registry.register(Tool(
        name="list_potentials",
        description="List available interatomic potentials (EAM, MEAM, Tersoff, etc.) for a given element.",
        input_schema={
            "type": "object",
            "properties": {
                "element": {"type": "string", "description": "Filter by element symbol, e.g. 'Fe'"},
                "potential_type": {"type": "string", "description": "Filter by potential type: eam, meam, tersoff, lj, etc."},
            },
        },
        func=_list_potentials,
    ))

    # --- C-2: Simulation / Job tools -----------------------------------------
    registry.register(Tool(
        name="run_simulation",
        description="Run an atomistic simulation (LAMMPS, VASP, ABINIT, GPAW, QE) on a structure.",
        input_schema={
            "type": "object",
            "properties": {
                "structure_id": {"type": "string", "description": "ID of the structure to simulate"},
                "code": {"type": "string", "description": "Simulation code: lammps, vasp, abinit, gpaw, qe. Default: lammps"},
                "potential": {"type": "string", "description": "Interatomic potential name (for LAMMPS)"},
                "parameters": {"type": "object", "description": "Code-specific parameters: {calc_type: 'static'|'minimize'|'md', temperature: 300, pressure: 0, n_ionic_steps: 1000}"},
            },
            "required": ["structure_id"],
        },
        func=_run_simulation,
        requires_approval=True,
    ))

    registry.register(Tool(
        name="get_job_status",
        description="Get the current status of a simulation job.",
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job ID returned by run_simulation"},
            },
            "required": ["job_id"],
        },
        func=_get_job_status,
    ))

    registry.register(Tool(
        name="get_job_results",
        description="Get results from a completed simulation job: energy, forces, stress, volume.",
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job ID"},
                "properties": {"type": "array", "items": {"type": "string"}, "description": "Properties to retrieve: energy_tot, forces, stress, volume, etc."},
            },
            "required": ["job_id"],
        },
        func=_get_job_results,
    ))

    registry.register(Tool(
        name="list_jobs",
        description="List simulation jobs, optionally filtered by status or code.",
        input_schema={
            "type": "object",
            "properties": {
                "status_filter": {"type": "string", "description": "Filter by status: finished, running, aborted, etc."},
                "code_filter": {"type": "string", "description": "Filter by simulation code: lammps, vasp, etc."},
            },
        },
        func=_list_jobs,
    ))

    registry.register(Tool(
        name="delete_job",
        description="Delete a simulation job and its output files.",
        input_schema={
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job ID to delete"},
                "confirm": {"type": "boolean", "description": "Must be true to confirm deletion"},
            },
            "required": ["job_id"],
        },
        func=_delete_job,
    ))

    # --- C-3: HPC + Workflow tools -------------------------------------------
    registry.register(Tool(
        name="submit_hpc_job",
        description="Submit an atomistic simulation to an HPC queue (SLURM/PBS/SGE).",
        input_schema={
            "type": "object",
            "properties": {
                "structure_id": {"type": "string", "description": "Structure ID"},
                "code": {"type": "string", "description": "Simulation code: lammps, vasp, etc. Default: lammps"},
                "potential": {"type": "string", "description": "Potential name"},
                "parameters": {"type": "object", "description": "Code-specific parameters"},
                "queue": {"type": "string", "description": "Queue/partition name. Default: default"},
                "cores": {"type": "integer", "description": "Number of CPU cores. Default: 1"},
                "walltime": {"type": "string", "description": "Wall time limit, e.g. '01:00:00'. Default: 01:00:00"},
            },
            "required": ["structure_id"],
        },
        func=_submit_hpc_job,
        requires_approval=True,
    ))

    registry.register(Tool(
        name="check_hpc_queue",
        description="Check the HPC queue for running and queued simulation jobs.",
        input_schema={"type": "object", "properties": {}},
        func=_check_hpc_queue,
    ))

    registry.register(Tool(
        name="run_convergence_test",
        description="Run a convergence test: vary one parameter (encut, kpoints) across multiple values and return energies.",
        input_schema={
            "type": "object",
            "properties": {
                "structure_id": {"type": "string", "description": "Structure ID"},
                "code": {"type": "string", "description": "Simulation code. Default: lammps"},
                "potential": {"type": "string", "description": "Potential name"},
                "parameter_name": {"type": "string", "description": "Parameter to vary: encut, kpoints, etc."},
                "parameter_values": {"type": "array", "items": {"type": "number"}, "description": "List of values to test"},
            },
            "required": ["structure_id", "parameter_name", "parameter_values"],
        },
        func=_run_convergence_test,
    ))

    registry.register(Tool(
        name="run_workflow",
        description="Run a predefined workflow: elastic_constants, phonons, equation_of_state, thermal_expansion.",
        input_schema={
            "type": "object",
            "properties": {
                "workflow_type": {"type": "string", "description": "Workflow type: elastic_constants, phonons, equation_of_state, thermal_expansion"},
                "structure_id": {"type": "string", "description": "Structure ID"},
                "parameters": {"type": "object", "description": "Workflow parameters including 'code' and 'potential' for the reference job"},
            },
            "required": ["workflow_type", "structure_id"],
        },
        func=_run_workflow,
    ))
