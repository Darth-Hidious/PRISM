# `prism labs` -- Premium Marketplace Tools

Rarely-used, high-cost, high-value materials science services available through
the MARC27 platform marketplace. All labs tools are plugin-backed -- vendors
register on the platform, users subscribe and run via CLI or agent.

## Service Categories

| Category | Examples | Cost Range |
|----------|----------|------------|
| **A-Labs** | Berkeley A-Lab (autonomous synthesis), Kebotix (ChemOS) | $50-10,000/experiment |
| **DfM** | Manufacturability assessment, synthesis route analysis | $25-100/assessment |
| **Cloud DFT** | Matlantis (100,000x faster), Mat3ra (cloud VASP/QE) | $0.10-5.00/calc |
| **Quantum** | HQS (quantum chemistry), AQT (trapped-ion) | $5-100/qubit-hour |
| **Synchrotron** | SSRL beamtime booking, remote access | $500-2,000/shift |
| **HT Screening** | Combinatorial synthesis + ML-guided iteration | $5,000-50,000/campaign |

## CLI Commands

```bash
prism labs list                              # Browse all services
prism labs list --category quantum           # Filter by category
prism labs info matlantis_dft                # Detailed service info
prism labs status                            # Show active subscriptions
prism labs subscribe matlantis_dft --api-key KEY  # Subscribe to a service
```

## Agent Tools

| Tool | Description | Approval |
|------|-------------|----------|
| `list_lab_services` | Browse available services (with optional category filter) | No |
| `get_lab_service_info` | Detailed info, capabilities, requirements, pricing | No |
| `check_lab_subscriptions` | Show active subscriptions and usage | No |
| `submit_lab_job` | Submit a job to a subscribed service | **Yes** |

## How It Works

```
1. Browse   →  prism labs list
2. Learn    →  prism labs info <service>
3. Subscribe →  prism labs subscribe <service> --api-key KEY
4. Run      →  Agent calls submit_lab_job (with approval)
5. Results  →  Platform notifies via webhook / polling
```

All billing, security, and access control is handled by the MARC27 platform.
PRISM acts as the client — vendors register their services, set pricing, and
manage capacity on the platform side.

## Plugin Integration

Lab services are plugins. Third-party vendors can register services:

```python
# In a PRISM plugin
def register(registry):
    registry.tool_registry.register(Tool(
        name="my_cloud_dft",
        description="Submit DFT calculations to MyService",
        input_schema={...},
        func=_my_submission_function,
        requires_approval=True,
    ))
```

Or via the labs catalog (for platform-managed services):

```json
{
  "my_service": {
    "name": "My Cloud DFT Service",
    "category": "cloud-dft",
    "provider": "MyCompany",
    "cost_model": "per-calculation ($1.00)",
    "status": "available",
    "tools": ["my_submit", "my_status", "my_results"]
  }
}
```

## Available Services

### A-Labs (Autonomous Discovery)

- **A-Lab Autonomous Synthesis** (Berkeley Lab) -- Closed-loop robotic synthesis
  with XRD feedback. Targets oxide/phosphate compounds.
- **Kebotix Automated Discovery** -- ChemOS-powered multi-objective experiment
  design with robotic execution.

### Cloud DFT

- **Matlantis** (Preferred Networks) -- Universal neural network potential.
  100,000x faster than classical DFT. 72 elements, up to 20,000 atoms.
- **Mat3ra** -- End-to-end cloud platform. VASP, QE, LAMMPS + ML workflows.

### Quantum Computing

- **HQS Quantum Chemistry** -- 10 simulation modules for quantum chemistry.
  Battery materials, photocatalysts.
- **AQT ARNICA** -- Trapped-ion quantum simulation. Strongly correlated systems.

### Design for Manufacturability

- **Materials DfM Assessment** (MARC27) -- Synthesis feasibility, scalability,
  cost projections, risk identification.

### Synchrotron

- **SSRL Beamtime** -- 30 experimental stations. XRD, XAS, SAXS, microscopy.

### High-Throughput Screening

- **HT Screening** (MARC27) -- Combinatorial synthesis + rapid characterization
  + ML-guided adaptive sampling.

## Related

- [`prism model`](predict.md) -- ML prediction (free, local)
- [`prism sim`](sim.md) -- Atomistic simulation (local pyiron)
- [Plugins](plugins.md) -- Register custom lab services
- [Marketplace](https://prism.marc27.com/labs) -- Browse and subscribe
